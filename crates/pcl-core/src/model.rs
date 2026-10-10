use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Platform {
    pub os: String,
    pub arch: String,
    pub version: String,
}

impl Platform {
    pub fn current() -> Self {
        Self {
            os: if cfg!(target_os = "macos") {
                "osx"
            } else if cfg!(windows) {
                "windows"
            } else {
                "linux"
            }
            .into(),
            arch: std::env::consts::ARCH.into(),
            version: current_os_version().unwrap_or_default(),
        }
    }
}

#[cfg(windows)]
fn current_os_version() -> Option<String> {
    #[repr(C)]
    struct VersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        service_pack: [u16; 128],
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut VersionInfo) -> i32;
    }
    let mut info = VersionInfo {
        size: std::mem::size_of::<VersionInfo>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        service_pack: [0; 128],
    };
    // RtlGetVersion writes this fixed C structure and reports the actual kernel version,
    // independently of compatibility manifests. It does not create a child process.
    let status = unsafe { RtlGetVersion(&mut info) };
    (status == 0 && info.major > 0).then(|| format!("{}.{}.{}", info.major, info.minor, info.build))
}

#[cfg(not(windows))]
fn current_os_version() -> Option<String> {
    use std::{
        io::Read,
        process::{Command, Stdio},
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };
    #[cfg(target_os = "macos")]
    let (program, argument) = ("/usr/bin/sw_vers", "-productVersion");
    #[cfg(not(target_os = "macos"))]
    let (program, argument) = ("/usr/bin/uname", "-r");
    let mut child = Command::new(program)
        .arg(argument)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut output = String::new();
        let result = stdout.take(256).read_to_string(&mut output).map(|_| output);
        let _ = sender.send(result);
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return None,
            Ok(None) if start.elapsed() < Duration::from_secs(2) => {
                thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = receiver
        .recv_timeout(Duration::from_millis(100))
        .ok()?
        .ok()?;
    let version = output.trim();
    if version.is_empty()
        || !version.bytes().next()?.is_ascii_digit()
        || version.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(version.to_owned())
}

#[derive(Clone, Debug)]
pub struct Artifact {
    pub relative_path: PathBuf,
    pub url: String,
    pub sha1: Option<String>,
    pub size: Option<u64>,
    pub native: bool,
    pub excludes: Vec<String>,
}

/// Never serialize or Debug a session: access tokens remain in memory only.
#[derive(Clone)]
pub struct Session {
    pub username: String,
    pub uuid: String,
    pub access_token: String,
    pub user_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledVersion {
    pub id: String,
    pub kind: String,
    pub required_java: u32,
    pub error: Option<String>,
}

/// A user cancellation observed by an operation, distinct from a coincident network error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperationCancelled;
impl std::fmt::Display for OperationCancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("安装已取消")
    }
}
impl std::error::Error for OperationCancelled {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressStage {
    VersionMetadata,
    AssetIndex,
    CoreLibraries,
    AssetFiles,
    NativeLibraries,
    VersionCommit,
    ExistingVersionValidation,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferProgress {
    /// HTTP response-body bytes actually read during this operation, including metadata.
    /// Cached bytes and local copies are deliberately excluded.
    pub downloaded_bytes: u64,
    /// Monotonic time since this transfer operation started, for delta-based rates.
    pub elapsed_ms: u64,
    /// Files still awaiting integrity verification or download; unknown until planned.
    pub remaining_files: Option<u64>,
    /// In-flight HTTP requests, including response-header waits.
    pub active_downloads: Option<u32>,
    /// Maximum simultaneous HTTP requests for the current operation/stage.
    /// None means unreported; Some(0) means this stage performs only local work.
    pub concurrency_limit: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub message: String,
    /// Original overall file/operation counters; not bytes or per-stage percentages.
    pub completed: u64,
    pub total: u64,
    pub stage: Option<ProgressStage>,
    pub stage_progress: Option<(u64, u64)>,
    /// Present on the first event only, and only when the actual pipeline is known.
    pub plan: Option<Vec<ProgressStage>>,
    pub transfer: Option<TransferProgress>,
}

#[cfg(all(test, any(target_os = "macos", windows)))]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(target_os = "macos", windows))]
    fn current_platform_reports_actual_os_version() {
        let version = Platform::current().version;
        assert!(!version.is_empty(), "当前主机无法读取操作系统版本");
        assert!(version.bytes().next().unwrap().is_ascii_digit());
    }
}
