//! CurseForge official API adapter. API keys stay in the OS vault/request headers.
//! Contract: https://docs.curseforge.com/rest-api/ . Never synthesize a denied URL.
//! CDN header authentication is required from 2026-07-16:
//! https://blog.curseforge.com/introducing-api-key-authentication-for-curseforge-file-downloads/
use crate::resources::{
    Dependency, ModrinthProject, ModrinthVersion, ProjectHit, ResourceKind, SearchOptions,
    SearchPage, VersionFile,
};
use anyhow::{bail, ensure, Context, Result};
use reqwest::{
    blocking::{Client, RequestBuilder, Response},
    header::HeaderValue,
    redirect::Policy,
    Url,
};
use serde::Deserialize;
use std::{collections::BTreeMap, io::Read, sync::atomic::AtomicBool, time::Duration};
const API: &str = "https://api.curseforge.com/v1/";
const JSON_LIMIT: u64 = 16 * 1024 * 1024;

fn checked_key(value: String) -> Result<String> {
    ensure!(
        !value.trim().is_empty()
            && value.len() <= 4096
            && value.bytes().all(|b| b.is_ascii_graphic()),
        "CurseForge API Key 格式无效"
    );
    Ok(value)
}
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn entry() -> Result<keyring::Entry> {
    keyring::Entry::new("org.pcl-rust.curseforge", "api-key")
        .map_err(|_| anyhow::anyhow!("无法访问 CurseForge 系统凭据存储"))
}
fn api_key() -> Result<Option<String>> {
    if let Ok(value) = std::env::var("PCL_CURSEFORGE_API_KEY") {
        return checked_key(value).map(Some);
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        match entry()?.get_password() {
            Ok(value) => checked_key(value).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => bail!("读取 CurseForge 系统凭据失败"),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Ok(None)
    }
}
pub fn has_api_key() -> Result<bool> {
    Ok(api_key()?.is_some())
}
pub fn set_api_key(value: &str) -> Result<()> {
    let value = checked_key(value.trim().to_owned())?;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        entry()?
            .set_password(&value)
            .map_err(|_| anyhow::anyhow!("保存 CurseForge 系统凭据失败"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = value;
        bail!("此平台请通过 PCL_CURSEFORGE_API_KEY 环境变量配置")
    }
}
pub fn clear_api_key() -> Result<()> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        match entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => bail!("清除 CurseForge 系统凭据失败"),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Ok(())
    }
}
fn key_header() -> Result<HeaderValue> {
    let key =
        api_key()?.context("CurseForge 需要 API Key；请在设置 → 其他中配置，或使用 Modrinth")?;
    let mut value =
        HeaderValue::from_str(&key).map_err(|_| anyhow::anyhow!("CurseForge API Key 格式无效"))?;
    value.set_sensitive(true);
    Ok(value)
}
fn secure(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.fragment().is_none()
}
pub(crate) fn trusted_file(url: &Url) -> bool {
    secure(url)
        && url.query().is_none()
        && matches!(
            url.host_str(),
            Some("edge.forgecdn.net" | "mediafilez.forgecdn.net" | "mediafiles.forgecdn.net")
        )
}
fn trusted_image(url: &Url) -> bool {
    secure(url) && url.query().is_none() && matches!(url.host_str(), Some("media.forgecdn.net"))
}
fn client(files: bool, images: bool) -> Result<Client> {
    Ok(Client::builder()
        .user_agent(concat!("PCL-Rust-thirdparty/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .redirect(Policy::custom(move |a| {
            let allowed = if files {
                trusted_file(a.url())
            } else if images {
                trusted_image(a.url())
            } else {
                secure(a.url()) && a.url().host_str() == Some("api.curseforge.com")
            };
            if a.previous().len() < 3 && allowed {
                a.follow()
            } else {
                a.error("拒绝不可信的 CurseForge 重定向")
            }
        }))
        .build()?)
}
fn send(request: RequestBuilder, cancel: &AtomicBool) -> Result<Response> {
    crate::install::cancelled(cancel)?;
    let response = request.send().map_err(|e| {
        anyhow::anyhow!(if e.is_timeout() {
            "CurseForge 请求超时"
        } else {
            "CurseForge HTTPS 请求失败"
        })
    })?;
    crate::install::cancelled(cancel)?;
    match response.status().as_u16() {
        401 | 403 => bail!("CurseForge 拒绝请求，请检查 API Key 的有效性和访问权限"),
        429 => bail!("CurseForge 请求频率受限，请稍后重试"),
        code if !(200..300).contains(&code) => bail!("CurseForge 返回 HTTP {code}"),
        _ => Ok(response),
    }
}
fn read_limited(mut response: Response, limit: u64, cancel: &AtomicBool) -> Result<Vec<u8>> {
    ensure!(
        response.content_length().is_none_or(|n| n <= limit),
        "CurseForge 响应过大"
    );
    let mut out = Vec::new();
    let mut buf = [0u8; 16384];
    loop {
        crate::install::cancelled(cancel)?;
        let n = response
            .read(&mut buf)
            .map_err(|_| anyhow::anyhow!("读取 CurseForge 响应失败"))?;
        if n == 0 {
            break;
        }
        ensure!(out.len() + n <= limit as usize, "CurseForge 响应过大");
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}
fn request(
    path: &str,
    query: &[(&str, String)],
    body: Option<serde_json::Value>,
    cancel: &AtomicBool,
) -> Result<serde_json::Value> {
    let url = Url::parse(API)?.join(path)?;
    ensure!(
        secure(&url)
            && url.host_str() == Some("api.curseforge.com")
            && url.path().starts_with("/v1/"),
        "CurseForge API 路径无效"
    );
    let client = client(false, false)?;
    let request = if let Some(body) = body {
        client.post(url).json(&body)
    } else {
        client.get(url)
    };
    let bytes = read_limited(
        send(
            request.query(query).header("x-api-key", key_header()?),
            cancel,
        )?,
        JSON_LIMIT,
        cancel,
    )?;
    serde_json::from_slice(&bytes).context("CurseForge 响应 JSON 无效")
}
pub(crate) fn project_id(id: &str) -> Result<u64> {
    let number = id
        .strip_prefix("cf:")
        .context("CurseForge 项目标识无效")?
        .parse::<u64>()
        .context("CurseForge 项目标识无效")?;
    ensure!(
        number > 0 && number <= u32::MAX as u64,
        "CurseForge 项目标识越界"
    );
    Ok(number)
}
pub(crate) fn version_id(id: &str) -> Result<(u64, u64)> {
    let (project, file) = id.rsplit_once(':').context("CurseForge 文件标识无效")?;
    let project = project_id(project)?;
    let file = file.parse::<u64>().context("CurseForge 文件标识无效")?;
    ensure!(
        file > 0 && file <= u32::MAX as u64,
        "CurseForge 文件标识越界"
    );
    Ok((project, file))
}
fn class(kind: ResourceKind) -> u64 {
    match kind {
        ResourceKind::Mod => 6,
        ResourceKind::Modpack => 4471,
        ResourceKind::ResourcePack => 12,
        ResourceKind::Shader => 6552,
        ResourceKind::DataPack => 6945,
    }
}
fn kind(class: u64) -> Result<ResourceKind> {
    Ok(match class {
        6 => ResourceKind::Mod,
        4471 => ResourceKind::Modpack,
        12 => ResourceKind::ResourcePack,
        6552 => ResourceKind::Shader,
        6945 => ResourceKind::DataPack,
        _ => bail!("不支持此 CurseForge 资源类别"),
    })
}
fn loader_id(loader: &str) -> Result<u8> {
    Ok(match loader {
        "forge" => 1,
        "fabric" => 4,
        "quilt" => 5,
        "neoforge" => 6,
        _ => bail!("不支持此 CurseForge 加载器"),
    })
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfProject {
    id: u64,
    game_id: u64,
    class_id: Option<u64>,
    name: String,
    slug: String,
    summary: String,
    download_count: f64,
    date_modified: String,
    #[serde(default)]
    authors: Vec<Author>,
    #[serde(default)]
    categories: Vec<Category>,
    logo: Option<Logo>,
    #[serde(default)]
    links: Links,
    #[serde(default)]
    latest_files_indexes: Vec<FileIndex>,
}
#[derive(Deserialize)]
struct Author {
    name: String,
}
#[derive(Deserialize)]
struct Category {
    id: u64,
}
#[derive(Deserialize)]
struct Logo {
    url: String,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Links {
    website_url: Option<String>,
    source_url: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileIndex {
    game_version: String,
    mod_loader: Option<u8>,
}
fn loaders(index: &[FileIndex]) -> Vec<String> {
    let mut out: Vec<String> = index
        .iter()
        .filter_map(|i| match i.mod_loader {
            Some(1) => Some("forge"),
            Some(4) => Some("fabric"),
            Some(5) => Some("quilt"),
            Some(6) => Some("neoforge"),
            _ => None,
        })
        .map(str::to_owned)
        .collect();
    out.sort();
    out.dedup();
    out
}
fn project_data(id: u64, cancel: &AtomicBool) -> Result<CfProject> {
    let value = request(&format!("mods/{id}"), &[], None, cancel)?;
    let p: CfProject = serde_json::from_value(value["data"].clone())?;
    ensure!(p.id == id && p.game_id == 432, "CurseForge 项目响应不匹配");
    Ok(p)
}
pub fn get_project(id: u64, cancel: &AtomicBool) -> Result<ModrinthProject> {
    let p = project_data(id, cancel)?;
    let kind = kind(p.class_id.context("CurseForge 项目缺少类别")?)?;
    let value = request(&format!("mods/{id}/description"), &[], None, cancel)?;
    let html = value["data"].as_str().unwrap_or(&p.summary);
    // Render remote HTML as inert text in the common Markdown renderer.
    let body = regex::Regex::new(r"(?s)<[^>]*>")?
        .replace_all(html, " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ");
    Ok(ModrinthProject {
        id: format!("cf:{id}"),
        slug: p.slug,
        title: p.name,
        description: p.summary,
        body,
        project_type: kind.web_type().into(),
        icon_url: p.logo.map(|l| l.url),
        client_side: "unknown".into(),
        server_side: "unknown".into(),
        game_versions: p
            .latest_files_indexes
            .iter()
            .map(|f| f.game_version.clone())
            .collect(),
        loaders: match kind {
            ResourceKind::ResourcePack => vec!["minecraft".into()],
            ResourceKind::DataPack => vec!["datapack".into()],
            ResourceKind::Shader => vec!["optifine".into(), "iris".into()],
            _ => loaders(&p.latest_files_indexes),
        },
        downloads: p.download_count.max(0.0) as u64,
        updated: p.date_modified,
        source_url: p.links.source_url.or(p.links.website_url),
    })
}
pub fn search(
    kind: ResourceKind,
    options: &SearchOptions,
    category: &str,
    cancel: &AtomicBool,
) -> Result<SearchPage> {
    ensure!(
        options.query.len() <= 512 && !options.query.chars().any(char::is_control),
        "搜索词无效"
    );
    ensure!(
        (1..=50).contains(&options.limit)
            && options
                .offset
                .checked_add(options.limit)
                .is_some_and(|end| end <= 10000),
        "CurseForge 分页超出范围"
    );
    let mut query = vec![
        ("gameId", "432".into()),
        ("classId", class(kind).to_string()),
        (
            "searchFilter",
            crate::wiki::search_query(
                crate::resources::ResourceProvider::CurseForge,
                &options.query,
            ),
        ),
        ("index", options.offset.to_string()),
        ("pageSize", options.limit.to_string()),
        ("sortField", "2".into()),
        ("sortOrder", "desc".into()),
    ];
    if !category.is_empty() {
        let id = category.parse::<u32>().context("CurseForge 分类无效")?;
        query.push(("categoryId", id.to_string()));
    }
    if let Some(mc) = &options.minecraft {
        crate::metadata::validate_id(mc)?;
        query.push(("gameVersion", mc.clone()));
    }
    if let Some(loader) = &options.loader {
        query.push(("modLoaderType", loader_id(loader)?.to_string()));
    }
    let value = request("mods/search", &query, None, cancel)?;
    let projects: Vec<CfProject> = serde_json::from_value(value["data"].clone())?;
    let mut hits = Vec::new();
    for p in projects {
        ensure!(
            p.game_id == 432 && p.class_id == Some(class(kind)),
            "CurseForge 搜索返回其他资源类别"
        );
        hits.push(ProjectHit {
            project_id: format!("cf:{}", p.id),
            slug: p.slug,
            title: p.name,
            description: p.summary,
            author: p
                .authors
                .into_iter()
                .map(|a| a.name)
                .collect::<Vec<_>>()
                .join(", "),
            downloads: p.download_count.max(0.0) as u64,
            categories: p.categories.into_iter().map(|c| c.id.to_string()).collect(),
            icon_url: p.logo.map(|l| l.url),
            date_modified: p.date_modified,
            versions: p
                .latest_files_indexes
                .into_iter()
                .map(|f| f.game_version)
                .collect(),
        });
    }
    Ok(SearchPage {
        hits,
        offset: options.offset,
        limit: options.limit,
        total_hits: value["pagination"]["totalCount"]
            .as_u64()
            .context("CurseForge 缺少分页总数")?
            .min(10000),
    })
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfFile {
    id: u64,
    mod_id: u64,
    game_id: u64,
    is_available: bool,
    display_name: String,
    file_name: String,
    release_type: u8,
    hashes: Vec<CfHash>,
    file_date: String,
    file_length: u64,
    download_url: Option<String>,
    game_versions: Vec<String>,
    #[serde(default)]
    dependencies: Vec<CfDependency>,
    #[serde(default)]
    file_fingerprint: u32,
}
#[derive(Deserialize)]
struct CfHash {
    value: String,
    algo: u8,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfDependency {
    mod_id: u64,
    relation_type: u8,
}
fn adapt_file(f: CfFile, kind: ResourceKind) -> Result<ModrinthVersion> {
    ensure!(
        f.game_id == 432 && f.mod_id > 0 && f.id > 0,
        "CurseForge 文件不属于 Minecraft"
    );
    let mut loaders: Vec<String> = f
        .game_versions
        .iter()
        .filter_map(|v| match v.to_ascii_lowercase().as_str() {
            "forge" => Some("forge"),
            "fabric" => Some("fabric"),
            "quilt" => Some("quilt"),
            "neoforge" => Some("neoforge"),
            _ => None,
        })
        .map(str::to_owned)
        .collect();
    match kind {
        ResourceKind::ResourcePack => loaders.push("minecraft".into()),
        ResourceKind::DataPack => loaders.push("datapack".into()),
        ResourceKind::Shader => loaders.extend(["iris".into(), "optifine".into()]),
        _ => (),
    }
    let hashes = f
        .hashes
        .into_iter()
        .filter_map(|h| match h.algo {
            1 => Some(("sha1".into(), h.value.to_ascii_lowercase())),
            2 => Some(("md5".into(), h.value.to_ascii_lowercase())),
            _ => None,
        })
        .collect();
    let files = if f.is_available {
        f.download_url
            .filter(|u| !u.is_empty())
            .map(|url| {
                vec![VersionFile {
                    hashes,
                    url,
                    filename: f.file_name,
                    primary: true,
                    size: f.file_length,
                    file_type: None,
                }]
            })
            .unwrap_or_default()
    } else {
        vec![]
    };
    Ok(ModrinthVersion {
        id: format!("cf:{}:{}", f.mod_id, f.id),
        project_id: format!("cf:{}", f.mod_id),
        name: f.display_name.clone(),
        version_number: f.display_name,
        version_type: match f.release_type {
            1 => "release",
            2 => "beta",
            _ => "alpha",
        }
        .into(),
        date_published: f.file_date,
        game_versions: f.game_versions,
        loaders,
        files,
        dependencies: f
            .dependencies
            .into_iter()
            .filter_map(|d| {
                let ty = match d.relation_type {
                    3 => "required",
                    5 => "incompatible",
                    2 => "optional",
                    1 | 6 => "embedded",
                    _ => return None,
                };
                Some(Dependency {
                    version_id: None,
                    project_id: Some(format!("cf:{}", d.mod_id)),
                    file_name: None,
                    dependency_type: ty.into(),
                })
            })
            .collect(),
        environment: None,
    })
}
pub fn get_file(project: u64, file: u64, cancel: &AtomicBool) -> Result<ModrinthVersion> {
    get_file_with_kind(project, file, cancel).map(|(_, v)| v)
}
pub fn get_file_with_kind(
    project: u64,
    file: u64,
    cancel: &AtomicBool,
) -> Result<(ResourceKind, ModrinthVersion)> {
    let p = project_data(project, cancel)?;
    let kind = kind(p.class_id.context("CurseForge 项目缺少类别")?)?;
    let value = request(&format!("mods/{project}/files/{file}"), &[], None, cancel)?;
    let f: CfFile = serde_json::from_value(value["data"].clone())?;
    ensure!(
        f.id == file && f.mod_id == project,
        "CurseForge 文件响应不匹配"
    );
    let v = adapt_file(f, kind)?;
    ensure!(
        !v.files.is_empty(),
        "作者未允许通过 CurseForge API 下载此文件，请从项目官网手动下载"
    );
    Ok((kind, v))
}
pub fn list_files(
    project: u64,
    resource_kind: ResourceKind,
    minecraft: &str,
    loader: &str,
    cancel: &AtomicBool,
) -> Result<Vec<ModrinthVersion>> {
    let p = project_data(project, cancel)?;
    ensure!(
        p.class_id == Some(class(resource_kind)),
        "CurseForge 项目类别不匹配"
    );
    let mut out = Vec::new();
    let mut index = 0u64;
    loop {
        let mut query = vec![("pageSize", "50".into()), ("index", index.to_string())];
        if !minecraft.is_empty() {
            crate::metadata::validate_id(minecraft)?;
            query.push(("gameVersion", minecraft.into()));
        }
        if !loader.is_empty() {
            query.push(("modLoaderType", loader_id(loader)?.to_string()));
        }
        let value = request(&format!("mods/{project}/files"), &query, None, cancel)?;
        let files: Vec<CfFile> = serde_json::from_value(value["data"].clone())?;
        let count = files.len() as u64;
        let total = value["pagination"]["totalCount"]
            .as_u64()
            .context("CurseForge 缺少文件分页信息")?;
        for f in files {
            ensure!(f.mod_id == project, "CurseForge 文件列表含其他项目");
            let v = adapt_file(f, resource_kind)?;
            if !v.files.is_empty()
                && crate::resources::resource_compatible(resource_kind, &v, minecraft, loader)
            {
                out.push(v);
            }
        }
        index += count;
        if index >= total {
            break;
        }
        ensure!(
            count > 0 && index + 50 <= 10000,
            "CurseForge 文件列表超过分页上限，请缩小版本筛选"
        );
    }
    out.sort_by(|a, b| b.date_published.cmp(&a.date_published));
    Ok(out)
}
pub fn download_file(file: &VersionFile, cancel: &AtomicBool) -> Result<Response> {
    let url = Url::parse(&file.url).context("CurseForge 文件地址无效")?;
    ensure!(trusted_file(&url), "CurseForge 文件仅允许官方 HTTPS CDN");
    send(
        client(true, false)?
            .get(url)
            .header("x-api-key", key_header()?),
        cancel,
    )
}
pub(crate) fn fetch_icon(url: &str, cancel: &AtomicBool) -> Result<Vec<u8>> {
    let url = Url::parse(url)?;
    ensure!(trusted_image(&url), "CurseForge 图标地址无效");
    read_limited(
        send(client(false, true)?.get(url), cancel)?,
        2 * 1024 * 1024,
        cancel,
    )
}

/// CurseForge's whitespace-filtered MurmurHash2 is an identification hint only;
/// a server match is accepted only when its SHA1 matches the local bytes as well.
fn fingerprint(bytes: &[u8]) -> u32 {
    const M: u32 = 0x5bd1e995;
    let mut h = 1 ^ (bytes.len() as u32);
    let (chunks, tail) = bytes.as_chunks::<4>();
    for b in chunks {
        let mut k = u32::from_le_bytes(*b);
        k = k.wrapping_mul(M);
        k ^= k >> 24;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M) ^ k;
    }
    let mut k = 0;
    for (i, b) in tail.iter().enumerate() {
        k |= (*b as u32) << (i * 8);
    }
    if !tail.is_empty() {
        h = (h ^ k).wrapping_mul(M);
    }
    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^ (h >> 15)
}
pub(crate) fn identify_files(
    snapshot: &BTreeMap<std::path::PathBuf, String>,
    kind: ResourceKind,
    cancel: &AtomicBool,
) -> Result<Vec<ModrinthVersion>> {
    identify_files_with_disabled(snapshot, kind, false, cancel)
}
pub(crate) fn identify_files_with_disabled(
    snapshot: &BTreeMap<std::path::PathBuf, String>,
    kind: ResourceKind,
    include_disabled: bool,
    cancel: &AtomicBool,
) -> Result<Vec<ModrinthVersion>> {
    use sha2::Digest;
    let mut local: BTreeMap<u32, Vec<(String, String, u64)>> = BTreeMap::new();
    for (path, expected) in snapshot {
        if !include_disabled
            && path
                .to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(".disabled")
        {
            continue;
        }
        crate::install::cancelled(cancel)?;
        let meta = std::fs::symlink_metadata(path)?;
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 512 * 1024 * 1024,
            "本地资源类型或大小不安全"
        );
        let mut input = std::fs::File::open(path)?;
        let mut filtered = Vec::new();
        let mut sha1 = sha1::Sha1::new();
        let mut sha512 = sha2::Sha512::new();
        let mut total = 0;
        let mut buf = [0u8; 65536];
        loop {
            crate::install::cancelled(cancel)?;
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            ensure!(total <= meta.len(), "资源文件在扫描期间变更");
            sha1.update(&buf[..n]);
            sha512.update(&buf[..n]);
            filtered.extend(
                buf[..n]
                    .iter()
                    .copied()
                    .filter(|b| !matches!(b, 9 | 10 | 13 | 32)),
            );
        }
        let actual = format!("{:x}", sha512.finalize());
        ensure!(&actual == expected, "资源文件在扫描期间变更");
        local.entry(fingerprint(&filtered)).or_default().push((
            format!("{:x}", sha1.finalize()),
            actual,
            total,
        ));
    }
    let fingerprints: Vec<u32> = local.keys().copied().collect();
    let mut out = BTreeMap::new();
    for chunk in fingerprints.chunks(100) {
        let value = request(
            "fingerprints/432",
            &[],
            Some(serde_json::json!({"fingerprints":chunk})),
            cancel,
        )?;
        for matched in value["data"]["exactMatches"]
            .as_array()
            .context("CurseForge 指纹响应无效")?
        {
            let f: CfFile = serde_json::from_value(matched["file"].clone())?;
            let candidates = local
                .get(&f.file_fingerprint)
                .context("CurseForge 返回未查询的指纹")?;
            let sha1 = f
                .hashes
                .iter()
                .find(|h| h.algo == 1)
                .context("CurseForge 指纹响应缺少 SHA1")?;
            let Some((_, sha512, _)) = candidates.iter().find(|(hash, _, size)| {
                hash.eq_ignore_ascii_case(&sha1.value) && *size == f.file_length
            }) else {
                continue;
            };
            let mut v = adapt_file(f, kind)?;
            if let Some(file) = v.files.first_mut() {
                file.hashes.insert("sha512".into(), sha512.clone());
                out.insert(v.id.clone(), v);
            }
        }
    }
    Ok(out.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file() -> CfFile {
        serde_json::from_value(serde_json::json!({"id":20,"modId":10,"gameId":432,"isAvailable":true,"displayName":"Fixture","fileName":"fixture.jar","releaseType":1,"hashes":[{"algo":1,"value":"0123456789012345678901234567890123456789"}],"fileDate":"2026-01-01","fileLength":100,"downloadUrl":"https://edge.forgecdn.net/files/1/2/fixture.jar","gameVersions":["1.21.1","Fabric"],"dependencies":[{"modId":11,"relationType":3},{"modId":12,"relationType":5}]})).unwrap()
    }
    #[test]
    fn adapter_preserves_identity_hashes_and_dependency_types() {
        let v = adapt_file(file(), ResourceKind::Mod).unwrap();
        assert_eq!(v.id, "cf:10:20");
        assert_eq!(v.project_id, "cf:10");
        assert_eq!(v.loaders, ["fabric"]);
        assert!(v.files[0].hashes.contains_key("sha1"));
        assert!(!v.files[0].hashes.contains_key("sha512"));
        assert_eq!(v.dependencies[0].dependency_type, "required");
        assert_eq!(v.dependencies[1].dependency_type, "incompatible");
        assert_eq!(version_id(&v.id).unwrap(), (10, 20));
    }
    #[test]
    fn unavailable_files_never_get_a_synthesized_download() {
        let mut f = file();
        f.download_url = None;
        assert!(adapt_file(f, ResourceKind::Mod).unwrap().files.is_empty());
        let mut f = file();
        f.is_available = false;
        assert!(adapt_file(f, ResourceKind::Mod).unwrap().files.is_empty());
    }
    #[test]
    fn keys_and_cdn_redirect_boundaries() {
        for v in ["", "a\nb", "a b"] {
            assert!(checked_key(v.into()).is_err());
        }
        for v in [
            "http://edge.forgecdn.net/files/a.jar",
            "https://edge.forgecdn.net.evil.test/a.jar",
            "https://edge.forgecdn.net/a?key=x",
            "https://user:secret@edge.forgecdn.net/a.jar",
            "https://127.0.0.1/a.jar",
        ] {
            assert!(!trusted_file(&Url::parse(v).unwrap()), "{v}");
        }
        assert!(trusted_file(
            &Url::parse("https://mediafilez.forgecdn.net/files/1/2/a.jar").unwrap()
        ));
        assert!(version_id("cf:1:0").is_err());
        assert!(project_id("cf:1/../../bad").is_err());
    }
    #[test]
    fn murmur_matches_known_seed_one_vector() {
        assert_eq!(fingerprint(b""), 0x5bd15e36);
        assert_eq!(fingerprint(b"hello"), 0xa631918e);
    }
}
