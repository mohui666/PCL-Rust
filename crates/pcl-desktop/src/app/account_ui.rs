pub(super) use super::modal_ui::{account_input_modal, account_modal, modal_frame};
use super::modal_ui::{account_modal_with_options, modal_frame_with_options, ModalOptions};
use super::{hint_ui::HintKind, Event, Launcher, MUTED};
use crate::theme;
use crate::ui_style;
use eframe::egui::{self, Color32, Rect, RichText, Vec2};
use pcl_core::{
    accounts::{self, AccountCatalog, AccountSession},
    auth,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) enum AccountEvent {
    Loaded(u64, Result<AccountCatalog, String>),
    Device {
        request: u64,
        code: String,
        url: String,
        expires: u64,
    },
    Stage(u64, auth::LoginStage),
    Ready(u64, Box<Result<AccountSession, AccountFailure>>),
    Profile(u64, Box<Result<AccountSession, AccountFailure>>),
    Selection(u64, bool, Result<AccountCatalog, String>),
    Removed(u64, Result<AccountCatalog, String>),
    Skin(u64, Result<Vec<u8>, String>),
}
pub(crate) struct AccountFailure {
    message: String,
    cancelled: bool,
    issue: Option<auth::AuthenticationIssue>,
    profile_committed: bool,
}
impl AccountFailure {
    fn from_error(error: anyhow::Error, prefix: &str) -> Self {
        let cancelled = error.is::<auth::AuthenticationCancelled>()
            || error
                .chain()
                .any(|cause| cause.is::<auth::AuthenticationCancelled>());
        Self {
            message: format!("{prefix}：{error:#}"),
            cancelled,
            profile_committed: error.is::<accounts::ProfileChangeCommitted>()
                || error
                    .chain()
                    .any(|cause| cause.is::<accounts::ProfileChangeCommitted>()),
            issue: error
                .chain()
                .find_map(|cause| cause.downcast_ref::<auth::AuthenticationIssue>())
                .copied(),
        }
    }
}
struct DevicePrompt {
    code: String,
    url: String,
    expires: Instant,
    opened: bool,
}
#[derive(Default)]
pub(super) struct AccountUiState {
    catalog: Option<AccountCatalog>,
    loading: bool,
    selected: Option<String>,
    request: u64,
    auto_restore: bool,
    auth_pending: bool,
    stage: Option<auth::LoginStage>,
    refresh_at: Option<Instant>,
    pending_launch: Option<PendingLaunch>,
    device: Option<DevicePrompt>,
    error: Option<String>,
    remove: Option<String>,
    upload: Option<std::path::PathBuf>,
    reset_skin: bool,
    cape_picker: bool,
    cape_draft: Option<CapeDraft>,
    purpose: LoginPurpose,
    automatic_reauth_used: bool,
    profile_retry_used: bool,
    device_auth: bool,
    web_success_shown: bool,
    pending_profile: Option<PendingProfile>,
    error_title: Option<&'static str>,
    error_warning: bool,
    error_relogin: bool,
    error_issue: Option<auth::AuthenticationIssue>,
    skin: Option<egui::TextureHandle>,
    skin_bytes: Option<Vec<u8>>,
    incoming_skin: Option<Vec<u8>>,
    icons: Vec<egui::TextureHandle>,
}
impl AccountUiState {
    fn accept_stage(&mut self, request: u64, stage: auth::LoginStage) -> bool {
        if request != self.request || !self.auth_pending {
            return false;
        }
        self.stage = Some(stage);
        if stage != auth::LoginStage::Microsoft {
            self.device = None;
        }
        true
    }

    fn can_reauthenticate(&self, failure: &AccountFailure) -> bool {
        !self.automatic_reauth_used
            && !self.device_auth
            && !failure.cancelled
            && !failure.profile_committed
            && matches!(
                failure.issue,
                Some(
                    auth::AuthenticationIssue::ReauthenticationRequired
                        | auth::AuthenticationIssue::SessionUnauthorized
                )
            )
    }

    fn web_success(&mut self, request: u64, stage: auth::LoginStage) -> bool {
        if request == self.request
            && self.auth_pending
            && self.device_auth
            && stage == auth::LoginStage::Xbox
            && !self.web_success_shown
        {
            self.web_success_shown = true;
            true
        } else {
            false
        }
    }
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum LoginPurpose {
    #[default]
    Login,
    Restore,
    Launch,
    Profile,
}
#[derive(Clone)]
enum ProfileChange {
    RefreshSkin,
    RefreshCapes,
    Upload(std::path::PathBuf, auth::SkinVariant),
    Reset,
    Cape(Option<String>),
}
impl ProfileChange {
    fn label(&self) -> &'static str {
        match self {
            Self::RefreshSkin => "正在刷新皮肤……",
            Self::RefreshCapes => "正在刷新披风列表……",
            Self::Upload(..) => "正在更改皮肤……",
            Self::Reset => "正在重置皮肤……",
            Self::Cape(_) => "正在更改披风……",
        }
    }
    fn success(&self) -> &'static str {
        match self {
            Self::RefreshSkin => "已刷新皮肤！",
            Self::RefreshCapes => "已刷新披风列表！",
            Self::Upload(..) => "更改皮肤成功！",
            Self::Reset => "重置皮肤成功！",
            Self::Cape(_) => "更改披风成功！",
        }
    }
    fn failure(&self) -> &'static str {
        match self {
            Self::RefreshSkin => "刷新皮肤失败",
            Self::RefreshCapes => "刷新披风列表失败",
            Self::Upload(..) => "更改皮肤失败",
            Self::Reset => "重置皮肤失败",
            Self::Cape(_) => "更改披风失败",
        }
    }
    fn resume_hint(&self) -> &'static str {
        match self {
            Self::RefreshSkin | Self::RefreshCapes => "正在重新登录，将在登录后自动刷新资料……",
            Self::Upload(..) => "正在重新登录，将在登录后自动更改皮肤……",
            Self::Reset => "正在重新登录，将在登录后自动重置皮肤……",
            Self::Cape(_) => "正在重新登录，将在登录后自动更改披风……",
        }
    }
}
#[derive(Clone)]
struct PendingProfile {
    account_id: String,
    change: ProfileChange,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingLaunch {
    account_id: String,
    preview: bool,
}
#[derive(Clone, Default)]
struct CapeDraft {
    selected: Option<String>,
    #[cfg(test)]
    rows: Vec<Rect>,
}
impl CapeDraft {
    fn from_capes(capes: &[auth::MinecraftCape]) -> Self {
        Self {
            selected: capes
                .iter()
                .find(|cape| cape.state == "ACTIVE")
                .map(|cape| cape.id.clone()),
            #[cfg(test)]
            rows: Vec::new(),
        }
    }
}
impl Launcher {
    fn present_account_failure(&mut self, failure: AccountFailure, purpose: LoginPurpose) {
        self.accounts.error = None;
        self.accounts.error_issue = None;
        self.accounts.error_relogin = false;
        if failure.issue == Some(auth::AuthenticationIssue::SessionUnauthorized) {
            self.session = None;
            self.accounts.refresh_at = None;
        }
        if failure.cancelled {
            self.status = if purpose == LoginPurpose::Profile {
                "外观操作已取消"
            } else {
                "登录已取消"
            }
            .into();
            self.push_hint(HintKind::Info, self.status.clone());
            return;
        }
        self.status = if purpose == LoginPurpose::Profile {
            "账号外观操作未完成"
        } else {
            "正版登录未完成"
        }
        .into();
        let presentation = if failure.profile_committed {
            FailurePresentation::Modal("外观资料刷新失败", true)
        } else {
            failure_presentation(failure.issue, purpose)
        };
        match presentation {
            FailurePresentation::Hint => self.push_hint(HintKind::Error, failure.message),
            FailurePresentation::Modal(title, warning) => {
                self.noticed_status = self.status.clone();
                self.accounts.error_title = Some(title);
                self.accounts.error_warning = warning;
                self.accounts.error_relogin = purpose != LoginPurpose::Profile;
                self.accounts.error_issue = if failure.profile_committed {
                    None
                } else {
                    failure.issue
                };
                self.accounts.error = Some(failure.message);
            }
        }
    }
    pub(super) fn init_accounts(&mut self) {
        self.accounts.loading = true;
        self.accounts.request = self.accounts.request.wrapping_add(1);
        let request = self.accounts.request;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result =
                accounts::load_accounts().map_err(|error| format!("无法读取保存的账号：{error:#}"));
            let _ = tx.send(Event::Account(AccountEvent::Loaded(request, result)));
        });
    }
    pub(super) fn select_account_mode(&mut self, microsoft: bool) {
        if self.busy.is_some() {
            return;
        }
        if microsoft {
            self.microsoft = true;
            if self.accounts.catalog.is_none() {
                self.init_accounts();
            }
        } else {
            self.error = Some("离线登录已禁用，请使用正版账号。".into());
        }
    }
    fn clear_account_selection(&mut self, microsoft: bool) {
        let Some((tx, _)) = self.start_job("正在切换登录方式") else {
            return;
        };
        self.accounts.request = self.accounts.request.wrapping_add(1);
        let request = self.accounts.request;
        std::thread::spawn(move || {
            let result = accounts::select_account(None)
                .and_then(|()| accounts::load_accounts())
                .map_err(|error| format!("无法保存账号选择：{error:#}"));
            let _ = tx.send(Event::Account(AccountEvent::Selection(
                request, microsoft, result,
            )));
        });
    }
    pub(super) fn account_tick(&mut self) {
        let refresh_due = self.microsoft
            && self.session.is_some()
            && self
                .accounts
                .refresh_at
                .is_some_and(|at| Instant::now() >= at);
        let dialog_open = self.java_download.has_dialog()
            || self.accounts.upload.is_some()
            || self.accounts.reset_skin
            || self.accounts.cape_picker
            || self.accounts.device.is_some()
            || self.accounts.error.is_some()
            || self.accounts.remove.is_some();
        if (self.accounts.auto_restore || refresh_due) && self.busy.is_none() && !dialog_open {
            self.accounts.auto_restore = false;
            self.accounts.refresh_at = None;
            self.restore_selected_account(None, LoginPurpose::Restore);
        }
    }
    pub(super) fn login(&mut self) {
        if self.accounts.selected.is_some() {
            self.restore_selected_account(None, LoginPurpose::Login);
            return;
        }
        self.begin_account_login(self.settings.microsoft_client_id.clone());
    }
    fn reauthenticate_account(&mut self) {
        let client = self.account_client_id();
        self.begin_account_login(client);
    }
    fn account_client_id(&self) -> String {
        self.accounts
            .selected
            .as_ref()
            .and_then(|id| {
                self.accounts
                    .catalog
                    .as_ref()?
                    .accounts
                    .iter()
                    .find(|account| &account.id == id)
                    .map(|account| account.client_id.clone())
            })
            .unwrap_or_else(|| self.settings.microsoft_client_id.clone())
    }
    fn begin_account_login(&mut self, client: String) {
        self.accounts.pending_launch = None;
        self.accounts.pending_profile = None;
        self.accounts.purpose = LoginPurpose::Login;
        self.accounts.automatic_reauth_used = false;
        self.accounts.profile_retry_used = false;
        self.begin_device_auth(client);
    }
    fn continue_reauthentication(&mut self) {
        self.accounts.automatic_reauth_used = true;
        self.begin_device_auth(self.account_client_id());
    }
    fn begin_device_auth(&mut self, client: String) {
        if client.trim().is_empty() {
            self.accounts.pending_launch = None;
            self.accounts.pending_profile = None;
            self.accounts.error_title = Some("登录设置提示");
            self.accounts.error_warning = false;
            self.accounts.error_relogin = false;
            self.accounts.error_issue = Some(auth::AuthenticationIssue::ClientConfiguration);
            self.accounts.error = Some(
                "请先在设置中填写你注册的 Microsoft 应用客户端 ID，并启用公共客户端设备代码流。"
                    .into(),
            );
            self.open_client_id_settings();
            return;
        }
        let Some((tx, cancel)) = self.start_job("正在请求微软登录") else {
            return;
        };
        self.accounts.request = self.accounts.request.wrapping_add(1);
        let request = self.accounts.request;
        self.accounts.auth_pending = true;
        self.accounts.device_auth = true;
        self.accounts.web_success_shown = false;
        self.accounts.stage = Some(auth::LoginStage::Microsoft);
        self.accounts.error_issue = None;
        self.accounts.error = None;
        self.accounts.error_relogin = false;
        self.microsoft = true;
        self.session = None;
        self.accounts.refresh_at = None;
        std::thread::spawn(move || {
            let result = (|| {
                let code = auth::begin_device_login(&client)?;
                let _ = tx.send(Event::Account(AccountEvent::Device {
                    request,
                    code: code.user_code.clone(),
                    url: code.verification_uri.clone(),
                    expires: code.expires_in,
                }));
                accounts::complete_device_login_and_save_with_stage(
                    &client,
                    &code,
                    &cancel,
                    |stage| {
                        let _ = tx.send(Event::Account(AccountEvent::Stage(request, stage)));
                    },
                )
            })()
            .map_err(|error: anyhow::Error| AccountFailure::from_error(error, "登录未完成"));
            let _ = tx.send(Event::Account(AccountEvent::Ready(
                request,
                Box::new(result),
            )));
        });
    }
    fn restore_selected_account(&mut self, launch: Option<bool>, purpose: LoginPurpose) {
        let Some(id) = self.accounts.selected.clone() else {
            self.accounts.error = Some("请先选择或添加一个正版账号。".into());
            return;
        };
        let Some((tx, cancel)) = self.start_job(if launch.is_some() {
            "正在验证正版会话"
        } else {
            "正在恢复正版账号"
        }) else {
            return;
        };
        self.accounts.request = self.accounts.request.wrapping_add(1);
        let request = self.accounts.request;
        self.accounts.pending_launch = launch.map(|preview| PendingLaunch {
            account_id: id.clone(),
            preview,
        });
        self.accounts.pending_profile = None;
        self.accounts.purpose = purpose;
        self.accounts.automatic_reauth_used = false;
        self.accounts.device_auth = false;
        self.accounts.web_success_shown = false;
        self.accounts.auth_pending = true;
        self.accounts.stage = Some(auth::LoginStage::Microsoft);
        self.accounts.error_issue = None;
        self.accounts.error = None;
        std::thread::spawn(move || {
            let result = accounts::ensure_valid_session_with_stage(&id, &cancel, |stage| {
                let _ = tx.send(Event::Account(AccountEvent::Stage(request, stage)));
            })
            .and_then(|session| {
                accounts::select_account(Some(&id))?;
                Ok(session)
            })
            .map_err(|error| {
                AccountFailure::from_error(error, "正版会话验证未完成，请重试或重新登录")
            });
            let _ = tx.send(Event::Account(AccountEvent::Ready(
                request,
                Box::new(result),
            )));
        });
    }
    pub(super) fn account_store_ready(&mut self) -> bool {
        if self.accounts.loading {
            self.status = "正在读取保存的账号，请稍候…".into();
            false
        } else {
            true
        }
    }
    pub(super) fn account_launch(&mut self, preview: bool) {
        if self.game_pid.is_some() && !preview {
            self.error = Some("当前游戏仍在运行。".into());
            return;
        }
        if self.settings.selected_version.is_none() {
            self.version_view = true;
            return;
        }
        self.restore_selected_account(Some(preview), LoginPurpose::Launch);
    }
    fn switch_account(&mut self) {
        if self.busy.is_none() {
            self.clear_account_selection(true);
        }
    }
    fn remove_selected_account(&mut self, id: String) {
        let Some((tx, _)) = self.start_job("正在移除保存的账号") else {
            return;
        };
        self.accounts.request = self.accounts.request.wrapping_add(1);
        let request = self.accounts.request;
        std::thread::spawn(move || {
            let result = accounts::remove_account(&id)
                .and_then(|()| accounts::load_accounts())
                .map_err(|error| format!("无法移除账号：{error:#}"));
            let _ = tx.send(Event::Account(AccountEvent::Removed(request, result)));
        });
    }
    fn change_account_profile(&mut self, change: ProfileChange) {
        let Some(id) = self.accounts.selected.clone() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        self.accounts.automatic_reauth_used = false;
        self.accounts.profile_retry_used = false;
        self.accounts.device_auth = false;
        self.start_profile_operation(PendingProfile {
            account_id: id,
            change,
        });
    }
    fn start_profile_operation(&mut self, pending: PendingProfile) {
        let Some((tx, cancel)) = self.start_job(pending.change.label()) else {
            return;
        };
        self.push_hint(HintKind::Info, pending.change.label());
        self.accounts.request = self.accounts.request.wrapping_add(1);
        let request = self.accounts.request;
        self.accounts.error = None;
        self.accounts.error_issue = None;
        self.accounts.error_relogin = false;
        self.accounts.pending_launch = None;
        self.accounts.purpose = LoginPurpose::Profile;
        self.accounts.pending_profile = Some(pending.clone());
        std::thread::spawn(move || {
            let PendingProfile {
                account_id: id,
                change,
            } = pending;
            let failure = change.failure();
            let progress = |message| {
                let _ = tx.send(Event::Log(message));
            };
            let result = match change {
                ProfileChange::RefreshSkin | ProfileChange::RefreshCapes => {
                    accounts::refresh_profile(&id, &cancel, progress)
                }
                ProfileChange::Upload(path, variant) => {
                    accounts::upload_skin(&id, &path, variant, &cancel, progress)
                }
                ProfileChange::Reset => accounts::reset_skin(&id, &cancel, progress),
                ProfileChange::Cape(cape) => {
                    accounts::select_cape(&id, cape.as_deref(), &cancel, progress)
                }
            }
            .map_err(|error| AccountFailure::from_error(error, failure));
            let _ = tx.send(Event::Account(AccountEvent::Profile(
                request,
                Box::new(result),
            )));
        });
    }
    fn refresh_for_profile(&mut self) {
        let Some(id) = self
            .accounts
            .pending_profile
            .as_ref()
            .map(|pending| pending.account_id.clone())
        else {
            return;
        };
        let Some((tx, cancel)) = self.start_job("正在重新登录") else {
            return;
        };
        self.accounts.request = self.accounts.request.wrapping_add(1);
        let request = self.accounts.request;
        self.accounts.auth_pending = true;
        self.accounts.device_auth = false;
        self.accounts.stage = Some(auth::LoginStage::Microsoft);
        std::thread::spawn(move || {
            let result = accounts::refresh_account_with_stage(&id, &cancel, |stage| {
                let _ = tx.send(Event::Account(AccountEvent::Stage(request, stage)));
            })
            .map_err(|error| AccountFailure::from_error(error, "重新登录失败"));
            let _ = tx.send(Event::Account(AccountEvent::Ready(
                request,
                Box::new(result),
            )));
        });
    }
    fn refresh_account_skin(&mut self) {
        let Some(id) = self.accounts.selected.as_ref() else {
            return;
        };
        let Some(account) = self
            .accounts
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.accounts.iter().find(|account| &account.id == id))
        else {
            return;
        };
        let Some(url) = account
            .profile
            .skins
            .iter()
            .find(|skin| skin.state == "ACTIVE")
            .map(|skin| skin.url.clone())
        else {
            self.accounts.skin = None;
            self.accounts.skin_bytes = None;
            self.accounts.incoming_skin = None;
            return;
        };
        let request = self.accounts.request;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = auth::fetch_skin_png(&url, &AtomicBool::new(false))
                .map_err(|error| format!("皮肤读取失败：{error:#}"));
            let _ = tx.send(Event::Account(AccountEvent::Skin(request, result)));
        });
    }
    pub(super) fn handle_account_event(&mut self, event: AccountEvent) {
        match event {
            AccountEvent::Selection(request, microsoft, result) => {
                if request != self.accounts.request {
                    return;
                }
                self.busy = None;
                self.accounts.loading = false;
                match result {
                    Ok(catalog) => {
                        self.accounts.catalog = Some(catalog);
                        self.accounts.selected = None;
                        self.accounts.auto_restore = false;
                        self.microsoft = microsoft;
                        self.session = None;
                        self.accounts.refresh_at = None;
                        self.accounts.skin = None;
                        self.accounts.skin_bytes = None;
                        self.accounts.incoming_skin = None;
                    }
                    Err(error) => self.accounts.error = Some(error),
                }
            }
            AccountEvent::Loaded(request, result) => {
                if request != self.accounts.request {
                    return;
                }
                self.accounts.loading = false;
                match result {
                    Ok(catalog) => {
                        self.accounts.selected = catalog.selected_id.clone();
                        self.accounts.auto_restore = self.accounts.selected.is_some();
                        if self.accounts.auto_restore {
                            self.microsoft = true;
                        }
                        self.accounts.catalog = Some(catalog);
                    }
                    Err(error) => {
                        self.microsoft = true;
                        self.accounts.error = Some(error);
                    }
                }
            }
            AccountEvent::Device {
                request,
                code,
                url,
                expires,
            } => {
                if request != self.accounts.request
                    || !self.accounts.auth_pending
                    || !self.accounts.device_auth
                {
                    return;
                }
                if self.cancel.load(Ordering::Relaxed) {
                    return;
                }
                self.accounts.device = Some(DevicePrompt {
                    code,
                    url,
                    expires: Instant::now() + Duration::from_secs(expires.min(3600)),
                    opened: false,
                });
                self.status = "请在浏览器中完成微软登录".into();
            }
            AccountEvent::Stage(request, stage) => {
                if !self.accounts.accept_stage(request, stage) {
                    return;
                }
                // 100% is displayed only when Ready also confirms selection persistence.
                if stage != auth::LoginStage::Complete {
                    self.status = stage.message().into();
                    self.record(stage.message().into());
                }
                if self.accounts.web_success(request, stage) {
                    self.push_hint(HintKind::Success, "网页登录成功！");
                }
            }
            AccountEvent::Ready(request, result) => {
                if request != self.accounts.request || !self.accounts.auth_pending {
                    return;
                }
                self.accounts.device = None;
                self.accounts.auth_pending = false;
                self.accounts.stage = None;
                self.busy = None;
                let cancelled_after_completion =
                    result.is_ok() && self.cancel.load(Ordering::Relaxed);
                let cancelled_action = if self.accounts.pending_profile.is_some() {
                    "；已取消后续外观操作"
                } else if self.accounts.pending_launch.is_some() {
                    "；已取消后续启动"
                } else {
                    "；取消请求到达时登录已完成"
                };
                if cancelled_after_completion {
                    self.accounts.pending_launch = None;
                    self.accounts.pending_profile = None;
                }
                match *result {
                    Ok(ready) => {
                        self.microsoft = true;
                        self.accounts.selected = Some(ready.account.id.clone());
                        self.status = if cancelled_after_completion {
                            format!("{} 的登录已完成{cancelled_action}", ready.session.username)
                        } else {
                            format!("已登录 {}", ready.session.username)
                        };
                        self.accounts.refresh_at = next_refresh(ready.expires_at);
                        self.session = Some(ready.session);
                        if let Some(catalog) = &mut self.accounts.catalog {
                            catalog
                                .accounts
                                .retain(|account| account.id != ready.account.id);
                            catalog.accounts.insert(0, ready.account);
                            catalog.selected_id = self.accounts.selected.clone();
                        } else {
                            self.accounts.catalog = Some(AccountCatalog {
                                accounts: vec![ready.account],
                                selected_id: self.accounts.selected.clone(),
                            });
                        }
                        self.refresh_account_skin();
                        if let Some(pending) = self.accounts.pending_profile.take() {
                            if self.accounts.selected.as_ref() == Some(&pending.account_id) {
                                self.start_profile_operation(pending);
                            } else {
                                self.status = "登录的账号与原操作账号不同，未更改皮肤或披风".into();
                                self.push_hint(HintKind::Error, self.status.clone());
                            }
                            return;
                        }
                        self.push_hint(
                            if cancelled_after_completion {
                                HintKind::Info
                            } else {
                                HintKind::Success
                            },
                            self.status.clone(),
                        );
                        if let Some(pending) = self.accounts.pending_launch.take() {
                            if self.accounts.selected.as_ref() == Some(&pending.account_id) {
                                self.launch_with_current_session(pending.preview);
                            } else {
                                self.status = "登录的账号与原启动账号不同，已取消原启动操作".into();
                                self.push_hint(HintKind::Error, self.status.clone());
                            }
                        }
                    }
                    Err(error) => {
                        self.session = None;
                        self.accounts.refresh_at = None;
                        self.accounts.skin = None;
                        self.accounts.skin_bytes = None;
                        self.accounts.incoming_skin = None;
                        if !self.cancel.load(Ordering::Relaxed)
                            && self.accounts.can_reauthenticate(&error)
                        {
                            self.continue_reauthentication();
                            return;
                        }
                        self.accounts.pending_launch = None;
                        self.accounts.pending_profile = None;
                        self.present_account_failure(error, self.accounts.purpose);
                    }
                }
            }
            AccountEvent::Profile(request, result) => {
                if request != self.accounts.request || self.accounts.pending_profile.is_none() {
                    return;
                }
                self.busy = None;
                match *result {
                    Ok(ready) => {
                        // A completed remote mutation cannot be undone by a late cancel.
                        // Display only the authoritative profile returned by the server.
                        let pending = self.accounts.pending_profile.take().unwrap();
                        self.status = pending.change.success().into();
                        self.push_hint(HintKind::Success, self.status.clone());
                        self.accounts.refresh_at = next_refresh(ready.expires_at);
                        self.session = Some(ready.session);
                        if let Some(catalog) = &mut self.accounts.catalog {
                            if let Some(account) = catalog
                                .accounts
                                .iter_mut()
                                .find(|account| account.id == ready.account.id)
                            {
                                *account = ready.account;
                            }
                        }
                        self.accounts.skin = None;
                        self.accounts.skin_bytes = None;
                        self.accounts.incoming_skin = None;
                        self.refresh_account_skin();
                    }
                    Err(error) => {
                        if !self.cancel.load(Ordering::Relaxed)
                            && !self.accounts.profile_retry_used
                            && self.accounts.can_reauthenticate(&error)
                        {
                            let text = self
                                .accounts
                                .pending_profile
                                .as_ref()
                                .unwrap()
                                .change
                                .resume_hint();
                            self.accounts.profile_retry_used = true;
                            if error.issue == Some(auth::AuthenticationIssue::SessionUnauthorized) {
                                self.refresh_for_profile();
                            } else {
                                self.continue_reauthentication();
                            }
                            self.push_hint(HintKind::Info, text);
                            return;
                        }
                        self.accounts.pending_profile = None;
                        self.present_account_failure(error, LoginPurpose::Profile);
                    }
                }
            }
            AccountEvent::Removed(request, result) => {
                if request != self.accounts.request {
                    return;
                }
                self.busy = None;
                match result {
                    Ok(catalog) => {
                        self.accounts.selected = catalog.selected_id.clone();
                        self.accounts.catalog = Some(catalog);
                        self.session = None;
                        self.accounts.refresh_at = None;
                        self.accounts.skin = None;
                        self.accounts.skin_bytes = None;
                        self.accounts.incoming_skin = None;
                        self.status = "已移除本机保存的账号".into();
                    }
                    Err(error) => self.accounts.error = Some(error),
                }
            }
            AccountEvent::Skin(request, result) => {
                if request != self.accounts.request {
                    return;
                }
                match result {
                    Ok(bytes) => self.accounts.incoming_skin = Some(bytes),
                    Err(error) => {
                        self.record(error);
                        self.status = "账号已登录，皮肤暂时无法读取".into();
                    }
                }
            }
        }
    }
    pub(super) fn account_sidebar(&mut self, ui: &mut egui::Ui, rect: Rect, center: f32) {
        if let Some(bytes) = self.accounts.incoming_skin.take() {
            match image::load_from_memory(&bytes) {
                Ok(image)
                    if image.width() == 64 && (image.height() == 32 || image.height() == 64) =>
                {
                    let rgba = image.to_rgba8();
                    let pixels = egui::ColorImage::from_rgba_unmultiplied(
                        [rgba.width() as usize, rgba.height() as usize],
                        rgba.as_raw(),
                    );
                    self.accounts.skin = Some(ui.ctx().load_texture(
                        "microsoft-account-skin",
                        pixels,
                        egui::TextureOptions::NEAREST,
                    ));
                    self.accounts.skin_bytes = Some(bytes);
                }
                _ => self.accounts.error = Some("皮肤图片尺寸无效，已拒绝载入。".into()),
            }
        }
        let enabled = self.busy.is_none() && !self.accounts.loading;
        if let Some(session) = self.session.as_ref() {
            let name = session.username.clone();
            let head = Rect::from_center_size(
                egui::pos2(rect.center().x, center - 24.0),
                Vec2::splat(64.0),
            );
            if let Some(texture) = &self.accounts.skin {
                let height = texture.size()[1] as f32;
                ui.painter().image(
                    texture.id(),
                    head.shrink(8.0),
                    Rect::from_min_max(
                        egui::pos2(8.0 / 64.0, 8.0 / height),
                        egui::pos2(16.0 / 64.0, 16.0 / height),
                    ),
                    Color32::WHITE,
                );
                ui.painter().image(
                    texture.id(),
                    head.shrink(4.0),
                    Rect::from_min_max(
                        egui::pos2(40.0 / 64.0, 8.0 / height),
                        egui::pos2(48.0 / 64.0, 16.0 / height),
                    ),
                    Color32::WHITE,
                );
            } else {
                self.assets.head(ui, head);
            }
            ui_style::place_left(
                ui,
                Rect::from_center_size(
                    egui::pos2(rect.center().x, center + 34.0),
                    Vec2::new(rect.width() - 32.0, 30.0),
                ),
                egui::Label::new(
                    RichText::new(name)
                        .size(17.0)
                        .color(theme::palette(ui.ctx()).text),
                )
                .halign(egui::Align::Center)
                .truncate(),
            );
            let hover = Rect::from_min_max(
                egui::pos2(rect.left(), center - 65.0),
                egui::pos2(rect.right(), center + 100.0),
            );
            if ui.rect_contains_pointer(hover) || egui::Popup::is_any_open(ui.ctx()) {
                let panel = Rect::from_center_size(
                    egui::pos2(rect.center().x, center + 77.0),
                    Vec2::new(109.0, 30.0),
                );
                ui.painter()
                    .rect_filled(panel, 5, theme::palette(ui.ctx()).light);
                let mut refresh = None;
                let mut edit_skin = false;
                let mut edit_cape = false;
                let mut save = false;
                let mut switch = false;
                for (index, label) in ["皮肤与披风", "修改信息", "切换账号"]
                    .into_iter()
                    .enumerate()
                {
                    let button = Rect::from_min_size(
                        panel.min + Vec2::new(10.0 + index as f32 * 33.0, 3.0),
                        Vec2::splat(24.0),
                    );
                    let response = ui.place(
                        button,
                        egui::Button::new("")
                            .fill(Color32::TRANSPARENT)
                            .stroke(egui::Stroke::NONE),
                    );
                    draw_account_icon(
                        ui,
                        &mut self.accounts.icons,
                        index + 1,
                        button.shrink(3.0),
                        if response.hovered() {
                            theme::palette(ui.ctx()).accent
                        } else {
                            MUTED
                        },
                    );
                    response.clone().on_hover_text(label);
                    if index == 2 {
                        if response.clicked() && enabled {
                            switch = true;
                        }
                        continue;
                    }
                    egui::Popup::from_toggle_button_response(&response).show(|ui| {
                        if index == 0 {
                            if ui.add_enabled(enabled, egui::Button::new("修改皮肤")).clicked() { edit_skin = true; ui.close(); }
                            if ui.add_enabled(enabled, egui::Button::new("刷新皮肤")).clicked() { refresh = Some(ProfileChange::RefreshSkin); ui.close(); }
                            if ui.add_enabled(self.accounts.skin_bytes.is_some(), egui::Button::new("保存皮肤文件")).clicked() { save = true; ui.close(); }
                            ui.separator();
                            if ui.add_enabled(enabled, egui::Button::new("修改披风")).clicked() { edit_cape = true; ui.close(); }
                            if ui.add_enabled(enabled, egui::Button::new("刷新披风列表")).clicked() { refresh = Some(ProfileChange::RefreshCapes); ui.close(); }
                            if ui.add_enabled(enabled, egui::Button::new("重置为默认皮肤")).clicked() { self.accounts.reset_skin = true; ui.close(); }
                            if ui.button("使用 CDKEY 兑换奖励").clicked() { open_account_url("https://www.minecraft.net/zh-hans/redeem", &mut self.accounts.error); ui.close(); }
                        } else {
                            for (text, url) in [("修改密码", "https://account.live.com/password/Change"), ("修改玩家名", "https://www.minecraft.net/zh-hans/msaprofile/mygames/editprofile")] {
                                if ui.button(text).clicked() { open_account_url(url, &mut self.accounts.error); ui.close(); }
                            }
                        }
                    });
                }
                if let Some(change) = refresh {
                    self.change_account_profile(change);
                }
                if edit_skin {
                    self.accounts.upload = rfd::FileDialog::new()
                        .set_title("选择 Minecraft 皮肤")
                        .add_filter("PNG 皮肤", &["png"])
                        .pick_file();
                }
                if edit_cape {
                    self.accounts.cape_picker = true;
                    self.accounts.cape_draft = None;
                }
                if save {
                    if let (Some(bytes), Some(path)) = (
                        self.accounts.skin_bytes.as_ref(),
                        rfd::FileDialog::new()
                            .set_file_name("skin.png")
                            .add_filter("PNG", &["png"])
                            .save_file(),
                    ) {
                        if let Err(error) = std::fs::write(path, bytes) {
                            self.accounts.error = Some(format!("保存皮肤失败：{error}"));
                        }
                    }
                }
                if switch {
                    self.switch_account();
                }
            }
        } else {
            draw_account_icon(
                ui,
                &mut self.accounts.icons,
                0,
                Rect::from_center_size(
                    egui::pos2(rect.center().x, center - 24.0),
                    Vec2::new(40.0, 43.2),
                ),
                theme::palette(ui.ctx()).border,
            );
            let combo = Rect::from_min_size(
                egui::pos2(rect.left() + 20.0, center + 23.0),
                Vec2::new(rect.width() - 110.0, 28.0),
            );
            let label = self
                .accounts
                .selected
                .as_ref()
                .and_then(|id| {
                    self.accounts
                        .catalog
                        .as_ref()?
                        .accounts
                        .iter()
                        .find(|account| &account.id == id)
                        .map(|account| account.username.clone())
                })
                .unwrap_or_else(|| "添加新账号".into());
            let mut chosen = None;
            let mut remove = None;
            let entries = self
                .accounts
                .catalog
                .as_ref()
                .map(|catalog| catalog.accounts.clone())
                .unwrap_or_default();
            ui.scope_builder(egui::UiBuilder::new().max_rect(combo), |ui| {
                ui.add_enabled_ui(enabled, |ui| {
                    ui_style::PclComboBox::from_id_salt("microsoft-account-selector")
                        .width(combo.width())
                        .selected_text(&label)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(self.accounts.selected.is_none(), "添加新账号")
                                .clicked()
                            {
                                chosen = Some(None);
                            }
                            for account in &entries {
                                let (response, removed) = ui.selectable_label_with_remove(
                                    self.accounts.selected.as_ref() == Some(&account.id),
                                    &account.username,
                                );
                                if response.clicked() {
                                    chosen = Some(Some(account.id.clone()));
                                }
                                if removed {
                                    remove = Some(account.id.clone());
                                }
                            }
                        });
                });
            });
            if enabled {
                if let Some(selected) = chosen {
                    self.accounts.selected = selected;
                }
                if let Some(id) = remove {
                    self.accounts.remove = Some(id);
                }
            }
            let button = Rect::from_min_size(
                combo.right_top() + Vec2::new(10.0, 0.0),
                Vec2::new(60.0, 28.0),
            );
            let login_label = if self.accounts.auth_pending {
                format!(
                    "{}%",
                    self.accounts
                        .stage
                        .map_or(0, |stage| stage.percent().min(98))
                )
            } else {
                "登录".into()
            };
            if ui_style::outline_button(ui, button, &login_label, None, true, enabled).clicked() {
                self.login();
            }
            if self.accounts.selected.is_some() {
                let relogin = Rect::from_min_size(
                    egui::pos2(rect.left() + 20.0, center + 59.0),
                    Vec2::new(260.0, 22.0),
                );
                if ui
                    .place(relogin, egui::Button::new("重新登录此账号").frame(false))
                    .clicked()
                    && enabled
                {
                    self.reauthenticate_account();
                }
            }
            let y = center
                + if self.accounts.selected.is_some() {
                    95.0
                } else {
                    81.0
                };
            for (index,(text,url)) in [("»  购买正版","https://www.xbox.com/zh-cn/games/store/minecraft-java-bedrock-edition-for-pc/9nxp44l49shj"),("»  前往官网","https://www.minecraft.net/zh-hans")].into_iter().enumerate(){
                let r=Rect::from_min_size(egui::pos2(rect.center().x-110.0+index as f32*125.0,y),Vec2::new(100.0,22.0));
                if ui.place(r,egui::Button::new(RichText::new(text).size(12.0).color(Color32::from_gray(165))).frame(false)).clicked(){open_account_url(url,&mut self.accounts.error);}
            }
        }
    }
    pub(super) fn account_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(path) = self.accounts.upload.clone() {
            let caption = format!(
                "已选择 {}。\n请选择皮肤模型。经典模型为 4 像素手臂，纤细模型为 3 像素手臂。\n选择后会上传并应用到当前正版账号。",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            if let Some(action) = account_modal(
                ctx,
                "skin-model",
                "选择皮肤模型",
                &caption,
                &["经典 · Steve", "纤细 · Alex", "取消"],
            ) {
                self.accounts.upload = None;
                let variant = match action {
                    0 => Some(auth::SkinVariant::Classic),
                    1 => Some(auth::SkinVariant::Slim),
                    _ => None,
                };
                if let Some(variant) = variant {
                    self.change_account_profile(ProfileChange::Upload(path, variant));
                }
            }
            return;
        }
        if self.accounts.reset_skin {
            if let Some(action) = account_modal(
                ctx,
                "reset-skin",
                "重置皮肤",
                "恢复此正版账号的默认皮肤？",
                &["重置", "取消"],
            ) {
                self.accounts.reset_skin = false;
                if action == 0 {
                    self.change_account_profile(ProfileChange::Reset);
                }
            }
            return;
        }
        if self.accounts.cape_picker {
            let capes = self
                .accounts
                .catalog
                .as_ref()
                .and_then(|catalog| {
                    catalog
                        .accounts
                        .iter()
                        .find(|account| Some(&account.id) == self.accounts.selected.as_ref())
                })
                .map(|account| account.profile.capes.clone())
                .unwrap_or_default();
            let draft = self
                .accounts
                .cape_draft
                .get_or_insert_with(|| CapeDraft::from_capes(&capes));
            if let Some(action) = cape_modal(ctx, &capes, draft) {
                self.accounts.cape_picker = false;
                self.accounts.cape_draft = None;
                match action {
                    CapeAction::Select(cape) => {
                        self.accounts.cape_picker = false;
                        self.change_account_profile(ProfileChange::Cape(cape));
                    }
                    CapeAction::Close => self.accounts.cape_picker = false,
                }
            }
            return;
        }
        if let Some(device) = &mut self.accounts.device {
            if !device.opened {
                ctx.copy_text(device.code.clone());
                open_account_url(&device.url, &mut self.accounts.error);
                device.opened = true;
            }
            let expired = Instant::now() >= device.expires;
            let caption = format!(
                "登录网页将自动开启，请在网页中输入 {}（已自动复制）。\n\n如果网络环境不佳，网页可能一直加载不出来，届时请检查网络连接。\n你也可以用其他设备打开 {} 并输入上述代码。{}",
                device.code,
                device.url,
                if expired {
                    "\n\n设备代码已过期，请重新登录。"
                } else {
                    ""
                }
            );
            let action = account_modal_with_options(
                ctx,
                "device-login",
                "登录 Minecraft",
                &caption,
                &["重新打开网页", "复制代码", "取消"],
                ModalOptions::device(),
            );
            match action {
                Some(0) => open_account_url(&device.url, &mut self.accounts.error),
                Some(1) => ctx.copy_text(device.code.clone()),
                Some(2) => {
                    self.cancel.store(true, Ordering::Relaxed);
                    self.accounts.device = None;
                    self.status = "正在取消登录…".into();
                }
                _ => {}
            }
            return;
        }
        if let Some(id) = self.accounts.remove.clone() {
            let name = self
                .accounts
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.accounts.iter().find(|account| account.id == id))
                .map(|account| account.username.as_str())
                .unwrap_or("此账号");
            if let Some(action) = account_modal(
                ctx,
                "remove-account",
                "移除账号",
                &format!("从此设备移除 {name} 的保存登录信息？\n游戏文件和微软账户不会被删除。"),
                &["移除", "取消"],
            ) {
                self.accounts.remove = None;
                if action == 0 {
                    self.remove_selected_account(id);
                }
            }
            return;
        }
        if let Some(message) = self.accounts.error.clone() {
            let actions = recovery_actions(self.accounts.error_issue, self.accounts.error_relogin);
            let labels: Vec<_> = actions.iter().map(|(label, _)| *label).collect();
            let keep_password =
                self.accounts.error_issue == Some(auth::AuthenticationIssue::PasswordLoginRequired);
            if let Some(index) = account_modal_with_options(
                ctx,
                "account-error",
                self.accounts.error_title.unwrap_or("账号操作未完成"),
                &message,
                &labels,
                ModalOptions {
                    keep_open_buttons: if keep_password { 1 << 1 } else { 0 },
                    ..if self.accounts.error_warning {
                        ModalOptions::warning()
                    } else {
                        ModalOptions::default()
                    }
                },
            ) {
                if keep_password && index == 1 {
                    if let Some((_, RecoveryAction::Web(url))) = actions.get(index) {
                        let mut open_error = None;
                        open_account_url(url, &mut open_error);
                        if let Some(error) = open_error {
                            self.push_hint(HintKind::Error, error);
                        }
                    }
                    return;
                }
                self.accounts.error = None;
                self.accounts.error_relogin = false;
                self.accounts.error_issue = None;
                self.accounts.error_title = None;
                self.accounts.error_warning = false;
                match actions.get(index).map(|(_, action)| *action) {
                    Some(RecoveryAction::Relogin) => self.reauthenticate_account(),
                    Some(RecoveryAction::Settings) => self.open_client_id_settings(),
                    Some(RecoveryAction::Web(url)) => {
                        open_account_url(url, &mut self.accounts.error)
                    }
                    _ => {}
                }
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecoveryAction {
    Relogin,
    Settings,
    Web(&'static str),
    Close,
}
#[derive(Debug, PartialEq, Eq)]
enum FailurePresentation {
    Hint,
    Modal(&'static str, bool),
}
fn failure_presentation(
    issue: Option<auth::AuthenticationIssue>,
    purpose: LoginPurpose,
) -> FailurePresentation {
    use auth::AuthenticationIssue as Issue;
    use FailurePresentation::{Hint, Modal};
    match issue {
        Some(Issue::PasswordLoginRequired) => Modal("需要使用密码登录", false),
        Some(Issue::ClientConfiguration) => Modal("登录设置提示", false),
        Some(Issue::MinecraftAccessDenied) => Modal("登录失败", true),
        Some(Issue::XboxProfileRequired | Issue::FamilyPermissionRequired) => {
            Modal("登录提示", false)
        }
        Some(
            Issue::OwnershipRequired | Issue::MinecraftProfileRequired | Issue::RegionUnavailable,
        ) => Modal("登录失败", false),
        Some(Issue::SecurityInterrupt | Issue::Suspended) => Modal("登录失败", true),
        _ if purpose == LoginPurpose::Launch => Modal("启动失败", false),
        _ if purpose == LoginPurpose::Profile => Hint,
        Some(
            Issue::AuthorizationDeclined
            | Issue::CodeExpired
            | Issue::RateLimited
            | Issue::ServiceUnavailable,
        ) => Hint,
        _ => Modal("错误", true),
    }
}
fn recovery_actions(
    issue: Option<auth::AuthenticationIssue>,
    relogin: bool,
) -> Vec<(&'static str, RecoveryAction)> {
    use auth::AuthenticationIssue as Issue;
    use RecoveryAction::{Close, Relogin, Settings, Web};
    match issue {
        Some(Issue::ClientConfiguration | Issue::MinecraftAccessDenied) => {
            vec![("应用设置", Settings), ("关闭", Close)]
        }
        Some(Issue::XboxProfileRequired) => vec![
            ("注册", Web("https://www.xbox.com/zh-CN/")),
            ("取消", Close),
        ],
        Some(Issue::FamilyPermissionRequired) => vec![
            ("查看家庭设置", Web("https://account.microsoft.com/family/")),
            ("取消", Close),
        ],
        Some(Issue::OwnershipRequired) => vec![
            (
                "购买 Minecraft",
                Web(
                    "https://www.xbox.com/zh-cn/games/store/minecraft-java-bedrock-edition-for-pc/9nxp44l49shj",
                ),
            ),
            ("取消", Close),
        ],
        Some(Issue::MinecraftProfileRequired) => vec![
            (
                "创建档案",
                Web("https://www.minecraft.net/zh-hans/msaprofile/mygames/editprofile"),
            ),
            ("取消", Close),
        ],
        Some(Issue::SecurityInterrupt | Issue::Suspended) => vec![
            ("微软账户", Web("https://account.microsoft.com/")),
            ("关闭", Close),
        ],
        Some(Issue::PasswordLoginRequired) => vec![
            ("重新登录", Relogin),
            ("设置密码", Web("https://account.live.com/password/Change")),
            ("取消", Close),
        ],
        Some(Issue::RegionUnavailable | Issue::RateLimited | Issue::ServiceUnavailable) => {
            vec![("我知道了", Close)]
        }
        Some(_) => vec![("重新登录", Relogin), ("取消", Close)],
        None if relogin => vec![("重新登录", Relogin), ("关闭", Close)],
        None => vec![("关闭", Close)],
    }
}
fn refresh_delay(expires_at: u64, now: u64) -> Duration {
    Duration::from_secs(expires_at.saturating_sub(now).saturating_sub(60).max(1))
}
fn next_refresh(expires_at: u64) -> Option<Instant> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Instant::now().checked_add(refresh_delay(expires_at, now))
}
fn open_account_url(url: &str, error: &mut Option<String>) {
    if let Err(reason) = webbrowser::open(url) {
        *error = Some(format!("无法打开浏览器：{reason}"));
    }
}
// Exact vector paths from upstream PageLoginMs.xaml / PageLoginMsSkin.xaml.
const ACCOUNT_ICONS: [&str; 4] = [
    r##"<svg xmlns="http://www.w3.org/2000/svg" width="1100" height="1100" viewBox="0 0 1100 1100"><path d="M660.338 528.065c63.61-46.825 105.131-121.964 105.131-206.83 0-141.7-115.29-256.987-256.997-256.987-141.706 0-256.998 115.288-256.998 256.987 0 85.901 42.52 161.887 107.456 208.562-152.1 59.92-260.185 207.961-260.185 381.077 0 21.276 17.253 38.53 38.53 38.53 21.278 0 38.53-17.254 38.53-38.53 0-183.426 149.232-332.671 332.667-332.671 1.589 0 3.113-0.207 4.694-0.244 0.8 0.056 1.553 0.244 2.362 0.244 183.434 0 332.664 149.245 332.664 332.671 0 21.276 17.255 38.53 38.533 38.53 21.277 0 38.53-17.254 38.53-38.53 0-174.885-110.354-324.13-264.917-382.809z m-331.803-206.83c0-99.22 80.72-179.927 179.935-179.927s179.937 80.708 179.937 179.927c0 99.203-80.721 179.91-179.937 179.91s-179.935-80.708-179.935-179.91z" fill="white"/></svg>"##,
    r##"<svg xmlns="http://www.w3.org/2000/svg" width="1100" height="1100" viewBox="0 0 1100 1100"><path d="M764.0003 0.076794a215.433442 215.433442 0 0 0-76.282279 13.950954l-9.06172 3.686123c-49.148314 20.734445-88.518161 30.53851-115.498538 30.53851-27.031573 0-66.478214-9.855261-116.676049-31.050471a217.583681 217.583681 0 0 0-72.186586-16.766743l-9.138515-0.307177-3.839712-0.051196-8.626553 0.307177-4.223683 0.281579A160.755943 160.755943 0 0 0 250.477214 45.385396l-219.32435 212.438467a102.392321 102.392321 0 0 0-11.263156 134.210734l106.360023 144.552359 3.378947 4.351674a102.469115 102.469115 0 0 0 112.810739 31.690423l5.657176-2.175837V883.210559a140.789441 140.789441 0 0 0 140.789441 140.789441h372.170487l5.657175-0.102392a140.789441 140.789441 0 0 0 135.132266-140.687049l-0.025599-318.6705 1.177512 0.716747a102.392321 102.392321 0 0 0 141.941355-44.233483l70.906682-144.39877a102.392321 102.392321 0 0 0-20.657651-118.647101L875.86391 45.385396A160.730345 160.730345 0 0 0 764.0003 0.076794z m-357.349199 111.454041C468.444867 137.615279 520.562558 150.644702 563.157763 150.644702c42.492813 0 94.584906-12.978227 156.199485-38.985876A112.580356 112.580356 0 0 1 764.0003 102.469115c15.154063 0 29.693773 5.887558 40.598555 16.433967L1023.923206 331.495138l-70.906682 144.39877-93.023423-65.684674a38.39712 38.39712 0 0 0-60.53946 31.383247V883.210559a38.39712 38.39712 0 0 1-38.39712 38.39712H388.886034a38.39712 38.39712 0 0 1-38.397121-38.39712V445.176212a38.39712 38.39712 0 0 0-61.691373-30.487314l-80.070795 61.20501L102.392321 331.341549l219.32435-212.438467a58.363623 58.363623 0 0 1 35.581332-16.229183L362.110442 102.469115l6.322726 0.179186a115.191361 115.191361 0 0 1 38.217933 8.882534z" fill="white"/></svg>"##,
    r##"<svg xmlns="http://www.w3.org/2000/svg" width="1100" height="1100" viewBox="0 0 1100 1100"><path d="M462.336 924.891429L73.142857 950.857143l25.965714-389.193143 467.017143-467.017143a73.398857 73.398857 0 0 1 103.789715 0l259.437714 259.437714a73.398857 73.398857 0 0 1 0 103.789715l-467.017143 467.017143z m155.684571-778.349715L202.861714 561.664l259.474286 259.474286L877.458286 405.942857 618.057143 146.541714zM151.003429 873.033143l233.508571-25.965714-207.579429-207.579429-25.965714 233.545143z m544.841142-492.982857l51.894858 51.894857-233.508572 233.508571-51.931428-51.894857 233.545142-233.508571z" fill="white"/></svg>"##,
    r##"<svg xmlns="http://www.w3.org/2000/svg" width="1100" height="1100" viewBox="0 0 1100 1100"><path d="M512 0A512 512 0 1 0 1024 512 512 512 0 0 0 512 0z m0 941.407086A429.407086 429.407086 0 1 1 941.407086 512 429.407086 429.407086 0 0 1 512 941.407086z m235.139657-368.64H264.045714a43.505371 43.505371 0 0 0-31.246628 72.060343l159.305143 166.765714a41.281829 41.281829 0 1 0 59.626057-56.788114l-94.997943-99.474286H747.227429a41.311086 41.311086 0 1 0 0-82.622172zM262.670629 450.940343H745.764571a43.505371 43.505371 0 0 0 31.246629-72.352914l-159.305143-166.765715a41.281829 41.281829 0 1 0-59.713828 57.022172l94.968685 99.474285H262.670629a41.281829 41.281829 0 0 0 0 82.563658z" fill="white"/></svg>"##,
];
fn draw_account_icon(
    ui: &egui::Ui,
    textures: &mut Vec<egui::TextureHandle>,
    index: usize,
    rect: Rect,
    color: Color32,
) {
    if textures.is_empty() {
        for (index, svg) in ACCOUNT_ICONS.iter().enumerate() {
            let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default())
                .expect("upstream account SVG parses");
            let bounds = tree.root().abs_bounding_box();
            let factor = 128.0 / bounds.height();
            let width = (bounds.width() * factor).ceil() as u32;
            let mut pixels =
                resvg::tiny_skia::Pixmap::new(width, 128).expect("bounded account icon");
            let transform = resvg::tiny_skia::Transform::from_scale(factor, factor)
                .pre_translate(-bounds.x(), -bounds.y());
            resvg::render(&tree, transform, &mut pixels.as_mut());
            textures.push(ui.ctx().load_texture(
                format!("account-action-{index}"),
                egui::ColorImage::from_rgba_premultiplied([width as usize, 128], pixels.data()),
                egui::TextureOptions::LINEAR,
            ));
        }
    }
    let texture = &textures[index];
    let size = texture.size_vec2();
    let scale = (rect.width() / size.x).min(rect.height() / size.y);
    let target = Rect::from_center_size(rect.center(), size * scale);
    ui.painter().image(
        texture.id(),
        target,
        Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        color,
    );
}
#[derive(Debug, PartialEq, Eq)]
enum CapeAction {
    Select(Option<String>),
    Close,
}
fn cape_name(alias: &str) -> &str {
    match alias {
        "Migrator" => "迁移者披风",
        "MapMaker" => "Realms 地图制作者披风",
        "Moderator" => "Mojira 管理员披风",
        "Translator-Chinese" => "Crowdin 中文翻译者披风",
        "Translator" => "Crowdin 翻译者披风",
        "Cobalt" => "Cobalt 披风",
        "Vanilla" => "原版披风",
        "Minecon2011" => "Minecon 2011 参与者披风",
        "Minecon2012" => "Minecon 2012 参与者披风",
        "Minecon2013" => "Minecon 2013 参与者披风",
        "Minecon2015" => "Minecon 2015 参与者披风",
        "Minecon2016" => "Minecon 2016 参与者披风",
        "Cherry Blossom" => "樱花披风",
        "15th Anniversary" => "15 周年纪念披风",
        "Purple Heart" => "紫色心形披风",
        "Follower's" => "追随者披风",
        "MCC 15th Year" => "MCC 15 周年披风",
        "Minecraft Experience" => "村民救援披风",
        "Mojang Office" => "Mojang 办公室披风",
        "Home" => "家园披风",
        "Menace" => "入侵披风",
        "Yearn" => "渴望披风",
        "Common" => "普通披风",
        "Pan" => "薄煎饼披风",
        "Founder's" => "创始人披风",
        "Copper" => "铜披风",
        "Zombie Horse" => "僵尸马披风",
        "Builder" => "建造者披风",
        "Crafter" => "工匠披风",
        "" => "披风",
        other => other,
    }
}
fn cape_modal(
    ctx: &egui::Context,
    capes: &[auth::MinecraftCape],
    draft: &mut CapeDraft,
) -> Option<CapeAction> {
    let width = (ctx.content_rect().width() - 50.0).clamp(400.0, 600.0);
    let height =
        ((capes.len() + 1) as f32 * 24.0).min((ctx.content_rect().height() - 225.0).max(100.0));
    let action = modal_frame_with_options(
        ctx,
        "cape-picker",
        "选择披风",
        width,
        height,
        &["确定", "取消"],
        ModalOptions {
            focus_first: false,
            ..Default::default()
        },
        |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            #[cfg(test)]
            draft.rows.clear();
            egui::ScrollArea::vertical()
                .id_salt("cape-list")
                .max_height(height)
                .show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, selected, text: &str| {
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 24.0),
                            egui::Sense::hover(),
                        );
                        let mut row = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(rect)
                                .layout(egui::Layout::left_to_right(egui::Align::Center)),
                        );
                        row.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                        row.set_clip_rect(rect.intersect(ui.clip_rect()));
                        row.add(egui::RadioButton::new(
                            selected,
                            RichText::new(text).size(13.0),
                        ))
                        .on_hover_text(text)
                    };
                    let response = row(ui, draft.selected.is_none(), "无披风");
                    #[cfg(test)]
                    draft.rows.push(response.rect);
                    if response.clicked() {
                        draft.selected = None;
                    }
                    for cape in capes {
                        let response = row(
                            ui,
                            draft.selected.as_ref() == Some(&cape.id),
                            cape_name(&cape.alias),
                        );
                        #[cfg(test)]
                        draft.rows.push(response.rect);
                        if response.clicked() {
                            draft.selected = Some(cape.id.clone());
                        }
                    }
                });
        },
    );
    action.map(|index| {
        if index == 0 {
            CapeAction::Select(draft.selected.clone())
        } else {
            CapeAction::Close
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn failed(issue: auth::AuthenticationIssue) -> AccountFailure {
        AccountFailure::from_error(issue.into(), "登录失败")
    }
    #[test]
    fn automatic_reauthentication_is_bounded_and_never_repeats_committed_mutation() {
        use auth::AuthenticationIssue as Issue;
        let mut state = AccountUiState::default();
        assert!(state.can_reauthenticate(&failed(Issue::ReauthenticationRequired)));
        assert!(state.can_reauthenticate(&failed(Issue::SessionUnauthorized)));
        for issue in [
            Issue::NetworkUnavailable,
            Issue::SecurityInterrupt,
            Issue::Suspended,
            Issue::MinecraftAccessDenied,
            Issue::FamilyPermissionRequired,
            Issue::OwnershipRequired,
        ] {
            assert!(!state.can_reauthenticate(&failed(issue)));
        }
        let committed = AccountFailure::from_error(
            anyhow::Error::new(Issue::SessionUnauthorized)
                .context(accounts::ProfileChangeCommitted),
            "刷新资料失败",
        );
        assert!(committed.profile_committed);
        assert!(!state.can_reauthenticate(&committed));
        state.automatic_reauth_used = true;
        assert!(!state.can_reauthenticate(&failed(Issue::ReauthenticationRequired)));
        state.automatic_reauth_used = false;
        state.device_auth = true;
        assert!(!state.can_reauthenticate(&failed(Issue::ReauthenticationRequired)));
    }
    #[test]
    fn expired_refresh_reenters_device_once_preserving_launch_and_filters_old_events() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        // Invalid UUID is rejected before constructing any network request. Never
        // use Launcher::new or a real client/catalog in this event fixture.
        app.settings.microsoft_client_id = "fixture-invalid-client".into();
        app.accounts.request = 4;
        app.accounts.auth_pending = true;
        app.accounts.purpose = LoginPurpose::Launch;
        app.accounts.pending_launch = Some(PendingLaunch {
            account_id: "original".into(),
            preview: true,
        });
        app.busy = Some("fixture".into());
        app.handle_account_event(AccountEvent::Ready(
            4,
            Box::new(Err(failed(
                auth::AuthenticationIssue::ReauthenticationRequired,
            ))),
        ));
        assert_eq!(app.accounts.request, 5);
        assert!(
            app.accounts.auth_pending
                && app.accounts.automatic_reauth_used
                && app.accounts.device_auth
        );
        assert_eq!(
            app.accounts.pending_launch,
            Some(PendingLaunch {
                account_id: "original".into(),
                preview: true
            })
        );
        app.handle_account_event(AccountEvent::Ready(
            4,
            Box::new(Err(failed(auth::AuthenticationIssue::CodeExpired))),
        ));
        assert!(app.busy.is_some());
        app.handle_account_event(AccountEvent::Ready(
            5,
            Box::new(Err(failed(
                auth::AuthenticationIssue::ReauthenticationRequired,
            ))),
        ));
        assert_eq!(
            app.accounts.request, 5,
            "a second failure must not start another login"
        );
        assert!(!app.accounts.auth_pending);
        assert!(app.accounts.pending_launch.is_none());
        assert_eq!(app.accounts.error_title, Some("启动失败"));
        app.busy = Some("new independent work".into());
        app.handle_account_event(AccountEvent::Ready(
            5,
            Box::new(Err(failed(auth::AuthenticationIssue::CodeExpired))),
        ));
        assert_eq!(app.busy.as_deref(), Some("new independent work"));
    }
    #[test]
    fn device_web_success_survives_later_failure_and_has_no_refresh_false_positive() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        app.accounts.request = 9;
        app.accounts.auth_pending = true;
        app.accounts.device_auth = true;
        app.handle_account_event(AccountEvent::Stage(8, auth::LoginStage::Xbox));
        assert!(!app.hints.contains(HintKind::Success, "网页登录成功！"));
        app.handle_account_event(AccountEvent::Stage(9, auth::LoginStage::Xbox));
        assert!(app.hints.contains(HintKind::Success, "网页登录成功！"));
        assert!(!app.accounts.web_success(9, auth::LoginStage::Xbox));
        app.handle_account_event(AccountEvent::Ready(
            9,
            Box::new(Err(failed(
                auth::AuthenticationIssue::MinecraftAccessDenied,
            ))),
        ));
        assert!(app.hints.contains(HintKind::Success, "网页登录成功！"));
        assert_eq!(app.accounts.error_title, Some("登录失败"));
        assert!(app.accounts.error_warning);
        assert!(app.session.is_none());
        let mut refresh = AccountUiState {
            request: 1,
            auth_pending: true,
            ..Default::default()
        };
        assert!(!refresh.web_success(1, auth::LoginStage::Xbox));
    }
    #[test]
    fn login_hint_launch_dialog_and_cancel_are_distinct() {
        use auth::AuthenticationIssue as Issue;
        assert_eq!(
            failure_presentation(Some(Issue::CodeExpired), LoginPurpose::Login),
            FailurePresentation::Hint
        );
        assert_eq!(
            failure_presentation(Some(Issue::CodeExpired), LoginPurpose::Launch),
            FailurePresentation::Modal("启动失败", false)
        );
        assert_eq!(
            failure_presentation(Some(Issue::Suspended), LoginPurpose::Login),
            FailurePresentation::Modal("登录失败", true)
        );
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        app.present_account_failure(failed(Issue::CodeExpired), LoginPurpose::Login);
        assert!(app.hints.contains(
            HintKind::Error,
            "登录失败：登录用时太长，设备代码已过期，请重新登录。"
        ));
        assert!(app.accounts.error.is_none());
        app.present_account_failure(
            AccountFailure::from_error(auth::AuthenticationCancelled.into(), "登录"),
            LoginPurpose::Login,
        );
        assert!(app.hints.contains(HintKind::Info, "登录已取消"));
        assert!(app.accounts.error.is_none());
    }
    fn synthetic_session(id: &str) -> AccountSession {
        let uuid = "00112233445566778899aabbccddeeff";
        AccountSession {
            account: accounts::AccountSummary {
                id: id.into(),
                username: "Fixture".into(),
                uuid: uuid.into(),
                client_id: "fixture-invalid-client".into(),
                profile: auth::MinecraftProfile {
                    id: uuid.into(),
                    name: "Fixture".into(),
                    skins: vec![],
                    capes: vec![],
                },
                last_used_at: 0,
            },
            session: pcl_core::model::Session {
                username: "Fixture".into(),
                uuid: uuid.into(),
                access_token: "FIXTURE_ONLY".into(),
                user_type: "msa".into(),
            },
            expires_at: u64::MAX / 2,
        }
    }
    #[test]
    fn desktop_cannot_switch_to_or_launch_with_an_offline_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        assert!(app.microsoft);
        app.accounts.selected = Some("existing-selection".into());
        app.select_account_mode(false);
        assert!(app.microsoft);
        assert_eq!(app.accounts.selected.as_deref(), Some("existing-selection"));
        assert!(app.error.as_deref().unwrap().contains("离线登录已禁用"));
        app.settings.selected_version = Some("fixture".into());
        app.session = Some(auth::offline_session("Fixture").unwrap());
        let path = dir.path().join("launch.command");
        for action in [
            super::super::LaunchAction::Run,
            super::super::LaunchAction::Preview,
            super::super::LaunchAction::Export {
                path: path.clone(),
                format: pcl_core::launch_script::ScriptFormat::MacCommand,
            },
        ] {
            app.error = None;
            app.start_launch(action);
            assert!(app.error.as_deref().unwrap().contains("离线登录已禁用"));
            assert!(app.busy.is_none());
            assert!(app.game_pid.is_none());
            assert!(!path.exists());
        }
    }
    #[test]
    fn changing_identity_during_reauthentication_cancels_original_launch_and_mutation() {
        for profile in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = super::super::event_tests::fixture(dir.path());
            app.accounts.request = 2;
            app.accounts.auth_pending = true;
            app.accounts.device_auth = true;
            if profile {
                app.accounts.pending_profile = Some(PendingProfile {
                    account_id: "original".into(),
                    change: ProfileChange::Cape(None),
                });
            } else {
                app.accounts.pending_launch = Some(PendingLaunch {
                    account_id: "original".into(),
                    preview: true,
                });
            }
            app.handle_account_event(AccountEvent::Ready(
                2,
                Box::new(Ok(synthetic_session("different"))),
            ));
            assert_eq!(
                app.accounts.selected.as_deref(),
                Some("different"),
                "keep the explicitly completed new login"
            );
            assert!(
                app.accounts.pending_profile.is_none() && app.accounts.pending_launch.is_none()
            );
            assert!(app.busy.is_none(), "no continuation worker may run");
            assert!(
                !app.version_view,
                "do not call the launch path for the other account"
            );
            assert!(app.status.contains("账号不同"));
        }
    }
    #[test]
    fn late_cancel_keeps_completed_login_but_drops_pending_remote_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        app.accounts.request = 1;
        app.accounts.auth_pending = true;
        app.accounts.pending_profile = Some(PendingProfile {
            account_id: "same".into(),
            change: ProfileChange::Cape(None),
        });
        app.cancel.store(true, Ordering::Relaxed);
        app.handle_account_event(AccountEvent::Ready(
            1,
            Box::new(Ok(synthetic_session("same"))),
        ));
        assert!(app.session.is_some());
        assert!(app.accounts.pending_profile.is_none() && app.busy.is_none());
        assert!(app.status.contains("已取消后续外观操作"));
        assert!(app.accounts.error.is_none());
    }
    #[test]
    fn long_cape_name_stays_within_its_radio_row() {
        let capes = vec![auth::MinecraftCape {
            id: "long-fixture".into(),
            state: "ACTIVE".into(),
            alias: "a very long cape alias ".repeat(50),
            url: String::new(),
        }];
        let ctx = egui::Context::default();
        let mut draft = CapeDraft::from_capes(&capes);
        for time in [0.0, 0.5] {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(500.0, 450.0),
                    )),
                    time: Some(time),
                    ..Default::default()
                },
                |ctx| {
                    assert!(cape_modal(ctx, &capes, &mut draft).is_none());
                },
            );
        }
        assert_eq!(draft.rows.len(), 2);
        for row in &draft.rows {
            assert!(row.width() <= 450.0 - 66.0, "{row:?}");
            assert!(row.height() <= 24.0, "{row:?}");
        }
        assert_eq!(draft.selected.as_deref(), Some("long-fixture"));
    }

    #[test]
    fn cape_radio_click_only_changes_draft_and_cancel_never_submits() {
        let capes = vec![auth::MinecraftCape {
            id: "one".into(),
            state: "ACTIVE".into(),
            alias: "Migrator".into(),
            url: String::new(),
        }];
        let ctx = egui::Context::default();
        let mut draft = CapeDraft::from_capes(&capes);
        let frame = |time, events, draft: &mut CapeDraft| {
            let mut action = None;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(850.0, 600.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    action = cape_modal(ctx, &capes, draft);
                },
            );
            action
        };
        frame(0.0, vec![], &mut draft);
        frame(1.0, vec![], &mut draft);
        assert_eq!(draft.selected.as_deref(), Some("one"));
        let pos = draft.rows[0].center();
        for (time, pressed) in [(1.1, true), (1.2, false)] {
            let action = frame(
                time,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                &mut draft,
            );
            assert!(
                action.is_none(),
                "a radio click must not create a remote mutation"
            );
        }
        assert_eq!(draft.selected, None);
        let key = |key| {
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]
        };
        assert_eq!(
            frame(1.3, key(egui::Key::Escape), &mut draft),
            Some(CapeAction::Close)
        );
        assert_eq!(cape_name("Migrator"), "迁移者披风");
        assert_eq!(cape_name("Crafter"), "工匠披风");
        assert_eq!(cape_name("Future Cape"), "Future Cape");
    }
    #[test]
    fn cape_confirm_uses_current_draft_not_original_active_value() {
        let ctx = egui::Context::default();
        let mut draft = CapeDraft {
            selected: Some("chosen".into()),
            ..Default::default()
        };
        let mut action = None;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(850.0, 600.0),
                )),
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ctx| {
                action = cape_modal(ctx, &[], &mut draft);
            },
        );
        assert_eq!(action, Some(CapeAction::Select(Some("chosen".into()))));
    }
    #[test]
    fn only_typed_auth_cancellation_is_treated_as_cancelled() {
        let cancelled = AccountFailure::from_error(
            anyhow::Error::new(auth::AuthenticationCancelled).context("获取账号"),
            "未完成",
        );
        assert!(cancelled.cancelled);
        let failure =
            AccountFailure::from_error(anyhow::anyhow!("登录已取消：来自远端的无效文字"), "未完成");
        assert!(!failure.cancelled);
        assert!(failure.message.contains("来自远端"));
    }
    #[test]
    fn custom_modal_escape_never_selects_upload_or_cape_mutation() {
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(850.0, 600.0),
            )),
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut action = None;
        let _ = ctx.run(raw.clone(), |ctx| {
            action = account_modal(
                ctx,
                "skin-test",
                "选择皮肤模型",
                "选择后上传",
                &["经典 · Steve", "纤细 · Alex", "取消"],
            );
        });
        assert_eq!(action, Some(2));
        let ctx = egui::Context::default();
        let mut cape = None;
        let _ = ctx.run(raw, |ctx| {
            cape = cape_modal(ctx, &[], &mut CapeDraft::default());
        });
        assert!(matches!(cape, Some(CapeAction::Close)));
    }
    #[test]
    fn upstream_account_vectors_parse_with_nonempty_bounds() {
        for svg in ACCOUNT_ICONS {
            let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).unwrap();
            let bounds = tree.root().abs_bounding_box();
            assert!(bounds.width() > 0.0 && bounds.height() > 0.0);
        }
    }
    #[test]
    fn stages_close_code_only_for_current_active_login_and_do_not_finish_session() {
        let mut state = AccountUiState {
            request: 2,
            auth_pending: true,
            device: Some(DevicePrompt {
                code: "TEST".into(),
                url: "https://microsoft.com/link".into(),
                expires: Instant::now() + Duration::from_secs(60),
                opened: false,
            }),
            ..Default::default()
        };
        assert!(!state.accept_stage(1, auth::LoginStage::Xbox));
        assert!(state.device.is_some());
        assert!(state.accept_stage(2, auth::LoginStage::Microsoft));
        assert!(state.device.is_some());
        assert!(state.accept_stage(2, auth::LoginStage::Xbox));
        assert!(state.device.is_none());
        assert!(state.accept_stage(2, auth::LoginStage::Complete));
        assert!(
            state.auth_pending,
            "Ready must confirm persistence before ending the login"
        );
        state.auth_pending = false;
        assert!(!state.accept_stage(2, auth::LoginStage::Microsoft));
        assert_eq!(state.stage, Some(auth::LoginStage::Complete));
    }
    #[test]
    fn classified_auth_errors_offer_specific_actions_without_dropping_cancel() {
        use auth::AuthenticationIssue as Issue;
        for (issue, first) in [
            (Issue::OwnershipRequired, "购买 Minecraft"),
            (Issue::MinecraftProfileRequired, "创建档案"),
            (Issue::ClientConfiguration, "应用设置"),
            (Issue::XboxProfileRequired, "注册"),
            (Issue::PasswordLoginRequired, "重新登录"),
        ] {
            let failure =
                AccountFailure::from_error(anyhow::Error::new(issue).context("登录"), "失败");
            assert_eq!(failure.issue, Some(issue));
            assert!(!failure.cancelled);
            let actions = recovery_actions(failure.issue, true);
            assert_eq!(actions[0].0, first);
            assert_eq!(actions.last().unwrap().1, RecoveryAction::Close);
        }
        assert_eq!(
            recovery_actions(Some(Issue::RateLimited), true),
            vec![("我知道了", RecoveryAction::Close)]
        );
    }
    #[test]
    fn idle_refresh_uses_real_expiry_with_one_minute_lead() {
        assert_eq!(refresh_delay(4600, 1000), Duration::from_secs(3540));
        assert_eq!(refresh_delay(1030, 1000), Duration::from_secs(1));
        assert_eq!(refresh_delay(900, 1000), Duration::from_secs(1));
    }
}
