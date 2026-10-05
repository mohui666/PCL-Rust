use super::*;

pub(super) struct Extra {
    pub relative: String,
    source: PathBuf,
    original: PathBuf,
    boundary: PathBuf,
    pub stamp: Stamp,
    hashes: (String, String),
    pub permissions: u32,
}
impl Extra {
    fn source(&self) -> Result<&Path> {
        let current = self.original.canonicalize().context("附加导出文件已变化")?;
        ensure!(
            current == self.source && current.starts_with(&self.boundary),
            "附加导出文件链接目标已变化"
        );
        Ok(&self.source)
    }
    pub fn verify(&self, cancel: &AtomicBool) -> Result<()> {
        let (a, b, _) = hash_file(self.source()?, &self.stamp, cancel, false)?;
        ensure!(
            (a, b) == self.hashes,
            "附加导出文件在打包期间发生变化：{}",
            self.relative
        );
        Ok(())
    }
    pub fn copy_to(&self, writer: &mut impl Write, cancel: &AtomicBool) -> Result<()> {
        ensure!(
            copy_verified(self.source()?, &self.stamp, writer, cancel)? == self.hashes,
            "附加导出文件在打包期间发生变化：{}",
            self.relative
        );
        Ok(())
    }
}
fn scan_tree(
    base: &Path,
    boundary: &Path,
    prefix: &str,
    cancel: &AtomicBool,
    selected_executable: Option<&Path>,
) -> Result<Vec<Extra>> {
    let boundary = boundary.canonicalize()?;
    let mut pending = vec![(base.to_owned(), prefix.to_owned(), Vec::<PathBuf>::new())];
    let mut result = Vec::new();
    let mut size = 0u64;
    let mut visited = 0usize;
    while let Some((original, relative, mut ancestors)) = pending.pop() {
        install::cancelled(cancel)?;
        visited += 1;
        ensure!(visited <= 50_000, "附加导出目录条目过多");
        metadata::safe_relative(&relative)?;
        let source = original.canonicalize()?;
        ensure!(
            source.starts_with(&boundary),
            "附加导出文件链接越过选定目录：{relative}"
        );
        let meta = fs::metadata(&source)?;
        if meta.is_dir() {
            ensure!(
                ancestors.len() < 64 && !ancestors.contains(&source),
                "附加导出目录存在循环或层级过深"
            );
            ancestors.push(source);
            for entry in fs::read_dir(&original)? {
                let entry = entry?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("附加导出文件名不是 UTF-8"))?;
                pending.push((
                    entry.path(),
                    format!("{relative}/{name}"),
                    ancestors.clone(),
                ));
            }
        } else {
            ensure!(meta.is_file(), "附加导出目录含特殊文件");
            ensure!(
                selected_executable == Some(original.as_path())
                    || !sensitive_path(
                        relative
                            .strip_prefix(prefix)
                            .unwrap_or(&relative)
                            .trim_start_matches('/')
                    ),
                "附加目录含账户或敏感配置，不会导出：{relative}"
            );
            if relative.to_lowercase().ends_with(".log") || relative.ends_with(".DS_Store") {
                continue;
            }
            let stamp = file_stamp(&source)?;
            size = size.checked_add(stamp.len).context("附加导出大小溢出")?;
            ensure!(
                stamp.len <= MAX_FILE && size <= MAX_TOTAL,
                "附加导出内容超过大小限制"
            );
            let (a, b, sensitive) = hash_file(&source, &stamp, cancel, true)?;
            ensure!(!sensitive, "附加导出配置含敏感字段：{relative}");
            #[cfg(unix)]
            let permissions = {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o111 != 0 {
                    0o755
                } else {
                    0o644
                }
            };
            #[cfg(not(unix))]
            let permissions = 0o644;
            result.push(Extra {
                relative,
                original,
                source,
                boundary: boundary.clone(),
                stamp,
                hashes: (a, b),
                permissions,
            });
        }
    }
    result.sort_by(|a, b| a.relative.cmp(&b.relative));
    check_collisions(&result, std::iter::empty())?;
    Ok(result)
}
pub(super) fn check_collisions<'a>(
    extras: &[Extra],
    paths: impl Iterator<Item = &'a str>,
) -> Result<()> {
    let mut seen = paths.map(str::to_lowercase).collect::<BTreeSet<_>>();
    for file in extras {
        ensure!(
            seen.insert(file.relative.to_lowercase()),
            "Java 或附加文件与导出内容冲突：{}",
            file.relative
        );
    }
    for path in &seen {
        let mut parent = Path::new(path).parent();
        while let Some(value) = parent {
            ensure!(
                !seen.contains(&value.to_string_lossy().to_string()),
                "导出附加文件与目录冲突"
            );
            parent = value.parent();
        }
    }
    Ok(())
}

/// Structural discovery only: never executes a Java binary while browsing/exporting.
/// Only the selected version directory is scanned, with the same explicit folder mode.
pub fn available_java_roots(root: &Path, id: &str) -> Result<Vec<PathBuf>> {
    metadata::validate_id(id)?;
    let settings = config::load_instance_settings(root, id)?;
    if settings.java_mode != Some(crate::java_selection::JavaSelectionMode::VersionFolder) {
        return Ok(Vec::new());
    }
    let version =
        metadata::confined_path(root, &PathBuf::from(format!("versions/{id}")))?.canonicalize()?;
    let mut pending = vec![(version.clone(), 0)];
    let mut roots = Vec::new();
    let mut visited = 0;
    while let Some((path, depth)) = pending.pop() {
        visited += 1;
        ensure!(visited <= 10_000, "版本目录条目过多");
        let java = path.join("bin/java");
        let java_exe = path.join("bin/java.exe");
        if path.join("release").is_file() && (java.is_file() || java_exe.is_file()) {
            roots.push(path);
            continue;
        }
        if depth >= 7 {
            continue;
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let meta = entry.file_type()?;
            if meta.is_dir() && !meta.is_symlink() {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    roots.sort();
    Ok(roots)
}
pub(super) fn java_files(root: &Path, id: &str, cancel: &AtomicBool) -> Result<Vec<Extra>> {
    let roots = available_java_roots(root, id)?;
    ensure!(
        !roots.is_empty(),
        "版本设置必须为使用版本文件夹中的 Java，且目录内应有 bin/java 和 release"
    );
    let version = root.join("versions").join(id).canonicalize()?;
    let mut files = Vec::new();
    for runtime in roots {
        let relative = runtime
            .strip_prefix(&version)?
            .to_string_lossy()
            .replace('\\', "/");
        ensure!(!relative.is_empty(), "Java 不能直接覆盖整个版本目录");
        files.extend(scan_tree(&runtime, &runtime, &relative, cancel, None)?);
    }
    check_collisions(&files, std::iter::empty())?;
    Ok(files)
}

pub fn validate_launcher_export(launcher: &Path) -> Result<()> {
    let launcher = launcher.canonicalize()?;
    if launcher.is_dir() {
        ensure!(
            launcher.extension().is_some_and(|e| e == "app")
                && launcher.join("Contents/Info.plist").is_file(),
            "启动器目录必须为 macOS .app 包"
        );
    } else {
        ensure!(launcher.is_file(), "启动器必须为普通文件或 macOS .app 包");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LauncherPlatform {
    Windows,
    Macos,
    Linux,
}

impl LauncherPlatform {
    fn name(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Macos => "macOS",
            Self::Linux => "Linux",
        }
    }

    fn archive_path(self) -> &'static str {
        match self {
            Self::Windows => "launchers/windows/PCL-Rust.exe",
            Self::Macos => "launchers/macos/PCL-Rust.app",
            Self::Linux => "launchers/linux/PCL-Rust",
        }
    }
}

#[derive(Clone, Debug)]
pub struct LauncherExport {
    pub platform: LauncherPlatform,
    pub path: PathBuf,
}

/// Verify that every platform was explicitly supplied. Never substitute the host
/// executable for a missing target, or execute a supplied program while checking it.
pub fn validate_launchers(launchers: &[LauncherExport]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for launcher in launchers {
        ensure!(
            seen.insert(launcher.platform),
            "重复提供 {} 启动器",
            launcher.platform.name()
        );
    }
    for platform in [
        LauncherPlatform::Windows,
        LauncherPlatform::Macos,
        LauncherPlatform::Linux,
    ] {
        ensure!(seen.contains(&platform), "缺少 {} 启动器", platform.name());
    }
    for launcher in launchers {
        let path = launcher
            .path
            .canonicalize()
            .with_context(|| format!("无法读取 {} 启动器", launcher.platform.name()))?;
        let executable = match launcher.platform {
            LauncherPlatform::Macos => {
                validate_launcher_export(&path)?;
                ensure!(path.is_dir(), "macOS 启动器必须为 .app 包");
                let binary = path
                    .join("Contents/MacOS/pcl-desktop")
                    .canonicalize()
                    .context("macOS 应用缺少 Contents/MacOS/pcl-desktop")?;
                ensure!(
                    binary.starts_with(&path),
                    "macOS 启动器程序链接越过应用目录"
                );
                binary
            }
            LauncherPlatform::Windows => {
                if path.is_dir() {
                    let binary = path
                        .join("PCL-Rust.exe")
                        .canonicalize()
                        .context("Windows 启动器目录缺少 PCL-Rust.exe")?;
                    ensure!(
                        binary.starts_with(&path),
                        "Windows 启动器程序链接越过选定目录"
                    );
                    binary
                } else {
                    ensure!(
                        path.is_file()
                            && path
                                .extension()
                                .is_some_and(|e| e.eq_ignore_ascii_case("exe")),
                        "Windows 启动器必须为 .exe 文件"
                    );
                    path
                }
            }
            LauncherPlatform::Linux => {
                if path.is_dir() {
                    let binary = path
                        .join("PCL-Rust")
                        .canonicalize()
                        .context("Linux 启动器目录缺少 PCL-Rust")?;
                    ensure!(
                        binary.starts_with(&path),
                        "Linux 启动器程序链接越过选定目录"
                    );
                    binary
                } else {
                    ensure!(path.is_file(), "Linux 启动器必须为 ELF 程序文件");
                    path
                }
            }
        };
        let mut header = [0u8; 4];
        File::open(&executable)?
            .read_exact(&mut header)
            .with_context(|| format!("{} 启动器程序文件不完整", launcher.platform.name()))?;
        let matches_platform = match launcher.platform {
            LauncherPlatform::Windows => &header[..2] == b"MZ",
            LauncherPlatform::Linux => header == *b"\x7fELF",
            LauncherPlatform::Macos => matches!(
                header,
                [0xfe, 0xed, 0xfa, 0xce]
                    | [0xce, 0xfa, 0xed, 0xfe]
                    | [0xfe, 0xed, 0xfa, 0xcf]
                    | [0xcf, 0xfa, 0xed, 0xfe]
                    | [0xca, 0xfe, 0xba, 0xbe]
                    | [0xbe, 0xba, 0xfe, 0xca]
                    | [0xca, 0xfe, 0xba, 0xbf]
                    | [0xbf, 0xba, 0xfe, 0xca]
            ),
        };
        ensure!(
            matches_platform,
            "{} 启动器程序格式不匹配",
            launcher.platform.name()
        );
    }
    Ok(())
}

fn validate_bundle_destination(destination: &Path, options: &PackExportOptions) -> Result<()> {
    ensure!(
        options.format == PackFormat::Mrpack,
        "附带启动器只使用内层 mrpack 格式"
    );
    ensure!(
        destination.is_absolute()
            && destination
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("zip")),
        "附带启动器的包必须保存为 .zip"
    );
    require_absent(destination)
}

/// Export all three supplied desktop launchers without modifying their resources.
/// The original macOS fonts and code-signing resources remain byte-for-byte intact.
#[allow(clippy::too_many_arguments)]
pub fn export_pack_with_launchers(
    root: &Path,
    id: &str,
    destination: &Path,
    options: &PackExportOptions,
    launchers: &[LauncherExport],
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
) -> Result<PackExportReport> {
    if !options.include_launcher {
        return export_pack(root, id, destination, options, cancel, progress);
    }
    install::cancelled(cancel)?;
    validate_bundle_destination(destination, options)?;
    validate_launchers(launchers)?;
    let mut files = Vec::new();
    for launcher in launchers {
        let canonical = launcher.path.canonicalize()?;
        let boundary = if canonical.is_dir() {
            canonical.as_path()
        } else {
            canonical.parent().context("启动器缺少父目录")?
        };
        let prefix = if canonical.is_dir() && launcher.platform != LauncherPlatform::Macos {
            launcher.platform.archive_path().rsplit_once('/').unwrap().0
        } else {
            launcher.platform.archive_path()
        };
        // PCL-Rust is normally a private application-data directory name. Here
        // this exact, prevalidated regular file is the selected Linux program;
        // all other paths and every file's content retain the secret checks.
        let selected_executable = (canonical.is_dir()
            && launcher.platform == LauncherPlatform::Linux)
            .then(|| canonical.join("PCL-Rust"));
        let mut entries = scan_tree(
            &canonical,
            boundary,
            prefix,
            cancel,
            selected_executable.as_deref(),
        )?;
        for entry in &mut entries {
            // ZIP modes must work even when the export host is Windows.
            if (launcher.platform == LauncherPlatform::Linux
                && entry.relative == launcher.platform.archive_path())
                || (launcher.platform == LauncherPlatform::Macos
                    && entry.relative.ends_with("/Contents/MacOS/pcl-desktop"))
            {
                entry.permissions = 0o755;
            }
        }
        files.extend(entries);
    }
    check_collisions(&files, std::iter::empty())?;
    write_launcher_bundle(root, id, destination, options, &files, cancel, progress,
        "launchers/windows、launchers/macos、launchers/linux 分别附带 Windows、macOS、Linux 的 PCL-Rust 第三方启动器。请使用对应系统的程序并手动导入 modpack.mrpack；不会自动安装 Java 或运行游戏。账户和全局设置未打包。应用资源与许可文件按原样保留。\n")
}

/// PCL's double ZIP layout: explicit current-platform launcher + modpack.mrpack.
/// No launcher download, credentials, global settings, startup commands or game launch.
#[allow(clippy::too_many_arguments)]
pub fn export_pack_with_launcher(
    root: &Path,
    id: &str,
    destination: &Path,
    options: &PackExportOptions,
    launcher: Option<&Path>,
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
) -> Result<PackExportReport> {
    if !options.include_launcher {
        return export_pack(root, id, destination, options, cancel, progress);
    }
    install::cancelled(cancel)?;
    validate_bundle_destination(destination, options)?;
    let launcher = launcher.context("未提供当前 PCL-Rust 启动器程序")?;
    validate_launcher_export(launcher)?;
    let canonical = launcher.canonicalize()?;
    let files = if canonical.is_dir() {
        ensure!(
            canonical.extension().is_some_and(|e| e == "app")
                && canonical.join("Contents/Info.plist").is_file(),
            "启动器目录必须为当前 macOS .app 包"
        );
        scan_tree(&canonical, &canonical, "PCL-Rust.app", cancel, None)?
    } else {
        let parent = canonical.parent().context("启动器缺少父目录")?;
        scan_tree(
            &canonical,
            parent,
            if cfg!(windows) {
                "PCL-Rust.exe"
            } else {
                "PCL-Rust"
            },
            cancel,
            None,
        )?
    };
    ensure!(!files.is_empty(), "启动器程序为空");
    write_launcher_bundle(root, id, destination, options, &files, cancel, progress,
        "附带程序为 PCL-Rust 第三方 Rust 启动器，仅适用于导出时的平台。请打开启动器并手动导入 modpack.mrpack；不会自动安装 Java 或运行游戏。账户和全局设置未打包。\n")
}

#[allow(clippy::too_many_arguments)]
fn write_launcher_bundle(
    root: &Path,
    id: &str,
    destination: &Path,
    options: &PackExportOptions,
    files: &[Extra],
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
    instructions: &str,
) -> Result<PackExportReport> {
    let total = files
        .iter()
        .try_fold(0u64, |sum, file| sum.checked_add(file.stamp.len))
        .context("附加导出大小溢出")?;
    ensure!(total <= MAX_TOTAL, "附加导出内容超过大小限制");
    let parent = destination
        .parent()
        .context("导出位置缺少父目录")?
        .canonicalize()?;
    let stage = tempfile::tempdir_in(&parent)?;
    let mut inner = options.clone();
    inner.include_launcher = false;
    let mut report = export_pack(
        root,
        id,
        &stage.path().join("modpack.mrpack"),
        &inner,
        cancel,
        &progress,
    )?;
    let inner_stamp = file_stamp(&report.path)?;
    let mut output = tempfile::NamedTempFile::new_in(&parent)?;
    {
        let mut zip = ZipWriter::new(output.as_file_mut());
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zip.start_file("modpack.mrpack", options.unix_permissions(0o644))?;
        copy_verified(&report.path, &inner_stamp, &mut zip, cancel)?;
        for file in files {
            zip.start_file(&file.relative, options.unix_permissions(file.permissions))?;
            file.copy_to(&mut zip, cancel)?;
        }
        zip.start_file("使用说明.txt", options.unix_permissions(0o644))?;
        zip.write_all(instructions.as_bytes())?;
        zip.start_file("UPSTREAM-LICENCE", options.unix_permissions(0o644))?;
        zip.write_all(include_bytes!("../../../UPSTREAM-LICENCE"))?;
        zip.finish()?;
    }
    for file in files {
        file.verify(cancel)?;
    }
    install::cancelled(cancel)?;
    output.as_file().sync_all()?;
    report.bytes = output.as_file().metadata()?.len();
    report.path = destination.to_owned();
    output.persist_noclobber(destination).map_err(|e| e.error)?;
    Ok(report)
}
