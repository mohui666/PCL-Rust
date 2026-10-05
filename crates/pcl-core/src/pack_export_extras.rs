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
                !sensitive_path(
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
        files.extend(scan_tree(&runtime, &runtime, &relative, cancel)?);
    }
    check_collisions(&files, std::iter::empty())?;
    Ok(files)
}

pub fn validate_launcher_export(launcher: &Path) -> Result<()> {
    let launcher = launcher.canonicalize()?;
    if launcher.is_dir() {
        for name in ["PingFang-Regular.otf", "PingFang-Semibold.otf"] {
            ensure!(!launcher.join("Contents/Resources").join(name).exists(),"此 macOS 应用包含仅本机派生的 PingFang 字库，不能附带导出；请导出整合包本身，使用合法可分发字体构建后再附带启动器");
        }
    }
    Ok(())
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
    require_absent(destination)?;
    let launcher = launcher.context("未提供当前 PCL-Rust 启动器程序")?;
    validate_launcher_export(launcher)?;
    let canonical = launcher.canonicalize()?;
    let files = if canonical.is_dir() {
        ensure!(
            canonical.extension().is_some_and(|e| e == "app")
                && canonical.join("Contents/Info.plist").is_file(),
            "启动器目录必须为当前 macOS .app 包"
        );
        scan_tree(&canonical, &canonical, "PCL-Rust.app", cancel)?
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
        )?
    };
    ensure!(!files.is_empty(), "启动器程序为空");
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
        for file in &files {
            zip.start_file(&file.relative, options.unix_permissions(file.permissions))?;
            file.copy_to(&mut zip, cancel)?;
        }
        zip.start_file("使用说明.txt", options.unix_permissions(0o644))?;
        zip.write_all("附带程序为 PCL-Rust 第三方 Rust 启动器，仅适用于导出时的平台。请打开启动器并手动导入 modpack.mrpack；不会自动安装 Java 或运行游戏。账户和全局设置未打包。\n".as_bytes())?;
        zip.start_file("UPSTREAM-LICENCE", options.unix_permissions(0o644))?;
        zip.write_all(include_bytes!("../../../UPSTREAM-LICENCE"))?;
        zip.finish()?;
    }
    for file in &files {
        file.verify(cancel)?;
    }
    install::cancelled(cancel)?;
    output.as_file().sync_all()?;
    report.bytes = output.as_file().metadata()?.len();
    report.path = destination.to_owned();
    output.persist_noclobber(destination).map_err(|e| e.error)?;
    Ok(report)
}
