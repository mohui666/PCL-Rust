//! Verified vanilla installation. The version JSON is committed only after all files are ready.
use crate::{
    metadata::{library_artifacts, safe_relative, validate_id},
    model::{Artifact, Platform, Progress, ProgressStage},
    transfer::{with_transfer, TrackedReader, TransferTracker},
};
use anyhow::{bail, Context, Result};
use reqwest::{blocking::Client, redirect::Policy, Url};
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
};

pub const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const BUFFER_SIZE: usize = 64 * 1024;
const MAX_METADATA_SIZE: u64 = 32 * 1024 * 1024;
const MAX_ASSET_DOWNLOADS: usize = 8;

fn asset_worker_count(files: usize) -> usize {
    files.min(MAX_ASSET_DOWNLOADS)
}

pub(crate) fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(crate::model::OperationCancelled.into());
    }
    Ok(())
}

fn approved_url(url: &Url) -> bool {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some_and(|p| p != 443)
    {
        return false;
    }
    match url.host_str() {
        Some(
            "piston-meta.mojang.com"
            | "piston-data.mojang.com"
            | "launchermeta.mojang.com"
            | "launcher.mojang.com"
            | "libraries.minecraft.net"
            | "resources.download.minecraft.net"
            | "meta.fabricmc.net"
            | "maven.fabricmc.net"
            | "maven.minecraftforge.net"
            | "meta.quiltmc.org",
        ) => true,
        Some("maven.quiltmc.org") => url.path().starts_with("/repository/release/"),
        Some("maven.neoforged.net") => url.path().starts_with("/releases/"),
        Some("s3.amazonaws.com") => url.path().starts_with("/Minecraft.Download/"),
        _ => false,
    }
}

pub(crate) fn validate_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| anyhow::anyhow!("下载地址格式无效"))?;
    if !approved_url(&url) {
        bail!("下载地址不是允许的发行方官方 HTTPS 地址");
    }
    Ok(url)
}

pub(crate) fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent("PCL-Rust/0.1")
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("官方下载重定向次数超限")
            } else if !approved_url(attempt.url()) {
                attempt.error("拒绝非官方 HTTPS 重定向")
            } else {
                attempt.follow()
            }
        }))
        .build()?)
}

pub(crate) fn expected_hash(value: Option<&str>) -> Result<Option<String>> {
    match value {
        Some(value) if value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()) => {
            Ok(Some(value.to_ascii_lowercase()))
        }
        Some(_) => bail!("元数据中的 SHA1 格式无效"),
        None => Ok(None),
    }
}

pub(crate) fn request_bytes(
    client: &Client,
    url: &str,
    sha1: Option<&str>,
    size: Option<u64>,
    cancel: &AtomicBool,
) -> Result<Vec<u8>> {
    request_bytes_tracked(client, url, sha1, size, cancel, None)
}

fn request_bytes_tracked(
    client: &Client,
    url: &str,
    sha1: Option<&str>,
    size: Option<u64>,
    cancel: &AtomicBool,
    tracker: Option<&TransferTracker<'_>>,
) -> Result<Vec<u8>> {
    cancelled(cancel)?;
    let url = validate_url(url)?;
    let hash = expected_hash(sha1)?;
    if size.is_some_and(|size| size > MAX_METADATA_SIZE) {
        bail!("元数据大小超过限制");
    }
    let active = tracker.map(TransferTracker::begin_download);
    let response = client.get(url).send()?.error_for_status()?;
    if !approved_url(response.url()) {
        bail!("拒绝非官方下载响应");
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_METADATA_SIZE)
    {
        bail!("元数据大小超过限制");
    }
    let mut response = TrackedReader {
        source: response,
        tracker,
        active,
    };
    let mut bytes = Vec::new();
    let mut buf = [0; BUFFER_SIZE];
    loop {
        cancelled(cancel)?;
        let count = response.read(&mut buf)?;
        if count == 0 {
            break;
        }
        if bytes.len() as u64 + count as u64 > MAX_METADATA_SIZE {
            bail!("元数据大小超过限制");
        }
        bytes.extend_from_slice(&buf[..count]);
    }
    cancelled(cancel)?;
    if size.is_some_and(|size| size != bytes.len() as u64) {
        bail!("元数据大小不匹配");
    }
    if hash.is_some_and(|hash| hash != format!("{:x}", Sha1::digest(&bytes))) {
        bail!("元数据 SHA1 不匹配");
    }
    Ok(bytes)
}

pub fn fetch_manifest() -> Result<Value> {
    fetch_manifest_with_cancel(&AtomicBool::new(false))
}

pub fn fetch_manifest_with_cancel(cancel: &AtomicBool) -> Result<Value> {
    let client = http_client()?;
    let bytes = request_bytes(&client, MANIFEST_URL, None, None, cancel)
        .context("无法获取 Mojang 版本清单")?;
    let manifest: Value = serde_json::from_slice(&bytes).context("Mojang 版本清单格式无效")?;
    if !manifest["versions"].is_array() {
        bail!("Mojang 版本清单缺少 versions");
    }
    Ok(manifest)
}

// Reject metadata traversal and existing symlinks below the selected data directory.
pub(crate) fn safe_target(root: &Path, relative: &Path) -> Result<PathBuf> {
    // PathBuf uses backslashes on Windows, whereas metadata uses forward slashes.
    // Validate actual components rather than treating a native PathBuf as raw metadata.
    let mut pieces = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(piece) = component else {
            bail!("目标文件路径不是安全的相对路径");
        };
        pieces.push(piece.to_str().context("文件路径不是 UTF-8")?);
    }
    let relative = safe_relative(&pieces.join("/"))?;
    let mut target = root.to_owned();
    for part in relative.components() {
        target.push(part);
        match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("目标路径含符号链接：{}", relative.display())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(target)
}

pub(crate) fn cache_valid(path: &Path, artifact: &Artifact, cancel: &AtomicBool) -> Result<bool> {
    cancelled(cancel)?;
    let hash = expected_hash(artifact.sha1.as_deref())?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        bail!("缓存路径是符号链接：{}", artifact.relative_path.display());
    }
    if !metadata.is_file() {
        bail!("缓存路径不是文件：{}", artifact.relative_path.display());
    }
    if artifact.size.is_some_and(|size| size != metadata.len()) {
        return Ok(false);
    }
    // A length alone cannot establish cache integrity. Fetch again if no digest was published.
    let Some(expected) = hash else {
        return Ok(false);
    };
    let mut source = File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buf = [0; BUFFER_SIZE];
    loop {
        cancelled(cancel)?;
        let count = source.read(&mut buf)?;
        if count == 0 {
            break;
        }
        hasher.update(&buf[..count]);
    }
    cancelled(cancel)?;
    Ok(format!("{:x}", hasher.finalize()) == expected)
}

pub(crate) fn download_artifact(
    client: &Client,
    root: &Path,
    artifact: &Artifact,
    cancel: &AtomicBool,
) -> Result<()> {
    download_artifact_tracked(client, root, artifact, cancel, None)
}

fn download_artifact_tracked(
    client: &Client,
    root: &Path,
    artifact: &Artifact,
    cancel: &AtomicBool,
    tracker: Option<&TransferTracker<'_>>,
) -> Result<()> {
    let target = safe_target(root, &artifact.relative_path)?;
    let url = validate_url(&artifact.url)?;
    if cache_valid(&target, artifact, cancel)? {
        return Ok(());
    }
    cancelled(cancel)?;
    let active = tracker.map(TransferTracker::begin_download);
    let response = client.get(url).send()?.error_for_status()?;
    if !approved_url(response.url()) {
        bail!("拒绝非官方下载响应");
    }
    if let (Some(actual), Some(expected)) = (response.content_length(), artifact.size) {
        if actual != expected {
            bail!("下载大小不匹配（预期 {expected}，响应 {actual}）");
        }
    }
    let mut response = TrackedReader {
        source: response,
        tracker,
        active,
    };
    store_verified(root, artifact, &mut response, cancel)
}

fn store_verified(
    root: &Path,
    artifact: &Artifact,
    source: &mut impl Read,
    cancel: &AtomicBool,
) -> Result<()> {
    cancelled(cancel)?;
    let expected = expected_hash(artifact.sha1.as_deref())?;
    let target = safe_target(root, &artifact.relative_path)?;
    let parent = target.parent().context("下载路径缺少父目录")?;
    fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    let mut hasher = Sha1::new();
    let mut written = 0u64;
    let mut buf = [0; BUFFER_SIZE];
    loop {
        cancelled(cancel)?;
        let count = source.read(&mut buf)?;
        if count == 0 {
            break;
        }
        written += count as u64;
        if artifact.size.is_some_and(|size| written > size) {
            bail!("下载超过预期文件大小");
        }
        staged.write_all(&buf[..count])?;
        hasher.update(&buf[..count]);
    }
    cancelled(cancel)?;
    if artifact.size.is_some_and(|size| written != size) {
        bail!("下载文件不完整");
    }
    if expected.is_some_and(|hash| hash != format!("{:x}", hasher.finalize())) {
        bail!("下载 SHA1 校验失败");
    }
    staged.as_file().sync_all()?;
    // Persist only verified complete bytes; an existing file survives every preceding failure.
    let _ = safe_target(root, &artifact.relative_path)?;
    staged.persist(&target).map_err(|error| error.error)?;
    Ok(())
}

fn metadata_artifact(value: &Value, relative_path: PathBuf) -> Result<Artifact> {
    let url = value["url"]
        .as_str()
        .context("下载元数据缺少 URL")?
        .to_owned();
    validate_url(&url)?;
    let sha1 = Some(expected_hash(value["sha1"].as_str())?.context("官方下载元数据缺少 SHA1")?);
    let size = Some(
        value["size"]
            .as_u64()
            .context("官方下载元数据缺少文件大小")?,
    );
    Ok(Artifact {
        relative_path,
        url,
        sha1,
        size,
        native: false,
        excludes: vec![],
    })
}

fn download_assets(
    client: &Client,
    root: &Path,
    assets: &[&Artifact],
    cancel: &AtomicBool,
    completed: &AtomicU64,
    tracker: &TransferTracker<'_>,
) -> Result<()> {
    tracker.set_concurrency_limit(Some(asset_worker_count(assets.len()) as u32));
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let first_error = Mutex::new(None);
    std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..asset_worker_count(assets.len()) {
            let next = &next;
            let failed = &failed;
            let first_error = &first_error;
            workers.push(scope.spawn(move || loop {
                if failed.load(Ordering::Acquire) {
                    break;
                }
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(artifact) = assets.get(index) else {
                    break;
                };
                if failed.load(Ordering::Acquire) {
                    break;
                }
                tracker.message(format!("校验或下载 {}", artifact.relative_path.display()));
                match download_artifact_tracked(client, root, artifact, cancel, Some(tracker))
                    .with_context(|| format!("下载失败：{}", artifact.relative_path.display()))
                {
                    Ok(()) => {
                        completed.fetch_add(1, Ordering::Relaxed);
                        tracker.finished_item(true, true);
                    }
                    Err(error) => {
                        failed.store(true, Ordering::Release);
                        if let Ok(mut first) = first_error.lock() {
                            if first.is_none() {
                                *first = Some(error);
                            }
                        }
                        break;
                    }
                }
            }));
        }
        for worker in workers {
            if worker.join().is_err() {
                failed.store(true, Ordering::Release);
                let mut first = first_error
                    .lock()
                    .map_err(|_| anyhow::anyhow!("资源下载工作线程状态异常"))?;
                if first.is_none() {
                    *first = Some(anyhow::anyhow!("资源下载工作线程异常退出"));
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    })?;
    if let Some(error) = first_error
        .into_inner()
        .map_err(|_| anyhow::anyhow!("资源下载工作线程状态异常"))?
    {
        return Err(error);
    }
    cancelled(cancel)
}

pub(crate) fn extract_natives(
    archive_path: &Path,
    destination: &Path,
    excludes: &[String],
    cancel: &AtomicBool,
) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(archive_path)?)?;
    if archive.len() > 10_000 {
        bail!("原生库压缩包条目数量超限");
    }
    let mut extracted = 0u64;
    for index in 0..archive.len() {
        cancelled(cancel)?;
        let mut file = archive.by_index(index)?;
        let name = file.name().to_owned();
        // Validate before applying exclusions, including Windows separators on Unix.
        if name.contains('\\') || file.enclosed_name().is_none() {
            bail!("原生库 ZIP 存在不安全路径");
        }
        let relative = safe_relative(name.trim_end_matches('/')).context("原生库 ZIP 路径无效")?;
        if file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            bail!("原生库 ZIP 包含符号链接");
        }
        if name.starts_with("META-INF/") || excludes.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }
        if file.size() > 512 * 1024 * 1024
            || extracted.saturating_add(file.size()) > 1024 * 1024 * 1024
        {
            bail!("原生库解压大小超限");
        }
        let target = safe_target(destination, &relative)?;
        if file.is_dir() {
            fs::create_dir_all(target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = File::create(&target)?;
        let mut buf = [0; BUFFER_SIZE];
        let mut count_for_file = 0u64;
        loop {
            cancelled(cancel)?;
            let count = file.read(&mut buf)?;
            if count == 0 {
                break;
            }
            count_for_file += count as u64;
            extracted += count as u64;
            if count_for_file > file.size() || extracted > 1024 * 1024 * 1024 {
                bail!("原生库解压大小超限");
            }
            output.write_all(&buf[..count])?;
        }
        if count_for_file != file.size() {
            bail!("原生库 ZIP 文件不完整");
        }
    }
    cancelled(cancel)
}

fn copy_asset(
    root: &Path,
    artifact: &Artifact,
    destination: PathBuf,
    cancel: &AtomicBool,
) -> Result<()> {
    let source = safe_target(root, &artifact.relative_path)?;
    let target = safe_target(root, &destination)?;
    let mapped = Artifact {
        relative_path: destination,
        ..artifact.clone()
    };
    if cache_valid(&target, &mapped, cancel)? {
        return Ok(());
    }
    let parent = target.parent().context("资源路径缺少父目录")?;
    fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    let mut source = File::open(source)?;
    let mut hasher = Sha1::new();
    let mut size = 0u64;
    let mut buf = [0; BUFFER_SIZE];
    loop {
        cancelled(cancel)?;
        let count = source.read(&mut buf)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        staged.write_all(&buf[..count])?;
        hasher.update(&buf[..count]);
    }
    if artifact.size.is_some_and(|expected| expected != size)
        || artifact
            .sha1
            .as_ref()
            .is_some_and(|expected| expected != &format!("{:x}", hasher.finalize()))
    {
        bail!("复制历史版本资源时校验失败");
    }
    cancelled(cancel)?;
    staged.as_file().sync_all()?;
    staged.persist(target).map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn commit_native_directory(staged: &Path, target: &Path, backup: &Path) -> Result<()> {
    let existing = target.exists();
    if existing {
        fs::rename(target, backup).context("无法暂存已有原生库目录")?;
    }
    if let Err(error) = fs::rename(staged, target) {
        if existing {
            fs::rename(backup, target).context("原生库更新失败，且无法恢复已有目录")?;
        }
        return Err(error).context("无法提交原生库目录");
    }
    Ok(())
}

pub fn install_version(
    root: &Path,
    id: &str,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: impl Fn(Progress) + Sync,
) -> Result<()> {
    with_transfer(
        &progress,
        vanilla_plan(),
        "读取 Mojang 版本清单",
        |tracker| install_version_tracked(root, id, platform, cancel, tracker, None),
    )
}

fn vanilla_plan() -> Vec<ProgressStage> {
    vec![
        ProgressStage::VersionMetadata,
        ProgressStage::AssetIndex,
        ProgressStage::CoreLibraries,
        ProgressStage::AssetFiles,
        ProgressStage::NativeLibraries,
        ProgressStage::VersionCommit,
    ]
}

fn install_version_tracked(
    root: &Path,
    id: &str,
    platform: &Platform,
    cancel: &AtomicBool,
    tracker: &TransferTracker<'_>,
    alias: Option<&str>,
) -> Result<()> {
    validate_id(id)?;
    cancelled(cancel)?;
    fs::create_dir_all(root).context("无法创建 Minecraft 数据目录")?;
    let client = http_client()?;
    tracker.begin_stage(
        ProgressStage::VersionMetadata,
        "读取 Mojang 版本清单",
        0,
        Some(1),
    );
    let manifest: Value = serde_json::from_slice(&request_bytes_tracked(
        &client,
        MANIFEST_URL,
        None,
        None,
        cancel,
        Some(tracker),
    )?)?;
    let entry = manifest["versions"]
        .as_array()
        .context("版本清单格式无效")?
        .iter()
        .find(|entry| entry["id"].as_str() == Some(id))
        .context("Mojang 清单中不存在所选版本")?;
    let metadata_url = entry["url"].as_str().context("版本条目缺少元数据 URL")?;
    let metadata_hash = entry["sha1"].as_str().context("版本条目缺少元数据 SHA1")?;
    tracker.message(format!("读取版本元数据 {id}"));
    let version_bytes = request_bytes_tracked(
        &client,
        metadata_url,
        Some(metadata_hash),
        None,
        cancel,
        Some(tracker),
    )
    .with_context(|| format!("读取版本元数据失败：{id}"))?;
    let version: Value = serde_json::from_slice(&version_bytes).context("版本 JSON 格式无效")?;
    if version["id"].as_str() != Some(id) {
        bail!("下载的版本元数据 ID 不匹配");
    }
    if version.get("inheritsFrom").is_some() {
        bail!("官方安装器暂不支持继承版本；请导入已经安装的实例");
    }
    tracker.finish_stage();
    tracker.begin_stage(ProgressStage::AssetIndex, "读取资源索引", 0, Some(1));
    let mut artifacts = library_artifacts(&version, platform)?;
    artifacts.push(metadata_artifact(
        &version["downloads"]["client"],
        PathBuf::from(format!("versions/{id}/{id}.jar")),
    )?);
    if let Some(logging) = version.pointer("/logging/client/file") {
        let log_id = logging["id"].as_str().context("日志配置缺少 ID")?;
        validate_id(log_id)?;
        artifacts.push(metadata_artifact(
            logging,
            PathBuf::from("assets/log_configs").join(log_id),
        )?);
    }
    let index_id = version
        .pointer("/assetIndex/id")
        .and_then(Value::as_str)
        .context("版本元数据缺少资源索引 ID")?;
    validate_id(index_id)?;
    let index_download = metadata_artifact(
        &version["assetIndex"],
        PathBuf::from(format!("assets/indexes/{index_id}.json")),
    )?;
    let index_bytes = request_bytes_tracked(
        &client,
        &index_download.url,
        index_download.sha1.as_deref(),
        index_download.size,
        cancel,
        Some(tracker),
    )
    .with_context(|| format!("读取资源索引失败：{index_id}"))?;
    let index: Value = serde_json::from_slice(&index_bytes).context("资源索引 JSON 格式无效")?;
    let objects = index["objects"]
        .as_object()
        .context("资源索引缺少 objects")?;
    let mut mapped_assets = Vec::new();
    let virtual_assets = index["virtual"].as_bool().unwrap_or(false);
    let resources = index["map_to_resources"].as_bool().unwrap_or(false);
    let mapped_resource_dir = if resources {
        let game_dir = crate::config::instance_game_dir(root, id)?;
        let canonical_root = root.canonicalize()?;
        Some(
            game_dir
                .strip_prefix(&canonical_root)
                .or_else(|_| game_dir.strip_prefix(root))
                .context("版本数据目录不属于当前 Minecraft 目录")?
                .join("resources"),
        )
    } else {
        None
    };
    let mut hashes = HashSet::new();
    for (name, object) in objects {
        let logical = safe_relative(name).context("资源索引中存在不安全的资源路径")?;
        let hash = expected_hash(object["hash"].as_str())?.context("资源条目缺少 SHA1")?;
        let size = object["size"].as_u64().context("资源条目缺少大小")?;
        let asset = Artifact {
            relative_path: PathBuf::from(format!("assets/objects/{}/{hash}", &hash[..2])),
            url: format!(
                "https://resources.download.minecraft.net/{}/{hash}",
                &hash[..2]
            ),
            sha1: Some(hash.clone()),
            size: Some(size),
            native: false,
            excludes: vec![],
        };
        if hashes.insert(hash) {
            artifacts.push(asset.clone());
        }
        if virtual_assets {
            mapped_assets.push((
                asset.clone(),
                PathBuf::from("assets/virtual")
                    .join(index_id)
                    .join(&logical),
            ));
        }
        if let Some(directory) = &mapped_resource_dir {
            mapped_assets.push((asset, directory.join(&logical)));
        }
    }
    // Validate the complete plan before making any download writes.
    let mut paths = HashMap::new();
    for artifact in &artifacts {
        safe_target(root, &artifact.relative_path)?;
        validate_url(&artifact.url)?;
        expected_hash(artifact.sha1.as_deref())?
            .with_context(|| format!("官方下载缺少 SHA1：{}", artifact.relative_path.display()))?;
        artifact.size.with_context(|| {
            format!("官方下载缺少文件大小：{}", artifact.relative_path.display())
        })?;
        if let Some(previous) = paths.insert(
            artifact.relative_path.clone(),
            (&artifact.url, &artifact.sha1, artifact.size),
        ) {
            if previous != (&artifact.url, &artifact.sha1, artifact.size) {
                bail!(
                    "下载清单中存在冲突路径：{}",
                    artifact.relative_path.display()
                );
            }
        }
    }
    for (_, destination) in &mapped_assets {
        safe_target(root, destination)?;
    }
    tracker.finish_stage();
    let total = (artifacts.len() + mapped_assets.len() + 2) as u64;
    tracker.plan_files(total, artifacts.len() as u64);
    let mut completed = 0u64;
    let asset_prefix = PathBuf::from("assets").join("objects");
    let core_count = artifacts
        .iter()
        .filter(|a| !a.relative_path.starts_with(&asset_prefix))
        .count() as u64;
    tracker.begin_stage(
        ProgressStage::CoreLibraries,
        "校验或下载核心与支持库",
        core_count,
        Some(1),
    );
    for artifact in artifacts
        .iter()
        .filter(|artifact| !artifact.relative_path.starts_with(&asset_prefix))
    {
        tracker.message(format!("校验或下载 {}", artifact.relative_path.display()));
        download_artifact_tracked(&client, root, artifact, cancel, Some(tracker))
            .with_context(|| format!("下载失败：{}", artifact.relative_path.display()))?;
        completed += 1;
        tracker.finished_item(true, true);
    }
    tracker.finish_stage();
    let asset_downloads: Vec<_> = artifacts
        .iter()
        .filter(|artifact| artifact.relative_path.starts_with(&asset_prefix))
        .collect();
    tracker.begin_stage(
        ProgressStage::AssetFiles,
        "校验或下载资源文件",
        (asset_downloads.len() + mapped_assets.len()) as u64,
        Some(asset_worker_count(asset_downloads.len()) as u32),
    );
    let concurrent_completed = AtomicU64::new(completed);
    download_assets(
        &client,
        root,
        &asset_downloads,
        cancel,
        &concurrent_completed,
        tracker,
    )?;
    completed = concurrent_completed.load(Ordering::Relaxed);
    // Every network worker has joined; the remaining mappings are local copies.
    tracker.set_concurrency_limit(Some(0));
    for (asset, destination) in mapped_assets {
        tracker.message(format!("准备历史版本资源 {}", destination.display()));
        copy_asset(root, &asset, destination, cancel)?;
        completed += 1;
        tracker.finished_item(true, false);
    }
    tracker.finish_stage();
    tracker.begin_stage(
        ProgressStage::NativeLibraries,
        "整理原生库",
        artifacts.iter().filter(|a| a.native).count() as u64 + 1,
        Some(0),
    );
    let version_dir = safe_target(root, &PathBuf::from("versions").join(id))?;
    fs::create_dir_all(&version_dir)?;
    let native_staging = tempfile::tempdir_in(&version_dir)?;
    let staged_natives = native_staging.path().join("natives");
    fs::create_dir(&staged_natives)?;
    tracker.finished_item(false, false);
    for artifact in artifacts.iter().filter(|artifact| artifact.native) {
        tracker.message(format!("解压原生库 {}", artifact.relative_path.display()));
        extract_natives(
            &safe_target(root, &artifact.relative_path)?,
            &staged_natives,
            &artifact.excludes,
            cancel,
        )
        .with_context(|| format!("原生库解压失败：{}", artifact.relative_path.display()))?;
        tracker.finished_item(false, false);
    }
    tracker.finish_stage();
    tracker.begin_stage(
        ProgressStage::VersionCommit,
        "写入版本与资源索引",
        if alias.is_some() { 5 } else { 3 },
        Some(0),
    );
    // Keep both JSON files in temporary files until every file and native library is ready.
    let index_target = safe_target(root, &index_download.relative_path)?;
    fs::create_dir_all(index_target.parent().context("资源索引路径缺少父目录")?)?;
    let mut staged_index = tempfile::NamedTempFile::new_in(index_target.parent().unwrap())?;
    staged_index.write_all(&index_bytes)?;
    staged_index.as_file().sync_all()?;
    let version_relative = PathBuf::from(format!("versions/{id}/{id}.json"));
    let version_target = safe_target(root, &version_relative)?;
    let mut staged_version = tempfile::NamedTempFile::new_in(&version_dir)?;
    staged_version.write_all(&version_bytes)?;
    staged_version.as_file().sync_all()?;
    cancelled(cancel)?;
    let natives_target = safe_target(root, &PathBuf::from("versions").join(id).join("natives"))?;
    commit_native_directory(
        &staged_natives,
        &natives_target,
        &native_staging.path().join("previous-natives"),
    )?;
    tracker.finished_item(false, false);
    staged_index
        .persist(index_target)
        .map_err(|error| error.error)?;
    completed += 1;
    tracker.finished_item(true, false);
    staged_version
        .persist(version_target)
        .map_err(|error| error.error)?;
    completed += 1;
    tracker.finished_item(true, false);
    if let Some(alias) = alias {
        tracker.message("校验已安装的原版，准备登记实例");
        verify_vanilla_parent(root, id, platform, cancel)?;
        tracker.finished_item(false, false);
        tracker.message(format!("登记实例 {alias}"));
        register_instance_id(root, id, alias, platform, cancel)?;
        tracker.finished_item(false, false);
    }
    debug_assert_eq!(completed, total);
    tracker.message(format!("{id} 安装完成，所有资源已校验"));
    tracker.finish_stage();
    Ok(())
}

/// Read-only validation for a loader's vanilla parent. Existing vanilla files are never rewritten.
pub(crate) fn verify_vanilla_parent(
    root: &Path,
    id: &str,
    platform: &Platform,
    cancel: &AtomicBool,
) -> Result<Value> {
    validate_id(id)?;
    cancelled(cancel)?;
    let metadata_path = safe_target(root, &PathBuf::from(format!("versions/{id}/{id}.json")))?;
    let version: Value = serde_json::from_slice(&fs::read(metadata_path)?)?;
    if version["id"].as_str() != Some(id) || version.get("inheritsFrom").is_some() {
        bail!("加载器必须以独立的原版版本为父版本：{id}");
    }
    let mut artifacts = library_artifacts(&version, platform)?;
    artifacts.push(metadata_artifact(
        &version["downloads"]["client"],
        format!("versions/{id}/{id}.jar").into(),
    )?);
    if let Some(logging) = version.pointer("/logging/client/file") {
        let log_id = logging["id"].as_str().context("日志配置缺少 ID")?;
        validate_id(log_id)?;
        artifacts.push(metadata_artifact(
            logging,
            PathBuf::from("assets/log_configs").join(log_id),
        )?);
    }
    let index_id = version
        .pointer("/assetIndex/id")
        .and_then(Value::as_str)
        .context("原版缺少资源索引")?;
    validate_id(index_id)?;
    let index_artifact = metadata_artifact(
        &version["assetIndex"],
        format!("assets/indexes/{index_id}.json").into(),
    )?;
    let index_path = safe_target(root, &index_artifact.relative_path)?;
    if !cache_valid(&index_path, &index_artifact, cancel)? {
        bail!("原版资源索引缺失或损坏，请先修复原版：{id}");
    }
    let index: Value = serde_json::from_slice(&fs::read(index_path)?)?;
    if index["map_to_resources"].as_bool() == Some(true) {
        bail!("该历史原版需要独立 resources 布局，当前加载器安装尚不支持此布局");
    }
    let mut seen = HashSet::new();
    for (name, object) in index["objects"]
        .as_object()
        .context("资源索引缺少 objects")?
    {
        safe_relative(name)?;
        let hash = expected_hash(object["hash"].as_str())?.context("资源缺少 SHA1")?;
        if !seen.insert(hash.clone()) {
            continue;
        }
        let relative = format!("assets/objects/{}/{hash}", &hash[..2]);
        artifacts.push(Artifact {
            relative_path: relative.into(),
            url: format!(
                "https://resources.download.minecraft.net/{}/{hash}",
                &hash[..2]
            ),
            sha1: Some(hash),
            size: Some(object["size"].as_u64().context("资源缺少大小")?),
            native: false,
            excludes: vec![],
        });
    }
    for artifact in artifacts {
        if !cache_valid(
            &safe_target(root, &artifact.relative_path)?,
            &artifact,
            cancel,
        )? {
            bail!(
                "原版文件缺失或损坏，请先修复原版（不会改动已有原版）：{}",
                artifact.relative_path.display()
            );
        }
    }
    Ok(version)
}

/// Checks a requested separate instance ID before downloading any dependencies.
pub fn validate_instance_id(root: &Path, source_id: &str, id: &str) -> Result<()> {
    validate_id(source_id)?;
    validate_id(id)?;
    if source_id == id {
        return Ok(());
    }
    for relative in [format!("versions/{id}"), format!("instances/{id}")] {
        let path = safe_target(root, &PathBuf::from(relative))?;
        match fs::symlink_metadata(&path) {
            Ok(_) => bail!("实例名称已存在，不能覆盖：{id}"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Installs a separate vanilla instance, preserving and validating any existing parent.
pub fn install_vanilla_instance(
    root: &Path,
    minecraft: &str,
    id: &str,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: impl Fn(Progress) + Sync,
) -> Result<String> {
    cancelled(cancel)?;
    validate_instance_id(root, minecraft, id)?;
    let parent = safe_target(
        root,
        &PathBuf::from(format!("versions/{minecraft}/{minecraft}.json")),
    )?;
    match fs::symlink_metadata(&parent) {
        Ok(_) => with_transfer(
            &progress,
            vec![
                ProgressStage::ExistingVersionValidation,
                ProgressStage::VersionCommit,
            ],
            "只读校验已有原版",
            |tracker| {
                tracker.plan_files(0, 0);
                tracker.begin_stage(
                    ProgressStage::ExistingVersionValidation,
                    format!("只读校验原版 {minecraft}，保留已有配置"),
                    0,
                    Some(0),
                );
                verify_vanilla_parent(root, minecraft, platform, cancel)?;
                tracker.finish_stage();
                tracker.begin_stage(
                    ProgressStage::VersionCommit,
                    format!("登记实例 {id}"),
                    0,
                    Some(0),
                );
                let id = register_instance_id(root, minecraft, id, platform, cancel)?;
                tracker.finish_stage();
                Ok(id)
            },
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            with_transfer(
                &progress,
                vanilla_plan(),
                "读取 Mojang 版本清单",
                |tracker| {
                    install_version_tracked(root, minecraft, platform, cancel, tracker, Some(id))
                },
            )?;
            Ok(id.to_owned())
        }
        Err(error) => Err(error.into()),
    }
}

/// Registers a distinct instance inheriting an installed version, without modifying the source.
pub fn register_instance_id(
    root: &Path,
    source_id: &str,
    id: &str,
    platform: &Platform,
    cancel: &AtomicBool,
) -> Result<String> {
    cancelled(cancel)?;
    validate_instance_id(root, source_id, id)?;
    if source_id == id {
        return Ok(id.to_owned());
    }
    let version = crate::metadata::resolve_version(root, source_id)?;
    version["mainClass"]
        .as_str()
        .context("源版本缺少主类，无法登记实例")?;
    if let Some(index_id) = version.pointer("/assetIndex/id").and_then(Value::as_str) {
        validate_id(index_id)?;
        let path = safe_target(
            root,
            &PathBuf::from(format!("assets/indexes/{index_id}.json")),
        )?;
        let index: Value = serde_json::from_slice(&fs::read(path)?)?;
        if index["map_to_resources"].as_bool() == Some(true) {
            bail!("此历史版本使用独立 resources 布局，暂不支持自定义实例名称；原版安装已保留");
        }
    }
    let artifacts = library_artifacts(&version, platform)?;
    let profile = serde_json::json!({"id":id,"inheritsFrom":source_id,"type":version["type"].as_str().unwrap_or("release")});
    let directory = safe_target(root, &PathBuf::from(format!("versions/{id}")))?;
    let instance = safe_target(root, &PathBuf::from(format!("instances/{id}")))?;
    fs::create_dir_all(directory.parent().context("版本目录缺少父目录")?)?;
    fs::create_dir_all(instance.parent().context("实例目录缺少父目录")?)?;
    // Reserve both names atomically. Existing or concurrently created user directories are
    // never reused, and error cleanup removes only our own still-empty directories.
    fs::create_dir(&directory).context("实例名称被占用，不能覆盖版本目录")?;
    let mut instance_owned = false;
    let result = (|| {
        fs::create_dir(&instance).context("实例名称被占用，不能覆盖游戏目录")?;
        instance_owned = true;
        crate::loaders::commit_profile(root, id, &profile, &artifacts, cancel)
    })();
    if let Err(error) = result {
        let instance_cleanup = !instance_owned || fs::remove_dir(&instance).is_ok();
        let directory_cleanup = fs::remove_dir(&directory).is_ok();
        return Err(error).context(if !instance_cleanup || !directory_cleanup {
            "自定义实例未登记；源版本已保留，目标目录含其他文件，请检查后使用其他名称"
        } else {
            "自定义实例未登记；源版本已保留"
        });
    }
    Ok(id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_instance_id_inherits_and_never_overwrites_existing_data() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("versions/base")).unwrap();
        let source = br#"{"id":"base","mainClass":"example.Main","libraries":[],"type":"release"}"#;
        fs::write(root.path().join("versions/base/base.json"), source).unwrap();
        let cancel = AtomicBool::new(false);
        register_instance_id(
            root.path(),
            "base",
            "我的实例",
            &Platform::current(),
            &cancel,
        )
        .unwrap();
        let profile: Value = serde_json::from_slice(
            &fs::read(root.path().join("versions/我的实例/我的实例.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(profile["inheritsFrom"], "base");
        assert!(root.path().join("instances/我的实例").is_dir());
        assert_eq!(
            fs::read(root.path().join("versions/base/base.json")).unwrap(),
            source
        );
        assert!(register_instance_id(
            root.path(),
            "base",
            "我的实例",
            &Platform::current(),
            &cancel
        )
        .is_err());
        assert!(validate_instance_id(root.path(), "base", "../escape").is_err());
        fs::create_dir_all(root.path().join("instances/user-world")).unwrap();
        fs::write(
            root.path().join("instances/user-world/level.dat"),
            b"user world",
        )
        .unwrap();
        assert!(validate_instance_id(root.path(), "base", "user-world").is_err());
        assert_eq!(
            fs::read(root.path().join("instances/user-world/level.dat")).unwrap(),
            b"user world"
        );
        assert!(register_instance_id(
            root.path(),
            "base",
            "cancelled",
            &Platform::current(),
            &AtomicBool::new(true)
        )
        .is_err());
        assert!(!root.path().join("versions/cancelled").exists());
    }

    #[test]
    fn named_vanilla_reuses_custom_parent_without_rewriting_json_libraries_or_natives() {
        let root = tempfile::tempdir().unwrap();
        let client = b"client";
        let library = b"custom-library";
        let index = br#"{"objects":{}}"#;
        for (relative, bytes) in [
            ("versions/base/base.jar", client.as_slice()),
            (
                "libraries/org/example/custom/1/custom-1.jar",
                library.as_slice(),
            ),
            (
                "versions/base/natives/user-note",
                b"preserve native directory".as_slice(),
            ),
            ("assets/indexes/test.json", index.as_slice()),
        ] {
            let path = root.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        let hash = |bytes: &[u8]| format!("{:x}", Sha1::digest(bytes));
        let source = serde_json::to_vec_pretty(&serde_json::json!({
            "id":"base", "mainClass":"example.Main", "type":"release",
            "arguments":{"jvm":["-Dmy.user.flag=true"]},
            "downloads":{"client":{"url":"https://piston-data.mojang.com/client.jar", "sha1":hash(client), "size":client.len()}},
            "assetIndex":{"id":"test", "url":"https://piston-meta.mojang.com/test.json", "sha1":hash(index), "size":index.len()},
            "libraries":[{"name":"org.example:custom:1", "downloads":{"artifact":{
                "url":"https://libraries.minecraft.net/org/example/custom/1/custom-1.jar", "sha1":hash(library), "size":library.len()
            }}}]
        })).unwrap();
        let source_path = root.path().join("versions/base/base.json");
        fs::write(&source_path, &source).unwrap();
        install_vanilla_instance(
            root.path(),
            "base",
            "custom",
            &Platform::current(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(fs::read(&source_path).unwrap(), source);
        assert_eq!(
            fs::read(
                root.path()
                    .join("libraries/org/example/custom/1/custom-1.jar")
            )
            .unwrap(),
            library
        );
        assert_eq!(
            fs::read(root.path().join("versions/base/natives/user-note")).unwrap(),
            b"preserve native directory"
        );
        let resolved = crate::metadata::resolve_version(root.path(), "custom").unwrap();
        assert_eq!(resolved["arguments"]["jvm"][0], "-Dmy.user.flag=true");
        // A corrupt parent remains untouched and fails before registering another instance.
        fs::write(root.path().join("versions/base/base.jar"), b"broken").unwrap();
        assert!(install_vanilla_instance(
            root.path(),
            "base",
            "failed",
            &Platform::current(),
            &AtomicBool::new(false),
            |_| {}
        )
        .is_err());
        assert!(!root.path().join("versions/failed").exists());
        assert_eq!(fs::read(source_path).unwrap(), source);
        assert_eq!(
            fs::read(root.path().join("versions/base/base.jar")).unwrap(),
            b"broken"
        );
    }

    #[test]
    fn failed_custom_registration_releases_only_its_own_directories() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("versions/base")).unwrap();
        let source = serde_json::json!({
            "id":"base", "mainClass":"example.Main", "libraries":[{
                "name":"org.example:native:2", "natives":{"windows":"natives-${arch}"},
                "downloads":{"classifiers":{"natives-64":{
                    "path":"org/example/native/2/native-2-natives-64.jar",
                    "url":"https://libraries.minecraft.net/missing.jar"
                }}}
            }]
        });
        let source_path = root.path().join("versions/base/base.json");
        fs::write(&source_path, serde_json::to_vec(&source).unwrap()).unwrap();
        let windows = Platform {
            os: "windows".into(),
            arch: "x86_64".into(),
            version: "10.0".into(),
        };
        assert!(register_instance_id(
            root.path(),
            "base",
            "retry",
            &windows,
            &AtomicBool::new(false)
        )
        .is_err());
        assert!(!root.path().join("versions/retry").exists());
        assert!(!root.path().join("instances/retry").exists());
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(source_path).unwrap()).unwrap(),
            source
        );
        validate_instance_id(root.path(), "base", "retry").unwrap();
    }

    use zip::{write::SimpleFileOptions, ZipWriter};

    fn artifact(bytes: &[u8]) -> Artifact {
        Artifact {
            relative_path: "test.bin".into(),
            url: "https://libraries.minecraft.net/test.bin".into(),
            sha1: Some(format!("{:x}", Sha1::digest(bytes))),
            size: Some(bytes.len() as u64),
            native: false,
            excludes: vec![],
        }
    }

    #[test]
    fn corrupted_same_size_cache_is_not_reused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.bin");
        fs::write(&path, b"bad!").unwrap();
        assert!(!cache_valid(&path, &artifact(b"good"), &AtomicBool::new(false)).unwrap());
        fs::write(&path, b"good").unwrap();
        assert!(cache_valid(&path, &artifact(b"good"), &AtomicBool::new(false)).unwrap());
    }

    #[test]
    fn cancelled_cache_check_exits_before_using_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.bin");
        fs::write(&path, b"good").unwrap();
        assert!(
            cache_valid(&path, &artifact(b"good"), &AtomicBool::new(true))
                .unwrap_err()
                .to_string()
                .contains("取消")
        );
    }

    #[test]
    fn failed_or_incomplete_replacement_preserves_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.bin");
        fs::write(&path, b"previous").unwrap();
        for bytes in [b"bad!".as_slice(), b"go".as_slice()] {
            assert!(store_verified(
                directory.path(),
                &artifact(b"good"),
                &mut &bytes[..],
                &AtomicBool::new(false)
            )
            .is_err());
            assert_eq!(fs::read(&path).unwrap(), b"previous");
            assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        }
        store_verified(
            directory.path(),
            &artifact(b"good"),
            &mut &b"good"[..],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"good");
    }

    #[test]
    fn cancellation_mid_stream_never_commits_partial_bytes() {
        struct CancellingReader<'a> {
            cancel: &'a AtomicBool,
        }
        impl Read for CancellingReader<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                buf[..4].copy_from_slice(b"good");
                self.cancel.store(true, Ordering::Relaxed);
                Ok(4)
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let error = store_verified(
            directory.path(),
            &artifact(b"good"),
            &mut CancellingReader { cancel: &cancel },
            &cancel,
        )
        .unwrap_err();
        assert!(error.to_string().contains("取消"));
        assert!(!directory.path().join("test.bin").exists());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn cancellation_is_typed_and_late_cancel_does_not_relabel_a_real_io_error() {
        let error = cancelled(&AtomicBool::new(true)).unwrap_err();
        assert!(error
            .downcast_ref::<crate::model::OperationCancelled>()
            .is_some());
        struct FailingReader<'a>(&'a AtomicBool);
        impl Read for FailingReader<'_> {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                self.0.store(true, Ordering::Relaxed);
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "real reset",
                ))
            }
        }
        let root = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let error = store_verified(
            root.path(),
            &artifact(b"good"),
            &mut FailingReader(&cancel),
            &cancel,
        )
        .unwrap_err();
        assert!(cancel.load(Ordering::Relaxed));
        assert!(error.chain().all(|cause| {
            cause
                .downcast_ref::<crate::model::OperationCancelled>()
                .is_none()
        }));
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::ConnectionReset
        );
        assert!(!root.path().join("test.bin").exists());
    }

    fn make_zip(path: &Path, entry: &str) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        zip.start_file(entry, SimpleFileOptions::default()).unwrap();
        zip.write_all(b"native").unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn rejects_zip_traversal_and_windows_separators() {
        for name in ["../escape", "a/../../escape", "a\\..\\escape", "/absolute"] {
            let directory = tempfile::tempdir().unwrap();
            let archive = directory.path().join("natives.jar");
            let destination = directory.path().join("out");
            fs::create_dir(&destination).unwrap();
            make_zip(&archive, name);
            assert!(
                extract_natives(&archive, &destination, &[], &AtomicBool::new(false)).is_err(),
                "{name}"
            );
            assert!(!directory.path().join("escape").exists());
        }
    }

    #[test]
    fn extracts_safe_native_and_honors_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("natives.jar");
        let destination = directory.path().join("out");
        fs::create_dir(&destination).unwrap();
        make_zip(&archive, "lib/example.dylib");
        assert!(extract_natives(&archive, &destination, &[], &AtomicBool::new(true)).is_err());
        assert!(!destination.join("lib/example.dylib").exists());
        extract_natives(&archive, &destination, &[], &AtomicBool::new(false)).unwrap();
        assert_eq!(
            fs::read(destination.join("lib/example.dylib")).unwrap(),
            b"native"
        );
    }

    #[test]
    fn rejects_symlinks_inside_native_zip() {
        let directory = tempfile::tempdir().unwrap();
        let archive_path = directory.path().join("natives.jar");
        let mut archive = ZipWriter::new(File::create(&archive_path).unwrap());
        archive
            .add_symlink("link", "../../outside", SimpleFileOptions::default())
            .unwrap();
        archive.finish().unwrap();
        let destination = directory.path().join("out");
        fs::create_dir(&destination).unwrap();
        assert!(
            extract_natives(&archive_path, &destination, &[], &AtomicBool::new(false))
                .unwrap_err()
                .to_string()
                .contains("符号链接")
        );
        assert!(!destination.join("link").exists());
    }

    #[test]
    fn empty_and_small_asset_batches_report_the_actual_worker_capacity() {
        let directory = tempfile::tempdir().unwrap();
        let client = http_client().unwrap();
        for count in [0, 3] {
            let mut artifacts = Vec::new();
            for index in 0..count {
                let mut item = artifact(b"good");
                item.relative_path = format!("small-asset-{index}").into();
                fs::write(directory.path().join(&item.relative_path), b"good").unwrap();
                artifacts.push(item);
            }
            let items: Vec<_> = artifacts.iter().collect();
            let events = Mutex::new(Vec::new());
            let completed = AtomicU64::new(0);
            with_transfer(
                &|event| events.lock().unwrap().push(event),
                vec![ProgressStage::AssetFiles],
                "small cache fixture",
                |tracker| {
                    tracker.plan_files(count as u64, count as u64);
                    tracker.begin_stage(
                        ProgressStage::AssetFiles,
                        "cached files",
                        count as u64,
                        None,
                    );
                    download_assets(
                        &client,
                        directory.path(),
                        &items,
                        &AtomicBool::new(false),
                        &completed,
                        tracker,
                    )
                },
            )
            .unwrap();
            let events = events.lock().unwrap();
            let sample = events.last().unwrap().transfer.as_ref().unwrap();
            assert_eq!(sample.concurrency_limit, Some(count as u32));
            assert_eq!(sample.active_downloads, Some(0));
            assert_eq!(sample.downloaded_bytes, 0);
            assert_eq!(completed.load(Ordering::Relaxed), count as u64);
        }
    }

    #[test]
    fn concurrent_cache_verification_counts_once_and_failure_preserves_user_cancel() {
        let directory = tempfile::tempdir().unwrap();
        let mut artifacts = Vec::new();
        for index in 0..24 {
            let mut item = artifact(b"good");
            item.relative_path = format!("asset-{index}").into();
            fs::write(directory.path().join(&item.relative_path), b"good").unwrap();
            artifacts.push(item);
        }
        let client = http_client().unwrap();
        let cancel = AtomicBool::new(false);
        let completed = AtomicU64::new(3);
        let events = Mutex::new(Vec::new());
        let progress = |event: Progress| {
            assert!(event.completed <= event.total);
            events.lock().unwrap().push(event);
        };
        let items: Vec<_> = artifacts.iter().collect();
        with_transfer(
            &progress,
            vec![ProgressStage::CoreLibraries, ProgressStage::AssetFiles],
            "cache test",
            |tracker| {
                tracker.plan_files(27, 24);
                tracker.begin_stage(ProgressStage::CoreLibraries, "previous files", 3, Some(1));
                for _ in 0..3 {
                    tracker.finished_item(true, false);
                }
                tracker.finish_stage();
                tracker.begin_stage(
                    ProgressStage::AssetFiles,
                    "cached files",
                    24,
                    Some(asset_worker_count(items.len()) as u32),
                );
                download_assets(
                    &client,
                    directory.path(),
                    &items,
                    &cancel,
                    &completed,
                    tracker,
                )?;
                tracker.finish_stage();
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(completed.load(Ordering::Relaxed), 27);
        let events = events.lock().unwrap();
        let last = events.last().unwrap();
        assert_eq!(last.completed, 27);
        assert_eq!(last.stage_progress, Some((24, 24)));
        assert_eq!(last.transfer.as_ref().unwrap().remaining_files, Some(0));
        assert_eq!(last.transfer.as_ref().unwrap().concurrency_limit, Some(8));
        assert!(events.iter().all(|event| {
            let transfer = event.transfer.as_ref().unwrap();
            transfer.downloaded_bytes == 0 && transfer.active_downloads == Some(0)
        }));
        drop(events);
        artifacts[0].url = "https://invalid.example/blocked".into();
        let items: Vec<_> = artifacts.iter().collect();
        let completed = AtomicU64::new(0);
        assert!(with_transfer(
            &progress,
            vec![ProgressStage::AssetFiles],
            "invalid URL",
            |tracker| {
                tracker.plan_files(24, 24);
                tracker.begin_stage(
                    ProgressStage::AssetFiles,
                    "verify",
                    24,
                    Some(asset_worker_count(items.len()) as u32),
                );
                download_assets(
                    &client,
                    directory.path(),
                    &items,
                    &cancel,
                    &completed,
                    tracker,
                )
            }
        )
        .is_err());
        assert!(!cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn url_policy_rejects_credentials_untrusted_hosts_and_http() {
        for url in [
            "http://libraries.minecraft.net/a",
            "https://example.org/a",
            "https://libraries.minecraft.net.evil.org/a",
            "https://user:secret@libraries.minecraft.net/a",
            "https://libraries.minecraft.net/a?token=secret",
            "https://s3.amazonaws.com/another-bucket/a",
        ] {
            assert!(validate_url(url).is_err());
        }
        assert!(validate_url(MANIFEST_URL).is_ok());
        assert!(validate_url("https://libraries.minecraft.net/org/lwjgl/lwjgl.jar").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), directory.path().join("assets")).unwrap();
        assert!(safe_target(directory.path(), Path::new("assets/objects/a")).is_err());
    }

    #[test]
    fn old_native_directory_survives_failed_swap() {
        let directory = tempfile::tempdir().unwrap();
        let current = directory.path().join("natives");
        fs::create_dir(&current).unwrap();
        fs::write(current.join("working"), b"old").unwrap();
        assert!(commit_native_directory(
            &directory.path().join("missing"),
            &current,
            &directory.path().join("backup")
        )
        .is_err());
        assert_eq!(fs::read(current.join("working")).unwrap(), b"old");
    }
}
