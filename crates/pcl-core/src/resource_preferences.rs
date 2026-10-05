//! Public metadata cache and explicit resource presentation preferences.
use super::*;
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSort {
    #[default]
    Relevance,
    Downloads,
    Updated,
    Newest,
}
impl SearchSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Relevance => "相关度",
            Self::Downloads => "下载量",
            Self::Updated => "最近更新",
            Self::Newest => "最近发布",
        }
    }
    pub fn index(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Downloads => "downloads",
            Self::Updated => "updated",
            Self::Newest => "newest",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceNaming {
    #[default]
    Original,
    ProjectVersion,
}
impl ResourceNaming {
    pub fn label(self) -> &'static str {
        match self {
            Self::Original => "原始文件名",
            Self::ProjectVersion => "项目名与版本",
        }
    }
}
fn named_file(title: &str, version: &str, original: &str) -> Result<String> {
    let extension = Path::new(original)
        .extension()
        .and_then(|s| s.to_str())
        .context("资源没有文件扩展名")?;
    let clean = |s: &str| {
        s.chars()
            .map(|c| {
                if c.is_control() || "/\\:*?\"<>|".contains(c) {
                    '_'
                } else {
                    c
                }
            })
            .take(80)
            .collect::<String>()
            .trim_matches([' ', '.'])
            .to_owned()
    };
    let name = format!("{}-{}.{}", clean(title), clean(version), extension);
    validate_id(&name)?;
    Ok(name)
}
impl InstallPlan {
    /// Change destination names only; trusted URLs and expected hashes stay untouched.
    pub fn with_naming(mut self, naming: ResourceNaming) -> Result<Self> {
        if naming == ResourceNaming::Original {
            return Ok(self);
        }
        let mut names = std::collections::HashSet::new();
        for item in self.ordered.iter_mut().filter(|p| !p.reused) {
            let chosen = resource_file(self.context.kind, &item.version)?
                .filename
                .clone();
            let name = named_file(&item.title, &item.version.version_number, &chosen)?;
            ensure!(
                names.insert(name.to_lowercase()),
                "所选命名方式产生同名文件，请使用原始文件名"
            );
            no_conflict(&self.context.directory(&item.version, false)?, &name)?;
            for file in &mut item.version.files {
                if file.filename == chosen {
                    file.filename = name.clone();
                }
            }
        }
        Ok(self)
    }
}
#[derive(Serialize, Deserialize)]
struct CacheEntry {
    key: String,
    saved: u64,
    sha256: String,
    value: serde_json::Value,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn cache_root() -> Result<PathBuf> {
    Ok(dirs::cache_dir()
        .context("无法确定资源缓存目录")?
        .join("pcl-rust/resource-metadata-v1"))
}
fn cached_at<T: Serialize + DeserializeOwned>(
    root: &Path,
    key: &str,
    cancel: &AtomicBool,
    fetch: impl FnOnce() -> Result<T>,
) -> Result<T> {
    cancelled(cancel)?;
    let filename = format!("{:x}.json", Sha256::digest(key.as_bytes()));
    let path = root.join(filename);
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= JSON_LIMIT {
            if let Ok(bytes) = fs::read(&path) {
                if let Ok(entry) = serde_json::from_slice::<CacheEntry>(&bytes) {
                    let age = now().checked_sub(entry.saved);
                    let payload = serde_json::to_vec(&entry.value)?;
                    if entry.key == key
                        && age.is_some_and(|v| v < 300)
                        && entry.sha256 == format!("{:x}", Sha256::digest(&payload))
                    {
                        if let Ok(value) = serde_json::from_value(entry.value) {
                            cancelled(cancel)?;
                            return Ok(value);
                        }
                    }
                }
            }
        }
    }
    let result = fetch()?;
    cancelled(cancel)?;
    let value = serde_json::to_value(&result)?;
    let payload = serde_json::to_vec(&value)?;
    ensure!(payload.len() as u64 <= JSON_LIMIT, "资源元数据超过缓存上限");
    fs::create_dir_all(root).context("无法创建资源元数据缓存")?;
    let meta = fs::symlink_metadata(root)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "资源缓存目录不能是符号链接"
    );
    let entry = CacheEntry {
        key: key.into(),
        saved: now(),
        sha256: format!("{:x}", Sha256::digest(&payload)),
        value,
    };
    let mut temp = tempfile::NamedTempFile::new_in(root)?;
    serde_json::to_writer(&mut temp, &entry)?;
    temp.as_file().sync_all()?;
    cancelled(cancel)?;
    temp.persist(&path).map_err(|e| e.error)?;
    Ok(result)
}
pub(super) fn cached<T: Serialize + DeserializeOwned>(
    key: &str,
    cancel: &AtomicBool,
    fetch: impl FnOnce() -> Result<T>,
) -> Result<T> {
    cached_at(&cache_root()?, key, cancel, fetch)
}
#[cfg(test)]
mod tests {
    #[test]
    fn renamed_plan_preserves_official_url_and_hash_and_rejects_conflicts() {
        let d = tempfile::tempdir().unwrap();
        let version:ModrinthVersion=serde_json::from_value(serde_json::json!({"id":"v1","project_id":"p1","name":"Readable Name","version_number":"1.0","version_type":"release","date_published":"2026-01-01","game_versions":["1.21.1"],"loaders":["fabric"],"files":[{"hashes":{"sha1":"a".repeat(40),"sha512":"b".repeat(128)},"url":"https://cdn.modrinth.com/data/p1/versions/v1/original.jar","filename":"original.jar","primary":true,"size":1,"file_type":null}]})).unwrap();
        let plan = || InstallPlan {
            context: PlanContext {
                kind: ResourceKind::Mod,
                instance: d.path().to_owned(),
                world: None,
                minecraft: "1.21.1".into(),
                loader: "fabric".into(),
            },
            primary_project: "p1".into(),
            ordered: vec![PlannedFile {
                version: version.clone(),
                title: "Readable Name".into(),
                reused: false,
            }],
            existing: BTreeMap::new(),
        };
        let named = plan().with_naming(ResourceNaming::ProjectVersion).unwrap();
        let file = &named.ordered[0].version.files[0];
        assert_eq!(file.filename, "Readable Name-1.0.jar");
        assert_eq!(file.url, version.files[0].url);
        assert_eq!(file.hashes, version.files[0].hashes);
        fs::create_dir(d.path().join("mods")).unwrap();
        fs::write(d.path().join("mods/Readable Name-1.0.jar"), b"keep").unwrap();
        assert!(plan().with_naming(ResourceNaming::ProjectVersion).is_err());
        assert_eq!(
            fs::read(d.path().join("mods/Readable Name-1.0.jar")).unwrap(),
            b"keep"
        );
    }

    use super::*;
    #[test]
    fn provider_cache_isolated_corruption_refresh_and_cancel_no_write() {
        let d = tempfile::tempdir().unwrap();
        let c = AtomicBool::new(false);
        let a: Vec<u32> = cached_at(d.path(), "mr:project:1", &c, || Ok(vec![1])).unwrap();
        assert_eq!(a, vec![1]);
        let a: Vec<u32> = cached_at(d.path(), "mr:project:1", &c, || bail!("must reuse")).unwrap();
        assert_eq!(a, vec![1]);
        let b: Vec<u32> = cached_at(d.path(), "cf:project:1", &c, || Ok(vec![2])).unwrap();
        assert_eq!(b, vec![2]);
        for f in fs::read_dir(d.path()).unwrap() {
            fs::write(f.unwrap().path(), b"broken").unwrap();
        }
        let a: Vec<u32> = cached_at(d.path(), "mr:project:1", &c, || Ok(vec![3])).unwrap();
        assert_eq!(a, vec![3]);
        assert!(
            cached_at::<Vec<u32>>(d.path(), "new", &AtomicBool::new(true), || panic!(
                "cancelled before fetch"
            ))
            .is_err()
        );
    }
    #[test]
    fn expired_cache_does_not_mask_network_failure() {
        let d = tempfile::tempdir().unwrap();
        let c = AtomicBool::new(false);
        cached_at(d.path(), "mr", &c, || Ok(vec![1])).unwrap();
        let path = fs::read_dir(d.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut e: CacheEntry = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        e.saved = 0;
        fs::write(path, serde_json::to_vec(&e).unwrap()).unwrap();
        assert!(cached_at::<Vec<u32>>(d.path(), "mr", &c, || bail!("offline")).is_err());
    }
    #[test]
    fn naming_removes_path_characters_and_keeps_extension() {
        assert_eq!(
            named_file("目录/../项目", "1:2", "file.jar").unwrap(),
            "目录_.._项目-1_2.jar"
        );
        assert!(named_file("x", "v", "bad").is_err());
    }
}
