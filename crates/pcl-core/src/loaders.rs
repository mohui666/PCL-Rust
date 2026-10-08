//! Fabric / Quilt installation from their official launcher profiles.
//! API sources: https://github.com/FabricMC/fabric-meta/blob/master/README.md
//! and https://meta.quiltmc.org/openapi.yaml (v3 launcher profile endpoints).
#[path = "liteloader.rs"]
pub mod liteloader;
#[path = "install_registration.rs"]
mod registration;
pub use registration::RetryRegistration;
#[path = "optifine.rs"]
pub mod optifine;
use crate::{
    install::{
        self, cancelled, expected_hash, http_client, request_bytes, safe_target, validate_url,
    },
    metadata::{library_artifacts, resolve_version, validate_id},
    model::{Artifact, Platform, Progress},
};
use anyhow::{bail, Context, Result};
use reqwest::{blocking::Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoaderKind {
    Fabric,
    Quilt,
}

impl LoaderKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Fabric => "fabric",
            Self::Quilt => "quilt",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Fabric => "Fabric",
            Self::Quilt => "Quilt",
        }
    }
    fn meta(self) -> &'static str {
        match self {
            Self::Fabric => "https://meta.fabricmc.net/v2/versions/",
            Self::Quilt => "https://meta.quiltmc.org/v3/versions/",
        }
    }
    fn coordinate(self, version: &str) -> String {
        match self {
            Self::Fabric => format!("net.fabricmc:fabric-loader:{version}"),
            Self::Quilt => format!("org.quiltmc:quilt-loader:{version}"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoaderVersion {
    pub version: String,
    /// Uses upstream stable when present; Quilt loader releases without a prerelease suffix are stable.
    pub stable: bool,
}

fn endpoint(kind: LoaderKind, segments: &[&str]) -> Result<String> {
    let mut url = Url::parse(kind.meta())?;
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("加载器 API URL 无效"))?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
    }
    Ok(url.to_string())
}

fn read_json(client: &Client, url: &str, cancel: &AtomicBool) -> Result<Value> {
    serde_json::from_slice(&request_bytes(client, url, None, None, cancel)?)
        .context("加载器官方 API 返回了无效 JSON")
}

fn versions(value: &Value, nested: bool) -> Result<Vec<LoaderVersion>> {
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for entry in value.as_array().context("加载器版本清单必须是数组")? {
        let entry = if nested {
            entry.get("loader").context("兼容版本条目缺少 loader")?
        } else {
            entry
        };
        let version = entry["version"]
            .as_str()
            .context("加载器版本条目缺少 version")?;
        validate_id(version)?;
        if seen.insert(version.to_owned()) {
            output.push(LoaderVersion {
                version: version.to_owned(),
                stable: entry["stable"].as_bool().unwrap_or(!version.contains('-')),
            });
        }
    }
    Ok(output)
}

pub fn list_game_versions(kind: LoaderKind, cancel: &AtomicBool) -> Result<Vec<LoaderVersion>> {
    let client = http_client()?;
    versions(
        &read_json(&client, &endpoint(kind, &["game"])?, cancel)?,
        false,
    )
}

fn compare_loader_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let parts = |version: &str| -> (Vec<u64>, bool, Vec<String>) {
        let version = version.split('+').next().unwrap_or(version);
        let (base, pre) = version.split_once('-').unwrap_or((version, ""));
        (
            base.split('.')
                .map(|part| part.parse().unwrap_or(0))
                .collect(),
            pre.is_empty(),
            pre.split('.').map(str::to_owned).collect(),
        )
    };
    let (a, a_release, a_pre) = parts(left);
    let (b, b_release, b_pre) = parts(right);
    a.cmp(&b).then(a_release.cmp(&b_release)).then_with(|| {
        for (a, b) in a_pre.iter().zip(&b_pre) {
            let order = match (a.parse::<u64>(), b.parse::<u64>()) {
                (Ok(a), Ok(b)) => a.cmp(&b),
                _ => a.cmp(b),
            };
            if !order.is_eq() {
                return order;
            }
        }
        a_pre.len().cmp(&b_pre.len())
    })
}

pub fn list_loader_versions(
    kind: LoaderKind,
    minecraft: &str,
    cancel: &AtomicBool,
) -> Result<Vec<LoaderVersion>> {
    validate_id(minecraft)?;
    let client = http_client()?;
    let mut output = versions(
        &read_json(&client, &endpoint(kind, &["loader", minecraft])?, cancel)?,
        true,
    )?;
    // Quilt's compatible-loader endpoint does not promise sorted output.
    output.sort_by(|a, b| compare_loader_versions(&b.version, &a.version));
    Ok(output)
}

fn validate_profile(
    kind: LoaderKind,
    minecraft: &str,
    loader: &str,
    profile: &Value,
) -> Result<String> {
    validate_id(minecraft)?;
    validate_id(loader)?;
    let id = profile["id"]
        .as_str()
        .context("官方加载器 profile 缺少 id")?;
    validate_id(id)?;
    if id == minecraft || !id.starts_with(&format!("{}-loader-", kind.id())) {
        bail!("加载器 profile ID 无效或会覆盖原版");
    }
    if profile["inheritsFrom"].as_str() != Some(minecraft) {
        bail!("加载器 profile 的 Minecraft 父版本不匹配");
    }
    if profile.get("jar").is_some() || profile.get("downloads").is_some() {
        bail!("加载器 profile 意外替换原版 client，拒绝安装");
    }
    let main = profile["mainClass"]
        .as_str()
        .context("加载器 profile 缺少主类")?;
    let prefix = match kind {
        LoaderKind::Fabric => "net.fabricmc.loader.",
        LoaderKind::Quilt => "org.quiltmc.loader.",
    };
    if !main.starts_with(prefix) || !main.ends_with(".KnotClient") {
        bail!("加载器 profile 不是所选加载器的客户端入口");
    }
    let libraries = profile["libraries"]
        .as_array()
        .context("加载器 profile 缺少 libraries")?;
    if libraries.is_empty() || libraries.len() > 256 {
        bail!("加载器 libraries 数量无效");
    }
    let required = kind.coordinate(loader);
    if !libraries
        .iter()
        .any(|library| library["name"].as_str() == Some(required.as_str()))
    {
        bail!("加载器 profile 未包含所选 loader 版本");
    }
    Ok(id.to_owned())
}

pub fn fetch_profile(
    kind: LoaderKind,
    minecraft: &str,
    loader: &str,
    cancel: &AtomicBool,
) -> Result<Value> {
    validate_id(minecraft)?;
    validate_id(loader)?;
    let client = http_client()?;
    let profile = read_json(
        &client,
        &endpoint(kind, &["loader", minecraft, loader, "profile", "json"])?,
        cancel,
    )?;
    validate_profile(kind, minecraft, loader, &profile)?;
    Ok(profile)
}

fn parse_checksum(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes).context("Maven SHA1 文件不是 UTF-8")?;
    if bytes.len() > 1024 {
        bail!("Maven SHA1 文件过长");
    }
    // Maven repositories publish either a bare digest or sha1sum's digest + filename format.
    let digest = text
        .split_whitespace()
        .next()
        .context("Maven SHA1 文件为空")?;
    expected_hash(Some(digest))?.context("Maven SHA1 文件缺少摘要")
}

fn enrich_libraries(
    client: &Client,
    profile: &mut Value,
    platform: &Platform,
    cancel: &AtomicBool,
) -> Result<Vec<Artifact>> {
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    for library in profile["libraries"]
        .as_array_mut()
        .context("profile libraries 必须是数组")?
    {
        cancelled(cancel)?;
        if library.get("natives").is_some() {
            bail!("官方 Fabric/Quilt profile 含未支持的额外原生库声明");
        }
        let mut artifacts = library_artifacts(&json!({"libraries":[library.clone()]}), platform)?;
        if artifacts.is_empty() {
            continue;
        }
        if artifacts.len() != 1 {
            bail!("加载器依赖包含未支持的复合原生库");
        }
        let mut artifact = artifacts.remove(0);
        let url = validate_url(&artifact.url)?;
        if !matches!(
            url.host_str(),
            Some("maven.fabricmc.net" | "maven.quiltmc.org" | "libraries.minecraft.net")
        ) {
            bail!(
                "加载器依赖不是受支持的官方 Maven 来源：{}",
                artifact.relative_path.display()
            );
        }
        if !seen.insert(artifact.relative_path.clone()) {
            bail!("加载器 profile 含重复依赖路径");
        }
        artifact.sha1 = match artifact
            .sha1
            .as_deref()
            .or_else(|| library["sha1"].as_str())
        {
            Some(hash) => expected_hash(Some(hash))?,
            None => Some(parse_checksum(
                &request_bytes(
                    client,
                    &format!("{}.sha1", artifact.url),
                    None,
                    None,
                    cancel,
                )
                .with_context(|| {
                    format!(
                        "获取 Maven 校验和失败：{}",
                        artifact.relative_path.display()
                    )
                })?,
            )?),
        };
        artifact.size = artifact.size.or_else(|| library["size"].as_u64());
        if artifact.size.is_none() {
            cancelled(cancel)?;
            let response = client.head(url).send()?.error_for_status()?;
            validate_url(response.url().as_str())?;
            artifact.size = response
                .headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|size| size.to_str().ok())
                .and_then(|size| size.parse().ok());
            if artifact.size.is_none() {
                bail!(
                    "官方 Maven 未提供文件大小：{}",
                    artifact.relative_path.display()
                );
            }
        }
        let relative = artifact
            .relative_path
            .strip_prefix("libraries")
            .context("加载器库路径不在 libraries 内")?
            .components()
            .map(|part| part.as_os_str().to_str().context("库路径不是 UTF-8"))
            .collect::<Result<Vec<_>>>()?
            .join("/");
        // Preserve the standard profile, adding verified standard downloads metadata for future repair.
        library["downloads"] = json!({"artifact": {"path": relative, "url": artifact.url, "sha1": artifact.sha1, "size": artifact.size}});
        output.push(artifact);
    }
    Ok(output)
}

fn require_absent(path: &Path, description: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("{description}已存在，拒绝覆盖：{}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn verify_artifact(root: &Path, artifact: &Artifact, cancel: &AtomicBool) -> Result<()> {
    expected_hash(artifact.sha1.as_deref())?.with_context(|| {
        format!(
            "既有依赖缺少 SHA1，无法只读验证：{}",
            artifact.relative_path.display()
        )
    })?;
    artifact.size.with_context(|| {
        format!(
            "既有依赖缺少大小，无法只读验证：{}",
            artifact.relative_path.display()
        )
    })?;
    if !install::cache_valid(
        &safe_target(root, &artifact.relative_path)?,
        artifact,
        cancel,
    )? {
        bail!(
            "既有加载器文件缺失或损坏：{}",
            artifact.relative_path.display()
        );
    }
    Ok(())
}

fn metadata_file(value: &Value, relative_path: PathBuf) -> Result<Artifact> {
    Ok(Artifact {
        relative_path,
        url: String::new(),
        sha1: expected_hash(value["sha1"].as_str())?,
        size: value["size"].as_u64(),
        native: false,
        excludes: vec![],
    })
}

fn file_digest(path: &Path, cancel: &AtomicBool) -> Result<(String, u64)> {
    let mut input = fs::File::open(path)?;
    let mut hash = Sha1::new();
    let mut size = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        cancelled(cancel)?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        size += count as u64;
    }
    Ok((format!("{:x}", hash.finalize()), size))
}

fn verify_native_tree(
    root: &Path,
    expected: &Path,
    relative: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    for entry in fs::read_dir(expected)? {
        cancelled(cancel)?;
        let entry = entry?;
        let relative = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            verify_native_tree(root, &entry.path(), &relative, cancel)?;
        } else if kind.is_file() {
            let (sha1, size) = file_digest(&entry.path(), cancel)?;
            verify_artifact(
                root,
                &Artifact {
                    relative_path: relative,
                    url: String::new(),
                    sha1: Some(sha1),
                    size: Some(size),
                    native: false,
                    excludes: vec![],
                },
                cancel,
            )?;
        } else {
            bail!("原生库校验目录包含不支持的文件类型");
        }
    }
    Ok(())
}

fn reuse_existing_profile(
    root: &Path,
    id: &str,
    kind: LoaderKind,
    minecraft: &str,
    loader: &str,
    platform: &Platform,
    cancel: &AtomicBool,
) -> Result<Option<String>> {
    let path = safe_target(root, &PathBuf::from(format!("versions/{id}/{id}.json")))?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let verify = (|| {
        cancelled(cancel)?;
        if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
            bail!("既有 profile 文件类型或大小无效");
        }
        let existing: Value = serde_json::from_slice(&fs::read(path)?)?;
        if validate_profile(kind, minecraft, loader, &existing)? != id {
            bail!("既有 profile ID 与目录不一致");
        }
        let parent = install::verify_vanilla_parent(root, minecraft, platform, cancel)?;
        let resolved = resolve_version(root, id)?;
        // A customized asset source needs its own repair workflow; do not silently replace it.
        if resolved.get("assetIndex") != parent.get("assetIndex")
            || resolved.get("assets") != parent.get("assets")
        {
            bail!("既有 profile 更改了原版资源配置，当前无法只读验证");
        }
        let mut artifacts = Vec::new();
        for library in resolved["libraries"]
            .as_array()
            .context("既有 profile libraries 无效")?
        {
            for mut artifact in
                library_artifacts(&json!({"libraries":[library.clone()]}), platform)?
            {
                if !artifact.native {
                    artifact.sha1 = artifact
                        .sha1
                        .or_else(|| library["sha1"].as_str().map(str::to_owned));
                    artifact.size = artifact.size.or_else(|| library["size"].as_u64());
                }
                verify_artifact(root, &artifact, cancel)?;
                artifacts.push(artifact);
            }
        }
        let jar_id = resolved["_pcl_jar_id"]
            .as_str()
            .context("既有 profile 缺少 client jar 来源")?;
        validate_id(jar_id)?;
        verify_artifact(
            root,
            &metadata_file(
                &resolved["downloads"]["client"],
                format!("versions/{jar_id}/{jar_id}.jar").into(),
            )?,
            cancel,
        )?;
        if let Some(logging) = resolved.pointer("/logging/client/file") {
            let logging_id = logging["id"].as_str().context("既有日志配置缺少 ID")?;
            validate_id(logging_id)?;
            verify_artifact(
                root,
                &metadata_file(
                    logging,
                    PathBuf::from("assets/log_configs").join(logging_id),
                )?,
                cancel,
            )?;
        }
        if artifacts.iter().any(|artifact| artifact.native) {
            let relative = PathBuf::from(format!("versions/{id}/natives"));
            if !safe_target(root, &relative)?.is_dir() {
                bail!("既有 profile 缺少原生库目录");
            }
            let expected = tempfile::tempdir()?;
            for artifact in artifacts.iter().filter(|artifact| artifact.native) {
                install::extract_natives(
                    &safe_target(root, &artifact.relative_path)?,
                    expected.path(),
                    &artifact.excludes,
                    cancel,
                )?;
            }
            verify_native_tree(root, expected.path(), &relative, cancel)?;
        }
        cancelled(cancel)?;
        Ok::<_, anyhow::Error>(())
    })();
    verify.with_context(|| {
        format!("既有加载器 {id} 存在冲突或无法验证，保留原有 JSON、库及原生库，不会覆盖")
    })?;
    Ok(Some(id.to_owned()))
}

enum CreatedPath {
    File(PathBuf),
    Directory(PathBuf),
}

fn rollback_created_paths(created: &[CreatedPath]) -> Result<()> {
    let mut failed = Vec::new();
    for entry in created.iter().rev() {
        let (path, result) = match entry {
            CreatedPath::File(path) => (path, fs::remove_file(path)),
            CreatedPath::Directory(path) => (path, fs::remove_dir(path)),
        };
        if let Err(error) = result {
            if error.kind() != std::io::ErrorKind::NotFound {
                failed.push(path.display().to_string());
            }
        }
    }
    if !failed.is_empty() {
        bail!(
            "部分本次创建的路径未能回滚（目录可能含其他文件，已保留）：{}",
            failed.join("、")
        );
    }
    Ok(())
}

fn commit_native_tree_new(
    source: &Path,
    target: &Path,
    cancel: &AtomicBool,
    created: &mut Vec<CreatedPath>,
) -> Result<()> {
    cancelled(cancel)?;
    // Each successful mutation is recorded immediately; rollback never traverses unknown files.
    fs::create_dir(target).context("原生库目标已存在或无法创建，未替换已有目录")?;
    created.push(CreatedPath::Directory(target.to_owned()));
    for entry in fs::read_dir(source)? {
        cancelled(cancel)?;
        let entry = entry?;
        let destination = target.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            commit_native_tree_new(&entry.path(), &destination, cancel, created)?;
        } else if kind.is_file() {
            fs::hard_link(entry.path(), &destination).context("原生库提交冲突，未覆盖文件")?;
            created.push(CreatedPath::File(destination));
        } else {
            bail!("待提交原生库包含不支持的文件类型");
        }
    }
    Ok(())
}

pub(crate) fn commit_profile(
    root: &Path,
    id: &str,
    profile: &Value,
    parent_artifacts: &[Artifact],
    cancel: &AtomicBool,
) -> Result<()> {
    validate_id(id)?;
    cancelled(cancel)?;
    let target = safe_target(root, &PathBuf::from(format!("versions/{id}/{id}.json")))?;
    require_absent(&target, "加载器 profile")?;
    let natives_target = safe_target(root, &PathBuf::from(format!("versions/{id}/natives")))?;
    require_absent(&natives_target, "加载器原生库目录")?;
    let directory = target.parent().context("加载器版本路径缺少父目录")?;
    fs::create_dir_all(directory.parent().context("加载器版本目录缺少父目录")?)?;
    let mut created = Vec::new();
    match fs::create_dir(directory) {
        Ok(()) => created.push(CreatedPath::Directory(directory.to_owned())),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && directory.is_dir() => (),
        Err(error) => return Err(error.into()),
    }
    let result = (|| -> Result<()> {
        let staging = tempfile::tempdir_in(directory)?;
        let natives = staging.path().join("natives");
        fs::create_dir(&natives)?;
        for artifact in parent_artifacts.iter().filter(|artifact| artifact.native) {
            install::extract_natives(
                &safe_target(root, &artifact.relative_path)?,
                &natives,
                &artifact.excludes,
                cancel,
            )?;
        }
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer_pretty(&mut temporary, profile)?;
        temporary.write_all(b"\n")?;
        temporary.as_file().sync_all()?;
        cancelled(cancel)?;
        require_absent(&target, "加载器 profile")?;
        commit_native_tree_new(&natives, &natives_target, cancel, &mut created)?;
        cancelled(cancel)?;
        safe_target(root, &PathBuf::from(format!("versions/{id}/{id}.json")))?;
        temporary
            .persist_noclobber(&target)
            .map_err(|error| error.error)
            .context("加载器登记冲突，未覆盖已有 profile")?;
        Ok(())
    })();
    // Staging and temporary JSON have been dropped before attempting empty directory cleanup.
    if let Err(error) = result {
        if let Err(rollback) = rollback_created_paths(&created) {
            return Err(error.context(format!("安装未登记，{}：{rollback:#}", directory.display())));
        }
        return Err(error);
    }
    Ok(())
}

pub fn install_loader(
    root: &Path,
    kind: LoaderKind,
    minecraft: &str,
    loader: &str,
    platform: &Platform,
    cancel: &AtomicBool,
    progress: impl Fn(Progress) + Sync,
) -> Result<String> {
    validate_id(minecraft)?;
    validate_id(loader)?;
    cancelled(cancel)?;
    // This standard ID is only used to find a local profile. New profiles still come
    // exclusively from the official API; a local hit needs no network and is never rewritten.
    let local_id = format!("{}-loader-{loader}-{minecraft}", kind.id());
    validate_id(&local_id)?;
    if let Some(id) =
        reuse_existing_profile(root, &local_id, kind, minecraft, loader, platform, cancel)?
    {
        progress(Progress {
            message: format!("已只读验证并复用 {id}，保留用户配置"),
            completed: 1,
            total: 1,
            ..Default::default()
        });
        return Ok(id);
    }
    require_absent(
        &safe_target(root, &PathBuf::from(format!("versions/{local_id}/natives")))?,
        "待安装版本的原生库目录",
    )?;
    progress(Progress {
        message: format!("查询 {} 兼容版本", kind.label()),
        completed: 0,
        total: 0,
        ..Default::default()
    });
    let compatible = list_loader_versions(kind, minecraft, cancel)?;
    if !compatible.iter().any(|version| version.version == loader) {
        bail!(
            "官方 API 未列出 {minecraft} 与 {} {loader} 的兼容组合",
            kind.label()
        );
    }
    let mut profile = fetch_profile(kind, minecraft, loader, cancel)?;
    let id = validate_profile(kind, minecraft, loader, &profile)?;
    if let Some(id) = reuse_existing_profile(root, &id, kind, minecraft, loader, platform, cancel)?
    {
        progress(Progress {
            message: format!("已只读验证并复用 {id}，保留用户配置"),
            completed: 1,
            total: 1,
            ..Default::default()
        });
        return Ok(id);
    }
    require_absent(
        &safe_target(root, &PathBuf::from(format!("versions/{id}/natives")))?,
        "待安装版本的原生库目录",
    )?;
    let parent_path = safe_target(
        root,
        &PathBuf::from(format!("versions/{minecraft}/{minecraft}.json")),
    )?;
    if !parent_path.exists() {
        install::install_version(root, minecraft, platform, cancel, &progress)?;
    }
    progress(Progress {
        message: format!("只读校验原版 {minecraft}"),
        completed: 0,
        total: 0,
        ..Default::default()
    });
    let parent = install::verify_vanilla_parent(root, minecraft, platform, cancel)?;
    let parent_artifacts = library_artifacts(&parent, platform)?;
    let client = http_client()?;
    progress(Progress {
        message: format!("读取 {} 依赖校验信息", kind.label()),
        completed: 0,
        total: 0,
        ..Default::default()
    });
    let artifacts = enrich_libraries(&client, &mut profile, platform, cancel)?;
    for artifact in &artifacts {
        if let Some(parent) = parent_artifacts
            .iter()
            .find(|parent| parent.relative_path == artifact.relative_path)
        {
            if expected_hash(parent.sha1.as_deref())? != artifact.sha1
                || parent.size != artifact.size
            {
                bail!(
                    "加载器依赖与原版共享文件冲突，拒绝改动原版：{}",
                    artifact.relative_path.display()
                );
            }
        }
    }
    let total = artifacts.len() as u64 + 1;
    for (index, artifact) in artifacts.iter().enumerate() {
        cancelled(cancel)?;
        progress(Progress {
            message: format!("校验或下载 {}", artifact.relative_path.display()),
            completed: index as u64,
            total,
            ..Default::default()
        });
        install::download_artifact(&client, root, artifact, cancel).with_context(|| {
            format!(
                "{} 依赖下载失败：{}",
                kind.label(),
                artifact.relative_path.display()
            )
        })?;
    }
    commit_profile(root, &id, &profile, &parent_artifacts, cancel)?;
    progress(Progress {
        message: format!("{id} 安装完成，原版保持不变"),
        completed: total,
        total,
        ..Default::default()
    });
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth,
        launch::{build_plan, LaunchOptions},
    };
    use sha1::{Digest, Sha1};
    use std::collections::BTreeMap;
    use zip::{write::SimpleFileOptions, ZipWriter};

    fn write_fixture_file(root: &Path, path: &str, bytes: &[u8]) -> Value {
        let target = root.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
        json!({"url":"https://libraries.minecraft.net/fixture", "sha1":format!("{:x}",Sha1::digest(bytes)),"size":bytes.len()})
    }

    fn existing_custom_profile() -> (tempfile::TempDir, Platform, String) {
        let root = tempfile::tempdir().unwrap();
        let platform = Platform::current();
        let client =
            write_fixture_file(root.path(), "versions/1.21.1/1.21.1.jar", b"vanilla client");
        let mut index = write_fixture_file(
            root.path(),
            "assets/indexes/fixture.json",
            br#"{"objects":{}}"#,
        );
        index["id"] = json!("fixture");
        let mut logging = write_fixture_file(
            root.path(),
            "assets/log_configs/client.xml",
            b"<Configuration/>",
        );
        logging["id"] = json!("client.xml");
        let parent = json!({"id":"1.21.1","type":"release","mainClass":"net.minecraft.client.main.Main", "downloads":{"client":client},"libraries":[],"assetIndex":index,
            "logging":{"client":{"argument":"-Dlog4j.configurationFile=${path}","file":logging}},
            "arguments":{"jvm":["-cp","${classpath}"],"game":["--username","${auth_player_name}"]}});
        fs::write(
            root.path().join("versions/1.21.1/1.21.1.json"),
            parent.to_string(),
        )
        .unwrap();
        let mut profile = fabric_fixture();
        profile["arguments"]["jvm"]
            .as_array_mut()
            .unwrap()
            .push(json!("-Dmy.user.flag=true"));
        profile["_user_note"] = json!("Preserve my customized profile byte for byte");
        let path = "net/fabricmc/fabric-loader/0.19.5/fabric-loader-0.19.5.jar";
        let mut loader =
            write_fixture_file(root.path(), &format!("libraries/{path}"), b"fixture loader");
        loader["path"] = json!(path);
        profile["libraries"][0]["downloads"] = json!({"artifact":loader});
        let extra_path = "org/example/user-library/1.0/user-library-1.0.jar";
        let mut extra = write_fixture_file(
            root.path(),
            &format!("libraries/{extra_path}"),
            b"user library",
        );
        extra["path"] = json!(extra_path);
        profile["libraries"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"org.example:user-library:1.0","downloads":{"artifact":extra}}));
        let native_path = "org/example/native/1.0/native-1.0-fixture.jar";
        let archive = root.path().join("libraries").join(native_path);
        fs::create_dir_all(archive.parent().unwrap()).unwrap();
        let mut writer = ZipWriter::new(fs::File::create(&archive).unwrap());
        writer
            .start_file("libfixture.bin", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"native!").unwrap();
        writer.finish().unwrap();
        let mut native = write_fixture_file(
            root.path(),
            &format!("libraries/{native_path}"),
            &fs::read(&archive).unwrap(),
        );
        native["path"] = json!(native_path);
        let mut os = serde_json::Map::new();
        os.insert(platform.os.clone(), json!("fixture"));
        profile["libraries"].as_array_mut().unwrap().push(json!({"name":"org.example:native:1.0","natives":os,"downloads":{"classifiers":{"fixture":native}}}));
        let id = "fabric-loader-0.19.5-1.21.1".to_owned();
        let folder = root.path().join("versions").join(&id);
        fs::create_dir_all(folder.join("natives")).unwrap();
        fs::write(folder.join("natives/libfixture.bin"), b"native!").unwrap();
        fs::write(folder.join("natives/my-notes.txt"), b"keep this too").unwrap();
        fs::write(
            folder.join(format!("{id}.json")),
            serde_json::to_vec_pretty(&profile).unwrap(),
        )
        .unwrap();
        (root, platform, id)
    }

    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(root: &Path, current: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(current).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(root, &path, result);
                } else {
                    result.insert(
                        path.strip_prefix(root).unwrap().to_owned(),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }
        let mut result = BTreeMap::new();
        walk(root, root, &mut result);
        result
    }

    #[test]
    fn complete_install_reuses_custom_profile_and_added_libraries_without_network_or_writes() {
        let (root, platform, id) = existing_custom_profile();
        let before = snapshot(root.path());
        let actual = install_loader(
            root.path(),
            LoaderKind::Fabric,
            "1.21.1",
            "0.19.5",
            &platform,
            &AtomicBool::new(false),
            |event| {
                assert!(!event.message.contains("查询"));
            },
        )
        .unwrap();
        assert_eq!(actual, id);
        assert_eq!(snapshot(root.path()), before);
    }

    #[test]
    fn unverifiable_existing_user_libraries_or_natives_fail_before_network_and_remain_untouched() {
        for damage in ["missing hash", "corrupt native", "wrong parent"] {
            let (root, platform, id) = existing_custom_profile();
            let path = root.path().join(format!("versions/{id}/{id}.json"));
            let mut profile: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            match damage {
                "missing hash" => {
                    profile["libraries"][1]["downloads"]["artifact"]
                        .as_object_mut()
                        .unwrap()
                        .remove("sha1");
                }
                "wrong parent" => {
                    profile["inheritsFrom"] = json!("1.21.2");
                }
                _ => {
                    fs::write(
                        root.path()
                            .join(format!("versions/{id}/natives/libfixture.bin")),
                        b"damaged",
                    )
                    .unwrap();
                }
            }
            fs::write(path, serde_json::to_vec_pretty(&profile).unwrap()).unwrap();
            let before = snapshot(root.path());
            let error = install_loader(
                root.path(),
                LoaderKind::Fabric,
                "1.21.1",
                "0.19.5",
                &platform,
                &AtomicBool::new(false),
                |event| {
                    assert!(!event.message.contains("查询"));
                },
            )
            .unwrap_err();
            assert!(format!("{error:#}").contains("不会覆盖"), "{damage}");
            assert_eq!(snapshot(root.path()), before, "{damage}");
        }
    }

    #[test]
    fn fresh_commit_refuses_existing_profile_or_user_native_directory() {
        for has_profile in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let id = "fabric-loader-0.19.5-1.21.1";
            let folder = root.path().join("versions").join(id);
            fs::create_dir_all(&folder).unwrap();
            if has_profile {
                fs::write(folder.join(format!("{id}.json")), b"user modified profile").unwrap();
            } else {
                fs::create_dir(folder.join("natives")).unwrap();
                fs::write(folder.join("natives/user.dll"), b"user native").unwrap();
            }
            let before = snapshot(root.path());
            assert!(commit_profile(
                root.path(),
                id,
                &fabric_fixture(),
                &[],
                &AtomicBool::new(false)
            )
            .is_err());
            assert_eq!(snapshot(root.path()), before);
        }
    }

    #[test]
    fn failed_registration_cleans_owned_directory_and_retry_preserves_existing_notes() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("libraries/bad-native.jar");
        fs::create_dir_all(archive.parent().unwrap()).unwrap();
        let mut zip = ZipWriter::new(fs::File::create(&archive).unwrap());
        zip.start_file("../escape.dll", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"not allowed").unwrap();
        zip.finish().unwrap();
        let bad = Artifact {
            relative_path: PathBuf::from("libraries/bad-native.jar"),
            url: String::new(),
            sha1: None,
            size: None,
            native: true,
            excludes: vec![],
        };
        let id = "retry-loader";
        let directory = root.path().join("versions").join(id);
        assert!(commit_profile(
            root.path(),
            id,
            &json!({"id":id}),
            std::slice::from_ref(&bad),
            &AtomicBool::new(false)
        )
        .is_err());
        assert!(
            !directory.exists(),
            "our empty version directory must not block retries"
        );
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("my-notes.txt"), b"user notes").unwrap();
        assert!(commit_profile(
            root.path(),
            id,
            &json!({"id":id}),
            &[bad],
            &AtomicBool::new(false)
        )
        .is_err());
        assert_eq!(
            fs::read(directory.join("my-notes.txt")).unwrap(),
            b"user notes"
        );
        commit_profile(
            root.path(),
            id,
            &json!({"id":id}),
            &[],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(directory.join(format!("{id}.json")).is_file());
    }

    #[test]
    fn cancellation_after_native_writes_rolls_back_only_recorded_paths() {
        for concurrent_user_file in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source");
            let target = root.path().join("natives");
            fs::create_dir_all(source.join("nested")).unwrap();
            fs::write(source.join("nested/library.dll"), b"native library").unwrap();
            let cancel = AtomicBool::new(false);
            let mut created = Vec::new();
            commit_native_tree_new(&source, &target, &cancel, &mut created).unwrap();
            assert!(target.join("nested/library.dll").exists());
            if concurrent_user_file {
                fs::write(target.join("nested/user.txt"), b"keep user data").unwrap();
            }
            // This is the cancellation boundary immediately before committing the profile JSON.
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            assert!(cancelled(&cancel).is_err());
            let result = rollback_created_paths(&created);
            assert!(!target.join("nested/library.dll").exists());
            assert!(
                source.join("nested/library.dll").exists(),
                "unlink must preserve its source"
            );
            if concurrent_user_file {
                assert!(
                    result.is_err(),
                    "unknown files must keep their containing directory"
                );
                assert_eq!(
                    fs::read(target.join("nested/user.txt")).unwrap(),
                    b"keep user data"
                );
            } else {
                result.unwrap();
                assert!(!target.exists());
                cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                commit_native_tree_new(&source, &target, &cancel, &mut Vec::new()).unwrap();
            }
        }
    }

    #[test]
    fn pack_with_same_loader_preserves_user_profile_and_inherits_user_arguments() {
        let (root, platform, loader_id) = existing_custom_profile();
        let pack = root.path().join("test.mrpack");
        let mut zip = ZipWriter::new(fs::File::create(&pack).unwrap());
        zip.start_file("modrinth.index.json", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(json!({"formatVersion":1,"game":"minecraft","versionId":"1","name":"Existing Loader Pack","files":[],"dependencies":{"minecraft":"1.21.1","fabric-loader":"0.19.5"}}).to_string().as_bytes()).unwrap();
        zip.start_file("overrides/config/pack.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"pack config").unwrap();
        zip.finish().unwrap();
        let before = snapshot(root.path());
        assert_eq!(
            crate::packs::install_pack(
                root.path(),
                &pack,
                "new-pack",
                false,
                &platform,
                &AtomicBool::new(false),
                |_| {}
            )
            .unwrap(),
            "new-pack"
        );
        let after = snapshot(root.path());
        for (path, bytes) in before {
            assert_eq!(after[&path], bytes, "{}", path.display());
        }
        let resolved = resolve_version(root.path(), "new-pack").unwrap();
        assert!(resolved["arguments"]["jvm"]
            .as_array()
            .unwrap()
            .contains(&json!("-Dmy.user.flag=true")));
        assert_eq!(resolved["inheritsFrom"], loader_id);
        assert_eq!(
            fs::read(root.path().join("versions/new-pack/natives/libfixture.bin")).unwrap(),
            b"native!"
        );
    }

    fn fabric_fixture() -> Value {
        // Structural fields copied from the official 1.21.1 / 0.19.5 profile.
        json!({
            "id":"fabric-loader-0.19.5-1.21.1", "inheritsFrom":"1.21.1", "type":"release",
            "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
            "arguments":{"game":[],"jvm":["-DFabricMcEmu= net.minecraft.client.main.Main "]},
            "libraries":[{"name":"net.fabricmc:fabric-loader:0.19.5","url":"https://maven.fabricmc.net/"}]
        })
    }

    #[test]
    fn profile_must_match_selected_loader_and_preserve_vanilla_id() {
        let mut value = fabric_fixture();
        assert_eq!(
            validate_profile(LoaderKind::Fabric, "1.21.1", "0.19.5", &value).unwrap(),
            "fabric-loader-0.19.5-1.21.1"
        );
        assert!(validate_profile(LoaderKind::Quilt, "1.21.1", "0.19.5", &value).is_err());
        assert!(validate_profile(LoaderKind::Fabric, "1.21.1", "0.18.0", &value).is_err());
        value["id"] = json!("1.21.1");
        assert!(validate_profile(LoaderKind::Fabric, "1.21.1", "0.19.5", &value).is_err());
        value["id"] = json!("../../escape");
        assert!(validate_profile(LoaderKind::Fabric, "1.21.1", "0.19.5", &value).is_err());
    }

    #[test]
    fn api_versions_handle_quilt_stability_and_numeric_order() {
        let parsed = versions(
            &json!([
                {"loader":{"version":"0.29.0-beta.9"}},
                {"loader":{"version":"0.29.0-beta.10"}},
                {"loader":{"version":"0.29.0"}}
            ]),
            true,
        )
        .unwrap();
        assert!(!parsed[0].stable);
        assert!(parsed[2].stable);
        assert!(compare_loader_versions("0.29.0-beta.10", "0.29.0-beta.9").is_gt());
        assert!(compare_loader_versions("0.29.0", "0.29.0-beta.10").is_gt());
        assert!(compare_loader_versions("0.30.0", "0.9.0").is_gt());
        assert!(endpoint(
            LoaderKind::Fabric,
            &[
                "loader",
                "1.14 Pre-Release 5",
                "0.4.2+build.132",
                "profile",
                "json"
            ]
        )
        .unwrap()
        .contains("1.14%20Pre-Release%205"));
    }

    #[test]
    fn checksum_parser_rejects_missing_or_forged_digest() {
        let digest = "ff9e65cffca4a67f31523e1807fe0855940fcbfa";
        assert_eq!(
            parse_checksum(format!("{digest}\n").as_bytes()).unwrap(),
            digest
        );
        assert_eq!(
            parse_checksum(format!("{digest}  fabric-loader.jar\n").as_bytes()).unwrap(),
            digest
        );
        assert!(parse_checksum(b"<html>not found</html>").is_err());
        assert!(parse_checksum(b"").is_err());
    }

    #[test]
    fn quilt_official_profile_preserves_both_mapping_coordinates() {
        // Standard fields from the official v3 / 1.21.1 / 0.20.0-beta.9 profile.
        let profile = json!({
            "id":"quilt-loader-0.20.0-beta.9-1.21.1", "inheritsFrom":"1.21.1", "type":"release",
            "mainClass":"org.quiltmc.loader.impl.launch.knot.KnotClient", "arguments":{"game":[]},
            "libraries":[
                {"name":"org.quiltmc:quilt-loader:0.20.0-beta.9","url":"https://maven.quiltmc.org/repository/release/"},
                {"name":"org.quiltmc:hashed:1.21.1","url":"https://maven.quiltmc.org/repository/release/"},
                {"name":"net.fabricmc:intermediary:1.21.1","url":"https://maven.fabricmc.net/"}
            ]
        });
        validate_profile(LoaderKind::Quilt, "1.21.1", "0.20.0-beta.9", &profile).unwrap();
        let artifacts = library_artifacts(&profile, &Platform::current()).unwrap();
        assert_eq!(artifacts.len(), 3);
        assert_eq!(
            artifacts[1].url,
            "https://maven.quiltmc.org/repository/release/org/quiltmc/hashed/1.21.1/hashed-1.21.1.jar"
        );
        assert_eq!(
            artifacts[2].url,
            "https://maven.fabricmc.net/net/fabricmc/intermediary/1.21.1/intermediary-1.21.1.jar"
        );
        assert!(artifacts
            .iter()
            .all(|artifact| validate_url(&artifact.url).is_ok()));
    }

    #[test]
    fn cancelled_profile_commit_preserves_existing_version() {
        let root = tempfile::tempdir().unwrap();
        let id = "fabric-loader-0.19.5-1.21.1";
        let folder = root.path().join("versions").join(id);
        fs::create_dir_all(&folder).unwrap();
        let file = folder.join(format!("{id}.json"));
        fs::write(&file, "user-existing").unwrap();
        assert!(commit_profile(
            root.path(),
            id,
            &fabric_fixture(),
            &[],
            &AtomicBool::new(true)
        )
        .is_err());
        assert_eq!(fs::read_to_string(file).unwrap(), "user-existing");
    }

    #[test]
    fn standard_profile_inherits_client_arguments_and_logging_without_changing_parent() {
        let root = tempfile::tempdir().unwrap();
        let parent_dir = root.path().join("versions/1.21.1");
        fs::create_dir_all(&parent_dir).unwrap();
        let parent = json!({"id":"1.21.1","mainClass":"net.minecraft.client.main.Main", "downloads":{"client":{}},
            "arguments":{"jvm":["-cp","${classpath}"],"game":["--username","${auth_player_name}"]},
            "logging":{"client":{"argument":"-Dlog4j.configurationFile=${path}","file":{"id":"client-1.12.xml"}}}}).to_string();
        fs::write(parent_dir.join("1.21.1.json"), &parent).unwrap();
        fs::write(parent_dir.join("1.21.1.jar"), b"vanilla client").unwrap();
        let mut profile = fabric_fixture();
        let artifact = library_artifacts(&profile, &Platform::current())
            .unwrap()
            .remove(0);
        let library = safe_target(root.path(), &artifact.relative_path).unwrap();
        fs::create_dir_all(library.parent().unwrap()).unwrap();
        fs::write(&library, b"fixture loader").unwrap();
        profile["libraries"][0]["downloads"] = json!({"artifact":{"path":"net/fabricmc/fabric-loader/0.19.5/fabric-loader-0.19.5.jar","url":artifact.url,"sha1":format!("{:x}",Sha1::digest(b"fixture loader")),"size":14}});
        fs::create_dir_all(root.path().join("assets/log_configs")).unwrap();
        fs::write(
            root.path().join("assets/log_configs/client-1.12.xml"),
            b"<Configuration/>",
        )
        .unwrap();
        fs::write(root.path().join("java"), b"fixture executable").unwrap();
        let id = "fabric-loader-0.19.5-1.21.1";
        commit_profile(root.path(), id, &profile, &[], &AtomicBool::new(false)).unwrap();
        let plan = build_plan(
            &LaunchOptions {
                root: root.path().into(),
                version_id: id.into(),
                java: root.path().join("java"),
                memory_mb: 2048,
                width: 854,
                height: 480,
            },
            &crate::model::Session {
                access_token: "FIXTURE_TOKEN".into(),
                user_type: "msa".into(),
                ..auth::offline_session("Player").unwrap()
            },
            &Platform::current(),
        )
        .unwrap();
        let main = plan
            .args
            .iter()
            .position(|arg| arg == "net.fabricmc.loader.impl.launch.knot.KnotClient")
            .unwrap();
        assert!(plan.args[..main]
            .iter()
            .any(|arg| arg.starts_with("-Dlog4j.configurationFile=")));
        assert!(plan.args[..main]
            .iter()
            .any(|arg| arg.contains("1.21.1.jar")));
        assert!(plan.args[main + 1..].contains(&"--username".into()));
        assert_eq!(
            fs::read_to_string(parent_dir.join("1.21.1.json")).unwrap(),
            parent
        );
        assert!(!root.path().join(format!("versions/{id}/{id}.jar")).exists());
    }

    #[test]
    #[ignore = "Explicit live integration: installs only Fabric 0.19.5 for an already installed 1.21.1; never launches Java"]
    fn live_fabric_install_for_existing_1_21_1() {
        let root =
            std::env::var_os("PCL_LOADER_TEST_ROOT").expect("set PCL_LOADER_TEST_ROOT explicitly");
        let root = PathBuf::from(root);
        let parent = root.join("versions/1.21.1/1.21.1.json");
        let before = fs::read(&parent).unwrap();
        let id = install_loader(
            &root,
            LoaderKind::Fabric,
            "1.21.1",
            "0.19.5",
            &Platform::current(),
            &AtomicBool::new(false),
            |event| eprintln!("{} / {} {}", event.completed, event.total, event.message),
        )
        .unwrap();
        assert_eq!(id, "fabric-loader-0.19.5-1.21.1");
        assert_eq!(fs::read(parent).unwrap(), before);
        let resolved = crate::metadata::resolve_version(&root, &id).unwrap();
        assert_eq!(resolved["_pcl_jar_id"], "1.21.1");
        assert_eq!(
            resolved["mainClass"],
            "net.fabricmc.loader.impl.launch.knot.KnotClient"
        );
        let version: Value = serde_json::from_slice(
            &fs::read(root.join(format!("versions/{id}/{id}.json"))).unwrap(),
        )
        .unwrap();
        for artifact in library_artifacts(&version, &Platform::current()).unwrap() {
            assert!(install::cache_valid(
                &safe_target(&root, &artifact.relative_path).unwrap(),
                &artifact,
                &AtomicBool::new(false)
            )
            .unwrap());
        }
    }
}
