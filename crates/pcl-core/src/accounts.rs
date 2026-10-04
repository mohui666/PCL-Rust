//! Persistent Microsoft accounts. Only non-secret metadata is written to disk.
//! Refresh tokens use the native OS vault; Minecraft access tokens stay in memory.
//! https://learn.microsoft.com/en-us/entra/identity-platform/refresh-tokens

use crate::auth::{self, AuthenticatedAccount, DeviceCode, MinecraftProfile};
use crate::model::Session;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, MutexGuard, OnceLock};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AccountCatalog {
    pub accounts: Vec<AccountSummary>,
    pub selected_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountSummary {
    pub id: String,
    pub username: String,
    pub uuid: String,
    pub client_id: String,
    pub profile: MinecraftProfile,
    pub last_used_at: u64,
}

/// Contains a bearer token: intentionally neither Debug nor Serialize.
#[derive(Clone)]
pub struct AccountSession {
    pub account: AccountSummary,
    pub session: Session,
    pub expires_at: u64,
}

trait Vault {
    fn read(&self, id: &str) -> Result<Option<String>>;
    fn write(&self, id: &str, secret: &str) -> Result<()>;
    fn delete(&self, id: &str) -> Result<()>;
}

struct NativeVault;

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn entry(id: &str) -> Result<keyring::Entry> {
    keyring::Entry::new("org.pcl-rust.microsoft", id).map_err(vault_error)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn vault_error(error: keyring::Error) -> anyhow::Error {
    // Some keyring errors contain raw secret bytes/ambiguous entries. Do not
    // attach their Display, Debug or source to the user-facing error chain.
    anyhow::anyhow!(match error {
        keyring::Error::NoStorageAccess(_) =>
            "系统安全存储无法访问，请解锁 Keychain / Windows 凭据管理器后重试",
        keyring::Error::NoEntry => "系统安全存储中没有此账户的凭据，请重新登录",
        keyring::Error::TooLong(_, _) => "登录凭据超过系统安全存储的长度限制，未保存账户",
        keyring::Error::BadEncoding(_) => "系统安全存储中的登录凭据格式无效，请重新登录",
        keyring::Error::Ambiguous(_) => "系统安全存储中存在多个同名凭据，无法安全选择",
        _ => "系统安全存储操作失败，未确认账户已保存",
    })
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl Vault for NativeVault {
    fn read(&self, id: &str) -> Result<Option<String>> {
        match entry(id)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(vault_error(error)),
        }
    }
    fn write(&self, id: &str, secret: &str) -> Result<()> {
        entry(id)?.set_password(secret).map_err(vault_error)
    }
    fn delete(&self, id: &str) -> Result<()> {
        match entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(vault_error(error)),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl Vault for NativeVault {
    fn read(&self, _: &str) -> Result<Option<String>> {
        bail!("此平台尚未接入系统安全存储")
    }
    fn write(&self, _: &str, _: &str) -> Result<()> {
        bail!("此平台尚未接入系统安全存储")
    }
    fn delete(&self, _: &str) -> Result<()> {
        bail!("此平台尚未接入系统安全存储")
    }
}

struct AccountStore<V: Vault> {
    path: PathBuf,
    vault: V,
    sessions: HashMap<String, AccountSession>,
}

fn account_id(client_id: &str, uuid: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{}:{uuid}", client_id.to_ascii_lowercase()))
    )
}

fn validate_catalog(catalog: &AccountCatalog) -> Result<()> {
    let mut ids = HashSet::new();
    if catalog.accounts.len() > 1000 {
        bail!("账户清单过大");
    }
    for account in &catalog.accounts {
        auth::validate_client_id(&account.client_id)?;
        auth::validate_profile(&account.profile)?;
        if account.id != account_id(&account.client_id, &account.uuid)
            || account.uuid != account.profile.id.to_ascii_lowercase()
            || account.username != account.profile.name
            || !ids.insert(&account.id)
        {
            bail!("账户清单元数据不一致，未修改原文件");
        }
    }
    if catalog
        .selected_id
        .as_ref()
        .is_some_and(|id| !ids.contains(id))
    {
        bail!("账户清单引用了不存在的选中账户");
    }
    Ok(())
}

fn require_regular_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => bail!("账户清单路径不是普通文件，未覆盖"),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("无法检查账户清单"),
    }
}

fn read_catalog(path: &Path) -> Result<AccountCatalog> {
    require_regular_file(path)?;
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AccountCatalog::default());
        }
        Err(error) => return Err(error).context("读取账户清单失败"),
    };
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .context("读取账户清单失败")?;
    if bytes.len() > 1024 * 1024 {
        bail!("账户清单超过大小限制");
    }
    // Avoid echoing arbitrary malformed content which might contain old secrets.
    let catalog = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("账户清单不是有效的 UTF-8 JSON，未修改原文件"))?;
    validate_catalog(&catalog)?;
    Ok(catalog)
}

fn write_catalog(path: &Path, catalog: &AccountCatalog) -> Result<()> {
    validate_catalog(catalog)?;
    require_regular_file(path)?;
    let parent = path.parent().context("账户清单缺少父目录")?;
    fs::create_dir_all(parent).context("创建账户配置目录失败")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).context("创建临时账户清单失败")?;
    let bytes = serde_json::to_vec_pretty(catalog).context("编码账户清单失败")?;
    temporary.write_all(&bytes).context("写入账户清单失败")?;
    temporary.as_file().sync_all().context("同步账户清单失败")?;
    require_regular_file(path)?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("提交账户清单失败")?;
    Ok(())
}

impl<V: Vault> AccountStore<V> {
    fn save_login(
        &mut self,
        client_id: &str,
        refresh: &str,
        auth: AuthenticatedAccount,
        cancel: &AtomicBool,
    ) -> Result<AccountSession> {
        auth::check_cancel(cancel)?;
        if refresh.is_empty() {
            bail!("Microsoft 未返回刷新令牌，无法保存账户；请重新授权 offline_access");
        }
        let id = account_id(client_id, &auth.session.uuid);
        let mut catalog = read_catalog(&self.path)?;
        let previous = self.vault.read(&id)?;
        let account = summary(client_id, auth.profile)?;
        let result = AccountSession {
            account: account.clone(),
            session: auth.session,
            expires_at: auth.expires_at,
        };
        require_valid_session(&result)?;
        catalog.accounts.retain(|item| item.id != id);
        catalog.accounts.push(account);
        catalog.selected_id = Some(id.clone());
        self.vault.write(&id, refresh)?;
        if let Err(error) =
            auth::check_cancel(cancel).and_then(|()| write_catalog(&self.path, &catalog))
        {
            self.sessions.remove(&id);
            let restored = match previous {
                Some(secret) => self.vault.write(&id, &secret),
                None => self.vault.delete(&id),
            };
            if restored.is_err() {
                bail!("账户清单保存失败，安全存储回滚也失败；请重新登录或移除此账户");
            }
            return Err(error).context("账户未保存");
        }
        self.sessions.insert(id, result.clone());
        Ok(result)
    }

    fn refresh_with(
        &mut self,
        id: &str,
        cancel: &AtomicBool,
        microsoft: impl FnOnce(&str, &str) -> Result<auth::MicrosoftToken>,
        exchange: impl FnOnce(&str) -> Result<AuthenticatedAccount>,
    ) -> Result<AccountSession> {
        auth::check_cancel(cancel)?;
        let mut catalog = read_catalog(&self.path)?;
        let account = catalog
            .accounts
            .iter()
            .find(|account| account.id == id)
            .cloned()
            .context("账户不存在，请重新选择或登录")?;
        let previous = self
            .vault
            .read(id)?
            .context("账户没有保存的登录凭据，请重新登录")?;
        // A failed refresh must not quietly fall back to the previous session.
        self.sessions.remove(id);
        let token = microsoft(&account.client_id, &previous)?;
        if let Some(refresh) = &token.refresh_token {
            if refresh.is_empty() {
                bail!("Microsoft 返回了空的刷新令牌，未建立新会话");
            }
            // Preserve rotation even if Xbox, ownership, profile or cancellation
            // subsequently fails. The metadata does not contain this credential.
            self.vault
                .write(id, refresh)
                .context("无法保存轮换后的登录凭据，未建立新会话")?;
        }
        auth::check_cancel(cancel)?;
        let authenticated = exchange(&token.access_token)?;
        if authenticated.session.uuid != account.uuid {
            bail!("刷新后角色与所选账户不一致，未建立会话；请重新登录");
        }
        auth::check_cancel(cancel)?;
        let result = AccountSession {
            account: summary(&account.client_id, authenticated.profile)?,
            session: authenticated.session,
            expires_at: authenticated.expires_at,
        };
        require_valid_session(&result)?;
        *catalog
            .accounts
            .iter_mut()
            .find(|account| account.id == id)
            .unwrap() = result.account.clone();
        // Refresh does not change selection: choosing offline remains persistent.
        write_catalog(&self.path, &catalog)?;
        self.sessions.insert(id.into(), result.clone());
        Ok(result)
    }

    fn remove(&mut self, id: &str) -> Result<()> {
        let mut catalog = read_catalog(&self.path)?;
        if !catalog.accounts.iter().any(|account| account.id == id) {
            bail!("账户不存在");
        }
        let old = self.vault.read(id)?;
        self.vault.delete(id)?;
        self.sessions.remove(id);
        catalog.accounts.retain(|account| account.id != id);
        if catalog.selected_id.as_deref() == Some(id) {
            catalog.selected_id = None;
        }
        if let Err(error) = write_catalog(&self.path, &catalog) {
            if let Some(secret) = old {
                if self.vault.write(id, &secret).is_err() {
                    bail!("账户清单更新失败且安全存储回滚失败，请重新登录");
                }
            }
            return Err(error).context("未完成账户移除");
        }
        Ok(())
    }
}

fn summary(client_id: &str, profile: MinecraftProfile) -> Result<AccountSummary> {
    auth::validate_client_id(client_id)?;
    auth::validate_profile(&profile)?;
    let uuid = profile.id.to_ascii_lowercase();
    Ok(AccountSummary {
        id: account_id(client_id, &uuid),
        username: profile.name.clone(),
        uuid,
        client_id: client_id.to_ascii_lowercase(),
        profile,
        last_used_at: auth::unix_time()?,
    })
}

fn require_valid_session(session: &AccountSession) -> Result<()> {
    if session.expires_at <= auth::unix_time()?
        || session.session.access_token.is_empty()
        || session.session.user_type != "msa"
        || session.session.uuid != session.account.uuid
        || session.session.username != session.account.username
    {
        bail!("Minecraft 会话无效或已过期，未建立登录状态");
    }
    Ok(())
}

fn store() -> Result<MutexGuard<'static, AccountStore<NativeVault>>> {
    static STORE: OnceLock<Mutex<AccountStore<NativeVault>>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            Mutex::new(AccountStore {
                path: crate::config::settings_path().with_file_name("accounts.json"),
                vault: NativeVault,
                sessions: HashMap::new(),
            })
        })
        .lock()
        .map_err(|_| anyhow::anyhow!("账户存储状态异常，请重启启动器"))
}

/// These APIs are blocking. Call from a worker thread, including native vault IO.
pub fn load_accounts() -> Result<AccountCatalog> {
    read_catalog(&store()?.path)
}

pub fn select_account(id: Option<&str>) -> Result<()> {
    let store = store()?;
    let mut catalog = read_catalog(&store.path)?;
    if id.is_some_and(|id| !catalog.accounts.iter().any(|account| account.id == id)) {
        bail!("所选账户不存在");
    }
    catalog.selected_id = id.map(str::to_owned);
    write_catalog(&store.path, &catalog)
}

pub fn complete_device_login_and_save(
    client_id: &str,
    code: &DeviceCode,
    cancel: &AtomicBool,
    progress: impl Fn(String),
) -> Result<AccountSession> {
    complete_device_login_and_save_with_stage(client_id, code, cancel, |stage| {
        progress(stage.message().into())
    })
}

/// Typed stages let the desktop close the code window immediately after OAuth,
/// while keeping the account unverified until Minecraft checks and vault save finish.
pub fn complete_device_login_and_save_with_stage(
    client_id: &str,
    code: &DeviceCode,
    cancel: &AtomicBool,
    progress: impl Fn(auth::LoginStage),
) -> Result<AccountSession> {
    progress(auth::LoginStage::Microsoft);
    let tokens = auth::device_tokens(client_id, code, cancel, &|_| {})?;
    let refresh = tokens
        .refresh_token
        .as_deref()
        .filter(|value| !value.is_empty())
        .context("Microsoft 未返回刷新令牌，请重新授权 offline_access")?;
    let authenticated =
        auth::exchange_minecraft_with_stage(&tokens.access_token, cancel, &progress)?;
    progress(auth::LoginStage::Saving);
    let result = store()?.save_login(client_id, refresh, authenticated, cancel)?;
    progress(auth::LoginStage::Complete);
    Ok(result)
}

pub fn restore_account(
    id: &str,
    cancel: &AtomicBool,
    progress: impl Fn(String),
) -> Result<AccountSession> {
    ensure_valid_session(id, cancel, progress)
}

pub fn refresh_account(
    id: &str,
    cancel: &AtomicBool,
    progress: impl Fn(String),
) -> Result<AccountSession> {
    refresh_account_with_stage(id, cancel, |stage| progress(stage.message().into()))
}

/// Force a fresh Minecraft session after an authoritative HTTP 401. This bypasses
/// only the in-memory access-token cache, never the native vault or identity check.
pub fn refresh_account_with_stage(
    id: &str,
    cancel: &AtomicBool,
    progress: impl Fn(auth::LoginStage),
) -> Result<AccountSession> {
    progress(auth::LoginStage::Microsoft);
    let result = store()?.refresh_with(
        id,
        cancel,
        |client_id, secret| auth::refresh_microsoft(client_id, secret, cancel),
        |access| {
            let authenticated = auth::exchange_minecraft_with_stage(access, cancel, &progress)?;
            progress(auth::LoginStage::Saving);
            Ok(authenticated)
        },
    )?;
    progress(auth::LoginStage::Complete);
    Ok(result)
}

pub fn ensure_valid_session(
    id: &str,
    cancel: &AtomicBool,
    progress: impl Fn(String),
) -> Result<AccountSession> {
    ensure_valid_session_with_stage(id, cancel, |stage| progress(stage.message().into()))
}

pub fn ensure_valid_session_with_stage(
    id: &str,
    cancel: &AtomicBool,
    progress: impl Fn(auth::LoginStage),
) -> Result<AccountSession> {
    auth::check_cancel(cancel)?;
    let mut store = store()?;
    let catalog = read_catalog(&store.path)?;
    if !catalog.accounts.iter().any(|account| account.id == id) {
        bail!("账户不存在，请重新选择或登录");
    }
    if let Some(cached) = store.sessions.get(id) {
        if cached.expires_at > auth::unix_time()?.saturating_add(60) {
            progress(auth::LoginStage::Complete);
            return Ok(cached.clone());
        }
    }
    progress(auth::LoginStage::Microsoft);
    let result = store.refresh_with(
        id,
        cancel,
        |client_id, secret| auth::refresh_microsoft(client_id, secret, cancel),
        |access| {
            let result = auth::exchange_minecraft_with_stage(access, cancel, &progress)?;
            progress(auth::LoginStage::Saving);
            Ok(result)
        },
    )?;
    progress(auth::LoginStage::Complete);
    Ok(result)
}

pub fn remove_account(id: &str) -> Result<()> {
    store()?.remove(id)
}

/// Refresh profile metadata without changing the selected account.
pub fn refresh_profile(
    id: &str,
    cancel: &AtomicBool,
    progress: impl Fn(String),
) -> Result<AccountSession> {
    let session = ensure_valid_session(id, cancel, &progress)?;
    let mut store = store()?;
    let profile =
        invalidate_unauthorized(&mut store, id, auth::read_profile(&session.session, cancel))?;
    persist_profile(&mut store, session, profile)
}

fn persist_profile<V: Vault>(
    store: &mut AccountStore<V>,
    mut session: AccountSession,
    profile: MinecraftProfile,
) -> Result<AccountSession> {
    let mut catalog = read_catalog(&store.path)?;
    let account = catalog
        .accounts
        .iter_mut()
        .find(|item| item.id == session.account.id)
        .context("账户已移除，未重新创建账户")?;
    if profile.id.to_ascii_lowercase() != account.uuid {
        bail!("角色资料与账户不匹配");
    }
    session.account = summary(&account.client_id, profile)?;
    session.session.username = session.account.username.clone();
    *account = session.account.clone();
    store.sessions.remove(&session.account.id);
    write_catalog(&store.path, &catalog)?;
    store
        .sessions
        .insert(session.account.id.clone(), session.clone());
    Ok(session)
}

/// A remote mutation succeeded. Even if its follow-up read returns 401, callers
/// must not automatically repeat the mutation after authenticating again.
#[derive(Debug)]
pub struct ProfileChangeCommitted;
impl std::fmt::Display for ProfileChangeCommitted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("服务器已接受外观更改，请刷新资料确认；不会自动重复提交")
    }
}
impl std::error::Error for ProfileChangeCommitted {}

fn invalidate_unauthorized<T, V: Vault>(
    store: &mut AccountStore<V>,
    id: &str,
    result: Result<T>,
) -> Result<T> {
    if result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<auth::AuthenticationIssue>())
        == Some(&auth::AuthenticationIssue::SessionUnauthorized)
    {
        store.sessions.remove(id);
    }
    result
}

fn change_profile(
    id: &str,
    cancel: &AtomicBool,
    progress: &dyn Fn(String),
    change: impl FnOnce(&Session, &MinecraftProfile) -> Result<()>,
) -> Result<AccountSession> {
    let session = ensure_valid_session(id, cancel, progress)?;
    let mut store = store()?;
    let catalog = read_catalog(&store.path)?;
    if !catalog.accounts.iter().any(|account| account.id == id) {
        bail!("账户已移除");
    }
    let profile =
        invalidate_unauthorized(&mut store, id, auth::read_profile(&session.session, cancel))?;
    auth::check_cancel(cancel)?;
    invalidate_unauthorized(&mut store, id, change(&session.session, &profile))?;
    // After a successful remote mutation, cancellation cannot undo it. Always
    // obtain and save authoritative metadata instead of pretending cancellation.
    let profile = invalidate_unauthorized(
        &mut store,
        id,
        auth::read_profile(&session.session, &AtomicBool::new(false)),
    )
    .context(ProfileChangeCommitted)?;
    persist_profile(&mut store, session, profile).context(ProfileChangeCommitted)
}

/// Call only after the user chooses a skin file and explicitly requests upload.
pub fn upload_skin(
    id: &str,
    path: &Path,
    variant: auth::SkinVariant,
    cancel: &AtomicBool,
    progress: impl Fn(String),
) -> Result<AccountSession> {
    auth::check_cancel(cancel)?;
    let data = auth::read_skin_file(path)?;
    change_profile(id, cancel, &progress, |session, _| {
        auth::upload_skin(session, data, variant, cancel)
    })
}

pub fn reset_skin(
    id: &str,
    cancel: &AtomicBool,
    progress: impl Fn(String),
) -> Result<AccountSession> {
    change_profile(id, cancel, &progress, |session, _| {
        auth::reset_skin(session, cancel)
    })
}

/// `None` unequips the cape. IDs must belong to this account's live profile.
pub fn select_cape(
    id: &str,
    cape_id: Option<&str>,
    cancel: &AtomicBool,
    progress: impl Fn(String),
) -> Result<AccountSession> {
    change_profile(id, cancel, &progress, |session, profile| {
        if cape_id.is_some_and(|id| !profile.capes.iter().any(|cape| cape.id == id)) {
            bail!("此账户不拥有所选披风");
        }
        auth::select_cape(session, cape_id, cancel)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::Ordering;

    const CLIENT: &str = "00000000-1111-2222-3333-444444444444";
    const UUID: &str = "a01e3843e5213998958af459800e4d11";
    #[derive(Default)]
    struct FakeVault {
        entries: RefCell<HashMap<String, String>>,
        fail_write: Cell<bool>,
        fail_delete: Cell<bool>,
        after_write: RefCell<Option<Box<dyn FnOnce()>>>,
    }
    impl Vault for FakeVault {
        fn read(&self, id: &str) -> Result<Option<String>> {
            Ok(self.entries.borrow().get(id).cloned())
        }
        fn write(&self, id: &str, secret: &str) -> Result<()> {
            if self.fail_write.get() {
                bail!("fixture vault locked");
            }
            self.entries.borrow_mut().insert(id.into(), secret.into());
            if let Some(action) = self.after_write.borrow_mut().take() {
                action();
            }
            Ok(())
        }
        fn delete(&self, id: &str) -> Result<()> {
            if self.fail_delete.get() {
                bail!("fixture delete denied");
            }
            self.entries.borrow_mut().remove(id);
            Ok(())
        }
    }
    fn store(path: &Path) -> AccountStore<FakeVault> {
        AccountStore {
            path: path.join("accounts.json"),
            vault: FakeVault::default(),
            sessions: HashMap::new(),
        }
    }
    fn authenticated() -> AuthenticatedAccount {
        AuthenticatedAccount {
            profile: MinecraftProfile {
                id: UUID.into(),
                name: "Player".into(),
                skins: vec![],
                capes: vec![],
            },
            session: Session {
                username: "Player".into(),
                uuid: UUID.into(),
                access_token: "SECRET_ACCESS".into(),
                user_type: "msa".into(),
            },
            expires_at: auth::unix_time().unwrap() + 3600,
        }
    }
    fn token(refresh: Option<&str>) -> auth::MicrosoftToken {
        serde_json::from_value(serde_json::json!({"access_token":"SECRET_MS", "expires_in":3600, "refresh_token":refresh})).unwrap()
    }
    fn login(store: &mut AccountStore<FakeVault>) -> AccountSession {
        store
            .save_login(
                CLIENT,
                "OLD_REFRESH",
                authenticated(),
                &AtomicBool::new(false),
            )
            .unwrap()
    }

    #[test]
    fn authoritative_profile_401_invalidates_cache_but_preserves_refresh_and_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path());
        let account = login(&mut store);
        let before = fs::read(&store.path).unwrap();
        let result: Result<()> = invalidate_unauthorized(
            &mut store,
            &account.account.id,
            Err(auth::AuthenticationIssue::NetworkUnavailable.into()),
        );
        assert!(result.is_err());
        assert!(store.sessions.contains_key(&account.account.id));
        let result: Result<()> = invalidate_unauthorized(
            &mut store,
            &account.account.id,
            Err(auth::AuthenticationIssue::SessionUnauthorized.into()),
        );
        assert!(result.unwrap_err().is::<auth::AuthenticationIssue>());
        assert!(!store.sessions.contains_key(&account.account.id));
        assert_eq!(fs::read(&store.path).unwrap(), before);
        assert_eq!(
            store.vault.read(&account.account.id).unwrap().as_deref(),
            Some("OLD_REFRESH")
        );
    }

    #[test]
    fn saved_catalog_contains_no_tokens_and_selection_can_stay_offline() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path());
        let session = login(&mut store);
        let text = fs::read_to_string(&store.path).unwrap();
        for secret in [
            "SECRET_ACCESS",
            "OLD_REFRESH",
            "access_token",
            "refresh_token",
        ] {
            assert!(!text.contains(secret));
        }
        assert_eq!(
            store.vault.read(&session.account.id).unwrap().as_deref(),
            Some("OLD_REFRESH")
        );
        let mut catalog = read_catalog(&store.path).unwrap();
        catalog.selected_id = None;
        write_catalog(&store.path, &catalog).unwrap();
        store
            .refresh_with(
                &session.account.id,
                &AtomicBool::new(false),
                |_, secret| {
                    assert_eq!(secret, "OLD_REFRESH");
                    Ok(token(Some("NEW_REFRESH")))
                },
                |_| Ok(authenticated()),
            )
            .unwrap();
        assert_eq!(read_catalog(&store.path).unwrap().selected_id, None);
        assert_eq!(
            store.vault.read(&session.account.id).unwrap().as_deref(),
            Some("NEW_REFRESH")
        );
    }

    #[test]
    fn vault_save_failure_never_registers_or_caches_account() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path());
        store.vault.fail_write.set(true);
        assert!(store
            .save_login(CLIENT, "SECRET", authenticated(), &AtomicBool::new(false))
            .is_err());
        assert!(!store.path.exists());
        assert!(store.sessions.is_empty());
    }

    #[test]
    fn metadata_save_failure_rolls_back_new_vault_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path());
        let path = store.path.clone();
        *store.vault.after_write.borrow_mut() =
            Some(Box::new(move || fs::create_dir(path).unwrap()));
        assert!(store
            .save_login(CLIENT, "SECRET", authenticated(), &AtomicBool::new(false))
            .is_err());
        assert!(store.vault.entries.borrow().is_empty());
        assert!(store.sessions.is_empty());
    }

    #[test]
    fn rotation_is_retained_if_later_cancelled_or_xbox_fails() {
        for cancel_after_token in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut store = store(dir.path());
            let account = login(&mut store).account;
            let cancel = AtomicBool::new(false);
            let error = store
                .refresh_with(
                    &account.id,
                    &cancel,
                    |_, _| {
                        cancel.store(cancel_after_token, Ordering::Relaxed);
                        Ok(token(Some("ROTATED")))
                    },
                    |_| bail!("Xbox fixture failed"),
                )
                .err()
                .unwrap();
            assert_eq!(
                error.is::<auth::AuthenticationCancelled>(),
                cancel_after_token
            );
            assert_eq!(
                store.vault.read(&account.id).unwrap().as_deref(),
                Some("ROTATED")
            );
            assert!(store.sessions.is_empty());
        }
    }

    #[test]
    fn refresh_rejection_or_expired_session_never_returns_cached_success() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path());
        let account = login(&mut store).account;
        assert!(store
            .refresh_with(
                &account.id,
                &AtomicBool::new(false),
                |_, _| bail!("invalid_grant"),
                |_| unreachable!()
            )
            .is_err());
        assert!(store.sessions.is_empty());
        assert!(store
            .refresh_with(
                &account.id,
                &AtomicBool::new(false),
                |_, _| Ok(token(None)),
                |_| {
                    let mut result = authenticated();
                    result.expires_at = auth::unix_time().unwrap() - 1;
                    Ok(result)
                }
            )
            .is_err());
        assert!(store.sessions.is_empty());
    }

    #[test]
    fn failed_rotation_write_stops_before_xbox_and_drops_cached_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path());
        let account = login(&mut store).account;
        store.vault.fail_write.set(true);
        assert!(store
            .refresh_with(
                &account.id,
                &AtomicBool::new(false),
                |_, _| Ok(token(Some("NEW"))),
                |_| unreachable!()
            )
            .is_err());
        assert_eq!(
            store.vault.read(&account.id).unwrap().as_deref(),
            Some("OLD_REFRESH")
        );
        assert!(store.sessions.is_empty());
    }

    #[test]
    fn removal_failure_preserves_metadata_then_success_removes_vault_and_selection() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path());
        let id = login(&mut store).account.id;
        store.vault.fail_delete.set(true);
        assert!(store.remove(&id).is_err());
        assert_eq!(read_catalog(&store.path).unwrap().accounts.len(), 1);
        store.vault.fail_delete.set(false);
        store.remove(&id).unwrap();
        let catalog = read_catalog(&store.path).unwrap();
        assert!(catalog.accounts.is_empty());
        assert!(catalog.selected_id.is_none());
        assert!(store.sessions.is_empty());
        assert!(store.vault.entries.borrow().is_empty());
    }

    #[test]
    fn cancelled_login_and_malformed_catalog_are_preserved_without_vault_write() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path());
        assert!(store
            .save_login(CLIENT, "secret", authenticated(), &AtomicBool::new(true))
            .err()
            .unwrap()
            .is::<auth::AuthenticationCancelled>());
        fs::write(&store.path, b"malformed SECRET").unwrap();
        let error = store
            .save_login(CLIENT, "secret", authenticated(), &AtomicBool::new(false))
            .err()
            .unwrap();
        assert!(!format!("{error:#}").contains("SECRET"));
        assert_eq!(fs::read(&store.path).unwrap(), b"malformed SECRET");
        assert!(store.vault.entries.borrow().is_empty());
    }
}
