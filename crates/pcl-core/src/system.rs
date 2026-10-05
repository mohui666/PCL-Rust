//! Local launcher preferences and checks against this Rust project's public releases.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LauncherUpdateMode {
    Download,
    #[default]
    Notify,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemSettings {
    pub auto_chinese: bool,
    pub notify_release: bool,
    pub notify_snapshot: bool,
    pub launcher_update: LauncherUpdateMode,
    pub cache_dir: Option<PathBuf>,
    pub last_release: Option<String>,
    pub last_snapshot: Option<String>,
    pub last_launcher_tag: Option<String>,
}
impl Default for SystemSettings {
    fn default() -> Self {
        Self {
            auto_chinese: true,
            notify_release: false,
            notify_snapshot: false,
            launcher_update: LauncherUpdateMode::Notify,
            cache_dir: None,
            last_release: None,
            last_snapshot: None,
            last_launcher_tag: None,
        }
    }
}
impl SystemSettings {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.cache_dir.as_ref().is_some_and(|p| !p.is_absolute()),
            "缓存文件夹必须是绝对路径"
        );
        for value in [
            &self.last_release,
            &self.last_snapshot,
            &self.last_launcher_tag,
        ]
        .into_iter()
        .flatten()
        {
            anyhow::ensure!(
                value.len() <= 256 && !value.chars().any(char::is_control),
                "更新记录无效"
            );
        }
        Ok(())
    }
}

use anyhow::{ensure, Context, Result};
use reqwest::{blocking::Client, redirect::Policy, Url};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
    time::Duration,
};

pub const RELEASES_PAGE: &str = "https://github.com/mohui666/PCL-Rust/releases";
const RELEASE_API: &str = "https://api.github.com/repos/mohui666/PCL-Rust/releases/latest";
const MAX_PACKAGE: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct UpdateAsset {
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub url: String,
}
#[derive(Clone, Debug)]
pub struct LauncherRelease {
    pub tag: String,
    pub name: String,
    pub url: String,
    pub published_at: String,
    pub body: String,
    pub newer: bool,
    pub assets: Vec<UpdateAsset>,
    pub unavailable_assets: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameUpdate {
    pub version: String,
    pub snapshot: bool,
}

/// Match upstream's first-observation rule: record the baseline silently, then
/// report a changed release/snapshot only when that category is opted in.
pub fn observe_game_versions(
    settings: &mut SystemSettings,
    manifest: &Value,
) -> Result<Vec<GameUpdate>> {
    let release = manifest["latest"]["release"]
        .as_str()
        .context("官方清单缺少正式版版本号")?;
    let snapshot = manifest["latest"]["snapshot"]
        .as_str()
        .context("官方清单缺少测试版版本号")?;
    for id in [release, snapshot] {
        crate::metadata::validate_id(id)?;
    }
    let mut updates = Vec::new();
    if settings.notify_snapshot
        && settings
            .last_snapshot
            .as_deref()
            .is_some_and(|old| old != snapshot)
    {
        updates.push(GameUpdate {
            version: snapshot.into(),
            snapshot: true,
        });
    }
    if settings.notify_release
        && settings
            .last_release
            .as_deref()
            .is_some_and(|old| old != release)
        && !updates.iter().any(|item| item.version == release)
    {
        updates.push(GameUpdate {
            version: release.into(),
            snapshot: false,
        });
    }
    settings.last_release = Some(release.into());
    settings.last_snapshot = Some(snapshot.into());
    Ok(updates)
}

pub fn cache_directory(settings: &SystemSettings) -> PathBuf {
    settings.cache_dir.clone().unwrap_or_else(|| {
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("pcl-rust")
    })
}
fn clean_https(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}
fn download_redirect(url: &Url) -> bool {
    clean_https(url)
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}
fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent("PCL-Rust (https://github.com/mohui666/PCL-Rust)")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() < 5 && download_redirect(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("更新下载跳转超出 GitHub 官方站点")
            }
        }))
        .build()?)
}
fn clean_asset_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("更新资源地址无效")?;
    ensure!(
        clean_https(&url)
            && url.host_str() == Some("github.com")
            && url.query().is_none()
            && url
                .path()
                .starts_with("/mohui666/PCL-Rust/releases/download/"),
        "更新资源不属于本项目公开 GitHub Releases"
    );
    Ok(url)
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 180
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}
fn platform_asset(name: &str, platform: &crate::model::Platform) -> bool {
    let name = name.to_ascii_lowercase();
    let os = match platform.os.as_str() {
        "osx" => {
            (name.contains("macos") || name.contains("darwin") || name.contains("mac-"))
                && (name.ends_with(".zip") || name.ends_with(".dmg"))
        }
        "windows" => {
            (name.contains("windows") || name.contains("win-"))
                && (name.ends_with(".zip") || name.ends_with(".exe") || name.ends_with(".msi"))
        }
        _ => false,
    };
    let arch = if matches!(platform.arch.as_str(), "aarch64" | "arm64") {
        name.contains("aarch64") || name.contains("arm64") || name.contains("universal")
    } else {
        (name.contains("x86_64")
            || name.contains("x64")
            || name.contains("amd64")
            || name.contains("universal"))
            && !name.contains("arm64")
    };
    os && arch
}
fn version_tuple(value: &str) -> Option<(u64, u64, u64)> {
    let clean = value.strip_prefix('v').unwrap_or(value);
    let mut parts = clean.split('.');
    let result = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(result)
}
fn parse_release(
    value: &Value,
    current: &str,
    platform: &crate::model::Platform,
) -> Result<LauncherRelease> {
    ensure!(
        value["draft"] == false && value["prerelease"] == false,
        "最新发布不是公开稳定版本"
    );
    let tag = value["tag_name"].as_str().context("发布缺少版本号")?;
    ensure!(
        tag.len() <= 100 && !tag.chars().any(char::is_control),
        "发布版本号无效"
    );
    let latest = version_tuple(tag).context("发布版本号不是可比较的 v主.次.修订 格式")?;
    let current = version_tuple(current).context("当前版本号格式无效")?;
    let url = value["html_url"].as_str().context("发布缺少页面")?;
    let parsed = Url::parse(url)?;
    ensure!(
        clean_https(&parsed)
            && parsed.host_str() == Some("github.com")
            && parsed
                .path()
                .starts_with("/mohui666/PCL-Rust/releases/tag/")
            && parsed.query().is_none(),
        "发布页面不属于本项目"
    );
    let mut assets = Vec::new();
    let mut unavailable_assets = 0;
    for item in value["assets"]
        .as_array()
        .context("发布资源列表无效")?
        .iter()
        .take(100)
    {
        let Some(name) = item["name"].as_str() else {
            continue;
        };
        if !platform_asset(name, platform) {
            continue;
        }
        let Some(digest) = item["digest"]
            .as_str()
            .and_then(|x| x.strip_prefix("sha256:"))
        else {
            unavailable_assets += 1;
            continue;
        };
        let size = item["size"].as_u64().unwrap_or(0);
        let url = item["browser_download_url"].as_str().unwrap_or("");
        if !valid_name(name)
            || digest.len() != 64
            || !digest.bytes().all(|b| b.is_ascii_hexdigit())
            || !(1..=MAX_PACKAGE).contains(&size)
            || clean_asset_url(url).is_err()
            || item["state"] != "uploaded"
        {
            unavailable_assets += 1;
            continue;
        }
        assets.push(UpdateAsset {
            name: name.into(),
            size,
            sha256: digest.to_ascii_lowercase(),
            url: url.into(),
        });
    }
    Ok(LauncherRelease {
        tag: tag.into(),
        name: value["name"]
            .as_str()
            .unwrap_or(tag)
            .chars()
            .take(256)
            .collect(),
        url: url.into(),
        published_at: value["published_at"].as_str().unwrap_or("").into(),
        body: value["body"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(16_384)
            .collect(),
        newer: latest > current,
        assets,
        unavailable_assets,
    })
}

/// Public, unauthenticated read of this project's release. A 404 is reported as
/// no published release; it never falls back to the upstream Windows launcher.
pub fn check_launcher_update(
    current: &str,
    platform: &crate::model::Platform,
    cancel: &AtomicBool,
) -> Result<Option<LauncherRelease>> {
    crate::install::cancelled(cancel)?;
    let response = client()?
        .get(RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .map_err(|_| anyhow::anyhow!("连接本项目 GitHub 更新服务失败"))?;
    crate::install::cancelled(cancel)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    ensure!(
        response.status().is_success(),
        "GitHub 更新服务返回 HTTP {}",
        response.status().as_u16()
    );
    let mut bytes = Vec::new();
    response.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 2 * 1024 * 1024, "更新资料超过大小限制");
    crate::install::cancelled(cancel)?;
    parse_release(
        &serde_json::from_slice(&bytes).context("GitHub 发布资料无效")?,
        current,
        platform,
    )
    .map(Some)
}

/// Download only; no executable is run and no application bundle is replaced.
/// The public GitHub SHA-256 digest and size must both match before no-clobber commit.
pub fn download_update(
    asset: &UpdateAsset,
    settings: &SystemSettings,
    cancel: &AtomicBool,
    progress: impl Fn(u64, u64),
) -> Result<PathBuf> {
    crate::install::cancelled(cancel)?;
    settings.validate()?;
    ensure!(
        valid_name(&asset.name)
            && asset.sha256.len() == 64
            && asset.sha256.bytes().all(|x| x.is_ascii_hexdigit())
            && (1..=MAX_PACKAGE).contains(&asset.size),
        "更新资源校验信息无效"
    );
    let url = clean_asset_url(&asset.url)?;
    let cache = cache_directory(settings);
    ensure!(cache.is_absolute(), "缓存目录必须是绝对路径");
    if cache.exists() {
        ensure!(
            !fs::symlink_metadata(&cache)?.file_type().is_symlink(),
            "缓存目录不能是符号链接"
        );
    }
    fs::create_dir_all(&cache)?;
    let root = cache.canonicalize()?;
    let folder = crate::metadata::confined_path(&root, Path::new("updates"))?;
    match fs::create_dir(&folder) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => ensure!(
            fs::symlink_metadata(&folder)?.is_dir(),
            "更新缓存被文件占用"
        ),
        Err(e) => return Err(e.into()),
    }
    let path = crate::metadata::confined_path(&root, &Path::new("updates").join(&asset.name))?;
    if fs::symlink_metadata(&path).is_ok() {
        verify_file(&path, asset, cancel)?;
        progress(asset.size, asset.size);
        return Ok(path);
    }
    crate::install::cancelled(cancel)?;
    let response = client()?
        .get(url)
        .send()
        .map_err(|_| anyhow::anyhow!("连接 GitHub 更新包下载地址失败"))?;
    ensure!(
        response.status().is_success(),
        "更新包下载返回 HTTP {}",
        response.status().as_u16()
    );
    if let Some(size) = response.content_length() {
        ensure!(size == asset.size, "更新包长度与公开资料不符");
    }
    commit_package(&root, &folder, asset, response, cancel, progress)
}
fn commit_package(
    root: &Path,
    folder: &Path,
    asset: &UpdateAsset,
    mut reader: impl Read,
    cancel: &AtomicBool,
    progress: impl Fn(u64, u64),
) -> Result<PathBuf> {
    let mut file = tempfile::NamedTempFile::new_in(folder)?;
    let mut hash = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 65536];
    loop {
        crate::install::cancelled(cancel)?;
        let n = reader
            .read(&mut buffer)
            .map_err(|_| anyhow::anyhow!("读取更新包失败"))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        ensure!(total <= asset.size, "更新包超过声明大小");
        crate::network::throttle(n, cancel)?;
        hash.update(&buffer[..n]);
        file.write_all(&buffer[..n])?;
        progress(total, asset.size);
    }
    ensure!(
        total == asset.size && format!("{:x}", hash.finalize()).eq_ignore_ascii_case(&asset.sha256),
        "更新包 SHA-256 或长度校验失败，未保存"
    );
    file.as_file().sync_all()?;
    crate::install::cancelled(cancel)?;
    let path = crate::metadata::confined_path(root, &Path::new("updates").join(&asset.name))?;
    file.persist_noclobber(&path)
        .map_err(|e| anyhow::anyhow!("更新包目标已存在或无法保存：{}", e.error))?;
    Ok(path)
}
fn verify_file(path: &Path, asset: &UpdateAsset, cancel: &AtomicBool) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() == asset.size,
        "同名更新文件已存在且不匹配，未覆盖"
    );
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut total = 0;
    let mut buf = [0; 65536];
    loop {
        crate::install::cancelled(cancel)?;
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        ensure!(total <= asset.size, "更新文件在读取中发生变化");
        hash.update(&buf[..n]);
    }
    ensure!(
        total == asset.size && format!("{:x}", hash.finalize()).eq_ignore_ascii_case(&asset.sha256),
        "同名更新文件 SHA-256 不匹配，未覆盖"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn platform() -> crate::model::Platform {
        crate::model::Platform {
            os: "osx".into(),
            arch: "aarch64".into(),
            version: String::new(),
        }
    }
    fn release() -> Value {
        json!({"draft":false,"prerelease":false,"tag_name":"v0.2.0","html_url":"https://github.com/mohui666/PCL-Rust/releases/tag/v0.2.0","assets":[{"name":"PCL-Rust-macos-arm64.zip","size":4,"digest":format!("sha256:{}","a".repeat(64)),"state":"uploaded","browser_download_url":"https://github.com/mohui666/PCL-Rust/releases/download/v0.2.0/PCL-Rust-macos-arm64.zip"}]})
    }
    #[test]
    fn release_selection_requires_this_project_platform_and_public_digest() {
        let mut json = release();
        let value = parse_release(&json, "0.1.0", &platform()).unwrap();
        assert!(value.newer);
        assert_eq!(value.assets.len(), 1);
        json["assets"][0]["digest"] = Value::Null;
        let value = parse_release(&json, "0.1.0", &platform()).unwrap();
        assert!(value.assets.is_empty());
        assert_eq!(value.unavailable_assets, 1);
        json["assets"][0]["digest"] = format!("sha256:{}", "a".repeat(64)).into();
        json["assets"][0]["browser_download_url"] =
            "https://github.com/other/repo/releases/download/x/file.zip".into();
        assert!(parse_release(&json, "0.1.0", &platform())
            .unwrap()
            .assets
            .is_empty());
        assert!(!platform_asset("PCL-Rust-Windows-x64.exe", &platform()));
        assert!(!platform_asset("PCL-Rust-macos-x64.zip", &platform()));
        assert!(clean_asset_url(
            "https://secret@github.com/mohui666/PCL-Rust/releases/download/x/a.zip"
        )
        .is_err());
        assert!(clean_asset_url(
            "https://github.com/mohui666/PCL-Rust/releases/download/x/a.zip?token=x"
        )
        .is_err());
        assert!(!download_redirect(
            &Url::parse("http://github.com/x").unwrap()
        ));
        assert!(!download_redirect(
            &Url::parse("https://localhost/x").unwrap()
        ));
    }
    #[test]
    fn game_notifications_need_baseline_and_opt_in_and_deduplicate() {
        let mut settings = SystemSettings {
            notify_release: true,
            notify_snapshot: true,
            ..Default::default()
        };
        let old = json!({"latest":{"release":"1.21.1","snapshot":"24w01a"}});
        assert!(observe_game_versions(&mut settings, &old)
            .unwrap()
            .is_empty());
        assert!(observe_game_versions(&mut settings, &old)
            .unwrap()
            .is_empty());
        let next = json!({"latest":{"release":"1.21.2","snapshot":"1.21.2"}});
        assert_eq!(
            observe_game_versions(&mut settings, &next).unwrap().len(),
            1
        );
        settings.notify_release = false;
        settings.notify_snapshot = false;
        assert!(observe_game_versions(&mut settings, &old)
            .unwrap()
            .is_empty());
        assert_eq!(settings.last_release.as_deref(), Some("1.21.1"));
    }
    #[test]
    fn cached_update_never_overwrites_unknown_data_and_cancel_never_downloads() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        fs::create_dir_all(cache.join("updates")).unwrap();
        let data = b"four";
        let asset=UpdateAsset{name:"PCL-Rust-macos-arm64.zip".into(),size:4,sha256:format!("{:x}",Sha256::digest(data)),url:"https://github.com/mohui666/PCL-Rust/releases/download/v0.2.0/PCL-Rust-macos-arm64.zip".into()};
        let settings = SystemSettings {
            cache_dir: Some(cache.clone()),
            ..Default::default()
        };
        let path = cache.join("updates").join(&asset.name);
        fs::write(&path, data).unwrap();
        assert_eq!(
            download_update(&asset, &settings, &AtomicBool::new(false), |_, _| {}).unwrap(),
            path.canonicalize().unwrap()
        );
        fs::write(&path, b"user").unwrap();
        assert!(download_update(&asset, &settings, &AtomicBool::new(false), |_, _| {}).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"user");
        let error =
            download_update(&asset, &settings, &AtomicBool::new(true), |_, _| {}).unwrap_err();
        assert!(error.is::<crate::model::OperationCancelled>());
    }
    #[test]
    fn streamed_update_requires_hash_size_and_no_clobber_and_honors_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let folder = root.join("updates");
        fs::create_dir(&folder).unwrap();
        let mut asset = UpdateAsset {
            name: "PCL-Rust-macos-arm64.zip".into(),
            size: 4,
            sha256: format!("{:x}", Sha256::digest(b"good")),
            url: String::new(),
        };
        let flag = AtomicBool::new(false);
        for bytes in [
            b"evil".as_slice(),
            b"goodextra".as_slice(),
            b"go".as_slice(),
        ] {
            assert!(commit_package(&root, &folder, &asset, bytes, &flag, |_, _| {}).is_err());
            assert_eq!(fs::read_dir(&folder).unwrap().count(), 0);
        }
        assert!(
            commit_package(&root, &folder, &asset, b"good".as_slice(), &flag, |_, _| {
                flag.store(true, std::sync::atomic::Ordering::Relaxed)
            })
            .unwrap_err()
            .is::<crate::model::OperationCancelled>()
        );
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 0);
        flag.store(false, std::sync::atomic::Ordering::Relaxed);
        let path = folder.join(&asset.name);
        assert!(
            commit_package(&root, &folder, &asset, b"good".as_slice(), &flag, |_, _| {
                fs::write(&path, b"user").unwrap()
            })
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"user");
        fs::remove_file(&path).unwrap();
        asset.sha256 = asset.sha256.to_uppercase();
        assert_eq!(
            commit_package(&root, &folder, &asset, b"good".as_slice(), &flag, |_, _| {}).unwrap(),
            path
        );
        assert_eq!(fs::read(path).unwrap(), b"good");
    }
    #[cfg(unix)]
    #[test]
    fn updater_rejects_a_symlink_cache_or_target_without_touching_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        std::os::unix::fs::symlink(other.path(), &cache).unwrap();
        let asset=UpdateAsset{name:"PCL-Rust-macos-arm64.zip".into(),size:4,sha256:"a".repeat(64),url:"https://github.com/mohui666/PCL-Rust/releases/download/v0.2.0/PCL-Rust-macos-arm64.zip".into()};
        assert!(download_update(
            &asset,
            &SystemSettings {
                cache_dir: Some(cache),
                ..Default::default()
            },
            &AtomicBool::new(false),
            |_, _| {}
        )
        .is_err());
        assert_eq!(fs::read_dir(other.path()).unwrap().count(), 0);
    }
}
