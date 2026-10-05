//! Public Modrinth v2 discovery, dependency planning and non-overwriting resource installs.
//! API contract: https://docs.modrinth.com/api/operations/searchprojects/
//! https://docs.modrinth.com/api/operations/getprojectversions/
//! https://docs.modrinth.com/api/operations/versionsfromhashes/
//! Required dependencies are resolved before staged, verified, exclusive commits.
//! Project Markdown is untrusted display content.
use crate::{metadata::validate_id, model::Progress, mods};
use anyhow::{bail, ensure, Context, Result};
use reqwest::{
    blocking::{Client, RequestBuilder, Response},
    redirect::Policy,
    Url,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha512};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

const API: &str = "https://api.modrinth.com/v2/";
const JSON_LIMIT: u64 = 16 * 1024 * 1024;
const ICON_LIMIT: u64 = 2 * 1024 * 1024;
const MOD_LIMIT: u64 = 512 * 1024 * 1024;

/// The API's v2 datapacks share projects with Mods; version loaders and ZIP
/// contents distinguish the files. Never infer an install directory from a title.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceKind {
    #[default]
    Mod,
    Modpack,
    ResourcePack,
    Shader,
    DataPack,
}
impl ResourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mod => "Mod",
            Self::Modpack => "整合包",
            Self::ResourcePack => "资源包",
            Self::Shader => "光影包",
            Self::DataPack => "数据包",
        }
    }
    pub fn web_type(self) -> &'static str {
        match self {
            Self::Mod => "mod",
            Self::Modpack => "modpack",
            Self::ResourcePack => "resourcepack",
            Self::Shader => "shader",
            Self::DataPack => "datapack",
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Mod => ".jar",
            Self::Modpack => ".mrpack",
            _ => ".zip",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceProvider {
    #[default]
    Modrinth,
    CurseForge,
}
impl ResourceProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Modrinth => "Modrinth",
            Self::CurseForge => "CurseForge",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchOptions {
    #[serde(default)]
    pub provider: ResourceProvider,
    pub query: String,
    pub minecraft: Option<String>,
    pub loader: Option<String>,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchPage {
    pub hits: Vec<ProjectHit>,
    pub offset: u32,
    pub limit: u32,
    pub total_hits: u64,
}

fn nullable_string<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> std::result::Result<String, D::Error> {
    Ok(Option::<String>::deserialize(de)?.unwrap_or_default())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectHit {
    pub project_id: String,
    #[serde(default, deserialize_with = "nullable_string")]
    pub slug: String,
    pub title: String,
    pub description: String,
    pub author: String,
    pub downloads: u64,
    #[serde(default)]
    pub categories: Vec<String>,
    pub icon_url: Option<String>,
    pub date_modified: String,
    #[serde(default)]
    pub versions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModrinthProject {
    pub id: String,
    #[serde(default, deserialize_with = "nullable_string")]
    pub slug: String,
    pub title: String,
    pub description: String,
    pub body: String,
    pub project_type: String,
    pub icon_url: Option<String>,
    #[serde(default)]
    pub client_side: String,
    #[serde(default)]
    pub server_side: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    pub downloads: u64,
    pub updated: String,
    pub source_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModrinthVersion {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    pub version_type: String,
    pub date_published: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub files: Vec<VersionFile>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub environment: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VersionFile {
    pub hashes: BTreeMap<String, String>,
    pub url: String,
    pub filename: String,
    pub primary: bool,
    pub size: u64,
    pub file_type: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dependency {
    pub version_id: Option<String>,
    pub project_id: Option<String>,
    pub file_name: Option<String>,
    pub dependency_type: String,
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(crate::model::OperationCancelled.into());
    }
    Ok(())
}

fn trusted(url: &Url, host: &str) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(host)
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && (host == "api.modrinth.com" || url.query().is_none())
}

fn client(host: &'static str) -> Result<Client> {
    Ok(Client::builder()
        .user_agent(concat!("PCL-Rust-thirdparty/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .redirect(Policy::custom(move |attempt| {
            if attempt.previous().len() >= 3 || !trusted(attempt.url(), host) {
                attempt.error("拒绝不可信的 Modrinth 重定向")
            } else {
                attempt.follow()
            }
        }))
        .build()?)
}

fn send(request: RequestBuilder, cancel: &AtomicBool) -> Result<Response> {
    cancelled(cancel)?;
    let response = request.send().map_err(|error| {
        // Never return a request URL, proxy credentials, or a remote error body.
        anyhow::anyhow!(if error.is_timeout() {
            "Modrinth 请求超时"
        } else {
            "Modrinth HTTPS 请求失败"
        })
    })?;
    cancelled(cancel)?;
    if !response.status().is_success() {
        if response.status().as_u16() == 429 {
            bail!("Modrinth 请求频率受限，请稍后重试");
        }
        bail!("Modrinth 返回 HTTP {}", response.status().as_u16());
    }
    Ok(response)
}

fn read_limited(mut reader: impl Read, limit: u64, cancel: &AtomicBool) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut buffer = [0u8; 32 * 1024];
    loop {
        cancelled(cancel)?;
        let count = reader
            .read(&mut buffer)
            .map_err(|error| anyhow::anyhow!("读取 Modrinth 数据失败（{:?}）", error.kind()))?;
        if count == 0 {
            break;
        }
        ensure!(
            result.len() as u64 + count as u64 <= limit,
            "Modrinth 响应超过大小限制"
        );
        result.extend_from_slice(&buffer[..count]);
    }
    cancelled(cancel)?;
    Ok(result)
}

fn json<T: DeserializeOwned>(request: RequestBuilder, cancel: &AtomicBool) -> Result<T> {
    let response = send(request.timeout(Duration::from_secs(30)), cancel)?;
    let bytes = read_limited(response, JSON_LIMIT, cancel)?;
    serde_json::from_slice(&bytes).context("Modrinth 返回的数据格式无效")
}

fn api_url(segments: &[&str]) -> Result<Url> {
    let mut url = Url::parse(API)?;
    let mut path = url
        .path_segments_mut()
        .map_err(|_| anyhow::anyhow!("API 地址无效"))?;
    path.pop_if_empty();
    for segment in segments {
        ensure!(
            !segment.is_empty() && segment.len() <= 128 && !segment.chars().any(char::is_control),
            "Modrinth 标识无效"
        );
        // Url encodes path segments; an ID cannot add a query, authority or path.
        ensure!(
            !matches!(*segment, "." | "..") && !segment.contains(['/', '\\']),
            "Modrinth 标识无效"
        );
        path.push(segment);
    }
    drop(path);
    Ok(url)
}

fn validate_filter(minecraft: &str, loader: &str) -> Result<()> {
    ensure!(
        !minecraft.is_empty()
            && minecraft.len() <= 64
            && minecraft
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-+".contains(c)),
        "Minecraft 版本过滤条件无效"
    );
    ensure!(
        matches!(loader, "fabric" | "quilt" | "forge" | "neoforge"),
        "请选择 Fabric、Quilt、Forge 或 NeoForge"
    );
    Ok(())
}

#[cfg(test)]
fn search_url(options: &SearchOptions) -> Result<Url> {
    resource_search_url(ResourceKind::Mod, options, "")
}

fn resource_search_url(kind: ResourceKind, options: &SearchOptions, category: &str) -> Result<Url> {
    ensure!(
        options.query.len() <= 512 && !options.query.chars().any(char::is_control),
        "搜索词过长或含控制字符"
    );
    ensure!(
        (1..=100).contains(&options.limit) && options.offset <= 1_000_000,
        "搜索分页参数无效"
    );
    let facet = match kind {
        ResourceKind::Mod => "project_type:mod",
        ResourceKind::Modpack => "project_type:modpack",
        ResourceKind::DataPack => "all_project_types:datapack",
        ResourceKind::ResourcePack => "all_project_types:resourcepack",
        ResourceKind::Shader => "all_project_types:shader",
    };
    let mut facets = vec![vec![facet.to_owned()]];
    ensure!(
        category.len() <= 64
            && category
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_+".contains(&c)),
        "资源类型筛选条件无效"
    );
    if !category.is_empty() {
        facets.push(vec![format!("categories:{category}")]);
    }
    if let Some(mc) = &options.minecraft {
        validate_filter(mc, options.loader.as_deref().unwrap_or("fabric"))?;
        facets.push(vec![format!("versions:{mc}")]);
    }
    if let Some(loader) = &options.loader {
        ensure!(
            matches!(kind, ResourceKind::Mod | ResourceKind::Modpack),
            "此资源类别不使用 Mod 加载器筛选"
        );
        validate_filter(options.minecraft.as_deref().unwrap_or("1.0"), loader)?;
        facets.push(vec![format!("categories:{loader}")]);
    }
    let mut url = api_url(&["search"])?;
    url.query_pairs_mut()
        .append_pair(
            "query",
            &crate::wiki::search_query(ResourceProvider::Modrinth, &options.query),
        )
        .append_pair("facets", &serde_json::to_string(&facets)?)
        .append_pair("index", "relevance")
        .append_pair("offset", &options.offset.to_string())
        .append_pair("limit", &options.limit.to_string());
    Ok(url)
}

pub fn search_mods(options: &SearchOptions, cancel: &AtomicBool) -> Result<SearchPage> {
    search_resources(ResourceKind::Mod, options, "", cancel)
}

/// Search one resource class; a category is an additional AND facet.
pub fn search_resources(
    kind: ResourceKind,
    options: &SearchOptions,
    category: &str,
    cancel: &AtomicBool,
) -> Result<SearchPage> {
    if options.provider == ResourceProvider::CurseForge {
        return crate::curseforge::search(kind, options, category, cancel);
    }
    json(
        client("api.modrinth.com")?.get(resource_search_url(kind, options, category)?),
        cancel,
    )
}

pub fn get_project(id: &str, cancel: &AtomicBool) -> Result<ModrinthProject> {
    if id.starts_with("cf:") {
        return crate::curseforge::get_project(crate::curseforge::project_id(id)?, cancel);
    }
    json(
        client("api.modrinth.com")?.get(api_url(&["project", id])?),
        cancel,
    )
}

pub fn list_versions(
    project: &str,
    minecraft: &str,
    loader: &str,
    cancel: &AtomicBool,
) -> Result<Vec<ModrinthVersion>> {
    list_resource_versions(ResourceKind::Mod, project, minecraft, loader, cancel)
}

pub fn list_resource_versions(
    kind: ResourceKind,
    project: &str,
    minecraft: &str,
    loader: &str,
    cancel: &AtomicBool,
) -> Result<Vec<ModrinthVersion>> {
    validate_filter(
        if minecraft.is_empty() {
            "1.0"
        } else {
            minecraft
        },
        if loader.is_empty() { "fabric" } else { loader },
    )?;
    if project.starts_with("cf:") {
        return crate::curseforge::list_files(
            crate::curseforge::project_id(project)?,
            kind,
            minecraft,
            loader,
            cancel,
        );
    }
    let resolved = get_project(project, cancel)?;
    ensure!(project_matches(kind, &resolved), "此项目不属于所选资源类别");
    let project = resolved.id.as_str();
    let mut url = api_url(&["project", project, "version"])?;
    let filters: Vec<&str> = match kind {
        ResourceKind::DataPack => vec!["datapack"],
        ResourceKind::ResourcePack => vec!["minecraft"],
        ResourceKind::Shader => vec!["iris", "optifine", "vanilla"],
        _ if !loader.is_empty() => vec![loader],
        _ => vec![],
    };
    if !filters.is_empty() {
        url.query_pairs_mut()
            .append_pair("loaders", &serde_json::to_string(&filters)?);
    }
    if !minecraft.is_empty() {
        url.query_pairs_mut()
            .append_pair("game_versions", &serde_json::to_string(&[minecraft])?);
    }
    url.query_pairs_mut()
        .append_pair("include_changelog", "false");
    let versions: Vec<ModrinthVersion> = json(client("api.modrinth.com")?.get(url), cancel)?;
    ensure!(
        versions.iter().all(|v| v.project_id == project),
        "资源版本列表含其他项目的文件"
    );
    Ok(versions
        .into_iter()
        .filter(|v| {
            resource_compatible(kind, v, minecraft, loader) && resource_file(kind, v).is_ok()
        })
        .collect())
}

pub fn resource_compatible(
    kind: ResourceKind,
    version: &ModrinthVersion,
    minecraft: &str,
    loader: &str,
) -> bool {
    if !minecraft.is_empty() && !version.game_versions.iter().any(|v| v == minecraft) {
        return false;
    }
    match kind {
        ResourceKind::Mod => browsable(version, minecraft, loader),
        ResourceKind::Modpack => {
            (loader.is_empty() || version.loaders.iter().any(|v| v == loader))
                && !matches!(
                    version.environment.as_deref(),
                    Some("dedicated_server_only" | "server_only")
                )
        }
        ResourceKind::ResourcePack => version.loaders.iter().any(|v| v == "minecraft"),
        ResourceKind::Shader => version
            .loaders
            .iter()
            .any(|v| matches!(v.as_str(), "iris" | "optifine" | "vanilla")),
        ResourceKind::DataPack => version.loaders.iter().any(|v| v == "datapack"),
    }
}

#[cfg(test)]
fn compatible(version: &ModrinthVersion, minecraft: &str, loader: &str) -> bool {
    !minecraft.is_empty() && !loader.is_empty() && browsable(version, minecraft, loader)
}

fn browsable(version: &ModrinthVersion, minecraft: &str, loader: &str) -> bool {
    (minecraft.is_empty() || version.game_versions.iter().any(|v| v == minecraft))
        && (loader.is_empty() || version.loaders.iter().any(|v| v == loader))
        && version
            .loaders
            .iter()
            .any(|v| matches!(v.as_str(), "fabric" | "quilt" | "forge" | "neoforge"))
        && !matches!(
            version.environment.as_deref(),
            Some("server_only" | "dedicated_server_only")
        )
}

#[cfg(test)]
fn primary_file(version: &ModrinthVersion) -> Result<&VersionFile> {
    resource_file(ResourceKind::Mod, version)
}

pub(crate) fn resource_file(kind: ResourceKind, version: &ModrinthVersion) -> Result<&VersionFile> {
    ensure!(
        version.files.iter().filter(|f| f.primary).count() <= 1,
        "Mod 版本含多个主文件"
    );
    let file = version
        .files
        .iter()
        .find(|f| f.primary)
        .or_else(|| version.files.first())
        .context("Mod 版本没有文件")?;
    validate_id(&file.filename)?;
    ensure!(
        file.filename.len() <= 240
            && file.filename.to_ascii_lowercase().ends_with(
                if kind == ResourceKind::Modpack && version.id.starts_with("cf:") {
                    ".zip"
                } else {
                    kind.extension()
                }
            ),
        "资源下载文件名与所选资源类型不匹配"
    );
    ensure!(
        file.file_type.is_none() || file.file_type.as_deref() == Some("unknown"),
        "Mod 主文件是源码、开发包或附属文件，不能直接安装"
    );
    ensure!(
        file.size > 0 && file.size <= MOD_LIMIT,
        "Mod 文件大小无效或超过 512 MiB"
    );
    let curseforge = version.id.starts_with("cf:");
    for (algorithm, length) in [("sha1", 40), ("sha512", 128)] {
        if curseforge && algorithm == "sha512" && !file.hashes.contains_key(algorithm) {
            continue;
        }
        ensure!(
            file.hashes
                .get(algorithm)
                .is_some_and(|s| s.len() == length && s.bytes().all(|c| c.is_ascii_hexdigit())),
            "Mod 缺少有效的 SHA1/SHA512 哈希"
        );
    }
    let url = Url::parse(&file.url).context("Mod 下载地址无效")?;
    ensure!(
        if curseforge {
            crate::curseforge::trusted_file(&url)
        } else {
            trusted(&url, "cdn.modrinth.com")
        },
        "资源下载仅允许所选平台的官方 HTTPS CDN"
    );
    Ok(file)
}

/// Fetch a small raster icon only. SVG and other active/unsupported formats are rejected.
pub fn fetch_project_icon(url: &str, cancel: &AtomicBool) -> Result<Vec<u8>> {
    let parsed = Url::parse(url).context("图标地址无效")?;
    if parsed.host_str() == Some("media.forgecdn.net") {
        let bytes = crate::curseforge::fetch_icon(url, cancel)?;
        ensure!(raster_icon(&bytes), "图标格式无效");
        return Ok(bytes);
    }
    let url = parsed;
    ensure!(
        trusted(&url, "cdn.modrinth.com"),
        "图标仅允许官方 Modrinth HTTPS CDN"
    );
    let bytes = read_limited(
        send(client("cdn.modrinth.com")?.get(url), cancel)?,
        ICON_LIMIT,
        cancel,
    )?;
    ensure!(
        raster_icon(&bytes),
        "图标不是受支持的 PNG/JPEG/WebP/GIF 图像"
    );
    Ok(bytes)
}

fn raster_icon(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
}

fn hash_local(path: &Path, cancel: &AtomicBool) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= MOD_LIMIT,
        "本地 Mod 文件无效或超过大小限制"
    );
    let mut file = File::open(path)?;
    let mut hash = Sha512::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        cancelled(cancel)?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        ensure!(count <= MOD_LIMIT, "本地 Mod 文件超过大小限制");
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn dependency_matches(dependency: &Dependency, installed: &ModrinthVersion) -> bool {
    if let Some(version) = &dependency.version_id {
        installed.id == *version
            && dependency
                .project_id
                .as_ref()
                .is_none_or(|project| installed.project_id == *project)
    } else if let Some(project) = &dependency.project_id {
        installed.project_id == *project
    } else {
        false
    } // Filename-only external dependencies cannot be reliably identified.
}

#[cfg(test)]
fn dependency_issues(
    version: &ModrinthVersion,
    installed: &[ModrinthVersion],
    minecraft: &str,
    loader: &str,
) -> Vec<String> {
    let mut issues = Vec::new();
    for dep in &version.dependencies {
        let matching = installed.iter().find(|item| dependency_matches(dep, item));
        let label = format!(
            "project={} version={} file={}",
            dep.project_id.as_deref().unwrap_or("未知"),
            dep.version_id.as_deref().unwrap_or("任意兼容版本"),
            dep.file_name.as_deref().unwrap_or("未指定")
        );
        match dep.dependency_type.as_str() {
            "required" if !matching.is_some_and(|item| compatible(item, minecraft, loader)) => {
                issues.push(format!("缺少或未启用必需依赖：{label}"))
            }
            "incompatible" if matching.is_some() => {
                issues.push(format!("已安装不兼容依赖：{label}"))
            }
            "required" | "optional" | "incompatible" | "embedded" => (),
            _ => issues.push(format!("无法识别的依赖类型：{label}")),
        }
    }
    if installed
        .iter()
        .any(|item| item.project_id == version.project_id)
    {
        issues.push("此项目已有启用版本；请先在 Mod 管理中处理旧版本，安装不会自动替换".into());
    }
    issues
}

fn verified_download(
    mut reader: impl Read,
    target: &Path,
    file: &VersionFile,
    cancel: &AtomicBool,
    progress: &impl Fn(Progress),
) -> Result<()> {
    let mut output = File::create(target)?;
    let (mut sha1, mut sha512) = (Sha1::new(), Sha512::new());
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        cancelled(cancel)?;
        let count = reader
            .read(&mut buffer)
            .map_err(|error| anyhow::anyhow!("读取资源下载失败（{:?}）", error.kind()))?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(
            total <= file.size && total <= MOD_LIMIT,
            "资源下载超过声明大小"
        );
        sha1.update(&buffer[..count]);
        sha512.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
        crate::network::throttle(count, cancel)?;
        progress(Progress {
            message: format!("下载 {}", file.filename),
            completed: total,
            total: file.size,
            ..Default::default()
        });
    }
    cancelled(cancel)?;
    ensure!(total == file.size, "资源下载文件大小不匹配");
    ensure!(
        file.hashes.contains_key("sha512")
            || Url::parse(&file.url).is_ok_and(|url| crate::curseforge::trusted_file(&url)),
        "Modrinth 资源缺少 SHA512 校验值"
    );
    ensure!(
        file.hashes
            .get("sha1")
            .is_some_and(|hash| hash.eq_ignore_ascii_case(&format!("{:x}", sha1.finalize())))
            && file
                .hashes
                .get("sha512")
                .is_none_or(|hash| hash.eq_ignore_ascii_case(&format!("{:x}", sha512.finalize()))),
        "资源 SHA1/SHA512 校验失败"
    );
    output.sync_all()?;
    Ok(())
}

#[cfg(test)]
fn install_download(
    instance: &Path,
    file: &VersionFile,
    reader: impl Read,
    cancel: &AtomicBool,
    progress: &impl Fn(Progress),
) -> Result<PathBuf> {
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join(&file.filename);
    verified_download(reader, &source, file, cancel, progress)?;
    cancelled(cancel)?;
    // Reuse local Mod inspection, symlink checks and exclusive persistence.
    let path = mods::import_mod(instance, &source)?;
    progress(Progress {
        message: format!("已安装 {}", file.filename),
        completed: file.size,
        total: file.size,
        ..Default::default()
    });
    Ok(path)
}

/// Installs the primary client JAR into an existing instance. No existing file is
/// replaced; required dependencies are planned and installed in one transaction.
/// Cancellation is checked between reads/requests; an in-flight HTTP call is bounded
/// by the request timeout. Existing project versions are never automatically updated.
pub fn install_version(
    instance: &Path,
    version_id: &str,
    minecraft: &str,
    loader: &str,
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
) -> Result<PathBuf> {
    let plan = plan_mod_install(instance, version_id, minecraft, loader, cancel)?;
    execute_install_plan(&plan, cancel, progress)?;
    plan.primary_path()
}

/// Installation target captured when the user clicks a particular version.
/// `world` is mandatory only for datapacks and must be a direct, real saves child.
pub struct ResourceInstall<'a> {
    pub kind: ResourceKind,
    pub instance: &'a Path,
    pub world: Option<&'a Path>,
    pub project_id: &'a str,
    pub version_id: &'a str,
    pub minecraft: &'a str,
}

fn ordinary_directory(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "资源目标必须是绝对路径");
    let meta = fs::symlink_metadata(path).context("资源目标目录不存在")?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "资源目标必须是普通目录，不能是符号链接"
    );
    Ok(path.canonicalize()?)
}

fn child_directory(parent: &Path, name: &str, create: bool) -> Result<PathBuf> {
    validate_id(name)?;
    let child = parent.join(name);
    match fs::symlink_metadata(&child) {
        Ok(meta) => ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "资源目录不能是文件或符号链接：{}",
            child.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if create {
                fs::create_dir(&child)?;
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(child)
}

fn checked_world(instance: &Path, world: &Path) -> Result<PathBuf> {
    let instance = ordinary_directory(instance)?;
    let saves = child_directory(&instance, "saves", false)?;
    ensure!(saves.is_dir(), "此实例尚没有 saves 世界目录");
    let world = ordinary_directory(world)?;
    ensure!(
        world.parent() == Some(saves.as_path()),
        "必须明确选择当前实例 saves 下的一个世界目录"
    );
    let level = fs::symlink_metadata(world.join("level.dat"))
        .context("选择的目录没有 level.dat，不是已有世界")?;
    ensure!(
        level.is_file() && !level.file_type().is_symlink(),
        "世界 level.dat 必须是普通文件"
    );
    Ok(world)
}

pub fn list_worlds(instance: &Path) -> Result<Vec<PathBuf>> {
    let instance = ordinary_directory(instance)?;
    let saves = child_directory(&instance, "saves", false)?;
    if !saves.exists() {
        return Ok(vec![]);
    }
    let mut worlds = Vec::new();
    for entry in fs::read_dir(saves)? {
        let path = entry?.path();
        if checked_world(&instance, &path).is_ok() {
            worlds.push(path);
        }
    }
    worlds.sort();
    Ok(worlds)
}

fn archive_directory(
    request: &ResourceInstall<'_>,
    vanilla_shader: bool,
    create: bool,
) -> Result<PathBuf> {
    let instance = ordinary_directory(request.instance)?;
    let (parent, name) = match request.kind {
        ResourceKind::ResourcePack => (instance, "resourcepacks"),
        ResourceKind::Shader if vanilla_shader => (instance, "resourcepacks"),
        ResourceKind::Shader => (instance, "shaderpacks"),
        ResourceKind::DataPack => (
            checked_world(
                &instance,
                request.world.context("请先明确选择安装数据包的世界")?,
            )?,
            "datapacks",
        ),
        _ => bail!("此类别不能作为资源 ZIP 安装"),
    };
    child_directory(&parent, name, create)
}

fn no_conflict(directory: &Path, filename: &str) -> Result<()> {
    if directory.exists() {
        for item in fs::read_dir(directory)? {
            ensure!(
                !item?
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(filename),
                "同名资源已存在，未覆盖原文件：{filename}"
            );
        }
    }
    Ok(())
}

fn inspect_archive(
    kind: ResourceKind,
    path: &Path,
    vanilla_shader: bool,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(path)?).context("下载文件不是有效 ZIP")?;
    ensure!(archive.len() <= 100_000, "资源 ZIP 条目过多");
    let mut metadata = false;
    let mut payload = false;
    let mut bytes = 0u64;
    for i in 0..archive.len() {
        cancelled(cancel)?;
        let mut entry = archive.by_index(i)?;
        ensure!(
            !entry.name().contains('\\') && entry.enclosed_name().is_some(),
            "资源 ZIP 含不安全路径"
        );
        ensure!(
            entry
                .unix_mode()
                .is_none_or(|mode| mode & 0o170000 != 0o120000),
            "资源 ZIP 含符号链接"
        );
        let name = entry.name();
        metadata |= !entry.is_dir()
            && match kind {
                ResourceKind::Modpack => matches!(name, "modrinth.index.json" | "manifest.json"),
                ResourceKind::Shader if !vanilla_shader => name.starts_with("shaders/"),
                _ => name == "pack.mcmeta",
            };
        payload |= !entry.is_dir()
            && match kind {
                ResourceKind::ResourcePack => name.starts_with("assets/"),
                ResourceKind::DataPack => name.starts_with("data/"),
                ResourceKind::Shader if vanilla_shader => {
                    name.starts_with("assets/minecraft/shaders/")
                }
                _ => true,
            };
        // Read, do not extract: verify CRC, bound decompression, and honor cancellation.
        let mut buffer = [0u8; 64 * 1024];
        loop {
            cancelled(cancel)?;
            let n = entry.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            ensure!(
                bytes <= 2 * 1024 * 1024 * 1024,
                "资源 ZIP 解压大小超过 2 GiB"
            );
        }
    }
    ensure!(metadata && payload, "资源 ZIP 缺少所选类别必需的内容");
    Ok(())
}

fn project_matches(kind: ResourceKind, project: &ModrinthProject) -> bool {
    match kind {
        ResourceKind::Mod => project.project_type == "mod" && project.client_side != "unsupported",
        ResourceKind::DataPack => {
            matches!(project.project_type.as_str(), "mod" | "datapack")
                && project.loaders.iter().any(|v| v == "datapack")
        }
        ResourceKind::Shader => {
            project.project_type == "shader"
                || (project.project_type == "resourcepack"
                    && project.loaders.iter().any(|v| v == "vanilla"))
        }
        _ => project.project_type == kind.web_type(),
    }
}

fn resource_version(
    kind: ResourceKind,
    project_id: &str,
    version_id: &str,
    cancel: &AtomicBool,
) -> Result<ModrinthVersion> {
    let version = get_version(version_id, cancel)?;
    ensure!(
        version.id == version_id && version.project_id == project_id,
        "下载版本不属于所选项目"
    );
    let project = get_project(project_id, cancel)?;
    ensure!(
        project.id == project_id && project_matches(kind, &project),
        "项目类别不符合当前资源页面"
    );
    resource_file(kind, &version)?;
    Ok(version)
}

fn archive_dependency_compatible(
    dependency: &Dependency,
    installed: &ModrinthVersion,
    minecraft: &str,
) -> bool {
    dependency_matches(dependency, installed)
        && installed
            .game_versions
            .iter()
            .any(|version| version == minecraft)
}

#[cfg(test)]
fn archive_download(
    request: &ResourceInstall<'_>,
    vanilla_shader: bool,
    file: &VersionFile,
    reader: impl Read,
    cancel: &AtomicBool,
    progress: &impl Fn(Progress),
) -> Result<PathBuf> {
    let directory = archive_directory(request, vanilla_shader, false)?;
    no_conflict(&directory, &file.filename)?;
    let temporary = tempfile::NamedTempFile::new()?;
    verified_download(reader, temporary.path(), file, cancel, progress)?;
    inspect_archive(request.kind, temporary.path(), vanilla_shader, cancel)?;
    cancelled(cancel)?;
    let directory = archive_directory(request, vanilla_shader, true)?;
    no_conflict(&directory, &file.filename)?;
    let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
    std::io::copy(&mut File::open(temporary.path())?, staged.as_file_mut())?;
    staged.as_file().sync_all()?;
    cancelled(cancel)?;
    ensure!(
        archive_directory(request, vanilla_shader, false)? == directory,
        "资源目录在下载期间发生变化"
    );
    let path = directory.join(&file.filename);
    staged
        .persist_noclobber(&path)
        .map_err(|error| error.error)
        .context("资源目标冲突或写入失败，未覆盖原文件")?;
    Ok(path)
}

/// Installs a ZIP without extraction or replacing existing resources. A vanilla
/// core shader is explicitly routed to resourcepacks; Iris/OptiFine to shaderpacks.
pub fn install_resource(
    request: &ResourceInstall<'_>,
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
) -> Result<PathBuf> {
    let plan = plan_resource_install(request, cancel)?;
    execute_install_plan(&plan, cancel, progress)?;
    plan.primary_path()
}

/// A display-only description; callers cannot alter the plan's verified files.
#[derive(Clone, Debug)]
pub struct PlannedResource {
    pub project: String,
    pub version: String,
    pub filename: String,
    pub reused: bool,
}

#[derive(Clone)]
struct PlanContext {
    kind: ResourceKind,
    instance: PathBuf,
    world: Option<PathBuf>,
    minecraft: String,
    loader: String,
}
impl PlanContext {
    fn request<'a>(&'a self, version: &'a ModrinthVersion) -> ResourceInstall<'a> {
        ResourceInstall {
            kind: self.kind,
            instance: &self.instance,
            world: self.world.as_deref(),
            project_id: &version.project_id,
            version_id: &version.id,
            minecraft: &self.minecraft,
        }
    }
    fn directory(&self, version: &ModrinthVersion, create: bool) -> Result<PathBuf> {
        if self.kind == ResourceKind::Mod {
            child_directory(&ordinary_directory(&self.instance)?, "mods", create)
        } else {
            archive_directory(
                &self.request(version),
                vanilla_shader(self.kind, version),
                create,
            )
        }
    }
}
fn vanilla_shader(kind: ResourceKind, version: &ModrinthVersion) -> bool {
    kind == ResourceKind::Shader
        && version.loaders.iter().any(|v| v == "vanilla")
        && !version
            .loaders
            .iter()
            .any(|v| matches!(v.as_str(), "iris" | "optifine"))
}
struct PlannedFile {
    version: ModrinthVersion,
    title: String,
    reused: bool,
}
/// All required versions are resolved before any destination file is written.
/// Existing files are snapshotted and rechecked immediately before commit.
pub struct InstallPlan {
    context: PlanContext,
    primary_project: String,
    ordered: Vec<PlannedFile>,
    existing: BTreeMap<PathBuf, String>,
}
impl InstallPlan {
    fn primary_path(&self) -> Result<PathBuf> {
        let item = self
            .ordered
            .iter()
            .find(|item| item.version.project_id == self.primary_project)
            .context("安装计划缺少主项目")?;
        let file = resource_file(self.context.kind, &item.version)?;
        if item.reused {
            self.existing
                .iter()
                .find(|(_, hash)| file.hashes.get("sha512") == Some(*hash))
                .map(|(path, _)| path.clone())
                .context("已安装的主文件哈希无法确认")
        } else {
            Ok(self
                .context
                .directory(&item.version, false)?
                .join(&file.filename))
        }
    }
    pub fn resources(&self) -> Vec<PlannedResource> {
        self.ordered
            .iter()
            .map(|item| PlannedResource {
                project: item.title.clone(),
                version: item.version.version_number.clone(),
                filename: resource_file(self.context.kind, &item.version)
                    .map(|f| f.filename.clone())
                    .unwrap_or_default(),
                reused: item.reused,
            })
            .collect()
    }
}

trait DependencySource {
    fn version(&mut self, id: &str) -> Result<ModrinthVersion>;
    fn versions(&mut self, project: &str) -> Result<Vec<ModrinthVersion>>;
    fn project(&mut self, id: &str) -> Result<ModrinthProject>;
}
pub fn get_version(id: &str, cancel: &AtomicBool) -> Result<ModrinthVersion> {
    if id.starts_with("cf:") {
        let (p, f) = crate::curseforge::version_id(id)?;
        return crate::curseforge::get_file(p, f, cancel);
    }
    json(
        client("api.modrinth.com")?.get(api_url(&["version", id])?),
        cancel,
    )
}
struct OnlineDependencies<'a> {
    context: &'a PlanContext,
    cancel: &'a AtomicBool,
}
impl DependencySource for OnlineDependencies<'_> {
    fn version(&mut self, id: &str) -> Result<ModrinthVersion> {
        let version = get_version(id, self.cancel)?;
        ensure!(version.id == id, "依赖版本标识不匹配");
        Ok(version)
    }
    fn versions(&mut self, project: &str) -> Result<Vec<ModrinthVersion>> {
        list_resource_versions(
            self.context.kind,
            project,
            &self.context.minecraft,
            &self.context.loader,
            self.cancel,
        )
    }
    fn project(&mut self, id: &str) -> Result<ModrinthProject> {
        get_project(id, self.cancel)
    }
}

#[derive(Debug)]
struct Replan;
impl std::fmt::Display for Replan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "依赖精确版本约束已更新")
    }
}
impl std::error::Error for Replan {}
struct Planner<'a, S> {
    source: &'a mut S,
    context: &'a PlanContext,
    installed: &'a [ModrinthVersion],
    cancel: &'a AtomicBool,
    pins: &'a mut BTreeMap<String, String>,
    pin_owners: &'a mut BTreeMap<String, std::collections::HashSet<Option<(String, String)>>>,
    edges: BTreeMap<(String, String), std::collections::HashSet<(String, String)>>,
    selected: BTreeMap<String, String>,
    visiting: std::collections::HashSet<String>,
    ordered: Vec<PlannedFile>,
}
impl<S: DependencySource> Planner<'_, S> {
    fn discard_constraints_from(&mut self, abandoned: (String, String)) {
        let mut queue = vec![abandoned];
        let mut discarded = std::collections::HashSet::new();
        while let Some(owner) = queue.pop() {
            if discarded.insert(owner.clone()) {
                if let Some(children) = self.edges.get(&owner) {
                    queue.extend(children.iter().cloned());
                }
            }
        }
        for owners in self.pin_owners.values_mut() {
            owners.retain(|owner| {
                owner
                    .as_ref()
                    .is_none_or(|owner| !discarded.contains(owner))
            });
        }
        self.pins.retain(|project, _| {
            self.pin_owners
                .get(project)
                .is_some_and(|owners| !owners.is_empty())
        });
    }
    fn visit(
        &mut self,
        mut version: ModrinthVersion,
        required_pin: bool,
        parent: Option<(String, String)>,
    ) -> Result<()> {
        cancelled(self.cancel)?;
        let project = version.project_id.clone();
        if required_pin {
            if let Some(previous) = self.pins.get(&project) {
                ensure!(
                    previous == &version.id,
                    "项目 {project} 被要求安装两个不同的精确版本：{previous} / {}",
                    version.id
                );
            } else {
                self.pins.insert(project.clone(), version.id.clone());
            }
            self.pin_owners
                .entry(project.clone())
                .or_default()
                .insert(parent.clone());
        } else if let Some(pin) = self.pins.get(&project) {
            if pin != &version.id {
                version = self.source.version(pin)?;
                ensure!(version.project_id == project, "依赖精确版本不属于目标项目");
            }
        }
        if let Some(parent) = parent {
            let effective = if !required_pin {
                self.selected.get(&project).unwrap_or(&version.id)
            } else {
                &version.id
            };
            self.edges
                .entry(parent)
                .or_default()
                .insert((project.clone(), effective.clone()));
        }
        if let Some(selected) = self.selected.get(&project).cloned() {
            if selected != version.id && required_pin {
                self.discard_constraints_from((project.clone(), selected));
                return Err(Replan.into());
            }
            // An unpinned requirement accepts the already selected compatible version.
            ensure!(
                !self.visiting.contains(&project),
                "检测到必需依赖循环：{project}"
            );
            return Ok(());
        }
        ensure!(self.selected.len() < 128, "资源依赖超过 128 项限制");
        ensure!(
            resource_compatible(
                self.context.kind,
                &version,
                &self.context.minecraft,
                &self.context.loader
            ),
            "依赖 {} 不支持目标 Minecraft / 加载器",
            version.name
        );
        resource_file(self.context.kind, &version)?;
        let metadata = self.source.project(&project)?;
        ensure!(
            metadata.id == project && project_matches(self.context.kind, &metadata),
            "依赖 {project} 属于其他资源类别，不能自动写入此目录"
        );
        let directory = self.context.directory(&version, false)?;
        // A dependency cannot silently switch between shaderpacks and resourcepacks.
        if let Some(first) = self.ordered.first() {
            ensure!(
                self.context.directory(&first.version, false)? == directory,
                "依赖跨越不同资源目录，请分别安装"
            );
        }
        let local: Vec<_> = self
            .installed
            .iter()
            .filter(|item| item.project_id == project)
            .collect();
        ensure!(
            local.len() <= 1,
            "本地存在此项目的多个启用版本，请先处理：{}",
            metadata.title
        );
        let reused = if let Some(local) = local.first() {
            ensure!(
                local.id == version.id,
                "本地 {} 的版本 {} 与需要的 {} 冲突；不会替换原文件",
                metadata.title,
                local.version_number,
                version.version_number
            );
            true
        } else {
            false
        };
        self.selected.insert(project.clone(), version.id.clone());
        self.visiting.insert(project.clone());
        for dependency in &version.dependencies {
            match dependency.dependency_type.as_str() {
                "required" => {
                    let (child, pinned) = if let Some(pin) = &dependency.version_id {
                        let child = self.source.version(pin)?;
                        ensure!(
                            dependency
                                .project_id
                                .as_ref()
                                .is_none_or(|id| id == &child.project_id),
                            "依赖精确版本与项目标识不符"
                        );
                        (child, true)
                    } else {
                        let id = dependency
                            .project_id
                            .as_deref()
                            .context("必需依赖只有外部文件名，无法从 Modrinth 自动分发")?;
                        if let Some(pin) = self.pins.get(id) {
                            (self.source.version(pin)?, false)
                        } else if let Some(local) =
                            self.installed.iter().find(|v| v.project_id == id)
                        {
                            ensure!(
                                archive_dependency_compatible(
                                    dependency,
                                    local,
                                    &self.context.minecraft
                                ),
                                "已安装依赖不支持当前 Minecraft，未替换原文件"
                            );
                            (local.clone(), false)
                        } else if let Some(selected) = self.selected.get(id) {
                            (self.source.version(selected)?, false)
                        } else {
                            let mut versions = self.source.versions(id)?;
                            versions.retain(|v| {
                                v.project_id == id
                                    && resource_compatible(
                                        self.context.kind,
                                        v,
                                        &self.context.minecraft,
                                        &self.context.loader,
                                    )
                                    && resource_file(self.context.kind, v).is_ok()
                            });
                            versions.sort_by(|a, b| {
                                b.date_published
                                    .cmp(&a.date_published)
                                    .then(b.id.cmp(&a.id))
                            });
                            (
                                versions.into_iter().next().with_context(|| {
                                    format!("必需依赖 {id} 没有支持当前版本的可自动分发文件")
                                })?,
                                false,
                            )
                        }
                    };
                    self.visit(child, pinned, Some((project.clone(), version.id.clone())))?;
                }
                "optional" | "embedded" | "incompatible" => (),
                _ => bail!("资源含无法识别的依赖类型"),
            }
        }
        self.visiting.remove(&project);
        self.ordered.push(PlannedFile {
            version,
            title: metadata.title,
            reused,
        });
        Ok(())
    }
}

fn build_dependency_plan(
    source: &mut impl DependencySource,
    context: PlanContext,
    root: ModrinthVersion,
    installed: Vec<ModrinthVersion>,
    existing: BTreeMap<PathBuf, String>,
    cancel: &AtomicBool,
) -> Result<InstallPlan> {
    let mut pins = BTreeMap::from([(root.project_id.clone(), root.id.clone())]);
    let mut pin_owners = BTreeMap::from([(
        root.project_id.clone(),
        std::collections::HashSet::from([None]),
    )]);
    for _ in 0..128 {
        let mut planner = Planner {
            source,
            context: &context,
            installed: &installed,
            cancel,
            pins: &mut pins,
            pin_owners: &mut pin_owners,
            edges: BTreeMap::new(),
            selected: BTreeMap::new(),
            visiting: std::collections::HashSet::new(),
            ordered: Vec::new(),
        };
        match planner.visit(root.clone(), true, None) {
            Err(error) if error.is::<Replan>() => continue,
            Err(error) => return Err(error),
            Ok(()) => (),
        }
        let mut all: Vec<&ModrinthVersion> = installed.iter().collect();
        all.extend(planner.ordered.iter().map(|item| &item.version));
        for version in &all {
            for dependency in version
                .dependencies
                .iter()
                .filter(|d| d.dependency_type == "incompatible")
            {
                ensure!(
                    !all.iter().any(|item| dependency_matches(dependency, item)),
                    "不兼容资源：{} 与 {}，未开始下载或写入",
                    version.name,
                    dependency
                        .project_id
                        .as_deref()
                        .or(dependency.version_id.as_deref())
                        .unwrap_or("外部文件")
                );
            }
        }
        let mut filenames = std::collections::HashSet::new();
        if let Some(first) = planner.ordered.first() {
            let directory = context.directory(&first.version, false)?;
            ensure!(
                planner.ordered.iter().all(|item| context
                    .directory(&item.version, false)
                    .is_ok_and(|path| path == directory)),
                "依赖跨越不同资源目录，请分别安装"
            );
        }
        for item in planner.ordered.iter().filter(|v| !v.reused) {
            let file = resource_file(context.kind, &item.version)?;
            ensure!(
                filenames.insert(file.filename.to_lowercase()),
                "不同依赖使用了相同的文件名，未开始下载"
            );
            no_conflict(&context.directory(&item.version, false)?, &file.filename)?;
        }
        let ordered = planner.ordered;
        return Ok(InstallPlan {
            context,
            primary_project: root.project_id,
            ordered,
            existing,
        });
    }
    bail!("无法收敛依赖精确版本约束，未开始下载或写入")
}

fn directory_snapshot(directory: &Path, cancel: &AtomicBool) -> Result<BTreeMap<PathBuf, String>> {
    let mut snapshot = BTreeMap::new();
    if directory.exists() {
        for entry in fs::read_dir(directory)? {
            cancelled(cancel)?;
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if !name.ends_with(".jar")
                && !name.ends_with(".jar.disabled")
                && !name.ends_with(".zip")
            {
                continue;
            }
            let meta = entry.file_type()?;
            ensure!(
                meta.is_file() && !meta.is_symlink(),
                "已有资源不是普通文件，无法安全规划"
            );
            ensure!(snapshot.len() < 1000, "资源数量超过检查限制");
            snapshot.insert(entry.path(), hash_local(&entry.path(), cancel)?);
        }
    }
    Ok(snapshot)
}
fn enabled_snapshot_hashes(snapshot: &BTreeMap<PathBuf, String>) -> Vec<String> {
    snapshot
        .iter()
        .filter(|(path, _)| {
            !path
                .to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(".disabled")
        })
        .map(|(_, hash)| hash.clone())
        .collect()
}
fn snapshot_versions(
    snapshot: &BTreeMap<PathBuf, String>,
    cancel: &AtomicBool,
) -> Result<Vec<ModrinthVersion>> {
    identify_hashes(&enabled_snapshot_hashes(snapshot), cancel)
}
pub(crate) fn identify_hashes(
    hashes: &[String],
    cancel: &AtomicBool,
) -> Result<Vec<ModrinthVersion>> {
    let mut versions = BTreeMap::new();
    let api = client("api.modrinth.com")?;
    for chunk in hashes.chunks(100) {
        let found: BTreeMap<String, ModrinthVersion> = json(
            api.post(api_url(&["version_files"])?)
                .json(&serde_json::json!({"hashes":chunk,"algorithm":"sha512"})),
            cancel,
        )?;
        for (hash, version) in found {
            ensure!(
                chunk.contains(&hash)
                    && version
                        .files
                        .iter()
                        .any(|f| f.hashes.get("sha512") == Some(&hash)),
                "资源哈希查询响应不匹配"
            );
            versions.insert(version.id.clone(), version);
        }
    }
    Ok(versions.into_values().collect())
}
fn prepare_plan(
    context: PlanContext,
    project: Option<&str>,
    version: &str,
    cancel: &AtomicBool,
) -> Result<InstallPlan> {
    cancelled(cancel)?;
    ordinary_directory(&context.instance)?;
    validate_filter(
        &context.minecraft,
        if context.loader.is_empty() {
            "fabric"
        } else {
            &context.loader
        },
    )?;
    let mut source = OnlineDependencies {
        context: &context,
        cancel,
    };
    let root = source.version(version)?;
    ensure!(
        project.is_none_or(|id| id == root.project_id),
        "资源版本不属于所选项目"
    );
    let directory = context.directory(&root, false)?;
    let existing = directory_snapshot(&directory, cancel)?;
    let installed = if root.id.starts_with("cf:") {
        crate::curseforge::identify_files(&existing, context.kind, cancel)?
    } else {
        snapshot_versions(&existing, cancel)?
    };
    let cloned = context.clone();
    build_dependency_plan(&mut source, cloned, root, installed, existing, cancel)
}
pub fn plan_mod_install(
    instance: &Path,
    version: &str,
    minecraft: &str,
    loader: &str,
    cancel: &AtomicBool,
) -> Result<InstallPlan> {
    validate_filter(minecraft, loader)?;
    prepare_plan(
        PlanContext {
            kind: ResourceKind::Mod,
            instance: instance.into(),
            world: None,
            minecraft: minecraft.into(),
            loader: loader.into(),
        },
        None,
        version,
        cancel,
    )
}
pub fn plan_resource_install(
    request: &ResourceInstall<'_>,
    cancel: &AtomicBool,
) -> Result<InstallPlan> {
    ensure!(
        matches!(
            request.kind,
            ResourceKind::DataPack | ResourceKind::ResourcePack | ResourceKind::Shader
        ),
        "该资源类别不能按 ZIP 安装"
    );
    prepare_plan(
        PlanContext {
            kind: request.kind,
            instance: request.instance.into(),
            world: request.world.map(Path::to_path_buf),
            minecraft: request.minecraft.into(),
            loader: String::new(),
        },
        Some(request.project_id),
        request.version_id,
        cancel,
    )
}

fn rollback_new_files(files: &[(PathBuf, String)], created_directory: Option<&Path>) -> Result<()> {
    let mut failed = Vec::new();
    let never_cancel = AtomicBool::new(false);
    for (path, hash) in files.iter().rev() {
        let result = (|| {
            let meta = fs::symlink_metadata(path)?;
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "新增资源已被替换"
            );
            ensure!(
                hash_local(path, &never_cancel)? == *hash,
                "新增资源内容已被其他程序修改"
            );
            fs::remove_file(path)?;
            anyhow::Ok(())
        })();
        if result.is_err() {
            failed.push(path.display().to_string());
        }
    }
    if let Some(directory) = created_directory {
        if fs::remove_dir(directory).is_err() {
            failed.push(directory.display().to_string());
        }
    }
    ensure!(
        failed.is_empty(),
        "本次新增文件未能全部回滚，已保留未知或变更文件：{}",
        failed.join("，")
    );
    Ok(())
}

/// Download everything first, then commit exclusive files. On failure, rollback
/// only tracked new files whose hashes still match what this operation wrote.
fn download_response(file: &VersionFile, cancel: &AtomicBool) -> Result<Response> {
    let url = Url::parse(&file.url)?;
    if crate::curseforge::trusted_file(&url) {
        crate::curseforge::download_file(file, cancel)
    } else {
        ensure!(trusted(&url, "cdn.modrinth.com"), "资源下载地址不可信");
        send(client("cdn.modrinth.com")?.get(url), cancel)
    }
}

pub fn execute_install_plan(
    plan: &InstallPlan,
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
) -> Result<Vec<PathBuf>> {
    execute_plan_with(plan, cancel, &progress, |file, source| {
        let response = download_response(file, cancel)?;
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length == file.size),
            "资源 Content-Length 不匹配"
        );
        verified_download(response, source, file, cancel, &progress)
    })
}
fn execute_plan_with(
    plan: &InstallPlan,
    cancel: &AtomicBool,
    progress: &impl Fn(Progress),
    mut fetch: impl FnMut(&VersionFile, &Path) -> Result<()>,
) -> Result<Vec<PathBuf>> {
    cancelled(cancel)?;
    let stage = tempfile::tempdir()?;
    let mut staged = Vec::new();
    for item in plan.ordered.iter().filter(|item| !item.reused) {
        cancelled(cancel)?;
        let file = resource_file(plan.context.kind, &item.version)?;
        let path = stage.path().join(&file.filename);
        fetch(file, &path)?;
        if plan.context.kind == ResourceKind::Mod {
            // Existing inspector validates JAR metadata without executing the mod.
            mods::import_mod(stage.path(), &path)?;
        } else {
            inspect_archive(
                plan.context.kind,
                &path,
                vanilla_shader(plan.context.kind, &item.version),
                cancel,
            )?;
        }
        staged.push((item, file, path));
    }
    cancelled(cancel)?;
    for (path, hash) in &plan.existing {
        ensure!(
            fs::symlink_metadata(path)?.is_file() && hash_local(path, cancel)? == *hash,
            "已有资源在规划后发生变化，未写入目标目录"
        );
    }
    if plan.context.kind == ResourceKind::Mod {
        let existing = mods::list_mods(&plan.context.instance)?;
        let incoming = mods::list_mods(stage.path())?;
        let mut ids = std::collections::HashSet::new();
        for item in existing
            .iter()
            .filter(|item| item.enabled)
            .chain(incoming.iter())
        {
            ensure!(
                item.error.is_none(),
                "无法检查资源元数据：{}",
                item.file_name
            );
            for id in &item.mod_ids {
                ensure!(
                    ids.insert(id),
                    "已有或待安装 JAR 含重复 Mod ID {id}；未覆盖手动安装文件"
                );
            }
        }
    }
    let mut committed = Vec::new();
    let mut created_directory = None;
    let result = (|| {
        for (item, file, source) in staged {
            cancelled(cancel)?;
            let directory = plan.context.directory(&item.version, false)?;
            match fs::create_dir(&directory) {
                Ok(()) => created_directory = Some(directory.clone()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error.into()),
            }
            ensure!(
                plan.context.directory(&item.version, false)? == directory,
                "资源目录在创建时发生变化"
            );
            no_conflict(&directory, &file.filename)?;
            let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
            let committed_hash = hash_local(&source, cancel)?;
            std::io::copy(&mut File::open(source)?, temporary.as_file_mut())?;
            temporary.as_file().sync_all()?;
            cancelled(cancel)?;
            ensure!(
                plan.context.directory(&item.version, false)? == directory,
                "资源目录在提交前发生变化"
            );
            let path = directory.join(&file.filename);
            temporary
                .persist_noclobber(&path)
                .map_err(|e| e.error)
                .context("资源文件已存在或无法提交，未覆盖原文件")?;
            committed.push((path, committed_hash));
            progress(Progress {
                message: format!("安装 {}", item.title),
                completed: committed.len() as u64,
                total: plan.ordered.iter().filter(|item| !item.reused).count() as u64,
                ..Default::default()
            });
        }
        cancelled(cancel)?;
        anyhow::Ok(())
    })();
    if let Err(error) = result {
        if let Err(cleanup) = rollback_new_files(&committed, created_directory.as_deref()) {
            bail!("资源安装失败：{error:#}；{cleanup:#}");
        }
        return Err(error);
    }
    Ok(committed.into_iter().map(|(path, _)| path).collect())
}

/// Owns the verified temporary .mrpack until the caller finishes packs::install_pack.
pub fn download_modpack(
    project_id: &str,
    version_id: &str,
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
) -> Result<tempfile::NamedTempFile> {
    let version = resource_version(ResourceKind::Modpack, project_id, version_id, cancel)?;
    ensure!(
        resource_compatible(ResourceKind::Modpack, &version, "", ""),
        "此整合包不支持客户端环境"
    );
    let file = resource_file(ResourceKind::Modpack, &version)?;
    let response = download_response(file, cancel)?;
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length == file.size),
        "整合包 Content-Length 不匹配"
    );
    let temporary = tempfile::Builder::new()
        .suffix(if version.id.starts_with("cf:") {
            ".zip"
        } else {
            ".mrpack"
        })
        .tempfile()?;
    verified_download(response, temporary.path(), file, cancel, &progress)?;
    inspect_archive(ResourceKind::Modpack, temporary.path(), false, cancel)?;
    crate::modpack::inspect_mrpack(temporary.path())?;
    Ok(temporary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn resource_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }
    fn zip_version(bytes: &[u8], loader: &str) -> ModrinthVersion {
        let mut v = version(bytes);
        v.files[0].filename = "safe.zip".into();
        v.files[0].url = "https://cdn.modrinth.com/data/project1/versions/version1/safe.zip".into();
        v.loaders = vec![loader.into()];
        v
    }
    #[test]
    fn curseforge_sha1_download_keeps_modrinth_hash_requirements() {
        let bytes = jar();
        let mut v = version(&bytes);
        v.id = "cf:10:20".into();
        v.project_id = "cf:10".into();
        v.files[0].url = "https://edge.forgecdn.net/files/1/2/test.jar".into();
        v.files[0].hashes.remove("sha512");
        assert!(resource_file(ResourceKind::Mod, &v).is_ok());
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("test.jar");
        let cancel = AtomicBool::new(false);
        verified_download(Cursor::new(&bytes), &path, &v.files[0], &cancel, &|_| {}).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(verified_download(
            Cursor::new(vec![0; bytes.len()]),
            &path,
            &v.files[0],
            &cancel,
            &|_| {}
        )
        .is_err());
        v.id = "version1".into();
        v.project_id = "project1".into();
        v.files[0].url = "https://cdn.modrinth.com/data/project1/versions/version1/test.jar".into();
        assert!(resource_file(ResourceKind::Mod, &v).is_err());
        assert!(
            verified_download(Cursor::new(&bytes), &path, &v.files[0], &cancel, &|_| {}).is_err()
        );
    }
    #[test]
    fn disabled_jars_never_satisfy_dependencies_even_with_uppercase_suffix() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("active.jar"),
            dependency_jar("active", "v1"),
        )
        .unwrap();
        fs::write(
            root.path().join("off.jar.disabled"),
            dependency_jar("off", "v1"),
        )
        .unwrap();
        fs::write(
            root.path().join("also-off.jar.DISABLED"),
            dependency_jar("also_off", "v1"),
        )
        .unwrap();
        let snapshot = directory_snapshot(root.path(), &AtomicBool::new(false)).unwrap();
        assert_eq!(snapshot.len(), 3);
        assert_eq!(
            enabled_snapshot_hashes(&snapshot),
            vec![hash_local(&root.path().join("active.jar"), &AtomicBool::new(false)).unwrap()]
        );
    }
    #[test]
    fn archive_dependencies_reject_same_project_for_another_minecraft_version() {
        let dependency = Dependency {
            project_id: Some("project1".into()),
            version_id: None,
            file_name: None,
            dependency_type: "required".into(),
        };
        let mut installed = zip_version(
            &resource_zip(&[("pack.mcmeta", b"{}"), ("data/a/a.json", b"{}")]),
            "datapack",
        );
        installed.game_versions = vec!["1.20.1".into()];
        assert!(dependency_matches(&dependency, &installed));
        assert!(!archive_dependency_compatible(
            &dependency,
            &installed,
            "1.21.1"
        ));
        installed.game_versions.push("1.21.1".into());
        assert!(archive_dependency_compatible(
            &dependency,
            &installed,
            "1.21.1"
        ));
        let pinned = Dependency {
            version_id: Some("different-version".into()),
            ..dependency
        };
        assert!(!archive_dependency_compatible(
            &pinned, &installed, "1.21.1"
        ));
    }
    #[test]
    fn resource_classes_filter_datapacks_and_preserve_primary_file_type() {
        let options = SearchOptions {
            provider: ResourceProvider::Modrinth,
            query: "x & y".into(),
            minecraft: Some("1.21.1".into()),
            loader: None,
            offset: 0,
            limit: 20,
        };
        for (kind, facet) in [
            (ResourceKind::Modpack, "project_type:modpack"),
            (ResourceKind::ResourcePack, "all_project_types:resourcepack"),
            (ResourceKind::Shader, "all_project_types:shader"),
            (ResourceKind::DataPack, "all_project_types:datapack"),
        ] {
            let url = resource_search_url(kind, &options, "adventure").unwrap();
            let pairs: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
            let filters: serde_json::Value = serde_json::from_str(&pairs["facets"]).unwrap();
            assert_eq!(filters[0][0], facet);
            assert_eq!(filters[1][0], "categories:adventure");
        }
        assert!(
            resource_search_url(ResourceKind::DataPack, &options, "x\"],[\"project_type:mod")
                .is_err()
        );
        let v = zip_version(
            &resource_zip(&[("pack.mcmeta", b"{}"), ("data/a/test.json", b"{}")]),
            "datapack",
        );
        assert!(resource_compatible(
            ResourceKind::DataPack,
            &v,
            "1.21.1",
            ""
        ));
        assert!(!resource_compatible(ResourceKind::Mod, &v, "1.21.1", ""));
        assert!(!resource_compatible(
            ResourceKind::ResourcePack,
            &v,
            "1.21.1",
            ""
        ));
        assert!(!resource_compatible(
            ResourceKind::DataPack,
            &v,
            "1.20.1",
            ""
        ));
        assert!(resource_file(ResourceKind::DataPack, &v).is_ok());
        assert!(resource_file(ResourceKind::Mod, &v).is_err());
        assert!(resource_file(ResourceKind::Modpack, &v).is_err());
    }
    #[test]
    fn zip_install_routes_to_selected_instance_and_world_without_replacing_files() {
        let root = tempfile::tempdir().unwrap();
        let instance = root.path().join("game");
        fs::create_dir(&instance).unwrap();
        let world = instance.join("saves/Chosen World");
        fs::create_dir_all(&world).unwrap();
        fs::write(world.join("level.dat"), b"fixture world marker").unwrap();
        let other = instance.join("saves/Other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("level.dat"), b"other").unwrap();
        let cancel = AtomicBool::new(false);
        for (kind, vanilla, entries, directory) in [
            (
                ResourceKind::DataPack,
                false,
                vec![
                    ("pack.mcmeta", b"{}".as_slice()),
                    ("data/a/test.json", b"{}".as_slice()),
                ],
                world.join("datapacks"),
            ),
            (
                ResourceKind::ResourcePack,
                false,
                vec![
                    ("pack.mcmeta", b"{}".as_slice()),
                    ("assets/a/test.json", b"{}".as_slice()),
                ],
                instance.join("resourcepacks"),
            ),
            (
                ResourceKind::Shader,
                false,
                vec![("shaders/test.fsh", b"void main(){}".as_slice())],
                instance.join("shaderpacks"),
            ),
        ] {
            let bytes = resource_zip(&entries);
            let v = zip_version(&bytes, "datapack");
            let request = ResourceInstall {
                kind,
                instance: &instance,
                world: Some(&world),
                project_id: "project1",
                version_id: "version1",
                minecraft: "1.21.1",
            };
            let path = archive_download(
                &request,
                vanilla,
                &v.files[0],
                Cursor::new(&bytes),
                &cancel,
                &|_| {},
            )
            .unwrap();
            assert_eq!(path, directory.canonicalize().unwrap().join("safe.zip"));
            assert_eq!(fs::read(&path).unwrap(), bytes);
            assert!(archive_download(
                &request,
                vanilla,
                &v.files[0],
                Cursor::new(&bytes),
                &cancel,
                &|_| {}
            )
            .is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        assert!(!other.join("datapacks").exists());
        let request = ResourceInstall {
            kind: ResourceKind::DataPack,
            instance: &instance,
            world: None,
            project_id: "p",
            version_id: "v",
            minecraft: "1.21.1",
        };
        assert!(archive_directory(&request, false, true).is_err());
        assert_eq!(list_worlds(&instance).unwrap().len(), 2);
    }
    #[test]
    fn zip_validation_hash_cancel_and_world_escape_fail_before_commit() {
        let root = tempfile::tempdir().unwrap();
        let instance = root.path().join("game");
        fs::create_dir(&instance).unwrap();
        let request = ResourceInstall {
            kind: ResourceKind::ResourcePack,
            instance: &instance,
            world: None,
            project_id: "p",
            version_id: "v",
            minecraft: "1.21.1",
        };
        let bytes = resource_zip(&[("pack.mcmeta", b"{}"), ("assets/a/test.json", b"{}")]);
        let mut v = zip_version(&bytes, "minecraft");
        let cancel = AtomicBool::new(false);
        v.files[0].hashes.insert("sha512".into(), "0".repeat(128));
        assert!(archive_download(
            &request,
            false,
            &v.files[0],
            Cursor::new(&bytes),
            &cancel,
            &|_| {}
        )
        .is_err());
        assert!(!instance.join("resourcepacks").exists());
        let v = zip_version(&bytes, "minecraft");
        let error = archive_download(
            &request,
            false,
            &v.files[0],
            Cursor::new(&bytes),
            &cancel,
            &|_| cancel.store(true, Ordering::Relaxed),
        )
        .unwrap_err();
        assert!(error
            .chain()
            .any(|e| e.is::<crate::model::OperationCancelled>()));
        assert!(!instance.join("resourcepacks").exists());
        cancel.store(false, Ordering::Relaxed);
        for entries in [
            vec![
                ("pack.mcmeta", b"{}".as_slice()),
                ("../assets/x", b"bad".as_slice()),
            ],
            vec![("shaders/a.fsh", b"wrong type".as_slice())],
        ] {
            let bytes = resource_zip(&entries);
            let v = zip_version(&bytes, "minecraft");
            assert!(archive_download(
                &request,
                false,
                &v.files[0],
                Cursor::new(&bytes),
                &cancel,
                &|_| {}
            )
            .is_err());
        }
        let external = root.path().join("external");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("level.dat"), b"x").unwrap();
        fs::create_dir(instance.join("saves")).unwrap();
        assert!(checked_world(&instance, &external).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&external, instance.join("saves/Linked")).unwrap();
            assert!(list_worlds(&instance).unwrap().is_empty());
            std::os::unix::fs::symlink(&external, instance.join("resourcepacks")).unwrap();
            assert!(archive_directory(&request, false, true).is_err());
        }
    }
    #[test]
    fn vanilla_core_shader_uses_resourcepacks_and_mrpack_requires_index() {
        let root = tempfile::tempdir().unwrap();
        let bytes = resource_zip(&[
            ("pack.mcmeta", b"{}"),
            ("assets/minecraft/shaders/test.json", b"{}"),
        ]);
        let v = zip_version(&bytes, "vanilla");
        let request = ResourceInstall {
            kind: ResourceKind::Shader,
            instance: root.path(),
            world: None,
            project_id: "p",
            version_id: "v",
            minecraft: "1.21.1",
        };
        let cancel = AtomicBool::new(false);
        let path = archive_download(
            &request,
            true,
            &v.files[0],
            Cursor::new(&bytes),
            &cancel,
            &|_| {},
        )
        .unwrap();
        assert_eq!(
            path.parent().unwrap(),
            root.path().canonicalize().unwrap().join("resourcepacks")
        );
        assert!(!root.path().join("shaderpacks").exists());
        assert!(inspect_archive(ResourceKind::Modpack, &path, false, &cancel).is_err());
    }

    struct FixtureDependencies {
        versions: BTreeMap<String, ModrinthVersion>,
    }
    impl DependencySource for FixtureDependencies {
        fn version(&mut self, id: &str) -> Result<ModrinthVersion> {
            self.versions
                .get(id)
                .cloned()
                .context("missing fixture version")
        }
        fn versions(&mut self, project: &str) -> Result<Vec<ModrinthVersion>> {
            Ok(self
                .versions
                .values()
                .filter(|v| v.project_id == project)
                .cloned()
                .collect())
        }
        fn project(&mut self, id: &str) -> Result<ModrinthProject> {
            Ok(serde_json::from_value(
                serde_json::json!({"id":id,"slug":id,"title":id,"description":"fixture","body":"","project_type":"mod","icon_url":null,"client_side":"required","server_side":"optional","game_versions":["1.21.1"],"loaders":["fabric"],"downloads":0,"updated":"2026-10-04T00:00:00Z","source_url":null}),
            )?)
        }
    }
    fn dependency(project: &str, pin: Option<&str>) -> Dependency {
        Dependency {
            project_id: Some(project.into()),
            version_id: pin.map(str::to_owned),
            file_name: None,
            dependency_type: "required".into(),
        }
    }
    fn dependency_jar(project: &str, version_id: &str) -> Vec<u8> {
        let metadata = serde_json::to_vec(
            &serde_json::json!({"schemaVersion":1,"id":project,"version":version_id}),
        )
        .unwrap();
        resource_zip(&[("fabric.mod.json", &metadata)])
    }
    fn dependency_version(
        project: &str,
        id: &str,
        dependencies: Vec<Dependency>,
    ) -> ModrinthVersion {
        let bytes = dependency_jar(project, id);
        let mut result = version(&bytes);
        result.id = id.into();
        result.project_id = project.into();
        result.name = project.into();
        result.version_number = id.into();
        result.files[0].filename = format!("{project}-{id}.jar");
        result.dependencies = dependencies;
        result
    }
    fn fixture_plan(
        root: &Path,
        main: ModrinthVersion,
        others: Vec<ModrinthVersion>,
        installed: Vec<ModrinthVersion>,
    ) -> Result<InstallPlan> {
        let mut source = FixtureDependencies {
            versions: others
                .into_iter()
                .chain([main.clone()])
                .map(|v| (v.id.clone(), v))
                .collect(),
        };
        let context = PlanContext {
            kind: ResourceKind::Mod,
            instance: root.into(),
            world: None,
            minecraft: "1.21.1".into(),
            loader: "fabric".into(),
        };
        let snapshot = directory_snapshot(&root.join("mods"), &AtomicBool::new(false))?;
        build_dependency_plan(
            &mut source,
            context,
            main,
            installed,
            snapshot,
            &AtomicBool::new(false),
        )
    }
    fn fetch_fixture(file: &VersionFile, path: &Path, cancel: &AtomicBool) -> Result<()> {
        let stem = file.filename.strip_suffix(".jar").unwrap();
        let (project, version) = stem.split_once('-').unwrap();
        verified_download(
            Cursor::new(dependency_jar(project, version)),
            path,
            file,
            cancel,
            &|_| {},
        )
    }
    #[test]
    fn dependency_plan_topologically_resolves_pins_and_reuses_renamed_installed_file() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("mods")).unwrap();
        let b = dependency_version("b", "b1", vec![]);
        let renamed = root.path().join("mods/user-renamed.jar");
        let bytes = dependency_jar("b", "b1");
        fs::write(&renamed, &bytes).unwrap();
        let a = dependency_version("a", "a1", vec![dependency("b", Some("b1"))]);
        let plan = fixture_plan(root.path(), a, vec![b.clone()], vec![b]).unwrap();
        assert_eq!(
            plan.resources()
                .iter()
                .map(|i| (i.project.as_str(), i.reused))
                .collect::<Vec<_>>(),
            vec![("b", true), ("a", false)]
        );
        let cancel = AtomicBool::new(false);
        let mut downloads = 0;
        let paths = execute_plan_with(&plan, &cancel, &|_| {}, |file, path| {
            downloads += 1;
            fetch_fixture(file, path, &cancel)
        })
        .unwrap();
        assert_eq!(downloads, 1);
        assert_eq!(paths.len(), 1);
        assert_eq!(fs::read(&renamed).unwrap(), bytes);
        assert!(root.path().join("mods/a-a1.jar").is_file());
    }
    #[test]
    fn dependency_plan_rejects_cycle_exact_pin_and_incompatible_conflicts_before_writes() {
        let root = tempfile::tempdir().unwrap();
        let a = dependency_version("a", "a1", vec![dependency("b", None)]);
        let b = dependency_version("b", "b1", vec![dependency("a", None)]);
        assert!(fixture_plan(root.path(), a, vec![b], vec![])
            .unwrap_err_text()
            .contains("循环"));
        let a = dependency_version(
            "a",
            "a1",
            vec![dependency("b", Some("b1")), dependency("c", None)],
        );
        let b1 = dependency_version("b", "b1", vec![]);
        let b2 = dependency_version("b", "b2", vec![]);
        let c = dependency_version("c", "c1", vec![dependency("b", Some("b2"))]);
        assert!(
            fixture_plan(root.path(), a, vec![b1.clone(), b2, c], vec![])
                .unwrap_err_text()
                .contains("精确版本")
        );
        let mut a = dependency_version("a", "a1", vec![dependency("b", None)]);
        let mut incompatible = dependency("a", None);
        incompatible.dependency_type = "incompatible".into();
        let b = dependency_version("b", "b1", vec![incompatible]);
        assert!(fixture_plan(root.path(), a.clone(), vec![b], vec![])
            .unwrap_err_text()
            .contains("不兼容"));
        a.dependencies = vec![dependency("b", Some("b2"))];
        let b2 = dependency_version("b", "b2", vec![]);
        assert!(fixture_plan(root.path(), a, vec![b2], vec![b1])
            .unwrap_err_text()
            .contains("不会替换"));
        assert!(!root.path().join("mods").exists());
    }
    // Avoid requiring Debug on InstallPlan, which contains internal plan snapshots.
    trait PlanError {
        fn unwrap_err_text(self) -> String;
    }
    impl PlanError for Result<InstallPlan> {
        fn unwrap_err_text(self) -> String {
            match self {
                Err(e) => format!("{e:#}"),
                Ok(_) => panic!("expected plan failure"),
            }
        }
    }
    #[test]
    fn dependency_plan_late_exact_pin_replans_an_unpinned_compatible_choice() {
        let root = tempfile::tempdir().unwrap();
        let a = dependency_version(
            "a",
            "a1",
            vec![dependency("b", None), dependency("c", None)],
        );
        let b1 = dependency_version("b", "b1", vec![]);
        let mut b2 = dependency_version("b", "b2", vec![]);
        b2.date_published = "2026-10-05T00:00:00Z".into();
        let c = dependency_version("c", "c1", vec![dependency("b", Some("b1"))]);
        let plan = fixture_plan(root.path(), a, vec![b1, b2, c], vec![]).unwrap();
        assert!(plan
            .resources()
            .iter()
            .any(|item| item.project == "b" && item.version == "b1"));
        assert!(!plan.resources().iter().any(|item| item.version == "b2"));
    }
    #[test]
    fn dependency_replan_discards_pins_from_replaced_transitive_versions() {
        let root = tempfile::tempdir().unwrap();
        let main = dependency_version(
            "root",
            "r1",
            vec![dependency("a", None), dependency("c", None)],
        );
        let a1 = dependency_version("a", "a1", vec![dependency("b", Some("b1"))]);
        let mut a2 = dependency_version("a", "a2", vec![dependency("b", Some("b2"))]);
        a2.date_published = "2026-10-05T00:00:00Z".into();
        let b1 = dependency_version("b", "b1", vec![dependency("d", Some("d1"))]);
        let b2 = dependency_version("b", "b2", vec![dependency("d", Some("d2"))]);
        let d1 = dependency_version("d", "d1", vec![]);
        let d2 = dependency_version("d", "d2", vec![]);
        let c = dependency_version("c", "c1", vec![dependency("a", Some("a1"))]);
        let versions = vec![a1, a2, b1, b2, d1, d2, c];
        let plan = fixture_plan(root.path(), main.clone(), versions.clone(), vec![]).unwrap();
        assert!(plan.resources().iter().any(|item| item.version == "a1"));
        assert!(plan.resources().iter().any(|item| item.version == "b1"));
        assert!(plan.resources().iter().any(|item| item.version == "d1"));
        assert!(!plan
            .resources()
            .iter()
            .any(|item| ["a2", "b2", "d2"].contains(&item.version.as_str())));
        let mut conflicting = main;
        conflicting
            .dependencies
            .insert(0, dependency("d", Some("d2")));
        assert!(fixture_plan(root.path(), conflicting, versions, vec![])
            .unwrap_err_text()
            .contains("精确版本"));
    }
    #[test]
    fn dependency_download_failure_and_cancel_commit_roll_back_only_new_files() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("mods")).unwrap();
        let note = root.path().join("mods/user-note.txt");
        fs::write(&note, b"preserve").unwrap();
        let a = dependency_version("a", "a1", vec![dependency("b", None)]);
        let b = dependency_version("b", "b1", vec![]);
        let plan = fixture_plan(root.path(), a, vec![b], vec![]).unwrap();
        let cancel = AtomicBool::new(false);
        let error = execute_plan_with(&plan, &cancel, &|_| {}, |file, path| {
            if file.filename.starts_with("a-") {
                bail!("fixture network reset");
            }
            fetch_fixture(file, path, &cancel)
        })
        .unwrap_err();
        assert!(error.to_string().contains("network reset"));
        assert_eq!(fs::read_dir(root.path().join("mods")).unwrap().count(), 1);
        let error = execute_plan_with(
            &plan,
            &cancel,
            &|p| {
                if p.completed == 1 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            |file, path| fetch_fixture(file, path, &cancel),
        )
        .unwrap_err();
        assert!(error
            .chain()
            .any(|e| e.is::<crate::model::OperationCancelled>()));
        assert_eq!(fs::read_dir(root.path().join("mods")).unwrap().count(), 1);
        assert_eq!(fs::read(note).unwrap(), b"preserve");
    }
    #[test]
    fn dependency_commit_race_and_manual_mod_id_preserve_unknown_user_files() {
        let root = tempfile::tempdir().unwrap();
        let a = dependency_version("a", "a1", vec![dependency("b", None)]);
        let b = dependency_version("b", "b1", vec![]);
        let plan = fixture_plan(root.path(), a.clone(), vec![b], vec![]).unwrap();
        let cancel = AtomicBool::new(false);
        let error = execute_plan_with(
            &plan,
            &cancel,
            &|p| {
                if p.completed == 1 {
                    fs::write(root.path().join("mods/a-a1.jar"), b"user wins race").unwrap();
                }
            },
            |file, path| fetch_fixture(file, path, &cancel),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("同名"));
        assert_eq!(
            fs::read(root.path().join("mods/a-a1.jar")).unwrap(),
            b"user wins race"
        );
        assert!(!root.path().join("mods/b-b1.jar").exists());
        let manual = tempfile::tempdir().unwrap();
        fs::create_dir(manual.path().join("mods")).unwrap();
        let path = manual.path().join("mods/manual.jar");
        let bytes = dependency_jar("a", "private");
        fs::write(&path, &bytes).unwrap();
        let a = dependency_version("a", "a1", vec![]);
        let plan = fixture_plan(manual.path(), a, vec![], vec![]).unwrap();
        let error = execute_plan_with(&plan, &cancel, &|_| {}, |file, path| {
            fetch_fixture(file, path, &cancel)
        })
        .unwrap_err();
        assert!(error.to_string().contains("重复 Mod ID"));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!manual.path().join("mods/a-a1.jar").exists());
    }

    fn jar() -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(br#"{"schemaVersion":1,"id":"test_mod","name":"Test Mod","version":"1.0"}"#)
            .unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn version(bytes: &[u8]) -> ModrinthVersion {
        ModrinthVersion {
            id: "version1".into(),
            project_id: "project1".into(),
            name: "Test".into(),
            version_number: "1.0".into(),
            version_type: "release".into(),
            date_published: "2026-10-04T00:00:00Z".into(),
            game_versions: vec!["1.21.1".into()],
            loaders: vec!["fabric".into()],
            environment: None,
            dependencies: vec![],
            files: vec![VersionFile {
                hashes: BTreeMap::from([
                    ("sha1".into(), format!("{:x}", Sha1::digest(bytes))),
                    ("sha512".into(), format!("{:x}", Sha512::digest(bytes))),
                ]),
                url: "https://cdn.modrinth.com/data/project1/versions/version1/test.jar".into(),
                filename: "test.jar".into(),
                primary: true,
                size: bytes.len() as u64,
                file_type: None,
            }],
        }
    }

    #[test]
    fn search_encodes_query_and_ands_filters_with_pagination() {
        let options = SearchOptions {
            provider: ResourceProvider::Modrinth,
            query: "sodium & other?token=x".into(),
            minecraft: Some("1.21.1".into()),
            loader: Some("fabric".into()),
            offset: 40,
            limit: 20,
        };
        let url = search_url(&options).unwrap();
        let pairs: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["query"], options.query);
        assert_eq!(pairs["offset"], "40");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&pairs["facets"]).unwrap(),
            serde_json::json!([
                ["project_type:mod"],
                ["versions:1.21.1"],
                ["categories:fabric"]
            ])
        );
        assert!(search_url(&SearchOptions {
            minecraft: Some("1.21.1\"],[\"project_type:modpack".into()),
            ..options
        })
        .is_err());
        assert!(api_url(&["project", "../admin"]).is_err());
    }

    #[test]
    fn download_policy_rejects_untrusted_url_paths_hashes_and_auxiliary_files() {
        let mut v = version(&jar());
        for url in [
            "http://cdn.modrinth.com/test.jar",
            "https://cdn.modrinth.com.evil.test/a.jar",
            "https://token@cdn.modrinth.com/a.jar",
            "https://cdn.modrinth.com/a.jar?token=secret",
            "https://127.0.0.1/a.jar",
        ] {
            v.files[0].url = url.into();
            assert!(primary_file(&v).is_err());
        }
        v = version(&jar());
        v.files[0].filename = "../test.jar".into();
        assert!(primary_file(&v).is_err());
        v = version(&jar());
        v.files[0].hashes.remove("sha512");
        assert!(primary_file(&v).is_err());
        v = version(&jar());
        v.files[0].file_type = Some("sources-jar".into());
        assert!(primary_file(&v).is_err());
        v = version(&jar());
        assert!(browsable(&v, "", ""));
        assert!(browsable(&v, "1.21.1", ""));
        assert!(browsable(&v, "", "fabric"));
        assert!(!compatible(&v, "", ""));
        v.environment = Some("dedicated_server_only".into());
        assert!(!compatible(&v, "1.21.1", "fabric"));
    }

    #[test]
    fn verified_install_reads_metadata_and_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let bytes = jar();
        let v = version(&bytes);
        let cancel = AtomicBool::new(false);
        let target = install_download(
            root.path(),
            &v.files[0],
            Cursor::new(&bytes),
            &cancel,
            &|_| {},
        )
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), bytes);
        let entries = mods::list_mods(root.path()).unwrap();
        assert_eq!(entries[0].mod_ids, vec!["test_mod"]);
        assert!(install_download(
            root.path(),
            &v.files[0],
            Cursor::new(&bytes),
            &cancel,
            &|_| {}
        )
        .is_err());
        assert_eq!(fs::read(&target).unwrap(), bytes);
    }

    #[test]
    fn bad_sha512_size_or_cancel_never_commits() {
        let root = tempfile::tempdir().unwrap();
        let bytes = jar();
        let cancel = AtomicBool::new(false);
        let mut v = version(&bytes);
        v.files[0].hashes.insert("sha512".into(), "0".repeat(128));
        assert!(install_download(
            root.path(),
            &v.files[0],
            Cursor::new(&bytes),
            &cancel,
            &|_| {}
        )
        .is_err());
        v = version(&bytes);
        v.files[0].size += 1;
        assert!(install_download(
            root.path(),
            &v.files[0],
            Cursor::new(&bytes),
            &cancel,
            &|_| {}
        )
        .is_err());
        v = version(&bytes);
        assert!(install_download(
            root.path(),
            &v.files[0],
            Cursor::new(&bytes),
            &cancel,
            &|_| { cancel.store(true, Ordering::Relaxed) }
        )
        .is_err());
        assert!(!root.path().join("mods").exists());
    }

    #[test]
    fn dependency_checks_require_exact_pinned_version_and_compatible_enabled_project() {
        let mut v = version(&jar());
        v.dependencies.push(Dependency {
            version_id: Some("pinned".into()),
            project_id: Some("dep".into()),
            file_name: None,
            dependency_type: "required".into(),
        });
        let mut dependency = version(&jar());
        dependency.project_id = "dep".into();
        dependency.id = "different".into();
        assert_eq!(
            dependency_issues(&v, &[dependency.clone()], "1.21.1", "fabric").len(),
            1
        );
        dependency.id = "pinned".into();
        assert!(dependency_issues(&v, &[dependency.clone()], "1.21.1", "fabric").is_empty());
        dependency.game_versions = vec!["1.20.1".into()];
        assert_eq!(
            dependency_issues(&v, &[dependency.clone()], "1.21.1", "fabric").len(),
            1
        );
        assert_eq!(dependency_issues(&v, &[], "1.21.1", "fabric").len(), 1); // Disabled JARs are not sent to hash lookup.
        v.dependencies[0].dependency_type = "incompatible".into();
        assert_eq!(
            dependency_issues(&v, &[dependency], "1.21.1", "fabric").len(),
            1
        );
        v.dependencies[0].dependency_type = "optional".into();
        assert!(dependency_issues(&v, &[], "1.21.1", "fabric").is_empty());
        v.dependencies[0].dependency_type = "required".into();
        v.dependencies[0].project_id = None;
        v.dependencies[0].version_id = None;
        v.dependencies[0].file_name = Some("external.jar".into());
        assert_eq!(dependency_issues(&v, &[], "1.21.1", "fabric").len(), 1);
    }

    #[test]
    fn icon_and_response_limits_and_error_redaction() {
        let cancel = AtomicBool::new(false);
        assert!(raster_icon(b"\x89PNG\r\n\x1a\n"));
        assert!(!raster_icon(b"<svg onload='bad'>"));
        assert!(read_limited(Cursor::new(vec![0; 11]), 10, &cancel).is_err());
        struct BadReader;
        impl Read for BadReader {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("https://token:secret@host/"))
            }
        }
        let error = read_limited(BadReader, 10, &cancel)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("secret"));
    }
}
