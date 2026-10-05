//! Versioned, bundled compatibility patches. Pack metadata never supplies code here.
use crate::{metadata::confined_path, model::Platform};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const WRAPPER: &[u8] = include_bytes!("../assets/launch/JavaWrapper.jar");
const UNSAFE_AGENT: &[u8] = include_bytes!("../assets/launch/LwjglUnsafeAgent.jar");

#[derive(Clone, Copy, Default)]
pub struct PatchOptions {
    pub disable_java_wrapper: bool,
    pub disable_lwjgl_unsafe_agent: bool,
}

/// Called before the game's main class is appended, so the wrapper retains all
/// existing JVM options and the exact classpath/main-class argument boundary.
pub fn apply(
    root: &Path,
    jvm: &mut Vec<String>,
    libraries: &[String],
    java: u32,
    platform: &Platform,
    options: PatchOptions,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let user_agent = jvm.iter().any(|s| s.starts_with("-javaagent:"));
    // JLW fixes Windows JDK-8272352, fixed in Java 19. It cannot repair agent
    // classpaths, so preserve custom agents and report the skipped workaround.
    let wrapper = platform.os == "windows"
        && (6..19).contains(&java)
        && windows_code_page() != 936
        && !options.disable_java_wrapper;
    if wrapper && user_agent {
        warnings.push("已有自定义 Java Agent，未启用 Java Launch Wrapper 编码补丁。".into());
    }
    if !options.disable_lwjgl_unsafe_agent && libraries.iter().any(|s| s == "org.lwjgl:lwjgl:3.4.1")
    {
        if java >= 25 {
            let path = bundled(root, "LwjglUnsafeAgent.jar", UNSAFE_AGENT)?;
            jvm.push(format!("-javaagent:{}", path.display()));
        } else {
            warnings.push("LWJGL Unsafe Agent 需要 Java 25 或更高版本，本次未注入。".into());
        }
    }
    if wrapper && !user_agent {
        let path = bundled(root, "JavaWrapper.jar", WRAPPER)?;
        let path = wrapper_path(&path)?;
        if java >= 9 {
            jvm.extend([
                "--add-exports".into(),
                "cpw.mods.bootstraplauncher/cpw.mods.bootstraplauncher=ALL-UNNAMED".into(),
            ]);
        }
        jvm.push(format!(
            "-Doolloo.jlw.tmpdir={}",
            path.parent().context("补丁路径无父目录")?.display()
        ));
        jvm.extend(["-jar".into(), path.to_string_lossy().into_owned()]);
    }
    Ok(warnings)
}

fn bundled(root: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let digest = format!("{:x}", Sha256::digest(bytes));
    let folder = Path::new("PCL-Rust").join("patches").join(&digest);
    let mut current = PathBuf::new();
    for component in folder.components() {
        current.push(component);
        let path = confined_path(root, &current)?;
        match fs::create_dir(&path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                anyhow::ensure!(
                    fs::symlink_metadata(&path)?.is_dir(),
                    "启动补丁目录被非目录占用"
                );
            }
            Err(e) => return Err(e).context("创建启动补丁目录失败"),
        }
    }
    let path = confined_path(root, &folder.join(name))?;
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || meta.len() != bytes.len() as u64
            || fs::read(&path)? != bytes
        {
            bail!("启动补丁路径被其他内容占用，未覆盖：{}", path.display());
        }
        return Ok(path);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&path) {
        Ok(_) => Ok(path),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
            anyhow::ensure!(
                fs::read(&path)? == bytes && !fs::symlink_metadata(&path)?.file_type().is_symlink(),
                "启动补丁发生并发冲突"
            );
            Ok(path)
        }
        Err(e) => Err(e.error).context("写入启动补丁失败"),
    }
}
#[cfg(windows)]
fn windows_code_page() -> u32 {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetACP() -> u32;
    }
    unsafe { GetACP() }
}
#[cfg(not(windows))]
fn windows_code_page() -> u32 {
    65001
}

#[cfg(windows)]
fn wrapper_path(path: &Path) -> Result<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    if path.as_os_str().to_string_lossy().is_ascii() {
        return Ok(path.into());
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetShortPathNameW(long: *const u16, short: *mut u16, size: u32) -> u32;
    }
    let input: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let size = unsafe { GetShortPathNameW(input.as_ptr(), std::ptr::null_mut(), 0) };
    anyhow::ensure!(
        size > 0,
        "Java 编码补丁需要可用的短路径；可在设置中禁用补丁或使用 Java 19+。{}。",
        std::io::Error::last_os_error()
    );
    let mut output = vec![0; size as usize];
    let n = unsafe { GetShortPathNameW(input.as_ptr(), output.as_mut_ptr(), size) };
    anyhow::ensure!(n > 0 && n < size, "获取补丁短路径失败");
    let path = PathBuf::from(std::ffi::OsString::from_wide(&output[..n as usize]));
    anyhow::ensure!(
        path.to_string_lossy().is_ascii(),
        "补丁短路径仍含非 ASCII 字符，请使用 Java 19+ 或禁用编码补丁"
    );
    Ok(path)
}
#[cfg(not(windows))]
fn wrapper_path(path: &Path) -> Result<PathBuf> {
    Ok(path.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn platform(os: &str) -> Platform {
        Platform {
            os: os.into(),
            arch: "x86_64".into(),
            version: "10".into(),
        }
    }
    #[test]
    fn eligibility_disabled_flags_and_main_boundary() {
        let root = tempfile::tempdir().unwrap();
        let mut args = vec!["-cp".into(), "a b.jar".into()];
        apply(
            root.path(),
            &mut args,
            &[],
            17,
            &platform("windows"),
            PatchOptions::default(),
        )
        .unwrap();
        assert_eq!(&args[..2], ["-cp", "a b.jar"]);
        assert_eq!(args[args.len() - 2], "-jar");
        let mut modern = vec![];
        apply(
            root.path(),
            &mut modern,
            &["org.lwjgl:lwjgl:3.4.1".into()],
            25,
            &platform("osx"),
            PatchOptions::default(),
        )
        .unwrap();
        assert_eq!(modern.len(), 1);
        assert!(modern[0].starts_with("-javaagent:"));
        let mut disabled = vec![];
        apply(
            root.path(),
            &mut disabled,
            &["org.lwjgl:lwjgl:3.4.1".into()],
            25,
            &platform("osx"),
            PatchOptions {
                disable_lwjgl_unsafe_agent: true,
                disable_java_wrapper: true,
            },
        )
        .unwrap();
        assert!(disabled.is_empty());
    }
    #[test]
    fn custom_agent_and_incompatible_java_are_not_silently_patched() {
        let root = tempfile::tempdir().unwrap();
        let mut args = vec!["-javaagent:custom.jar".into()];
        assert_eq!(
            apply(
                root.path(),
                &mut args,
                &[],
                17,
                &platform("windows"),
                PatchOptions::default()
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(args, ["-javaagent:custom.jar"]);
        let mut args = vec![];
        assert_eq!(
            apply(
                root.path(),
                &mut args,
                &["org.lwjgl:lwjgl:3.4.1".into()],
                21,
                &platform("osx"),
                PatchOptions::default()
            )
            .unwrap()
            .len(),
            1
        );
        assert!(args.is_empty());
    }
    #[test]
    fn bundled_files_are_exact_and_user_changes_are_not_replaced() {
        let root = tempfile::tempdir().unwrap();
        let path = bundled(root.path(), "JavaWrapper.jar", WRAPPER).unwrap();
        assert_eq!(fs::read(&path).unwrap(), WRAPPER);
        assert_eq!(
            bundled(root.path(), "JavaWrapper.jar", WRAPPER).unwrap(),
            path
        );
        fs::write(&path, b"user content").unwrap();
        assert!(bundled(root.path(), "JavaWrapper.jar", WRAPPER).is_err());
        assert_eq!(fs::read(path).unwrap(), b"user content");
    }
}
