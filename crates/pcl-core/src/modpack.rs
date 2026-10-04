//! Client-side Modrinth .mrpack import. Game/loader installation is a separate step.
//! Format: https://support.modrinth.com/en/articles/8802351-modrinth-modpack-format-mrpack
use crate::{
    metadata::{safe_relative, validate_id},
    model::Progress,
};
use anyhow::{bail, Context, Result};
use reqwest::{blocking::Client, redirect::Policy, Url};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha512};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use zip::ZipArchive;

const MAX_INDEX: u64 = 8 * 1024 * 1024;
const MAX_FILE: u64 = 2 * 1024 * 1024 * 1024;
const MAX_TOTAL: u64 = 20 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModpackInfo {
    pub name: String,
    pub version_id: String,
    pub summary: Option<String>,
    pub minecraft: String,
    pub dependencies: BTreeMap<String, String>,
    /// Download entries compatible with a client, including optional entries.
    pub files: usize,
    pub optional_files: usize,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    format_version: u32,
    game: String,
    version_id: String,
    name: String,
    summary: Option<String>,
    files: Vec<PackFile>,
    dependencies: BTreeMap<String, String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackFile {
    path: String,
    hashes: BTreeMap<String, String>,
    downloads: Vec<String>,
    file_size: u64,
    env: Option<Environment>,
}

#[derive(Clone, Deserialize)]
struct Environment {
    client: String,
    server: String,
}

impl PackFile {
    fn client_side(&self) -> &str {
        self.env
            .as_ref()
            .map_or("required", |env| env.client.as_str())
    }
}

#[derive(Clone)]
enum Payload {
    Download(PackFile),
    Override { index: usize, size: u64 },
}

struct Prepared {
    info: ModpackInfo,
    files: BTreeMap<String, (PathBuf, Payload)>,
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(crate::model::OperationCancelled.into());
    }
    Ok(())
}

fn approved_url(url: &Url, redirect: bool) -> bool {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port().is_some_and(|port| port != 443)
    {
        return false;
    }
    match url.host_str() {
        Some("cdn.modrinth.com" | "github.com" | "raw.githubusercontent.com" | "gitlab.com") => {
            url.query().is_none()
        }
        // GitHub itself adds short-lived signatures when redirecting release downloads.
        // Index URLs may not supply these, credentials, arbitrary hosts, or query tokens.
        Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com") => redirect,
        _ => false,
    }
}

fn validate_download_url(value: &str) -> Result<Url> {
    if value.chars().any(char::is_whitespace) {
        bail!("下载地址包含未编码空白");
    }
    let url = Url::parse(value).map_err(|_| anyhow::anyhow!("整合包下载地址格式无效"))?;
    if !approved_url(&url, false) {
        bail!("整合包下载地址不在可信 HTTPS 白名单内，或包含凭据/查询参数");
    }
    Ok(url)
}

fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent("PCL-Rust/0.1 (local development)")
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("整合包下载重定向次数超限")
            } else if !approved_url(attempt.url(), true) {
                attempt.error("拒绝非可信整合包下载重定向")
            } else {
                attempt.follow()
            }
        }))
        .build()?)
}

fn valid_hash(value: Option<&String>, length: usize) -> bool {
    value.is_some_and(|value| {
        value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn prepare(archive: &mut ZipArchive<File>, include_optional: bool) -> Result<Prepared> {
    if archive.len() > 50_000 {
        bail!("整合包 ZIP 条目过多");
    }
    let index_text = {
        let mut entry = archive
            .by_name("modrinth.index.json")
            .context("缺少 modrinth.index.json")?;
        if entry.size() > MAX_INDEX {
            bail!("整合包索引超过 8 MiB");
        }
        let mut bytes = Vec::new();
        entry.by_ref().take(MAX_INDEX + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_INDEX {
            bail!("整合包索引超过 8 MiB");
        }
        String::from_utf8(bytes).context("整合包索引必须是 UTF-8")?
    };
    let index: Index = serde_json::from_str(&index_text).context("整合包索引格式无效")?;
    if index.format_version != 1 || index.game != "minecraft" {
        bail!("仅支持 formatVersion=1 的 Minecraft mrpack");
    }
    if index.name.trim().is_empty() || index.version_id.trim().is_empty() {
        bail!("整合包名称/版本为空");
    }
    if index.files.len() > 20_000 {
        bail!("整合包文件数量超过限制");
    }
    let minecraft = index
        .dependencies
        .get("minecraft")
        .context("整合包缺少 Minecraft 版本依赖")?
        .clone();
    for version in index.dependencies.values() {
        validate_id(version)?;
    }
    let mut files = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut compatible = 0;
    let mut optional = 0;
    for file in &index.files {
        let relative = safe_relative(&file.path)?;
        let key = file.path.to_ascii_lowercase();
        if !seen.insert(key.clone()) {
            bail!("整合包索引存在重复或大小写冲突的路径");
        }
        if !valid_hash(file.hashes.get("sha1"), 40) || !valid_hash(file.hashes.get("sha512"), 128) {
            bail!("整合包文件必须同时包含有效的 SHA1 和 SHA512");
        }
        if file.file_size > MAX_FILE {
            bail!("整合包单文件超过 2 GiB 限制");
        }
        if let Some(env) = &file.env {
            for side in [&env.client, &env.server] {
                if !matches!(side.as_str(), "required" | "optional" | "unsupported") {
                    bail!("整合包环境类型无效");
                }
            }
        }
        if file.client_side() == "unsupported" {
            continue;
        }
        compatible += 1;
        if file.client_side() == "optional" {
            optional += 1;
            if !include_optional {
                continue;
            }
        }
        if file.downloads.is_empty() || file.downloads.len() > 8 {
            bail!("整合包下载地址数量必须介于 1 和 8");
        }
        for url in &file.downloads {
            validate_download_url(url)?;
        }
        files.insert(key, (relative, Payload::Download(file.clone())));
    }
    let mut layers: [Vec<(String, PathBuf, Payload)>; 2] = [vec![], vec![]];
    let mut layer_seen = [HashSet::new(), HashSet::new()];
    let mut index_count = 0;
    for position in 0..archive.len() {
        let entry = archive.by_index(position)?;
        let raw = entry.name();
        let name = raw.trim_end_matches('/');
        safe_relative(name).context("整合包包含不安全的 ZIP 路径")?;
        let kind = entry.unix_mode().unwrap_or(0) & 0o170000;
        if !matches!(kind, 0 | 0o100000 | 0o040000) {
            bail!("整合包不允许符号链接或特殊文件");
        }
        if name == "modrinth.index.json" {
            index_count += 1;
        }
        if entry.is_dir() {
            continue;
        }
        let (layer, relative) = if let Some(relative) = name.strip_prefix("overrides/") {
            (0, relative)
        } else if let Some(relative) = name.strip_prefix("client-overrides/") {
            (1, relative)
        } else {
            continue;
        };
        let path = safe_relative(relative)?;
        if entry.size() > MAX_FILE {
            bail!("整合包覆盖文件超过 2 GiB");
        }
        let key = relative.to_ascii_lowercase();
        if !layer_seen[layer].insert(key.clone()) {
            bail!("整合包同一覆盖层存在重复或大小写冲突路径");
        }
        layers[layer].push((
            key,
            path,
            Payload::Override {
                index: position,
                size: entry.size(),
            },
        ));
    }
    if index_count != 1 {
        bail!("整合包必须恰好包含一个 modrinth.index.json");
    }
    // Overrides replace only files in our private staging plan. Existing user files
    // are checked separately and are never replaced, including an identical file.
    for layer in layers {
        for (key, relative, payload) in layer {
            files.insert(key, (relative, payload));
        }
    }
    let mut total = 0u64;
    for (relative, payload) in files.values() {
        total = total
            .checked_add(match payload {
                Payload::Download(file) => file.file_size,
                Payload::Override { size, .. } => *size,
            })
            .context("整合包大小溢出")?;
        if total > MAX_TOTAL {
            bail!("整合包总文件大小超过 20 GiB");
        }
        let mut ancestor = relative.parent();
        while let Some(path) = ancestor {
            if files.contains_key(
                &path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_ascii_lowercase(),
            ) {
                bail!("整合包文件与目录路径冲突");
            }
            ancestor = path.parent();
        }
    }
    Ok(Prepared {
        info: ModpackInfo {
            name: index.name,
            version_id: index.version_id,
            summary: index.summary,
            minecraft,
            dependencies: index.dependencies,
            files: compatible,
            optional_files: optional,
        },
        files,
    })
}

pub fn inspect_mrpack(pack: &Path) -> Result<ModpackInfo> {
    let mut archive = ZipArchive::new(File::open(pack)?).context("文件不是有效的 mrpack ZIP")?;
    Ok(prepare(&mut archive, true)?.info)
}

fn checked_target(root: &Path, relative: &Path) -> Result<PathBuf> {
    let mut result = root.to_owned();
    for component in relative.components() {
        result.push(component);
        match fs::symlink_metadata(&result) {
            Ok(meta) if meta.file_type().is_symlink() => bail!("实例目标路径包含符号链接"),
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(result)
}

fn stream_verified(
    mut reader: impl Read,
    target: &Path,
    file: &PackFile,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut output = File::create(target)?;
    let (mut sha1, mut sha512) = (Sha1::new(), Sha512::new());
    let mut total = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        cancelled(cancel)?;
        let count = reader.read(&mut buffer).map_err(|error| {
            // A network reader error may include a signed redirect URL.
            anyhow::anyhow!("读取整合包下载数据失败（{:?}）", error.kind())
        })?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > file.file_size || total > MAX_FILE {
            bail!("整合包下载文件大于声明大小");
        }
        sha1.update(&buffer[..count]);
        sha512.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    cancelled(cancel)?;
    if total != file.file_size {
        bail!("整合包下载文件大小不匹配");
    }
    if format!("{:x}", sha1.finalize()) != file.hashes["sha1"].to_ascii_lowercase()
        || format!("{:x}", sha512.finalize()) != file.hashes["sha512"].to_ascii_lowercase()
    {
        bail!("整合包下载文件 SHA1/SHA512 校验失败");
    }
    output.sync_all()?;
    Ok(())
}

fn download(client: &Client, file: &PackFile, target: &Path, cancel: &AtomicBool) -> Result<()> {
    let mut last_error = None;
    for value in &file.downloads {
        cancelled(cancel)?;
        let url = validate_download_url(value)?;
        let attempt = (|| {
            let response = client.get(url).send().map_err(|error| {
                if error.is_timeout() {
                    anyhow::anyhow!("整合包 HTTPS 下载超时")
                } else if error.is_connect() {
                    anyhow::anyhow!("无法连接整合包 HTTPS 下载服务")
                } else {
                    anyhow::anyhow!("整合包 HTTPS 下载请求失败")
                }
            })?;
            if !response.status().is_success() {
                bail!("整合包下载返回 HTTP {}", response.status().as_u16());
            }
            if !approved_url(response.url(), true) {
                bail!("整合包响应地址不可信");
            }
            if response
                .content_length()
                .is_some_and(|length| length != file.file_size)
            {
                bail!("整合包下载 Content-Length 不匹配");
            }
            stream_verified(response, target, file, cancel)
        })();
        match attempt {
            Ok(()) => return Ok(()),
            Err(error) if cancel.load(Ordering::Relaxed) => return Err(error),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("没有可用的下载地址")))
        .context("所有可信下载地址均失败或哈希不匹配，未导入任何文件")
}

/// Import pack files into an instance without replacing any existing user file.
/// `include_optional` applies to all optional client files; unsupported client files
/// and server-overrides are skipped. Returns dependencies for the caller to install.
pub fn import_mrpack(
    pack: &Path,
    instance_dir: &Path,
    include_optional: bool,
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
) -> Result<ModpackInfo> {
    let client = client()?;
    import_with(
        pack,
        instance_dir,
        include_optional,
        cancel,
        progress,
        |file, target, cancel| download(&client, file, target, cancel),
    )
}

fn import_with(
    pack: &Path,
    instance_dir: &Path,
    include_optional: bool,
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
    fetch: impl Fn(&PackFile, &Path, &AtomicBool) -> Result<()>,
) -> Result<ModpackInfo> {
    cancelled(cancel)?;
    if !instance_dir.is_absolute() {
        bail!("实例目录必须是绝对路径");
    }
    match fs::symlink_metadata(instance_dir) {
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            bail!("实例目录不能是文件或符号链接")
        }
        Ok(_) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let mut archive = ZipArchive::new(File::open(pack)?).context("文件不是有效的 mrpack ZIP")?;
    let prepared = prepare(&mut archive, include_optional)?;
    for (relative, _) in prepared.files.values() {
        let target = checked_target(instance_dir, relative)?;
        if fs::symlink_metadata(target).is_ok() {
            bail!(
                "实例内存在同名文件，导入已中止，原文件未覆盖：{}",
                relative.display()
            );
        }
    }
    let parent = instance_dir.parent().context("实例目录没有父目录")?;
    fs::create_dir_all(parent).context("无法创建实例父目录")?;
    let stage = tempfile::Builder::new()
        .prefix(".pcl-mrpack-")
        .tempdir_in(parent)?;
    let total = prepared.files.len() as u64;
    for (completed, (relative, payload)) in prepared.files.values().enumerate() {
        cancelled(cancel)?;
        progress(Progress {
            message: format!("导入 {}", relative.display()),
            completed: completed as u64,
            total,
            ..Default::default()
        });
        let destination = stage.path().join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        match payload {
            Payload::Download(file) => fetch(file, &destination, cancel)?,
            Payload::Override { index, size } => {
                let mut entry = archive.by_index(*index)?;
                let mut output = File::create(&destination)?;
                let mut copied = 0u64;
                let mut buffer = [0; 64 * 1024];
                loop {
                    cancelled(cancel)?;
                    let count = entry.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    copied += count as u64;
                    if copied > *size || copied > MAX_FILE {
                        bail!("整合包覆盖文件大小超限");
                    }
                    output.write_all(&buffer[..count])?;
                }
                if copied != *size {
                    bail!("整合包覆盖文件不完整");
                }
                output.sync_all()?;
            }
        }
    }
    cancelled(cancel)?;
    let mut created_dirs = Vec::new();
    let mut committed = Vec::new();
    let commit = (|| {
        match fs::create_dir(instance_dir) {
            Ok(()) => created_dirs.push(instance_dir.to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        if fs::symlink_metadata(instance_dir)?.file_type().is_symlink() {
            bail!("实例目录在导入时变成符号链接");
        }
        for (relative, _) in prepared.files.values() {
            cancelled(cancel)?;
            let destination = checked_target(instance_dir, relative)?;
            let mut directory = instance_dir.to_owned();
            if let Some(parent) = relative.parent() {
                for component in parent.components() {
                    directory.push(component);
                    match fs::create_dir(&directory) {
                        Ok(()) => created_dirs.push(directory.clone()),
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            let meta = fs::symlink_metadata(&directory)?;
                            if !meta.is_dir() || meta.file_type().is_symlink() {
                                bail!("导入目标目录发生冲突");
                            }
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            // A link in the same filesystem commits verified bytes without overwriting
            // even if a destination was created after the preflight check.
            fs::hard_link(stage.path().join(relative), &destination)
                .context("导入提交冲突，未覆盖目标文件")?;
            committed.push(destination);
        }
        cancelled(cancel)?;
        Ok::<(), anyhow::Error>(())
    })();
    if let Err(error) = commit {
        let mut rollback_failed = false;
        for path in committed.iter().rev() {
            if fs::remove_file(path).is_err() {
                rollback_failed = true;
            }
        }
        for path in created_dirs.iter().rev() {
            let _ = fs::remove_dir(path);
        }
        if rollback_failed {
            bail!("整合包导入失败且部分新增文件无法回滚；原有文件未覆盖：{error:#}");
        }
        return Err(error);
    }
    progress(Progress {
        message: "整合包文件已导入；仍需安装对应游戏与加载器".into(),
        completed: total,
        total,
        ..Default::default()
    });
    Ok(prepared.info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zip::{write::SimpleFileOptions, ZipWriter};

    fn entry(path: &str, bytes: &[u8], side: &str) -> serde_json::Value {
        json!({"path":path,"hashes":{"sha1":format!("{:x}",Sha1::digest(bytes)),"sha512":format!("{:x}",Sha512::digest(bytes))},
            "env":{"client":side,"server":"unsupported"},"downloads":["https://cdn.modrinth.com/data/test/test.jar"],"fileSize":bytes.len()})
    }
    fn pack(path: &Path, files: Vec<serde_json::Value>, overrides: &[(&str, &[u8])]) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        zip.start_file("modrinth.index.json", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(json!({"formatVersion":1,"game":"minecraft","versionId":"1","name":"Test Pack","files":files,
            "dependencies":{"minecraft":"1.21.1","fabric-loader":"0.16.0"}}).to_string().as_bytes()).unwrap();
        for (name, content) in overrides {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(content).unwrap();
        }
        zip.finish().unwrap();
    }
    #[test]
    fn client_import_verifies_download_and_applies_client_override_layer() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("test.mrpack");
        let target = temp.path().join("instance");
        pack(
            &source,
            vec![
                entry("mods/client.jar", b"verified", "required"),
                entry("mods/server.jar", b"skip", "unsupported"),
                entry("mods/optional.jar", b"skip", "optional"),
            ],
            &[
                ("overrides/config/settings.txt", b"base"),
                ("client-overrides/config/settings.txt", b"client"),
                ("server-overrides/server.txt", b"skip"),
            ],
        );
        let info = import_with(
            &source,
            &target,
            false,
            &AtomicBool::new(false),
            |_| {},
            |file, path, cancel| stream_verified(&b"verified"[..], path, file, cancel),
        )
        .unwrap();
        assert_eq!(info.minecraft, "1.21.1");
        assert_eq!(info.optional_files, 1);
        assert_eq!(
            fs::read(target.join("mods/client.jar")).unwrap(),
            b"verified"
        );
        assert_eq!(
            fs::read(target.join("config/settings.txt")).unwrap(),
            b"client"
        );
        assert!(!target.join("mods/server.jar").exists());
        assert!(!target.join("mods/optional.jar").exists());
        assert!(!target.join("server.txt").exists());
        assert!(source.exists());
    }
    #[test]
    fn sha512_mismatch_does_not_commit_even_if_sha1_matches() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("test.mrpack");
        let target = temp.path().join("instance");
        let mut file = entry("mods/a.jar", b"data", "required");
        file["hashes"]["sha512"] = "0".repeat(128).into();
        pack(&source, vec![file], &[]);
        assert!(import_with(
            &source,
            &target,
            true,
            &AtomicBool::new(false),
            |_| {},
            |file, path, cancel| stream_verified(&b"data"[..], path, file, cancel)
        )
        .is_err());
        assert!(!target.exists());
    }
    #[test]
    fn existing_conflicts_and_cancel_preserve_user_files() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("test.mrpack");
        let target = temp.path().join("instance");
        fs::create_dir_all(target.join("config")).unwrap();
        fs::write(target.join("config/a.txt"), b"user").unwrap();
        pack(&source, vec![], &[("overrides/config/a.txt", b"pack")]);
        assert!(import_with(
            &source,
            &target,
            true,
            &AtomicBool::new(false),
            |_| {},
            |_, _, _| Ok(())
        )
        .is_err());
        assert_eq!(fs::read(target.join("config/a.txt")).unwrap(), b"user");
        pack(&source, vec![entry("mods/a.jar", b"data", "required")], &[]);
        let cancel = AtomicBool::new(false);
        assert!(import_with(
            &source,
            &target,
            true,
            &cancel,
            |_| {},
            |file, path, cancel| {
                stream_verified(&b"data"[..], path, file, cancel)?;
                cancel.store(true, Ordering::Relaxed);
                Ok(())
            }
        )
        .is_err());
        assert!(!target.join("mods/a.jar").exists());
        assert_eq!(fs::read(target.join("config/a.txt")).unwrap(), b"user");
    }
    #[test]
    fn rejects_unsafe_paths_urls_and_missing_hashes() {
        for url in [
            "http://cdn.modrinth.com/a.jar",
            "https://evil.example/a.jar",
            "https://user:token@cdn.modrinth.com/a.jar",
            "https://cdn.modrinth.com/a.jar?token=secret",
            "https://cdn.modrinth.com:444/a.jar",
        ] {
            assert!(validate_download_url(url).is_err());
        }
        assert!(validate_download_url("https://cdn.modrinth.com/data/a/b.jar").is_ok());
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("test.mrpack");
        for name in [
            "overrides/../escape",
            "overrides/C:/escape",
            "overrides/a\\b",
            "overrides/CON.txt",
        ] {
            pack(&source, vec![], &[(name, b"bad")]);
            assert!(inspect_mrpack(&source).is_err());
        }
        let mut file = entry("mods/a.jar", b"data", "required");
        file["hashes"].as_object_mut().unwrap().remove("sha512");
        pack(&source, vec![file], &[]);
        assert!(inspect_mrpack(&source).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn refuses_existing_symlink_directories() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("test.mrpack");
        let target = temp.path().join("instance");
        let outside = temp.path().join("outside");
        fs::create_dir(&target).unwrap();
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, target.join("config")).unwrap();
        pack(&source, vec![], &[("overrides/config/a.txt", b"bad")]);
        assert!(import_with(
            &source,
            &target,
            true,
            &AtomicBool::new(false),
            |_| {},
            |_, _, _| Ok(())
        )
        .is_err());
        assert!(!outside.join("a.txt").exists());
    }
    #[test]
    fn duplicate_case_paths_and_file_directory_collisions_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("test.mrpack");
        pack(
            &source,
            vec![
                entry("mods/A.jar", b"a", "required"),
                entry("mods/a.jar", b"b", "required"),
            ],
            &[],
        );
        assert!(inspect_mrpack(&source).is_err());
        pack(
            &source,
            vec![],
            &[
                ("overrides/config", b"file"),
                ("overrides/config/a.txt", b"nested"),
            ],
        );
        assert!(inspect_mrpack(&source).is_err());
    }

    #[test]
    fn late_commit_conflict_rolls_back_only_our_new_files() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("test.mrpack");
        let target = temp.path().join("instance");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("existing.txt"), b"original").unwrap();
        pack(
            &source,
            vec![
                entry("a.jar", b"data", "required"),
                entry("z.jar", b"data", "required"),
            ],
            &[],
        );
        let result = import_with(
            &source,
            &target,
            true,
            &AtomicBool::new(false),
            |_| {},
            |file, path, cancel| {
                stream_verified(&b"data"[..], path, file, cancel)?;
                if file.path == "z.jar" {
                    // Simulate another writer creating a destination after preflight.
                    fs::write(target.join("z.jar"), b"concurrent-user-file")?;
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!target.join("a.jar").exists());
        assert_eq!(
            fs::read(target.join("z.jar")).unwrap(),
            b"concurrent-user-file"
        );
        assert_eq!(fs::read(target.join("existing.txt")).unwrap(), b"original");
    }

    #[test]
    fn rejects_symlink_entries_inside_zip() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("test.mrpack");
        pack(&source, vec![], &[]);
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&source)
            .unwrap();
        let mut archive = ZipWriter::new_append(file).unwrap();
        archive
            .add_symlink("overrides/link", "../outside", SimpleFileOptions::default())
            .unwrap();
        archive.finish().unwrap();
        assert!(inspect_mrpack(&source).is_err());
    }

    #[test]
    fn mid_stream_cancel_and_network_error_do_not_leak_urls() {
        struct CancelReader<'a>(&'a AtomicBool);
        impl Read for CancelReader<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                buffer[..4].copy_from_slice(b"data");
                self.0.store(true, Ordering::Relaxed);
                Ok(4)
            }
        }
        struct FailingReader;
        impl Read for FailingReader {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other(
                    "https://release-assets.githubusercontent.com/x?jwt=SECRET_TOKEN",
                ))
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let file: PackFile = serde_json::from_value(entry("a.jar", b"data", "required")).unwrap();
        let cancel = AtomicBool::new(false);
        assert!(stream_verified(
            CancelReader(&cancel),
            &temp.path().join("cancel.part"),
            &file,
            &cancel
        )
        .is_err());
        let error = stream_verified(
            FailingReader,
            &temp.path().join("error.part"),
            &file,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(!format!("{error:#}").contains("SECRET_TOKEN"));
        assert!(!format!("{error:#}").contains("https://"));
    }
}
