//! In-memory ownership for retrying an installation after its alias was committed.
//! Existing profiles are only reused when this same job created them and neither
//! their directory identities nor their JSON bytes changed in the meantime.
use crate::{install, instances, model::Platform};
use anyhow::{ensure, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc, Mutex},
};

#[derive(Clone, Default)]
pub struct RetryRegistration(Arc<Mutex<Option<Receipt>>>);

struct Receipt {
    path: PathBuf,
    source: String,
    json: Vec<u8>,
    file: instances::Identity,
    version: instances::Identity,
    game: instances::Identity,
}

impl RetryRegistration {
    pub fn register(
        &self,
        root: &Path,
        source: &str,
        id: &str,
        platform: &Platform,
        cancel: &AtomicBool,
    ) -> Result<String> {
        self.run(root, source, id, cancel, || {
            install::register_instance_id(root, source, id, platform, cancel)
        })
        .map(|(id, _)| id)
    }

    pub fn vanilla(
        &self,
        root: &Path,
        source: &str,
        id: &str,
        platform: &Platform,
        cancel: &AtomicBool,
        progress: impl Fn(crate::model::Progress) + Sync,
    ) -> Result<String> {
        let (result, reused) = self.run(root, source, id, cancel, || {
            install::install_vanilla_instance(root, source, id, platform, cancel, progress)
        })?;
        // The registration receipt covers only the alias. Its parent still needs
        // normal integrity validation on a retry, including all game libraries.
        if reused {
            install::verify_vanilla_parent(root, source, platform, cancel)?;
        }
        Ok(result)
    }

    fn run(
        &self,
        root: &Path,
        source: &str,
        id: &str,
        cancel: &AtomicBool,
        create: impl FnOnce() -> Result<String>,
    ) -> Result<(String, bool)> {
        install::cancelled(cancel)?;
        if source == id {
            return create().map(|id| (id, false));
        }
        crate::metadata::validate_id(id)?;
        let path = install::safe_target(root, &PathBuf::from(format!("versions/{id}/{id}.json")))?;
        let game = install::safe_target(root, &PathBuf::from(format!("instances/{id}")))?;
        let directory = path.parent().context("实例目录无效")?;
        let mut saved = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("实例登记状态损坏"))?;
        if let Some(receipt) = saved.as_ref() {
            ensure!(
                receipt.path == path && receipt.source == source,
                "重试任务的目标版本已改变"
            );
            ensure!(
                instances::identity(&path)? == receipt.file
                    && instances::identity(directory)? == receipt.version
                    && instances::identity(&game)? == receipt.game
                    && fs::read(&path)? == receipt.json,
                "已登记的实例在重试前被修改或替换；已保留现有文件，请使用新名称"
            );
            return Ok((id.to_owned(), true));
        }
        let result = create()?;
        let json = fs::read(&path)?;
        let profile: serde_json::Value = serde_json::from_slice(&json)?;
        ensure!(
            profile["id"].as_str() == Some(id) && profile["inheritsFrom"].as_str() == Some(source),
            "登记后的实例与当前任务不符"
        );
        *saved = Some(Receipt {
            path: path.clone(),
            source: source.into(),
            json,
            file: instances::identity(&path)?,
            version: instances::identity(directory)?,
            game: instances::identity(&game)?,
        });
        Ok((result, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn base(root: &Path) {
        fs::create_dir_all(root.join("versions/base")).unwrap();
        fs::write(
            root.join("versions/base/base.json"),
            br#"{"id":"base","mainClass":"net.minecraft.client.main.Main","libraries":[]}"#,
        )
        .unwrap();
    }
    #[test]
    fn retry_reuses_only_its_own_unchanged_alias_and_preserves_settings() {
        let root = tempfile::tempdir().unwrap();
        base(root.path());
        let receipt = RetryRegistration::default();
        let register = |receipt: &RetryRegistration| {
            receipt.register(
                root.path(),
                "base",
                "custom",
                &Platform::current(),
                &AtomicBool::new(false),
            )
        };
        register(&receipt).unwrap();
        fs::write(root.path().join("versions/custom/user.txt"), b"preferences").unwrap();
        register(&receipt.clone()).unwrap();
        assert!(register(&RetryRegistration::default()).is_err());
        let profile = root.path().join("versions/custom/custom.json");
        let bytes = fs::read(&profile).unwrap();
        let replacement = root.path().join("replacement.json");
        fs::write(&replacement, &bytes).unwrap();
        fs::rename(&replacement, &profile).unwrap();
        assert!(register(&receipt).is_err());
        assert_eq!(fs::read(profile).unwrap(), bytes);
        assert_eq!(
            fs::read(root.path().join("versions/custom/user.txt")).unwrap(),
            b"preferences"
        );
    }
}
