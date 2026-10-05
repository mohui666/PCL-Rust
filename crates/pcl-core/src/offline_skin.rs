//! Offline appearance only. No Microsoft credentials or authentication state are read.
use crate::{
    config::{OfflineSkinMode, Settings},
    metadata::{confined_path, resolve_version},
    model::{OperationCancelled, Session},
};
use anyhow::{bail, Context, Result};
use image::{GenericImageView, ImageFormat};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Cursor, Read, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

const MAX_PNG: u64 = 1024 * 1024;
const MAX_OPTIONS: u64 = 4 * 1024 * 1024;
static WRITE_LOCK: Mutex<()> = Mutex::new(());
#[derive(Clone)]
pub enum SkinUpdate {
    Disable,
    Activate {
        png: Vec<u8>,
        slim: bool,
        modern: bool,
        legacy_crop: bool,
        pack_format: u32,
        new_options: bool,
    },
}
pub struct PreparedSkin {
    pub session: Session,
    pub update: SkinUpdate,
    pub warnings: Vec<String>,
}
fn cancel(flag: &AtomicBool) -> Result<()> {
    if flag.load(Ordering::Relaxed) {
        return Err(OperationCancelled.into());
    }
    Ok(())
}

/// Only call for a user-requested launch; planning/exporting never needs this network lookup.
pub fn prepare(
    root: &Path,
    version_id: &str,
    settings: &Settings,
    session: &Session,
    cancelled: &AtomicBool,
) -> Result<PreparedSkin> {
    cancel(cancelled)?;
    let mut output = PreparedSkin {
        session: session.clone(),
        update: SkinUpdate::Disable,
        warnings: vec![],
    };
    if session.user_type != "legacy" || settings.offline_skin_mode == OfflineSkinMode::Default {
        return Ok(output);
    }
    let metadata = resolve_version(root, version_id)?;
    let vanilla = metadata["_pcl_jar_id"].as_str().unwrap_or(version_id);
    let version = release_version(vanilla);
    match settings.offline_skin_mode {
        OfflineSkinMode::Default => (),
        OfflineSkinMode::Steve | OfflineSkinMode::Alex => {
            output.session.uuid = uuid_for_model(
                &session.uuid,
                settings.offline_skin_mode == OfflineSkinMode::Alex,
            )?;
        }
        OfflineSkinMode::OfficialName => {
            if version.is_none_or(|(minor, _)| minor >= 20) {
                output.warnings.push("此 Minecraft 版本不支持原版的正版名称离线皮肤方式，保留默认离线 UUID；可以改用本地自定义皮肤。".into());
            } else {
                output.session.uuid = official_uuid(&settings.offline_skin_name, cancelled)?;
            }
        }
        OfflineSkinMode::Custom => {
            let path = settings
                .offline_skin_path
                .as_deref()
                .context("请先选择本地自定义皮肤 PNG")?;
            let png = read_skin(path)?;
            if version.is_some_and(|(minor, _)| minor < 6) {
                bail!("Minecraft 1.6 以前不支持皮肤资源包");
            }
            let pack_format = pack_format(root, vanilla, version)?;
            output.session.uuid = uuid_for_model(&session.uuid, settings.offline_skin_slim)?;
            let modern = version.is_none_or(|v| v >= (19, 3));
            output.update = SkinUpdate::Activate {
                png,
                slim: settings.offline_skin_slim,
                modern,
                legacy_crop: version.is_some_and(|v| v.0 <= 7),
                pack_format,
                new_options: version.is_none_or(|v| v.0 >= 13),
            };
        }
    }
    if version.is_some_and(|(minor, _)| (2..=7).contains(&minor))
        && (settings.offline_skin_mode == OfflineSkinMode::Alex
            || (settings.offline_skin_mode == OfflineSkinMode::Custom
                && settings.offline_skin_slim))
    {
        output
            .warnings
            .push("此 Minecraft 版本尚不支持 Alex 模型，游戏可能仍显示 Steve。".into());
    }
    cancel(cancelled)?;
    Ok(output)
}
fn release_version(id: &str) -> Option<(u32, u32)> {
    let mut parts = id.split('.');
    if parts.next()? != "1" {
        return None;
    }
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        None
    } else {
        Some((minor, patch))
    }
}
pub fn uuid_for_model(uuid: &str, slim: bool) -> Result<String> {
    let compact = uuid.replace('-', "");
    anyhow::ensure!(
        compact.len() == 32 && compact.bytes().all(|v| v.is_ascii_hexdigit()),
        "离线 UUID 格式无效"
    );
    let mut value = u128::from_str_radix(&compact, 16)?;
    for _ in 0..4 {
        let hash = (value as u32)
            ^ ((value >> 32) as u32)
            ^ ((value >> 64) as u32)
            ^ ((value >> 96) as u32);
        if (hash & 1 == 1) == slim {
            return Ok(format!("{value:032x}"));
        }
        value = (value & !0xfffff) | ((value.wrapping_add(1)) & 0xfffff);
    }
    bail!("无法选择离线皮肤模型")
}
fn official_uuid(name: &str, cancelled: &AtomicBool) -> Result<String> {
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= 16
            && name.bytes().all(|v| v.is_ascii_alphanumeric() || v == b'_'),
        "正版皮肤名称应为 1–16 位英文字母、数字或下划线"
    );
    cancel(cancelled)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client
        .get(format!(
            "https://api.mojang.com/users/profiles/minecraft/{name}"
        ))
        .send()
        .context("查询公开皮肤 UUID 失败")?;
    if response.status() == reqwest::StatusCode::NO_CONTENT
        || response.status() == reqwest::StatusCode::NOT_FOUND
    {
        bail!("没有找到该正版玩家名称");
    }
    anyhow::ensure!(
        response.status().is_success(),
        "公开玩家资料服务返回 HTTP {}",
        response.status().as_u16()
    );
    let mut bytes = vec![];
    response.take(65537).read_to_end(&mut bytes)?;
    cancel(cancelled)?;
    anyhow::ensure!(bytes.len() <= 65536, "公开玩家资料响应过大");
    let json: Value = serde_json::from_slice(&bytes).context("公开玩家资料格式无效")?;
    let id = json["id"].as_str().context("公开玩家资料缺少 UUID")?;
    anyhow::ensure!(
        id.len() == 32 && id.bytes().all(|v| v.is_ascii_hexdigit()),
        "公开玩家 UUID 格式无效"
    );
    Ok(id.to_ascii_lowercase())
}
/// Public, unauthenticated skin data for the explicitly configured player name.
/// No account token is sent and no login state is changed.
pub struct PublicSkin {
    pub uuid: String,
    pub png: Option<Vec<u8>>,
}
pub fn fetch_public_skin(name: &str, cancelled: &AtomicBool) -> Result<PublicSkin> {
    let uuid = official_uuid(name, cancelled)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    cancel(cancelled)?;
    let response = client
        .get(format!(
            "https://sessionserver.mojang.com/session/minecraft/profile/{uuid}"
        ))
        .send()
        .context("查询公开皮肤资料失败")?;
    anyhow::ensure!(
        response.status().is_success(),
        "公开皮肤资料服务返回 HTTP {}",
        response.status().as_u16()
    );
    let mut bytes = Vec::new();
    response.take(65537).read_to_end(&mut bytes)?;
    cancel(cancelled)?;
    anyhow::ensure!(bytes.len() <= 65536, "公开皮肤资料响应过大");
    let profile: Value = serde_json::from_slice(&bytes).context("公开皮肤资料格式无效")?;
    let url = public_skin_url(&profile, &uuid)?;
    let png = url
        .map(|url| crate::auth::fetch_skin_png(&url, cancelled))
        .transpose()?;
    cancel(cancelled)?;
    Ok(PublicSkin { uuid, png })
}
fn public_skin_url(profile: &Value, uuid: &str) -> Result<Option<String>> {
    use base64::Engine;
    anyhow::ensure!(
        profile["id"]
            .as_str()
            .is_some_and(|id| id.eq_ignore_ascii_case(uuid)),
        "公开皮肤资料 UUID 不匹配"
    );
    let properties = profile["properties"]
        .as_array()
        .context("公开皮肤资料缺少属性")?;
    let Some(encoded) = properties
        .iter()
        .find(|p| p["name"] == "textures")
        .and_then(|p| p["value"].as_str())
    else {
        return Ok(None);
    };
    anyhow::ensure!(encoded.len() <= 32768, "公开皮肤纹理资料过大");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("公开皮肤纹理编码无效")?;
    let texture: Value = serde_json::from_slice(&bytes).context("公开皮肤纹理资料无效")?;
    if let Some(id) = texture["profileId"].as_str() {
        anyhow::ensure!(id.eq_ignore_ascii_case(uuid), "公开皮肤纹理 UUID 不匹配");
    }
    let Some(raw) = texture
        .pointer("/textures/SKIN/url")
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let mut url = reqwest::Url::parse(raw).context("公开皮肤 URL 无效")?;
    let path = url.path().strip_prefix("/texture/").unwrap_or_default();
    anyhow::ensure!(
        matches!(url.scheme(), "https" | "http")
            && url.host_str() == Some("textures.minecraft.net")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && (32..=64).contains(&path.len())
            && path.bytes().all(|b| b.is_ascii_hexdigit()),
        "公开皮肤只允许官方纹理 CDN"
    );
    // Upstream public texture metadata can contain historical HTTP URLs. Always
    // fetch over HTTPS; never permit a redirect or send an authorization header.
    url.set_scheme("https")
        .map_err(|_| anyhow::anyhow!("公开皮肤协议无效"))?;
    Ok(Some(url.into()))
}
/// Same UUID.hashCode parity used by the source launcher's McSkinSex.
pub fn default_slim(uuid: &str) -> Result<bool> {
    let compact = uuid.replace('-', "");
    anyhow::ensure!(
        compact.len() == 32 && compact.bytes().all(|b| b.is_ascii_hexdigit()),
        "离线 UUID 格式无效"
    );
    let value = u128::from_str_radix(&compact, 16)?;
    Ok(
        ((value as u32) ^ ((value >> 32) as u32) ^ ((value >> 64) as u32) ^ ((value >> 96) as u32))
            & 1
            != 0,
    )
}

pub fn read_skin(path: &Path) -> Result<Vec<u8>> {
    let file = fs::File::open(path).context("读取皮肤 PNG 失败")?;
    anyhow::ensure!(file.metadata()?.len() <= MAX_PNG, "皮肤 PNG 超过 1 MiB");
    let mut bytes = vec![];
    file.take(MAX_PNG + 1).read_to_end(&mut bytes)?;
    validate_png(&bytes)?;
    Ok(bytes)
}
fn validate_png(bytes: &[u8]) -> Result<image::DynamicImage> {
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_PNG && bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "皮肤必须是至多 1 MiB 的 PNG 文件"
    );
    let reader = image::ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    let dimensions = reader.into_dimensions()?;
    anyhow::ensure!(
        matches!(dimensions, (64, 32) | (64, 64)),
        "皮肤尺寸必须为 64×32 或 64×64"
    );
    image::load_from_memory_with_format(bytes, ImageFormat::Png).context("皮肤 PNG 数据损坏")
}
fn pack_format(root: &Path, vanilla: &str, version: Option<(u32, u32)>) -> Result<u32> {
    let path = confined_path(
        root,
        &Path::new("versions")
            .join(vanilla)
            .join(format!("{vanilla}.jar")),
    )?;
    if path.is_file() {
        let mut jar = ZipArchive::new(fs::File::open(&path)?).context("游戏 JAR 不是有效 ZIP")?;
        if let Ok(mut metadata) = jar.by_name("version.json") {
            anyhow::ensure!(metadata.size() <= 65536, "游戏资源包版本资料过大");
            let mut bytes = vec![];
            metadata.read_to_end(&mut bytes)?;
            let json: Value = serde_json::from_slice(&bytes)?;
            if let Some(format) = json
                .pointer("/pack_version/resource")
                .or_else(|| json.pointer("/pack_version/resource_major"))
                .and_then(Value::as_u64)
                .filter(|v| *v > 0 && *v < 10000)
            {
                return Ok(format as u32);
            }
        };
    }
    match version {
        Some((6..=8, _)) => Ok(1),
        Some((9..=10, _)) => Ok(2),
        Some((11..=12, _)) => Ok(3),
        Some((13..=14, _)) => Ok(4),
        Some((15, _)) => Ok(5),
        Some((16, _)) => Ok(6),
        Some((17, _)) => Ok(7),
        Some((18, _)) => Ok(8),
        Some((19, 0..=2)) => Ok(9),
        Some((19, 3)) => Ok(12),
        Some((19, 4)) => Ok(13),
        Some((20, 0..=1)) => Ok(15),
        Some((20, 2)) => Ok(18),
        Some((20, 3..=4)) => Ok(22),
        _ => bail!("无法从游戏 JAR 确定资源包格式，未修改皮肤设置"),
    }
}
fn skin_pack(update: &SkinUpdate) -> Result<Option<(Vec<u8>, bool)>> {
    let SkinUpdate::Activate {
        png,
        slim,
        modern,
        legacy_crop,
        pack_format,
        new_options,
    } = update
    else {
        return Ok(None);
    };
    let mut image = validate_png(png)?;
    if *legacy_crop && image.dimensions() == (64, 64) {
        image = image.crop_imm(0, 0, 64, 32);
    }
    let mut encoded = Cursor::new(vec![]);
    image.write_to(&mut encoded, ImageFormat::Png)?;
    let mut zip = ZipWriter::new(Cursor::new(vec![]));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("pack.mcmeta", options)?;
    // Since resource format 65 (1.21.9), min_format/max_format replace the
    // legacy required pack_format field; integer maxima include all minors.
    let mut pack = serde_json::json!({"description":"PCL Rust 自定义离线皮肤"});
    if *pack_format >= 65 {
        pack["min_format"] = (*pack_format).into();
        pack["max_format"] = (*pack_format).into();
    } else {
        pack["pack_format"] = (*pack_format).into();
    }
    zip.write_all(serde_json::to_vec(&serde_json::json!({"pack": pack}))?.as_slice())?;
    let names = if *modern {
        [
            "alex", "ari", "efe", "kai", "makena", "noor", "steve", "sunny", "zuri",
        ]
        .into_iter()
        .map(|name| {
            format!(
                "assets/minecraft/textures/entity/player/{}/{name}.png",
                if *slim { "slim" } else { "wide" }
            )
        })
        .collect::<Vec<_>>()
    } else {
        vec![format!(
            "assets/minecraft/textures/entity/{}.png",
            if *slim && !*legacy_crop {
                "alex"
            } else {
                "steve"
            }
        )]
    };
    for name in names {
        zip.start_file(name, options)?;
        zip.write_all(encoded.get_ref())?;
    }
    Ok(Some((zip.finish()?.into_inner(), *new_options)))
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    filename: String,
    sha256: String,
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn optional(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(m) => {
            anyhow::ensure!(
                m.is_file() && !m.file_type().is_symlink() && m.len() <= limit,
                "皮肤设置路径被占用或过大：{}",
                path.display()
            );
            let mut bytes = Vec::new();
            fs::File::open(path)?
                .take(limit + 1)
                .read_to_end(&mut bytes)?;
            anyhow::ensure!(bytes.len() as u64 <= limit, "设置文件在读取时增长，未覆盖");
            Ok(Some(bytes))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn replace(root: &Path, relative: &Path, before: Option<&[u8]>, bytes: &[u8]) -> Result<()> {
    let path = confined_path(root, relative)?;
    anyhow::ensure!(
        optional(&path, MAX_OPTIONS)?.as_deref() == before,
        "皮肤设置已被其他程序修改，未覆盖"
    );
    let mut staged = tempfile::NamedTempFile::new_in(path.parent().context("缺少父目录")?)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    anyhow::ensure!(
        optional(&confined_path(root, relative)?, MAX_OPTIONS)?.as_deref() == before,
        "提交前皮肤设置发生变化，未覆盖"
    );
    if before.is_some() {
        staged.persist(path).map_err(|e| e.error)?;
    } else {
        staged.persist_noclobber(path).map_err(|e| e.error)?;
    }
    Ok(())
}
fn directory(root: &Path, relative: &str) -> Result<()> {
    let path = confined_path(root, Path::new(relative))?;
    match fs::create_dir(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            anyhow::ensure!(fs::symlink_metadata(&path)?.is_dir(), "皮肤目录被文件占用");
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}
/// Add the initial game language without replacing an existing user choice.
/// Uses the same confined, bounded, atomic options writer as offline skins.
pub fn set_initial_language(
    game_dir: &Path,
    language: &str,
    cancelled: &AtomicBool,
) -> Result<bool> {
    anyhow::ensure!(matches!(language, "zh_cn" | "zh_CN"), "无效的游戏语言代码");
    let _guard = WRITE_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("游戏设置写入锁不可用"))?;
    cancel(cancelled)?;
    let root = game_dir.canonicalize().context("实例游戏目录不存在")?;
    let relative = Path::new("options.txt");
    let before = optional(&confined_path(&root, relative)?, MAX_OPTIONS)?;
    let text = std::str::from_utf8(before.as_deref().unwrap_or_default())
        .context("游戏 options.txt 不是有效 UTF-8，未改写")?;
    if text
        .lines()
        .any(|line| line.split_once(':').is_some_and(|(key, _)| key == "lang"))
    {
        return Ok(false);
    }
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut updated = text.to_owned();
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push_str(newline);
    }
    updated.push_str(&format!("lang:{language}{newline}"));
    cancel(cancelled)?;
    replace(&root, relative, before.as_deref(), updated.as_bytes())?;
    Ok(true)
}

/// Atomically replace options only after an immutable content-addressed pack exists.
/// Unknown packs, source PNGs and other option lines are never deleted.
pub fn apply(game_dir: &Path, update: &SkinUpdate, cancelled: &AtomicBool) -> Result<()> {
    let _guard = WRITE_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("皮肤写入锁不可用"))?;
    cancel(cancelled)?;
    let root = game_dir.canonicalize().context("实例游戏目录不存在")?;
    let receipt_rel = Path::new("PCL-Rust/offline-skin.json");
    let receipt_path = confined_path(&root, receipt_rel)?;
    let prior_receipt = optional(&receipt_path, 65536)?;
    let prior = prior_receipt
        .as_deref()
        .map(serde_json::from_slice::<Receipt>)
        .transpose()
        .context("皮肤管理记录损坏；未自动覆盖")?;
    if matches!(update, SkinUpdate::Disable) && prior.is_none() {
        return Ok(());
    }
    if let Some(prior) = &prior {
        anyhow::ensure!(
            prior.filename == format!("PCL-Rust-Skin-{}.zip", prior.sha256)
                && prior.sha256.len() == 64
                && prior.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "皮肤管理记录无效"
        );
        let path = confined_path(&root, &Path::new("resourcepacks").join(&prior.filename))?;
        if let Some(bytes) = optional(&path, MAX_OPTIONS)? {
            anyhow::ensure!(
                sha(&bytes) == prior.sha256,
                "原皮肤资源包已被修改，保留现有设置"
            );
        }
    }
    let generated = skin_pack(update)?;
    let next = generated.as_ref().map(|(bytes, _)| Receipt {
        filename: format!("PCL-Rust-Skin-{}.zip", sha(bytes)),
        sha256: sha(bytes),
    });
    let options_rel = Path::new("options.txt");
    let options_path = confined_path(&root, options_rel)?;
    let before = optional(&options_path, MAX_OPTIONS)?;
    let body = std::str::from_utf8(before.as_deref().unwrap_or_default())
        .context("options.txt 不是 UTF-8，未覆盖")?;
    let after = rewrite_options(
        body,
        prior.as_ref().map(|r| r.filename.as_str()),
        next.as_ref().map(|r| r.filename.as_str()),
        generated.as_ref().is_some_and(|(_, modern)| *modern),
    )?;
    directory(&root, "PCL-Rust")?;
    if let (Some((bytes, _)), Some(next)) = (&generated, &next) {
        directory(&root, "resourcepacks")?;
        let relative = Path::new("resourcepacks").join(&next.filename);
        let path = confined_path(&root, &relative)?;
        if let Some(existing) = optional(&path, MAX_OPTIONS)? {
            anyhow::ensure!(&existing == bytes, "皮肤包同名内容不符，未覆盖");
        } else {
            replace(&root, &relative, None, bytes)?;
        }
    }
    cancel(cancelled)?;
    // Receipt first: should options commit fail, restore only our own exact bytes.
    // A retained immutable pack is safe and can be reused on retry.
    let receipt_bytes = serde_json::to_vec(
        &next
            .as_ref()
            .map(|r| Receipt {
                filename: r.filename.clone(),
                sha256: r.sha256.clone(),
            })
            .unwrap_or_else(|| Receipt {
                filename: String::new(),
                sha256: String::new(),
            }),
    )?;
    if next.is_some() {
        replace(&root, receipt_rel, prior_receipt.as_deref(), &receipt_bytes)?;
    }
    if let Err(error) = replace(&root, options_rel, before.as_deref(), after.as_bytes()) {
        if next.is_some() {
            if let Some(prior) = &prior_receipt {
                replace(&root, receipt_rel, Some(&receipt_bytes), prior)
                    .context("options 写入失败且管理记录回滚失败")?;
            } else if optional(&receipt_path, 65536)?.as_deref() == Some(receipt_bytes.as_slice()) {
                fs::remove_file(&receipt_path)?;
            }
        }
        return Err(error);
    }
    if next.is_none() && prior_receipt.is_some() {
        anyhow::ensure!(
            optional(&receipt_path, 65536)? == prior_receipt,
            "皮肤管理记录并发变化；options 已更新，管理记录未删除"
        );
        fs::remove_file(receipt_path)?;
    }
    Ok(())
}
fn rewrite_options(
    body: &str,
    prior: Option<&str>,
    next: Option<&str>,
    modern: bool,
) -> Result<String> {
    let newline = if body.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = body.lines().map(str::to_owned).collect();
    let indexes: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.starts_with("resourcePacks:").then_some(i))
        .collect();
    anyhow::ensure!(
        indexes.len() <= 1,
        "options.txt 有重复 resourcePacks 项，未覆盖"
    );
    let index = indexes.first().copied();
    let mut packs: Vec<String> = if let Some(i) = index {
        serde_json::from_str(lines[i].split_once(':').unwrap().1)
            .context("resourcePacks 不是有效列表")?
    } else {
        vec![]
    };
    if let Some(prior) = prior {
        packs.retain(|v| v != prior && v != &format!("file/{prior}"));
    }
    if let Some(next) = next {
        if modern && packs.is_empty() {
            packs.push("vanilla".into());
        }
        let name = if modern {
            format!("file/{next}")
        } else {
            next.into()
        };
        if !packs.contains(&name) {
            packs.push(name);
        }
    }
    let line = format!("resourcePacks:{}", serde_json::to_string(&packs)?);
    if let Some(i) = index {
        lines[i] = line
    } else if next.is_some() {
        lines.push(line)
    }
    let mut output = lines.join(newline);
    if !output.is_empty() {
        output.push_str(newline)
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    #[test]
    fn public_skin_metadata_is_bound_to_uuid_and_official_https_cdn() {
        use base64::Engine;
        let uuid = "0".repeat(32);
        let profile = |id: &str, url: &str| serde_json::json!({"id":id,"properties":[{"name":"textures","value":base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&serde_json::json!({"profileId":id,"textures":{"SKIN":{"url":url}}})).unwrap())}]});
        let url = format!("http://textures.minecraft.net/texture/{}", "a".repeat(64));
        assert_eq!(
            public_skin_url(&profile(&uuid, &url), &uuid).unwrap(),
            Some(url.replacen("http:", "https:", 1))
        );
        assert!(public_skin_url(&profile(&"1".repeat(32), &url), &uuid).is_err());
        assert!(public_skin_url(
            &profile(
                &uuid,
                &url.replace("textures.minecraft.net", "evil.example")
            ),
            &uuid
        )
        .is_err());
        assert!(public_skin_url(&profile(&uuid, &format!("{url}?secret=x")), &uuid).is_err());
        assert_eq!(
            public_skin_url(&serde_json::json!({"id":uuid,"properties":[]}), &uuid).unwrap(),
            None
        );
    }
    #[test]
    fn default_model_matches_java_uuid_hash_parity() {
        assert!(!default_slim(&"0".repeat(32)).unwrap());
        assert!(default_slim("00000000000000000000000000000001").unwrap());
        assert!(!default_slim("00000001000000000000000000000001").unwrap());
        assert!(default_slim("not-a-uuid").is_err());
    }

    use super::*;
    fn png() -> Vec<u8> {
        let mut data = Cursor::new(vec![]);
        image::DynamicImage::new_rgba8(64, 64)
            .write_to(&mut data, ImageFormat::Png)
            .unwrap();
        data.into_inner()
    }
    fn update() -> SkinUpdate {
        SkinUpdate::Activate {
            png: png(),
            slim: false,
            modern: true,
            legacy_crop: false,
            pack_format: 34,
            new_options: true,
        }
    }
    #[test]
    fn uuid_modes_reproduce_original_parity_without_changing_the_name() {
        let uuid = "b50ad385829d3141a2167e7d7539ba7f";
        for slim in [false, true] {
            let result = uuid_for_model(uuid, slim).unwrap();
            let x = u128::from_str_radix(&result, 16).unwrap();
            assert_eq!(
                ((x as u32) ^ ((x >> 32) as u32) ^ ((x >> 64) as u32) ^ ((x >> 96) as u32)) & 1,
                u32::from(slim)
            );
            assert_eq!(&result[..27], &uuid[..27]);
        }
        assert!(uuid_for_model("x", false).is_err());
    }
    #[test]
    fn custom_pack_has_all_modern_models_and_old_crop() {
        let (bytes, _) = skin_pack(&update()).unwrap().unwrap();
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(archive.len(), 10);
        assert!(archive
            .by_name("assets/minecraft/textures/entity/player/wide/zuri.png")
            .is_ok());
        let update = SkinUpdate::Activate {
            png: png(),
            slim: true,
            modern: false,
            legacy_crop: true,
            pack_format: 1,
            new_options: false,
        };
        let (bytes, _) = skin_pack(&update).unwrap().unwrap();
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut data = vec![];
        archive
            .by_name("assets/minecraft/textures/entity/steve.png")
            .unwrap()
            .read_to_end(&mut data)
            .unwrap();
        assert_eq!(validate_png(&data).unwrap().dimensions(), (64, 32));
    }
    #[test]
    fn new_pack_format_uses_required_major_range() {
        let mut value = update();
        if let SkinUpdate::Activate { pack_format, .. } = &mut value {
            *pack_format = 97;
        }
        let (bytes, _) = skin_pack(&value).unwrap().unwrap();
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let metadata: Value =
            serde_json::from_reader(archive.by_name("pack.mcmeta").unwrap()).unwrap();
        assert_eq!(metadata["pack"]["min_format"], 97);
        assert_eq!(metadata["pack"]["max_format"], 97);
        assert!(metadata["pack"].get("pack_format").is_none());
    }
    #[test]
    fn initial_language_preserves_existing_choice_bytes_and_cancel() {
        let root = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        assert!(set_initial_language(root.path(), "zh_CN", &cancel).unwrap());
        assert_eq!(
            fs::read(root.path().join("options.txt")).unwrap(),
            b"lang:zh_CN\n"
        );
        fs::write(
            root.path().join("options.txt"),
            "volume:0.5\r\nlang:en_us\r\ncustom:x:y",
        )
        .unwrap();
        let before = fs::read(root.path().join("options.txt")).unwrap();
        assert!(!set_initial_language(root.path(), "zh_cn", &cancel).unwrap());
        assert_eq!(fs::read(root.path().join("options.txt")).unwrap(), before);
        fs::write(root.path().join("options.txt"), "volume:0.5\r\ncustom:x:y").unwrap();
        assert!(set_initial_language(root.path(), "zh_cn", &AtomicBool::new(true)).is_err());
        assert!(set_initial_language(root.path(), "zh_cn", &cancel).unwrap());
        assert_eq!(
            fs::read_to_string(root.path().join("options.txt")).unwrap(),
            "volume:0.5\r\ncustom:x:y\r\nlang:zh_cn\r\n"
        );
    }
    #[test]
    fn activate_disable_preserves_unknown_options_and_user_packs() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("options.txt"),
            "volume:0.6\r\nresourcePacks:[\"file/User.zip\"]\r\ncustom:with:colon\r\n",
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        apply(root.path(), &update(), &cancel).unwrap();
        let options = fs::read_to_string(root.path().join("options.txt")).unwrap();
        assert!(options.contains("file/User.zip"));
        assert!(options.contains("custom:with:colon\r\n"));
        apply(root.path(), &SkinUpdate::Disable, &cancel).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("options.txt")).unwrap(),
            "volume:0.6\r\nresourcePacks:[\"file/User.zip\"]\r\ncustom:with:colon\r\n"
        );
        assert!(!root.path().join("PCL-Rust/offline-skin.json").exists());
    }
    #[test]
    fn cancellation_corrupt_options_and_modified_pack_never_replace_user_data() {
        let root = tempfile::tempdir().unwrap();
        assert!(apply(root.path(), &update(), &AtomicBool::new(true)).is_err());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        fs::write(root.path().join("options.txt"), "resourcePacks:broken\n").unwrap();
        assert!(apply(root.path(), &update(), &AtomicBool::new(false)).is_err());
        assert_eq!(
            fs::read_to_string(root.path().join("options.txt")).unwrap(),
            "resourcePacks:broken\n"
        );
        fs::remove_file(root.path().join("options.txt")).unwrap();
        apply(root.path(), &update(), &AtomicBool::new(false)).unwrap();
        let receipt: Receipt = serde_json::from_slice(
            &fs::read(root.path().join("PCL-Rust/offline-skin.json")).unwrap(),
        )
        .unwrap();
        fs::write(
            root.path().join("resourcepacks").join(receipt.filename),
            b"user change",
        )
        .unwrap();
        let before = fs::read(root.path().join("options.txt")).unwrap();
        assert!(apply(root.path(), &SkinUpdate::Disable, &AtomicBool::new(false)).is_err());
        assert_eq!(fs::read(root.path().join("options.txt")).unwrap(), before);
    }
    #[cfg(unix)]
    #[test]
    fn symlink_target_is_not_followed() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(other.path(), root.path().join("resourcepacks")).unwrap();
        assert!(apply(root.path(), &update(), &AtomicBool::new(false)).is_err());
        assert_eq!(fs::read_dir(other.path()).unwrap().count(), 0);
    }
    #[test]
    fn online_session_never_reads_offline_files_or_changes_account_data() {
        let root = tempfile::tempdir().unwrap();
        let settings = Settings {
            offline_skin_mode: OfflineSkinMode::Custom,
            offline_skin_path: Some(root.path().join("missing.png")),
            ..Settings::default()
        };
        let session = Session {
            username: "Fixture".into(),
            uuid: "1".repeat(32),
            access_token: "fixture-not-real".into(),
            user_type: "msa".into(),
        };
        let prepared = prepare(
            root.path(),
            "missing-version",
            &settings,
            &session,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(prepared.session.uuid, session.uuid);
        assert!(matches!(prepared.update, SkinUpdate::Disable));
        assert!(prepared.warnings.is_empty());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
