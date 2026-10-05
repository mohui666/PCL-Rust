//! Forge / NeoForge installers: modern spec 1 processors, legacy install/versionInfo and json/maven.
//! Processor semantics follow MinecraftForge/Installer (2.0) and neoforged/LegacyInstaller
//! (main), src/main/java/net/minecraftforge/installer/actions/PostProcessors.java.
//! Java runs in a private staging root; only verified files are committed, without replacement.
use crate::{
    install::{self, cancelled, expected_hash, http_client, request_bytes, safe_target},
    java::{inspect_java, validate_for_version},
    loaders::commit_profile,
    metadata::{library_artifacts, safe_relative, validate_id},
    model::{Artifact, Platform, Progress},
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{atomic::AtomicBool, mpsc},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    Forge,
    NeoForge,
}
impl ForgeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Forge => "Forge",
            Self::NeoForge => "NeoForge",
        }
    }
    fn repository(self) -> &'static str {
        match self {
            Self::Forge => "https://maven.minecraftforge.net/net/minecraftforge/forge",
            Self::NeoForge => "https://maven.neoforged.net/releases/net/neoforged/neoforge",
        }
    }
    fn repository_for(self, minecraft: &str) -> &'static str {
        if self == Self::NeoForge && minecraft == "1.20.1" {
            "https://maven.neoforged.net/releases/net/neoforged/forge"
        } else {
            self.repository()
        }
    }
    fn artifact_for(self, minecraft: &str) -> &'static str {
        if self == Self::NeoForge && minecraft == "1.20.1" {
            "forge"
        } else {
            self.artifact()
        }
    }
    fn artifact(self) -> &'static str {
        match self {
            Self::Forge => "forge",
            Self::NeoForge => "neoforge",
        }
    }
    fn id(self, mc: &str, loader: &str) -> String {
        match self {
            Self::Forge => format!("{mc}-forge-{loader}"),
            Self::NeoForge => format!("neoforge-{loader}"),
        }
    }
}

/// Returns installable official versions. Historical Forge rows are classified using the
/// official index, so universal/client ZIPs are not presented as installer JARs.
pub fn list_versions(kind: ForgeKind, minecraft: &str, cancel: &AtomicBool) -> Result<Vec<String>> {
    validate_id(minecraft)?;
    cancelled(cancel)?;
    if kind == ForgeKind::Forge && legacy_minecraft(minecraft) {
        return Ok(legacy_entries(minecraft, cancel)?
            .into_iter()
            .filter(|entry| entry.category == "installer")
            .map(|entry| entry.file_version)
            .collect());
    }
    let Some(prefix) = maven_version_prefix(kind, minecraft) else {
        return Ok(Vec::new());
    };
    let bytes = request_bytes(
        &http_client()?,
        &format!("{}/maven-metadata.xml", kind.repository_for(minecraft)),
        None,
        None,
        cancel,
    )?;
    let text = std::str::from_utf8(&bytes).context("Maven 版本清单不是 UTF-8")?;
    parse_maven_versions(kind, minecraft, &prefix, text)
}

fn maven_version_prefix(kind: ForgeKind, minecraft: &str) -> Option<String> {
    match kind {
        ForgeKind::Forge => Some(format!("{minecraft}-")),
        ForgeKind::NeoForge if minecraft == "1.20.1" => Some(format!("{minecraft}-")),
        ForgeKind::NeoForge => {
            let parts: Vec<_> = minecraft.split('.').collect();
            if !(2..=3).contains(&parts.len())
                || parts
                    .iter()
                    .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
            {
                return None;
            }
            let patch = parts.get(2).unwrap_or(&"0");
            // Official versioning: 1.21.1 -> 21.1.x; from Minecraft 26.1
            // onward, retain the year and add the omitted hotfix zero:
            // 26.3 -> 26.3.0.x, 26.1.2 -> 26.1.2.x.
            // https://docs.neoforged.net/docs/gettingstarted/versioning/
            if parts[0] == "1" {
                Some(format!("{}.{patch}.", parts[1]))
            } else if parts[0].parse::<u32>().ok().is_some_and(|year| year >= 26) {
                Some(format!("{}.{}.{patch}.", parts[0], parts[1]))
            } else {
                None
            }
        }
    }
}

fn parse_maven_versions(
    kind: ForgeKind,
    minecraft: &str,
    prefix: &str,
    text: &str,
) -> Result<Vec<String>> {
    let regex = regex::Regex::new(r"<version>([^<>]+)</version>")?;
    let mut seen = HashSet::new();
    let mut versions = Vec::new();
    for capture in regex.captures_iter(text) {
        let raw = capture[1].trim();
        if !raw.starts_with(prefix) {
            continue;
        }
        // NeoForge snapshot/pre-release builds carry +snapshot-N / +pre-N.
        // They share the release prefix but do not target the final Minecraft
        // release. Official installer metadata remains the final install guard.
        if kind == ForgeKind::NeoForge && raw.contains('+') {
            continue;
        }
        let version = if kind == ForgeKind::Forge || minecraft == "1.20.1" {
            &raw[prefix.len()..]
        } else {
            raw
        };
        validate_id(version)?;
        if seen.insert(version.to_owned()) {
            versions.push(version.to_owned());
        }
    }
    versions.sort_by(|left, right| compare_loader_versions(right, left));
    Ok(versions)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LegacyEntry {
    version: String,
    file_version: String,
    category: String,
    md5: Option<String>,
}

fn legacy_minecraft(minecraft: &str) -> bool {
    let parts: Vec<_> = minecraft.split('.').collect();
    parts.first() == Some(&"1")
        && parts
            .get(1)
            .and_then(|p| p.parse::<u32>().ok())
            .is_some_and(|v| v <= 12)
}

// Fixed upstream ModDownload.vb, DlForgeVersionEntry.New (630–634): these
// historical Maven filenames carry a branch even when the index omits it.
fn legacy_file_version(mc: &str, version: &str, branch: Option<&str>) -> Result<String> {
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 4 || parts.iter().any(|v| v.parse::<u32>().is_err()) {
        bail!("旧版 Forge 版本号不是四段数字");
    }
    let branch = if matches!(version, "11.15.1.2318" | "11.15.1.1902" | "11.15.1.1890") {
        Some("1.8.9")
    } else if branch.is_none() && mc == "1.7.10" && parts[3].parse::<u32>()? >= 1300 {
        Some("1.7.10")
    } else {
        branch
    };
    if let Some(branch) = branch {
        validate_id(branch)?;
    }
    Ok(format!(
        "{version}{}",
        branch.map(|v| format!("-{v}")).unwrap_or_default()
    ))
}

fn parse_legacy_entries(html: &str, mc: &str) -> Result<Vec<LegacyEntry>> {
    // This follows the official table/classifier route in fixed upstream
    // ModDownload.vb:672–730, not a guess based on a Maven version existing.
    let rows = regex::Regex::new(r#"(?i)<td\s+class=["']download-version\b"#)?;
    let filename = regex::Regex::new(&format!(
        r#"forge-{}-([0-9]+(?:\.[0-9]+){{3}})(?:-([^/\s"'<>?]+?))?-(installer\.jar|universal\.zip|client\.zip)"#,
        regex::escape(mc)
    ))?;
    let md5 = regex::Regex::new(r"(?i)MD5:</strong>\s*([0-9a-f]{32})(?:\s|<)")?;
    let mut entries = BTreeMap::new();
    for row in rows.split(html).skip(1) {
        let matches: Vec<_> = filename.captures_iter(row).collect();
        let preferred = matches
            .iter()
            .find(|v| &v[3] == "installer.jar")
            .or_else(|| matches.iter().find(|v| &v[3] == "universal.zip"))
            .or_else(|| matches.iter().find(|v| &v[3] == "client.zip"));
        let Some(capture) = preferred else { continue };
        let version = capture[1].to_owned();
        let category = capture[3].split('.').next().unwrap().to_owned();
        let file_version = legacy_file_version(mc, &version, capture.get(2).map(|v| v.as_str()))?;
        let digest = md5
            .captures(&row[capture.get(0).unwrap().end()..])
            .map(|v| v[1].to_ascii_lowercase());
        if category == "installer" && digest.is_none() {
            bail!("Forge 官方 installer 条目缺少有效 MD5：{file_version}");
        }
        let entry = LegacyEntry {
            version,
            file_version: file_version.clone(),
            category,
            md5: digest,
        };
        if entries
            .insert(file_version, entry.clone())
            .is_some_and(|previous| previous != entry)
        {
            bail!("Forge 官方列表包含冲突条目");
        }
    }
    let mut values: Vec<_> = entries.into_values().collect();
    values.sort_by(|a, b| compare_loader_versions(&b.file_version, &a.file_version));
    Ok(values)
}

fn legacy_entries(mc: &str, cancel: &AtomicBool) -> Result<Vec<LegacyEntry>> {
    let bytes = request_bytes(
        &http_client()?,
        &format!(
            "https://files.minecraftforge.net/maven/net/minecraftforge/forge/index_{}.html",
            mc.replace('-', "_")
        ),
        None,
        None,
        cancel,
    )?;
    parse_legacy_entries(
        std::str::from_utf8(&bytes).context("Forge 官方索引不是 UTF-8")?,
        mc,
    )
}

fn select_legacy_entry(entries: Vec<LegacyEntry>, loader: &str) -> Result<LegacyEntry> {
    let mut matches = entries
        .into_iter()
        .filter(|entry| entry.file_version == loader || entry.version == loader);
    let entry = matches.next().context("官方索引未列出此 Forge 版本")?;
    if matches.next().is_some() {
        bail!("Forge 版本对应多个分支，请选择完整分支版本名");
    }
    if entry.category != "installer" {
        bail!(
            "此 Forge 版本只有 {} 文件，不能按 installer 安装",
            entry.category
        );
    }
    Ok(entry)
}

fn verify_legacy_installer(entry: &LegacyEntry, bytes: &[u8]) -> Result<()> {
    let actual = format!("{:x}", md5::Md5::digest(bytes));
    if entry.category != "installer" || entry.md5.as_deref() != Some(actual.as_str()) {
        bail!("旧 Forge 官方安装包 MD5 校验失败或不是 installer");
    }
    Ok(())
}

fn compare_loader_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let tokens = |value: &str| -> Vec<String> {
        let mut result = Vec::new();
        let mut start = 0;
        let mut numeric = None;
        for (index, ch) in value.char_indices() {
            let current = ch.is_ascii_digit();
            if numeric.is_some_and(|previous| previous != current) {
                result.push(value[start..index].to_owned());
                start = index;
            }
            numeric = Some(current);
        }
        result.push(value[start..].to_owned());
        result
    };
    let a = tokens(left);
    let b = tokens(right);
    for (a, b) in a.iter().zip(&b) {
        let order =
            if a.bytes().all(|v| v.is_ascii_digit()) && b.bytes().all(|v| v.is_ascii_digit()) {
                let a = a.trim_start_matches('0');
                let b = b.trim_start_matches('0');
                a.len().cmp(&b.len()).then_with(|| a.cmp(b))
            } else {
                a.cmp(b)
            };
        if order != std::cmp::Ordering::Equal {
            return order;
        }
    }
    // A final release sorts ahead of a prerelease sharing the same numeric version.
    b.len().cmp(&a.len()).then_with(|| left.cmp(right))
}

fn absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("目标已存在，拒绝覆盖：{}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn maven_path(name: &str) -> Result<PathBuf> {
    let (name, ext) = name.split_once('@').unwrap_or((name, "jar"));
    let parts: Vec<_> = name.split(':').collect();
    if !(3..=4).contains(&parts.len()) {
        bail!("无效处理器 Maven 坐标");
    }
    for part in &parts {
        validate_id(part)?;
    }
    for part in parts[0].split('.') {
        validate_id(part)?;
    }
    validate_id(ext)?;
    let classifier = parts.get(3).map(|p| format!("-{p}")).unwrap_or_default();
    safe_relative(&format!(
        "libraries/{}/{}/{}/{}-{}{classifier}.{ext}",
        parts[0].replace('.', "/"),
        parts[1],
        parts[2],
        parts[1],
        parts[2]
    ))
}

fn zip_bytes(
    archive: &mut zip::ZipArchive<File>,
    name: &str,
    max: u64,
    cancel: &AtomicBool,
) -> Result<Vec<u8>> {
    let name = name.strip_prefix('/').unwrap_or(name);
    safe_relative(name)?;
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("安装包缺少 {name}"))?;
    if entry.is_dir()
        || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
        || entry.size() > max
    {
        bail!("安装包条目类型或大小无效：{name}");
    }
    let mut bytes = Vec::new();
    let mut buffer = [0; 65536];
    loop {
        cancelled(cancel)?;
        let count = entry.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if bytes.len() as u64 + count as u64 > max {
            bail!("安装包条目超限：{name}");
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}

fn open_historical_installer(path: &Path, cancel: &AtomicBool) -> Result<zip::ZipArchive<File>> {
    cancelled(cancel)?;
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let tail_len = length.min(65_557);
    file.seek(SeekFrom::End(-(tail_len as i64)))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    let footer = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&at| {
            tail.get(at..at + 4) == Some(b"PK\x05\x06")
                && at + 22 + usize::from(u16::from_le_bytes([tail[at + 20], tail[at + 21]]))
                    == tail.len()
        })
        .context("历史安装器 ZIP 结束记录无效")?;
    let eocd = &tail[footer..];
    let u16_at = |n| u16::from_le_bytes([eocd[n], eocd[n + 1]]);
    let u32_at = |n| u32::from_le_bytes([eocd[n], eocd[n + 1], eocd[n + 2], eocd[n + 3]]);
    let count = u16_at(10);
    let central_size = u32_at(12);
    let central_offset = u32_at(16);
    if u16_at(4) != 0
        || u16_at(6) != 0
        || u16_at(8) != count
        || count == u16::MAX
        || central_size == u32::MAX
        || central_offset == u32::MAX
        || u64::from(central_offset) + u64::from(central_size) != length - tail_len + footer as u64
    {
        bail!("历史安装器 ZIP64、分卷或特殊 ZIP 布局尚未支持");
    }
    file.seek(SeekFrom::Start(0))?;
    let archive = zip::ZipArchive::new(file)?;
    // zip 2.x stores entries in an IndexMap keyed by filename and silently
    // collapses exact duplicates. Compare with the original central count.
    if archive.len() != usize::from(count)
        || archive.offset() != 0
        || archive.central_directory_start() != u64::from(central_offset)
    {
        bail!("历史安装器存在重复 ZIP 路径或自解压前缀");
    }
    cancelled(cancel)?;
    Ok(archive)
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("官方安装包缺少字符串 {key}"))
}

fn validate_profiles(
    profile: &Value,
    version: &Value,
    kind: ForgeKind,
    mc: &str,
    loader: &str,
) -> Result<String> {
    if profile["spec"].as_u64() != Some(1) {
        bail!("此安装包不是受支持的 spec 1 处理器格式");
    }
    let id = string(version, "id")?;
    validate_id(id)?;
    if string(profile, "minecraft")? != mc
        || string(version, "inheritsFrom")? != mc
        || string(profile, "version")? != id
        || (id != kind.id(mc, loader)
            && !(kind == ForgeKind::NeoForge
                && mc == "1.20.1"
                && id == format!("{mc}-forge-{loader}")))
    {
        bail!("官方安装包版本与请求的 Minecraft / 加载器不一致");
    }
    string(version, "mainClass")?;
    if profile["processors"].as_array().is_none() {
        bail!("安装包缺少 processors");
    }
    if client_processors(profile)?.is_empty() {
        bail!("此安装包没有受支持的客户端处理器；尚未支持该安装格式");
    }
    Ok(id.to_owned())
}

fn expand(value: &str, data: &HashMap<String, String>, stage: &Path) -> Result<String> {
    if let Some(coordinate) = value.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return Ok(safe_target(stage, &maven_path(coordinate)?)?
            .to_string_lossy()
            .into_owned());
    }
    let mut output = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('{') {
        if rest[..start].contains('}') {
            bail!("处理器参数占位符未配对");
        }
        output.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let end = tail.find('}').context("处理器参数占位符未闭合")?;
        output.push_str(
            data.get(&tail[..end])
                .with_context(|| format!("未知处理器占位符：{}", &tail[..end]))?,
        );
        rest = &tail[end + 1..];
        if output.len() > 65536 {
            bail!("处理器参数过长");
        }
    }
    if rest.contains('}') {
        bail!("处理器参数占位符未配对");
    }
    output.push_str(rest);
    if output.contains('\0') || output.len() > 65536 {
        bail!("处理器参数无效或过长");
    }
    // Expanded filesystem parameters must remain inside the processor's private root.
    if Path::new(&output).is_absolute() {
        return Ok(confined_output(stage, &output)?
            .to_string_lossy()
            .into_owned());
    }
    Ok(output)
}

fn confined_output(stage: &Path, value: &str) -> Result<PathBuf> {
    // canonicalize() returns a verbatim Windows root (\\?\...). Metadata still
    // appends '/' separators; in a verbatim Path those are literal characters.
    // Normalize only Windows separators before checking components, and keep the
    // prefix/traversal/symlink checks below rather than comparing string prefixes.
    #[cfg(windows)]
    let normalized = value.replace('/', "\\");
    #[cfg(windows)]
    let value = normalized.as_str();
    let path = Path::new(value);
    let relative = path.strip_prefix(stage).context("处理器输出超出临时目录")?;
    safe_target(stage, relative)
}

fn load_data(
    profile: &Value,
    archive: &mut zip::ZipArchive<File>,
    stage: &Path,
    installer: &Path,
    mc: &str,
    cancel: &AtomicBool,
) -> Result<HashMap<String, String>> {
    let mut data = HashMap::new();
    if let Some(entries) = profile["data"].as_object() {
        for (key, values) in entries {
            validate_id(key)?;
            let value = values["client"]
                .as_str()
                .context("安装包 data 缺少 client 值")?;
            let value = if value.starts_with('[') {
                expand(value, &HashMap::new(), stage)?
            } else if let Some(literal) =
                value.strip_prefix('\'').and_then(|v| v.strip_suffix('\''))
            {
                literal.to_owned()
            } else {
                let relative = safe_relative(value.strip_prefix('/').unwrap_or(value))?;
                let path = safe_target(stage, &Path::new("installer-data").join(relative))?;
                fs::create_dir_all(path.parent().context("安装数据路径无父目录")?)?;
                fs::write(&path, zip_bytes(archive, value, 128 * 1024 * 1024, cancel)?)?;
                path.to_string_lossy().into_owned()
            };
            data.insert(key.to_owned(), value);
        }
    }
    for (key, value) in [
        ("SIDE", "client".to_owned()),
        ("MINECRAFT_VERSION", mc.to_owned()),
        ("ROOT", stage.to_string_lossy().into_owned()),
        ("INSTALLER", installer.to_string_lossy().into_owned()),
        (
            "LIBRARY_DIR",
            stage.join("libraries").to_string_lossy().into_owned(),
        ),
        (
            "MINECRAFT_JAR",
            stage
                .join(format!("versions/{mc}/{mc}.jar"))
                .to_string_lossy()
                .into_owned(),
        ),
    ] {
        data.insert(key.to_owned(), value);
    }
    Ok(data)
}

fn file_artifact(stage: &Path, path: &Path, cancel: &AtomicBool) -> Result<Artifact> {
    let relative = path.strip_prefix(stage).context("生成文件越界")?.to_owned();
    safe_target(stage, &relative)?;
    let mut file = File::open(path)?;
    let mut hash = Sha1::new();
    let mut size = 0;
    let mut buffer = [0; 65536];
    loop {
        cancelled(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        size += count as u64;
    }
    if size == 0 {
        bail!("处理器生成了空文件：{}", relative.display());
    }
    Ok(Artifact {
        relative_path: relative,
        url: String::new(),
        sha1: Some(format!("{:x}", hash.finalize())),
        size: Some(size),
        native: false,
        excludes: vec![],
    })
}

fn verify_file(stage: &Path, artifact: &Artifact, cancel: &AtomicBool) -> Result<()> {
    expected_hash(artifact.sha1.as_deref())?.context("安装依赖缺少 SHA1，拒绝执行")?;
    if !install::cache_valid(
        &safe_target(stage, &artifact.relative_path)?,
        artifact,
        cancel,
    )? {
        bail!(
            "安装文件缺失或校验失败：{}",
            artifact.relative_path.display()
        );
    }
    Ok(())
}

fn main_class(jar: &Path, cancel: &AtomicBool) -> Result<String> {
    let mut archive = zip::ZipArchive::new(File::open(jar)?)?;
    let bytes = zip_bytes(&mut archive, "META-INF/MANIFEST.MF", 1024 * 1024, cancel)?;
    let manifest = String::from_utf8(bytes)?
        .replace("\r\n", "\n")
        .replace("\n ", "");
    let value = manifest
        .split("\n\n")
        .next()
        .unwrap_or("")
        .lines()
        .find_map(|line| line.strip_prefix("Main-Class: "))
        .context("处理器 JAR 没有 Main-Class")?;
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'$')
    {
        bail!("处理器 Main-Class 无效");
    }
    Ok(value.to_owned())
}

fn client_processors(profile: &Value) -> Result<Vec<&Value>> {
    let mut out = Vec::new();
    for proc in profile["processors"]
        .as_array()
        .context("processors 必须是数组")?
    {
        if let Some(sides) = proc.get("sides") {
            let sides = sides.as_array().context("processor.sides 必须是数组")?;
            if !sides.iter().any(|v| v.as_str() == Some("client")) {
                continue;
            }
        }
        out.push(proc);
    }
    Ok(out)
}

/// Drains both pipes continuously (including after the retained log limit), avoiding pipe deadlocks.
fn run_java(mut command: Command, cancel: &AtomicBool, timeout: Duration) -> Result<String> {
    cancelled(cancel)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().context("无法启动官方安装处理器")?;
    let (tx, rx) = mpsc::channel();
    for mut pipe in [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let tx = tx.clone();
        thread::spawn(move || {
            let mut tail = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        tail.extend_from_slice(&buffer[..count]);
                        if tail.len() > 65536 {
                            tail.drain(..tail.len() - 65536);
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(tail);
        });
    }
    drop(tx);
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => (),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.into());
            }
        }
        if cancelled(cancel).is_err() || start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            cancelled(cancel)?;
            bail!("官方安装处理器超时（{} 秒）", timeout.as_secs());
        }
        thread::sleep(Duration::from_millis(50));
    };
    let mut log = String::new();
    for _ in 0..2 {
        if let Ok(bytes) = rx.recv_timeout(Duration::from_secs(2)) {
            log.push_str(&String::from_utf8_lossy(&bytes));
            log.push('\n');
        }
    }
    cancelled(cancel)?;
    if !status.success() {
        bail!("官方处理器退出失败：{status}\n{log}");
    }
    Ok(log)
}

fn processor_outputs(
    proc: &Value,
    data: &HashMap<String, String>,
    stage: &Path,
) -> Result<Vec<Artifact>> {
    let mut outputs = Vec::new();
    if let Some(values) = proc.get("outputs") {
        for (path, hash) in values.as_object().context("processor.outputs 必须是对象")? {
            let path = confined_output(stage, &expand(path, data, stage)?)?;
            let hash = expand(
                hash.as_str()
                    .context("processor output SHA1 必须是字符串")?,
                data,
                stage,
            )?;
            outputs.push(Artifact {
                relative_path: path.strip_prefix(stage)?.to_owned(),
                url: String::new(),
                sha1: expected_hash(Some(&hash))?,
                size: None,
                native: false,
                excludes: vec![],
            });
        }
    }
    Ok(outputs)
}

fn verify_generated_jar(path: &Path, cancel: &AtomicBool) -> Result<()> {
    let mut zip = zip::ZipArchive::new(
        File::open(path).with_context(|| format!("处理器产物缺失：{}", path.display()))?,
    )?;
    if zip.is_empty() || zip.len() > 100_000 {
        bail!("处理器生成 JAR 无效");
    }
    let mut total = 0u64;
    for index in 0..zip.len() {
        cancelled(cancel)?;
        let mut entry = zip.by_index(index)?;
        safe_relative(entry.name().trim_end_matches('/'))?;
        if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            bail!("处理器 JAR 含符号链接");
        }
        if !entry.is_dir() {
            let mut buf = [0; 65536];
            loop {
                let count = entry.read(&mut buf)?;
                if count == 0 {
                    break;
                }
                cancelled(cancel)?;
                total += count as u64;
                if total > 2 * 1024 * 1024 * 1024 {
                    bail!("处理器 JAR 解压总大小超限");
                }
            }
        }
    }
    Ok(())
}

fn run_processors(
    profile: &Value,
    stage: &Path,
    data: &HashMap<String, String>,
    java: &Path,
    cancel: &AtomicBool,
    progress: &impl Fn(Progress),
) -> Result<Vec<Artifact>> {
    let processors = client_processors(profile)?;
    let mut generated = BTreeMap::new();
    for (index, proc) in processors.iter().enumerate() {
        cancelled(cancel)?;
        let name = string(proc, "jar")?;
        let jar = safe_target(stage, &maven_path(name)?)?;
        let mut paths = vec![jar.clone()];
        let mut seen = HashSet::from([jar.clone()]);
        for item in proc["classpath"]
            .as_array()
            .context("处理器缺少 classpath")?
        {
            let path = safe_target(
                stage,
                &maven_path(item.as_str().context("classpath 坐标无效")?)?,
            )?;
            if !path.is_file() {
                bail!("处理器依赖缺失：{}", path.display());
            }
            if seen.insert(path.clone()) {
                paths.push(path);
            }
        }
        let mut args = Vec::new();
        for arg in proc["args"].as_array().context("处理器缺少 args")? {
            args.push(expand(
                arg.as_str().context("处理器参数必须是字符串")?,
                data,
                stage,
            )?);
        }
        let mut outputs = processor_outputs(proc, data, stage)?;
        let inferred: Vec<PathBuf> = if outputs.is_empty() {
            args.windows(2)
                .filter(|pair| matches!(pair[0].as_str(), "--output" | "--slim" | "--extra"))
                .map(|pair| confined_output(stage, &pair[1]))
                .collect::<Result<_>>()?
        } else {
            Vec::new()
        };
        if outputs.is_empty() && inferred.is_empty() {
            bail!("处理器没有可验证的输出声明：{name}");
        }
        for path in &inferred {
            fs::create_dir_all(path.parent().context("处理器输出无父目录")?)?;
        }
        for output in &outputs {
            fs::create_dir_all(
                safe_target(stage, &output.relative_path)?
                    .parent()
                    .context("处理器输出无父目录")?,
            )?;
        }
        progress(Progress {
            message: format!("运行处理器 {}/{}：{name}", index + 1, processors.len()),
            completed: index as u64,
            total: processors.len() as u64,
            ..Default::default()
        });
        let mut command = Command::new(java);
        command
            .current_dir(stage)
            .arg("-Djava.awt.headless=true")
            .arg("-Djava.net.useSystemProxies=true")
            .arg("-cp")
            .arg(std::env::join_paths(paths)?)
            .arg(main_class(&jar, cancel)?)
            .args(&args);
        let log = run_java(command, cancel, Duration::from_secs(600))
            .with_context(|| format!("处理器 {} 失败", index + 1))?;
        fs::create_dir_all(stage.join("processor-logs"))?;
        fs::write(stage.join(format!("processor-logs/{}.log", index + 1)), log)?;
        for path in inferred {
            if path.extension().is_some_and(|e| e == "jar") {
                verify_generated_jar(&path, cancel)?;
            }
            outputs.push(file_artifact(stage, &path, cancel)?);
        }
        for mut output in outputs {
            verify_file(stage, &output, cancel)?;
            output.size = Some(fs::metadata(safe_target(stage, &output.relative_path)?)?.len());
            generated.insert(output.relative_path.clone(), output);
        }
    }
    Ok(generated.into_values().collect())
}

fn all_libraries(profile: &Value, version: &Value, platform: &Platform) -> Result<Vec<Artifact>> {
    let mut output: BTreeMap<PathBuf, Artifact> = BTreeMap::new();
    // Parse individual entries so duplicate paths with conflicting digests cannot be hidden by deduplication.
    for value in [profile, version] {
        for library in value["libraries"]
            .as_array()
            .context("安装包缺少 libraries")?
        {
            for artifact in library_artifacts(&json!({"libraries":[library]}), platform)? {
                expected_hash(artifact.sha1.as_deref())?.context("官方依赖缺少 SHA1")?;
                artifact.size.context("官方依赖缺少 size")?;
                if let Some(old) = output.get(&artifact.relative_path) {
                    if old.sha1 != artifact.sha1 || old.size != artifact.size {
                        bail!("重复依赖的校验值冲突：{}", artifact.relative_path.display());
                    }
                } else {
                    output.insert(artifact.relative_path.clone(), artifact);
                }
            }
        }
    }
    Ok(output.into_values().collect())
}

fn commit_files(
    root: &Path,
    stage: &Path,
    artifacts: &[Artifact],
    cancel: &AtomicBool,
) -> Result<()> {
    // Check the complete destination set before the first write; a concurrent writer still cannot be overwritten.
    for artifact in artifacts {
        cancelled(cancel)?;
        verify_file(stage, artifact, cancel)?;
        let target = safe_target(root, &artifact.relative_path)?;
        if target.exists() && !install::cache_valid(&target, artifact, cancel)? {
            bail!("既有文件冲突，未覆盖：{}", target.display());
        }
    }
    for artifact in artifacts {
        cancelled(cancel)?;
        let target = safe_target(root, &artifact.relative_path)?;
        if install::cache_valid(&target, artifact, cancel)? {
            continue;
        }
        fs::create_dir_all(target.parent().context("依赖路径无父目录")?)?;
        let mut temporary = tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
        let mut input = File::open(safe_target(stage, &artifact.relative_path)?)?;
        let mut buffer = [0; 65536];
        loop {
            cancelled(cancel)?;
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            temporary.write_all(&buffer[..count])?;
        }
        temporary.as_file().sync_all()?;
        safe_target(root, &artifact.relative_path)?;
        temporary
            .persist_noclobber(&target)
            .map_err(|e| e.error)
            .with_context(|| format!("提交文件冲突，未覆盖：{}", target.display()))?;
    }
    Ok(())
}

fn legacy_profile(profile: &Value, mc: &str, loader: &str, file_version: &str) -> Result<Value> {
    if profile.get("spec").is_some() || profile.get("processors").is_some() {
        bail!("旧版安装分支不能接受 spec / processors，必须按现代格式校验");
    }
    let install = profile
        .get("install")
        .and_then(Value::as_object)
        .context("旧版格式需要 install/versionInfo；json/maven 中版格式由独立分支处理")?;
    if install.get("minecraft").and_then(Value::as_str) != Some(mc) {
        bail!("旧 Forge 安装包的 Minecraft 版本不匹配");
    }
    let coordinate = format!("net.minecraftforge:forge:{mc}-{file_version}");
    if install.get("path").and_then(Value::as_str) != Some(coordinate.as_str()) {
        bail!("旧 Forge 安装包的 Maven 坐标与所选分支不匹配");
    }
    safe_relative(string(&profile["install"], "filePath")?)?;
    let version = profile
        .get("versionInfo")
        .filter(|v| v.is_object())
        .context("旧 Forge 安装包缺少 versionInfo")?
        .clone();
    validate_legacy_version(version, mc, loader, &coordinate)
}

fn validate_legacy_version(
    mut version: Value,
    mc: &str,
    loader: &str,
    coordinate: &str,
) -> Result<Value> {
    validate_id(string(&version, "id")?)?;
    for key in ["inheritsFrom", "jar"] {
        if version.get(key).is_some_and(|v| v.as_str() != Some(mc)) {
            bail!("旧 Forge versionInfo 的 {key} 与原版不匹配");
        }
    }
    if version.get("downloads").is_some() || version.get("arguments").is_some() {
        bail!("此旧 Forge profile 替换原版核心或含现代参数，不属于已支持的旧格式");
    }
    if string(&version, "mainClass")? != "net.minecraft.launchwrapper.Launch" {
        bail!("旧 Forge profile 不是支持的 LaunchWrapper 客户端入口");
    }
    let args = crate::launch::split_legacy_arguments(string(&version, "minecraftArguments")?)?;
    let tweaks: Vec<_> = args
        .windows(2)
        .filter(|pair| pair[0] == "--tweakClass")
        .collect();
    if tweaks.len() != 1
        || !matches!(
            tweaks[0][1].as_str(),
            "cpw.mods.fml.common.launcher.FMLTweaker"
                | "net.minecraftforge.fml.common.launcher.FMLTweaker"
        )
    {
        bail!("旧 Forge profile 缺少明确的客户端 FMLTweaker");
    }
    let libraries = version["libraries"]
        .as_array_mut()
        .context("旧 Forge 缺少 libraries")?;
    if libraries.is_empty() || libraries.len() > 256 {
        bail!("旧 Forge libraries 数量无效");
    }
    // Historical profiles may list server-only libraries; they must never enter the client classpath.
    libraries.retain(|library| library["clientreq"].as_bool() != Some(false));
    if libraries
        .iter()
        .filter(|v| v["name"].as_str() == Some(coordinate))
        .count()
        != 1
    {
        bail!("旧 Forge 客户端 libraries 必须恰好引用一次内嵌 Forge 坐标");
    }
    if libraries.iter().any(|library| {
        library["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("net.minecraftforge:forge:") && name != coordinate)
    }) {
        bail!("旧 Forge libraries 混入了其他 Forge 版本");
    }
    version["id"] = ForgeKind::Forge.id(mc, loader).into();
    version["inheritsFrom"] = mc.into();
    version.as_object_mut().unwrap().remove("jar");
    Ok(version)
}

/// The fixed PCL ModDownloadLib.vb:1316–1328 json/maven branch never runs Java.
/// Official Installer Util.loadInstallProfile recognizes spec 0 (also missing)
/// and 1; processors are a separate capability and must not be silently skipped.
fn middle_profile(
    archive: &mut zip::ZipArchive<File>,
    profile: &Value,
    mc: &str,
    loader: &str,
    file_version: &str,
    cancel: &AtomicBool,
) -> Result<Value> {
    if profile.get("install").is_some()
        || profile.get("versionInfo").is_some()
        || profile
            .get("spec")
            .is_some_and(|v| !matches!(v.as_u64(), Some(0 | 1)))
    {
        bail!("中版 Forge 安装包格式混合或 spec 未支持");
    }
    if !legacy_minecraft(mc)
        || string(profile, "minecraft")? != mc
        || profile["hideClient"].as_bool() == Some(true)
    {
        bail!("中版 Forge 安装包不是所选历史 Minecraft 的客户端安装器");
    }
    if let Some(processors) = profile.get("processors") {
        for processor in processors
            .as_array()
            .context("中版 processors 必须是数组")?
        {
            let sides = processor
                .get("sides")
                .and_then(Value::as_array)
                .context("中版处理器未明确限定为非客户端；不会跳过客户端安装步骤")?;
            if sides.is_empty()
                || sides
                    .iter()
                    .any(|side| !matches!(side.as_str(), Some("server" | "extract")))
            {
                bail!("此中版 Forge 需要客户端处理器，本分支不执行或忽略这些步骤");
            }
        }
    }
    let coordinate = string(profile, "path")?;
    let expected = format!("net.minecraftforge:forge:{mc}-{file_version}");
    if coordinate != expected && coordinate != format!("{expected}:universal") {
        bail!("中版 Forge 主 Maven 坐标与所选分支不匹配");
    }
    let name = string(profile, "json")?;
    let name = name.strip_prefix('/').unwrap_or(name);
    safe_relative(name)?;
    if name == "install_profile.json"
        || archive.file_names().filter(|entry| *entry == name).count() != 1
    {
        bail!("中版版本 JSON 条目缺失、重复或指向安装元数据自身");
    }
    let version: Value =
        serde_json::from_slice(&zip_bytes(archive, name, 16 * 1024 * 1024, cancel)?)?;
    if string(&version, "id")? != string(profile, "version")?
        || string(&version, "inheritsFrom")? != mc
    {
        bail!("中版版本 JSON 的 ID 或 inheritsFrom 与安装元数据不匹配");
    }
    validate_legacy_version(version, mc, loader, coordinate)
}

fn embedded_legacy_libraries(
    archive: &mut zip::ZipArchive<File>,
    profile: &Value,
    cancel: &AtomicBool,
) -> Result<(PathBuf, BTreeMap<PathBuf, String>)> {
    if let Some(install) = profile.get("install") {
        let coordinate = string(install, "path")?;
        let entry = string(install, "filePath")?;
        safe_relative(entry)?;
        if archive.file_names().filter(|name| *name == entry).count() != 1 {
            bail!("安装包内嵌 Forge JAR 条目缺失或重复");
        }
        let path = maven_path(coordinate)?;
        return Ok((path.clone(), BTreeMap::from([(path, entry.to_owned())])));
    }
    if archive.len() > 100_000 {
        bail!("中版安装包条目数量超限");
    }
    let main_path = maven_path(string(profile, "path")?)?;
    let mut entries = BTreeMap::new();
    let mut folded = HashSet::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        cancelled(cancel)?;
        let entry = archive.by_index(index)?;
        let Some(relative) = entry.name().strip_prefix("maven/") else {
            continue;
        };
        if relative.is_empty() && entry.is_dir() {
            continue;
        }
        let relative = safe_relative(relative.trim_end_matches('/'))?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            bail!("中版 maven 目录包含符号链接");
        }
        if entry.is_dir() {
            continue;
        }
        if entry.size() > 256 * 1024 * 1024 {
            bail!("中版 maven 文件大小超限");
        }
        total = total
            .checked_add(entry.size())
            .context("中版 maven 总大小溢出")?;
        if total > 512 * 1024 * 1024 {
            bail!("中版 maven 文件总大小超限");
        }
        let path = Path::new("libraries").join(relative);
        if !folded.insert(path.to_string_lossy().to_lowercase())
            || entries.insert(path, entry.name().into()).is_some()
        {
            bail!("中版 maven 文件路径重复或大小写冲突");
        }
    }
    if !entries.contains_key(&main_path) {
        bail!("中版安装包 maven/ 缺少声明的 Forge 客户端库");
    }
    Ok((main_path, entries))
}

fn legacy_library_url(value: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(value).context("旧版库 URL 无效")?;
    // Upstream old profiles use the historical HTTP Maven alias. Rewrite only
    // these exact official hosts; never downgrade arbitrary URLs or trust mirrors.
    let host = url.host_str().context("旧版库 URL 缺少主机")?.to_owned();
    if !matches!(
        host.as_str(),
        "files.minecraftforge.net" | "maven.minecraftforge.net" | "libraries.minecraft.net"
    ) {
        bail!("旧版库不是已支持的官方来源：{host}");
    }
    if !matches!(url.scheme(), "http" | "https")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("旧版库 URL 含不支持的协议或附加字段");
    }
    url.set_scheme("https")
        .map_err(|_| anyhow::anyhow!("无法规范旧版库协议"))?;
    if host == "files.minecraftforge.net" {
        let path = url
            .path()
            .strip_prefix("/maven/")
            .context("旧 Forge Maven 路径无效")?
            .to_owned();
        url.set_host(Some("maven.minecraftforge.net"))?;
        url.set_path(&format!("/{path}"));
    }
    Ok(install::validate_url(url.as_str())?.to_string())
}

fn legacy_checksum(bytes: &[u8]) -> Result<String> {
    if bytes.len() > 1024 {
        bail!("官方库 SHA1 文件超限");
    }
    let value = std::str::from_utf8(bytes)?
        .split_whitespace()
        .next()
        .context("官方库 SHA1 为空")?;
    expected_hash(Some(value))?.context("官方库 SHA1 缺失")
}

#[allow(clippy::too_many_arguments)]
fn prepare_legacy_libraries(
    root: &Path,
    stage: &Path,
    archive: &mut zip::ZipArchive<File>,
    profile: &Value,
    version: &mut Value,
    platform: &Platform,
    cancel: &AtomicBool,
    fetch: &impl Fn(&str, Option<&str>) -> Result<Vec<u8>>,
    progress: &impl Fn(Progress),
) -> Result<Vec<Artifact>> {
    let middle = profile.get("install").is_none();
    let (embedded_path, embedded_entries) = embedded_legacy_libraries(archive, profile, cancel)?;
    let libraries = version["libraries"]
        .as_array_mut()
        .context("旧 Forge 缺少 libraries")?;
    let total = libraries.len();
    let mut paths = HashSet::new();
    let mut output = Vec::new();
    for (index, library) in libraries.iter_mut().enumerate() {
        cancelled(cancel)?;
        let mut artifacts = library_artifacts(&json!({"libraries":[library.clone()]}), platform)?;
        if artifacts.is_empty() {
            continue;
        }
        if artifacts.len() != 1 || artifacts[0].native {
            bail!("该旧版 Forge 含额外原生库，尚未支持；不会登记不完整版本");
        }
        let mut artifact = artifacts.remove(0);
        if !paths.insert(artifact.relative_path.clone()) {
            bail!("旧 Forge libraries 含重复路径");
        }
        let coordinate_path = maven_path(string(library, "name")?)?;
        if (middle || coordinate_path == embedded_path) && artifact.relative_path != coordinate_path
        {
            bail!("内嵌 Forge 下载路径与 Maven 坐标冲突");
        }
        let embedded_entry = embedded_entries.get(&artifact.relative_path);
        let embedded = embedded_entry.is_some();
        artifact.url = if embedded {
            // Legacy installers carry their runtime JAR; no fabricated external repair URL.
            String::new()
        } else {
            legacy_library_url(&artifact.url)?
        };
        let mut expected = Vec::new();
        if let Some(hash) = artifact
            .sha1
            .as_deref()
            .or_else(|| library["sha1"].as_str())
        {
            expected.push(expected_hash(Some(hash))?.unwrap());
        }
        let primary_checksum = expected.first().cloned();
        if let Some(checksums) = library.get("checksums") {
            for hash in checksums.as_array().context("旧版 checksums 必须是数组")? {
                expected.push(
                    expected_hash(Some(hash.as_str().context("旧版 checksum 必须是字符串")?))?
                        .unwrap(),
                );
            }
        }
        if !embedded && expected.is_empty() {
            expected.push(legacy_checksum(&fetch(
                &format!("{}.sha1", artifact.url),
                None,
            )?)?);
        }
        let declared_size = artifact.size.or_else(|| library["size"].as_u64());
        artifact.sha1 = expected.first().cloned();
        artifact.size = declared_size;
        progress(Progress {
            message: format!("旧 Forge 支持库：{}", artifact.relative_path.display()),
            completed: index as u64,
            total: total as u64,
            ..Default::default()
        });
        let target = safe_target(stage, &artifact.relative_path)?;
        fs::create_dir_all(target.parent().context("旧版库路径无父目录")?)?;
        if embedded {
            fs::write(
                &target,
                zip_bytes(archive, embedded_entry.unwrap(), 256 * 1024 * 1024, cancel)?,
            )?;
            verify_generated_jar(&target, cancel).context("安装包内嵌 Forge JAR 无效")?;
        } else {
            let source = safe_target(root, &artifact.relative_path)?;
            if install::cache_valid(&source, &artifact, cancel)? {
                fs::copy(source, &target)?;
            } else {
                // request_bytes verifies this hash as it downloads. Multiple historical
                // checksums are handled below after receiving the bounded response.
                let bytes = fetch(
                    &artifact.url,
                    (expected.len() == 1).then(|| expected[0].as_str()),
                )?;
                cancelled(cancel)?;
                fs::write(&target, bytes)?;
            }
        }
        let actual = file_artifact(stage, &target, cancel)?;
        if (!expected.is_empty() && !expected.contains(actual.sha1.as_ref().unwrap()))
            || primary_checksum
                .as_ref()
                .is_some_and(|hash| Some(hash) != actual.sha1.as_ref())
        {
            bail!(
                "旧 Forge 支持库 SHA1 校验失败：{}",
                artifact.relative_path.display()
            );
        }
        if declared_size.is_some_and(|size| Some(size) != actual.size) {
            bail!(
                "旧 Forge 支持库大小不匹配：{}",
                artifact.relative_path.display()
            );
        }
        artifact.sha1 = actual.sha1;
        artifact.size = actual.size;
        let relative = artifact
            .relative_path
            .strip_prefix("libraries")?
            .to_string_lossy()
            .replace('\\', "/");
        library["downloads"] = json!({"artifact":{"path":relative,"url":artifact.url,"sha1":artifact.sha1,"size":artifact.size}});
        library["_pcl_checksum_source"] = if embedded && expected.is_empty() {
            "verified-installer-embedded-local-sha1"
        } else if embedded {
            "official-installer-library-sha1"
        } else {
            "official-library-sha1"
        }
        .into();
        output.push(artifact);
    }
    if !output
        .iter()
        .any(|artifact| artifact.relative_path == embedded_path)
    {
        bail!("平台规则排除了内嵌 Forge 客户端库");
    }
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn finish_legacy_install(
    root: &Path,
    stage: &Path,
    profile: &Value,
    version: &mut Value,
    artifacts: &[Artifact],
    parent: &Value,
    platform: &Platform,
    installer_md5: &str,
    installer_sha1: &str,
    cancel: &AtomicBool,
) -> Result<String> {
    let id = string(version, "id")?.to_owned();
    // The launch planner chooses arguments.game ahead of minecraftArguments.
    // A customized parent with modern game arguments would silently discard the
    // legacy FML arguments; refuse that mixed format instead of registering it.
    if parent.pointer("/arguments/game").is_some() {
        bail!("原版父版本使用现代游戏参数，无法安全合并旧 Forge minecraftArguments");
    }
    absent(&safe_target(
        root,
        &PathBuf::from(format!("versions/{id}")),
    )?)?;
    commit_files(root, stage, artifacts, cancel)?;
    let (format, minecraft) = if profile.get("install").is_some() {
        (
            "legacy-install-versionInfo",
            &profile["install"]["minecraft"],
        )
    } else {
        ("legacy-json-maven", &profile["minecraft"])
    };
    version["_pcl_forge_install"] = json!({"kind":"forge","format":format,
        "installerMd5":installer_md5,"installerSha1":installer_sha1,
        "installerSha1Source":"local-digest-after-official-md5-verification",
        "minecraft":minecraft,"processorCount":0,
        "embeddedLibrarySha1Source":"local-digest-of-verified-installer-entry",
        "files":artifacts.iter().map(|a|json!({"path":a.relative_path.to_string_lossy().replace('\\',"/"),"sha1":a.sha1,"size":a.size})).collect::<Vec<_>>()});
    let mut native_inputs = library_artifacts(parent, platform)?;
    native_inputs.extend(library_artifacts(version, platform)?);
    commit_profile(root, &id, version, &native_inputs, cancel)?;
    Ok(id)
}

/// Installs an official client profile. Existing target profiles/directories are never replaced.
/// Processor code runs only after the official installer and every input library pass SHA1 checks.
#[allow(clippy::too_many_arguments)]
pub fn install_forge(
    root: &Path,
    kind: ForgeKind,
    minecraft: &str,
    loader: &str,
    java: &Path,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: impl Fn(Progress) + Sync,
) -> Result<String> {
    cancelled(cancel)?;
    validate_id(minecraft)?;
    validate_id(loader)?;
    let id = kind.id(minecraft, loader);
    validate_id(&id)?;
    absent(&safe_target(
        root,
        &PathBuf::from(format!("versions/{id}")),
    )?)?;
    let client = http_client()?;
    let legacy = if kind == ForgeKind::Forge && legacy_minecraft(minecraft) {
        Some(select_legacy_entry(
            legacy_entries(minecraft, cancel)?,
            loader,
        )?)
    } else {
        None
    };
    let coordinate = if kind == ForgeKind::Forge || minecraft == "1.20.1" {
        format!(
            "{minecraft}-{}",
            legacy
                .as_ref()
                .map(|entry| entry.file_version.as_str())
                .unwrap_or(loader)
        )
    } else {
        loader.to_owned()
    };
    let url = format!(
        "{}/{coordinate}/{}-{coordinate}-installer.jar",
        kind.repository_for(minecraft),
        kind.artifact_for(minecraft)
    );
    progress(Progress {
        message: format!("校验 {} 官方安装包", kind.label()),
        completed: 0,
        total: 1,
        ..Default::default()
    });
    let official_sha = if legacy.is_none() {
        Some(legacy_checksum(&request_bytes(
            &client,
            &format!("{url}.sha1"),
            None,
            None,
            cancel,
        )?)?)
    } else {
        None
    };
    fs::create_dir_all(root)?;
    let staging = tempfile::Builder::new()
        .prefix(".pcl-forge-")
        .tempdir_in(root)?;
    let stage = staging.path().canonicalize()?;
    let installer = stage.join("installer.jar");
    let installer_bytes = request_bytes(&client, &url, official_sha.as_deref(), None, cancel)?;
    if let Some(entry) = &legacy {
        verify_legacy_installer(entry, &installer_bytes)?;
    }
    let sha = format!("{:x}", Sha1::digest(&installer_bytes));
    fs::write(&installer, installer_bytes)?;
    let mut archive = if legacy.is_some() {
        open_historical_installer(&installer, cancel)?
    } else {
        zip::ZipArchive::new(File::open(&installer)?)?
    };
    if legacy.is_some()
        && archive
            .file_names()
            .filter(|name| *name == "install_profile.json")
            .count()
            != 1
    {
        bail!("旧 Forge install_profile.json 缺失或重复");
    }
    let profile: Value = serde_json::from_slice(&zip_bytes(
        &mut archive,
        "install_profile.json",
        16 * 1024 * 1024,
        cancel,
    )?)?;
    if let Some(entry) = &legacy {
        let mut version = if profile.get("install").is_some() {
            legacy_profile(&profile, minecraft, loader, &entry.file_version)?
        } else {
            middle_profile(
                &mut archive,
                &profile,
                minecraft,
                loader,
                &entry.file_version,
                cancel,
            )?
        };
        let artifacts = prepare_legacy_libraries(
            root,
            &stage,
            &mut archive,
            &profile,
            &mut version,
            platform,
            cancel,
            &|url, expected| request_bytes(&client, url, expected, None, cancel),
            &progress,
        )?;
        let parent_path = safe_target(
            root,
            &PathBuf::from(format!("versions/{minecraft}/{minecraft}.json")),
        )?;
        if !parent_path.exists() {
            install::install_version(root, minecraft, platform, cancel, &progress)?;
        }
        let parent = install::verify_vanilla_parent(root, minecraft, platform, cancel)?;
        let id = finish_legacy_install(
            root,
            &stage,
            &profile,
            &mut version,
            &artifacts,
            &parent,
            platform,
            entry.md5.as_deref().unwrap(),
            &sha,
            cancel,
        )?;
        progress(Progress {
            message: format!("Forge 安装完成：{id}"),
            completed: 1,
            total: 1,
            ..Default::default()
        });
        return Ok(id);
    }
    let mut version: Value = serde_json::from_slice(&zip_bytes(
        &mut archive,
        string(&profile, "json")?,
        16 * 1024 * 1024,
        cancel,
    )?)?;
    validate_profiles(&profile, &version, kind, minecraft, loader)?;
    // Early NeoForge intentionally retained Forge's upstream profile id. Give
    // the local profile its own namespace to avoid colliding with real Forge.
    version["id"] = id.clone().into();
    let libraries = all_libraries(&profile, &version, platform)?;
    let parent_path = safe_target(
        root,
        &PathBuf::from(format!("versions/{minecraft}/{minecraft}.json")),
    )?;
    if !parent_path.exists() {
        install::install_version(root, minecraft, platform, cancel, &progress)?;
    }
    let parent = install::verify_vanilla_parent(root, minecraft, platform, cancel)?;
    let runtime = inspect_java(java)?;
    validate_for_version(
        &runtime,
        parent
            .pointer("/javaVersion/majorVersion")
            .and_then(Value::as_u64)
            .unwrap_or(8) as u32,
        platform,
    )?;
    let data = load_data(
        &profile,
        &mut archive,
        &stage,
        &installer,
        minecraft,
        cancel,
    )?;
    let mc_relative = PathBuf::from(format!("versions/{minecraft}/{minecraft}.jar"));
    let staged_mc = safe_target(&stage, &mc_relative)?;
    fs::create_dir_all(staged_mc.parent().unwrap())?;
    fs::copy(safe_target(root, &mc_relative)?, &staged_mc)?;
    for (index, artifact) in libraries.iter().enumerate() {
        cancelled(cancel)?;
        progress(Progress {
            message: format!(
                "{} 安装依赖：{}",
                kind.label(),
                artifact.relative_path.display()
            ),
            completed: index as u64,
            total: libraries.len() as u64,
            ..Default::default()
        });
        let source = safe_target(root, &artifact.relative_path)?;
        let target = safe_target(&stage, &artifact.relative_path)?;
        fs::create_dir_all(target.parent().unwrap())?;
        if install::cache_valid(&source, artifact, cancel)? {
            fs::copy(source, &target)?;
        } else {
            let entry = format!(
                "maven/{}",
                artifact
                    .relative_path
                    .strip_prefix("libraries")?
                    .to_string_lossy()
                    .replace('\\', "/")
            );
            if archive.index_for_name(&entry).is_some() {
                fs::write(
                    &target,
                    zip_bytes(&mut archive, &entry, 256 * 1024 * 1024, cancel)?,
                )?;
            } else if !artifact.url.is_empty() {
                install::download_artifact(&client, &stage, artifact, cancel)?;
            } else {
                continue;
            }
        }
        verify_file(&stage, artifact, cancel)?;
    }
    let generated = run_processors(&profile, &stage, &data, &runtime.path, cancel, &progress)?;
    let mut committed: BTreeMap<PathBuf, Artifact> = libraries
        .into_iter()
        .map(|a| (a.relative_path.clone(), a))
        .collect();
    for artifact in generated {
        committed
            .entry(artifact.relative_path.clone())
            .or_insert(artifact);
    }
    let artifacts: Vec<_> = committed.into_values().collect();
    for artifact in &artifacts {
        verify_file(&stage, artifact, cancel)?;
    }
    absent(&safe_target(
        root,
        &PathBuf::from(format!("versions/{id}")),
    )?)?;
    commit_files(root, &stage, &artifacts, cancel)?;
    version["_pcl_forge_install"] = json!({"kind":kind,"installerSha1":sha,"minecraft":minecraft,"loader":loader,"processorCount":client_processors(&profile)?.len(),"allProcessorOutputsHaveOfficialSha1":client_processors(&profile)?.iter().all(|p|p["outputs"].as_object().is_some_and(|o|!o.is_empty())),"files":artifacts.iter().map(|a|json!({"path":a.relative_path.to_string_lossy().replace('\\',"/"),"sha1":a.sha1,"size":a.size})).collect::<Vec<_>>()});
    let mut native_inputs = library_artifacts(&parent, platform)?;
    native_inputs.extend(library_artifacts(&version, platform)?);
    commit_profile(root, &id, &version, &native_inputs, cancel)?;
    progress(Progress {
        message: format!("{} 安装完成：{id}", kind.label()),
        completed: 1,
        total: 1,
        ..Default::default()
    });
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn legacy_fixture() -> Value {
        serde_json::from_str(include_str!(
            "../tests/fixtures/forge/legacy-install-profile.synthetic.json"
        ))
        .unwrap()
    }

    fn legacy_archive(stage: &Path, profile: &Value) -> zip::ZipArchive<File> {
        let mut inner = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        inner
            .start_file(
                "cpw/mods/fml/common/launcher/FMLTweaker.class",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        inner
            .write_all(b"synthetic fixture only; no executable bytecode")
            .unwrap();
        let bytes = inner.finish().unwrap().into_inner();
        let path = stage.join("synthetic-installer.jar");
        let mut outer = zip::ZipWriter::new(File::create(&path).unwrap());
        outer
            .start_file(
                "install_profile.json",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        outer
            .write_all(&serde_json::to_vec(profile).unwrap())
            .unwrap();
        outer
            .start_file(
                profile["install"]["filePath"].as_str().unwrap(),
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        outer.write_all(&bytes).unwrap();
        outer.finish().unwrap();
        open_historical_installer(&path, &AtomicBool::new(false)).unwrap()
    }

    fn legacy_fetch(url: &str, expected: Option<&str>) -> Result<Vec<u8>> {
        let bytes = b"synthetic launchwrapper dependency";
        let hash = format!("{:x}", Sha1::digest(bytes));
        if url.ends_with(".sha1") {
            return Ok(hash.into_bytes());
        }
        assert_eq!(url, "https://libraries.minecraft.net/net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar");
        assert_eq!(expected, Some(hash.as_str()));
        Ok(bytes.to_vec())
    }

    fn legacy_parent(root: &Path) -> Value {
        synthetic_parent(root, "1.7.10")
    }

    fn synthetic_parent(root: &Path, mc: &str) -> Value {
        let version_dir = root.join("versions").join(mc);
        fs::create_dir_all(&version_dir).unwrap();
        fs::create_dir_all(root.join("assets/indexes")).unwrap();
        let jar = b"synthetic vanilla client";
        let index = br#"{"objects":{}}"#;
        fs::write(version_dir.join(format!("{mc}.jar")), jar).unwrap();
        fs::write(root.join(format!("assets/indexes/{mc}.json")), index).unwrap();
        let parent = json!({"id":mc,"type":"release","mainClass":"net.minecraft.client.main.Main",
            "minecraftArguments":"--username ${auth_player_name}","libraries":[],
            "downloads":{"client":{"sha1":format!("{:x}",Sha1::digest(jar)),"size":jar.len(),"url":"https://piston-data.mojang.com/fixture.jar"}},
            "assetIndex":{"id":mc,"sha1":format!("{:x}",Sha1::digest(index)),"size":index.len(),"url":"https://piston-meta.mojang.com/fixture.json"}});
        fs::write(
            version_dir.join(format!("{mc}.json")),
            serde_json::to_vec(&parent).unwrap(),
        )
        .unwrap();
        parent
    }

    #[test]
    fn early_neoforge_uses_its_own_repository_and_accepts_publisher_legacy_id() {
        assert_eq!(
            ForgeKind::NeoForge.repository_for("1.20.1"),
            "https://maven.neoforged.net/releases/net/neoforged/forge"
        );
        assert_eq!(ForgeKind::NeoForge.artifact_for("1.20.1"), "forge");
        assert_eq!(ForgeKind::NeoForge.artifact_for("1.21.1"), "neoforge");
        let profile = json!({"spec":1,"minecraft":"1.20.1","version":"1.20.1-forge-47.1.106","processors":[{"sides":["client"],"jar":"net.neoforged:installertools:1.0","args":[],"classpath":[]}]});
        let version = json!({"id":"1.20.1-forge-47.1.106","inheritsFrom":"1.20.1","mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher"});
        assert!(validate_profiles(
            &profile,
            &version,
            ForgeKind::NeoForge,
            "1.20.1",
            "47.1.106"
        )
        .is_ok());
        assert!(
            validate_profiles(&profile, &version, ForgeKind::NeoForge, "1.20.1", "47.1.99")
                .is_err()
        );
        assert!(validate_profiles(
            &profile,
            &version,
            ForgeKind::NeoForge,
            "1.21.1",
            "47.1.106"
        )
        .is_err());
    }
    #[test]
    fn legacy_official_table_categories_checksums_and_source_branch_rules() {
        let entries = parse_legacy_entries(
            include_str!("../tests/fixtures/forge/legacy-index.synthetic.html"),
            "1.7.10",
        )
        .unwrap();
        assert_eq!(entries.len(), 3);
        let selected = select_legacy_entry(entries.clone(), "10.13.4.1614").unwrap();
        assert_eq!(selected.file_version, "10.13.4.1614-1.7.10");
        assert_eq!(
            selected.md5.as_deref(),
            Some("22222222222222222222222222222222")
        );
        assert!(select_legacy_entry(entries.clone(), "10.13.0.1299")
            .unwrap_err()
            .to_string()
            .contains("universal"));
        assert!(select_legacy_entry(entries, "10.13.0.1298")
            .unwrap_err()
            .to_string()
            .contains("client"));
        assert_eq!(
            legacy_file_version("1.7.10", "10.13.0.1300", None).unwrap(),
            "10.13.0.1300-1.7.10"
        );
        assert_eq!(
            legacy_file_version("1.7.10", "10.13.0.1299", None).unwrap(),
            "10.13.0.1299"
        );
        for version in ["11.15.1.2318", "11.15.1.1902", "11.15.1.1890"] {
            assert_eq!(
                legacy_file_version("1.8.9", version, Some("other")).unwrap(),
                format!("{version}-1.8.9")
            );
        }
        assert!(parse_legacy_entries(
            &include_str!("../tests/fixtures/forge/legacy-index.synthetic.html")
                .replace("22222222222222222222222222222222", "bad"),
            "1.7.10"
        )
        .is_err());
    }

    #[test]
    fn legacy_installer_digest_and_profile_identity_fail_closed() {
        let mut entry = LegacyEntry {
            version: "10.13.4.1614".into(),
            file_version: "10.13.4.1614-1.7.10".into(),
            category: "installer".into(),
            md5: Some(format!("{:x}", md5::Md5::digest(b"fixture"))),
        };
        verify_legacy_installer(&entry, b"fixture").unwrap();
        assert!(verify_legacy_installer(&entry, b"changed").is_err());
        entry.category = "universal".into();
        assert!(verify_legacy_installer(&entry, b"fixture").is_err());
        for (path, value) in [
            ("/install/minecraft", json!("1.8.9")),
            (
                "/install/path",
                json!("net.minecraftforge:forge:1.7.10-10.13.4.1614"),
            ),
            ("/install/filePath", json!("../outside.jar")),
            ("/versionInfo/mainClass", json!("net.minecraft.server.Main")),
            (
                "/versionInfo/minecraftArguments",
                json!("--tweakClass unknown.Tweaker"),
            ),
        ] {
            let mut profile = legacy_fixture();
            *profile.pointer_mut(path).unwrap() = value;
            assert!(
                legacy_profile(&profile, "1.7.10", "10.13.4.1614", "10.13.4.1614-1.7.10").is_err(),
                "{path}"
            );
        }
        for (key, value) in [("spec", json!(1)), ("processors", json!([]))] {
            let mut profile = legacy_fixture();
            profile[key] = value;
            assert!(
                legacy_profile(&profile, "1.7.10", "10.13.4.1614", "10.13.4.1614-1.7.10").is_err()
            );
        }
        let mut middle = legacy_fixture();
        middle.as_object_mut().unwrap().remove("install");
        assert!(
            legacy_profile(&middle, "1.7.10", "10.13.4.1614", "10.13.4.1614-1.7.10")
                .unwrap_err()
                .to_string()
                .contains("中版")
        );
        let mut mixed = legacy_fixture();
        mixed["versionInfo"]["libraries"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"net.minecraftforge:forge:1.7.10-10.13.4.1558-1.7.10"}));
        assert!(legacy_profile(&mixed, "1.7.10", "10.13.4.1614", "10.13.4.1614-1.7.10").is_err());
    }

    #[test]
    fn legacy_synthetic_install_preserves_parent_and_builds_windows_and_macos_argv() {
        for os in ["windows", "osx"] {
            let root = tempfile::tempdir().unwrap();
            let stage = tempfile::tempdir().unwrap();
            let profile = legacy_fixture();
            let mut archive = legacy_archive(stage.path(), &profile);
            let mut version = legacy_profile(
                &profile,
                "1.7.10",
                "10.13.4.1614-1.7.10",
                "10.13.4.1614-1.7.10",
            )
            .unwrap();
            let platform = Platform {
                os: os.into(),
                arch: "x86_64".into(),
                version: "10.0".into(),
            };
            let parent = legacy_parent(root.path());
            let original = fs::read(root.path().join("versions/1.7.10/1.7.10.json")).unwrap();
            install::verify_vanilla_parent(
                root.path(),
                "1.7.10",
                &platform,
                &AtomicBool::new(false),
            )
            .unwrap();
            let artifacts = prepare_legacy_libraries(
                root.path(),
                stage.path(),
                &mut archive,
                &profile,
                &mut version,
                &platform,
                &AtomicBool::new(false),
                &legacy_fetch,
                &|_| (),
            )
            .unwrap();
            assert_eq!(artifacts.len(), 2);
            let id = finish_legacy_install(
                root.path(),
                stage.path(),
                &profile,
                &mut version,
                &artifacts,
                &parent,
                &platform,
                &"a".repeat(32),
                &"b".repeat(40),
                &AtomicBool::new(false),
            )
            .unwrap();
            assert_eq!(
                fs::read(root.path().join("versions/1.7.10/1.7.10.json")).unwrap(),
                original
            );
            let resolved = crate::metadata::resolve_version(root.path(), &id).unwrap();
            assert_eq!(resolved["_pcl_jar_id"], "1.7.10");
            assert_eq!(resolved["_pcl_forge_install"]["processorCount"], 0);
            assert_eq!(
                resolved["_pcl_forge_install"]["installerSha1Source"],
                "local-digest-after-official-md5-verification"
            );
            fs::write(root.path().join("synthetic-java"), b"never executed").unwrap();
            let options = crate::launch::LaunchOptions {
                root: root.path().into(),
                version_id: id,
                java: root.path().join("synthetic-java"),
                memory_mb: 1024,
                width: 854,
                height: 480,
            };
            let plan = crate::launch::build_plan(
                &options,
                &crate::auth::offline_session("LegacyFixture").unwrap(),
                &platform,
            )
            .unwrap();
            assert!(plan
                .args
                .iter()
                .any(|v| v == "net.minecraft.launchwrapper.Launch"));
            assert!(plan
                .args
                .windows(2)
                .any(|v| v == ["--tweakClass", "cpw.mods.fml.common.launcher.FMLTweaker"]));
            assert!(plan
                .args
                .windows(2)
                .any(|v| v == ["--username", "LegacyFixture"]));
            assert!(plan.args.windows(2).any(|v| v == ["--accessToken", "0"]));
            let classpath = plan.args.windows(2).find(|v| v[0] == "-cp").unwrap()[1].clone();
            assert!(
                classpath.contains("1.7.10.jar")
                    && classpath.contains("forge-1.7.10-10.13.4.1614-1.7.10.jar")
                    && classpath.contains("launchwrapper-1.12.jar")
            );
            assert!(!classpath.contains("server-only"));
            assert_eq!(
                plan.args.iter().any(|v| v == "-XstartOnFirstThread"),
                os == "osx"
            );
        }
    }

    #[test]
    fn legacy_cached_library_is_verified_and_reused_without_fetching_its_bytes() {
        let root = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir().unwrap();
        let mut profile = legacy_fixture();
        profile["versionInfo"]["libraries"][1]["checksums"] =
            json!([format!("{:x}", Sha1::digest(b"cached wrapper"))]);
        let relative =
            Path::new("libraries/net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar");
        fs::create_dir_all(root.path().join(relative).parent().unwrap()).unwrap();
        fs::write(root.path().join(relative), b"cached wrapper").unwrap();
        let mut archive = legacy_archive(stage.path(), &profile);
        let mut version =
            legacy_profile(&profile, "1.7.10", "10.13.4.1614", "10.13.4.1614-1.7.10").unwrap();
        prepare_legacy_libraries(
            root.path(),
            stage.path(),
            &mut archive,
            &profile,
            &mut version,
            &Platform::current(),
            &AtomicBool::new(false),
            &|_, _| panic!("valid cache must not fetch"),
            &|_| (),
        )
        .unwrap();
        assert_eq!(
            fs::read(stage.path().join(relative)).unwrap(),
            b"cached wrapper"
        );
    }

    #[test]
    fn legacy_bad_hash_and_mid_download_cancellation_never_register_a_version() {
        for cancel_midway in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let stage = tempfile::tempdir().unwrap();
            let profile = legacy_fixture();
            let mut archive = legacy_archive(stage.path(), &profile);
            let mut version =
                legacy_profile(&profile, "1.7.10", "10.13.4.1614", "10.13.4.1614-1.7.10").unwrap();
            let cancel = AtomicBool::new(false);
            let error = prepare_legacy_libraries(
                root.path(),
                stage.path(),
                &mut archive,
                &profile,
                &mut version,
                &Platform::current(),
                &cancel,
                &|url, expected| {
                    if url.ends_with(".sha1") {
                        return legacy_fetch(url, expected);
                    }
                    if cancel_midway {
                        cancel.store(true, Ordering::Relaxed);
                    }
                    Ok(b"wrong bytes".to_vec())
                },
                &|_| (),
            )
            .unwrap_err();
            if cancel_midway {
                assert!(error
                    .downcast_ref::<crate::model::OperationCancelled>()
                    .is_some());
            } else {
                assert!(error.to_string().contains("SHA1"));
            }
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn legacy_conflicting_user_library_and_existing_profile_are_not_replaced() {
        let root = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir().unwrap();
        let profile = legacy_fixture();
        let mut archive = legacy_archive(stage.path(), &profile);
        let mut version =
            legacy_profile(&profile, "1.7.10", "10.13.4.1614", "10.13.4.1614-1.7.10").unwrap();
        let platform = Platform::current();
        let parent = legacy_parent(root.path());
        let artifacts = prepare_legacy_libraries(
            root.path(),
            stage.path(),
            &mut archive,
            &profile,
            &mut version,
            &platform,
            &AtomicBool::new(false),
            &legacy_fetch,
            &|_| (),
        )
        .unwrap();
        let mut modern_parent = parent.clone();
        modern_parent["arguments"] = json!({"game":["--username","${auth_player_name}"]});
        assert!(finish_legacy_install(
            root.path(),
            stage.path(),
            &profile,
            &mut version,
            &artifacts,
            &modern_parent,
            &platform,
            &"a".repeat(32),
            &"b".repeat(40),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .to_string()
        .contains("现代游戏参数"));
        assert!(!root.path().join("libraries").exists());
        let target = root.path().join(&artifacts[1].relative_path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"user bytes").unwrap();
        assert!(finish_legacy_install(
            root.path(),
            stage.path(),
            &profile,
            &mut version,
            &artifacts,
            &parent,
            &platform,
            &"a".repeat(32),
            &"b".repeat(40),
            &AtomicBool::new(false)
        )
        .is_err());
        assert_eq!(fs::read(&target).unwrap(), b"user bytes");
        assert!(!root.path().join(&artifacts[0].relative_path).exists());
        let dir = root.path().join("versions/1.7.10-forge-10.13.4.1614");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("notes.txt"), b"existing notes").unwrap();
        assert!(finish_legacy_install(
            root.path(),
            stage.path(),
            &profile,
            &mut version,
            &artifacts,
            &parent,
            &platform,
            &"a".repeat(32),
            &"b".repeat(40),
            &AtomicBool::new(false)
        )
        .is_err());
        assert_eq!(fs::read(dir.join("notes.txt")).unwrap(), b"existing notes");
    }

    #[test]
    fn legacy_url_rewrite_only_accepts_exact_official_hosts() {
        assert_eq!(
            legacy_library_url("http://files.minecraftforge.net/maven/a/b.jar").unwrap(),
            "https://maven.minecraftforge.net/a/b.jar"
        );
        assert_eq!(
            legacy_library_url("http://libraries.minecraft.net/a.jar").unwrap(),
            "https://libraries.minecraft.net/a.jar"
        );
        for value in [
            "http://files.minecraftforge.net.attacker.invalid/maven/a.jar",
            "https://files.minecraftforge.net/a.jar",
            "https://user@libraries.minecraft.net/a.jar",
            "file:///a.jar",
            "https://libraries.minecraft.net:444/a.jar",
        ] {
            assert!(legacy_library_url(value).is_err(), "{value}");
        }
    }

    fn middle_fixture() -> (Value, Value) {
        (
            serde_json::from_str(include_str!(
                "../tests/fixtures/forge/middle-install-profile.synthetic.json"
            ))
            .unwrap(),
            serde_json::from_str(include_str!(
                "../tests/fixtures/forge/middle-version.synthetic.json"
            ))
            .unwrap(),
        )
    }

    fn inert_jar(label: &str) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file("fixture.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(label.as_bytes()).unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn middle_archive(
        stage: &Path,
        profile: &Value,
        mut version: Value,
        extra: Option<(&str, bool)>,
    ) -> zip::ZipArchive<File> {
        let jars = [
            inert_jar("synthetic Forge; never executed"),
            inert_jar("synthetic ASM; never executed"),
        ];
        // The first embedded library has official-style download metadata; the
        // second intentionally has no standalone digest. Both are inert fixtures.
        let path = maven_path(version["libraries"][0]["name"].as_str().unwrap()).unwrap();
        version["libraries"][0]["downloads"] = json!({"artifact":{
            "path":path.strip_prefix("libraries").unwrap().to_string_lossy().replace('\\',"/"),
            "sha1":format!("{:x}",Sha1::digest(&jars[0])),"size":jars[0].len(),"url":""}});
        let target = stage.join("middle-installer.jar");
        let mut writer = zip::ZipWriter::new(File::create(&target).unwrap());
        for (name, bytes) in [
            ("install_profile.json", serde_json::to_vec(profile).unwrap()),
            ("version.json", serde_json::to_vec(&version).unwrap()),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        for (index, bytes) in jars.iter().enumerate() {
            let path = maven_path(version["libraries"][index]["name"].as_str().unwrap()).unwrap();
            let entry = format!(
                "maven/{}",
                path.strip_prefix("libraries")
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            );
            writer
                .start_file(entry, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer
            .start_file(
                "maven/fixture/unused/1/unused-1.jar",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer
            .write_all(b"not declared by client profile; must never be copied")
            .unwrap();
        if let Some((name, symlink)) = extra {
            if symlink {
                writer
                    .add_symlink(
                        name,
                        "../../outside",
                        zip::write::SimpleFileOptions::default(),
                    )
                    .unwrap();
            } else {
                writer
                    .start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(b"extra").unwrap();
            }
        }
        writer.finish().unwrap();
        open_historical_installer(&target, &AtomicBool::new(false)).unwrap()
    }

    #[test]
    fn middle_json_maven_installs_only_declared_client_libraries_and_builds_both_platforms() {
        for (os, spec) in [("windows", 0), ("osx", 1)] {
            let root = tempfile::tempdir().unwrap();
            let stage = tempfile::tempdir().unwrap();
            let (mut profile, source) = middle_fixture();
            profile["spec"] = spec.into();
            let mut archive = middle_archive(stage.path(), &profile, source, None);
            let mut version = middle_profile(
                &mut archive,
                &profile,
                "1.12.2",
                "14.23.5.2860",
                "14.23.5.2860",
                &AtomicBool::new(false),
            )
            .unwrap();
            let platform = Platform {
                os: os.into(),
                arch: "x86_64".into(),
                version: "10.0".into(),
            };
            let parent = synthetic_parent(root.path(), "1.12.2");
            let before = fs::read(root.path().join("versions/1.12.2/1.12.2.json")).unwrap();
            let artifacts = prepare_legacy_libraries(
                root.path(),
                stage.path(),
                &mut archive,
                &profile,
                &mut version,
                &platform,
                &AtomicBool::new(false),
                &legacy_fetch,
                &|_| (),
            )
            .unwrap();
            assert_eq!(artifacts.len(), 3);
            assert_eq!(
                version["libraries"][0]["_pcl_checksum_source"],
                "official-installer-library-sha1"
            );
            assert_eq!(
                version["libraries"][1]["_pcl_checksum_source"],
                "verified-installer-embedded-local-sha1"
            );
            let id = finish_legacy_install(
                root.path(),
                stage.path(),
                &profile,
                &mut version,
                &artifacts,
                &parent,
                &platform,
                &"a".repeat(32),
                &"b".repeat(40),
                &AtomicBool::new(false),
            )
            .unwrap();
            assert_eq!(version["_pcl_forge_install"]["format"], "legacy-json-maven");
            assert_eq!(version["_pcl_forge_install"]["processorCount"], 0);
            assert!(!root.path().join("libraries/fixture").exists());
            assert_eq!(
                fs::read(root.path().join("versions/1.12.2/1.12.2.json")).unwrap(),
                before
            );
            fs::write(root.path().join("not-java"), b"not executed").unwrap();
            let plan = crate::launch::build_plan(
                &crate::launch::LaunchOptions {
                    root: root.path().into(),
                    version_id: id,
                    java: root.path().join("not-java"),
                    memory_mb: 1024,
                    width: 854,
                    height: 480,
                },
                &crate::auth::offline_session("MiddleFixture").unwrap(),
                &platform,
            )
            .unwrap();
            assert!(plan.args.windows(2).any(|v| v
                == [
                    "--tweakClass",
                    "net.minecraftforge.fml.common.launcher.FMLTweaker"
                ]));
            assert!(plan
                .args
                .windows(2)
                .any(|v| v == ["--username", "MiddleFixture"]));
            let classpath = &plan.args.windows(2).find(|v| v[0] == "-cp").unwrap()[1];
            for name in [
                "1.12.2.jar",
                "forge-1.12.2-14.23.5.2860.jar",
                "asm-all-5.2.jar",
                "launchwrapper-1.12.jar",
            ] {
                assert!(classpath.contains(name));
            }
            assert!(!classpath.contains("unused") && !classpath.contains("server-only"));
        }
    }

    #[test]
    fn middle_profile_rejects_client_processors_unknown_spec_and_mismatched_identity() {
        for (key, value) in [
            ("spec", json!(2)),
            ("processors", json!([{"sides":["client"]}])),
            ("processors", json!([{"jar":"fixture:unknown:1"}])),
            ("minecraft", json!("1.7.10")),
            ("version", json!("different-id")),
            (
                "path",
                json!("net.minecraftforge:forge:1.12.2-14.23.5.2859"),
            ),
            ("json", json!("/../version.json")),
            ("hideClient", json!(true)),
        ] {
            let stage = tempfile::tempdir().unwrap();
            let (mut profile, version) = middle_fixture();
            profile[key] = value;
            let mut archive = middle_archive(stage.path(), &profile, version, None);
            assert!(
                middle_profile(
                    &mut archive,
                    &profile,
                    "1.12.2",
                    "14.23.5.2860",
                    "14.23.5.2860",
                    &AtomicBool::new(false)
                )
                .is_err(),
                "{key}"
            );
        }
        let stage = tempfile::tempdir().unwrap();
        let (mut profile, version) = middle_fixture();
        profile.as_object_mut().unwrap().remove("spec");
        profile.as_object_mut().unwrap().remove("processors");
        let mut archive = middle_archive(stage.path(), &profile, version, None);
        middle_profile(
            &mut archive,
            &profile,
            "1.12.2",
            "14.23.5.2860",
            "14.23.5.2860",
            &AtomicBool::new(false),
        )
        .unwrap();
    }

    #[test]
    fn middle_maven_rejects_traversal_symlink_and_case_collision_before_writing() {
        for (entry, symlink) in [
            ("maven/../escape.jar", false),
            ("maven/links/evil.jar", true),
            ("maven/org/ow2/asm/asm-all/5.2/ASM-ALL-5.2.JAR", false),
        ] {
            let root = tempfile::tempdir().unwrap();
            let stage = tempfile::tempdir().unwrap();
            let (profile, source) = middle_fixture();
            let mut archive =
                middle_archive(stage.path(), &profile, source, Some((entry, symlink)));
            let mut version = middle_profile(
                &mut archive,
                &profile,
                "1.12.2",
                "14.23.5.2860",
                "14.23.5.2860",
                &AtomicBool::new(false),
            )
            .unwrap();
            assert!(
                prepare_legacy_libraries(
                    root.path(),
                    stage.path(),
                    &mut archive,
                    &profile,
                    &mut version,
                    &Platform::current(),
                    &AtomicBool::new(false),
                    &|_, _| panic!("unsafe archive must fail before network"),
                    &|_| ()
                )
                .is_err(),
                "{entry}"
            );
            assert!(!stage.path().join("libraries").exists());
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn historical_archive_detects_duplicates_hidden_by_zip_index_map() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("duplicate.jar");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        for name in ["aaaa.txt", "bbbb.txt"] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"same inert payload").unwrap();
        }
        writer.finish().unwrap();
        let mut bytes = fs::read(&path).unwrap();
        let positions: Vec<_> = bytes
            .windows(8)
            .enumerate()
            .filter_map(|(i, b)| (b == b"bbbb.txt").then_some(i))
            .collect();
        assert_eq!(positions.len(), 2);
        for i in positions {
            bytes[i..i + 8].copy_from_slice(b"aaaa.txt");
        }
        fs::write(&path, bytes).unwrap();
        assert_eq!(
            zip::ZipArchive::new(File::open(&path).unwrap())
                .unwrap()
                .len(),
            1
        );
        assert!(open_historical_installer(&path, &AtomicBool::new(false))
            .unwrap_err()
            .to_string()
            .contains("重复"));
    }

    #[test]
    fn middle_embedded_metadata_mismatch_and_cancellation_do_not_register() {
        for cancelled_fetch in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let stage = tempfile::tempdir().unwrap();
            let (profile, source) = middle_fixture();
            let mut archive = middle_archive(stage.path(), &profile, source, None);
            let mut version = middle_profile(
                &mut archive,
                &profile,
                "1.12.2",
                "14.23.5.2860",
                "14.23.5.2860",
                &AtomicBool::new(false),
            )
            .unwrap();
            if !cancelled_fetch {
                // Alternatives cannot override a standard artifact digest mismatch.
                version["libraries"][0]["checksums"] =
                    json!([version["libraries"][0]["downloads"]["artifact"]["sha1"].clone()]);
                version["libraries"][0]["downloads"]["artifact"]["sha1"] = "0".repeat(40).into();
            }
            let cancel = AtomicBool::new(false);
            let result = prepare_legacy_libraries(
                root.path(),
                stage.path(),
                &mut archive,
                &profile,
                &mut version,
                &Platform::current(),
                &cancel,
                &|url, hash| {
                    assert!(
                        cancelled_fetch,
                        "bad embedded hash must fail before external network"
                    );
                    if !url.ends_with(".sha1") {
                        cancel.store(true, Ordering::Relaxed);
                    }
                    legacy_fetch(url, hash)
                },
                &|_| (),
            );
            let error = result.unwrap_err();
            if cancelled_fetch {
                assert!(error
                    .downcast_ref::<crate::model::OperationCancelled>()
                    .is_some());
            } else {
                assert!(error.to_string().contains("SHA1"));
            }
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn middle_commit_preserves_conflicting_user_library_and_can_reuse_identical_files() {
        let root = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir().unwrap();
        let (profile, source) = middle_fixture();
        let mut archive = middle_archive(stage.path(), &profile, source, None);
        let mut version = middle_profile(
            &mut archive,
            &profile,
            "1.12.2",
            "14.23.5.2860",
            "14.23.5.2860",
            &AtomicBool::new(false),
        )
        .unwrap();
        let platform = Platform::current();
        let parent = synthetic_parent(root.path(), "1.12.2");
        let artifacts = prepare_legacy_libraries(
            root.path(),
            stage.path(),
            &mut archive,
            &profile,
            &mut version,
            &platform,
            &AtomicBool::new(false),
            &legacy_fetch,
            &|_| (),
        )
        .unwrap();
        let path = root.path().join(&artifacts[1].relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"user ASM").unwrap();
        assert!(finish_legacy_install(
            root.path(),
            stage.path(),
            &profile,
            &mut version,
            &artifacts,
            &parent,
            &platform,
            &"a".repeat(32),
            &"b".repeat(40),
            &AtomicBool::new(false)
        )
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), b"user ASM");
        assert!(!root.path().join(&artifacts[0].relative_path).exists());
        fs::write(
            &path,
            fs::read(stage.path().join(&artifacts[1].relative_path)).unwrap(),
        )
        .unwrap();
        let id = finish_legacy_install(
            root.path(),
            stage.path(),
            &profile,
            &mut version,
            &artifacts,
            &parent,
            &platform,
            &"a".repeat(32),
            &"b".repeat(40),
            &AtomicBool::new(false),
        )
        .unwrap();
        let saved = fs::read(root.path().join(format!("versions/{id}/{id}.json"))).unwrap();
        assert!(finish_legacy_install(
            root.path(),
            stage.path(),
            &profile,
            &mut version,
            &artifacts,
            &parent,
            &platform,
            &"a".repeat(32),
            &"b".repeat(40),
            &AtomicBool::new(false)
        )
        .is_err());
        assert_eq!(
            fs::read(root.path().join(format!("versions/{id}/{id}.json"))).unwrap(),
            saved
        );
    }
    fn fixture(kind: ForgeKind) -> (Value, Value) {
        match kind {
            ForgeKind::Forge => (
                serde_json::from_str(include_str!(
                    "../tests/fixtures/forge/forge-install_profile.json"
                ))
                .unwrap(),
                serde_json::from_str(include_str!("../tests/fixtures/forge/forge-version.json"))
                    .unwrap(),
            ),
            ForgeKind::NeoForge => (
                serde_json::from_str(include_str!(
                    "../tests/fixtures/forge/neoforge-install_profile.json"
                ))
                .unwrap(),
                serde_json::from_str(include_str!(
                    "../tests/fixtures/forge/neoforge-version.json"
                ))
                .unwrap(),
            ),
        }
    }
    #[test]
    fn neoforge_official_calendar_versions_keep_exact_minecraft_hotfix() {
        let text = include_str!("../tests/fixtures/forge/neoforge-calver-metadata.xml");
        for (minecraft, expected) in [
            (
                "26.3",
                vec!["26.3.0.48-beta", "26.3.0.47-beta", "26.3.0.9-beta"],
            ),
            ("26.2", vec!["26.2.0.88"]),
            ("26.1.2", vec!["26.1.2.114"]),
            ("26.1.1", vec!["26.1.1.15-beta"]),
            // A release must not include +snapshot / +pre builds sharing its prefix.
            ("26.1", vec!["26.1.0.19-beta"]),
            ("1.21.1", vec!["21.1.255"]),
            ("1.21", vec!["21.0.167"]),
            ("26.3.1", vec![]),
            ("26.30", vec![]),
        ] {
            let prefix = maven_version_prefix(ForgeKind::NeoForge, minecraft).unwrap();
            assert_eq!(
                parse_maven_versions(ForgeKind::NeoForge, minecraft, &prefix, text).unwrap(),
                expected,
                "Minecraft {minecraft}"
            );
        }
    }

    #[test]
    fn maven_version_lists_preserve_forge_early_neo_and_invalid_entry_errors() {
        let text = "<metadata><version>1.20.1-47.1.106</version>\
                    <version>1.20.1-47.1.99</version>\
                    <version>1.20.1-47.1.106</version>\
                    <version>1.20.10-47.1.107</version></metadata>";
        for kind in [ForgeKind::Forge, ForgeKind::NeoForge] {
            let prefix = maven_version_prefix(kind, "1.20.1").unwrap();
            assert_eq!(
                parse_maven_versions(kind, "1.20.1", &prefix, text).unwrap(),
                ["47.1.106", "47.1.99"]
            );
        }
        assert!(parse_maven_versions(
            ForgeKind::NeoForge,
            "26.3",
            "26.3.0.",
            "<metadata><version>26.3.0.1/bad</version></metadata>"
        )
        .is_err());
    }

    #[test]
    fn unknown_neoforge_minecraft_names_are_empty_but_cancellation_is_not_success() {
        for minecraft in ["24w14a", "26.1-snapshot-1", "1.21-pre1", "2.0", "26.3.1.2"] {
            assert!(maven_version_prefix(ForgeKind::NeoForge, minecraft).is_none());
            // Unmapped names do not start a request or invent compatible builds.
            assert!(
                list_versions(ForgeKind::NeoForge, minecraft, &AtomicBool::new(false))
                    .unwrap()
                    .is_empty()
            );
        }
        let error = list_versions(ForgeKind::NeoForge, "26.3", &AtomicBool::new(true)).unwrap_err();
        assert!(error.is::<crate::model::OperationCancelled>());
        assert!(list_versions(ForgeKind::NeoForge, "../26.3", &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn loader_version_sort_is_numeric_newest_first() {
        let mut values = vec![
            "52.0.9",
            "52.1.2",
            "52.0.30",
            "52.1.16",
            "52.1.10",
            "52.1.16-beta",
        ];
        values.sort_by(|a, b| compare_loader_versions(b, a));
        assert_eq!(
            values,
            vec![
                "52.1.16",
                "52.1.16-beta",
                "52.1.10",
                "52.1.2",
                "52.0.30",
                "52.0.9"
            ]
        );
    }
    #[test]
    fn official_profiles_select_client_side_and_deduplicate_dependencies() {
        for (kind, loader, count) in [
            (ForgeKind::Forge, "52.1.16", 3),
            (ForgeKind::NeoForge, "21.1.255", 6),
        ] {
            let (profile, version) = fixture(kind);
            validate_profiles(&profile, &version, kind, "1.21.1", loader).unwrap();
            assert_eq!(client_processors(&profile).unwrap().len(), count);
            let libraries = all_libraries(&profile, &version, &Platform::current()).unwrap();
            assert_eq!(
                libraries
                    .iter()
                    .map(|a| &a.relative_path)
                    .collect::<HashSet<_>>()
                    .len(),
                libraries.len()
            );
            assert!(libraries
                .iter()
                .all(|a| a.sha1.is_some() && a.size.is_some()));
            assert!(validate_profiles(&profile, &version, kind, "1.20.1", loader).is_err());
        }
    }
    #[test]
    fn conflicting_duplicate_library_digest_is_rejected() {
        let (profile, mut version) = fixture(ForgeKind::Forge);
        let mut duplicate = version["libraries"][0].clone();
        duplicate["downloads"]["artifact"]["sha1"] = json!("0".repeat(40));
        version["libraries"].as_array_mut().unwrap().push(duplicate);
        assert!(all_libraries(&profile, &version, &Platform::current()).is_err());
    }
    #[test]
    fn processor_substitution_rejects_traversal_unknown_and_unbounded_values() {
        let tmp = tempfile::tempdir().unwrap();
        let stage = tmp.path().canonicalize().unwrap();
        let data = HashMap::from([
            ("ROOT".into(), stage.to_string_lossy().into_owned()),
            ("HASH".into(), "a".repeat(40)),
        ]);
        assert_eq!(
            maven_path("a.b:c:1:client@zip").unwrap(),
            PathBuf::from("libraries/a/b/c/1/c-1-client.zip")
        );
        assert!(expand("[a.b:c:../../escape]", &data, &stage).is_err());
        assert!(expand("{UNKNOWN}", &data, &stage).is_err());
        assert!(expand("{ROOT}/../escape", &data, &stage).is_err());
        assert!(expand("{ROOT}/x/{MISSING}", &data, &stage).is_err());
        assert!(expand(&"x".repeat(65537), &data, &stage).is_err());
        let outputs = processor_outputs(
            &json!({"outputs":{"{ROOT}/libraries/a.jar":"{HASH}"}}),
            &data,
            &stage,
        )
        .unwrap();
        assert_eq!(
            outputs[0].sha1.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        let relative = Path::new("libraries").join("a.jar");
        assert_eq!(outputs[0].relative_path, relative);
        assert_eq!(
            PathBuf::from(expand("{ROOT}/libraries/a.jar", &data, &stage).unwrap()),
            stage.join(&relative)
        );
        for template in ["{ROOT}/libraries/../../escape", "{ROOT}-sibling/a.jar"] {
            assert!(expand(template, &data, &stage).is_err(), "{template}");
        }
        #[cfg(windows)]
        for template in [r"{ROOT}\..\escape", r"{ROOT}/libraries\..\..\escape"] {
            assert!(expand(template, &data, &stage).is_err(), "{template}");
        }
    }
    #[test]
    fn official_processor_templates_preserve_braces_spaces_and_unicode_in_root() {
        let tmp = tempfile::tempdir().unwrap();
        let stage = tmp.path().join("Minecraft 中文 {modded}");
        fs::create_dir(&stage).unwrap();
        let stage = stage.canonicalize().unwrap();
        for kind in [ForgeKind::Forge, ForgeKind::NeoForge] {
            let (profile, _) = fixture(kind);
            let mut data = HashMap::from([
                ("ROOT".into(), stage.to_string_lossy().into_owned()),
                (
                    "LIBRARY_DIR".into(),
                    stage.join("libraries").to_string_lossy().into_owned(),
                ),
                (
                    "MINECRAFT_JAR".into(),
                    stage
                        .join("versions/1.21.1/1.21.1.jar")
                        .to_string_lossy()
                        .into_owned(),
                ),
                ("MINECRAFT_VERSION".into(), "1.21.1".into()),
                ("SIDE".into(), "client".into()),
            ]);
            for (key, values) in profile["data"].as_object().unwrap() {
                let value = values["client"].as_str().unwrap();
                let resolved = if value.starts_with('[') {
                    expand(value, &HashMap::new(), &stage).unwrap()
                } else if let Some(literal) =
                    value.strip_prefix('\'').and_then(|v| v.strip_suffix('\''))
                {
                    literal.to_owned()
                } else {
                    stage
                        .join(value.trim_start_matches('/'))
                        .to_string_lossy()
                        .into_owned()
                };
                data.insert(key.clone(), resolved);
            }
            for processor in client_processors(&profile).unwrap() {
                for template in processor["args"].as_array().unwrap() {
                    let template = template.as_str().unwrap();
                    let expanded = expand(template, &data, &stage).unwrap();
                    if Path::new(&expanded).is_absolute() {
                        assert!(expanded.contains("Minecraft 中文 {modded}"));
                        confined_output(&stage, &expanded).unwrap();
                    }
                }
                processor_outputs(processor, &data, &stage).unwrap();
            }
            for invalid in ["stray}", "stray}{ROOT}", "{ROOT", "{{ROOT}", "{ROOT}}"] {
                assert!(expand(invalid, &data, &stage).is_err(), "{invalid}");
            }
        }
    }
    #[test]
    fn existing_custom_profile_and_pre_cancelled_install_do_not_touch_files() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("versions/1.21.1-forge-52.1.16");
        fs::create_dir_all(&target).unwrap();
        let bytes = b"{\"arguments\":{\"jvm\":[\"-Duser.flag=true\"]}}";
        fs::write(target.join("1.21.1-forge-52.1.16.json"), bytes).unwrap();
        assert!(install_forge(
            tmp.path(),
            ForgeKind::Forge,
            "1.21.1",
            "52.1.16",
            Path::new("missing-java"),
            &Platform::current(),
            &AtomicBool::new(false),
            |_| ()
        )
        .is_err());
        assert_eq!(
            fs::read(target.join("1.21.1-forge-52.1.16.json")).unwrap(),
            bytes
        );
        let absent_root = tmp.path().join("absent");
        assert!(install_forge(
            &absent_root,
            ForgeKind::Forge,
            "1.21.1",
            "52.1.16",
            Path::new("missing-java"),
            &Platform::current(),
            &AtomicBool::new(true),
            |_| ()
        )
        .is_err());
        assert!(!absent_root.exists());
    }
    #[test]
    fn generated_hash_mismatch_and_existing_user_library_are_preserved() {
        let stage = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let rel = PathBuf::from("libraries/example.jar");
        fs::create_dir(stage.path().join("libraries")).unwrap();
        fs::write(stage.path().join(&rel), b"processor-output").unwrap();
        let artifact = file_artifact(
            stage.path(),
            &stage.path().join(&rel),
            &AtomicBool::new(false),
        )
        .unwrap();
        let mut wrong = artifact.clone();
        wrong.sha1 = Some("0".repeat(40));
        assert!(verify_file(stage.path(), &wrong, &AtomicBool::new(false)).is_err());
        fs::create_dir(root.path().join("libraries")).unwrap();
        fs::write(root.path().join(&rel), b"user library").unwrap();
        assert!(commit_files(
            root.path(),
            stage.path(),
            &[artifact],
            &AtomicBool::new(false)
        )
        .is_err());
        assert_eq!(fs::read(root.path().join(rel)).unwrap(), b"user library");
    }
    #[test]
    fn generated_jar_rejects_zip_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let jar = tmp.path().join("bad.jar");
        let mut writer = zip::ZipWriter::new(File::create(&jar).unwrap());
        writer
            .start_file("../escape.class", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"payload").unwrap();
        writer.finish().unwrap();
        assert!(verify_generated_jar(&jar, &AtomicBool::new(false)).is_err());
    }
    #[test]
    #[cfg(unix)]
    fn running_processor_cancels_and_nonzero_exit_is_visible() {
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let trigger = cancel.clone();
        let thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            trigger.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exec sleep 30"]);
        let start = Instant::now();
        assert!(run_java(command, &cancel, Duration::from_secs(60))
            .unwrap_err()
            .to_string()
            .contains("取消"));
        assert!(start.elapsed() < Duration::from_secs(2));
        thread.join().unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf fixture-failure >&2; exit 7"]);
        assert!(format!(
            "{:#}",
            run_java(command, &AtomicBool::new(false), Duration::from_secs(2)).unwrap_err()
        )
        .contains("fixture-failure"));
    }
}
