//! Repair the resolved local version without replacing its metadata or user files.
use super::*;
use anyhow::ensure;
#[path = "install_repair_generated.rs"]
mod generated;

/// Completes verifiable files of vanilla, inherited and loader versions. Only
/// declared artifacts and native members are replaced; JSON and extra files stay.
pub fn repair_version(
    root: &Path,
    id: &str,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: impl Fn(Progress) + Sync,
) -> Result<()> {
    repair_version_with_java(root, id, None, platform, cancel, progress)
}

pub fn repair_version_with_java(
    root: &Path,
    id: &str,
    java: Option<&Path>,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: impl Fn(Progress) + Sync,
) -> Result<()> {
    cancelled(cancel)?;
    ensure!(
        root.is_absolute() && root.is_dir(),
        "Minecraft 根目录必须是已有的绝对目录"
    );
    complete_official_parents(root, id, cancel)?;
    generated::regenerate(root, id, java, platform, cancel, &progress)?;
    repair_files(root, id, platform, cancel, progress)
}

/// A missing ancestor may be restored only when Mojang's authenticated manifest
/// names it and provides its SHA1. Existing JSON (including custom parents) is
/// read verbatim, never replaced by the network copy.
fn complete_official_parents(root: &Path, id: &str, cancel: &AtomicBool) -> Result<()> {
    let mut manifest = None;
    let client = http_client()?;
    complete_parents_with(root, id, cancel, |parent| {
        if manifest.is_none() {
            manifest = Some(fetch_manifest_with_cancel(cancel)?);
        }
        let entry = manifest.as_ref().unwrap()["versions"]
            .as_array()
            .context("官方版本清单缺少 versions")?
            .iter()
            .find(|entry| entry["id"].as_str() == Some(parent))
            .with_context(|| {
                format!("缺失父版本 {parent} 不在 Mojang 官方清单中，请提供该自定义版本")
            })?;
        let hash = expected_hash(entry["sha1"].as_str())?.context("官方父版本条目缺少 SHA1")?;
        let url = entry["url"].as_str().context("官方父版本条目缺少 URL")?;
        request_bytes(&client, url, Some(&hash), None, cancel)
    })
}

fn complete_parents_with(
    root: &Path,
    id: &str,
    cancel: &AtomicBool,
    mut fetch: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<()> {
    let mut current = id.to_owned();
    let mut seen = HashSet::new();
    let mut saved = Vec::new();
    loop {
        cancelled(cancel)?;
        validate_id(&current)?;
        ensure!(
            seen.insert(current.clone()) && seen.len() <= 65,
            "版本继承循环或过深"
        );
        let relative = PathBuf::from(format!("versions/{current}/{current}.json"));
        let path = safe_target(root, &relative)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && current != id => {
                let bytes = fetch(&current)?;
                ensure!(
                    bytes.len() <= MAX_METADATA_SIZE as usize,
                    "父版本元数据过大"
                );
                let value: Value = serde_json::from_slice(&bytes)?;
                ensure!(
                    value["id"].as_str() == Some(&current),
                    "官方父版本 ID 不匹配"
                );
                ensure!(
                    value.get("inheritsFrom").is_none(),
                    "官方父版本意外包含继承关系"
                );
                // Do not publish if a user edited the child while the network was busy.
                unchanged(&saved)?;
                cancelled(cancel)?;
                let folder = path.parent().context("父版本路径无效")?;
                fs::create_dir_all(folder)?;
                ensure!(safe_target(root, &relative)? == path, "父版本路径发生变化");
                let mut temporary = tempfile::NamedTempFile::new_in(folder)?;
                temporary.write_all(&bytes)?;
                temporary.as_file().sync_all()?;
                match temporary.persist_noclobber(&path) {
                    Ok(_) => bytes,
                    Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                        fs::read(&path)?
                    }
                    Err(error) => return Err(error.error.into()),
                }
            }
            Err(error) => {
                return Err(error).with_context(|| format!("读取版本元数据失败：{current}"))
            }
        };
        ensure!(bytes.len() <= MAX_METADATA_SIZE as usize, "版本元数据过大");
        let value: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            value["id"].as_str() == Some(&current),
            "版本元数据 ID 与目录不符：{current}"
        );
        saved.push((path, bytes));
        let Some(parent) = value.get("inheritsFrom") else {
            return unchanged(&saved);
        };
        current = parent
            .as_str()
            .context("inheritsFrom 必须为字符串")?
            .to_owned();
    }
}

fn repair_files(
    root: &Path,
    id: &str,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: impl Fn(Progress) + Sync,
) -> Result<()> {
    let client = http_client()?;
    with_transfer(
        &progress,
        vec![
            ProgressStage::ExistingVersionValidation,
            ProgressStage::AssetIndex,
            ProgressStage::CoreLibraries,
            ProgressStage::AssetFiles,
            ProgressStage::NativeLibraries,
        ],
        "检查既有版本与继承关系",
        |tracker| {
            repair_with(root, id, platform, cancel, tracker, |artifact, tracker| {
                download_artifact_tracked(&client, root, artifact, cancel, Some(tracker))
            })
        },
    )
}

fn snapshots(root: &Path, id: &str) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut current = id.to_owned();
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    loop {
        validate_id(&current)?;
        ensure!(
            seen.insert(current.clone()) && seen.len() <= 65,
            "版本继承循环或过深"
        );
        let path = safe_target(
            root,
            &PathBuf::from(format!("versions/{current}/{current}.json")),
        )?;
        let bytes = fs::read(&path).with_context(|| {
            format!("缺少父版本或版本元数据 {current}；请先安装该版本，未改写已有配置")
        })?;
        ensure!(bytes.len() <= MAX_METADATA_SIZE as usize, "版本元数据过大");
        let value: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            value["id"].as_str() == Some(&current),
            "版本元数据 ID 与目录不符：{current}"
        );
        result.push((path, bytes));
        let Some(parent) = value.get("inheritsFrom") else {
            break;
        };
        current = parent
            .as_str()
            .context("inheritsFrom 必须为字符串")?
            .to_owned();
    }
    Ok(result)
}

fn unchanged(saved: &[(PathBuf, Vec<u8>)]) -> Result<()> {
    for (path, bytes) in saved {
        ensure!(
            fs::read(path)? == *bytes,
            "补全过程中版本元数据发生变化，请重试；未覆盖用户配置"
        );
    }
    Ok(())
}

fn validate_artifact(root: &Path, artifact: &Artifact) -> Result<()> {
    safe_target(root, &artifact.relative_path)?;
    expected_hash(artifact.sha1.as_deref())?.with_context(|| {
        format!(
            "依赖缺少可信 SHA1，不能安全补全：{}；请使用该组件的官方安装器重新生成元数据",
            artifact.relative_path.display()
        )
    })?;
    if !artifact.url.is_empty() {
        validate_url(&artifact.url)?;
    }
    Ok(())
}

fn repair_with(
    root: &Path,
    id: &str,
    platform: &Platform,
    cancel: &AtomicBool,
    tracker: &TransferTracker<'_>,
    fetch: impl Fn(&Artifact, &TransferTracker<'_>) -> Result<()>,
) -> Result<()> {
    cancelled(cancel)?;
    ensure!(
        root.is_absolute() && root.is_dir(),
        "Minecraft 根目录必须是已有的绝对目录"
    );
    let saved = snapshots(root, id)?;
    let version = crate::metadata::resolve_version(root, id)?;
    let jar_id = version["_pcl_jar_id"]
        .as_str()
        .context("无法定位继承客户端 JAR")?;
    validate_id(jar_id)?;
    let mut artifacts = library_artifacts(&version, platform)?;
    artifacts.push(metadata_artifact(
        &version["downloads"]["client"],
        format!("versions/{jar_id}/{jar_id}.jar").into(),
    )?);
    if let Some(logging) = version.pointer("/logging/client/file") {
        let name = logging["id"].as_str().context("日志配置缺少 ID")?;
        validate_id(name)?;
        artifacts.push(metadata_artifact(
            logging,
            PathBuf::from("assets/log_configs").join(name),
        )?);
    }
    // Installer-generated files are not necessarily part of the Java classpath.
    // Their receipt records locally derived hashes separately from official ones.
    for file in version
        .pointer("/_pcl_forge_install/files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let relative = safe_relative(file["path"].as_str().context("安装收据缺少路径")?)?;
        ensure!(
            relative.starts_with("libraries"),
            "安装收据文件不在支持库目录中"
        );
        if !artifacts
            .iter()
            .any(|artifact| artifact.relative_path == relative)
        {
            artifacts.push(Artifact {
                relative_path: relative,
                url: String::new(),
                sha1: expected_hash(file["sha1"].as_str())?,
                size: file["size"].as_u64(),
                native: false,
                excludes: vec![],
            });
        }
    }
    let mut paths = HashMap::new();
    for artifact in &artifacts {
        validate_artifact(root, artifact)?;
        if let Some(previous) = paths.insert(
            artifact.relative_path.clone(),
            (&artifact.sha1, artifact.size),
        ) {
            ensure!(
                previous == (&artifact.sha1, artifact.size),
                "补全清单中存在冲突文件：{}",
                artifact.relative_path.display()
            );
        }
        if artifact.url.is_empty()
            && !cache_valid(
                &safe_target(root, &artifact.relative_path)?,
                artifact,
                cancel,
            )?
        {
            bail!(
                "本地安装器生成文件缺失或损坏：{}；需要重新运行对应加载器处理器，未覆盖版本配置",
                artifact.relative_path.display()
            );
        }
    }
    tracker.finish_stage();
    tracker.begin_stage(ProgressStage::AssetIndex, "校验资源索引", 1, Some(1));
    let index_id = version
        .pointer("/assetIndex/id")
        .and_then(Value::as_str)
        .context("版本缺少资源索引 ID")?;
    validate_id(index_id)?;
    let index_artifact = metadata_artifact(
        &version["assetIndex"],
        format!("assets/indexes/{index_id}.json").into(),
    )?;
    validate_artifact(root, &index_artifact)?;
    let index_path = safe_target(root, &index_artifact.relative_path)?;
    if !cache_valid(&index_path, &index_artifact, cancel)? {
        fetch(&index_artifact, tracker)?;
    }
    ensure!(
        cache_valid(&index_path, &index_artifact, cancel)?,
        "资源索引校验失败"
    );
    let index: Value = serde_json::from_slice(&fs::read(&index_path)?)?;
    let objects = index["objects"]
        .as_object()
        .context("资源索引缺少 objects")?;
    let mut assets = Vec::new();
    let mut mapped = Vec::new();
    let mut hashes = HashSet::new();
    let resources = if index["map_to_resources"].as_bool() == Some(true) {
        let path = crate::config::instance_game_dir(root, id)?;
        let canonical = root.canonicalize()?;
        Some(
            path.strip_prefix(&canonical)
                .or_else(|_| path.strip_prefix(root))
                .context("实例资源目录不属于当前根目录")?
                .join("resources"),
        )
    } else {
        None
    };
    for (name, value) in objects {
        let name = safe_relative(name)?;
        let hash = expected_hash(value["hash"].as_str())?.context("资源缺少 SHA1")?;
        let artifact = Artifact {
            relative_path: format!("assets/objects/{}/{hash}", &hash[..2]).into(),
            url: format!(
                "https://resources.download.minecraft.net/{}/{hash}",
                &hash[..2]
            ),
            sha1: Some(hash.clone()),
            size: Some(value["size"].as_u64().context("资源缺少大小")?),
            native: false,
            excludes: vec![],
        };
        if hashes.insert(hash) {
            assets.push(artifact.clone());
        }
        if index["virtual"].as_bool() == Some(true) {
            mapped.push((
                artifact.clone(),
                PathBuf::from("assets/virtual").join(index_id).join(&name),
            ));
        }
        if let Some(directory) = &resources {
            mapped.push((artifact, directory.join(name)));
        }
    }
    for (_, path) in &mapped {
        safe_target(root, path)?;
    }
    unchanged(&saved)?;
    tracker.finished_item(false, false);
    tracker.finish_stage();
    tracker.plan_files(
        (artifacts.len() + assets.len() + mapped.len()) as u64,
        (artifacts.len() + assets.len()) as u64,
    );
    tracker.begin_stage(
        ProgressStage::CoreLibraries,
        "补全核心与支持库",
        artifacts.len() as u64,
        Some(1),
    );
    for artifact in &artifacts {
        cancelled(cancel)?;
        tracker.message(format!("校验或补全 {}", artifact.relative_path.display()));
        if !cache_valid(
            &safe_target(root, &artifact.relative_path)?,
            artifact,
            cancel,
        )? {
            fetch(artifact, tracker)?;
        }
        ensure!(
            cache_valid(
                &safe_target(root, &artifact.relative_path)?,
                artifact,
                cancel
            )?,
            "补全文件校验失败：{}",
            artifact.relative_path.display()
        );
        tracker.finished_item(true, true);
    }
    tracker.finish_stage();
    tracker.begin_stage(
        ProgressStage::AssetFiles,
        "补全资源文件",
        (assets.len() + mapped.len()) as u64,
        Some(1),
    );
    for artifact in &assets {
        tracker.message(format!("校验或补全 {}", artifact.relative_path.display()));
        if !cache_valid(
            &safe_target(root, &artifact.relative_path)?,
            artifact,
            cancel,
        )? {
            fetch(artifact, tracker)?;
        }
        ensure!(
            cache_valid(
                &safe_target(root, &artifact.relative_path)?,
                artifact,
                cancel
            )?,
            "资源文件校验失败"
        );
        tracker.finished_item(true, true);
    }
    tracker.set_concurrency_limit(Some(0));
    for (artifact, target) in mapped {
        copy_asset(root, &artifact, target, cancel)?;
        tracker.finished_item(true, false);
    }
    tracker.finish_stage();
    tracker.begin_stage(
        ProgressStage::NativeLibraries,
        "补全原生库（保留额外文件）",
        0,
        Some(0),
    );
    let stage = tempfile::tempdir_in(root)?;
    for artifact in artifacts.iter().filter(|artifact| artifact.native) {
        extract_natives(
            &safe_target(root, &artifact.relative_path)?,
            stage.path(),
            &artifact.excludes,
            cancel,
        )?;
    }
    unchanged(&saved)?;
    restore_natives(
        root,
        stage.path(),
        &PathBuf::from(format!("versions/{id}/natives")),
        cancel,
    )?;
    unchanged(&saved)?;
    cancelled(cancel)?;
    tracker.message(format!("{id} 文件补全完成，版本配置与额外文件已保留"));
    tracker.finish_stage();
    Ok(())
}

fn restore_natives(root: &Path, source: &Path, relative: &Path, cancel: &AtomicBool) -> Result<()> {
    for entry in fs::read_dir(source)? {
        cancelled(cancel)?;
        let entry = entry?;
        let relative = relative.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            restore_natives(root, &entry.path(), &relative, cancel)?;
        } else {
            ensure!(entry.file_type()?.is_file(), "原生库包含非法文件类型");
            let bytes = fs::read(entry.path())?;
            let artifact = Artifact {
                relative_path: relative,
                url: String::new(),
                sha1: Some(format!("{:x}", Sha1::digest(&bytes))),
                size: Some(bytes.len() as u64),
                native: false,
                excludes: vec![],
            };
            if !cache_valid(
                &safe_target(root, &artifact.relative_path)?,
                &artifact,
                cancel,
            )? {
                store_verified(root, &artifact, &mut bytes.as_slice(), cancel)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    fn file(root: &Path, name: &str, bytes: &[u8]) {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn download(bytes: &[u8], path: &str) -> Value {
        json!({"url":format!("https://libraries.minecraft.net/{path}"),
            "sha1":format!("{:x}",Sha1::digest(bytes)),"size":bytes.len(),"path":path})
    }
    fn fixture(root: &Path) -> HashMap<PathBuf, Vec<u8>> {
        let asset = b"legacy resource";
        let hash = format!("{:x}", Sha1::digest(asset));
        let index = serde_json::to_vec(&json!({"virtual":true,"map_to_resources":true,
            "objects":{"sounds/a.ogg":{"hash":hash,"size":asset.len()}}}))
        .unwrap();
        let base = json!({"id":"base","mainClass":"example.Main","type":"release",
            "downloads":{"client":download(b"client","client")},
            "assetIndex":{"id":"old","url":"https://piston-meta.mojang.com/index.json",
                "sha1":format!("{:x}",Sha1::digest(&index)),"size":index.len()},
            "libraries":[],"logging":{"client":{"file":{
                "id":"log.xml","url":"https://libraries.minecraft.net/log.xml",
                "sha1":format!("{:x}",Sha1::digest(b"log")),"size":3}}}});
        let child = json!({"id":"custom","inheritsFrom":"base",
            "arguments":{"jvm":["-Dmy.setting=true"]},"customUserKey":"preserve",
            "libraries":[{"name":"org.example:custom:1","downloads":{"artifact":download(b"library","org/example/custom/1/custom-1.jar")}}]});
        file(
            root,
            "versions/base/base.json",
            &serde_json::to_vec_pretty(&base).unwrap(),
        );
        file(
            root,
            "versions/custom/custom.json",
            &serde_json::to_vec_pretty(&child).unwrap(),
        );
        file(root, "versions/custom/natives/user-extra.txt", b"preserve");
        fs::create_dir_all(root.join("instances/custom")).unwrap();
        HashMap::from([
            (PathBuf::from("versions/base/base.jar"), b"client".to_vec()),
            (
                PathBuf::from("libraries/org/example/custom/1/custom-1.jar"),
                b"library".to_vec(),
            ),
            (PathBuf::from("assets/indexes/old.json"), index),
            (PathBuf::from("assets/log_configs/log.xml"), b"log".to_vec()),
            (
                PathBuf::from(format!("assets/objects/{}/{hash}", &hash[..2])),
                asset.to_vec(),
            ),
        ])
    }
    #[test]
    fn repairs_inherited_files_and_legacy_layout_without_rewriting_profiles() {
        let root = tempfile::tempdir().unwrap();
        let files = fixture(root.path());
        let snapshots = snapshots(root.path(), "custom").unwrap();
        file(
            root.path(),
            "libraries/org/example/custom/1/custom-1.jar",
            b"broken cache",
        );
        let fetched = Mutex::new(Vec::new());
        let cancel = AtomicBool::new(false);
        with_transfer(
            &|_| {},
            vec![ProgressStage::ExistingVersionValidation],
            "test",
            |tracker| {
                repair_with(
                    root.path(),
                    "custom",
                    &Platform::current(),
                    &cancel,
                    tracker,
                    |artifact, _| {
                        fetched.lock().unwrap().push(artifact.relative_path.clone());
                        store_verified(
                            root.path(),
                            artifact,
                            &mut files[&artifact.relative_path].as_slice(),
                            &cancel,
                        )
                    },
                )
            },
        )
        .unwrap();
        unchanged(&snapshots).unwrap();
        assert_eq!(fetched.lock().unwrap().len(), 5);
        assert_eq!(
            fs::read(root.path().join("instances/custom/resources/sounds/a.ogg")).unwrap(),
            b"legacy resource"
        );
        assert_eq!(
            fs::read(root.path().join("assets/virtual/old/sounds/a.ogg")).unwrap(),
            b"legacy resource"
        );
        assert_eq!(
            fs::read(root.path().join("versions/custom/natives/user-extra.txt")).unwrap(),
            b"preserve"
        );
        with_transfer(
            &|_| {},
            vec![ProgressStage::ExistingVersionValidation],
            "test",
            |tracker| {
                repair_with(
                    root.path(),
                    "custom",
                    &Platform::current(),
                    &cancel,
                    tracker,
                    |_, _| panic!("valid cache must not fetch"),
                )
            },
        )
        .unwrap();
    }
    #[test]
    fn missing_generated_output_and_unhashed_library_fail_before_downloads() {
        for receipt in [false, true] {
            let root = tempfile::tempdir().unwrap();
            fixture(root.path());
            let path = root.path().join("versions/custom/custom.json");
            let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            if receipt {
                value["_pcl_forge_install"] = json!({"files":[{"path":"libraries/generated.jar","sha1":"0000000000000000000000000000000000000000","size":4}]});
            } else {
                value["libraries"][0]["downloads"]["artifact"]
                    .as_object_mut()
                    .unwrap()
                    .remove("sha1");
            }
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            let before = fs::read(&path).unwrap();
            let result = with_transfer(
                &|_| {},
                vec![ProgressStage::ExistingVersionValidation],
                "test",
                |tracker| {
                    repair_with(
                        root.path(),
                        "custom",
                        &Platform::current(),
                        &AtomicBool::new(false),
                        tracker,
                        |_, _| panic!("preflight must prevent writes"),
                    )
                },
            );
            assert!(result.is_err());
            assert_eq!(fs::read(path).unwrap(), before);
            assert!(!root.path().join("assets").exists());
        }
    }
    #[test]
    fn native_repair_changes_only_declared_members_and_cancellation_does_not_write() {
        let root = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir().unwrap();
        file(stage.path(), "sub/library", b"correct");
        file(
            root.path(),
            "versions/custom/natives/sub/library",
            b"broken",
        );
        file(root.path(), "versions/custom/natives/user", b"user");
        let relative = Path::new("versions/custom/natives");
        assert!(
            restore_natives(root.path(), stage.path(), relative, &AtomicBool::new(true)).is_err()
        );
        assert_eq!(
            fs::read(root.path().join(relative).join("sub/library")).unwrap(),
            b"broken"
        );
        restore_natives(root.path(), stage.path(), relative, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            fs::read(root.path().join(relative).join("sub/library")).unwrap(),
            b"correct"
        );
        assert_eq!(
            fs::read(root.path().join(relative).join("user")).unwrap(),
            b"user"
        );
    }
}

#[cfg(test)]
mod parent_tests {
    use super::*;
    fn child(root: &Path, value: &str) -> PathBuf {
        let folder = root.join("versions/custom");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join("custom.json");
        fs::write(&path, value).unwrap();
        path
    }
    #[test]
    fn restores_missing_official_parent_preserving_exact_child() {
        let dir = tempfile::tempdir().unwrap();
        let original = r#"{ "id":"custom", "inheritsFrom":"1.21.1", "user":"keep" }"#;
        let path = child(dir.path(), original);
        let mut calls = 0;
        complete_parents_with(dir.path(), "custom", &AtomicBool::new(false), |id| {
            calls += 1;
            assert_eq!(id, "1.21.1");
            Ok(br#"{"id":"1.21.1"}"#.to_vec())
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(fs::read_to_string(path).unwrap(), original);
        complete_parents_with(dir.path(), "custom", &AtomicBool::new(false), |_| {
            panic!("existing parent must not be fetched")
        })
        .unwrap();
    }
    #[test]
    fn invalid_missing_or_changed_parent_does_not_publish() {
        for (body, change) in [
            (br#"{"id":"wrong"}"#.as_slice(), false),
            (br#"{"id":"1.21.1"}"#.as_slice(), true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = child(dir.path(), r#"{"id":"custom","inheritsFrom":"1.21.1"}"#);
            assert!(
                complete_parents_with(dir.path(), "custom", &AtomicBool::new(false), |_| {
                    if change {
                        fs::write(&path, br#"{"id":"custom","inheritsFrom":"another"}"#)?;
                    }
                    Ok(body.to_vec())
                })
                .is_err()
            );
            assert!(!dir.path().join("versions/1.21.1/1.21.1.json").exists());
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(
            complete_parents_with(dir.path(), "custom", &AtomicBool::new(false), |_| panic!(
                "cannot invent selected metadata"
            ))
            .is_err()
        );
    }
}
