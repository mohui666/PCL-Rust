//! Reversible version removal. No permanent-delete fallback is provided.
use crate::{install, instances::identity, metadata};
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::SystemTime,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteDependency {
    pub version_id: String,
    pub field: String,
    pub reference: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EntryState {
    identity: crate::instances::Identity,
    directory: bool,
    len: u64,
    modified: SystemTime,
    // Unix ctime also catches same-size edits whose mtime is restored.
    changed: Option<(i64, i64)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TargetSnapshot {
    path: PathBuf,
    entries: BTreeMap<PathBuf, EntryState>,
}

/// Obtain in a worker, then display the exact directories before confirmation.
/// The private snapshots bind that confirmation to this version and file state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionDeletePreview {
    pub version_id: String,
    pub game_root: PathBuf,
    pub version_directory: PathBuf,
    pub instance_directory: Option<PathBuf>,
    pub preserved_shared_directories: Vec<PathBuf>,
    pub dependent_versions: Vec<DeleteDependency>,
    pub file_count: u64,
    pub total_bytes: u64,
    targets: Vec<TargetSnapshot>,
    root_identity: crate::instances::Identity,
    versions_identity: crate::instances::Identity,
    instances_identity: Option<crate::instances::Identity>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeleteReport {
    /// Original paths confirmed moved by the native Trash/Recycle Bin API.
    pub trashed: Vec<PathBuf>,
    /// Original paths not yet moved (or still present after a failed operation).
    pub remaining: Vec<PathBuf>,
    /// An API failed but the original path disappeared; inspect the system Trash.
    pub uncertain: Vec<PathBuf>,
    /// Native destination when macOS provides it; Windows uses its Recycle Bin UI.
    pub trash_locations: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct DeleteFailure {
    pub report: DeleteReport,
    pub cancelled: bool,
    pub message: String,
}

impl std::fmt::Display for DeleteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if !self.report.trashed.is_empty() {
            write!(f, "；以下目录已移入废纸篓/回收站，未自动还原：")?;
            for path in &self.report.trashed {
                write!(f, "\n{}", path.display())?;
            }
        }
        if !self.report.uncertain.is_empty() {
            write!(f, "；以下路径的系统操作结果不确定，请检查废纸篓/回收站：")?;
            for path in &self.report.uncertain {
                write!(f, "\n{}", path.display())?;
            }
        }
        Ok(())
    }
}
impl std::error::Error for DeleteFailure {}

fn state(path: &Path) -> Result<EntryState> {
    let id = identity(path)?; // Reject symlinks, Windows reparse points and special files.
    let meta = fs::symlink_metadata(path)?;
    let changed = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some((meta.ctime(), meta.ctime_nsec()))
        }
        #[cfg(not(unix))]
        {
            None
        }
    };
    Ok(EntryState {
        identity: id,
        directory: meta.is_dir(),
        len: if meta.is_file() { meta.len() } else { 0 },
        modified: meta.modified().context("无法读取文件修改时间")?,
        changed,
    })
}

fn directory(path: &Path) -> Result<crate::instances::Identity> {
    let info = state(path)?;
    ensure!(info.directory, "需要普通目录：{}", path.display());
    Ok(info.identity)
}

fn optional_directory(path: &Path) -> Result<Option<crate::instances::Identity>> {
    match fs::symlink_metadata(path) {
        Ok(_) => directory(path).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("无法检查目录：{}", path.display())),
    }
}

fn snapshot(path: &Path, cancel: &AtomicBool) -> Result<TargetSnapshot> {
    directory(path)?;
    let mut entries = BTreeMap::new();
    let mut queue = vec![PathBuf::new()];
    while let Some(relative) = queue.pop() {
        install::cancelled(cancel)?;
        ensure!(entries.len() < 1_000_000, "目录条目超过安全预览限制");
        let full = path.join(&relative);
        let info = state(&full)?;
        if info.directory {
            for entry in fs::read_dir(&full)? {
                queue.push(relative.join(entry?.file_name()));
            }
        }
        ensure!(
            state(&full)? == info,
            "目录在预览过程中发生变化，请重试：{}",
            full.display()
        );
        entries.insert(relative, info);
    }
    Ok(TargetSnapshot {
        path: path.into(),
        entries,
    })
}

fn dependencies(versions: &Path, id: &str, cancel: &AtomicBool) -> Result<Vec<DeleteDependency>> {
    let mut blockers = Vec::new();
    for (count, entry) in fs::read_dir(versions)?.enumerate() {
        install::cancelled(cancel)?;
        ensure!(count < 10_000, "版本过多，无法完整检查依赖关系");
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("版本目录名称不是 UTF-8，无法检查依赖"))?;
        if name == id {
            continue;
        }
        let info = state(&entry.path())?;
        if !info.directory {
            continue;
        }
        let profile = entry.path().join(format!("{name}.json"));
        match fs::symlink_metadata(&profile) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        let before = state(&profile)?;
        ensure!(
            !before.directory,
            "版本主 JSON 不是普通文件：{}",
            profile.display()
        );
        let mut bytes = Vec::new();
        fs::File::open(&profile)?
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 16 * 1024 * 1024, "版本主 JSON 超出大小限制");
        ensure!(
            state(&profile)? == before,
            "依赖元数据在检查时发生变化，请重新预览"
        );
        let value: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("无法检查 {name} 的依赖：主 JSON 损坏"))?;
        ensure!(
            value.is_object(),
            "无法检查 {name} 的依赖：主 JSON 不是对象"
        );
        for field in ["inheritsFrom", "jar"] {
            if let Some(value) = value.get(field) {
                let reference = value
                    .as_str()
                    .with_context(|| format!("无法检查 {name} 的依赖：{field} 不是字符串"))?;
                if reference.to_lowercase() == id.to_lowercase() {
                    blockers.push(DeleteDependency {
                        version_id: name.clone(),
                        field: field.into(),
                        reference: reference.into(),
                    });
                }
            }
        }
    }
    blockers.sort_by(|a, b| (&a.version_id, &a.field).cmp(&(&b.version_id, &b.field)));
    Ok(blockers)
}

pub fn preview_version_delete(root: &Path, id: &str) -> Result<VersionDeletePreview> {
    preview_version_delete_with_cancel(root, id, &AtomicBool::new(false))
}

pub fn preview_version_delete_with_cancel(
    root: &Path,
    id: &str,
    cancel: &AtomicBool,
) -> Result<VersionDeletePreview> {
    preview(root, id, cancel)
}

fn preview(root: &Path, id: &str, cancel: &AtomicBool) -> Result<VersionDeletePreview> {
    install::cancelled(cancel)?;
    metadata::validate_id(id)?;
    // System aliases in a chosen root (/var on macOS) are allowed. Managed
    // children and every descendant are checked without following links.
    let game_root = root.canonicalize().context("Minecraft 根目录不存在")?;
    let root_identity = directory(&game_root)?;
    let versions = game_root.join("versions");
    let versions_identity = directory(&versions)?;
    let version_directory = versions.join(id);
    let mut targets = vec![snapshot(&version_directory, cancel)?];
    let instances = game_root.join("instances");
    let instances_identity = optional_directory(&instances)?;
    let instance_directory =
        if instances_identity.is_some() && optional_directory(&instances.join(id))?.is_some() {
            let path = instances.join(id);
            targets.push(snapshot(&path, cancel)?);
            Some(path)
        } else {
            None
        };
    let dependent_versions = dependencies(&versions, id, cancel)?;
    let mut file_count = 0;
    let mut total_bytes = 0u64;
    for target in &targets {
        for entry in target.entries.values().filter(|entry| !entry.directory) {
            file_count += 1;
            total_bytes = total_bytes
                .checked_add(entry.len)
                .context("文件大小合计溢出")?;
        }
    }
    Ok(VersionDeletePreview {
        version_id: id.into(),
        game_root: game_root.clone(),
        version_directory,
        instance_directory,
        preserved_shared_directories: [
            "libraries",
            "assets",
            "saves",
            "mods",
            "resourcepacks",
            "shaderpacks",
            "screenshots",
        ]
        .map(|name| game_root.join(name))
        .into(),
        dependent_versions,
        file_count,
        total_bytes,
        targets,
        root_identity,
        versions_identity,
        instances_identity,
    })
}

/// Runs synchronously; call from a worker after the preview has been confirmed.
/// Once an item is in Trash, cancellation never pretends to restore it.
pub fn trash_version(
    root: &Path,
    id: &str,
    expected_preview: &VersionDeletePreview,
    cancel: &AtomicBool,
) -> std::result::Result<DeleteReport, Box<DeleteFailure>> {
    trash_with(root, id, expected_preview, cancel, native_trash)
}

fn trash_with(
    root: &Path,
    id: &str,
    expected: &VersionDeletePreview,
    cancel: &AtomicBool,
    mut trash: impl FnMut(&Path) -> Result<Option<PathBuf>>,
) -> std::result::Result<DeleteReport, Box<DeleteFailure>> {
    let mut report = DeleteReport {
        remaining: expected.targets.iter().map(|t| t.path.clone()).collect(),
        ..Default::default()
    };
    let result = (|| -> Result<()> {
        install::cancelled(cancel)?;
        ensure!(
            expected.game_root == root.canonicalize()? && expected.version_id == id,
            "删除确认不属于当前目录或版本，请重新预览"
        );
        ensure!(
            *expected == preview(root, id, cancel)?,
            "版本文件或目录在确认后发生变化，请重新预览"
        );
        ensure!(
            expected.dependent_versions.is_empty(),
            "其他版本仍引用此版本，不能删除：{}",
            expected
                .dependent_versions
                .iter()
                .map(|d| format!("{}（{} → {}）", d.version_id, d.field, d.reference))
                .collect::<Vec<_>>()
                .join("，")
        );
        for target in &expected.targets {
            install::cancelled(cancel)?;
            ensure!(
                directory(&expected.game_root)? == expected.root_identity
                    && directory(&expected.game_root.join("versions"))?
                        == expected.versions_identity
                    && optional_directory(&expected.game_root.join("instances"))?
                        == expected.instances_identity,
                "版本父目录在操作期间变化，已停止"
            );
            ensure!(
                dependencies(&expected.game_root.join("versions"), id, cancel)?.is_empty(),
                "操作期间出现其他版本依赖，已停止；请重新预览"
            );
            ensure!(
                *target == snapshot(&target.path, cancel)?,
                "待删除文件发生变化，已停止：{}",
                target.path.display()
            );
            install::cancelled(cancel)?;
            match trash(&target.path) {
                Ok(location) => {
                    if let Some(location) = location {
                        report.trash_locations.push(location);
                    }
                    if !fs::symlink_metadata(&target.path)
                        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                    {
                        report.uncertain.push(target.path.clone());
                        anyhow::bail!(
                            "系统报告已移入废纸篓，但原路径仍存在或无法核实；已停止，请刷新并检查：{}",
                            target.path.display()
                        );
                    }
                    report.remaining.retain(|p| p != &target.path);
                    report.trashed.push(target.path.clone());
                }
                Err(error) => {
                    if fs::symlink_metadata(&target.path)
                        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                    {
                        report.remaining.retain(|p| p != &target.path);
                        report.uncertain.push(target.path.clone());
                    }
                    return Err(error).context("移入系统废纸篓/回收站失败；没有执行永久删除");
                }
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(report),
        Err(error) => Err(Box::new(DeleteFailure {
            cancelled: error
                .chain()
                .any(|e| e.is::<crate::model::OperationCancelled>()),
            message: format!("{error:#}"),
            report,
        })),
    }
}

#[cfg(target_os = "macos")]
fn native_trash(path: &Path) -> Result<Option<PathBuf>> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    let path = NSString::from_str(path.to_str().context("废纸篓路径不是 UTF-8")?);
    let url = NSURL::fileURLWithPath(&path);
    let mut destination = None;
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, Some(&mut destination))
        .map_err(|error| anyhow::anyhow!("macOS 废纸篓操作失败：{error}"))?;
    Ok(destination
        .and_then(|url| url.path())
        .map(|path| PathBuf::from(path.to_string())))
}

#[cfg(windows)]
fn native_trash(path: &Path) -> Result<Option<PathBuf>> {
    use windows::{
        core::HSTRING,
        Win32::{System::Com::*, UI::Shell::*},
    };
    // FOFX_RECYCLEONDELETE is documented from Windows 8. Never call an older
    // shell that could ignore this mandatory no-permanent-delete flag.
    let platform = crate::model::Platform::current();
    let mut version = platform.version.split('.').map(str::parse::<u32>);
    let major = version
        .next()
        .transpose()?
        .context("无法确定 Windows 版本")?;
    let minor = version
        .next()
        .transpose()?
        .context("无法确定 Windows 版本")?;
    ensure!(
        (major, minor) >= (6, 2),
        "可恢复删除要求 Windows 8 或更新系统；未删除任何文件"
    );
    // The caller is a worker thread. Balance every successful COM initialization;
    // a conflicting existing apartment is a normal error, never a panic.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    struct ComGuard;
    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    let _guard = ComGuard;
    // Rust canonical paths carry a verbatim prefix that Shell does not accept.
    let text = path.to_str().context("回收站路径不是有效 Unicode")?;
    let shell_path = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(text).to_owned()
    };
    unsafe {
        let operation: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        operation.SetOperationFlags(
            FOF_NO_UI
                | FOF_NO_CONNECTED_ELEMENTS
                | FOF_WANTNUKEWARNING
                | FOFX_RECYCLEONDELETE
                | FOFX_EARLYFAILURE
                | FOFX_ADDUNDORECORD,
        )?;
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(shell_path), None)?;
        operation.DeleteItem(&item, None)?;
        operation.PerformOperations()?;
        ensure!(
            !operation.GetAnyOperationsAborted()?.as_bool(),
            "Windows 回收站操作被中止"
        );
    }
    Ok(None)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn native_trash(_path: &Path) -> Result<Option<PathBuf>> {
    anyhow::bail!("此平台尚未支持系统废纸篓；未删除任何文件")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::Ordering;

    fn fixture(root: &Path, legacy: bool) {
        fs::create_dir_all(root.join("versions/selected/saves/version-world")).unwrap();
        fs::write(
            root.join("versions/selected/selected.json"),
            br#"{"id":"selected","inheritsFrom":"parent"}"#,
        )
        .unwrap();
        fs::write(
            root.join("versions/selected/saves/version-world/level.dat"),
            b"version world",
        )
        .unwrap();
        for name in [
            "libraries",
            "assets",
            "saves",
            "mods",
            "resourcepacks",
            "shaderpacks",
            "screenshots",
        ] {
            fs::create_dir_all(root.join(name)).unwrap();
            fs::write(root.join(name).join("shared"), name).unwrap();
        }
        fs::create_dir_all(root.join("versions/parent")).unwrap();
        fs::write(
            root.join("versions/parent/parent.json"),
            br#"{"id":"parent"}"#,
        )
        .unwrap();
        if legacy {
            fs::create_dir_all(root.join("instances/selected/saves/old-world")).unwrap();
            fs::write(
                root.join("instances/selected/saves/old-world/level.dat"),
                b"legacy world",
            )
            .unwrap();
        }
    }

    fn no_call(_: &Path) -> Result<Option<PathBuf>> {
        panic!("must not call native trash");
    }

    #[test]
    fn preview_and_success_only_include_exact_version_and_legacy_data() {
        let root = tempfile::tempdir().unwrap();
        let recycled = tempfile::tempdir().unwrap();
        fixture(root.path(), true);
        let preview = preview_version_delete(root.path(), "selected").unwrap();
        assert_eq!(preview.targets.len(), 2);
        assert_eq!(preview.file_count, 3);
        assert_eq!(preview.preserved_shared_directories.len(), 7);
        let report = trash_with(
            root.path(),
            "selected",
            &preview,
            &AtomicBool::new(false),
            |path| {
                let index = if path.parent().unwrap().ends_with("versions") {
                    "version"
                } else {
                    "instance"
                };
                let destination = recycled.path().join(index);
                metadata::rename_directory_no_replace(path, &destination)?;
                Ok(Some(destination))
            },
        )
        .unwrap();
        assert_eq!(report.trashed.len(), 2);
        assert!(report.remaining.is_empty() && report.uncertain.is_empty());
        assert_eq!(
            fs::read(
                recycled
                    .path()
                    .join("version/saves/version-world/level.dat")
            )
            .unwrap(),
            b"version world"
        );
        assert_eq!(
            fs::read(recycled.path().join("instance/saves/old-world/level.dat")).unwrap(),
            b"legacy world"
        );
        assert_eq!(
            fs::read(root.path().join("versions/parent/parent.json")).unwrap(),
            br#"{"id":"parent"}"#
        );
        for preserved in preview.preserved_shared_directories {
            assert!(preserved.join("shared").is_file());
        }
    }

    #[test]
    fn exact_and_case_equivalent_dependencies_block_before_any_move() {
        for field in ["inheritsFrom", "jar"] {
            let root = tempfile::tempdir().unwrap();
            fixture(root.path(), false);
            fs::create_dir(root.path().join("versions/child")).unwrap();
            fs::write(
                root.path().join("versions/child/child.json"),
                json!({"id":"child",field:"SELECTED"}).to_string(),
            )
            .unwrap();
            let preview = preview_version_delete(root.path(), "selected").unwrap();
            assert_eq!(preview.dependent_versions[0].version_id, "child");
            let error = trash_with(
                root.path(),
                "selected",
                &preview,
                &AtomicBool::new(false),
                no_call,
            )
            .unwrap_err();
            assert!(error.report.trashed.is_empty());
            assert!(error.message.contains("child"));
        }
    }

    #[test]
    fn files_added_changed_or_replaced_after_confirmation_require_new_preview() {
        for change in ["added", "changed", "replaced", "new_instance", "dependency"] {
            let root = tempfile::tempdir().unwrap();
            fixture(root.path(), false);
            let preview = preview_version_delete(root.path(), "selected").unwrap();
            let path = root.path().join("versions/selected/selected.json");
            match change {
                "added" => {
                    fs::write(root.path().join("versions/selected/new-world"), b"new").unwrap()
                }
                "changed" => fs::write(path, b"new bytes").unwrap(),
                "replaced" => {
                    fs::rename(&path, path.with_extension("backup")).unwrap();
                    fs::write(path, b"{}").unwrap();
                }
                "new_instance" => {
                    fs::create_dir_all(root.path().join("instances/selected")).unwrap()
                }
                "dependency" => {
                    fs::create_dir(root.path().join("versions/child")).unwrap();
                    fs::write(
                        root.path().join("versions/child/child.json"),
                        br#"{"inheritsFrom":"selected"}"#,
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
            assert!(trash_with(
                root.path(),
                "selected",
                &preview,
                &AtomicBool::new(false),
                no_call
            )
            .is_err());
            assert!(root.path().join("versions/selected").is_dir());
        }
    }

    #[test]
    fn cancellation_before_and_between_targets_preserves_accurate_partial_report() {
        for after_first in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let recycled = tempfile::tempdir().unwrap();
            fixture(root.path(), true);
            let cancelled_preview =
                preview_version_delete_with_cancel(root.path(), "selected", &AtomicBool::new(true))
                    .unwrap_err();
            assert!(cancelled_preview
                .chain()
                .any(|cause| cause.is::<crate::model::OperationCancelled>()));
            let preview = preview_version_delete(root.path(), "selected").unwrap();
            let cancel = AtomicBool::new(!after_first);
            let mut calls = 0;
            let error = trash_with(root.path(), "selected", &preview, &cancel, |path| {
                calls += 1;
                metadata::rename_directory_no_replace(path, &recycled.path().join("first"))?;
                cancel.store(true, Ordering::Relaxed);
                Ok(None)
            })
            .unwrap_err();
            assert!(error.cancelled);
            assert_eq!(calls, usize::from(after_first));
            assert_eq!(error.report.trashed.len(), usize::from(after_first));
            assert_eq!(
                error.report.remaining.len(),
                if after_first { 1 } else { 2 }
            );
            assert!(root
                .path()
                .join("instances/selected/saves/old-world/level.dat")
                .is_file());
        }
    }

    #[test]
    fn native_failure_or_late_cancel_does_not_claim_rollback_or_false_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let recycled = tempfile::tempdir().unwrap();
        fixture(root.path(), true);
        let preview = preview_version_delete(root.path(), "selected").unwrap();
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        let error = trash_with(root.path(), "selected", &preview, &cancel, |path| {
            calls += 1;
            if calls == 2 {
                cancel.store(true, Ordering::Relaxed);
                anyhow::bail!("fixture OS refusal");
            }
            metadata::rename_directory_no_replace(path, &recycled.path().join("first"))?;
            Ok(None)
        })
        .unwrap_err();
        assert!(!error.cancelled);
        assert_eq!(error.report.trashed.len(), 1);
        assert_eq!(error.report.remaining.len(), 1);
        assert!(error.to_string().contains("未自动还原"));
        assert!(recycled.path().join("first/selected.json").is_file());
    }

    #[test]
    fn change_to_remaining_instance_after_first_move_stops_without_deleting_new_data() {
        let root = tempfile::tempdir().unwrap();
        let recycled = tempfile::tempdir().unwrap();
        fixture(root.path(), true);
        let preview = preview_version_delete(root.path(), "selected").unwrap();
        let mut calls = 0;
        let error = trash_with(
            root.path(),
            "selected",
            &preview,
            &AtomicBool::new(false),
            |path| {
                calls += 1;
                metadata::rename_directory_no_replace(path, &recycled.path().join("first"))?;
                fs::write(
                    root.path().join("instances/selected/new-save"),
                    b"concurrent world",
                )?;
                Ok(None)
            },
        )
        .unwrap_err();
        assert_eq!(calls, 1);
        assert!(!error.cancelled);
        assert_eq!(error.report.trashed.len(), 1);
        assert_eq!(error.report.remaining.len(), 1);
        assert_eq!(
            fs::read(root.path().join("instances/selected/new-save")).unwrap(),
            b"concurrent world"
        );
    }

    #[test]
    fn ambiguous_native_results_are_reported_without_claiming_success() {
        for vanished in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let recycled = tempfile::tempdir().unwrap();
            fixture(root.path(), false);
            let preview = preview_version_delete(root.path(), "selected").unwrap();
            let error = trash_with(
                root.path(),
                "selected",
                &preview,
                &AtomicBool::new(false),
                |path| {
                    if vanished {
                        metadata::rename_directory_no_replace(
                            path,
                            &recycled.path().join("ambiguous"),
                        )?;
                        anyhow::bail!("fixture API failed after moving");
                    }
                    Ok(None) // A lying backend cannot produce a completed report.
                },
            )
            .unwrap_err();
            assert!(error.report.trashed.is_empty());
            assert_eq!(error.report.uncertain.len(), 1);
        }
    }

    #[test]
    fn confirmation_is_bound_to_root_id_and_unmodified_public_fields() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        fixture(root.path(), false);
        fixture(other.path(), false);
        let mut preview = preview_version_delete(root.path(), "selected").unwrap();
        assert!(trash_with(
            other.path(),
            "selected",
            &preview,
            &AtomicBool::new(false),
            no_call
        )
        .is_err());
        preview.total_bytes = 0;
        assert!(trash_with(
            root.path(),
            "selected",
            &preview,
            &AtomicBool::new(false),
            no_call
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_rejected_without_following_or_removing_their_targets() {
        use std::os::unix::fs::symlink;
        for relative in [
            "versions/linked",
            "versions/selected/linked",
            "instances/selected",
        ] {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            fixture(root.path(), false);
            fs::write(outside.path().join("user"), b"keep").unwrap();
            fs::create_dir_all(root.path().join(relative).parent().unwrap()).unwrap();
            symlink(outside.path(), root.path().join(relative)).unwrap();
            assert!(preview_version_delete(root.path(), "selected").is_err());
            assert_eq!(fs::read(outside.path().join("user")).unwrap(), b"keep");
        }
    }
}
