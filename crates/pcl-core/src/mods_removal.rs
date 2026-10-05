//! Explicit, reversible removal. Only selected ordinary JAR files are moved.
use super::*;
use crate::{install::cancelled, instances::identity, metadata};
use sha2::{Digest, Sha256};
use std::sync::atomic::AtomicBool;

#[derive(Clone, Debug)]
pub struct ModRemoval {
    pub count: usize,
    pub backup: PathBuf,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    instance: PathBuf,
    files: Vec<SavedFile>,
}
#[derive(Serialize, Deserialize)]
struct SavedFile {
    name: String,
    sha256: String,
}
fn hash(path: &Path, cancel: &AtomicBool) -> Result<String> {
    identity(path)?;
    if !fs::symlink_metadata(path)?.is_file() {
        bail!("Mod 必须是普通文件");
    }
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        cancelled(cancel)?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn transfer(
    files: &[SavedFile],
    from: &Path,
    to: &Path,
    cancel: &AtomicBool,
    after: impl Fn(usize),
) -> Result<()> {
    let mut moved = Vec::new();
    let result = (|| {
        for (index, item) in files.iter().enumerate() {
            cancelled(cancel)?;
            let source = from.join(&item.name);
            let target = to.join(&item.name);
            if hash(&source, cancel)? != item.sha256 {
                bail!("Mod 已改变，未移动：{}", item.name);
            }
            let stamp = identity(&source)?;
            metadata::rename_directory_no_replace(&source, &target)?;
            moved.push((source, target.clone(), stamp));
            if hash(&target, cancel)? != item.sha256 {
                bail!("Mod 移动时内容发生改变，正在恢复：{}", item.name);
            }
            after(index);
        }
        cancelled(cancel)
    })();
    if let Err(error) = result {
        let mut failures = Vec::new();
        for (source, target, stamp) in moved.into_iter().rev() {
            if identity(&target).ok() != Some(stamp) {
                failures.push(format!("{} 已变化，保留未回滚", target.display()));
                continue;
            }
            if let Err(e) = metadata::rename_directory_no_replace(&target, &source) {
                failures.push(format!("{}：{e:#}", target.display()));
            }
        }
        if !failures.is_empty() {
            return Err(error.context(format!(
                "部分文件未能恢复，请保留备份：{}",
                failures.join("；")
            )));
        }
        return Err(error);
    }
    Ok(())
}
pub fn remove_mods(instance: &Path, names: &[String], cancel: &AtomicBool) -> Result<ModRemoval> {
    remove_with(instance, names, cancel, |_| {})
}
fn remove_with(
    instance: &Path,
    names: &[String],
    cancel: &AtomicBool,
    after: impl Fn(usize),
) -> Result<ModRemoval> {
    cancelled(cancel)?;
    if names.is_empty() {
        bail!("请先选择 Mod");
    }
    let source = mods_directory(instance, false)?;
    let instance = instance.canonicalize()?;
    let mut files = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for name in names {
        validate_id(name)?;
        if !jar_name(name) || !seen.insert(name.to_lowercase()) {
            bail!("所选 Mod 文件名无效或重复");
        }
        files.push(SavedFile {
            name: name.clone(),
            sha256: hash(&source.join(name), cancel)?,
        });
    }
    let parent = metadata::confined_path(&instance, Path::new("PCL-Rust/mod-removals"))?;
    fs::create_dir_all(&parent)?;
    let temporary = tempfile::Builder::new()
        .prefix("removed-")
        .tempdir_in(parent)?;
    let journal = Journal { instance, files };
    fs::write(
        temporary.path().join("restore.json"),
        serde_json::to_vec_pretty(&journal)?,
    )?;
    // Keep the recovery journal before any move; failures never recursively delete backups.
    let backup = temporary.keep();
    transfer(&journal.files, &source, &backup, cancel, after)
        .with_context(|| format!("移除未完成；恢复记录：{}", backup.display()))?;
    Ok(ModRemoval {
        count: journal.files.len(),
        backup,
    })
}
pub fn restore_removed_mods(instance: &Path, backup: &Path, cancel: &AtomicBool) -> Result<usize> {
    cancelled(cancel)?;
    let instance = instance.canonicalize()?;
    let base = metadata::confined_path(&instance, Path::new("PCL-Rust/mod-removals"))?;
    identity(backup)?;
    let backup = backup.canonicalize()?;
    if backup.parent() != Some(base.as_path()) {
        bail!("只能恢复当前实例的 Mod 移除备份");
    }
    let manifest = backup.join("restore.json");
    identity(&manifest)?;
    if fs::metadata(&manifest)?.len() > 1024 * 1024 {
        bail!("恢复记录过大");
    }
    let journal: Journal = serde_json::from_slice(&fs::read(manifest)?)?;
    if journal.instance != instance {
        bail!("恢复记录属于其他实例");
    }
    let target = mods_directory(&instance, true)?;
    let mut seen = std::collections::HashSet::new();
    for item in &journal.files {
        validate_id(&item.name)?;
        if !jar_name(&item.name) || !seen.insert(item.name.to_lowercase()) {
            bail!("恢复记录文件名无效或重复");
        }
        if target.join(&item.name).try_exists()? {
            bail!("已有同名 Mod，未覆盖：{}", item.name);
        }
    }
    transfer(&journal.files, &backup, &target, cancel, |_| {})?;
    Ok(journal.files.len())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("mods")).unwrap();
        fs::write(d.path().join("mods/a.jar"), b"a").unwrap();
        fs::write(d.path().join("mods/b.jar.disabled"), b"b").unwrap();
        d
    }
    #[test]
    fn selected_remove_restore_and_conflict_preserve_originals() {
        let d = fixture();
        let c = AtomicBool::new(false);
        let r = remove_mods(d.path(), &["a.jar".into()], &c).unwrap();
        assert!(d.path().join("mods/b.jar.disabled").exists());
        assert!(!d.path().join("mods/a.jar").exists());
        fs::write(d.path().join("mods/a.jar"), b"new").unwrap();
        assert!(restore_removed_mods(d.path(), &r.backup, &c).is_err());
        assert_eq!(fs::read(r.backup.join("a.jar")).unwrap(), b"a");
        fs::remove_file(d.path().join("mods/a.jar")).unwrap();
        assert_eq!(restore_removed_mods(d.path(), &r.backup, &c).unwrap(), 1);
        assert_eq!(fs::read(d.path().join("mods/a.jar")).unwrap(), b"a");
    }
    #[test]
    fn cancellation_mid_batch_rolls_back_only_owned_files() {
        let d = fixture();
        let c = AtomicBool::new(false);
        assert!(remove_with(
            d.path(),
            &["a.jar".into(), "b.jar.disabled".into()],
            &c,
            |_| c.store(true, Ordering::Relaxed)
        )
        .is_err());
        assert_eq!(fs::read(d.path().join("mods/a.jar")).unwrap(), b"a");
        assert_eq!(
            fs::read(d.path().join("mods/b.jar.disabled")).unwrap(),
            b"b"
        );
    }
    #[test]
    fn rollback_never_overwrites_concurrent_file() {
        let d = fixture();
        let c = AtomicBool::new(false);
        let e = remove_with(d.path(), &["a.jar".into()], &c, |_| {
            fs::write(d.path().join("mods/a.jar"), b"new").unwrap();
            c.store(true, Ordering::Relaxed);
        })
        .unwrap_err();
        assert!(format!("{e:#}").contains("部分文件未能恢复"));
        assert_eq!(fs::read(d.path().join("mods/a.jar")).unwrap(), b"new");
    }
}
