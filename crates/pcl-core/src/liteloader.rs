//! LiteLoader's official versions.json and publisher Maven repository. The
//! lightweight LaunchWrapper profile is assembled declaratively; no installer
//! or command supplied by a modpack is executed.
use super::*;
use anyhow::ensure;
use md5::Md5;

const LIST: &str = "https://dl.liteloader.com/versions/versions.json";
#[derive(Clone, Debug)]
struct Entry {
    version: String,
    file: String,
    repository: String,
    snapshot: bool,
    md5: String,
    libraries: Vec<Value>,
}
fn entries(value: &Value, minecraft: &str) -> Result<Vec<Entry>> {
    validate_id(minecraft)?;
    let Some(game) = value["versions"].get(minecraft) else {
        return Ok(Vec::new());
    };
    let repository = game["repo"]["url"]
        .as_str()
        .context("LiteLoader 缺少仓库地址")?
        .replacen("http://", "https://", 1);
    let url = validate_url(&repository)?;
    ensure!(
        matches!(
            url.host_str(),
            Some("dl.liteloader.com" | "repo.mumfrey.com")
        ),
        "LiteLoader 仓库不是发行方地址"
    );
    let snapshot = game.get("artefacts").is_none();
    let branch = if snapshot {
        &game["snapshots"]
    } else {
        &game["artefacts"]
    };
    let entries = branch["com.mumfrey:liteloader"]
        .as_object()
        .context("LiteLoader 缺少版本映射")?;
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    // latest owns the snapshot selection; timestamped Maven URLs pin actual bytes.
    let mut values = entries.iter().collect::<Vec<_>>();
    values.sort_by_key(|(key, _)| *key != "latest");
    for (_, entry) in values {
        let version = entry["version"].as_str().context("LiteLoader 缺少版本")?;
        validate_id(version)?;
        if !seen.insert(version.to_owned()) {
            continue;
        }
        ensure!(
            version.starts_with(minecraft),
            "LiteLoader 与 Minecraft 版本不符"
        );
        let file = entry["file"].as_str().context("LiteLoader 缺少文件名")?;
        validate_id(file)?;
        ensure!(file.ends_with(".jar"), "LiteLoader 文件不是 JAR");
        ensure!(
            entry["tweakClass"] == "com.mumfrey.liteloader.launch.LiteLoaderTweaker",
            "未知 LiteLoader Tweaker"
        );
        let md5 = entry["md5"].as_str().context("LiteLoader 缺少 MD5")?;
        ensure!(
            md5.len() == 32 && md5.bytes().all(|b| b.is_ascii_hexdigit()),
            "LiteLoader MD5 无效"
        );
        result.push(Entry {
            version: version.into(),
            file: file.into(),
            repository: repository.clone(),
            snapshot,
            md5: md5.into(),
            libraries: entry["libraries"]
                .as_array()
                .context("LiteLoader 缺少支持库")?
                .clone(),
        });
    }
    Ok(result)
}
pub fn list_versions(minecraft: &str, cancel: &AtomicBool) -> Result<Vec<LoaderVersion>> {
    let entries = entries(&read_json(&http_client()?, LIST, cancel)?, minecraft)?;
    Ok(entries
        .into_iter()
        .map(|entry| LoaderVersion {
            version: entry.version,
            stable: !entry.snapshot,
        })
        .collect())
}
fn source_url(
    client: &Client,
    entry: &Entry,
    minecraft: &str,
    cancel: &AtomicBool,
) -> Result<(String, Option<String>)> {
    if !entry.snapshot {
        return Ok((
            format!(
                "{}com/mumfrey/liteloader/{minecraft}/{}",
                entry.repository, entry.file
            ),
            None,
        ));
    }
    let base = format!(
        "{}com/mumfrey/liteloader/{}",
        entry.repository, entry.version
    );
    let xml = request_bytes(
        client,
        &format!("{base}/maven-metadata.xml"),
        None,
        None,
        cancel,
    )?;
    let xml = std::str::from_utf8(&xml)?;
    let read = |tag: &str| -> Result<String> {
        let pattern = regex::Regex::new(&format!("<{tag}>([^<>]+)</{tag}>"))?;
        let value = pattern
            .captures(xml)
            .context("LiteLoader Snapshot 元数据缺少时间戳或构建号")?[1]
            .to_owned();
        validate_id(&value)?;
        Ok(value)
    };
    let timestamp = read("timestamp")?;
    let build = read("buildNumber")?;
    ensure!(
        timestamp.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            && build.bytes().all(|b| b.is_ascii_digit()),
        "LiteLoader Snapshot 版本格式无效"
    );
    let url = format!("{base}/liteloader-{minecraft}-{timestamp}-{build}-release.jar");
    let sha1 = parse_checksum(&request_bytes(
        client,
        &format!("{url}.sha1"),
        None,
        None,
        cancel,
    )?)?;
    Ok((url, Some(sha1)))
}
fn publish(root: &Path, a: &Artifact, bytes: &[u8], cancel: &AtomicBool) -> Result<()> {
    let path = safe_target(root, &a.relative_path)?;
    if path.exists() {
        ensure!(
            install::cache_valid(&path, a, cancel)?,
            "既有支持库与 LiteLoader 冲突，未覆盖"
        );
        return Ok(());
    }
    fs::create_dir_all(path.parent().context("支持库路径缺少父目录")?)?;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    cancelled(cancel)?;
    safe_target(root, &a.relative_path)?;
    temp.persist_noclobber(path)
        .map_err(|e| e.error)
        .context("LiteLoader 支持库提交冲突，未覆盖")?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub fn install_liteloader(
    root: &Path,
    minecraft: &str,
    version: &str,
    parent: Option<&str>,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: impl Fn(Progress) + Sync,
) -> Result<String> {
    ensure!(root.is_absolute(), "Minecraft 根目录必须为绝对路径");
    validate_id(minecraft)?;
    validate_id(version)?;
    let parent = parent.unwrap_or(minecraft);
    validate_id(parent)?;
    let id = format!("liteloader-{version}-{parent}");
    validate_id(&id)?;
    let existing = safe_target(root, &PathBuf::from(format!("versions/{id}/{id}.json")))?;
    if existing.exists() {
        let resolved = resolve_version(root, &id)?;
        ensure!(
            resolved["_pcl_jar_id"].as_str() == Some(minecraft)
                && resolved["libraries"].as_array().is_some_and(|libs| libs
                    .iter()
                    .any(|l| l["name"] == format!("com.mumfrey:liteloader:{version}"))),
            "既有 LiteLoader 配置不符，未覆盖"
        );
        for artifact in library_artifacts(&resolved, platform)? {
            verify_artifact(root, &artifact, cancel)?;
        }
        return Ok(id);
    }
    let client = http_client()?;
    let entry = entries(&read_json(&client, LIST, cancel)?, minecraft)?
        .into_iter()
        .find(|entry| entry.version == version)
        .context("官方清单未列出指定 LiteLoader 版本")?;
    if !safe_target(
        root,
        &PathBuf::from(format!("versions/{minecraft}/{minecraft}.json")),
    )?
    .exists()
    {
        install::install_version(root, minecraft, platform, cancel, &progress)?;
    }
    install::verify_vanilla_parent(root, minecraft, platform, cancel)?;
    let resolved = resolve_version(root, parent)?;
    ensure!(
        resolved["_pcl_jar_id"].as_str() == Some(minecraft),
        "LiteLoader 父版本不是所选 Minecraft"
    );
    if parent != minecraft {
        ensure!(
            resolved["libraries"]
                .as_array()
                .is_some_and(|libs| libs.iter().any(|l| l["name"]
                    .as_str()
                    .is_some_and(|n| n.starts_with("net.minecraftforge:forge:")
                        || n.starts_with("optifine:OptiFine:")))),
            "LiteLoader 仅支持原版、Forge 或旧版 OptiFine 父版本"
        );
    }
    progress(Progress {
        message: format!("下载 LiteLoader {version}"),
        completed: 0,
        total: 0,
        ..Default::default()
    });
    let (url, official_sha1) = source_url(&client, &entry, minecraft, cancel)?;
    let bytes = request_bytes(&client, &url, official_sha1.as_deref(), None, cancel)?;
    if !entry.snapshot {
        ensure!(
            format!("{:x}", Md5::digest(&bytes)).eq_ignore_ascii_case(&entry.md5),
            "LiteLoader 官方 MD5 不匹配"
        );
    }
    ensure!(bytes.starts_with(b"PK"), "LiteLoader 下载内容不是 JAR");
    let path = format!("com/mumfrey/liteloader/{version}/liteloader-{version}.jar");
    let sha1 = format!("{:x}", Sha1::digest(&bytes));
    let a = Artifact {
        relative_path: PathBuf::from("libraries").join(&path),
        url: url.clone(),
        sha1: Some(sha1.clone()),
        size: Some(bytes.len() as u64),
        native: false,
        excludes: vec![],
    };
    publish(root, &a, &bytes, cancel)?;
    let mut libraries = Vec::new();
    for mut library in entry.libraries {
        if let Some(url) = library["url"].as_str() {
            library["url"] = url.replacen("http://", "https://", 1).into();
        }
        let mut list = library_artifacts(&json!({"libraries":[library.clone()]}), platform)?;
        ensure!(list.len() == 1, "LiteLoader 支持库必须为单个 JAR");
        let mut artifact = list.remove(0);
        validate_url(&artifact.url)?;
        artifact.sha1 = Some(parse_checksum(&request_bytes(
            &client,
            &format!("{}.sha1", artifact.url),
            None,
            None,
            cancel,
        )?)?);
        let data = request_bytes(
            &client,
            &artifact.url,
            artifact.sha1.as_deref(),
            None,
            cancel,
        )?;
        artifact.size = Some(data.len() as u64);
        publish(root, &artifact, &data, cancel)?;
        let relative = artifact
            .relative_path
            .strip_prefix("libraries")?
            .to_string_lossy()
            .replace('\\', "/");
        library["downloads"] = json!({"artifact":{"path":relative,"url":artifact.url,"sha1":artifact.sha1,"size":artifact.size}});
        libraries.push(library);
    }
    libraries.push(json!({"name":format!("com.mumfrey:liteloader:{version}"),"downloads":{"artifact":{"path":path,"url":url,"sha1":sha1,"size":bytes.len()}}}));
    let profile = json!({"id":id,"inheritsFrom":parent,"type":"release","mainClass":"net.minecraft.launchwrapper.Launch","minecraftArguments":format!("{} --tweakClass com.mumfrey.liteloader.launch.LiteLoaderTweaker",resolved["minecraftArguments"].as_str().context("旧版 LiteLoader 父版本缺少 Minecraft 参数")?),"libraries":libraries,"_pcl_liteloader":{"minecraft":minecraft,"version":version,"publisherDigest":if entry.snapshot{"maven-sha1"}else{"manifest-md5"}}});
    commit_profile(
        root,
        &id,
        &profile,
        &library_artifacts(&resolved, platform)?,
        cancel,
    )?;
    progress(Progress {
        message: format!("LiteLoader {version} 安装完成"),
        completed: 1,
        total: 1,
        ..Default::default()
    });
    Ok(id)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_versions_and_publish_boundaries() {
        let value = json!({"versions":{"1.7.10":{"repo":{"url":"http://dl.liteloader.com/versions/"},"artefacts":{"com.mumfrey:liteloader":{"latest":{"version":"1.7.10_04","file":"liteloader.jar","md5":"a".repeat(32),"tweakClass":"com.mumfrey.liteloader.launch.LiteLoaderTweaker","libraries":[]}}}}}});
        let rows = entries(&value, "1.7.10").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].version, "1.7.10_04");
        assert!(entries(&value, "1.21.1").unwrap().is_empty());
        let dir = tempfile::tempdir().unwrap();
        let a = Artifact {
            relative_path: "libraries/a.jar".into(),
            url: String::new(),
            sha1: Some(format!("{:x}", Sha1::digest(b"verified"))),
            size: Some(8),
            native: false,
            excludes: vec![],
        };
        publish(dir.path(), &a, b"verified", &AtomicBool::new(false)).unwrap();
        fs::write(dir.path().join("libraries/a.jar"), b"user").unwrap();
        assert!(publish(dir.path(), &a, b"verified", &AtomicBool::new(false)).is_err());
        assert_eq!(
            fs::read(dir.path().join("libraries/a.jar")).unwrap(),
            b"user"
        );
    }
}
