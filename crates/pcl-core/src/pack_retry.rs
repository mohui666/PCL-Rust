//! A retry belongs to one job, never to an arbitrary existing pack instance.
use super::*;
use crate::instances::{identity, Identity};
use anyhow::ensure;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct PackRetry(Arc<Mutex<State>>);
struct State {
    retryable: bool,
    saved: Option<Receipt>,
}
impl Default for PackRetry {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State {
            retryable: true,
            saved: None,
        })))
    }
}
#[derive(PartialEq, Eq)]
pub(super) struct Spec {
    root: PathBuf,
    id: String,
    optional: bool,
    pack_hash: String,
    retain_receipt: bool,
}
struct Receipt {
    spec: Spec,
    files: BTreeMap<PathBuf, Stamp>,
}
#[derive(PartialEq, Eq)]
struct Stamp {
    identity: Identity,
    hash: Option<String>,
}
fn digest(path: &Path, cancel: &AtomicBool) -> Result<String> {
    let before = fs::symlink_metadata(path)?;
    ensure!(
        before.is_file() && !before.file_type().is_symlink(),
        "整合包收据只接受普通文件"
    );
    let stamp = identity(path)?;
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        install::cancelled(cancel)?;
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hasher.update(&buffer[..size]);
    }
    let after = fs::symlink_metadata(path)?;
    ensure!(
        identity(path)? == stamp
            && before.len() == after.len()
            && before.modified()? == after.modified()?,
        "文件在记录收据时发生变化：{}",
        path.display()
    );
    Ok(format!("{:x}", hasher.finalize()))
}
impl Spec {
    pub(super) fn new(
        root: &Path,
        pack: &Path,
        id: &str,
        optional: bool,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        validate_id(id)?;
        ensure!(root.is_absolute(), "Minecraft 根目录必须为绝对路径");
        Ok(Self {
            root: root.into(),
            id: id.into(),
            optional,
            pack_hash: digest(pack, cancel)?,
            retain_receipt: true,
        })
    }
    pub(super) fn without_tail(mut self) -> Self {
        self.retain_receipt = false;
        self
    }
    fn paths(&self) -> Result<[PathBuf; 2]> {
        Ok([
            install::safe_target(&self.root, &PathBuf::from(format!("instances/{}", self.id)))?,
            install::safe_target(&self.root, &PathBuf::from(format!("versions/{}", self.id)))?,
        ])
    }
    fn residual(&self) -> bool {
        self.paths()
            .map(|paths| paths.iter().any(|p| fs::symlink_metadata(p).is_ok()))
            .unwrap_or(true)
    }
    fn capture(&self, cancel: &AtomicBool) -> Result<BTreeMap<PathBuf, Stamp>> {
        let mut pending = self
            .paths()?
            .into_iter()
            .map(|p| (p, 0usize))
            .collect::<Vec<_>>();
        let mut result = BTreeMap::new();
        while let Some((path, depth)) = pending.pop() {
            install::cancelled(cancel)?;
            ensure!(
                depth <= 64 && result.len() < 100_000,
                "实例内容超出重试收据限制"
            );
            let stamp = identity(&path)?;
            let meta = fs::symlink_metadata(&path)?;
            let hash = if meta.is_dir() {
                for entry in fs::read_dir(&path)? {
                    pending.push((entry?.path(), depth + 1));
                }
                None
            } else {
                Some(digest(&path, cancel)?)
            };
            ensure!(identity(&path)? == stamp, "实例内容在记录期间发生变化");
            result.insert(
                path,
                Stamp {
                    identity: stamp,
                    hash,
                },
            );
        }
        Ok(result)
    }
}
impl PackRetry {
    pub fn retryable(&self) -> bool {
        self.0.lock().map(|s| s.retryable).unwrap_or(false)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn install_with_java(
        &self,
        root: &Path,
        pack: &Path,
        id: &str,
        optional: bool,
        java: Option<&Path>,
        platform: &Platform,
        cancel: &AtomicBool,
        progress: impl Fn(Progress) + Sync,
    ) -> Result<String> {
        super::install_pack_attempt(
            root, pack, id, optional, java, platform, cancel, progress, self,
        )
    }
    pub(super) fn registered(
        &self,
        spec: Spec,
        cancel: &AtomicBool,
        create: impl FnOnce() -> Result<String>,
    ) -> Result<String> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("整合包重试状态损坏"))?;
        ensure!(
            state.retryable,
            "此任务已保留无法自动继续的实例文件，请检查任务中列出的路径后使用新名称安装"
        );
        if let Some(saved) = state.saved.as_ref() {
            let checked = (|| {
                ensure!(saved.spec == spec, "重试的整合包内容、目标或可选项已改变");
                ensure!(
                    saved.files == spec.capture(cancel)?,
                    "已登记实例的文件内容或身份已改变"
                );
                Ok::<_, anyhow::Error>(spec.id.clone())
            })();
            if let Err(error) = &checked {
                if !error
                    .chain()
                    .any(|e| e.is::<crate::model::OperationCancelled>())
                {
                    state.retryable = false;
                }
            }
            return checked.context("不能继续此整合包任务；已有文件保留，请检查后使用新实例名称");
        }
        let id = match create() {
            Ok(id) => id,
            Err(error) => {
                if spec.residual() {
                    state.retryable = false;
                    return Err(error).context(format!("任务未完整登记，文件保留在 {} 和 {}；不能自动重试，请检查保留文件后用新实例名称安装",spec.root.join("instances").join(&spec.id).display(),spec.root.join("versions").join(&spec.id).display()));
                }
                return Err(error);
            }
        };
        // A complete base-only installation has no later fallible component to
        // resume, so avoid rereading all saves and mods for an unused receipt.
        if !spec.retain_receipt {
            return Ok(id);
        }
        let files = match spec.capture(cancel) {
            Ok(files) => files,
            Err(error) => {
                state.retryable = false;
                return Err(error).context("实例已登记，但重试收据未建立；文件已保留，请从版本列表管理此实例，不可一键重试");
            }
        };
        state.saved = Some(Receipt { spec, files });
        Ok(id)
    }
    /// Successful tail installation may only add files; original pack files are immutable.
    pub(super) fn checkpoint(&self, cancel: &AtomicBool) -> Result<()> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("整合包重试状态损坏"))?;
        let result = (|| {
            let saved = state.saved.as_mut().context("整合包登记收据缺失")?;
            let files = saved.spec.capture(cancel)?;
            ensure!(
                saved
                    .files
                    .iter()
                    .all(|(path, stamp)| files.get(path) == Some(stamp)),
                "原整合包文件在组件安装期间被修改"
            );
            saved.files = files;
            Ok(())
        })();
        if result.is_err() {
            state.retryable = false;
        }
        result.context("组件步骤已结束，但实例收据无法安全更新；文件已保留，不可一键重试")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let pack = root.path().join("pack.zip");
        fs::write(&pack, b"same pack").unwrap();
        (root, pack)
    }
    fn create(root: &Path) -> Result<String> {
        fs::create_dir_all(root.join("instances/test/config"))?;
        fs::create_dir_all(root.join("versions/test"))?;
        fs::write(root.join("instances/test/config/a.txt"), b"pack file")?;
        fs::write(
            root.join("versions/test/test.json"),
            br#"{"id":"test","inheritsFrom":"base"}"#,
        )?;
        Ok("test".into())
    }
    #[test]
    fn committed_pack_retry_skips_base_and_preserves_completed_tail_files() {
        let (root, pack) = setup();
        let cancel = AtomicBool::new(false);
        let retry = PackRetry::default();
        retry
            .registered(
                Spec::new(root.path(), &pack, "test", true, &cancel).unwrap(),
                &cancel,
                || create(root.path()),
            )
            .unwrap();
        fs::create_dir(root.path().join("instances/test/mods")).unwrap();
        fs::write(
            root.path().join("instances/test/mods/bridge.jar"),
            b"verified bridge",
        )
        .unwrap();
        retry.checkpoint(&cancel).unwrap();
        let new_download = root.path().join("new-download.zip");
        fs::write(&new_download, b"same pack").unwrap();
        retry
            .clone()
            .registered(
                Spec::new(root.path(), &new_download, "test", true, &cancel).unwrap(),
                &cancel,
                || panic!("base must not repeat"),
            )
            .unwrap();
        assert!(retry.retryable());
        assert_eq!(
            fs::read(root.path().join("instances/test/mods/bridge.jar")).unwrap(),
            b"verified bridge"
        );
    }
    #[test]
    fn replacement_or_source_changes_disable_retry_without_touching_files() {
        for changed_pack in [false, true] {
            let (root, pack) = setup();
            let cancel = AtomicBool::new(false);
            let retry = PackRetry::default();
            retry
                .registered(
                    Spec::new(root.path(), &pack, "test", false, &cancel).unwrap(),
                    &cancel,
                    || create(root.path()),
                )
                .unwrap();
            if changed_pack {
                fs::write(&pack, b"other pack").unwrap();
            } else {
                let replacement = root.path().join("replacement");
                fs::write(&replacement, b"pack file").unwrap();
                fs::rename(
                    &replacement,
                    root.path().join("instances/test/config/a.txt"),
                )
                .unwrap();
            }
            assert!(retry
                .registered(
                    Spec::new(root.path(), &pack, "test", false, &cancel).unwrap(),
                    &cancel,
                    || panic!("must not overwrite")
                )
                .is_err());
            assert!(!retry.retryable());
            assert_eq!(
                fs::read(root.path().join("instances/test/config/a.txt")).unwrap(),
                b"pack file"
            );
        }
    }
    #[test]
    fn pre_registration_residue_is_not_adopted_by_a_new_or_retried_job() {
        let (root, pack) = setup();
        let cancel = AtomicBool::new(false);
        let retry = PackRetry::default();
        let error = retry
            .registered(
                Spec::new(root.path(), &pack, "test", false, &cancel).unwrap(),
                &cancel,
                || {
                    fs::create_dir_all(root.path().join("instances/test"))?;
                    fs::write(root.path().join("instances/test/user.txt"), b"retained")?;
                    bail!("cancel before registration")
                },
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("不能自动重试"));
        assert!(!retry.retryable());
        let fresh = PackRetry::default();
        assert!(fresh
            .registered(
                Spec::new(root.path(), &pack, "test", false, &cancel).unwrap(),
                &cancel,
                || {
                    destinations(root.path(), "test")?;
                    panic!("never adopt")
                }
            )
            .is_err());
        assert!(!fresh.retryable());
        assert_eq!(
            fs::read(root.path().join("instances/test/user.txt")).unwrap(),
            b"retained"
        );
    }
}
