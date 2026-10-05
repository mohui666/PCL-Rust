//! Re-run a recorded publisher installer in isolation; publish only receipt-matching bytes.
use super::*;
use crate::{
    forge::{self, ForgeKind},
    java,
};

struct Plan {
    kind: ForgeKind,
    minecraft: String,
    loader: String,
    installer_sha1: String,
    missing: Vec<Artifact>,
}
fn plan(
    root: &Path,
    version: &Value,
    platform: &Platform,
    cancel: &AtomicBool,
) -> Result<Option<Plan>> {
    let Some(receipt) = version.get("_pcl_forge_install") else {
        return Ok(None);
    };
    let libraries = library_artifacts(version, platform)?;
    let mut missing = Vec::new();
    for file in receipt["files"]
        .as_array()
        .context("加载器收据缺少文件列表")?
    {
        let path = safe_relative(file["path"].as_str().context("加载器收据缺少文件路径")?)?;
        ensure!(path.starts_with("libraries"), "加载器收据越过支持库目录");
        let artifact = Artifact {
            relative_path: path,
            url: String::new(),
            sha1: Some(expected_hash(file["sha1"].as_str())?.context("加载器收据缺少 SHA1")?),
            size: Some(file["size"].as_u64().context("加载器收据缺少文件大小")?),
            native: false,
            excludes: vec![],
        };
        if libraries
            .iter()
            .any(|a| a.relative_path == artifact.relative_path && !a.url.is_empty())
        {
            continue;
        }
        if !cache_valid(
            &safe_target(root, &artifact.relative_path)?,
            &artifact,
            cancel,
        )? {
            missing.push(artifact);
        }
    }
    if missing.is_empty() {
        return Ok(None);
    }
    let kind = match receipt["kind"].as_str() {
        Some("forge") => ForgeKind::Forge,
        Some("neoforge") => ForgeKind::NeoForge,
        _ => bail!("未知加载器安装收据，不能自动运行处理器"),
    };
    let minecraft = receipt["minecraft"]
        .as_str()
        .context("加载器收据缺少 Minecraft 版本")?
        .to_owned();
    validate_id(&minecraft)?;
    let mut loader = if let Some(loader) = receipt["loader"].as_str() {
        loader.to_owned()
    } else {
        ensure!(kind == ForgeKind::Forge, "安装收据缺少加载器版本");
        let prefix = format!("net.minecraftforge:forge:{minecraft}-");
        let versions = version["libraries"]
            .as_array()
            .context("版本缺少支持库")?
            .iter()
            .filter_map(|l| {
                l["name"]
                    .as_str()?
                    .strip_prefix(&prefix)
                    .map(|s| s.split(':').next().unwrap().to_owned())
            })
            .collect::<HashSet<_>>();
        ensure!(versions.len() == 1, "旧 Forge 版本坐标不唯一，不能自动重建");
        versions.into_iter().next().unwrap()
    };
    if kind == ForgeKind::Forge && receipt["loader"].is_null() {
        if let Some(version) = loader.strip_suffix(&format!("-{minecraft}")) {
            loader = version.to_owned();
        }
    }
    validate_id(&loader)?;
    let installer_sha1 = expected_hash(receipt["installerSha1"].as_str())?
        .context("安装收据缺少安装器 SHA1，不能自动重建")?;
    Ok(Some(Plan {
        kind,
        minecraft,
        loader,
        installer_sha1,
        missing,
    }))
}

fn seed_parent(
    root: &Path,
    stage: &Path,
    minecraft: &str,
    platform: &Platform,
    cancel: &AtomicBool,
) -> Result<Value> {
    let parent = verify_vanilla_parent(root, minecraft, platform, cancel)?;
    let mut artifacts = library_artifacts(&parent, platform)?;
    artifacts.push(metadata_artifact(
        &parent["downloads"]["client"],
        format!("versions/{minecraft}/{minecraft}.jar").into(),
    )?);
    let index_id = parent["assetIndex"]["id"]
        .as_str()
        .context("资源索引缺少 ID")?;
    validate_id(index_id)?;
    let index = metadata_artifact(
        &parent["assetIndex"],
        format!("assets/indexes/{index_id}.json").into(),
    )?;
    let data: Value = serde_json::from_slice(&fs::read(safe_target(root, &index.relative_path)?)?)?;
    artifacts.push(index);
    if let Some(logging) = parent.pointer("/logging/client/file") {
        let name = logging["id"].as_str().context("日志配置缺少 ID")?;
        validate_id(name)?;
        artifacts.push(metadata_artifact(
            logging,
            PathBuf::from("assets/log_configs").join(name),
        )?);
    }
    for object in data["objects"]
        .as_object()
        .context("资源索引缺少 objects")?
        .values()
    {
        let hash = expected_hash(object["hash"].as_str())?.context("资源对象缺少 SHA1")?;
        artifacts.push(Artifact {
            relative_path: format!("assets/objects/{}/{hash}", &hash[..2]).into(),
            url: String::new(),
            sha1: Some(hash),
            size: Some(object["size"].as_u64().context("资源对象缺少大小")?),
            native: false,
            excludes: vec![],
        });
    }
    for artifact in artifacts {
        cancelled(cancel)?;
        let source = safe_target(root, &artifact.relative_path)?;
        ensure!(
            cache_valid(&source, &artifact, cancel)?,
            "父版本文件在准备期间发生变化"
        );
        // A separate copy prevents processor writes from modifying a hard-linked source.
        store_verified(stage, &artifact, &mut fs::File::open(source)?, cancel)?;
    }
    let relative = PathBuf::from(format!("versions/{minecraft}/{minecraft}.json"));
    let destination = safe_target(stage, &relative)?;
    fs::create_dir_all(destination.parent().unwrap())?;
    fs::write(destination, serde_json::to_vec(&parent)?)?;
    Ok(parent)
}

fn publish(
    root: &Path,
    stage: &Path,
    rebuilt_id: &str,
    plan: &Plan,
    saved: &[(PathBuf, Vec<u8>)],
    cancel: &AtomicBool,
) -> Result<()> {
    let rebuilt = crate::metadata::resolve_version(stage, rebuilt_id)?;
    ensure!(
        rebuilt["_pcl_forge_install"]["installerSha1"].as_str() == Some(&plan.installer_sha1),
        "官方安装器已变化，与原安装收据不一致，未恢复生成物"
    );
    for artifact in &plan.missing {
        ensure!(
            cache_valid(
                &safe_target(stage, &artifact.relative_path)?,
                artifact,
                cancel
            )?,
            "重建输出与原收据 SHA1/大小不同：{}；未覆盖原文件",
            artifact.relative_path.display()
        );
    }
    unchanged(saved)?;
    for artifact in &plan.missing {
        cancelled(cancel)?;
        unchanged(saved)?;
        store_verified(
            root,
            artifact,
            &mut fs::File::open(safe_target(stage, &artifact.relative_path)?)?,
            cancel,
        )?;
    }
    Ok(())
}

fn regenerate_optifine(
    root: &Path,
    version: &Value,
    java_path: Option<&Path>,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: &(impl Fn(Progress) + Sync),
    saved: &[(PathBuf, Vec<u8>)],
) -> Result<()> {
    let Some(receipt) = version.get("_pcl_optifine_install") else {
        return Ok(());
    };
    let mut missing = Vec::new();
    for artifact in library_artifacts(version, platform)? {
        if artifact.relative_path.starts_with("libraries/optifine") && artifact.url.is_empty() {
            expected_hash(artifact.sha1.as_deref())?.context("OptiFine 生成物缺少原安装 SHA1")?;
            artifact.size.context("OptiFine 生成物缺少大小")?;
            if !cache_valid(
                &safe_target(root, &artifact.relative_path)?,
                &artifact,
                cancel,
            )? {
                missing.push(artifact);
            }
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let minecraft = version["_pcl_jar_id"]
        .as_str()
        .context("OptiFine 缺少原版来源")?;
    validate_id(minecraft)?;
    let filename = receipt["filename"]
        .as_str()
        .context("OptiFine 安装收据缺少文件名")?;
    let installer_sha1 =
        expected_hash(receipt["sha1"].as_str())?.context("OptiFine 安装收据缺少安装器 SHA1")?;
    let entry = crate::loaders::optifine::list_versions(minecraft, cancel)?
        .into_iter()
        .find(|entry| entry.filename == filename)
        .context("原 OptiFine 安装器已不在发行方列表中，保留当前文件")?;
    super::repair_files(root, minecraft, platform, cancel, progress)?;
    let stage = tempfile::Builder::new()
        .prefix(".pcl-repair-optifine-")
        .tempdir_in(root)?;
    seed_parent(root, stage.path(), minecraft, platform, cancel)?;
    let runtime = if let Some(java) = java_path {
        java.to_owned()
    } else {
        java::discover_java_with_cancel(cancel)?
            .runtimes
            .into_iter()
            .filter(|runtime| runtime.architecture == platform.arch)
            .max_by_key(|runtime| runtime.major)
            .context("恢复 OptiFine 需要选择 Java")?
            .path
    };
    let id = crate::loaders::optifine::install_optifine(
        stage.path(),
        &entry,
        &runtime,
        platform,
        cancel,
        progress,
    )?;
    let rebuilt = crate::metadata::resolve_version(stage.path(), &id)?;
    ensure!(
        rebuilt
            .pointer("/_pcl_optifine_install/sha1")
            .and_then(Value::as_str)
            == Some(&installer_sha1),
        "OptiFine 发行文件与原安装收据不同，未覆盖生成物"
    );
    publish_receipted_files(root, stage.path(), &missing, saved, cancel)
}
fn publish_receipted_files(
    root: &Path,
    stage: &Path,
    files: &[Artifact],
    saved: &[(PathBuf, Vec<u8>)],
    cancel: &AtomicBool,
) -> Result<()> {
    for artifact in files {
        ensure!(
            cache_valid(
                &safe_target(stage, &artifact.relative_path)?,
                artifact,
                cancel
            )?,
            "重建文件与原收据不符：{}",
            artifact.relative_path.display()
        );
    }
    unchanged(saved)?;
    for artifact in files {
        cancelled(cancel)?;
        unchanged(saved)?;
        store_verified(
            root,
            artifact,
            &mut fs::File::open(safe_target(stage, &artifact.relative_path)?)?,
            cancel,
        )?;
    }
    Ok(())
}

pub(super) fn regenerate(
    root: &Path,
    id: &str,
    java_path: Option<&Path>,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: &(impl Fn(Progress) + Sync),
) -> Result<()> {
    cancelled(cancel)?;
    let saved = snapshots(root, id)?;
    let version = crate::metadata::resolve_version(root, id)?;
    regenerate_optifine(
        root, &version, java_path, platform, cancel, progress, &saved,
    )?;
    let Some(plan) = plan(root, &version, platform, cancel)? else {
        return Ok(());
    };
    ensure!(id != plan.minecraft, "加载器收据不能指向自身作为原版");
    let parent_path = safe_target(
        root,
        &PathBuf::from(format!("versions/{0}/{0}.json", plan.minecraft)),
    )?;
    let original_parent: Value = serde_json::from_slice(&fs::read(parent_path)?)?;
    ensure!(
        original_parent["id"].as_str() == Some(&plan.minecraft)
            && original_parent.get("inheritsFrom").is_none()
            && original_parent.get("_pcl_forge_install").is_none(),
        "处理器修复必须使用独立原版父版本，拒绝嵌套收据或继承循环"
    );
    progress(Progress {
        message: "准备在隔离目录重新生成加载器文件，保留原版本配置".into(),
        ..Default::default()
    });
    // Repair only the declared vanilla parent first. This also verifies every
    // input before copying it into the isolated installer root.
    super::repair_files(root, &plan.minecraft, platform, cancel, progress)?;
    let stage = tempfile::Builder::new()
        .prefix(".pcl-repair-generated-")
        .tempdir_in(root)?;
    let parent = seed_parent(root, stage.path(), &plan.minecraft, platform, cancel)?;
    let major = parent["javaVersion"]["majorVersion"].as_u64().unwrap_or(8) as u32;
    let runtime = if let Some(path) = java_path {
        let runtime = java::inspect_java_with_cancel(path, cancel)?;
        java::validate_for_version(&runtime, major, platform)?;
        runtime
    } else {
        java::discover_java_with_cancel(cancel)?
            .runtimes
            .into_iter()
            .find(|runtime| java::validate_for_version(runtime, major, platform).is_ok())
            .with_context(|| {
                format!("重新运行加载器处理器需要 Java {major}，请在启动设置中选择或下载兼容 Java")
            })?
    };
    let rebuilt = forge::install_forge(
        stage.path(),
        plan.kind,
        &plan.minecraft,
        &plan.loader,
        &runtime.path,
        platform,
        cancel,
        progress,
    )?;
    publish(root, stage.path(), &rebuilt, &plan, &saved, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn put(root: &Path, path: &str, bytes: &[u8]) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn generated(bytes: &[u8]) -> Artifact {
        Artifact {
            relative_path: "libraries/generated/client.jar".into(),
            url: String::new(),
            sha1: Some(format!("{:x}", Sha1::digest(bytes))),
            size: Some(bytes.len() as u64),
            native: false,
            excludes: vec![],
        }
    }
    #[test]
    fn generated_optifine_files_are_all_checked_before_any_publication() {
        let root = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir().unwrap();
        let json = br#"{"id":"custom"}"#;
        put(root.path(), "versions/custom/custom.json", json);
        let saved = snapshots(root.path(), "custom").unwrap();
        let first = generated(b"verified");
        let mut second = generated(b"other");
        second.relative_path = "libraries/generated/second.jar".into();
        put(root.path(), "libraries/generated/client.jar", b"user-old");
        put(stage.path(), "libraries/generated/client.jar", b"verified");
        put(stage.path(), "libraries/generated/second.jar", b"wrong");
        assert!(publish_receipted_files(
            root.path(),
            stage.path(),
            &[first.clone(), second.clone()],
            &saved,
            &AtomicBool::new(false)
        )
        .is_err());
        assert_eq!(
            fs::read(root.path().join("libraries/generated/client.jar")).unwrap(),
            b"user-old"
        );
        put(stage.path(), "libraries/generated/second.jar", b"other");
        publish_receipted_files(
            root.path(),
            stage.path(),
            &[first, second],
            &saved,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            fs::read(root.path().join("libraries/generated/client.jar")).unwrap(),
            b"verified"
        );
        assert_eq!(
            fs::read(root.path().join("versions/custom/custom.json")).unwrap(),
            json
        );
    }
    #[test]
    fn regenerated_outputs_require_original_receipt_before_replacing_any_file() {
        let root = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir().unwrap();
        let user = br#"{"id":"user","arguments":{"jvm":["-Dkeep.custom=true"]}}"#;
        put(root.path(), "versions/user/user.json", user);
        put(root.path(), "libraries/generated/client.jar", b"corrupt");
        put(root.path(), "versions/user/natives/user-file", b"preserve");
        let saved = snapshots(root.path(), "user").unwrap();
        let p = Plan {
            kind: ForgeKind::Forge,
            minecraft: "1.21.1".into(),
            loader: "52.0.16".into(),
            installer_sha1: "a".repeat(40),
            missing: vec![generated(b"verified")],
        };
        let rebuilt =
            json!({"id":"rebuilt","_pcl_forge_install":{"installerSha1":p.installer_sha1}});
        put(
            stage.path(),
            "versions/rebuilt/rebuilt.json",
            &serde_json::to_vec(&rebuilt).unwrap(),
        );
        put(
            stage.path(),
            "libraries/generated/client.jar",
            b"wrong bytes",
        );
        assert!(publish(
            root.path(),
            stage.path(),
            "rebuilt",
            &p,
            &saved,
            &AtomicBool::new(false)
        )
        .is_err());
        assert_eq!(
            fs::read(root.path().join("libraries/generated/client.jar")).unwrap(),
            b"corrupt"
        );
        put(stage.path(), "libraries/generated/client.jar", b"verified");
        publish(
            root.path(),
            stage.path(),
            "rebuilt",
            &p,
            &saved,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            fs::read(root.path().join("libraries/generated/client.jar")).unwrap(),
            b"verified"
        );
        assert_eq!(
            fs::read(root.path().join("versions/user/user.json")).unwrap(),
            user
        );
        assert_eq!(
            fs::read(root.path().join("versions/user/natives/user-file")).unwrap(),
            b"preserve"
        );
        let mut changed = rebuilt;
        changed["_pcl_forge_install"]["installerSha1"] = json!("b".repeat(40));
        put(
            stage.path(),
            "versions/rebuilt/rebuilt.json",
            &serde_json::to_vec(&changed).unwrap(),
        );
        assert!(publish(
            root.path(),
            stage.path(),
            "rebuilt",
            &p,
            &saved,
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .to_string()
        .contains("安装器已变化"));
        put(
            root.path(),
            "versions/user/user.json",
            br#"{"id":"user","user-edit":true}"#,
        );
        assert!(publish(
            root.path(),
            stage.path(),
            "rebuilt",
            &p,
            &saved,
            &AtomicBool::new(false)
        )
        .is_err());
    }
    #[test]
    fn receipt_preflight_rejects_escape_and_unknown_installer_before_execution() {
        let root = tempfile::tempdir().unwrap();
        let artifact = generated(b"verified");
        let mut value = json!({"libraries":[],"_pcl_forge_install":{"kind":"forge","minecraft":"1.21.1","loader":"52.0.16","installerSha1":"a".repeat(40),"files":[{"path":artifact.relative_path,"sha1":artifact.sha1,"size":artifact.size}]}});
        assert_eq!(
            plan(
                root.path(),
                &value,
                &Platform::current(),
                &AtomicBool::new(false)
            )
            .unwrap()
            .unwrap()
            .missing
            .len(),
            1
        );
        value["_pcl_forge_install"]["files"][0]["path"] = json!("../outside");
        assert!(plan(
            root.path(),
            &value,
            &Platform::current(),
            &AtomicBool::new(false)
        )
        .is_err());
        value["_pcl_forge_install"]["files"][0]["path"] = json!("libraries/generated/client.jar");
        value["_pcl_forge_install"]["installerSha1"] = Value::Null;
        assert!(plan(
            root.path(),
            &value,
            &Platform::current(),
            &AtomicBool::new(false)
        )
        .is_err());
    }
    #[test]
    fn parent_inputs_are_copied_not_hardlinked_into_processor_staging() {
        let root = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir().unwrap();
        let client = b"fixture client";
        let index = br#"{"objects":{}}"#;
        put(root.path(), "versions/1.21.1/1.21.1.jar", client);
        put(root.path(), "assets/indexes/test.json", index);
        let file = |bytes: &[u8], url: &str| json!({"url":url,"sha1":format!("{:x}",Sha1::digest(bytes)),"size":bytes.len()});
        let mut asset = file(index, "https://piston-meta.mojang.com/test.json");
        asset["id"] = json!("test");
        put(root.path(),"versions/1.21.1/1.21.1.json",&serde_json::to_vec(&json!({"id":"1.21.1","libraries":[],"downloads":{"client":file(client,"https://piston-data.mojang.com/client.jar")},"assetIndex":asset})).unwrap());
        seed_parent(
            root.path(),
            stage.path(),
            "1.21.1",
            &Platform::current(),
            &AtomicBool::new(false),
        )
        .unwrap();
        fs::write(
            stage.path().join("versions/1.21.1/1.21.1.jar"),
            b"processor changed staging",
        )
        .unwrap();
        assert_eq!(
            fs::read(root.path().join("versions/1.21.1/1.21.1.jar")).unwrap(),
            client
        );
    }
}
