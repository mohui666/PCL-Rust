//! Version maintenance without resolving/merging inheritance metadata.
//! Rename only the exact primary JSON/JAR/native names. Unknown files are moved
//! unchanged; a retained journal is reported if concurrent changes prevent rollback.
use crate::{install, metadata};
use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

const JSON_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    volume: u64,
    file: u64,
}

pub(crate) fn identity(path: &Path) -> Result<Identity> {
    let meta =
        fs::symlink_metadata(path).with_context(|| format!("无法检查路径：{}", path.display()))?;
    ensure!(
        !meta.file_type().is_symlink() && (meta.is_dir() || meta.is_file()),
        "重命名路径不能是符号链接或特殊文件：{}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Identity {
            volume: meta.dev(),
            file: meta.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        #[repr(C)]
        struct Information {
            attributes: u32,
            created: [u32; 2],
            accessed: [u32; 2],
            modified: [u32; 2],
            volume: u32,
            size_high: u32,
            size_low: u32,
            links: u32,
            index_high: u32,
            index_low: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(
                handle: *mut std::ffi::c_void,
                info: *mut Information,
            ) -> i32;
        }
        // BACKUP_SEMANTICS opens directories; OPEN_REPARSE_POINT avoids following
        // a reparse point substituted between the metadata check and this open.
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x02000000 | 0x00200000)
            .open(path)?;
        let mut info: Information = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error()).context("读取文件身份失败");
        }
        ensure!(info.attributes & 0x400 == 0, "重命名路径不能是重解析点");
        Ok(Identity {
            volume: info.volume as u64,
            file: ((info.index_high as u64) << 32) | info.index_low as u64,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = meta;
        bail!("此平台不支持安全文件身份检查");
    }
}

fn ordinary_directory(path: &Path) -> Result<Identity> {
    let id = identity(path)?;
    ensure!(
        fs::symlink_metadata(path)?.is_dir(),
        "需要普通目录：{}",
        path.display()
    );
    Ok(id)
}

fn optional_path(path: &Path, directory: bool) -> Result<Option<Identity>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(
                if directory {
                    meta.is_dir()
                } else {
                    meta.is_file()
                },
                "路径类型不符合预期：{}",
                path.display()
            );
            Ok(Some(identity(path)?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("无法检查路径：{}", path.display())),
    }
}

fn read_json_file(path: &Path) -> Result<(Vec<u8>, Value)> {
    ensure!(
        optional_path(path, false)?.is_some(),
        "版本主 JSON 不存在：{}",
        path.display()
    );
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(JSON_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= JSON_LIMIT,
        "版本 JSON 超过大小限制：{}",
        path.display()
    );
    let value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("版本主 JSON 损坏，未修改：{}", path.display()))?;
    ensure!(
        value.is_object(),
        "版本主 JSON 必须是对象：{}",
        path.display()
    );
    for key in ["id", "inheritsFrom", "jar"] {
        if let Some(value) = value.get(key) {
            ensure!(
                value.is_string(),
                "版本 JSON 的 {key} 必须是字符串：{}",
                path.display()
            );
        }
    }
    Ok((bytes, value))
}

fn no_name_conflict(parent: &Path, name: &str) -> Result<()> {
    if !parent.exists() {
        return Ok(());
    }
    ordinary_directory(parent)?;
    let expected = name.to_lowercase();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        ensure!(
            entry.file_name().to_string_lossy().to_lowercase() != expected,
            "目标名称已存在（包含大小写冲突），未覆盖：{}",
            parent.join(name).display()
        );
    }
    Ok(())
}

fn block_references(versions: &Path, old: &str, journal: Option<&Path>) -> Result<()> {
    let mut blockers = Vec::new();
    let mut count = 0;
    for entry in fs::read_dir(versions)? {
        count += 1;
        ensure!(count <= 10_000, "版本目录过多，无法完整检查继承引用");
        let entry = entry?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("版本目录包含非 UTF-8 名称，无法检查引用"))?;
        if name == old || journal == Some(path.as_path()) {
            continue;
        }
        let meta = entry.file_type()?;
        ensure!(!meta.is_symlink(), "无法检查符号链接版本的继承关系：{name}");
        if !meta.is_dir() {
            continue;
        }
        let profile = path.join(format!("{name}.json"));
        if fs::symlink_metadata(&profile).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
            continue;
        }
        let (_, value) = read_json_file(&profile)?;
        for key in ["inheritsFrom", "jar"] {
            if let Some(reference) = value.get(key).and_then(Value::as_str) {
                // These names resolve to the same path on the default Windows
                // and macOS filesystems. Do not silently break a differently
                // cased reference, or rewrite metadata in another version.
                if reference.to_lowercase() == old.to_lowercase() {
                    blockers.push(format!("{name}（{key} → {reference}）"));
                }
            }
        }
    }
    ensure!(
        blockers.is_empty(),
        "以下版本仍引用 {old}，为保护继承关系未重命名；请先处理这些引用：{}",
        blockers.join("，")
    );
    Ok(())
}

struct Plan {
    versions: PathBuf,
    source: PathBuf,
    destination: PathBuf,
    source_identity: Identity,
    instance: Option<(PathBuf, PathBuf, Identity)>,
    new: String,
    original: Vec<u8>,
    updated: Vec<u8>,
    profile_identity: Identity,
    jar: Option<Identity>,
    natives: Option<Identity>,
}

fn prepare(root: &Path, old: &str, new: &str) -> Result<Plan> {
    metadata::validate_id(old)?;
    metadata::validate_id(new)?;
    ensure!(
        old.to_lowercase() != new.to_lowercase(),
        "新名称与原名称相同或仅大小写不同，未重命名"
    );
    // A canonical game root may have system aliases (for example /var on macOS),
    // but each managed child itself must be an ordinary directory.
    let root = root.canonicalize().context("Minecraft 根目录不存在")?;
    let versions = root.join("versions");
    ordinary_directory(&versions)?;
    let source = versions.join(old);
    let source_identity = ordinary_directory(&source)?;
    no_name_conflict(&versions, new)?;
    let destination = versions.join(new);
    let profile = source.join(format!("{old}.json"));
    let profile_identity = identity(&profile)?;
    let (original, mut value) = read_json_file(&profile)?;
    ensure!(
        !matches!(value.get("inheritsFrom").and_then(Value::as_str),Some(parent) if parent.to_lowercase()==old.to_lowercase() || parent.to_lowercase()==new.to_lowercase()),
        "主版本的继承关系会形成自引用，未重命名"
    );
    value["id"] = new.into();
    ensure!(
        !matches!(value.get("jar").and_then(Value::as_str),Some(jar) if jar != old && jar.to_lowercase()==old.to_lowercase()),
        "主 JSON 的 jar 以不同大小写引用自身，请先统一名称后重试"
    );
    if value.get("jar").and_then(Value::as_str) == Some(old) {
        value["jar"] = new.into();
    }
    let updated = serde_json::to_vec_pretty(&value)?;
    for name in [
        format!("{new}.json"),
        format!("{new}.jar"),
        format!("{new}-natives"),
    ] {
        no_name_conflict(&source, &name)?;
    }
    let jar = optional_path(&source.join(format!("{old}.jar")), false)?;
    let natives = optional_path(&source.join(format!("{old}-natives")), true)?;
    let instances = root.join("instances");
    let instance = if optional_path(&instances, true)?.is_some() {
        no_name_conflict(&instances, new)?;
        let old_path = instances.join(old);
        optional_path(&old_path, true)?.map(|id| (old_path, instances.join(new), id))
    } else {
        None
    };
    block_references(&versions, old, None)?;
    Ok(Plan {
        versions,
        source,
        destination,
        source_identity,
        instance,
        new: new.into(),
        original,
        updated,
        profile_identity,
        jar,
        natives,
    })
}

struct MoveRecord {
    from: PathBuf,
    to: PathBuf,
    identity: Identity,
    owned_bytes: Option<Vec<u8>>,
}
struct Journal {
    path: PathBuf,
    moves: Vec<MoveRecord>,
}
impl Journal {
    fn move_path(
        &mut self,
        from: &Path,
        to: &Path,
        expected: Identity,
        owned_bytes: Option<&[u8]>,
    ) -> Result<()> {
        ensure!(
            identity(from)? == expected,
            "重命名源在操作期间已被其他程序替换：{}",
            from.display()
        );
        if let Some(bytes) = owned_bytes {
            ensure!(fs::read(from)? == bytes, "准备的元数据在操作期间发生变化");
        }
        no_name_conflict(
            to.parent().context("目标缺少父目录")?,
            to.file_name()
                .context("目标缺少名称")?
                .to_str()
                .context("目标名称不是 UTF-8")?,
        )?;
        metadata::rename_directory_no_replace(from, to)?;
        self.moves.push(MoveRecord {
            from: from.into(),
            to: to.into(),
            identity: expected,
            owned_bytes: owned_bytes.map(Vec::from),
        });
        Ok(())
    }
    fn rollback(&mut self) -> Result<()> {
        for item in self.moves.iter().rev() {
            ensure!(
                identity(&item.to)? == item.identity,
                "回滚路径已被其他程序替换：{}",
                item.to.display()
            );
            if let Some(bytes) = &item.owned_bytes {
                ensure!(
                    fs::read(&item.to)? == *bytes,
                    "新的主 JSON 已被其他程序修改，未覆盖：{}",
                    item.to.display()
                );
            }
            no_name_conflict(
                item.from.parent().context("回滚路径缺少父目录")?,
                item.from
                    .file_name()
                    .unwrap()
                    .to_str()
                    .context("回滚名称不是 UTF-8")?,
            )?;
            metadata::rename_directory_no_replace(&item.to, &item.from)
                .with_context(|| format!("无法回滚 {}", item.from.display()))?;
        }
        self.moves.clear();
        Ok(())
    }
}

/// Rename a single version; inheritsFrom/jar references from other versions
/// (including case-equivalent names) are reported as blockers. The caller should refresh the version list on error,
/// because a concurrent change can require retaining a recovery journal.
pub fn rename_version(root: &Path, old: &str, new: &str) -> Result<()> {
    rename_version_with_cancel(root, old, new, &AtomicBool::new(false))
}

pub fn rename_version_with_cancel(
    root: &Path,
    old: &str,
    new: &str,
    cancel: &AtomicBool,
) -> Result<()> {
    rename_with_hook(root, old, new, cancel, |_, _| Ok(()))
}

fn cleanup_journal(path: &Path, file: &str, expected: &[u8]) -> Result<()> {
    let file = path.join(file);
    ensure!(
        optional_path(&file, false)?.is_some() && fs::read(&file)? == expected,
        "重命名备份已被修改，已保留：{}",
        path.display()
    );
    fs::remove_file(file)?;
    fs::remove_dir(path).context("重命名临时目录包含其他文件，已保留")
}

fn rename_with_hook(
    root: &Path,
    old: &str,
    new: &str,
    cancel: &AtomicBool,
    mut hook: impl FnMut(&str, &Path) -> Result<()>,
) -> Result<()> {
    install::cancelled(cancel)?;
    let plan = prepare(root, old, new)?;
    install::cancelled(cancel)?;
    let temporary = tempfile::Builder::new()
        .prefix(".pcl-rename-")
        .tempdir_in(&plan.versions)?;
    let prepared = temporary.path().join("prepared.json");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&prepared)?;
    file.write_all(&plan.updated)?;
    file.sync_all()?;
    drop(file);
    // Never let TempDir's recursive cleanup own user directories. Once moving
    // begins, failures retain the journal unless a verified rollback completes.
    let path = temporary.keep();
    let mut journal = Journal {
        path: path.clone(),
        moves: vec![],
    };
    let staged = path.join("version");
    let staged_instance = path.join("instance");
    let backup = path.join("original.json");
    let prepared_id = identity(&prepared)?;
    let result = (|| {
        hook("prepared", &path)?;
        install::cancelled(cancel)?;
        ensure!(
            fs::read(plan.source.join(format!("{old}.json")))? == plan.original,
            "主 JSON 在准备期间发生变化，未覆盖"
        );
        block_references(&plan.versions, old, Some(&path))?;
        journal.move_path(&plan.source, &staged, plan.source_identity, None)?;
        if let Some((source, _, id)) = &plan.instance {
            journal.move_path(source, &staged_instance, *id, None)?;
        }
        hook("staged", &path)?;
        install::cancelled(cancel)?;
        journal.move_path(
            &staged.join(format!("{old}.json")),
            &backup,
            plan.profile_identity,
            None,
        )?;
        if let Some(id) = plan.jar {
            journal.move_path(
                &staged.join(format!("{old}.jar")),
                &staged.join(format!("{new}.jar")),
                id,
                None,
            )?;
        }
        if let Some(id) = plan.natives {
            journal.move_path(
                &staged.join(format!("{old}-natives")),
                &staged.join(format!("{new}-natives")),
                id,
                None,
            )?;
        }
        journal.move_path(
            &prepared,
            &staged.join(format!("{new}.json")),
            prepared_id,
            Some(&plan.updated),
        )?;
        hook("profile_ready", &path)?;
        install::cancelled(cancel)?;
        ensure!(
            fs::read(&backup)? == plan.original,
            "原始主 JSON 被并发修改，准备回滚以保留更改"
        );
        block_references(&plan.versions, old, Some(&path))?;
        no_name_conflict(&plan.versions, new)?;
        if let Some((_, destination, id)) = &plan.instance {
            journal.move_path(&staged_instance, destination, *id, None)?;
        }
        journal.move_path(&staged, &plan.destination, plan.source_identity, None)?;
        hook("published", &path)?;
        install::cancelled(cancel)?;
        ensure!(
            fs::read(&backup)? == plan.original,
            "原始主 JSON 被并发修改，准备回滚以保留更改"
        );
        block_references(&plan.versions, old, Some(&path))?;
        Ok(())
    })();
    if let Err(error) = result {
        if let Err(rollback) = journal.rollback() {
            bail!(
                "重命名未完成：{error:#}；回滚未完成：{rollback:#}。原始文件与并发更改已保留在 {} 及原/新目录，请刷新版本列表后检查",
                journal.path.display()
            );
        }
        if let Err(cleanup) = cleanup_journal(&path, "prepared.json", &plan.updated) {
            bail!(
                "重命名已回滚：{error:#}；临时资料清理未完成：{cleanup:#}；保留位置：{}",
                path.display()
            );
        }
        return Err(error).context("版本重命名未完成，已恢复本次修改");
    }
    cleanup_journal(&path, "original.json", &plan.original).with_context(|| {
        format!(
            "版本已重命名为 {}，但备份清理未完成：{}",
            plan.new,
            path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::Ordering;
    fn version(root: &Path, id: &str, value: Value) -> Vec<u8> {
        let folder = root.join("versions").join(id);
        fs::create_dir_all(&folder).unwrap();
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(folder.join(format!("{id}.json")), &bytes).unwrap();
        bytes
    }
    fn base(root: &Path) -> Vec<u8> {
        let bytes = version(
            root,
            "old",
            json!({"id":"old","jar":"old","mainClass":"example.Main","unknown":{"value":"old","keep":true},"arguments":{"jvm":["-Dliteral=old"]}}),
        );
        fs::write(root.join("versions/old/old.jar"), b"jar bytes").unwrap();
        fs::create_dir_all(root.join("versions/old/old-natives")).unwrap();
        fs::write(root.join("versions/old/old-natives/native"), b"native").unwrap();
        fs::write(root.join("versions/old/old-user-note"), b"unknown").unwrap();
        bytes
    }
    fn no_journal(root: &Path) {
        assert!(!fs::read_dir(root.join("versions")).unwrap().any(|p| {
            p.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".pcl-rename-")
        }));
    }
    #[test]
    fn rename_preserves_unknown_fields_files_worlds_and_exact_primary_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        base(root);
        fs::create_dir_all(root.join("instances/old/saves/world")).unwrap();
        fs::write(root.join("instances/old/saves/world/level.dat"), b"world").unwrap();
        fs::create_dir_all(root.join("saves/shared")).unwrap();
        fs::write(root.join("saves/shared/level.dat"), b"shared").unwrap();
        rename_version(root, "old", "新的名字").unwrap();
        let renamed = root.join("versions/新的名字");
        let (_, value) = read_json_file(&renamed.join("新的名字.json")).unwrap();
        assert_eq!(value["id"], "新的名字");
        assert_eq!(value["jar"], "新的名字");
        assert_eq!(value["unknown"]["value"], "old");
        assert_eq!(value["arguments"]["jvm"][0], "-Dliteral=old");
        assert_eq!(
            fs::read(renamed.join("新的名字.jar")).unwrap(),
            b"jar bytes"
        );
        assert_eq!(
            fs::read(renamed.join("新的名字-natives/native")).unwrap(),
            b"native"
        );
        assert_eq!(fs::read(renamed.join("old-user-note")).unwrap(), b"unknown");
        assert_eq!(
            fs::read(root.join("instances/新的名字/saves/world/level.dat")).unwrap(),
            b"world"
        );
        assert_eq!(
            fs::read(root.join("saves/shared/level.dat")).unwrap(),
            b"shared"
        );
        assert!(!root.join("versions/old").exists());
        no_journal(root);
    }
    #[test]
    fn inherited_profile_is_not_merged_or_parent_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let parent = version(
            root,
            "parent",
            json!({"id":"parent","libraries":[{"name":"keep"}],"unknown":"parent"}),
        );
        version(
            root,
            "child",
            json!({"id":"child","inheritsFrom":"parent","jar":"parent","custom":17}),
        );
        rename_version(root, "child", "renamed").unwrap();
        let (_, v) = read_json_file(&root.join("versions/renamed/renamed.json")).unwrap();
        assert_eq!(v["inheritsFrom"], "parent");
        assert_eq!(v["jar"], "parent");
        assert_eq!(v["custom"], 17);
        assert!(v.get("libraries").is_none());
        assert_eq!(
            fs::read(root.join("versions/parent/parent.json")).unwrap(),
            parent
        );
    }
    #[test]
    fn exact_child_inheritance_and_jar_references_block_without_changes() {
        for (key, reference) in [
            ("inheritsFrom", "old"),
            ("jar", "old"),
            ("inheritsFrom", "OLD"),
            ("jar", "OLD"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let original = base(root);
            let mut child = json!({"id":"child","unknown":"old"});
            child[key] = reference.into();
            version(root, "child", child);
            let error = rename_version(root, "old", "new").unwrap_err().to_string();
            assert!(error.contains("child") && error.contains(key));
            assert_eq!(
                fs::read(root.join("versions/old/old.json")).unwrap(),
                original
            );
            assert!(!root.join("versions/new").exists());
            no_journal(root);
        }
    }
    #[test]
    fn self_references_with_different_case_are_rejected_before_mutation() {
        for (key, reference) in [
            ("inheritsFrom", "OLD"),
            ("inheritsFrom", "NEW"),
            ("jar", "OLD"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let mut value = json!({"id":"old"});
            value[key] = reference.into();
            let original = version(root, "old", value);
            assert!(rename_version(root, "old", "new").is_err());
            assert_eq!(
                fs::read(root.join("versions/old/old.json")).unwrap(),
                original
            );
            no_journal(root);
        }
    }
    #[test]
    fn existing_empty_case_conflict_and_primary_collision_are_never_overwritten() {
        for conflict in [
            "versions/new",
            "versions/NEW",
            "instances/new",
            "versions/old/new.jar",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let original = base(root);
            let p = root.join(conflict);
            if conflict.ends_with(".jar") {
                fs::write(&p, b"user").unwrap();
            } else {
                fs::create_dir_all(&p).unwrap();
            }
            assert!(rename_version(root, "old", "new").is_err());
            assert_eq!(
                fs::read(root.join("versions/old/old.json")).unwrap(),
                original
            );
            assert!(p.exists());
            no_journal(root);
        }
    }
    #[test]
    fn malformed_primary_or_other_version_does_not_get_silently_replaced() {
        for broken in ["old", "other"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            base(root);
            let p = root.join("versions").join(broken);
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join(format!("{broken}.json")), b"not json").unwrap();
            assert!(rename_version(root, "old", "new").is_err());
            assert_eq!(
                fs::read(p.join(format!("{broken}.json"))).unwrap(),
                b"not json"
            );
            assert!(!root.join("versions/new").exists());
            no_journal(root);
        }
    }
    #[test]
    fn cancellation_after_each_move_phase_restores_original_bytes_and_data() {
        for phase in ["prepared", "staged", "profile_ready", "published"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let original = base(root);
            fs::create_dir_all(root.join("instances/old")).unwrap();
            fs::write(root.join("instances/old/data"), b"keep").unwrap();
            let cancel = AtomicBool::new(false);
            let error = rename_with_hook(root, "old", "new", &cancel, |step, _| {
                if step == phase {
                    cancel.store(true, Ordering::Relaxed);
                }
                Ok(())
            })
            .unwrap_err();
            assert!(error
                .chain()
                .any(|cause| cause.is::<crate::model::OperationCancelled>()));
            assert_eq!(
                fs::read(root.join("versions/old/old.json")).unwrap(),
                original
            );
            assert_eq!(
                fs::read(root.join("versions/old/old.jar")).unwrap(),
                b"jar bytes"
            );
            assert_eq!(fs::read(root.join("instances/old/data")).unwrap(), b"keep");
            assert!(!root.join("versions/new").exists());
            assert!(!root.join("instances/new").exists());
            no_journal(root);
        }
    }
    #[test]
    fn concurrent_target_creation_rolls_back_without_deleting_user_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let original = base(root);
        fs::create_dir_all(root.join("instances/old")).unwrap();
        let error = rename_with_hook(root, "old", "new", &AtomicBool::new(false), |step, _| {
            if step == "profile_ready" {
                fs::create_dir(root.join("instances/new"))?;
                fs::write(root.join("instances/new/user"), b"new user data")?;
            }
            Ok(())
        });
        assert!(error.is_err());
        assert_eq!(
            fs::read(root.join("versions/old/old.json")).unwrap(),
            original
        );
        assert_eq!(
            fs::read(root.join("instances/new/user")).unwrap(),
            b"new user data"
        );
        assert!(root.join("instances/old").is_dir());
        no_journal(root);
    }
    #[test]
    fn concurrent_original_json_write_is_preserved_by_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        base(root);
        let result = rename_with_hook(root, "old", "new", &AtomicBool::new(false), |step, path| {
            if step == "profile_ready" {
                fs::write(path.join("original.json"), b"concurrent user metadata")?;
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(
            fs::read(root.join("versions/old/old.json")).unwrap(),
            b"concurrent user metadata"
        );
        no_journal(root);
    }
    #[test]
    fn rollback_refuses_to_overwrite_changed_new_json_and_keeps_recovery_data() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let original = base(root);
        let mut retained = None;
        let result = rename_with_hook(root, "old", "new", &AtomicBool::new(false), |step, path| {
            if step == "profile_ready" {
                fs::write(path.join("version/new.json"), b"user changed new metadata")?;
                retained = Some(path.to_path_buf());
                bail!("fixture failure after concurrent change");
            }
            Ok(())
        });
        let message = result.unwrap_err().to_string();
        assert!(message.contains("回滚未完成"));
        let path = retained.unwrap();
        assert_eq!(fs::read(path.join("original.json")).unwrap(), original);
        assert_eq!(
            fs::read(path.join("version/new.json")).unwrap(),
            b"user changed new metadata"
        );
        assert_eq!(
            fs::read(path.join("version/new.jar")).unwrap(),
            b"jar bytes"
        );
    }
    #[cfg(unix)]
    #[test]
    fn symlink_source_destination_and_primary_files_are_rejected() {
        use std::os::unix::fs::symlink;
        for part in ["versions/new", "instances/old", "versions/old/old.json"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            base(root);
            let outside = tempfile::tempdir().unwrap();
            let path = root.join(part);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            if path.is_file() {
                fs::remove_file(&path).unwrap();
            }
            symlink(outside.path(), &path).unwrap();
            assert!(rename_version(root, "old", "new").is_err());
            assert!(fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
        }
    }
}
