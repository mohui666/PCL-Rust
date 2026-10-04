//! Access tokens stay in memory. Refresh tokens may be persisted by `accounts` in the OS vault.
//! Credentials are never included in Debug or errors.
//! Device flow: https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-device-code
//! Polling behavior: https://www.rfc-editor.org/rfc/rfc8628#section-3.5
//! Xbox exchange: https://learn.microsoft.com/en-us/gaming/gdk/docs/services/fundamentals/s2s-auth-calls/service-authentication/live-website-authentication

use crate::model::Session;
use anyhow::{bail, Context, Result};
use md5::{Digest, Md5};
use reqwest::blocking::{Client, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEVICE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const XBOX_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MINECRAFT_LOGIN_URL: &str =
    "https://api.minecraftservices.com/authentication/login_with_xbox";
const ENTITLEMENTS_URL: &str = "https://api.minecraftservices.com/entitlements/mcstore";
const PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";

/// Real login boundaries and the fixed PCL 2.13.1.1 progress values. These are
/// stage weights, not elapsed-time estimates; Complete is emitted only after save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginStage {
    Microsoft,
    Xbox,
    Xsts,
    Minecraft,
    Ownership,
    Profile,
    Saving,
    Complete,
}
impl LoginStage {
    pub fn percent(self) -> u8 {
        match self {
            Self::Microsoft => 5,
            Self::Xbox => 25,
            Self::Xsts => 40,
            Self::Minecraft => 55,
            Self::Ownership => 70,
            Self::Profile => 85,
            Self::Saving => 98,
            Self::Complete => 100,
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::Microsoft => "正在进行微软登录（1/6）…",
            Self::Xbox => "正在验证 Xbox 账户（2/6）…",
            Self::Xsts => "正在验证 Xbox 权限（3/6）…",
            Self::Minecraft => "正在登录 Minecraft（4/6）…",
            Self::Ownership => "正在验证游戏所有权（5/6）…",
            Self::Profile => "正在获取玩家资料（6/6）…",
            Self::Saving => "正在安全保存登录信息…",
            Self::Complete => "正版登录成功",
        }
    }
}

/// Only classified public error information reaches the UI. Server bodies, tokens,
/// user identifiers and error_description are deliberately not stored in this type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthenticationIssue {
    AuthorizationDeclined,
    CodeExpired,
    ReauthenticationRequired,
    ClientConfiguration,
    XboxProfileRequired,
    FamilyPermissionRequired,
    RegionUnavailable,
    Suspended,
    SecurityInterrupt,
    PasswordLoginRequired,
    OwnershipRequired,
    MinecraftProfileRequired,
    RateLimited,
    ServiceUnavailable,
    MinecraftAccessDenied,
    SessionUnauthorized,
    NetworkUnavailable,
}
impl std::fmt::Display for AuthenticationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::AuthorizationDeclined => "你拒绝了本应用申请的登录权限。",
            Self::CodeExpired => "登录用时太长，设备代码已过期，请重新登录。",
            Self::ReauthenticationRequired => "Microsoft 登录凭据已失效或授权已撤销，请重新登录。",
            Self::ClientConfiguration => "Microsoft 应用配置无效，请检查 Client ID、个人账户及公共客户端设置。",
            Self::XboxProfileRequired => "你尚未创建 Xbox 账户资料，请先注册 Xbox 资料再登录。",
            Self::FamilyPermissionRequired => "此账号的年龄或家庭权限不满足 Xbox 登录要求，请由账户持有人或家庭组织者检查设置。",
            Self::RegionUnavailable => "Xbox 登录服务在当前国家或地区不可用。",
            Self::Suspended => "此账号已被登录服务限制或封禁，无法登录。",
            Self::SecurityInterrupt => "此账号需要完成 Microsoft 安全检查，请前往微软账户页处理。",
            Self::PasswordLoginRequired => "请在登录网页选择“其他登录方法”，再选择“使用我的密码”；若尚未设置密码，请先设置密码。",
            Self::OwnershipRequired => "你尚未购买 Minecraft Java 版，或 Xbox Game Pass 已到期。",
            Self::MinecraftProfileRequired => "请先创建 Minecraft 玩家档案，然后再重新登录。",
            Self::RateLimited => "登录尝试太过频繁，请等待几分钟后再试。",
            Self::ServiceUnavailable => "Minecraft 登录服务暂时不可用，请稍后再试。",
            Self::MinecraftAccessDenied => "Minecraft 登录服务拒绝了此应用的请求（HTTP 403）。请检查应用配置及 Minecraft API 访问资格；仅凭 403 无法确定是应用审核、配置还是服务端访问限制。",
            Self::SessionUnauthorized => "Minecraft 会话已失效，需要重新登录。",
            Self::NetworkUnavailable => "无法连接登录服务，请检查网络连接后重试。",
        })
    }
}
impl std::error::Error for AuthenticationIssue {}

/// A device code is a credential. Intentionally does not implement Debug or Serialize.
#[derive(Clone, Deserialize)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    pub device_code: String,
    #[serde(default = "default_interval")]
    pub interval: u64,
    pub expires_in: u64,
    #[serde(skip, default = "Instant::now")]
    issued_at: Instant,
}

fn default_interval() -> u64 {
    5
}

fn valid_player_name(name: &str) -> bool {
    (1..=16).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Java's UUID.nameUUIDFromBytes("OfflinePlayer:" + name), without a namespace.
pub fn offline_session(name: &str) -> Result<Session> {
    if !valid_player_name(name) {
        bail!("离线用户名须为 1–16 个 ASCII 字母、数字或下划线");
    }
    let mut bytes: [u8; 16] = Md5::digest(format!("OfflinePlayer:{name}").as_bytes()).into();
    bytes[6] = (bytes[6] & 0x0f) | 0x30;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let uuid = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(Session {
        username: name.into(),
        uuid,
        access_token: "0".into(),
        user_type: "legacy".into(),
    })
}

pub(crate) fn validate_client_id(client_id: &str) -> Result<()> {
    if client_id.len() != 36
        || !client_id.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Err(AuthenticationIssue::ClientConfiguration)
            .context("请先填写你注册的 Microsoft 应用 Client ID（UUID 格式）");
    }
    Ok(())
}

fn http_client() -> Result<Client> {
    Client::builder()
        .user_agent("PCL-Rust/0.1")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        // Credentials must never be forwarded through unexpected redirects.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("无法创建登录网络客户端")
}

pub fn begin_device_login(client_id: &str) -> Result<DeviceCode> {
    validate_client_id(client_id)?;
    let issued_at = Instant::now();
    let response = http_client()?
        .post(DEVICE_URL)
        .form(&[
            ("client_id", client_id),
            ("scope", "XboxLive.signin offline_access"),
        ])
        .send()
        .map_err(|_| anyhow::anyhow!("无法连接 Microsoft 设备登录服务"))?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = read_error_body(response, "Microsoft 设备登录")?;
        if let Some(issue) = oauth_issue(&poll_result(&body)) {
            return Err(issue.into());
        }
        bail!("Microsoft 设备登录失败（HTTP {status}）");
    }
    let mut code: DeviceCode = read_json(response, "Microsoft 设备登录")?;
    if code.device_code.is_empty() || code.user_code.is_empty() || code.expires_in == 0 {
        bail!("Microsoft 返回了不完整的设备登录信息");
    }
    let verification = reqwest::Url::parse(&code.verification_uri)
        .map_err(|_| anyhow::anyhow!("Microsoft 返回了无效的登录地址"))?;
    if verification.scheme() != "https"
        || !matches!(
            verification.host_str(),
            Some("microsoft.com" | "www.microsoft.com" | "login.microsoftonline.com")
        )
    {
        bail!("Microsoft 返回了非预期的登录地址");
    }
    code.interval = code.interval.max(1);
    code.issued_at = issued_at;
    Ok(code)
}

#[derive(Deserialize)]
struct AccessToken {
    access_token: String,
    expires_in: u64,
}

// Neither token type implements Debug or Serialize.
#[derive(Deserialize)]
pub(crate) struct MicrosoftToken {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
    expires_in: u64,
}

pub(crate) struct AuthenticatedAccount {
    pub(crate) profile: MinecraftProfile,
    pub(crate) session: Session,
    pub(crate) expires_at: u64,
}

pub(crate) fn unix_time() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("系统时间早于 Unix 纪元，请校准时间")?
        .as_secs())
}

fn token_expiry(expires_in: u64, issued_at: u64) -> Result<u64> {
    if expires_in == 0 {
        bail!("登录服务返回了已过期的会话，请重新登录");
    }
    issued_at
        .checked_add(expires_in)
        .context("登录服务返回了无效的会话有效期")
}

fn validate_microsoft_token(token: &MicrosoftToken) -> Result<()> {
    if token.access_token.is_empty() {
        bail!("Microsoft 未返回访问令牌");
    }
    token_expiry(token.expires_in, unix_time()?)?;
    Ok(())
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: String,
}

#[derive(Debug, PartialEq, Eq)]
enum PollResult {
    Pending,
    SlowDown,
    Denied,
    Expired,
    InvalidCode,
    InvalidClient,
    SecurityInterrupt,
    Suspended,
    PasswordRequired,
    Other,
}

fn poll_result(body: &[u8]) -> PollResult {
    let Ok(error) = serde_json::from_slice::<OAuthError>(body) else {
        return PollResult::Other;
    };
    let description = error.error_description.to_ascii_lowercase();
    if description.contains("account security interrupt") {
        return PollResult::SecurityInterrupt;
    }
    if description.contains("service abuse") {
        return PollResult::Suspended;
    }
    if description.contains("aadsts70000") {
        return PollResult::PasswordRequired;
    }
    match error.error.as_str() {
        "authorization_pending" => PollResult::Pending,
        "slow_down" => PollResult::SlowDown,
        "authorization_declined" | "access_denied" => PollResult::Denied,
        "expired_token" => PollResult::Expired,
        "bad_verification_code" | "invalid_grant" => PollResult::InvalidCode,
        "invalid_client" | "unauthorized_client" | "invalid_scope" => PollResult::InvalidClient,
        _ => PollResult::Other,
    }
}
fn oauth_issue(result: &PollResult) -> Option<AuthenticationIssue> {
    Some(match result {
        PollResult::Denied => AuthenticationIssue::AuthorizationDeclined,
        PollResult::Expired => AuthenticationIssue::CodeExpired,
        PollResult::InvalidCode => AuthenticationIssue::ReauthenticationRequired,
        PollResult::InvalidClient => AuthenticationIssue::ClientConfiguration,
        PollResult::SecurityInterrupt => AuthenticationIssue::SecurityInterrupt,
        PollResult::Suspended => AuthenticationIssue::Suspended,
        PollResult::PasswordRequired => AuthenticationIssue::PasswordLoginRequired,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct AuthenticationCancelled;
impl std::fmt::Display for AuthenticationCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("登录已取消")
    }
}
impl std::error::Error for AuthenticationCancelled {}

pub(crate) fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(AuthenticationCancelled.into());
    }
    Ok(())
}

fn check_device_lifetime(code: &DeviceCode, cancel: &AtomicBool) -> Result<()> {
    check_cancel(cancel)?;
    if code.issued_at.elapsed() >= Duration::from_secs(code.expires_in) {
        return Err(AuthenticationIssue::CodeExpired.into());
    }
    Ok(())
}

fn wait_poll_interval(seconds: u64, code: &DeviceCode, cancel: &AtomicBool) -> Result<()> {
    let started = Instant::now();
    let duration = Duration::from_secs(seconds);
    loop {
        check_device_lifetime(code, cancel)?;
        let Some(remaining) = duration.checked_sub(started.elapsed()) else {
            return Ok(());
        };
        if remaining.is_zero() {
            return Ok(());
        }
        thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

/// The caller supplies its own registered public-client application ID.
/// Polling is cancellable; a request already in flight is bounded by its timeout.
pub fn complete_device_login(
    client_id: &str,
    code: &DeviceCode,
    cancel: &AtomicBool,
    progress: impl Fn(String),
) -> Result<Session> {
    let token = device_tokens(client_id, code, cancel, &progress)?;
    Ok(exchange_minecraft(&token.access_token, cancel, &progress)?.session)
}

pub(crate) fn device_tokens(
    client_id: &str,
    code: &DeviceCode,
    cancel: &AtomicBool,
    progress: &dyn Fn(String),
) -> Result<MicrosoftToken> {
    validate_client_id(client_id)?;
    let client = http_client()?;
    let mut interval = code.interval.max(1);
    progress("等待在 Microsoft 页面完成登录…".into());
    let token = loop {
        wait_poll_interval(interval, code, cancel)?;
        let remaining = Duration::from_secs(code.expires_in)
            .checked_sub(code.issued_at.elapsed())
            .context("设备登录代码已过期，请重新登录")?;
        let response = match client
            .post(TOKEN_URL)
            .timeout(remaining.min(Duration::from_secs(20)))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", code.device_code.as_str()),
            ])
            .send()
        {
            Ok(response) => response,
            Err(error) if error.is_timeout() => {
                // RFC 8628 requires a reduced polling frequency on timeout.
                interval = interval.saturating_mul(2);
                continue;
            }
            Err(_) => return Err(AuthenticationIssue::NetworkUnavailable.into()),
        };
        check_device_lifetime(code, cancel)?;
        if response.status().is_success() {
            let token: MicrosoftToken = read_json(response, "Microsoft 登录")?;
            validate_microsoft_token(&token)?;
            break token;
        }
        let status = response.status();
        let body = read_error_body(response, "Microsoft 登录")?;
        let result = poll_result(&body);
        if let Some(issue) = oauth_issue(&result) {
            return Err(issue.into());
        }
        match result {
            PollResult::Pending => continue,
            PollResult::SlowDown => interval = interval.saturating_add(5),
            _ if status.as_u16() == 429 => interval = interval.saturating_add(5),
            _ => bail!("Microsoft 登录失败（HTTP {}）", status.as_u16()),
        }
    };

    Ok(token)
}

/// Microsoft rotates refresh tokens. The caller persists a new refresh token
/// before invoking Xbox/Minecraft, including when a later step is cancelled.
pub(crate) fn refresh_microsoft(
    client_id: &str,
    refresh_token: &str,
    cancel: &AtomicBool,
) -> Result<MicrosoftToken> {
    validate_client_id(client_id)?;
    check_cancel(cancel)?;
    if refresh_token.is_empty() {
        bail!("账户没有保存的登录凭据，请重新登录");
    }
    let response = http_client()?
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
            ("scope", "XboxLive.signin offline_access"),
        ])
        .send()
        .map_err(|_| AuthenticationIssue::NetworkUnavailable)?;
    parse_refresh_response(response)
}

fn parse_refresh_response(response: Response) -> Result<MicrosoftToken> {
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = read_error_body(response, "Microsoft 刷新登录")?;
        if let Some(issue) = oauth_issue(&poll_result(&body)) {
            return Err(issue.into());
        }
        bail!("Microsoft 刷新登录失败（HTTP {status}），请重试或重新登录");
    }
    let token: MicrosoftToken = read_json(response, "Microsoft 刷新登录")?;
    validate_microsoft_token(&token)?;
    Ok(token)
}

pub(crate) fn exchange_minecraft(
    microsoft_token: &str,
    cancel: &AtomicBool,
    progress: &dyn Fn(String),
) -> Result<AuthenticatedAccount> {
    exchange_minecraft_with_stage(microsoft_token, cancel, &|stage| {
        progress(stage.message().into())
    })
}

pub(crate) fn exchange_minecraft_with_stage(
    microsoft_token: &str,
    cancel: &AtomicBool,
    progress: &dyn Fn(LoginStage),
) -> Result<AuthenticatedAccount> {
    let client = http_client()?;
    check_cancel(cancel)?;
    progress(LoginStage::Xbox);
    let xbox: XboxToken = post_json(
        &client,
        XBOX_URL,
        &json!({
            "Properties": {"AuthMethod": "RPS", "SiteName": "user.auth.xboxlive.com", "RpsTicket": format!("d={microsoft_token}")},
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT"
        }),
        "Xbox 账户验证",
    )?;
    if xbox.token.is_empty() {
        bail!("Xbox 未返回登录令牌");
    }

    check_cancel(cancel)?;
    progress(LoginStage::Xsts);
    let xsts: XboxToken = post_json(
        &client,
        XSTS_URL,
        &json!({
            "Properties": {"SandboxId": "RETAIL", "UserTokens": [xbox.token]},
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT"
        }),
        "Xbox XSTS 验证（请确认已创建 Xbox 资料及家庭账户权限）",
    )?;
    let user_hash = xsts
        .display_claims
        .xui
        .first()
        .map(|claim| claim.uhs.as_str())
        .filter(|value| !value.is_empty())
        .context("Xbox 未返回用户标识")?;
    if xsts.token.is_empty() {
        bail!("Xbox XSTS 未返回登录令牌");
    }

    check_cancel(cancel)?;
    progress(LoginStage::Minecraft);
    let issued_at = unix_time()?;
    let minecraft: AccessToken = post_json(
        &client,
        MINECRAFT_LOGIN_URL,
        &json!({"identityToken": format!("XBL3.0 x={user_hash};{}", xsts.token)}),
        "Minecraft 登录",
    )?;
    if minecraft.access_token.is_empty() {
        bail!("Minecraft 未返回访问令牌");
    }

    check_cancel(cancel)?;
    progress(LoginStage::Ownership);
    let entitlements: Entitlements = authenticated_get(
        &client,
        ENTITLEMENTS_URL,
        &minecraft.access_token,
        "Minecraft 所有权查询",
    )?;
    if !owns_minecraft(&entitlements) {
        return Err(AuthenticationIssue::OwnershipRequired.into());
    }

    check_cancel(cancel)?;
    progress(LoginStage::Profile);
    let profile: MinecraftProfile = authenticated_get(
        &client,
        PROFILE_URL,
        &minecraft.access_token,
        "Minecraft 玩家资料查询（请确认已创建 Java 版角色）",
    )?;
    validate_profile(&profile)?;
    let expires_at = token_expiry(minecraft.expires_in, issued_at)?;
    if expires_at <= unix_time()? {
        bail!("Minecraft 会话已过期，请重试登录");
    }
    check_cancel(cancel)?;
    Ok(AuthenticatedAccount {
        session: Session {
            username: profile.name.clone(),
            uuid: profile.id.to_ascii_lowercase(),
            access_token: minecraft.access_token,
            user_type: "msa".into(),
        },
        profile,
        expires_at,
    })
}

#[derive(Deserialize)]
struct XboxToken {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims", default)]
    display_claims: XboxClaims,
}

#[derive(Default, Deserialize)]
struct XboxClaims {
    #[serde(default)]
    xui: Vec<XboxUser>,
}

#[derive(Deserialize)]
struct XboxUser {
    uhs: String,
}

#[derive(Deserialize)]
struct Entitlements {
    items: Vec<Entitlement>,
}

#[derive(Deserialize)]
struct Entitlement {
    name: String,
}

fn owns_minecraft(entitlements: &Entitlements) -> bool {
    entitlements
        .items
        .iter()
        .any(|item| matches!(item.name.as_str(), "game_minecraft" | "product_minecraft"))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MinecraftProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub skins: Vec<MinecraftSkin>,
    #[serde(default)]
    pub capes: Vec<MinecraftCape>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MinecraftSkin {
    pub id: String,
    pub state: String,
    pub url: String,
    #[serde(default)]
    pub variant: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MinecraftCape {
    pub id: String,
    pub state: String,
    pub url: String,
    #[serde(default)]
    pub alias: String,
}

/// Download only a Mojang-hosted skin, with no authorization header or redirects.
pub fn fetch_skin_png(url: &str, cancel: &AtomicBool) -> Result<Vec<u8>> {
    let data = fetch_texture_png(url, cancel)?;
    validate_skin_png(&data)?;
    Ok(data)
}

pub fn fetch_cape_png(url: &str, cancel: &AtomicBool) -> Result<Vec<u8>> {
    let data = fetch_texture_png(url, cancel)?;
    let (width, height) = png_dimensions(&data)?;
    if !(22..=1024).contains(&width) || !(17..=512).contains(&height) {
        bail!("披风图片尺寸超出范围");
    }
    Ok(data)
}

fn fetch_texture_png(url: &str, cancel: &AtomicBool) -> Result<Vec<u8>> {
    check_cancel(cancel)?;
    let url = texture_url(url)?;
    let mut response = http_client()?
        .get(url)
        .send()
        .map_err(|_| anyhow::anyhow!("下载 Minecraft 皮肤失败"))?;
    if !response.status().is_success() {
        bail!(
            "下载 Minecraft 皮肤失败（HTTP {}）",
            response.status().as_u16()
        );
    }
    const LIMIT: usize = 1024 * 1024;
    if response.content_length().is_some_and(|n| n > LIMIT as u64) {
        bail!("皮肤文件过大");
    }
    let mut data = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        check_cancel(cancel)?;
        let n = response
            .read(&mut buffer)
            .map_err(|_| anyhow::anyhow!("读取皮肤失败"))?;
        if n == 0 {
            break;
        }
        if data.len() + n > LIMIT {
            bail!("皮肤文件过大");
        }
        data.extend_from_slice(&buffer[..n]);
    }
    png_dimensions(&data)?;
    check_cancel(cancel)?;
    Ok(data)
}

fn texture_url(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).map_err(|_| anyhow::anyhow!("无效的皮肤地址"))?;
    let texture = url.path().strip_prefix("/texture/").unwrap_or_default();
    if url.scheme() != "https"
        || url.host_str() != Some("textures.minecraft.net")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(32..=64).contains(&texture.len())
        || !texture.bytes().all(|c| c.is_ascii_hexdigit())
    {
        bail!("只允许 Mojang 官方 HTTPS 皮肤地址");
    }
    Ok(url)
}

fn png_dimensions(data: &[u8]) -> Result<(u32, u32)> {
    if data.len() < 24 || &data[..8] != b"\x89PNG\r\n\x1a\n" || &data[12..16] != b"IHDR" {
        bail!("文件不是 PNG 图片");
    }
    Ok((
        u32::from_be_bytes(data[16..20].try_into().unwrap()),
        u32::from_be_bytes(data[20..24].try_into().unwrap()),
    ))
}

fn validate_skin_png(data: &[u8]) -> Result<()> {
    let (width, height) = png_dimensions(data)?;
    if width != 64 || ![32, 64].contains(&height) {
        bail!("皮肤尺寸须为 64×32 或 64×64");
    }
    Ok(())
}

/// The model selected by the user when uploading a skin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkinVariant {
    Classic,
    Slim,
}

impl SkinVariant {
    fn as_str(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Slim => "slim",
        }
    }
}

pub(crate) fn read_profile(session: &Session, cancel: &AtomicBool) -> Result<MinecraftProfile> {
    check_cancel(cancel)?;
    let profile: MinecraftProfile = authenticated_get(
        &http_client()?,
        PROFILE_URL,
        &session.access_token,
        "Minecraft 玩家资料查询",
    )?;
    validate_profile(&profile)?;
    if profile.id.to_ascii_lowercase() != session.uuid {
        bail!("返回的角色与当前账户不一致");
    }
    check_cancel(cancel)?;
    Ok(profile)
}

pub(crate) fn read_skin_file(path: &std::path::Path) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).context("无法读取所选皮肤文件")?;
    if !file.metadata().context("无法检查皮肤文件")?.is_file() {
        bail!("请选择普通 PNG 皮肤文件");
    }
    let mut data = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut data)
        .context("读取皮肤文件失败")?;
    if data.len() > 1024 * 1024 {
        bail!("皮肤文件不能超过 1 MiB");
    }
    validate_skin_png(&data)?;
    Ok(data)
}

// Endpoint and multipart fields match upstream PCL PageLoginMsSkin.EditSkin.
// These functions never retry a mutation: on an uncertain network outcome the
// user must refresh the profile to determine whether it was applied.
pub(crate) fn upload_skin(
    session: &Session,
    data: Vec<u8>,
    variant: SkinVariant,
    cancel: &AtomicBool,
) -> Result<()> {
    check_cancel(cancel)?;
    validate_skin_png(&data)?;
    let file = reqwest::blocking::multipart::Part::bytes(data)
        .file_name("skin.png")
        .mime_str("image/png")
        .context("无法编码皮肤上传")?;
    let form = reqwest::blocking::multipart::Form::new()
        .text("variant", variant.as_str())
        .part("file", file);
    let request = http_client()?
        .post(format!("{PROFILE_URL}/skins"))
        .bearer_auth(&session.access_token)
        .multipart(form);
    perform_profile_change(request)
}

pub(crate) fn reset_skin(session: &Session, cancel: &AtomicBool) -> Result<()> {
    check_cancel(cancel)?;
    perform_profile_change(
        http_client()?
            .delete(format!("{PROFILE_URL}/skins/active"))
            .bearer_auth(&session.access_token),
    )
}

// Upstream MySkin.BtnSkinCape_Click uses PUT capeId / DELETE on this endpoint.
pub(crate) fn select_cape(
    session: &Session,
    cape_id: Option<&str>,
    cancel: &AtomicBool,
) -> Result<()> {
    check_cancel(cancel)?;
    let client = http_client()?;
    let request = match cape_id {
        Some(id) => client
            .put(format!("{PROFILE_URL}/capes/active"))
            .json(&json!({"capeId":id})),
        None => client.delete(format!("{PROFILE_URL}/capes/active")),
    }
    .bearer_auth(&session.access_token);
    perform_profile_change(request)
}

fn perform_profile_change(request: reqwest::blocking::RequestBuilder) -> Result<()> {
    let response = request.send().map_err(|_| {
        anyhow::anyhow!("角色外观请求未得到确认；服务器可能已应用更改，请刷新资料确认")
    })?;
    match response.status().as_u16() {
        200..=299 => {
            let mut body = Vec::new();
            response
                .take(1024 * 1024 + 1)
                .read_to_end(&mut body)
                .map_err(|_| anyhow::anyhow!("外观请求响应读取失败；请刷新角色资料确认结果"))?;
            if body.len() > 1024 * 1024 {
                bail!("外观请求响应超过大小限制；请刷新资料确认结果");
            }
            if !body.is_empty() {
                let result: Value = serde_json::from_slice(&body)
                    .map_err(|_| anyhow::anyhow!("外观请求响应无效；请刷新角色资料确认结果"))?;
                if result.get("error").is_some() || result.get("errorMessage").is_some() {
                    bail!("Minecraft 返回外观更改错误，未确认更改成功");
                }
            }
            Ok(())
        }
        401 => Err(AuthenticationIssue::SessionUnauthorized.into()),
        403 => bail!("角色外观更改被服务拒绝（HTTP 403），请检查账户权限；未自动重新提交"),
        429 => Err(AuthenticationIssue::RateLimited.into()),
        503 => Err(AuthenticationIssue::ServiceUnavailable.into()),
        status => bail!("角色外观更改失败（HTTP {status}）"),
    }
}

pub(crate) fn validate_profile(profile: &MinecraftProfile) -> Result<()> {
    if profile.id.len() != 32
        || !profile.id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !valid_player_name(&profile.name)
    {
        bail!("Minecraft 返回了无效的玩家资料");
    }
    Ok(())
}

fn read_json<T: DeserializeOwned>(response: Response, stage: &str) -> Result<T> {
    // Do not include server response bodies or parser errors: either may echo credentials.
    let mut bytes = Vec::new();
    response
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("{stage} 响应读取失败"))?;
    if bytes.len() > 1024 * 1024 {
        bail!("{stage} 响应超过大小限制");
    }
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("{stage} 返回了无效的响应"))
}

#[cfg(test)]
fn checked_json<T: DeserializeOwned>(response: Response, stage: &str) -> Result<T> {
    if !response.status().is_success() {
        bail!("{stage} 失败（HTTP {}）", response.status().as_u16());
    }
    read_json(response, stage)
}

fn service_issue(url: &str, status: u16, body: &[u8]) -> Option<AuthenticationIssue> {
    let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    if url == XSTS_URL {
        if let Some(code) = value.get("XErr").and_then(Value::as_u64) {
            match code {
                2148916227 => return Some(AuthenticationIssue::Suspended),
                2148916233 => return Some(AuthenticationIssue::XboxProfileRequired),
                2148916235 => return Some(AuthenticationIssue::RegionUnavailable),
                2148916236..=2148916238 => {
                    return Some(AuthenticationIssue::FamilyPermissionRequired);
                }
                _ => {}
            }
        }
    }
    if value.get("error").and_then(Value::as_str) == Some("ACCOUNT_SUSPENDED")
        || value.get("errorMessage").and_then(Value::as_str) == Some("ACCOUNT_SUSPENDED")
    {
        return Some(AuthenticationIssue::Suspended);
    }
    match status {
        403 if url == MINECRAFT_LOGIN_URL => Some(AuthenticationIssue::MinecraftAccessDenied),
        401 if url == PROFILE_URL => Some(AuthenticationIssue::SessionUnauthorized),
        429 => Some(AuthenticationIssue::RateLimited),
        503 => Some(AuthenticationIssue::ServiceUnavailable),
        404 if url == PROFILE_URL => Some(AuthenticationIssue::MinecraftProfileRequired),
        _ => None,
    }
}
fn read_error_body(response: Response, stage: &str) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    response
        .take(64 * 1024 + 1)
        .read_to_end(&mut body)
        .map_err(|_| anyhow::anyhow!("{stage} 响应读取失败"))?;
    if body.len() > 64 * 1024 {
        bail!("{stage} 错误响应超过大小限制");
    }
    Ok(body)
}
fn checked_service_json<T: DeserializeOwned>(
    response: Response,
    url: &str,
    stage: &str,
) -> Result<T> {
    if response.status().is_success() {
        return read_json(response, stage);
    }
    let status = response.status().as_u16();
    let mut body = Vec::new();
    response
        .take(64 * 1024 + 1)
        .read_to_end(&mut body)
        .map_err(|_| anyhow::anyhow!("{stage} 响应读取失败"))?;
    if body.len() <= 64 * 1024 {
        if let Some(issue) = service_issue(url, status, &body) {
            return Err(issue.into());
        }
    }
    bail!("{stage} 失败（HTTP {status}）");
}

fn post_json<T: DeserializeOwned>(
    client: &Client,
    url: &str,
    body: &Value,
    stage: &str,
) -> Result<T> {
    let response = client
        .post(url)
        .header("x-xbl-contract-version", "1")
        .json(body)
        .send()
        .map_err(|_| AuthenticationIssue::NetworkUnavailable)
        .with_context(|| format!("{stage} 网络请求失败"))?;
    checked_service_json(response, url, stage)
}

fn authenticated_get<T: DeserializeOwned>(
    client: &Client,
    url: &str,
    token: &str,
    stage: &str,
) -> Result<T> {
    let response = client
        .get(url)
        .bearer_auth(token)
        .send()
        .map_err(|_| AuthenticationIssue::NetworkUnavailable)
        .with_context(|| format!("{stage} 网络请求失败"))?;
    checked_service_json(response, url, stage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_lifetime_and_skin_input_validation() {
        assert_eq!(token_expiry(3600, 1000).unwrap(), 4600);
        assert!(token_expiry(0, 1000).is_err());
        assert!(token_expiry(10, u64::MAX).is_err());
        let base = format!("https://textures.minecraft.net/texture/{}", "a".repeat(64));
        assert!(texture_url(&base).is_ok());
        for url in [
            base.replace("https:", "http:"),
            format!("{base}?token=secret"),
            base.replace("textures.minecraft.net", "evil.example"),
            base.replace("textures.minecraft.net", "user@textures.minecraft.net"),
        ] {
            assert!(texture_url(&url).is_err());
        }
        assert!(validate_skin_png(b"not a png").is_err());
        let mut header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        header.extend_from_slice(&64_u32.to_be_bytes());
        header.extend_from_slice(&64_u32.to_be_bytes());
        assert!(validate_skin_png(&header).is_ok());
        header[16..20].copy_from_slice(&4096_u32.to_be_bytes());
        assert!(validate_skin_png(&header).is_err());
    }

    #[test]
    fn refresh_response_rotation_and_revocation_are_sanitized() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        for (status, body, succeeds) in [
            (
                "200 OK",
                r#"{"access_token":"SECRET_ACCESS","refresh_token":"ROTATED","expires_in":3600}"#,
                true,
            ),
            (
                "400 Bad Request",
                r#"{"error":"invalid_grant","error_description":"SECRET_REFRESH"}"#,
                false,
            ),
            (
                "200 OK",
                r#"{"access_token":"SECRET_ACCESS","expires_in":0}"#,
                false,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = [0; 2048];
                assert!(socket.read(&mut request).unwrap() > 0);
                write!(socket,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            });
            let response = Client::builder()
                .no_proxy()
                .build()
                .unwrap()
                .get(format!("http://{address}"))
                .send()
                .unwrap();
            let result = parse_refresh_response(response);
            if succeeds {
                assert_eq!(
                    result.ok().unwrap().refresh_token.as_deref(),
                    Some("ROTATED")
                );
            } else {
                let error = result.err().unwrap();
                assert!(!format!("{error:#}").contains("SECRET"));
            }
            server.join().unwrap();
        }
        let error = check_cancel(&AtomicBool::new(true)).unwrap_err();
        assert!(error.is::<AuthenticationCancelled>());
    }

    #[test]
    fn successful_http_status_with_profile_error_is_not_success() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 2048];
            assert!(socket.read(&mut request).unwrap() > 0);
            let body = r#"{"errorMessage":"SECRET_TOKEN"}"#;
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let request = Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .delete(format!("http://{address}"));
        let error = perform_profile_change(request).unwrap_err();
        assert!(!format!("{error:#}").contains("SECRET_TOKEN"));
        server.join().unwrap();
    }

    #[test]
    fn profile_401_is_recoverable_but_403_and_uncertain_results_are_not() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        for (status, body, recoverable) in [
            ("401 Unauthorized", r#"{"error":"SECRET"}"#, true),
            ("403 Forbidden", r#"{"error":"SECRET"}"#, false),
            ("200 OK", "SECRET_INVALID_JSON", false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                assert!(socket.read(&mut [0; 2048]).unwrap() > 0);
                write!(
                    socket,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            });
            let error = perform_profile_change(
                Client::builder()
                    .no_proxy()
                    .build()
                    .unwrap()
                    .delete(format!("http://{address}")),
            )
            .unwrap_err();
            assert_eq!(
                error.downcast_ref::<AuthenticationIssue>()
                    == Some(&AuthenticationIssue::SessionUnauthorized),
                recoverable
            );
            assert!(!format!("{error:#}").contains("SECRET"));
            server.join().unwrap();
        }
        assert_eq!(
            service_issue(PROFILE_URL, 401, b"{}"),
            Some(AuthenticationIssue::SessionUnauthorized)
        );
        assert_eq!(service_issue(XBOX_URL, 401, b"{}"), None);
        let denied = service_issue(MINECRAFT_LOGIN_URL, 403, b"{}")
            .unwrap()
            .to_string();
        assert!(denied.contains("仅凭 403 无法确定"));
        assert!(!denied.contains("当前 IP"));
    }

    #[test]
    fn offline_uuid_matches_java_without_namespace() {
        let notch = offline_session("Notch").unwrap();
        assert_eq!(notch.uuid, "b50ad385829d3141a2167e7d7539ba7f");
        assert_eq!(notch.user_type, "legacy");
        assert_eq!(
            offline_session("Player").unwrap().uuid,
            "a01e3843e5213998958af459800e4d11"
        );
        assert_ne!(notch.uuid, offline_session("notch").unwrap().uuid);
    }

    #[test]
    fn invalid_offline_names_are_rejected() {
        for name in ["", "with spaces", "玩家", "../Player", "abcdefghijklmnopq"] {
            assert!(offline_session(name).is_err());
        }
        assert!(offline_session("_Player123").is_ok());
    }

    #[test]
    fn oauth_errors_are_classified_without_echoing_unknown_content() {
        assert_eq!(
            poll_result(br#"{"error":"authorization_pending"}"#),
            PollResult::Pending
        );
        assert_eq!(
            poll_result(br#"{"error":"slow_down"}"#),
            PollResult::SlowDown
        );
        assert_eq!(
            poll_result(br#"{"error":"access_denied"}"#),
            PollResult::Denied
        );
        assert_eq!(
            poll_result(br#"{"error":"expired_token"}"#),
            PollResult::Expired
        );
        assert_eq!(
            poll_result(br#"{"error":"SECRET_TOKEN","error_description":"secret"}"#),
            PollResult::Other
        );
        assert_eq!(poll_result(b"not json SECRET_TOKEN"), PollResult::Other);
    }

    #[test]
    fn http_and_json_errors_never_include_server_credentials() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        for status in ["401 Unauthorized", "200 OK"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 2048];
                let count = stream.read(&mut request).unwrap();
                assert!(count > 0, "test server must receive an HTTP request");
                let body = r#"{"access_token":123,"error_description":"SECRET_ACCESS_TOKEN"}"#;
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let response = Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap()
                .get(format!("http://{address}"))
                .send()
                .unwrap();
            let error = checked_json::<AccessToken>(response, "测试登录")
                .err()
                .expect("invalid response must fail");
            assert!(!format!("{error:#}").contains("SECRET_ACCESS_TOKEN"));
            server.join().unwrap();
        }
    }

    #[test]
    fn service_and_oauth_issues_keep_credentials_out_of_recovery_errors() {
        for (url, status, body, issue) in [
            (
                XSTS_URL,
                401,
                r#"{"XErr":2148916233,"Token":"SECRET"}"#,
                AuthenticationIssue::XboxProfileRequired,
            ),
            (
                XSTS_URL,
                401,
                r#"{"XErr":2148916238,"Message":"SECRET"}"#,
                AuthenticationIssue::FamilyPermissionRequired,
            ),
            (
                PROFILE_URL,
                404,
                r#"{"errorMessage":"SECRET"}"#,
                AuthenticationIssue::MinecraftProfileRequired,
            ),
            (
                MINECRAFT_LOGIN_URL,
                429,
                r#"{"errorMessage":"SECRET"}"#,
                AuthenticationIssue::RateLimited,
            ),
        ] {
            let result = service_issue(url, status, body.as_bytes()).unwrap();
            assert_eq!(result, issue);
            assert!(!result.to_string().contains("SECRET"));
        }
        assert_eq!(
            service_issue(MINECRAFT_LOGIN_URL, 403, br#"{"XErr":2148916233}"#),
            Some(AuthenticationIssue::MinecraftAccessDenied),
            "Xbox errors must not be inferred from a different service"
        );
        for (description, issue) in [
            (
                "Account security interrupt SECRET",
                AuthenticationIssue::SecurityInterrupt,
            ),
            ("service abuse SECRET", AuthenticationIssue::Suspended),
            (
                "AADSTS70000 SECRET",
                AuthenticationIssue::PasswordLoginRequired,
            ),
        ] {
            let body = serde_json::to_vec(
                &json!({"error":"invalid_grant", "error_description":description}),
            )
            .unwrap();
            let parsed = oauth_issue(&poll_result(&body)).unwrap();
            assert_eq!(parsed, issue);
            assert!(!format!("{parsed:?}: {parsed}").contains("SECRET"));
        }
    }
    #[test]
    fn expired_and_cancelled_codes_stop_before_network() {
        let mut code: DeviceCode = serde_json::from_value(json!({
            "user_code": "TEST", "device_code": "secret", "verification_uri": "https://microsoft.com/link", "expires_in": 900
        })).unwrap();
        assert_eq!(code.interval, 5);
        assert!(check_device_lifetime(&code, &AtomicBool::new(true)).is_err());
        code.issued_at = Instant::now() - Duration::from_secs(901);
        assert!(check_device_lifetime(&code, &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn unrelated_entitlements_and_invalid_profiles_are_rejected() {
        let unrelated: Entitlements =
            serde_json::from_value(json!({"items": [{"name": "unrelated_product"}]})).unwrap();
        assert!(!owns_minecraft(&unrelated));
        let owned: Entitlements =
            serde_json::from_value(json!({"items": [{"name": "game_minecraft"}]})).unwrap();
        assert!(owns_minecraft(&owned));
        assert!(validate_profile(&MinecraftProfile {
            id: "not-a-uuid".into(),
            name: "Player".into(),
            skins: vec![],
            capes: vec![]
        })
        .is_err());
        assert!(validate_profile(&MinecraftProfile {
            id: "b50ad385829d3141a2167e7d7539ba7f".into(),
            name: "Player".into(),
            skins: vec![],
            capes: vec![]
        })
        .is_ok());
    }
}
