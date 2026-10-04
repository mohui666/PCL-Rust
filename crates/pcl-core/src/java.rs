//! Bounded discovery of already installed Java runtimes. Downloads live in `java_download`.
use crate::model::Platform;
use anyhow::{bail, Context, Result};
use std::{
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct JavaVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub build: u32,
}

impl JavaVersion {
    pub const fn new(major: u32, minor: u32, patch: u32, build: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            build,
        }
    }
    /// Dotted numeric versions used by the launcher's interval controls.
    pub fn parse(value: &str) -> Result<Self> {
        let values: Vec<_> = value.trim().split('.').collect();
        anyhow::ensure!(
            !values.is_empty() && values.len() <= 4,
            "Java 版本必须有 1 至 4 段数字"
        );
        let mut parts = [0u32; 4];
        for (index, value) in values.iter().enumerate() {
            anyhow::ensure!(
                !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()),
                "Java 版本只能包含数字和小数点"
            );
            parts[index] = value.parse().context("Java 版本数字过大")?;
        }
        Ok(Self::new(parts[0], parts[1], parts[2], parts[3]))
    }
    /// Parse java.version or the version name from Mojang's runtime manifest.
    pub fn from_runtime(value: &str) -> Result<Self> {
        let value = value.trim().strip_prefix("1.").unwrap_or(value.trim());
        let normalized = value
            .split('-')
            .next()
            .unwrap_or(value)
            .replace(['_', '+'], ".");
        let normalized = if let Some((major, update)) = normalized.split_once('u') {
            format!("{major}.0.{update}")
        } else {
            normalized
        };
        let truncated = normalized.split('.').take(4).collect::<Vec<_>>().join(".");
        let version = Self::parse(&truncated)?;
        anyhow::ensure!((5..100).contains(&version.major), "Java 主版本无效");
        Ok(version)
    }
}
impl std::fmt::Display for JavaVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.build != 0 {
            write!(
                f,
                "{}.{}.{}.{}",
                self.major, self.minor, self.patch, self.build
            )
        } else {
            write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct JavaRuntime {
    pub path: PathBuf,
    pub major: u32,
    #[serde(default)]
    pub version: JavaVersion,
    pub architecture: String,
}

pub(crate) fn normalized_architecture(architecture: &str) -> String {
    match architecture.trim().to_ascii_lowercase().as_str() {
        "aarch64" | "arm64" => "aarch64".into(),
        "amd64" | "x86_64" | "x64" => "x86_64".into(),
        "x86" | "i386" | "i486" | "i586" | "i686" => "x86".into(),
        other => other.to_owned(),
    }
}

/// Legacy exact-major validation retained for installer/runtime-download checks.
/// Desktop game launches use `java_selection` and its explicit selection modes.
pub fn validate_for_version(
    runtime: &JavaRuntime,
    required_major: u32,
    platform: &Platform,
) -> Result<()> {
    if runtime.major != required_major {
        bail!(
            "此版本要求 Java {required_major}，当前为 Java {}。当前启动器要求主版本严格匹配；缺少 Java 元数据的历史版本按 Java 8 处理。",
            runtime.major
        );
    }
    validate_architecture(runtime, platform)
}

pub fn validate_architecture(runtime: &JavaRuntime, platform: &Platform) -> Result<()> {
    let runtime_arch = normalized_architecture(&runtime.architecture);
    let platform_arch = normalized_architecture(&platform.arch);
    if runtime_arch.is_empty() || platform_arch.is_empty() || runtime_arch != platform_arch {
        bail!(
            "Java 架构 {} 与当前平台 {} 不匹配；请选择相同架构的 Java。",
            runtime.architecture,
            platform.arch
        );
    }
    Ok(())
}

#[cfg(test)]
fn bounded_output(command: Command, timeout: Duration) -> Result<String> {
    bounded_output_with_cancel(command, timeout, &AtomicBool::new(false))
}

fn bounded_output_with_cancel(
    mut command: Command,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<String> {
    crate::install::cancelled(cancel)?;
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().context("无法启动 Java 探测进程")?;
    let (tx, rx) = mpsc::channel();
    for pipe in [
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
            let mut bytes = Vec::new();
            let result = pipe.take(256 * 1024).read_to_end(&mut bytes);
            let _ = tx.send(result.map(|_| bytes));
        });
    }
    drop(tx);
    let start = Instant::now();
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(crate::model::OperationCancelled.into());
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Java 探测超时（{} 秒）", timeout.as_secs());
        }
        thread::sleep(Duration::from_millis(25));
    };
    let mut text = String::new();
    for _ in 0..2 {
        let bytes = rx
            .recv_timeout(Duration::from_millis(500))
            .context("Java 探测输出读取超时")??;
        text.push_str(&String::from_utf8_lossy(&bytes));
        text.push('\n');
    }
    if !status.success() {
        bail!("Java 探测进程退出异常：{status}");
    }
    Ok(text)
}

fn parse_runtime(path: &Path, output: &str) -> Result<JavaRuntime> {
    let property = |key: &str| -> Option<&str> {
        output.lines().find_map(|line| {
            let (name, value) = line.trim().split_once('=')?;
            (name.trim() == key).then_some(value.trim())
        })
    };
    let version = property("java.version")
        .or_else(|| {
            output.lines().find_map(|line| {
                if line.starts_with("java version ") || line.starts_with("openjdk version ") {
                    line.split('"').nth(1)
                } else {
                    None
                }
            })
        })
        .context("Java 输出中没有可识别的版本")?;
    let version = JavaVersion::from_runtime(version)?;
    let architecture = match property("os.arch").context("Java 输出中没有 os.arch")? {
        "aarch64" | "arm64" => "aarch64",
        "amd64" | "x86_64" | "x64" => "x86_64",
        "x86" | "i386" | "i486" | "i586" | "i686" => "x86",
        other => other,
    }
    .to_owned();
    Ok(JavaRuntime {
        path: path.to_owned(),
        major: version.major,
        version,
        architecture,
    })
}

pub fn inspect_java(path: &Path) -> Result<JavaRuntime> {
    inspect_java_with_cancel(path, &AtomicBool::new(false))
}

pub fn inspect_java_with_cancel(path: &Path, cancel: &AtomicBool) -> Result<JavaRuntime> {
    let mut command = Command::new(path);
    command.args(["-XshowSettings:properties", "-version"]);
    let output = bounded_output_with_cancel(command, Duration::from_secs(5), cancel)
        .with_context(|| format!("无法检查 Java：{}", path.display()))?;
    parse_runtime(path, &output)
}

fn java_bin(home: &Path) -> PathBuf {
    home.join("bin")
        .join(if cfg!(windows) { "java.exe" } else { "java" })
}

// Only descend within known Java installation/runtime directories, with hard bounds.
#[cfg(test)]
fn collect_known_root(root: &Path, depth: usize, budget: &mut usize, out: &mut Vec<PathBuf>) {
    let _ = collect_root(
        root,
        depth,
        budget,
        out,
        &AtomicBool::new(false),
        &mut Vec::new(),
    );
}

fn collect_root(
    root: &Path,
    depth: usize,
    budget: &mut usize,
    out: &mut Vec<PathBuf>,
    cancel: &AtomicBool,
    diagnostics: &mut Vec<String>,
) -> Result<()> {
    crate::install::cancelled(cancel)?;
    if *budget == 0 || depth == 0 {
        diagnostics.push(format!(
            "Java 搜索已到达目录数量或深度限制：{}",
            root.display()
        ));
        return Ok(());
    }
    *budget -= 1;
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            diagnostics.push(format!("无法搜索 Java 目录 {}：{error}", root.display()));
            return Ok(());
        }
    };
    for entry in entries.flatten() {
        crate::install::cancelled(cancel)?;
        if *budget == 0 {
            diagnostics.push(format!("Java 搜索达到目录数量限制：{}", root.display()));
            break;
        }
        if entry.file_name() == if cfg!(windows) { "java.exe" } else { "java" }
            && entry.path().is_file()
        {
            out.push(entry.path());
            continue;
        }
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(".pcl-java-")
            && entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
        {
            collect_root(&entry.path(), depth - 1, budget, out, cancel, diagnostics)?;
        }
    }
    Ok(())
}

pub fn discover_java() -> Vec<JavaRuntime> {
    discover_java_with_cancel(&AtomicBool::new(false))
        .map(|report| report.runtimes)
        .unwrap_or_default()
}

#[derive(Clone, Debug, Default)]
pub struct JavaDiscovery {
    pub runtimes: Vec<JavaRuntime>,
    pub diagnostics: Vec<String>,
}

pub(crate) fn candidate_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = vec![crate::java_download::runtime_root()];
    for key in ["JDK_HOME", "JAVA_HOME"] {
        if let Some(path) = std::env::var_os(key) {
            roots.push(PathBuf::from(path));
        }
    }
    if let Some(home) = dirs::home_dir() {
        for relative in [
            ".jdks",
            ".sdkman/candidates/java",
            ".minecraft/runtime",
            "curseforge/minecraft/Install/runtime",
        ] {
            roots.push(home.join(relative));
        }
        #[cfg(target_os = "macos")]
        for relative in [
            "Library/Java/JavaVirtualMachines",
            "Library/Application Support/minecraft/runtime",
            "Library/Application Support/PrismLauncher/java",
            "Library/Application Support/ModrinthApp/meta/java_versions",
            "Library/Application Support/ATLauncher/runtimes/minecraft",
        ] {
            roots.push(home.join(relative));
        }
    }
    #[cfg(target_os = "macos")]
    roots.extend(
        [
            "/Library/Java/JavaVirtualMachines",
            "/opt/homebrew/opt/openjdk",
            "/usr/local/opt/openjdk",
        ]
        .map(PathBuf::from),
    );
    #[cfg(target_os = "linux")]
    roots.extend([PathBuf::from("/usr/lib/jvm"), PathBuf::from("/usr/java")]);
    #[cfg(windows)]
    {
        for key in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(key) {
                for folder in [
                    "Java",
                    "Eclipse Adoptium",
                    "Amazon Corretto",
                    "BellSoft",
                    "Zulu",
                    "Programs/Eclipse Adoptium",
                    "Minecraft Launcher/runtime",
                    "Minecraft/runtime",
                    ".ftba/bin/runtime",
                ] {
                    roots.push(PathBuf::from(&base).join(folder));
                }
                // Search Microsoft's installed JDKs without recursively walking
                // unrelated Edge/Office application data in the same vendor root.
                if let Ok(entries) = std::fs::read_dir(PathBuf::from(&base).join("Microsoft")) {
                    roots.extend(
                        entries
                            .flatten()
                            .filter(|entry| entry.file_name().to_string_lossy().starts_with("jdk-"))
                            .map(|entry| entry.path()),
                    );
                }
            }
        }
        if let Some(documents) = dirs::document_dir() {
            roots.push(documents.join("Curse/Minecraft/Install/runtime"));
        }
        if let Some(base) = std::env::var_os("LOCALAPPDATA") {
            roots.push(
                PathBuf::from(base).join(
                    "Packages/Microsoft.4297127D64EC6_8wekyb3d8bbwe/LocalCache/Local/runtime",
                ),
            );
        }
        if let Some(base) = std::env::var_os("APPDATA") {
            for folder in [
                ".minecraft/runtime",
                ".hmcl/java",
                "ATLauncher/runtimes/minecraft",
                "ModrinthApp/meta/java_versions",
                "PrismLauncher/java",
            ] {
                roots.push(PathBuf::from(&base).join(folder));
            }
        }
    }
    roots
}

pub fn discover_java_with_cancel(cancel: &AtomicBool) -> Result<JavaDiscovery> {
    crate::install::cancelled(cancel)?;
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        candidates.push(java_bin(Path::new(&home)));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for folder in std::env::split_paths(&path) {
            let bin = folder.join(if cfg!(windows) { "java.exe" } else { "java" });
            // Apple's stub can open a runtime-install dialog; inspect real homes instead.
            if cfg!(target_os = "macos") && bin == Path::new("/usr/bin/java") {
                continue;
            }
            candidates.push(bin);
        }
    }
    let roots = candidate_roots();
    let mut diagnostics = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("/usr/libexec/java_home");
        command.arg("-V");
        if let Ok(output) = bounded_output_with_cancel(command, Duration::from_secs(3), cancel) {
            for line in output.lines() {
                if let Some(index) = line.find('/') {
                    let home = PathBuf::from(line[index..].trim());
                    if home.is_dir() {
                        candidates.push(java_bin(&home));
                    }
                }
            }
        }
    }
    for root in &roots {
        if let Ok(root) = root.canonicalize() {
            collect_root(
                &root,
                16,
                &mut 1024,
                &mut candidates,
                cancel,
                &mut diagnostics,
            )?;
        }
    }
    probe_candidates(candidates, None, cancel, diagnostics)
}

pub fn discover_java_in(root: &Path, cancel: &AtomicBool) -> Result<JavaDiscovery> {
    let root = root.canonicalize().context("Java 搜索目录不存在")?;
    anyhow::ensure!(root.is_dir(), "Java 搜索路径不是目录");
    let mut candidates = Vec::new();
    let mut diagnostics = Vec::new();
    collect_root(
        &root,
        32,
        &mut 8192,
        &mut candidates,
        cancel,
        &mut diagnostics,
    )?;
    probe_candidates(candidates, Some(&root), cancel, diagnostics)
}

fn probe_candidates(
    candidates: Vec<PathBuf>,
    confined: Option<&Path>,
    cancel: &AtomicBool,
    mut diagnostics: Vec<String>,
) -> Result<JavaDiscovery> {
    crate::install::cancelled(cancel)?;
    let mut seen = HashSet::new();
    let mut runtimes = Vec::new();
    // At most 64 actual probes, each bounded; de-duplicate aliases before executing.
    for candidate in candidates {
        crate::install::cancelled(cancel)?;
        let Ok(path) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        if cfg!(target_os = "macos") && path == Path::new("/usr/bin/java") {
            continue;
        }
        if !path.is_file() || !seen.insert(path.clone()) {
            continue;
        }
        if confined.is_some_and(|root| !path.starts_with(root)) {
            diagnostics.push(format!(
                "已跳过指向版本目录外的 Java：{}",
                candidate.display()
            ));
            continue;
        }
        if seen.len() > 64 {
            diagnostics.push("本次 Java 检查达到 64 个候选限制".into());
            break;
        }
        match inspect_java_with_cancel(&path, cancel) {
            Ok(runtime) => runtimes.push(runtime),
            Err(error) => {
                if error
                    .chain()
                    .any(|cause| cause.is::<crate::model::OperationCancelled>())
                {
                    return Err(error);
                }
                diagnostics.push(format!("{error:#}"));
            }
        }
    }
    sort_java_candidates(&mut runtimes);
    Ok(JavaDiscovery {
        runtimes,
        diagnostics,
    })
}

pub fn sort_java_candidates(runtimes: &mut [JavaRuntime]) {
    let roots: Vec<_> = candidate_roots()
        .into_iter()
        .filter_map(|root| root.canonicalize().ok())
        .collect();
    let complete = |runtime: &JavaRuntime| roots.iter().any(|root| runtime.path.starts_with(root));
    runtimes.sort_by(|a, b| {
        complete(b)
            .cmp(&complete(a))
            .then(a.major.abs_diff(21).cmp(&b.major.abs_diff(21)))
            .then_with(|| a.path.cmp(&b.path))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_ignores_uncommitted_runtime_staging() {
        let temp = tempfile::tempdir().unwrap();
        let platform = temp.path().join("mac-os-arm64");
        for name in ["java-runtime-delta-21", ".pcl-java-incomplete"] {
            let home = platform.join(name).join("jre.bundle/Contents/Home");
            std::fs::create_dir_all(home.join("bin")).unwrap();
            std::fs::write(java_bin(&home), b"fixture").unwrap();
        }
        let mut candidates = Vec::new();
        collect_known_root(temp.path(), 8, &mut 256, &mut candidates);
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0]
            .to_string_lossy()
            .contains("java-runtime-delta-21"));
    }

    #[test]
    fn windows_rejects_both_x86_x64_mismatches_and_accepts_aliases() {
        let mut platform = Platform {
            os: "windows".into(),
            arch: "x86_64".into(),
            version: "10.0.26100".into(),
        };
        let mut runtime = JavaRuntime {
            path: "java.exe".into(),
            major: 21,
            version: JavaVersion::new(21, 0, 0, 0),
            architecture: "x86".into(),
        };
        assert!(validate_for_version(&runtime, 21, &platform).is_err());
        runtime.architecture = "amd64".into();
        assert!(validate_for_version(&runtime, 21, &platform).is_ok());
        platform.arch = "x86".into();
        assert!(validate_for_version(&runtime, 21, &platform).is_err());
        runtime.architecture = "i686".into();
        assert!(validate_for_version(&runtime, 21, &platform).is_ok());
    }

    #[test]
    fn validates_exact_java_major_and_arm_aliases() {
        let platform = Platform {
            os: "osx".into(),
            arch: "aarch64".into(),
            version: "15.7.1".into(),
        };
        let runtime = JavaRuntime {
            path: "java".into(),
            major: 21,
            version: JavaVersion::new(21, 0, 0, 0),
            architecture: "ARM64".into(),
        };
        assert!(validate_for_version(&runtime, 21, &platform).is_ok());
        assert!(validate_for_version(&runtime, 17, &platform)
            .unwrap_err()
            .to_string()
            .contains("严格匹配"));
    }

    #[test]
    fn parses_legacy_and_modern_properties() {
        let old = parse_runtime(
            Path::new("/test/java"),
            "    java.version = 1.8.0_442\n    os.arch = amd64\n",
        )
        .unwrap();
        assert_eq!(old.major, 8);
        assert_eq!(old.version, JavaVersion::new(8, 0, 442, 0));
        assert_eq!(old.architecture, "x86_64");
        let modern = parse_runtime(
            Path::new("java"),
            "os.arch = aarch64\njava.specification.version = 21\njava.version = 21.0.6\n",
        )
        .unwrap();
        assert_eq!(modern.major, 21);
        assert_eq!(modern.version, JavaVersion::new(21, 0, 6, 0));
        assert_eq!(modern.architecture, "aarch64");
    }

    #[test]
    fn parses_version_fallback_and_rejects_missing_architecture() {
        assert_eq!(
            parse_runtime(
                Path::new("java"),
                "openjdk version \"25-ea\"\nos.arch = x86_64\n"
            )
            .unwrap()
            .major,
            25
        );
        assert!(parse_runtime(Path::new("java"), "openjdk version \"21\"").is_err());
        assert!(parse_runtime(
            Path::new("java"),
            "java.specification.version = 21\nos.arch = aarch64\n"
        )
        .is_err());
        assert_eq!(
            JavaVersion::from_runtime("21.0.12.1+1-LTS").unwrap(),
            JavaVersion::new(21, 0, 12, 1)
        );
        assert_eq!(
            JavaVersion::from_runtime("1.8.0_141-b15").unwrap(),
            JavaVersion::new(8, 0, 141, 0)
        );
        assert_eq!(
            JavaVersion::from_runtime("8u51-cacert462b08").unwrap(),
            JavaVersion::new(8, 0, 51, 0)
        );
    }

    #[cfg(unix)]
    #[test]
    fn hung_probe_is_terminated() {
        let mut command = Command::new("sleep");
        command.arg("5");
        let start = Instant::now();
        assert!(bounded_output(command, Duration::from_millis(100))
            .unwrap_err()
            .to_string()
            .contains("超时"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn running_probe_cancel_is_typed_and_terminates_its_child() {
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let trigger = cancel.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(75));
            trigger.store(true, Ordering::Relaxed);
        });
        let mut command = Command::new("sleep");
        command.arg("5");
        let start = Instant::now();
        let error =
            bounded_output_with_cancel(command, Duration::from_secs(5), &cancel).unwrap_err();
        thread.join().unwrap();
        assert!(error
            .chain()
            .any(|e| e.is::<crate::model::OperationCancelled>()));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn recursive_version_discovery_preserves_full_patch_and_never_executes_outside_symlink() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let runtime = root
            .path()
            .join("nested folder/runtime/jre.bundle/Contents/Home");
        std::fs::create_dir_all(runtime.join("bin")).unwrap();
        let java = java_bin(&runtime);
        std::fs::write(
            &java,
            "#!/bin/sh\nprintf 'java.version = 17.0.16.1\\nos.arch = aarch64\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&java, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(root.path().join("escape/bin")).unwrap();
        let forbidden = outside.path().join("java");
        std::fs::write(&forbidden, b"must not execute").unwrap();
        symlink(&forbidden, root.path().join("escape/bin/java")).unwrap();
        symlink(outside.path(), root.path().join("directory-link")).unwrap();
        let found = discover_java_in(root.path(), &AtomicBool::new(false)).unwrap();
        assert_eq!(found.runtimes.len(), 1);
        assert_eq!(found.runtimes[0].version, JavaVersion::new(17, 0, 16, 1));
        assert!(found.diagnostics.iter().any(|d| d.contains("目录外")));
    }
}
