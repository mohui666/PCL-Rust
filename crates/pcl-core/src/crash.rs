//! Read-only, bounded crash evidence collection with known-credential redaction.
//! Signatures are derived from fixed upstream ModCrash.vb AnalyzeCrit1/AnalyzeCrit2.
//! A matching log phrase is evidence for a diagnosis, not proof of every causal chain.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

const FILE_LIMIT: u64 = 8 * 1024 * 1024;
const TOTAL_LIMIT: usize = 32 * 1024 * 1024;
const COUNT_LIMIT: usize = 32;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEvidence {
    pub name: String,
    pub text: String,
    pub truncated: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Finding {
    pub code: String,
    pub title: String,
    pub explanation: String,
    pub evidence: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CrashReport {
    pub files: Vec<LogEvidence>,
    pub findings: Vec<Finding>,
    pub warnings: Vec<String>,
}
impl CrashReport {
    pub fn summary(&self) -> String {
        if self.files.is_empty() {
            return "没有找到可分析的日志。可导入 .log、.txt 或日志 ZIP。".into();
        }
        if self.findings.is_empty() {
            return "尚未从日志中识别出明确原因。请结合完整堆栈、最近改动及 Mod 兼容性继续排查。"
                .into();
        }
        self.findings
            .iter()
            .map(|finding| format!("{}\n{}", finding.title, finding.explanation))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// `game_dir` must be the actual directory captured for this launch, not a later selected instance.
/// `since` excludes stale logs during automatic analysis; None is appropriate for explicit manual analysis.
pub fn collect(
    game_dir: &Path,
    since: Option<SystemTime>,
    launcher_output: &[String],
    secrets: &[String],
) -> Result<CrashReport> {
    let root = game_dir.canonicalize().context("游戏目录不可访问")?;
    let mut report = CrashReport::default();
    let mut candidates = Vec::new();
    for relative in ["logs/latest.log", "logs/debug.log"] {
        let path = crate::metadata::confined_path(&root, Path::new(relative))?;
        if path.exists() {
            candidates.push(path);
        }
    }
    for relative in ["crash-reports", ""] {
        let directory = crate::metadata::confined_path(&root, Path::new(relative))?;
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                report
                    .warnings
                    .push(format!("无法读取 {}：{error}", relative));
                continue;
            }
        };
        for entry in entries.take(2049) {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_file()
                && (relative == "crash-reports"
                    && (name.ends_with(".txt") || name.ends_with(".log"))
                    || relative.is_empty()
                        && name.starts_with("hs_err_pid")
                        && name.ends_with(".log"))
            {
                candidates.push(entry.path());
            }
        }
    }
    candidates.sort_by_key(|path| {
        std::cmp::Reverse(fs::metadata(path).and_then(|meta| meta.modified()).ok())
    });
    candidates.dedup();
    for path in candidates {
        if report.files.len() >= COUNT_LIMIT {
            report
                .warnings
                .push("日志超过 32 个，仅收集最新的日志".into());
            break;
        }
        let meta = fs::symlink_metadata(&path)?;
        if let Some(since) = since {
            if meta.modified()?.duration_since(since).is_err() {
                continue;
            }
        }
        let name = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        match read_log(&path, secrets) {
            Ok((text, truncated)) => {
                if report
                    .files
                    .iter()
                    .map(|file| file.text.len())
                    .sum::<usize>()
                    + text.len()
                    > TOTAL_LIMIT
                {
                    report
                        .warnings
                        .push("日志总量超过 32 MiB，剩余文件未收集".into());
                    break;
                }
                if !text.is_empty() {
                    report.files.push(LogEvidence {
                        name,
                        text,
                        truncated,
                    });
                }
            }
            Err(error) => report.warnings.push(format!("跳过 {name}：{error:#}")),
        }
    }
    if !launcher_output.is_empty() {
        let mut bytes = 0;
        let mut tail = Vec::new();
        for line in launcher_output.iter().rev().take(1000) {
            bytes += line.len();
            if bytes > 1024 * 1024 {
                break;
            }
            tail.push(line.as_str());
        }
        tail.reverse();
        report.files.push(LogEvidence {
            name: "launcher-output.log".into(),
            text: redact(&tail.join("\n"), secrets),
            truncated: tail.len() < launcher_output.len(),
        });
    }
    analyze(&mut report);
    Ok(report)
}

/// Reads logs in memory. ZIP entries are never extracted, and paths/size/symlinks are checked.
pub fn import(path: &Path, secrets: &[String]) -> Result<CrashReport> {
    let mut report = CrashReport::default();
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
    {
        let file = open_regular(path)?;
        if file.metadata()?.len() > 64 * 1024 * 1024 {
            bail!("日志 ZIP 超过 64 MiB");
        }
        let mut zip = ZipArchive::new(file).context("日志 ZIP 无法读取")?;
        if zip.len() > 256 {
            bail!("日志 ZIP 超过 256 个条目");
        }
        let mut names = HashSet::new();
        let mut total = 0_usize;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index)?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().replace('\\', "/");
            crate::metadata::safe_relative(&name)?;
            if !names.insert(name.clone()) {
                bail!("日志 ZIP 包含重复路径：{name}");
            }
            if entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                bail!("日志 ZIP 包含符号链接：{name}");
            }
            if !is_log_name(&name) {
                continue;
            }
            if report.files.len() >= COUNT_LIMIT {
                bail!("日志 ZIP 超过 32 个日志文件");
            }
            if entry.size() > FILE_LIMIT {
                bail!("日志 ZIP 单文件超过 8 MiB：{name}");
            }
            let mut bytes = Vec::new();
            (&mut entry).take(FILE_LIMIT + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > FILE_LIMIT {
                bail!("日志 ZIP 条目超过声明上限");
            }
            total += bytes.len();
            if total > TOTAL_LIMIT {
                bail!("日志 ZIP 解压内容超过 32 MiB");
            }
            report.files.push(LogEvidence {
                name,
                text: redact(&decode_text(&bytes), secrets),
                truncated: false,
            });
        }
    } else {
        if !is_log_name(&path.to_string_lossy()) {
            bail!("请选择 .log、.txt 或日志 ZIP");
        }
        let (text, truncated) = read_log(path, secrets)?;
        report.files.push(LogEvidence {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            text,
            truncated,
        });
    }
    analyze(&mut report);
    Ok(report)
}

/// Writes a newly named report ZIP using a same-directory temporary file; never overwrites.
/// The caller supplies only already-sanitized report data produced above.
pub fn export(report: &CrashReport, target: &Path) -> Result<PathBuf> {
    let parent = target.parent().context("导出目标没有父目录")?;
    if target.exists() {
        bail!("导出目标已存在，未覆盖");
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut zip = ZipWriter::new(temporary.as_file_mut());
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("analysis.txt", options)?;
        zip.write_all(report.summary().as_bytes())?;
        zip.start_file("report.json", options)?;
        serde_json::to_writer_pretty(&mut zip, report)?;
        for (index, file) in report.files.iter().enumerate() {
            let base = Path::new(&file.name)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            let name = base
                .chars()
                .filter(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '-' | '_'))
                .take(100)
                .collect::<String>();
            zip.start_file(
                format!(
                    "logs/{index:02}-{}",
                    if name.is_empty() { "log.txt" } else { &name }
                ),
                options,
            )?;
            zip.write_all(file.text.as_bytes())?;
        }
        zip.finish()?;
    }
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(target)
        .map_err(|error| error.error)
        .context("无法提交报告，目标未覆盖")?;
    Ok(target.to_path_buf())
}

fn is_log_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".log") || lower.ends_with(".txt")
}
fn open_regular(path: &Path) -> Result<File> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || before.file_type().is_symlink() {
        bail!("日志不是普通文件");
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(target_os = "macos")]
        const NOFOLLOW: i32 = 0x100;
        #[cfg(not(target_os = "macos"))]
        const NOFOLLOW: i32 = 0x20000;
        options.custom_flags(NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    let file = options.open(path)?;
    let current = file.metadata()?;
    if !current.is_file()
        || current.len() != before.len()
        || current.modified().ok() != before.modified().ok()
    {
        bail!("日志在读取前发生变化，请重试");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if current.dev() != before.dev() || current.ino() != before.ino() {
            bail!("日志在读取前被替换");
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if current.file_attributes() & 0x400 != 0 {
            bail!("日志不能是重解析点");
        }
    }
    Ok(file)
}
fn read_log(path: &Path, secrets: &[String]) -> Result<(String, bool)> {
    use std::io::{Seek, SeekFrom};
    let mut file = open_regular(path)?;
    let length = file.metadata()?.len();
    let truncated = length > FILE_LIMIT;
    if truncated {
        file.seek(SeekFrom::End(-(FILE_LIMIT as i64)))?;
    }
    let mut bytes = Vec::new();
    file.take(FILE_LIMIT).read_to_end(&mut bytes)?;
    Ok((redact(&decode_text(&bytes), secrets), truncated))
}
fn decode_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xff, 0xfe]) {
        let words: Vec<_> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&words)
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        let words: Vec<_> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&words)
    } else {
        String::from_utf8_lossy(bytes)
            .trim_start_matches('\u{feff}')
            .to_string()
    }
}

pub fn redact(text: &str, secrets: &[String]) -> String {
    use std::sync::OnceLock;
    static BEARER: OnceLock<regex::Regex> = OnceLock::new();
    static KEYS: OnceLock<regex::Regex> = OnceLock::new();
    static QUOTED: OnceLock<regex::Regex> = OnceLock::new();
    let mut text = text.to_owned();
    for secret in secrets.iter().filter(|secret| secret.len() >= 8) {
        text = text.replace(secret, "<redacted>");
    }
    text = BEARER
        .get_or_init(|| regex::Regex::new(r"(?i)\bBearer[ \t]+[A-Za-z0-9._~+/=-]+").unwrap())
        .replace_all(&text, "Bearer <redacted>")
        .into_owned();
    text = QUOTED.get_or_init(|| regex::Regex::new(r#"(?i)((?:--)?(?:access[_-]?token|refresh[_-]?token|client[_-]?token|client[_-]?secret|xsts[_-]?token|identitytoken|password|passwd|session)["']?\s*(?:[:=]\s*|\s+))(?:(?:"(?:\\.|[^"\\])*?")|(?:'(?:\\.|[^'\\])*?'))"#).unwrap()).replace_all(&text,"${1}<redacted>").into_owned();
    KEYS.get_or_init(||regex::Regex::new(r#"(?i)((?:--)?(?:access[_-]?token|refresh[_-]?token|client[_-]?token|client[_-]?secret|xsts[_-]?token|identitytoken|password|passwd|session)["']?\s*(?:[:=]\s*|\s+)["']?)([^\s"',;\]}]+)"#).unwrap()).replace_all(&text,"${1}<redacted>").into_owned()
}

struct Rule {
    code: &'static str,
    title: &'static str,
    explanation: &'static str,
    patterns: &'static [&'static str],
}
const RULES:&[Rule]=&[
 Rule{code:"java-arguments",title:"Java 虚拟机参数无效",explanation:"日志明确拒绝了 Java 参数。检查自定义 JVM 参数是否适用于所选 Java；不要靠重复添加相同参数修复。",patterns:&["Unrecognized option:","Unrecognized VM option"]},
 Rule{code:"java-too-old",title:"Java 或字节码版本不兼容",explanation:"游戏或 Mod 使用了当前 Java/ASM 无法识别的字节码。按该游戏与加载器要求选择 Java，并检查 Mod 对应的 Minecraft 版本。",patterns:&["UnsupportedClassVersionError","Unsupported class file major version","Unsupported major.minor version","Level is not supported by the active JRE or ASM version"]},
 Rule{code:"java-too-new",title:"旧组件访问了当前 Java 不开放的内部接口",explanation:"检查旧加载器/Mod 与 Java 的兼容范围。通常应选择该版本要求的 Java，或更新相关组件。",patterns:&["because module java.base does not export","java.lang.NoSuchFieldException: ucp","jdk.nashorn.api.scripting.NashornScriptEngineFactory","Unable to make protected final java.lang.Class java.lang.ClassLoader.defineClass"]},
 Rule{code:"openj9",title:"发现 OpenJ9 兼容性线索",explanation:"日志包含 OpenJ9 拒绝信息或内部调用栈。仅出现内部栈不能证明它是根因；请核对组件支持范围，必要时用同主版本 HotSpot Java 对照。",patterns:&["Open J9 is not supported","OpenJ9 is incompatible",".J9VMInternals."]},
 Rule{code:"heap-reserve",title:"JVM 无法保留所需堆内存",explanation:"确认使用 64 位 Java，并检查最大内存、系统空闲内存和虚拟内存。此日志本身不足以断言一定是 32 位 Java。",patterns:&["Invalid maximum heap size","Could not reserve enough space"]},
 Rule{code:"out-of-memory",title:"内存不足",explanation:"日志报告内存分配失败。区分 Java 堆、系统内存和原生内存；根据剩余内存调整游戏分配，关闭不需要的程序或降低资源包负载。",patterns:&["java.lang.OutOfMemoryError","The system is out of physical RAM or swap space","Out of Memory Error","an out of memory error"]},
 Rule{code:"opengl",title:"OpenGL 或显卡驱动初始化失败",explanation:"检查显卡驱动、游戏所需 OpenGL 版本、远程桌面/虚拟机环境和使用的显卡。日志仅证明图形初始化失败。",patterns:&["The driver does not appear to support OpenGL","Couldn't set pixel format","Pixel format not accelerated","GLFW error 65542"]},
 Rule{code:"native-driver",title:"原生图形驱动发生异常",explanation:"JVM 崩溃位置位于显卡驱动模块。尝试更新或回退官方驱动，并排查光影和图形 Mod；不要把整个 JVM 崩溃一概归因于内存。",patterns:&["# C  [ig","# C  [atio","# C  [nvoglv"]},
 Rule{code:"textures",title:"纹理或光影负载出现错误",explanation:"暂时关闭光影、换用较低分辨率资源包并检查图形 Mod 兼容性。",patterns:&["Maybe try a lower resolution resourcepack?","1282: Invalid operation"]},
 Rule{code:"duplicate-mods",title:"重复安装了同一 Mod",explanation:"按日志中的 Mod ID 和 JAR 文件名检查 mods 目录，保留一个与当前版本相符的版本。备份后再移动多余文件。",patterns:&["DuplicateModsFoundException","Found a duplicate mod","Found duplicate mods","ModResolutionException: Duplicate"]},
 Rule{code:"mod-dependencies",title:"Mod 依赖或版本组合不兼容",explanation:"优先阅读加载器提供的依赖版本要求与建议；同时确认 Minecraft、加载器和 Mod 三者版本。",patterns:&["Incompatible mods found!","Missing or unsupported mandatory dependencies","Mod resolution failed","A potential solution has been determined:","A potential solution has been determined, this may resolve your problem:"]},
 Rule{code:"mod-config",title:"Mod 配置文件无法读取",explanation:"备份日志中指出的配置文件后，核对格式或让对应 Mod 重新生成；不要直接删除整个 config 文件夹。",patterns:&["Failed loading config file ","com.electronwill.nightconfig.core.io.ParsingException"]},
 Rule{code:"mod-init",title:"加载器报告 Mod 初始化失败",explanation:"查看证据行和后续 Caused by，确定具体 Mod。初始化异常也可能来自依赖缺失，不能只凭堆栈中出现的 Mod 名称断言责任。",patterns:&["Caught exception from ","Failed to create mod instance.","Failure message:","due to errors, provided by '"]},
 Rule{code:"mixin",title:"Mixin 应用失败",explanation:"检查报错 Mixin 所属的 Mod、目标游戏版本及与其他修改同一类的 Mod 冲突。",patterns:&["Mixin apply failed","MixinApplyError","InvalidMixinException","InjectionError","Critical injection failure"]},
 Rule{code:"mixin-missing",title:"Mixin 启动组件缺失",explanation:"日志无法找到 MixinTweaker。核对旧版 Mod 所要求的 MixinBootstrap 或对应加载器依赖。",patterns:&["ClassNotFoundException: org.spongepowered.asm.launch.MixinTweaker"]},
 Rule{code:"extracted-mod",title:"Mod JAR 被解压",explanation:"Mod 通常应保持原始 JAR 文件，不要解压放入 mods 目录。",patterns:&["The directories below appear to be extracted jar files","Extracted mod jars found"]},
 Rule{code:"forge-incomplete",title:"Forge 安装或启动配置不完整",explanation:"按对应版本重新校验/安装 Forge 所需支持库；保留原版本与用户文件，不覆盖未知修改。",patterns:&["Cannot find launch target fmlclient, unable to launch"]},
 Rule{code:"forge-duplicate",title:"启动参数包含多个 Forge 版本",explanation:"检查版本 JSON 的继承与游戏参数，移除重复的 fml.forgeVersion 来源。",patterns:&["Found multiple arguments for option fml.forgeVersion"]},
 Rule{code:"forge-java",title:"旧 Forge 与当前 Java 内部 API 不兼容",explanation:"为旧 Forge 选择其支持的 Java 更新版本，或更新 Forge；不要盲目添加不安全的访问开关。",patterns:&["NoSuchMethodError: sun.security.util.ManifestEntryVerifier","NoSuchMethodError: 'void sun.security.util.ManifestEntryVerifier"]},
 Rule{code:"optifine-shaders",title:"Shaders Mod 与 OptiFine 重复提供光影功能",explanation:"日志要求移除独立 Shaders Mod。OptiFine 已包含相关光影支持，先备份并禁用冲突项。",patterns:&["Shaders Mod detected. Please remove it, OptiFine has built-in support for shaders."]},
 Rule{code:"optifine-forge",title:"OptiFine/渲染组件与 Forge API 不兼容",explanation:"核对该 OptiFine 发布页标注的 Forge 版本，或禁用 OptiFine 后复测。相关渲染方法缺失也可能来自其他渲染 Mod。",patterns:&["NoSuchMethodError: 'void net.minecraft.client.renderer.texture.SpriteContents.<init>","NoSuchMethodError: 'java.lang.String com.mojang.blaze3d.systems.RenderSystem.getBackendDescription","NoSuchMethodError: 'void net.minecraftforge.client.gui.overlay.ForgeGui.renderSelectedItemName"]},
 Rule{code:"signature",title:"同包类的签名信息不一致",explanation:"检查下载损坏、重复 JAR 或修改过的库，按官方摘要重新校验相关文件。不要直接关闭签名验证来掩盖问题。",patterns:&["signer information does not match signer information of other classes in the same package"]},
 Rule{code:"debug-crash",title:"触发了游戏调试崩溃",explanation:"日志明确记录手动调试崩溃（通常是长按 F3+C），不应据此判断安装损坏。",patterns:&["Manually triggered debug crash"]},
 Rule{code:"id-limit",title:"旧版游戏内容 ID 数量超限",explanation:"检查旧版本 Mod 数量及 ID 扩展兼容性，先备份存档。",patterns:&["maximum id range exceeded"]},
 Rule{code:"mod-name",title:"Mod 文件名无法转换为模块名",explanation:"检查 Mod JAR 文件名中的特殊字符，使用作者原始文件名。",patterns:&["Invalid module name: '' is not a Java identifier"]},
];
fn analyze(report: &mut CrashReport) {
    for rule in RULES {
        let mut evidence = Vec::new();
        for file in &report.files {
            for line in file.text.lines() {
                if rule.patterns.iter().any(|pattern| line.contains(pattern)) {
                    let excerpt = line.chars().take(400).collect::<String>();
                    evidence.push(format!("{}：{}", file.name, excerpt));
                    if evidence.len() >= 4 {
                        break;
                    }
                }
            }
            if evidence.len() >= 4 {
                break;
            }
        }
        if !evidence.is_empty() {
            report.findings.push(Finding {
                code: rule.code.into(),
                title: rule.title.into(),
                explanation: rule.explanation.into(),
                evidence,
            });
        }
    }
    if report.files.iter().any(|file| file.truncated) {
        report
            .warnings
            .push("部分日志仅保留最后 8 MiB，早期错误可能缺失".into());
    }
    report.warnings.push("仅已隐藏已知凭据和常见敏感字段。报告仍可能包含用户名、路径、服务器地址和 Mod 内容；分享前请检查。".into());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signatures_require_evidence_and_unknown_is_not_invented() {
        let mut report=CrashReport{files:vec![LogEvidence{name:"latest.log".into(),text:"java.lang.OutOfMemoryError: Java heap space\nFound duplicate mods\nunknown stack".into(),truncated:false}],..Default::default()};
        analyze(&mut report);
        assert_eq!(report.findings.len(), 2);
        assert!(report
            .findings
            .iter()
            .all(|finding| !finding.evidence.is_empty()));
        let mut unknown = CrashReport {
            files: vec![LogEvidence {
                name: "x.log".into(),
                text: "exit -1 unknown".into(),
                truncated: false,
            }],
            ..Default::default()
        };
        analyze(&mut unknown);
        assert!(unknown.findings.is_empty());
    }
    #[test]
    fn tokens_are_redacted_in_embedded_flags_json_and_authorization() {
        let text = r#"--accessToken=actual-secret-token {"refresh_token":"refresh-secret","clientSecret":"oauth-secret"} Authorization: Bearer jwt.secret.value embedded(actual-secret-token) --session another-session"#;
        let clean = redact(text, &["actual-secret-token".into()]);
        let quoted = redact(
            r#"--password "multiple words secret" {"client_secret":"embedded \" quote value"}"#,
            &[],
        );
        for part in ["multiple", "words", "embedded", "quote value"] {
            assert!(!quoted.contains(part), "{quoted}");
        }
        for secret in [
            "actual-secret-token",
            "refresh-secret",
            "oauth-secret",
            "jwt.secret.value",
            "another-session",
        ] {
            assert!(!clean.contains(secret), "{clean}");
        }
    }
    #[test]
    fn collect_uses_only_actual_game_dir_and_rejects_symlink_logs() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("logs")).unwrap();
        fs::write(
            dir.path().join("logs/latest.log"),
            "java.lang.OutOfMemoryError",
        )
        .unwrap();
        let report = collect(dir.path(), None, &[], &[]).unwrap();
        assert_eq!(report.findings[0].code, "out-of-memory");
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            fs::write(outside.path().join("x.log"), "private").unwrap();
            std::os::unix::fs::symlink(
                outside.path().join("x.log"),
                dir.path().join("logs/debug.log"),
            )
            .unwrap();
            assert!(collect(dir.path(), None, &[], &[]).is_err());
        }
    }
    #[test]
    fn zip_import_never_extracts_and_export_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("input.zip");
        {
            let mut zip = ZipWriter::new(File::create(&source).unwrap());
            zip.start_file("../escape.log", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"test").unwrap();
            zip.finish().unwrap();
        }
        assert!(import(&source, &[]).is_err());
        assert!(!dir.path().parent().unwrap().join("escape.log").exists());
        let log = dir.path().join("test.log");
        fs::write(
            &log,
            "--accessToken secret-value\nUnrecognized option: --bad",
        )
        .unwrap();
        let report = import(&log, &[]).unwrap();
        let output = dir.path().join("report.zip");
        export(&report, &output).unwrap();
        let before = fs::read(&output).unwrap();
        assert!(export(&report, &output).is_err());
        assert_eq!(fs::read(&output).unwrap(), before);
        let roundtrip = import(&output, &[]).unwrap();
        assert!(roundtrip
            .files
            .iter()
            .all(|file| !file.text.contains("secret-value")));
        assert!(!roundtrip.findings.is_empty());
    }
    #[test]
    fn stale_logs_are_excluded_and_utf16_is_decoded() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("logs")).unwrap();
        let text = "Found duplicate mods";
        let mut bytes = vec![0xff, 0xfe];
        for word in text.encode_utf16() {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        fs::write(dir.path().join("logs/latest.log"), bytes).unwrap();
        assert_eq!(
            collect(dir.path(), None, &[], &[]).unwrap().findings[0].code,
            "duplicate-mods"
        );
        let future = SystemTime::now() + std::time::Duration::from_secs(2);
        assert!(collect(dir.path(), Some(future), &[], &[])
            .unwrap()
            .files
            .is_empty());
    }
}
