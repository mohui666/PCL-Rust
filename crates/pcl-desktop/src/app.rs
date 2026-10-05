use crate::theme;
use crate::ui_style::{self, Assets};
mod account_ui;
mod appearance_ui;
mod crash_ui;
mod download_ui;
mod folder_ui;
mod game_window_ui;
mod hint_ui;
mod home_ui;
mod install_ui;
mod instance_setup_ui;
mod java_ui;
mod job;
pub(crate) mod launch_ui;
mod loading_ui;
mod mod_update_ui;
mod modal_ui;
mod more_ui;
mod offline_skin_ui;
mod pack_export_ui;
mod page_motion;
mod resource_ui;
mod settings_reset;
mod setup_launch_ui;
mod setup_system_ui;
mod shell_ui;
mod task_hub;
mod task_ui;
mod version_ui;
mod window_state;
mod xaml_ui;
use eframe::egui::{self, Color32, RichText, Vec2};
use install_ui::InstallKind;
use pcl_core::{
    auth,
    config::{self, Settings},
    install,
    java::{self, JavaRuntime},
    java_selection::{self, JavaSelectionResult},
    launch::{self, LaunchOptions},
    launch_script::{self, ScriptExport, ScriptFormat},
    loaders::LoaderVersion,
    metadata,
    model::{InstalledVersion, Platform, Progress, Session},
    modpack, mods,
};
use serde_json::Value;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    time::Duration,
};

const MUTED: Color32 = Color32::from_rgb(140, 140, 140);

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Launch,
    Download,
    Settings,
    More,
}

enum LaunchAction {
    Run,
    Preview,
    Export { path: PathBuf, format: ScriptFormat },
}

pub(crate) enum Event {
    Job(job::JobMessage),
    JobStarted,
    TaskPlan(Vec<task_ui::TaskComponentSpec>),
    TaskPart(usize),
    TaskPartDone(usize),
    Resource(resource_ui::ResourceEvent),
    Account(account_ui::AccountEvent),
    Runtime(java_ui::RuntimeEvent),
    ScriptExported(ScriptExport),
    Launch(launch_ui::LaunchEvent),
    DeletePreview(Result<pcl_core::deletion::VersionDeletePreview, String>),
    VersionDeleted {
        root: PathBuf,
        id: String,
        result: Result<pcl_core::deletion::DeleteReport, String>,
    },
    VersionList(download_ui::VersionListEvent),
    OptiFineList(install_ui::OptiFineListEvent),
    Java(u64, Result<Vec<JavaRuntime>, String>),
    Versions(PathBuf, u64, Result<Vec<InstalledVersion>, String>),
    Mods(PathBuf, String, u64, Result<Vec<mods::LocalMod>, String>),
    Progress(Progress),
    Log(String),
    Installed(String),
    ModsChanged {
        target: String,
        message: String,
    },
    ModsUpdated {
        target: String,
        report: pcl_core::mod_updates::UpdateReport,
    },
    VersionRenamed {
        root: PathBuf,
        old: String,
        new: String,
        result: Result<(), String>,
    },
    GameContext {
        pid: u32,
        game_dir: PathBuf,
        started: std::time::SystemTime,
        secret: String,
    },
    GameLog {
        pid: u32,
        line: String,
    },
    GameStarted(u32),
    GameReady {
        pid: u32,
        visibility: config::LauncherVisibility,
    },
    GameFinished {
        pid: u32,
        success: bool,
        stopped: bool,
        message: String,
    },
    GameStopFailed {
        pid: u32,
        message: String,
    },
    LaunchWarning(String),
    Done(String),
    DownloadFailed {
        message: String,
        cancelled: bool,
    },
    // Retain legacy unscoped error routing/rejection; current launch and
    // process workers send request- or PID-scoped failures instead.
    #[allow(dead_code)]
    Error(String),
}

impl Event {
    fn is_download_terminal(&self) -> bool {
        matches!(
            self,
            Self::Installed(_)
                | Self::ModsUpdated { .. }
                | Self::ModsChanged { .. }
                | Self::DownloadFailed { .. }
                | Self::Error(_)
                | Self::Done(_)
                | Self::Resource(resource_ui::ResourceEvent::Installed(..))
                | Self::Runtime(java_ui::RuntimeEvent::Installed(_))
        )
    }
    fn download_failed(prefix: &str, error: anyhow::Error) -> Self {
        let cancelled = error
            .chain()
            .any(|cause| cause.is::<pcl_core::model::OperationCancelled>());
        Self::DownloadFailed {
            message: format!("{prefix}：{error:#}"),
            cancelled,
        }
    }
    fn launch_failed(
        request: u64,
        prefix: &str,
        cancelled_message: &str,
        error: anyhow::Error,
    ) -> Self {
        let cancelled = error
            .chain()
            .any(|cause| cause.is::<pcl_core::model::OperationCancelled>());
        Self::Launch(launch_ui::LaunchEvent::Failed {
            request,
            cancelled,
            message: if cancelled {
                cancelled_message.into()
            } else {
                format!("{prefix}：{error:#}")
            },
        })
    }
}

pub struct Launcher {
    assets: Assets,
    folder_ui: folder_ui::FolderUi,
    appearance: appearance_ui::AppearanceState,
    home: home_ui::HomeState,
    crash: crash_ui::CrashUiState,
    more: more_ui::MoreState,
    accounts: account_ui::AccountUiState,
    java_download: java_ui::JavaDownloadState,
    instance_setup: instance_setup_ui::InstanceSetupState,
    setup_launch: setup_launch_ui::SetupLaunchState,
    setup_system: setup_system_ui::SetupSystemState,
    mod_update: mod_update_ui::ModUpdateState,
    offline_skin: offline_skin_ui::OfflineSkinState,
    pending_version_delete: Option<pcl_core::deletion::VersionDeletePreview>,
    script_export_result: Option<ScriptExport>,
    pack_export: pack_export_ui::PackExportState,
    noticed_status: String,
    hints: hint_ui::HintQueue,
    settings: Settings,
    settings_path: PathBuf,
    root_text: String,
    java_text: String,
    page: Page,
    version_view: bool,
    microsoft: bool,
    session: Option<Session>,
    versions: Vec<InstalledVersion>,
    versions_request: u64,
    mods_request: u64,
    manifest: Vec<Value>,
    version_lists: download_ui::VersionLists,
    optifine: install_ui::OptiFineState,
    java: Vec<JavaRuntime>,
    java_request: u64,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    busy: Option<String>,
    jobs: job::Jobs,
    progress: Option<Progress>,
    task: Option<task_ui::TaskState>,
    task_hub: task_hub::TaskHub,
    processing_job: bool,
    task_view: bool,
    cancel: Arc<AtomicBool>,
    game_stop: Arc<AtomicBool>,
    game_pid: Option<u32>,
    launch_ui: launch_ui::LaunchState,
    game_window: game_window_ui::GameWindowState,
    window_opacity: crate::native_window::WindowOpacity,
    last_viewport_size: Option<(u32, u32)>,
    window_state: window_state::WindowState,
    pending_settings_reset: Option<config::LauncherResetScope>,
    status: String,
    error: Option<String>,
    logs: VecDeque<String>,
    show_logs: bool,
    version_tools: bool,
    tools_tab: usize,
    settings_tab: usize,
    install_name: String,
    install_name_edited: bool,
    local_mods: Vec<mods::LocalMod>,
    mods_filter: String,
    download_selection: Option<String>,
    loader_kind: Option<InstallKind>,
    loader_expanded: Option<InstallKind>,
    loader_versions: Vec<LoaderVersion>,
    loader_version: Option<String>,
    download_tab: usize,
    pack_info: Option<(PathBuf, modpack::ModpackInfo)>,
    pack_id: String,
    pack_optional: bool,
    resource_browser: resource_ui::ResourceBrowser,
}

impl Launcher {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = &cc.egui_ctx;
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "PCL English".into(),
            egui::FontData::from_static(include_bytes!("../assets/upstream/Resources/Font.ttf"))
                .into(),
        );
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(0, "PCL English".into());
        let mut paths = Vec::new();
        if let Some(windows) = std::env::var_os("WINDIR") {
            paths.push(PathBuf::from(windows).join("Fonts/msyh.ttc"));
        }
        if let Ok(executable) = std::env::current_exe() {
            if let Some(contents) = executable.parent().and_then(|p| p.parent()) {
                paths.push(contents.join("Resources/PingFang-Regular.otf"));
            }
        }
        #[cfg(debug_assertions)]
        paths.push(PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test-output/fonts/PingFang-Regular.otf"
        )));
        // Apple's current hvgl PingFang container cannot be outlined by ab_glyph.
        // The packaging script exports a local subset via CoreText; never load hvgl as a font.
        let mut cjk_index = 1;
        paths.push(PathBuf::from("/System/Library/Fonts/Hiragino Sans GB.ttc"));
        for (i, path) in paths.iter().enumerate() {
            if let Ok(data) = std::fs::read(path) {
                let family = format!("CJK-{i}");
                let mut face_index = 0;
                if path.file_name().is_some_and(|name| name == "msyh.ttc") {
                    for index in 0..ttf_parser::fonts_in_collection(&data).unwrap_or(1).min(16) {
                        if ttf_parser::Face::parse(&data, index)
                            .ok()
                            .is_some_and(|face| {
                                face.names().into_iter().any(|name| {
                                    name.to_string()
                                        .is_some_and(|name| name == "Microsoft YaHei UI")
                                })
                            })
                        {
                            face_index = index;
                            break;
                        }
                    }
                }
                let mut font_data = egui::FontData::from_owned(data);
                font_data.index = face_index;
                fonts.font_data.insert(family.clone(), font_data.into());
                fonts
                    .families
                    .get_mut(&egui::FontFamily::Proportional)
                    .unwrap()
                    .insert(cjk_index, family.clone());
                fonts
                    .families
                    .get_mut(&egui::FontFamily::Monospace)
                    .unwrap()
                    .push(family);
                cjk_index += 1;
            }
        }
        let mut bold_family = fonts.families[&egui::FontFamily::Proportional].clone();
        let mut bold_paths = Vec::new();
        if let Some(windows) = std::env::var_os("WINDIR") {
            bold_paths.push(PathBuf::from(windows).join("Fonts/msyhbd.ttc"));
        }
        if let Ok(executable) = std::env::current_exe() {
            if let Some(contents) = executable.parent().and_then(|p| p.parent()) {
                bold_paths.push(contents.join("Resources/PingFang-Semibold.otf"));
            }
        }
        #[cfg(debug_assertions)]
        bold_paths.push(PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test-output/fonts/PingFang-Semibold.otf"
        )));
        for (index, path) in bold_paths.iter().enumerate() {
            if let Ok(data) = std::fs::read(path) {
                let mut font = egui::FontData::from_owned(data);
                for face_index in 0..ttf_parser::fonts_in_collection(&font.font)
                    .unwrap_or(1)
                    .min(16)
                {
                    if ttf_parser::Face::parse(&font.font, face_index)
                        .ok()
                        .is_some_and(|face| {
                            face.names().into_iter().any(|name| {
                                name.to_string()
                                    .is_some_and(|name| name.contains("Microsoft YaHei UI"))
                            })
                        })
                    {
                        font.index = face_index;
                        break;
                    }
                }
                let name = format!("PCL-Bold-{index}");
                fonts.font_data.insert(name.clone(), font.into());
                bold_family.insert(0, name);
            }
        }
        fonts
            .families
            .insert(egui::FontFamily::Name("PCL Bold".into()), bold_family);
        ctx.set_fonts(fonts);
        let mut style = (*ctx.style()).clone();
        style.visuals = egui::Visuals::light();
        style.visuals.override_text_color = Some(theme::palette(ctx).text);
        style.visuals.selection.bg_fill = theme::palette(ctx).accent;
        style.visuals.selection.stroke = egui::Stroke::new(1.0_f32, Color32::WHITE);
        style.visuals.widgets.inactive.bg_fill = Color32::WHITE;
        style.visuals.widgets.inactive.weak_bg_fill = Color32::WHITE;
        style.visuals.widgets.inactive.bg_stroke =
            egui::Stroke::new(1.0_f32, Color32::from_rgb(210, 220, 229));
        style.visuals.widgets.hovered.weak_bg_fill = theme::palette(ctx).light;
        style.visuals.widgets.hovered.bg_stroke =
            egui::Stroke::new(1.0_f32, theme::palette(ctx).accent);
        style.visuals.widgets.active.weak_bg_fill = theme::palette(ctx).light;
        style.spacing.item_spacing = Vec2::new(10.0, 8.0);
        style.spacing.interact_size.y = 28.0;
        style.visuals.extreme_bg_color = Color32::from_white_alpha(100);
        style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(3);
        style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(3);
        style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(3);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
        ctx.set_style(style);
        let path = config::settings_path();
        let (settings, initial_error) = match config::load_settings(&path) {
            Ok(s) => (s, None),
            Err(e) => (
                Settings::default(),
                Some(format!("配置读取失败，原文件已保留：{e:#}")),
            ),
        };
        let (tx, rx) = mpsc::channel();
        let mut app = Self {
            assets: Assets::new(ctx),
            folder_ui: Default::default(),
            appearance: appearance_ui::AppearanceState::default(),
            home: home_ui::HomeState::default(),
            crash: crash_ui::CrashUiState::default(),
            more: more_ui::MoreState::default(),
            accounts: account_ui::AccountUiState::default(),
            java_download: java_ui::JavaDownloadState::default(),
            instance_setup: instance_setup_ui::InstanceSetupState::default(),
            setup_launch: setup_launch_ui::SetupLaunchState::default(),
            setup_system: setup_system_ui::SetupSystemState::default(),
            mod_update: mod_update_ui::ModUpdateState::default(),
            offline_skin: offline_skin_ui::OfflineSkinState::default(),
            pending_version_delete: None,
            script_export_result: None,
            pack_export: Default::default(),
            noticed_status: "准备就绪".into(),
            hints: hint_ui::HintQueue::default(),
            root_text: settings.game_root.display().to_string(),
            java_text: settings
                .java_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            settings,
            settings_path: path,
            page: Page::Launch,
            version_view: false,
            microsoft: false,
            session: None,
            versions: vec![],
            versions_request: 0,
            mods_request: 0,
            manifest: vec![],
            version_lists: Default::default(),
            optifine: Default::default(),
            java: vec![],
            java_request: 0,
            tx,
            rx,
            busy: None,
            jobs: job::Jobs::default(),
            progress: None,
            task: None,
            task_hub: Default::default(),
            processing_job: false,
            task_view: false,
            cancel: Arc::new(AtomicBool::new(false)),
            game_stop: Arc::new(AtomicBool::new(false)),
            game_pid: None,
            launch_ui: Default::default(),
            game_window: Default::default(),
            window_opacity: Default::default(),
            last_viewport_size: None,
            window_state: Default::default(),
            pending_settings_reset: None,
            status: "准备就绪".into(),
            error: initial_error,
            logs: VecDeque::new(),
            show_logs: false,
            version_tools: false,
            tools_tab: 0,
            settings_tab: 0,
            install_name: String::new(),
            install_name_edited: false,
            local_mods: vec![],
            mods_filter: String::new(),
            download_selection: None,
            loader_kind: None,
            loader_expanded: None,
            loader_versions: vec![],
            loader_version: None,
            download_tab: 0,
            pack_info: None,
            pack_id: String::new(),
            pack_optional: false,
            resource_browser: resource_ui::ResourceBrowser::default(),
        };
        app.load_task_history();
        app.refresh_versions();
        app.detect_java();
        app.init_accounts();
        app.init_system_settings();
        app
    }

    fn record(&mut self, message: String) {
        self.logs.push_back(message);
        while self.logs.len()
            > if self.settings.system.debug_mode {
                2000
            } else {
                400
            }
        {
            self.logs.pop_front();
        }
    }
    fn persist(&mut self) {
        pcl_core::system::configure_debug(&self.settings.system);
        if let Err(e) = config::save_settings(&self.settings_path, &self.settings) {
            self.error = Some(format!("保存失败：{e:#}"));
        } else if let Err(e) = pcl_core::network::configure(&self.settings.downloads) {
            self.error = Some(format!("应用下载设置失败：{e:#}"));
        }
    }
    fn refresh_versions(&mut self) {
        let tx = self.tx.clone();
        let root = self.settings.game_root.clone();
        self.versions_request = self.versions_request.wrapping_add(1);
        let request = self.versions_request;
        std::thread::spawn(move || {
            let result =
                metadata::list_installed(&root).map_err(|error| format!("读取版本：{error:#}"));
            let _ = tx.send(Event::Versions(root, request, result));
        });
    }
    fn detect_java(&mut self) {
        self.java_request = self.java_request.wrapping_add(1);
        self.java_download.set_detecting(true);
        let request = self.java_request;
        let tx = self.tx.clone();
        let settings = self.settings.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<Vec<JavaRuntime>> {
                let cancel = AtomicBool::new(false);
                let report = java::discover_java_with_cancel(&cancel)?;
                let mut runtimes = report.runtimes;
                for message in report.diagnostics {
                    let _ = tx.send(Event::Log(message));
                }
                for path in java_ui::priority_paths(&settings) {
                    if settings
                        .java_excluded
                        .iter()
                        .any(|p| java_ui::same_java_path(p, &path))
                    {
                        continue;
                    }
                    if !runtimes
                        .iter()
                        .any(|runtime| java_ui::same_java_path(&runtime.path, &path))
                    {
                        match java::inspect_java_with_cancel(&path, &cancel) {
                            Ok(runtime) => runtimes.push(runtime),
                            Err(error) => {
                                let _ = tx
                                    .send(Event::Log(format!("已保存 Java 检测未通过：{error:#}")));
                            }
                        }
                    }
                }
                Ok(runtimes)
            })()
            .map_err(|error| format!("Java 搜索未完成：{error:#}"));
            let _ = tx.send(Event::Java(request, result));
        });
    }
    pub(super) fn is_current_game_root(&self, root: &std::path::Path) -> bool {
        self.settings
            .game_root
            .canonicalize()
            .ok()
            .zip(root.canonicalize().ok())
            .is_some_and(|(current, candidate)| current == candidate)
    }
    fn instance_dir(&self) -> anyhow::Result<PathBuf> {
        let id = self
            .settings
            .selected_version
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("请先选择一个游戏版本"))?;
        metadata::validate_id(id)?;
        config::instance_game_dir(&self.settings.game_root, id)
    }
    fn refresh_mods(&mut self) {
        self.local_mods.clear();
        self.mods_request = self.mods_request.wrapping_add(1);
        let request = self.mods_request;
        let root = self.settings.game_root.clone();
        let instance = match self.instance_dir() {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(format!("Mod 目录读取失败：{error:#}"));
                return;
            }
        };
        let tx = self.tx.clone();
        let id = self.settings.selected_version.clone().unwrap_or_default();
        std::thread::spawn(move || {
            let result =
                mods::list_mods(&instance).map_err(|error| format!("Mod 列表读取失败：{error:#}"));
            let _ = tx.send(Event::Mods(root, id, request, result));
        });
    }
    fn invalidate_root_views(&mut self) {
        self.versions_request = self.versions_request.wrapping_add(1);
        self.mods_request = self.mods_request.wrapping_add(1);
        self.versions.clear();
        self.local_mods.clear();
        self.resource_browser = resource_ui::ResourceBrowser::default();
    }
    fn start_job(&mut self, label: &str) -> Option<(Sender<Event>, Arc<AtomicBool>)> {
        if self.busy.is_some() || self.jobs.conflicts_with(&self.settings.game_root) {
            return None;
        }
        self.cancel = Arc::new(AtomicBool::new(false));
        self.busy = Some(label.into());
        self.progress = None;
        self.error = None;
        self.status = label.into();
        Some((self.tx.clone(), self.cancel.clone()))
    }
    fn start_download_job(
        &mut self,
        label: &str,
        target: Option<String>,
    ) -> Option<(job::JobSender, Arc<AtomicBool>)> {
        self.start_download_job_at(label, self.settings.game_root.clone(), target)
    }
    fn start_download_job_at(
        &mut self,
        label: &str,
        root: PathBuf,
        target: Option<String>,
    ) -> Option<(job::JobSender, Arc<AtomicBool>)> {
        let context = job::JobContext {
            root,
            game_root: self.settings.game_root.clone(),
            target,
        };
        self.start_download_job_with_context(label, context)
    }
    fn start_download_job_with_context(
        &mut self,
        label: &str,
        context: job::JobContext,
    ) -> Option<(job::JobSender, Arc<AtomicBool>)> {
        if self.busy.is_some() {
            self.error = Some("请等待当前操作完成后再添加下载任务".into());
            return None;
        }
        if self.game_pid.is_some() {
            self.error = Some("请先关闭正在运行的游戏，再执行文件任务".into());
            return None;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let sender = match self
            .jobs
            .begin(context.clone(), self.tx.clone(), cancel.clone())
        {
            Ok(sender) => sender,
            Err(error) => {
                self.error = Some(error.to_string());
                return None;
            }
        };
        self.register_task(sender.id, context);
        self.cancel = cancel.clone();
        self.error = None;
        self.progress = None;
        self.status = label.into();
        let selected_install = self.page == Page::Download
            && self.download_tab == 0
            && self.download_selection.is_some()
            && label.starts_with("正在安装 ");
        let mut task = task_ui::TaskState::new(task_ui::download_task_title(
            label,
            selected_install.then_some(self.install_name.as_str()),
        ));
        task.group_vanilla_install(selected_install && self.loader_kind.is_none());
        task.queued();
        self.task = Some(task);
        self.record_current_task();
        self.task_view = true;
        Some((sender, cancel))
    }
    fn finish_download_task(&mut self) {
        if let Some(task) = self.task.as_mut().filter(|task| task.is_running()) {
            task.finish();
            self.task_view = false;
        }
    }
    fn install(&mut self, id: String) {
        let Some((tx, _cancel)) =
            self.start_download_job(&format!("正在补全 {id}"), Some(id.clone()))
        else {
            return;
        };
        if let Some(task) = self.task.as_mut() {
            task.set_overall_plan_known(false);
        }
        let root = self.settings.game_root.clone();
        let java = self
            .settings
            .java_path
            .clone()
            .or_else(|| self.settings.java_priority.first().cloned());
        tx.spawn(move |tx| {
            let cancel = tx.cancel_token();
            let result = install::repair_version_with_java(
                &root,
                &id,
                java.as_deref(),
                &Platform::current(),
                &cancel,
                |p| {
                    let _ = tx.send(Event::Progress(p));
                },
            );
            let event = match result {
                Ok(()) => Event::Installed(id),
                Err(e) => Event::download_failed("文件补全未完成", e),
            };
            let _ = tx.send(event);
        });
    }
    fn launch(&mut self, preview: bool) {
        if !preview && self.game_pid.is_some() {
            self.launch_ui.manage = true;
            return;
        }
        if !self.account_store_ready() {
            return;
        }
        if let Some(id) = &self.settings.selected_version {
            match config::load_instance_settings(&self.settings.game_root, id) {
                Ok(settings) => match settings.login_requirement {
                    config::LoginRequirement::Microsoft if !self.microsoft => {
                        self.error = Some("此实例要求正版登录，请选择正版账号后启动。".into());
                        return;
                    }
                    config::LoginRequirement::Offline if self.microsoft => {
                        self.error = Some("此实例要求离线身份，请先手动切换登录方式。".into());
                        return;
                    }
                    _ => (),
                },
                Err(error) => {
                    self.error = Some(format!("读取实例设置失败：{error:#}"));
                    return;
                }
            }
        }
        if self.microsoft {
            let was_idle = self.busy.is_none();
            self.account_launch(preview);
            if was_idle && self.busy.is_some() {
                let action = if preview {
                    LaunchAction::Preview
                } else {
                    LaunchAction::Run
                };
                self.begin_launch_panel(
                    &action,
                    self.settings.selected_version.clone().unwrap_or_default(),
                    self.cancel.clone(),
                    true,
                );
            }
        } else {
            self.launch_with_current_session(preview);
        }
    }
    fn launch_with_current_session(&mut self, preview: bool) {
        self.start_launch(if preview {
            LaunchAction::Preview
        } else {
            LaunchAction::Run
        });
    }
    fn launch_home_server(&mut self, server: String) {
        if self.microsoft && self.session.is_none() {
            self.error = Some("请先完成正版登录再使用主页进服入口".into());
            return;
        }
        self.start_launch_with_server(LaunchAction::Run, Some(server));
    }
    fn start_launch(&mut self, action: LaunchAction) {
        self.start_launch_with_server(action, None);
    }
    fn start_launch_with_server(&mut self, action: LaunchAction, home_server: Option<String>) {
        if self.jobs.conflicts_with(&self.settings.game_root) {
            self.error = Some("游戏目录仍有写入任务，请等待完成后启动".into());
            return;
        }
        if self.game_pid.is_some() && matches!(action, LaunchAction::Run) {
            self.launch_ui.manage = true;
            return;
        }
        let Some(version) = self.settings.selected_version.clone() else {
            self.version_view = true;
            return;
        };
        let session = if self.microsoft {
            match &self.session {
                Some(s) => s.clone(),
                None => {
                    self.error = Some(if matches!(action, LaunchAction::Export { .. }) {
                        "当前没有已载入的正版会话，无法导出该身份的启动参数。请先通过启动器登录；导出本身不会刷新凭据。".into()
                    } else {
                        "请先完成微软登录。".into()
                    });
                    return;
                }
            }
        } else {
            match auth::offline_session(&self.settings.offline_name) {
                Ok(s) => s,
                Err(e) => {
                    self.error = Some(e.to_string());
                    return;
                }
            }
        };
        let instance = match config::load_instance_settings(&self.settings.game_root, &version) {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(format!("读取实例设置失败：{error:#}"));
                return;
            }
        };
        let request = java_ui::selection_request(&self.settings, &instance, &version);
        let candidates = self.java.clone();
        if !self.microsoft && matches!(action, LaunchAction::Run) {
            let name = self.settings.offline_name.trim().to_owned();
            self.settings
                .offline_history
                .retain(|previous| previous != &name);
            self.settings.offline_history.insert(0, name);
            self.settings.offline_history.truncate(20);
            self.persist();
        }
        let Some((tx, cancel)) = self.start_job("正在检查启动环境") else {
            return;
        };
        let launch_request = self.begin_launch_panel(&action, version, cancel.clone(), false);
        if matches!(action, LaunchAction::Run) {
            self.game_stop = Arc::new(AtomicBool::new(false));
        }
        let stop = self.game_stop.clone();
        let settings = self.settings.clone();
        let launcher_size = self.last_viewport_size;
        let failure_prefix = if matches!(action, LaunchAction::Export { .. }) {
            "启动脚本导出未完成"
        } else {
            "启动失败"
        };
        let cancelled_message = if matches!(action, LaunchAction::Export { .. }) {
            "启动脚本导出已取消"
        } else {
            "启动已取消"
        };
        std::thread::spawn(move || {
            use launch_ui::{LaunchEvent, Stage};
            let stage = |stage| {
                let _ = tx.send(Event::Launch(LaunchEvent::Stage {
                    request: launch_request,
                    stage,
                }));
            };
            let result = (|| -> anyhow::Result<()> {
                if cancel.load(Ordering::Relaxed) {
                    return Err(pcl_core::model::OperationCancelled.into());
                }
                let current = Platform::current();
                stage(Stage::Java);
                let runtime = match java_selection::select_java(
                    &request,
                    &candidates,
                    &current,
                    &cancel,
                )? {
                    JavaSelectionResult::Selected {
                        runtime,
                        source,
                        requirement,
                        warnings,
                    } => {
                        for message in warnings {
                            let _ = tx.send(Event::Log(message));
                        }
                        let detail = requirement
                            .map(|value| format!("；范围 {}", value.range))
                            .unwrap_or_default();
                        let _ = tx.send(Event::Log(format!(
                            "Java 选择：{}（{}，{source:?}）{}\n{}",
                            runtime.version,
                            runtime.architecture,
                            detail,
                            runtime.path.display()
                        )));
                        runtime
                    }
                    JavaSelectionResult::NeedsDownload {
                        requirement,
                        diagnostics,
                    } => {
                        for message in &diagnostics {
                            let _ = tx.send(Event::Log(message.clone()));
                        }
                        let message = format!("未找到满足此版本要求的 Java。\n版本范围：{}\n{}\n\n可以下载当前平台的官方 Java，或在设置中导入、调整优先级及排除名单。具体候选诊断已写入日志。", requirement.range, requirement.reasons.join("\n"));
                        let _ = tx.send(Event::Launch(LaunchEvent::Unavailable {
                            request: launch_request,
                        }));
                        let _ = tx.send(Event::Runtime(
                            java_ui::RuntimeEvent::SelectionUnavailable(message, requirement),
                        ));
                        return Ok(());
                    }
                };
                if cancel.load(Ordering::Relaxed) {
                    return Err(pcl_core::model::OperationCancelled.into());
                }
                let options = LaunchOptions {
                    root: request.root.clone(),
                    version_id: request.version_id.clone(),
                    java: runtime.path.clone(),
                    memory_mb: settings.memory_mb,
                    width: 1100,
                    height: 700,
                };
                let prepared_skin = if matches!(action, LaunchAction::Run) {
                    stage(Stage::Skin);
                    let prepared = pcl_core::offline_skin::prepare(
                        &options.root,
                        &options.version_id,
                        &settings,
                        &session,
                        &cancel,
                    )?;
                    for warning in &prepared.warnings {
                        let _ = tx.send(Event::Log(warning.clone()));
                    }
                    Some(prepared)
                } else {
                    None
                };
                stage(Stage::Arguments);
                let mut plan = launch::build_plan_with_overrides(
                    &options,
                    prepared_skin
                        .as_ref()
                        .map_or(&session, |prepared| &prepared.session),
                    &current,
                    &settings,
                    runtime.major,
                    launch::LaunchOverrides {
                        launcher_size,
                        server: home_server.as_deref(),
                    },
                )?;
                plan.behavior.offline_skin = prepared_skin.map(|prepared| prepared.update);
                if cancel.load(Ordering::Relaxed) {
                    return Err(pcl_core::model::OperationCancelled.into());
                }
                let _ = tx.send(Event::Log(plan.redacted_command()));
                match action {
                    LaunchAction::Preview => {
                        let _ = tx.send(Event::Launch(LaunchEvent::Finished {
                            request: launch_request,
                            message: "启动检查通过，脱敏命令已写入日志".into(),
                        }));
                    }
                    LaunchAction::Run => crate::process::run_game_with_launch_progress(
                        plan,
                        session.access_token,
                        tx.clone(),
                        stop,
                        cancel.clone(),
                        launch_request,
                    )?,
                    LaunchAction::Export { path, format } => {
                        stage(Stage::Export);
                        let exported = launch_script::export_launch_script_with_cancel(
                            &path, &plan, format, &cancel,
                        )?;
                        let _ = tx.send(Event::Launch(LaunchEvent::Finished {
                            request: launch_request,
                            message: "启动脚本已导出".into(),
                        }));
                        let _ = tx.send(Event::ScriptExported(exported));
                    }
                }
                Ok(())
            })();
            if let Err(e) = result {
                let _ = tx.send(Event::launch_failed(
                    launch_request,
                    failure_prefix,
                    cancelled_message,
                    e,
                ));
            }
        });
    }
    pub(super) fn export_launch_script(&mut self) {
        if self.busy.is_some() {
            return;
        }
        let Some(id) = self.settings.selected_version.as_ref() else {
            self.version_view = true;
            return;
        };
        let format = if cfg!(windows) {
            ScriptFormat::WindowsBatch
        } else {
            ScriptFormat::MacCommand
        };
        let Some(mut path) = rfd::FileDialog::new()
            .set_title("导出启动脚本")
            .add_filter("启动脚本", &[format.extension()])
            .set_file_name(format!("{id}.{}", format.extension()))
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension(format.extension());
        }
        // Export uses the already held session only. It never calls account_launch/refresh.
        self.start_launch(LaunchAction::Export { path, format });
    }
    fn script_export_dialog(&mut self, ctx: &egui::Context) {
        let Some(result) = self.script_export_result.as_ref() else {
            return;
        };
        let caption = format!(
            "启动脚本已保存：\n{}\n\n{}\n导出未启动 Minecraft。",
            result.path.display(),
            if result.runnable_without_credentials {
                "脚本包含离线启动参数；Java 和游戏文件仍需保留在原位置。"
            } else {
                "正版凭据已全部替换为占位符。此脚本只能用于诊断，不能直接完成正版登录；请使用启动器登录并启动。"
            }
        );
        if let Some(action) = account_ui::account_modal(
            ctx,
            "launch-script-exported",
            "启动脚本已导出",
            &caption,
            &["打开文件夹", "完成"],
        ) {
            let result = self.script_export_result.take().unwrap();
            if action == 0 {
                if let Some(parent) = result.path.parent() {
                    if let Err(error) = crate::process::open_folder(parent) {
                        self.error = Some(format!("打开脚本目录失败：{error}"));
                    }
                }
            }
        }
    }
    fn receive(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Job(message) => self.handle_job_message(message),
                event => self.handle_unscoped_event(event),
            }
        }
    }
    fn handle_unscoped_event(&mut self, event: Event) {
        // These outcomes can only originate from an ID-bearing download sender.
        // In particular, a late unscoped terminal must never finish the new card.
        if matches!(
            &event,
            Event::JobStarted
                | Event::TaskPlan(_)
                | Event::TaskPart(_)
                | Event::TaskPartDone(_)
                | Event::Installed(_)
                | Event::ModsUpdated { .. }
                | Event::ModsChanged { .. }
                | Event::DownloadFailed { .. }
                | Event::Resource(resource_ui::ResourceEvent::Installed(..))
                | Event::Runtime(java_ui::RuntimeEvent::Installed(_))
        ) {
            self.record("忽略没有作业编号的下载终态。".into());
            return;
        }
        // Generic launch progress is meaningful only while its own operation is active.
        // Stale unscoped events must not overwrite a queued download card.
        if self.busy.is_none()
            && self.jobs.is_active()
            && matches!(
                &event,
                Event::Progress(_) | Event::Done(_) | Event::Error(_)
            )
        {
            if let Event::Error(message) = event {
                self.record(message);
            }
            return;
        }
        self.handle_event(event);
    }
    fn handle_job_message(&mut self, message: job::JobMessage) {
        if self.jobs.get(message.id).is_none() {
            return;
        }
        let id = message.id;
        let terminal = message.event.is_download_terminal();
        let changed_plan = matches!(
            *message.event,
            Event::JobStarted | Event::TaskPlan(_) | Event::TaskPart(_) | Event::TaskPartDone(_)
        );
        let previous = self.task_hub.selected;
        let saved = (
            self.busy.clone(),
            self.progress.clone(),
            self.status.clone(),
            self.task_view,
            self.cancel.clone(),
        );
        self.select_task(id);
        self.processing_job = true;
        self.handle_job_message_inner(message);
        self.processing_job = false;
        if terminal || changed_plan {
            self.record_current_task();
        }
        let unrelated_operation = saved.0.is_some();
        self.busy = saved.0;
        if previous != Some(id) {
            if let Some(previous) = previous {
                self.select_task(previous);
            }
        }
        if previous != Some(id) || unrelated_operation {
            self.progress = saved.1;
            self.status = saved.2;
            self.task_view = saved.3;
            self.cancel = saved.4;
        }
    }
    fn handle_job_message_inner(&mut self, message: job::JobMessage) {
        let terminal = message.event.is_download_terminal();
        let Some(active) = self.jobs.route(message.id, terminal) else {
            return;
        };
        let current_root = active.context.applies_to(&self.settings.game_root);
        if terminal {
            self.busy = None;
            self.progress = None;
            if active.cancel_requested() {
                self.record(format!(
                    "{}：已请求取消，以下保留后台实际结果。",
                    active.label()
                ));
            }
        }
        match *message.event {
            Event::JobStarted => {
                if let Some(task) = self.task.as_mut() {
                    task.started();
                }
                if self.settings.system.debug_mode {
                    self.record(format!("{}：取得目录写入许可", active.label()));
                }
            }
            event @ (Event::TaskPlan(_) | Event::TaskPart(_) | Event::TaskPartDone(_)) => {
                self.handle_event(event)
            }
            Event::Log(message) => self.record(format!("{}：{message}", active.label())),
            Event::Progress(progress) => self.handle_event(Event::Progress(progress)),
            Event::Installed(id) => {
                if active
                    .context
                    .target
                    .as_deref()
                    .is_some_and(|target| target != id)
                {
                    self.handle_event(Event::DownloadFailed {
                        message: format!(
                            "{}：安装返回了不一致的目标 {id}，未改变当前版本选择。",
                            active.label()
                        ),
                        cancelled: false,
                    });
                    return;
                }
                self.record(format!("{}：安装完成。", active.label()));
                if current_root {
                    self.handle_event(Event::Installed(id));
                } else {
                    self.finish_download_task();
                    self.status = format!(
                        "{}：安装完成；当前游戏目录已切换。",
                        active.context.description()
                    );
                }
            }
            Event::Resource(resource_ui::ResourceEvent::Installed(target, filename)) => {
                // Completion is processed before any RequestKey/view filtering.
                if active
                    .context
                    .target
                    .as_deref()
                    .is_some_and(|expected| expected != target)
                {
                    self.handle_event(Event::DownloadFailed {
                        message: format!(
                            "{}：资源返回了不一致的目标 {target}，未刷新其他实例。",
                            active.label()
                        ),
                        cancelled: false,
                    });
                    return;
                }
                self.finish_download_task();
                self.status = format!("已将 {filename} 安装到 {}", active.context.description());
                self.record(format!("{}：{}", active.label(), self.status));
                if current_root
                    && self.settings.selected_version.as_deref() == Some(target.as_str())
                {
                    self.refresh_mods();
                }
            }
            Event::ModsChanged { target, message } => {
                if active.context.target.as_deref() != Some(target.as_str()) {
                    self.handle_event(Event::DownloadFailed {
                        message: "Mod 操作返回目标不一致".into(),
                        cancelled: false,
                    });
                } else {
                    self.finish_download_task();
                    self.status = message.clone();
                    self.record(message);
                    self.instance_setup = Default::default();
                    if current_root
                        && self.settings.selected_version.as_deref() == Some(target.as_str())
                    {
                        self.refresh_mods();
                    }
                }
            }
            Event::ModsUpdated { target, report } => {
                if active.context.target.as_deref() != Some(target.as_str()) {
                    self.handle_event(Event::DownloadFailed {
                        message: "Mod 更新返回了不同的目标，未刷新当前列表".into(),
                        cancelled: false,
                    });
                } else {
                    self.complete_mod_updates(target, report, current_root);
                }
            }
            Event::Runtime(java_ui::RuntimeEvent::Installed(runtime)) => {
                self.record(format!(
                    "{}：Java {} 已下载并验证。",
                    active.label(),
                    runtime.version
                ));
                if current_root {
                    self.handle_event(Event::Runtime(java_ui::RuntimeEvent::Installed(runtime)));
                } else {
                    self.finish_download_task();
                    self.status = format!(
                        "Java {} 已下载并验证；当前目录已切换，未修改当前 Java 设置。",
                        runtime.version
                    );
                }
            }
            Event::DownloadFailed { message, cancelled } => {
                self.record(format!("{}：{message}", active.label()));
                self.handle_event(Event::DownloadFailed { message, cancelled });
            }
            Event::Error(message) => {
                self.record(format!("{}：{message}", active.label()));
                self.handle_event(Event::DownloadFailed {
                    message,
                    cancelled: false,
                });
            }
            Event::Done(message) => {
                self.finish_download_task();
                self.status = message.clone();
                self.record(format!("{}：{message}", active.label()));
            }
            event if current_root => self.handle_event(event),
            _ => (),
        }
    }
    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Job(_) => (),
            Event::JobStarted => {
                if let Some(task) = self.task.as_mut() {
                    task.started();
                }
            }
            Event::TaskPlan(parts) => {
                if let Some(task) = self.task.as_mut() {
                    task.component_plan(parts);
                }
            }
            Event::TaskPart(index) => {
                if let Some(task) = self.task.as_mut() {
                    task.component_start(index);
                }
            }
            Event::TaskPartDone(index) => {
                if let Some(task) = self.task.as_mut() {
                    task.component_done(index);
                }
            }

            Event::VersionList(event) => self.handle_version_list_event(event),
            Event::OptiFineList(event) => self.handle_optifine_list(event),
            Event::Resource(event) => self.handle_resource_event(event),
            Event::Account(event) => {
                self.handle_account_event(event);
                if self.busy.is_none() {
                    self.launch_ui.authentication_ended();
                } else {
                    self.launch_ui.authentication_token(self.cancel.clone());
                }
            }
            Event::Launch(event) => self.handle_launch_event(event),
            Event::Runtime(event) => self.handle_runtime_event(event),
            Event::ScriptExported(result) => {
                self.busy = None;
                self.progress = None;
                self.status = format!("启动脚本已导出：{}", result.path.display());
                self.script_export_result = Some(result);
            }
            Event::DeletePreview(result) => {
                self.busy = None;
                match result {
                    Ok(preview) if self.is_current_game_root(&preview.game_root) => {
                        self.pending_version_delete = Some(preview)
                    }
                    Ok(_) => self.record("游戏目录已切换，已忽略旧目录的删除预览。".into()),
                    Err(error) => self.error = Some(error),
                }
            }
            Event::VersionDeleted { root, id, result } => {
                self.busy = None;
                self.progress = None;
                if self.is_current_game_root(&root) {
                    if result.is_ok() && self.settings.selected_version.as_deref() == Some(&id) {
                        self.settings.selected_version = None;
                        self.persist();
                        self.version_tools = false;
                        self.version_view = true;
                    }
                    self.instance_setup = instance_setup_ui::InstanceSetupState::default();
                    self.refresh_versions();
                }
                match result {
                    Ok(_) => {
                        self.status = format!("版本 {id} 已移入回收站");
                        self.record(format!("{} 中的版本 {id} 已移入回收站", root.display()));
                    }
                    Err(error) => self.error = Some(error),
                }
            }

            Event::Java(request, result) => {
                if request != self.java_request {
                    return;
                }
                self.java_download.set_detecting(false);
                match result {
                    Ok(values) => self.apply_discovered_java(values),
                    Err(error) => {
                        self.record(error.clone());
                        self.error = Some(error);
                    }
                }
            }
            Event::Mods(root, id, request, result) => {
                if root != self.settings.game_root
                    || request != self.mods_request
                    || self.settings.selected_version.as_deref() != Some(&id)
                {
                    return;
                }
                match result {
                    Ok(values) => self.local_mods = values,
                    Err(error) => {
                        self.record(error.clone());
                        self.error = Some(error);
                    }
                }
            }
            Event::Versions(root, request, result) => {
                if root != self.settings.game_root || request != self.versions_request {
                    return;
                }
                let values = match result {
                    Ok(values) => values,
                    Err(error) => {
                        self.record(error.clone());
                        self.error = Some(error);
                        return;
                    }
                };
                self.versions = values;
                if !self
                    .versions
                    .iter()
                    .any(|v| self.settings.selected_version.as_ref() == Some(&v.id))
                {
                    self.settings.selected_version = self
                        .versions
                        .iter()
                        .find(|v| v.error.is_none())
                        .map(|v| v.id.clone());
                }
            }
            Event::Progress(p) => {
                self.status = p.message.clone();
                if let Some(task) = self
                    .task
                    .as_mut()
                    .filter(|task| self.processing_job && task.is_running())
                {
                    task.update(&p);
                }
                self.progress = Some(p);
            }
            Event::Log(message) => self.record(message),
            Event::Installed(id) => {
                self.finish_download_task();
                self.settings.selected_version = Some(id.clone());
                self.persist();
                self.status = format!("{id} 安装完成");
                self.busy = None;
                self.progress = None;
                // A successful registration makes the name occupied. Leave
                // the completed form instead of showing a conflict error.
                if self.page == Page::Download && self.download_tab == 0 && self.install_name == id
                {
                    self.download_selection = None;
                }
                self.refresh_versions();
            }
            Event::VersionRenamed {
                root,
                old,
                new,
                result,
            } => {
                self.busy = None;
                self.progress = None;
                if root == self.settings.game_root {
                    if result.is_ok() && self.settings.selected_version.as_deref() == Some(&old) {
                        self.settings.selected_version = Some(new.clone());
                        self.persist();
                    }
                    self.instance_setup = instance_setup_ui::InstanceSetupState::default();
                    self.refresh_versions();
                    match result {
                        Ok(()) => self.status = format!("版本已重命名为 {new}"),
                        Err(error) => self.error = Some(error),
                    }
                } else {
                    match result {
                        Ok(()) => {
                            self.record(format!("{} 中的 {old} 已重命名为 {new}", root.display()))
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
            }
            Event::GameContext {
                pid,
                game_dir,
                started,
                secret,
            } => self.crash.context(pid, game_dir, started, secret),
            Event::GameLog { pid, line } => {
                self.crash.line(pid, line.clone());
                self.record(line);
            }
            Event::GameStarted(pid) => {
                self.game_pid = Some(pid);
                self.launch_ui.started(
                    pid,
                    self.settings
                        .selected_version
                        .as_deref()
                        .unwrap_or("Minecraft"),
                    self.microsoft,
                );
                if self.launch_ui.cancellation_requested(pid) {
                    // Covers a cancel arriving after spawn's last check but
                    // before the UI receives GameStarted.
                    self.game_stop.store(true, Ordering::Relaxed);
                }
                self.game_window.started();
                self.busy = None;
                self.status = format!("游戏进程已启动 · PID {pid}");
            }
            Event::GameReady { pid, visibility } => {
                if self.game_pid == Some(pid)
                    && !self.launch_ui.cancellation_requested(pid)
                    && !self.game_stop.load(Ordering::Relaxed)
                {
                    self.launch_ui.ready(pid);
                    self.game_window.ready(pid, visibility);
                    if let Err(error) = self.appearance.music_game_changed(true, &self.settings) {
                        self.error = Some(format!("背景音乐联动失败：{error:#}"));
                    }
                }
            }
            Event::GameFinished {
                pid,
                success,
                stopped,
                message,
            } => {
                if self.game_pid != Some(pid) {
                    return;
                }
                self.crash.finished(pid, success, stopped);
                self.game_window.finished(pid, success, stopped);
                if let Err(error) = self.appearance.music_game_changed(false, &self.settings) {
                    self.error = Some(format!("背景音乐联动失败：{error:#}"));
                }
                self.game_pid = None;
                self.launch_ui.exited(pid);
                self.status = message.clone();
                self.record(message);
            }
            Event::LaunchWarning(message) => {
                self.record(message.clone());
                self.push_hint(hint_ui::HintKind::Error, message);
            }
            Event::GameStopFailed { pid, message } => {
                if self.game_pid == Some(pid) {
                    self.launch_ui.stop_failed(pid);
                    self.record(message.clone());
                    self.error = Some(message);
                }
            }
            Event::ModsUpdated { .. } | Event::ModsChanged { .. } => (),
            Event::Done(message) => {
                self.status = message;
                self.busy = None;
            }
            Event::DownloadFailed { message, cancelled } => {
                self.record(message.clone());
                if let Some(task) = self.task.as_mut().filter(|task| task.is_running()) {
                    task.fail(message.clone(), cancelled);
                    self.task_view = !cancelled;
                    self.status = if cancelled {
                        "下载已取消".into()
                    } else {
                        message
                    };
                } else {
                    self.error = Some(message);
                }
                self.busy = None;
                self.progress = None;
            }
            Event::Error(message) => {
                self.record(message.clone());
                self.error = Some(message);
                self.busy = None;
                self.progress = None;
            }
        }
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        self.custom_home_page(ui);
    }
    fn modpack_page(&mut self, ui: &mut egui::Ui) {
        card(ui, "导入整合包", |ui| {
            if ui
                .add_enabled(
                    self.busy.is_none() && self.game_pid.is_none(),
                    egui::Button::new("选择整合包文件"),
                )
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("整合包", &["mrpack", "zip"])
                    .pick_file()
                {
                    match modpack::inspect_mrpack(&path) {
                        Ok(info) => {
                            self.pack_id = format!("{}-{}", info.name, info.version_id)
                                .chars()
                                .map(|c| {
                                    if c.is_ascii_alphanumeric()
                                        || c == '-'
                                        || c == '_'
                                        || c == '.'
                                        || ('\u{4e00}'..='\u{9fff}').contains(&c)
                                    {
                                        c
                                    } else {
                                        '-'
                                    }
                                })
                                .take(80)
                                .collect::<String>()
                                .trim_matches('.')
                                .to_owned();
                            self.pack_info = Some((path, info));
                        }
                        Err(error) => self.error = Some(format!("整合包读取失败：{error:#}")),
                    }
                }
            }
            ui.label(
                RichText::new("支持 Modrinth、MultiMC / Prism、HMCL 和 MCBBS 整合包，安装到新实例。已有文件不会覆盖。")
                    .small()
                    .color(MUTED),
            );
        });
        let Some((path, info)) = self.pack_info.clone() else {
            return;
        };
        card(ui, &info.name, |ui| {
            ui.label(format!(
                "{} · {} · Minecraft {}",
                info.format, info.version_id, info.minecraft
            ));
            if let Some(summary) = &info.summary {
                ui.label(summary);
            }
            for warning in &info.warnings {
                ui.label(RichText::new(warning).small().color(MUTED));
            }
            for (dependency, version) in &info.dependencies {
                ui.label(format!("{dependency}：{version}"));
            }
            ui.label(format!(
                "{} 个文件，其中 {} 个可选",
                info.files, info.optional_files
            ));
            ui.horizontal(|ui| {
                ui.label("新实例名称");
                ui.add(crate::ui_style::singleline(&mut self.pack_id).desired_width(300.0));
            });
            ui.checkbox(&mut self.pack_optional, "安装可选客户端文件");
        });
        if ui
            .add_enabled(
                self.busy.is_none() && self.game_pid.is_none(),
                egui::Button::new(
                    RichText::new("安装整合包").color(theme::palette(ui.ctx()).accent),
                )
                .min_size(Vec2::new(160.0, 35.0)),
            )
            .clicked()
        {
            let id = self.pack_id.trim().to_owned();
            if let Err(error) = metadata::validate_id(&id) {
                self.error = Some(format!("实例名称无效：{error:#}"));
                return;
            }
            let Some((tx, _cancel)) = self.start_download_job("正在安装整合包", Some(id.clone()))
            else {
                return;
            };
            let root = self.settings.game_root.clone();
            let optional = self.pack_optional;
            let java = self
                .settings
                .java_path
                .clone()
                .or_else(|| self.settings.java_priority.first().cloned());
            let retry = pcl_core::packs::PackRetry::default();
            tx.spawn(move |tx| {
                let cancel = tx.cancel_token();
                let result = retry.install_with_java(
                    &root,
                    &path,
                    &id,
                    optional,
                    java.as_deref(),
                    &Platform::current(),
                    &cancel,
                    |p| {
                        let _ = tx.send(Event::Progress(p));
                    },
                );
                if !retry.retryable() {
                    tx.disable_retry();
                }
                let _ = tx.send(match result {
                    Ok(id) => Event::Installed(id),
                    Err(error) => Event::download_failed("整合包安装未完成", error),
                });
            });
        }
    }

    fn open_root(&mut self) {
        if let Err(e) = std::fs::create_dir_all(&self.settings.game_root)
            .and_then(|_| crate::process::open_folder(&self.settings.game_root))
        {
            self.error = Some(format!("打开目录失败：{e}"));
        }
    }
    fn busy_status_controls(&mut self, ui: &mut egui::Ui) {
        // Bound the row's height so a following progress rail keeps its space.
        // Allocate actions before the truncating status label.
        ui.scope(|ui| {
            ui.spacing_mut().interact_size.y =
                ui.spacing().interact_size.y.min(ui.available_height());
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.busy.is_some() && ui.small_button("取消").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                        self.status = "正在取消…".into();
                    }
                    if ui.small_button("日志").clicked() {
                        self.show_logs = true;
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        if self.busy.is_some() {
                            loading_ui::inline(ui, "");
                        }
                        ui.add(
                            egui::Label::new(RichText::new(&self.status).size(12.0).color(MUTED))
                                .truncate(),
                        );
                    });
                });
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(message) = self.error.clone() {
            if account_ui::account_modal(
                ctx,
                "operation-error",
                "操作未完成",
                &message,
                &["知道了"],
            )
            .is_some()
            {
                self.error = None;
            }
        }
    }
}

impl Launcher {
    fn push_hint(&mut self, kind: hint_ui::HintKind, text: impl Into<String>) {
        self.noticed_status = self.status.clone();
        self.hints.push(kind, text);
    }
}

impl eframe::App for Launcher {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.flush_window_size();
    }
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.remember_window(ctx);
        theme::apply(ctx, &self.settings);
        if let Err(error) = crate::startup_splash::tick(ctx) {
            self.error = Some(format!("启动画面关闭失败：{error:#}"));
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F12)) {
            ctx.data_mut(|d| {
                let id = egui::Id::new("pcl-reveal-hidden");
                let show = d.get_temp::<bool>(id).unwrap_or(false);
                d.insert_temp(id, !show);
            });
        }
        self.receive();
        self.system_tick(ctx);
        self.mod_update_tick(ctx);
        self.offline_skin_tick(ctx);
        self.game_window.apply(ctx);
        let size = ctx.content_rect().size();
        let scale = ctx.pixels_per_point();
        self.last_viewport_size = Some((
            (size.x * scale).round().clamp(1.0, 16384.0) as u32,
            ((size.y - 29.5).max(100.0) * scale)
                .round()
                .clamp(1.0, 16384.0) as u32,
        ));
        if let Err(error) = self
            .window_opacity
            .update(frame, self.settings.ui_launcher_opacity)
        {
            self.error = Some(format!("设置窗口不透明度失败：{error:#}"));
        }
        if self.script_export_result.is_none() {
            self.account_tick();
        }
        if self.status != self.noticed_status {
            self.noticed_status = self.status.clone();
            if self.busy.is_none() {
                self.hints
                    .push(hint_ui::HintKind::Info, self.status.clone());
            }
        }
        ctx.request_repaint_after(Duration::from_millis(150));
        let painter = ctx.layer_painter(egui::LayerId::background());
        let screen = ctx.content_rect();
        if let Err(error) = self
            .appearance
            .ensure_loaded(ctx, &self.settings, &self.settings_path)
        {
            self.error = Some(format!("背景图片读取失败：{error:#}"));
        }
        self.appearance
            .paint_background(&painter, screen, &self.settings);
        self.title_bar(ctx);
        let downloading = self.task.as_ref().is_some_and(|task| task.is_running());
        if !downloading
            && !self.task_view
            && self.busy.is_some()
            && !self.resource_request_active()
            && !self.version_list_request_active()
            && !self.optifine_request_active()
            && !(self.page == Page::Launch && self.launch_ui.preparing())
        {
            egui::TopBottomPanel::bottom("status")
                .exact_height(if self.progress.is_some() { 57.0 } else { 31.0 })
                .frame(
                    egui::Frame::new()
                        .fill(Color32::WHITE)
                        .inner_margin(egui::Margin::symmetric(15, 5)),
                )
                .show(ctx, |ui| {
                    self.busy_status_controls(ui);
                    if let Some(p) = &self.progress {
                        let fraction = if p.total == 0 {
                            0.0
                        } else {
                            p.completed as f32 / p.total as f32
                        };
                        loading_ui::progress(ui, Some(fraction), "");
                    }
                });
        }
        let sidebar_edge = screen.left() + page_motion::sidebar_width(ctx, self.sidebar_width());
        ui_style::gradient(
            &painter,
            egui::Rect::from_min_max(
                egui::pos2(sidebar_edge, screen.top() + 48.0),
                egui::pos2(sidebar_edge + 4.0, screen.bottom() - 6.0),
            ),
            [
                Color32::from_black_alpha(10),
                Color32::TRANSPARENT,
                Color32::TRANSPARENT,
                Color32::from_black_alpha(10),
            ],
        );
        self.sidebar(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let motion = page_motion::begin(
                    ui,
                    "content",
                    self.page_motion_key(),
                    page_motion::Kind::Content,
                );
                // WPF puts PanMain's margin inside MyScrollViewer. The viewport
                // must reach the window edge, including under the last card.
                let content_style = ui.style().clone();
                let scroll = &mut ui.spacing_mut().scroll;
                scroll.bar_width = 4.0;
                scroll.floating_width = 4.0;
                scroll.bar_outer_margin = 2.0;
                scroll.dormant_handle_opacity = 0.5;
                scroll.active_handle_opacity = 0.5;
                scroll.interact_handle_opacity = 0.9;
                scroll.dormant_background_opacity = 0.0;
                scroll.active_background_opacity = 0.0;
                scroll.interact_background_opacity = 0.0;
                ui.visuals_mut().widgets.inactive.fg_stroke.color = theme::palette(ui.ctx()).accent;
                ui.visuals_mut().widgets.hovered.fg_stroke.color = theme::palette(ui.ctx()).accent;
                ui.visuals_mut().widgets.active.fg_stroke.color = theme::palette(ui.ctx()).accent;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .id_salt((
                        self.page as u8,
                        self.task_view,
                        self.version_view,
                        self.version_tools,
                        self.tools_tab,
                        self.settings_tab,
                        self.download_tab,
                        self.resource_browser.scroll_key(),
                        self.more_scroll_key(),
                    ))
                    .show(ui, |ui| {
                        ui.set_style(content_style);
                        if self.task_view {
                            egui::Frame::NONE
                                .inner_margin(egui::Margin {
                                    left: 25,
                                    right: 25,
                                    top: 25,
                                    bottom: 10,
                                })
                                .show(ui, |ui| self.task_page(ui));
                            return;
                        }
                        egui::Frame::NONE
                            .inner_margin(egui::Margin {
                                left: 25,
                                right: 25,
                                top: 25,
                                bottom: if self.page == Page::More { 10 } else { 25 },
                            })
                            .show(ui, |ui| match self.page {
                                Page::Launch if self.version_tools => self.version_tools_page(ui),
                                Page::Launch if self.version_view => self.versions_page(ui),
                                Page::Launch => self.home(ui),
                                Page::Download => self.downloads(ui),
                                Page::Settings => self.settings_page(ui),
                                Page::More => self.more_page(ui),
                            });
                    });
                page_motion::finish(ui, motion);
            });
        if !self.task_view
            && self.page == Page::Download
            && self.download_tab == 0
            && self.download_selection.is_some()
        {
            self.install_footer(ctx);
        }
        self.floating_entries(ctx);
        self.appearance.paint_startup_logo(ctx, &self.settings);
        self.hints.show(ctx);
        self.folder_dialog(ctx);
        self.handle_file_drop(ctx);
        self.dialogs(ctx);
        self.version_delete_dialog(ctx);
        self.account_dialogs(ctx);
        self.mod_update_dialog(ctx);
        self.java_download_dialog(ctx);
        self.script_export_dialog(ctx);
        self.more_dialogs(ctx);
        self.crash_dialogs(ctx);
        self.launch_running_dialog(ctx);
        self.mod_details_finish_frame(ctx);
        self.settings_reset_dialog(ctx);
        modal_ui::finish_frame(ctx);
    }
}

fn card(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 245))
        .corner_radius(5)
        .inner_margin(egui::Margin {
            left: 15,
            right: 15,
            top: 12,
            bottom: 18,
        })
        .stroke(egui::Stroke::new(0.5_f32, Color32::from_rgb(231, 235, 240)))
        .shadow(egui::epaint::Shadow {
            offset: [0, 2],
            blur: 3,
            spread: 0,
            color: Color32::from_black_alpha(9),
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(ui_style::card_title(title));
            ui.add_space(4.0);
            body(ui);
        });
    ui.add_space(7.0);
}

#[cfg(test)]
mod event_tests {
    use super::*;

    #[test]
    fn long_busy_status_keeps_both_actions_visible_and_clickable() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = fixture(directory.path());
        app.busy = Some("fixture".into());
        let long = "正在读取资源与游戏目录 Reading a very long resource or folder name ".repeat(40);
        let ctx = egui::Context::default();
        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = Vec2::new(10.0, 8.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        for text_style in [egui::TextStyle::Body, egui::TextStyle::Button] {
            style
                .text_styles
                .insert(text_style, egui::FontId::proportional(13.0));
        }
        ctx.set_style(style);
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "PCL English".into(),
            egui::FontData::from_static(include_bytes!("../assets/upstream/Resources/Font.ttf"))
                .into(),
        );
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(0, "PCL English".into());
        // Exercise the packaged CJK font when its local export is available.
        let cjk_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test-output/fonts/PingFang-Regular.otf"
        );
        if let Ok(data) = std::fs::read(cjk_path) {
            fonts
                .font_data
                .insert("Status CJK".into(), egui::FontData::from_owned(data).into());
            fonts
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .insert(1, "Status CJK".into());
            eprintln!("status geometry uses CJK font: {cjk_path}");
        }
        ctx.set_fonts(fonts);
        for width in [810.0, 989.0] {
            for show_progress in [false, true] {
                app.status = long.clone();
                app.cancel.store(false, Ordering::Relaxed);
                app.show_logs = false;
                let height = if show_progress { 57.0 } else { 31.0 };
                let draw = |app: &mut Launcher, events| {
                    let mut rail = None;
                    let mut content = egui::Rect::NOTHING;
                    let output = ctx.run(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                Vec2::new(width, 470.0),
                            )),
                            events,
                            ..Default::default()
                        },
                        |ctx| {
                            egui::TopBottomPanel::bottom(egui::Id::new((
                                "status-audit",
                                show_progress,
                            )))
                            .exact_height(height)
                            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(15, 5)))
                            .show(ctx, |ui| {
                                content = ui.max_rect();
                                app.busy_status_controls(ui);
                                if show_progress {
                                    rail = Some(loading_ui::progress(ui, Some(0.5), "").rect);
                                }
                            });
                        },
                    );
                    (output, rail, content)
                };
                let (output, rail, content) = draw(&mut app, vec![]);
                let text_rect = |name: &str| {
                    output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) if text.galley.text() == name => {
                                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
                            }
                            _ => None,
                        })
                        .unwrap()
                };
                let status = text_rect(&long);
                let cancel = text_rect("取消");
                let logs = text_rect("日志");
                assert!(
                    status.right() < logs.left(),
                    "status overlaps actions at width {width}"
                );
                assert!(logs.right() < cancel.left());
                assert!(cancel.right() <= width - 15.0);
                for text in [status, cancel, logs] {
                    assert!(
                        content.contains_rect(text),
                        "row text outside {height}-DIP panel: {text:?} / {content:?}"
                    );
                }
                if let Some(rail) = rail {
                    assert!(
                        content.contains_rect(rail),
                        "progress rail outside {height}-DIP panel: {rail:?} / {content:?}"
                    );
                    assert!(rail.top() > status.bottom().max(cancel.bottom()).max(logs.bottom()));
                    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                        egui::Shape::Rect(rect) if rect.rect.height()==3.0 && rect.rect.width()>100.0
                            && rail.contains_rect(rect.rect) && shape.clip_rect.contains_rect(rect.rect)
                    )), "the actual progress rail must be painted inside its clip");
                }
                let click = |app: &mut Launcher, point| {
                    for pressed in [true, false] {
                        draw(
                            app,
                            vec![
                                egui::Event::PointerMoved(point),
                                egui::Event::PointerButton {
                                    pos: point,
                                    button: egui::PointerButton::Primary,
                                    pressed,
                                    modifiers: egui::Modifiers::NONE,
                                },
                            ],
                        );
                    }
                };
                click(&mut app, logs.center());
                assert!(app.show_logs);
                click(&mut app, cancel.center());
                assert!(app.cancel.load(Ordering::Relaxed));
                assert_eq!(app.status, "正在取消…");
            }
        }
    }

    #[test]
    fn launch_cancel_is_normal_but_io_errors_mentioning_cancel_remain_errors() {
        let cancelled =
            anyhow::Error::new(pcl_core::model::OperationCancelled).context("等待启动前命令");
        assert!(
            matches!(Event::launch_failed(1, "启动失败", "启动已取消", cancelled), Event::Launch(launch_ui::LaunchEvent::Failed { request: 1, cancelled: true, message }) if message == "启动已取消")
        );
        let failure = anyhow::anyhow!("取消请求后无法读取游戏文件");
        assert!(
            matches!(Event::launch_failed(1, "启动失败", "启动已取消", failure), Event::Launch(launch_ui::LaunchEvent::Failed { request: 1, cancelled: false, message }) if message.contains("无法读取游戏文件"))
        );
    }

    #[test]
    fn stale_game_exit_and_launch_warning_do_not_clear_another_operation() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        app.game_pid = Some(20);
        app.busy = Some("fixture download".into());
        app.handle_event(Event::GameFinished {
            pid: 19,
            success: true,
            stopped: false,
            message: "old game".into(),
        });
        assert_eq!(app.game_pid, Some(20));
        assert_eq!(app.busy.as_deref(), Some("fixture download"));
        app.handle_event(Event::LaunchWarning("pre-launch warning".into()));
        assert_eq!(app.busy.as_deref(), Some("fixture download"));
        assert!(app.logs.iter().any(|line| line == "pre-launch warning"));
        app.handle_event(Event::GameFinished {
            pid: 20,
            success: false,
            stopped: true,
            message: "stopped game".into(),
        });
        assert!(app.game_pid.is_none());
        assert_eq!(app.busy.as_deref(), Some("fixture download"));
    }

    #[test]
    fn shutdown_entry_stays_global_and_task_entry_does_not_overlap_it() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        let ctx = egui::Context::default();
        app.assets = Assets::new(&ctx);
        app.game_pid = Some(123); // UI state only; this test never signals a PID.
        app.page = Page::Settings;
        app.task = Some(task_ui::TaskState::new("fixture"));
        let frame = |app: &mut Launcher, events| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(850.0, 600.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| app.floating_entries(ctx));
        };
        frame(&mut app, vec![]);
        frame(&mut app, vec![]);
        let click = |app: &mut Launcher, pos| {
            frame(
                app,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            frame(
                app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        };
        click(&mut app, egui::pos2(815.0, 565.0));
        assert!(app.game_stop.load(Ordering::Relaxed));
        assert!(!app.task_view);
        app.game_stop.store(false, Ordering::Relaxed);
        click(&mut app, egui::pos2(815.0, 515.0));
        assert!(app.task_view);
        assert!(!app.game_stop.load(Ordering::Relaxed));
        // The task entry disappears on entering its page. Its focus must not
        // transfer to the power button that moves into its allocation slot.
        frame(&mut app, vec![]);
        for pressed in [true, false] {
            frame(
                &mut app,
                vec![egui::Event::Key {
                    key: egui::Key::Space,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        assert!(!app.game_stop.load(Ordering::Relaxed));
        click(&mut app, egui::pos2(815.0, 565.0));
        assert!(app.game_stop.load(Ordering::Relaxed));
        app.handle_event(Event::GameFinished {
            pid: 123,
            success: true,
            stopped: false,
            message: "游戏已正常退出".into(),
        });
        app.game_stop.store(false, Ordering::Relaxed);
        click(&mut app, egui::pos2(815.0, 565.0));
        assert!(!app.game_stop.load(Ordering::Relaxed));
    }

    // Construct only in-memory UI state and a temporary settings destination.
    // Do not call Launcher::new: it loads the user's files and account catalog.
    pub(super) fn fixture(root: &std::path::Path) -> Launcher {
        let settings = Settings {
            game_root: root.join("game-a"),
            ..Default::default()
        };
        std::fs::create_dir_all(&settings.game_root).unwrap();
        let path = root.join("settings.json");
        config::save_settings(&path, &settings).unwrap();
        let (tx, rx) = mpsc::channel();
        Launcher {
            assets: Assets::new(&egui::Context::default()),
            folder_ui: Default::default(),
            appearance: appearance_ui::AppearanceState::default(),
            home: home_ui::HomeState::default(),
            crash: crash_ui::CrashUiState::default(),
            more: more_ui::MoreState::default(),
            accounts: account_ui::AccountUiState::default(),
            java_download: java_ui::JavaDownloadState::default(),
            instance_setup: instance_setup_ui::InstanceSetupState::default(),
            setup_launch: setup_launch_ui::SetupLaunchState::default(),
            setup_system: setup_system_ui::SetupSystemState::default(),
            mod_update: mod_update_ui::ModUpdateState::default(),
            offline_skin: offline_skin_ui::OfflineSkinState::default(),
            pending_version_delete: None,
            script_export_result: None,
            pack_export: Default::default(),
            noticed_status: "准备就绪".into(),
            hints: hint_ui::HintQueue::default(),
            root_text: settings.game_root.display().to_string(),
            java_text: settings
                .java_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            settings,
            settings_path: path,
            page: Page::Launch,
            version_view: false,
            microsoft: false,
            session: None,
            versions: vec![],
            versions_request: 0,
            mods_request: 0,
            manifest: vec![],
            version_lists: Default::default(),
            optifine: Default::default(),
            java: vec![],
            java_request: 0,
            tx,
            rx,
            busy: None,
            jobs: job::Jobs::default(),
            progress: None,
            task: None,
            task_hub: Default::default(),
            processing_job: false,
            task_view: false,
            cancel: Arc::new(AtomicBool::new(false)),
            game_stop: Arc::new(AtomicBool::new(false)),
            game_pid: None,
            launch_ui: Default::default(),
            game_window: Default::default(),
            window_opacity: Default::default(),
            last_viewport_size: None,
            window_state: Default::default(),
            pending_settings_reset: None,
            status: "准备就绪".into(),
            error: None,
            logs: VecDeque::new(),
            show_logs: false,
            version_tools: false,
            tools_tab: 0,
            settings_tab: 0,
            install_name: String::new(),
            install_name_edited: false,
            local_mods: vec![],
            mods_filter: String::new(),
            download_selection: None,
            loader_kind: None,
            loader_expanded: None,
            loader_versions: vec![],
            loader_version: None,
            download_tab: 0,
            pack_info: None,
            pack_id: String::new(),
            pack_optional: false,
            resource_browser: resource_ui::ResourceBrowser::default(),
        }
    }
    #[test]
    fn job_receiver_rejects_late_a_and_unscoped_events_without_changing_b() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        let (a, _) = app.start_download_job("A", Some("a".into())).unwrap();
        a.send(Event::DownloadFailed {
            message: "A cancelled".into(),
            cancelled: true,
        })
        .unwrap();
        app.receive();
        let (b, _) = app.start_download_job("B", Some("b".into())).unwrap();
        b.send(Event::Progress(Progress {
            message: "B progress".into(),
            ..Default::default()
        }))
        .unwrap();
        app.receive();
        let busy = app.busy.clone();
        for event in [
            Event::Progress(Progress {
                message: "late A".into(),
                ..Default::default()
            }),
            Event::Log("late A log".into()),
            Event::Installed("a".into()),
            Event::DownloadFailed {
                message: "late A failure".into(),
                cancelled: false,
            },
        ] {
            a.send(event).unwrap();
        }
        app.tx
            .send(Event::Progress(Progress {
                message: "unscoped progress".into(),
                ..Default::default()
            }))
            .unwrap();
        app.tx.send(Event::Done("unscoped done".into())).unwrap();
        app.tx
            .send(Event::Error("unscoped real error".into()))
            .unwrap();
        app.tx.send(Event::Installed("unscoped".into())).unwrap();
        app.receive();
        assert!(app.jobs.is_active());
        assert_eq!(app.busy, busy);
        assert_eq!(app.status, "B progress");
        assert_eq!(app.progress.as_ref().unwrap().message, "B progress");
        assert!(app.task.as_ref().unwrap().is_running());
        assert_eq!(app.settings.selected_version, None);
        assert!(!app.logs.iter().any(|line| line.contains("late A log")));
        assert!(app
            .logs
            .iter()
            .any(|line| line.contains("unscoped real error")));
    }
    #[test]
    fn job_old_root_completion_keeps_new_selection_and_settings_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        let (sender, _) = app
            .start_download_job("install old", Some("old".into()))
            .unwrap();
        app.settings.game_root = temp.path().join("game-b");
        app.settings.selected_version = Some("new-selection".into());
        let before = std::fs::read(&app.settings_path).unwrap();
        sender.send(Event::Installed("old".into())).unwrap();
        app.receive();
        assert!(!app.jobs.is_active() && app.busy.is_none());
        assert_eq!(
            app.settings.selected_version.as_deref(),
            Some("new-selection")
        );
        assert_eq!(std::fs::read(&app.settings_path).unwrap(), before);
        assert!(app.task.as_ref().unwrap().is_finished());
        assert!(app
            .logs
            .iter()
            .any(|line| line.contains("game-a") && line.contains("old")));
    }
    #[test]
    fn job_pack_export_success_failure_and_cancel_release_the_scoped_writer() {
        use pcl_core::pack_export::{self, PackExportOptions, PackExportReport, ResourceMode};
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        let destination = temp.path().join("fixture.mrpack");
        let options = PackExportOptions {
            name: "fixture".into(),
            resource_mode: ResourceMode::EmbedAll,
            ..Default::default()
        };
        let failure = pack_export::export_pack(
            &app.settings.game_root,
            "missing-instance",
            &destination,
            &options,
            &AtomicBool::new(false),
            |_| {},
        );
        assert!(failure.is_err());
        let cancelled = pack_export::export_pack(
            &app.settings.game_root,
            "missing-instance",
            &destination,
            &options,
            &AtomicBool::new(true),
            |_| {},
        );
        assert!(cancelled
            .as_ref()
            .unwrap_err()
            .chain()
            .any(|error| error.is::<pcl_core::model::OperationCancelled>()));
        let success = Ok(PackExportReport {
            path: destination.clone(),
            files: 0,
            hosted_files: 0,
            override_files: 0,
            unhosted_files: vec![],
            excluded_sensitive_files: vec![],
            bytes: 0,
        });
        for (result, failed) in [(failure, true), (cancelled, false), (success, false)] {
            let (sender, _) = app
                .start_download_job_at(
                    "正在导出整合包",
                    destination.clone(),
                    Some("fixture".into()),
                )
                .unwrap();
            sender
                .send(pack_export_ui::pack_export_event(result))
                .unwrap();
            app.receive();
            assert!(!app.jobs.is_active() && app.busy.is_none());
            assert_eq!(app.task.as_ref().unwrap().is_failed(), failed);
            if !failed {
                assert!(app.task.as_ref().unwrap().is_finished());
            }
        }
        assert!(app.status.contains("整合包已导出"));
        assert!(!destination.exists()); // The success record above is a UI-event fixture.
    }
    #[test]
    fn job_resource_terminal_survives_invalidated_browser_and_cancel_race_keeps_errors() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        let (sender, cancel) = app
            .start_download_job("resource", Some("old".into()))
            .unwrap();
        app.resource_browser = Default::default();
        app.settings.game_root = temp.path().join("game-b");
        app.settings.selected_version = Some("new".into());
        sender
            .send(Event::Resource(resource_ui::ResourceEvent::Installed(
                "old".into(),
                "file.jar".into(),
            )))
            .unwrap();
        cancel.store(true, Ordering::Relaxed);
        app.receive();
        assert!(!app.jobs.is_active() && app.busy.is_none());
        assert!(app.task.as_ref().unwrap().is_finished());
        assert_eq!(app.settings.selected_version.as_deref(), Some("new"));
        let (next, next_cancel) = app.start_download_job("next", Some("next".into())).unwrap();
        assert!(!next_cancel.load(Ordering::Relaxed));
        next_cancel.store(true, Ordering::Relaxed);
        assert!(app.start_job("must wait for cleanup").is_none());
        next.send(Event::download_failed(
            "failed",
            anyhow::anyhow!("cancelled, but cleanup failed"),
        ))
        .unwrap();
        app.receive();
        assert!(!app.jobs.is_active() && app.busy.is_none());
        assert!(app.task.as_ref().unwrap().is_failed());
        assert!(app.status.contains("cleanup failed"));
    }

    #[test]
    fn download_cancel_classification_uses_worker_error_type() {
        let cancelled =
            anyhow::Error::new(pcl_core::model::OperationCancelled).context("下载支持库时结束");
        assert!(matches!(
            Event::download_failed("安装未完成", cancelled),
            Event::DownloadFailed {
                cancelled: true,
                ..
            }
        ));
        // A real cleanup failure may mention cancellation; it must remain failed.
        let failure = anyhow::anyhow!("安装已取消，但部分新增文件无法回滚");
        assert!(matches!(
            Event::download_failed("安装未完成", failure),
            Event::DownloadFailed {
                cancelled: false,
                ..
            }
        ));
    }
    #[test]
    fn task_history_survives_reload_and_new_jobs_cannot_write_running_game() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        let (sender, _) = app
            .start_download_job("download api_key=fixture-private-key", Some("one".into()))
            .unwrap();
        sender
            .send(Event::TaskPlan(vec!["原版".into(), "Fabric".into()]))
            .unwrap();
        sender.send(Event::TaskPart(0)).unwrap();
        app.receive();
        let mut restored = fixture(temp.path());
        restored.load_task_history();
        assert!(restored.task_hub.has_history());
        assert!(!restored.jobs.is_active());
        let history =
            std::fs::read_to_string(app.settings_path.with_file_name("task-history.json")).unwrap();
        assert!(history.contains("Fabric") && history.contains("进行中"));
        assert!(!history.contains("fixture-private-key"));
        app.game_pid = Some(12345);
        assert!(app
            .start_download_job("cannot write", Some("two".into()))
            .is_none());
    }
}
