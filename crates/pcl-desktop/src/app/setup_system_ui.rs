//! PageSetupSystem: actual download policy, initial language and this Rust project's updates.
use super::{hint_ui::HintKind, setup_launch_ui::setup_card, Launcher};
use crate::ui_style;
use eframe::egui::{self, RichText, Vec2};
use pcl_core::{
    config,
    network::SourcePreference,
    system::{self, LauncherRelease, LauncherUpdateMode, UpdateAsset},
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
        Arc,
    },
};

#[derive(Default)]
pub(super) struct SetupSystemState {
    receiver: Option<Receiver<SystemMessage>>,
    cancel: Arc<AtomicBool>,
    loading: bool,
    downloading: bool,
    message: Option<String>,
    release: Option<LauncherRelease>,
    progress: Option<(u64, u64)>,
    saved: Option<PathBuf>,
    key_input: String,
    key_receiver: Option<Receiver<Result<bool, String>>>,
    key_checked: bool,
    key_present: bool,
    key_message: Option<String>,
}
enum KeyAction {
    Check,
    Save,
    Clear,
}
enum SystemMessage {
    Checked {
        launcher: Result<Option<LauncherRelease>, String>,
        game: Result<Option<serde_json::Value>, String>,
        cancelled: bool,
    },
    Progress(u64, u64),
    Downloaded(Result<PathBuf, String>, bool),
}
impl Launcher {
    pub(super) fn open_system_update_check(&mut self) {
        self.page = super::Page::Settings;
        self.settings_tab = 2;
        self.start_system_check(true);
    }
    pub(super) fn init_system_settings(&mut self) {
        if let Err(error) = pcl_core::network::configure(&self.settings.downloads) {
            self.error = Some(format!("下载设置无效：{error:#}"));
        }
        if self.settings.system.launcher_update != LauncherUpdateMode::Disabled
            || self.settings.system.notify_release
            || self.settings.system.notify_snapshot
        {
            self.start_system_check(false);
        }
    }
    fn start_system_check(&mut self, manual: bool) {
        if self.setup_system.loading {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let do_launcher =
            manual || self.settings.system.launcher_update != LauncherUpdateMode::Disabled;
        let do_game =
            manual || self.settings.system.notify_release || self.settings.system.notify_snapshot;
        self.setup_system.receiver = Some(rx);
        self.setup_system.cancel = cancel.clone();
        self.setup_system.loading = true;
        self.setup_system.downloading = false;
        self.setup_system.message = None;
        self.setup_system.release = None;
        self.setup_system.progress = None;
        std::thread::spawn(move || {
            let launcher = if do_launcher {
                system::check_launcher_update(
                    env!("CARGO_PKG_VERSION"),
                    &pcl_core::model::Platform::current(),
                    &cancel,
                )
            } else {
                Ok(None)
            };
            let game = if do_game {
                pcl_core::install::fetch_manifest_with_cancel(&cancel).map(Some)
            } else {
                Ok(None)
            };
            let cancelled = check_was_cancelled(&launcher, &game);
            let _ = tx.send(SystemMessage::Checked {
                launcher: launcher.map_err(|e| format!("{e:#}")),
                game: game.map_err(|e| format!("{e:#}")),
                cancelled,
            });
        });
    }
    fn start_update_download(&mut self, asset: UpdateAsset) {
        if self.setup_system.loading {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let settings = self.settings.system.clone();
        self.setup_system.receiver = Some(rx);
        self.setup_system.cancel = cancel.clone();
        self.setup_system.loading = true;
        self.setup_system.downloading = true;
        self.setup_system.progress = Some((0, asset.size));
        self.setup_system.message = None;
        std::thread::spawn(move || {
            let result = system::download_update(&asset, &settings, &cancel, |done, total| {
                let _ = tx.send(SystemMessage::Progress(done, total));
            });
            let cancelled = result
                .as_ref()
                .err()
                .is_some_and(|e| e.is::<pcl_core::model::OperationCancelled>());
            let _ = tx.send(SystemMessage::Downloaded(
                result.map_err(|e| format!("{e:#}")),
                cancelled,
            ));
        });
    }
    fn start_key_action(&mut self, action: KeyAction) {
        if self.setup_system.key_receiver.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.setup_system.key_receiver = Some(rx);
        self.setup_system.key_message = None;
        let input = if matches!(action, KeyAction::Save) {
            std::mem::take(&mut self.setup_system.key_input)
        } else {
            String::new()
        };
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<bool> {
                match action {
                    KeyAction::Check => (),
                    KeyAction::Save => pcl_core::curseforge::set_api_key(&input)?,
                    KeyAction::Clear => pcl_core::curseforge::clear_api_key()?,
                }
                pcl_core::curseforge::has_api_key()
            })();
            // The API exposes only presence/errors; never return the key to the UI.
            let _ = tx.send(result.map_err(|e| format!("{e:#}")));
        });
    }
    pub(super) fn system_tick(&mut self, ctx: &egui::Context) {
        if let Some(result) = self
            .setup_system
            .key_receiver
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.setup_system.key_receiver = None;
            self.setup_system.key_checked = true;
            match result {
                Ok(present) => {
                    self.setup_system.key_present = present;
                    self.setup_system.key_message = Some(
                        if present {
                            "已配置 API Key；实际访问权限仍需服务端验证。"
                        } else {
                            "未配置 API Key，需要 CurseForge 开发者 API Key 才能使用该来源。"
                        }
                        .into(),
                    );
                }
                Err(error) => self.setup_system.key_message = Some(error),
            }
        }
        let messages: Vec<_> = self
            .setup_system
            .receiver
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for event in messages {
            match event {
                SystemMessage::Progress(done, total) => {
                    self.setup_system.progress = Some((done, total))
                }
                SystemMessage::Downloaded(result, cancelled) => {
                    self.setup_system.loading = false;
                    self.setup_system.downloading = false;
                    match result {
                        Ok(path) => {
                            self.setup_system.message =
                                Some("更新包已通过 SHA-256 与长度验证，尚未安装。".into());
                            self.setup_system.saved = Some(path);
                            self.push_hint(
                                HintKind::Success,
                                "更新包已下载并验证，可在“设置 → 其他”打开缓存文件夹。",
                            );
                        }
                        Err(_) if cancelled => {
                            self.setup_system.message = Some("更新包下载已取消".into())
                        }
                        Err(error) => {
                            self.setup_system.message = Some(error.clone());
                            self.push_hint(HintKind::Error, format!("更新包下载失败：{error}"));
                        }
                    }
                }
                SystemMessage::Checked {
                    launcher,
                    game,
                    cancelled,
                } => {
                    self.setup_system.loading = false;
                    if cancelled {
                        self.setup_system.message = Some("更新检查已取消".into());
                        continue;
                    }
                    let mut next = self.settings.clone();
                    let mut notes = Vec::new();
                    let mut hints = Vec::new();
                    let mut auto_asset = None;
                    match game {
                        Ok(Some(manifest)) => {
                            match system::observe_game_versions(&mut next.system, &manifest) {
                                Ok(updates) => {
                                    for update in updates {
                                        hints.push(format!(
                                            "Minecraft {} {} 已发布。",
                                            if update.snapshot {
                                                "测试版"
                                            } else {
                                                "正式版"
                                            },
                                            update.version
                                        ));
                                    }
                                    notes.push(format!(
                                        "Minecraft 正式版：{}；测试版：{}",
                                        next.system.last_release.as_deref().unwrap_or("—"),
                                        next.system.last_snapshot.as_deref().unwrap_or("—")
                                    ));
                                }
                                Err(error) => {
                                    notes.push(format!("读取游戏更新信息失败：{error:#}"))
                                }
                            }
                        }
                        Err(error) => notes.push(format!("游戏更新检查失败：{error}")),
                        _ => (),
                    }
                    match launcher {
                        Ok(Some(release)) => {
                            notes.push(if release.newer {
                                format!("本项目已发布 {}。", release.tag)
                            } else {
                                format!(
                                    "当前 {}；公开稳定版 {}，没有更高版本。",
                                    env!("CARGO_PKG_VERSION"),
                                    release.tag
                                )
                            });
                            if release.assets.is_empty() {
                                notes.push(
                                    "当前发布没有适合本平台且带可验证 SHA-256 摘要的安装包。"
                                        .into(),
                                );
                            }
                            if release.newer
                                && next.system.last_launcher_tag.as_deref()
                                    != Some(release.tag.as_str())
                            {
                                if next.system.launcher_update != LauncherUpdateMode::Disabled {
                                    hints.push(format!(
                                        "PCL Rust {} 已发布，可在“设置 → 其他”查看。",
                                        release.tag
                                    ));
                                }
                                if next.system.launcher_update == LauncherUpdateMode::Download
                                    && !self.setup_system.cancel.load(Ordering::Relaxed)
                                {
                                    auto_asset = release.assets.first().cloned();
                                }
                            }
                            next.system.last_launcher_tag = Some(release.tag.clone());
                            self.setup_system.release = Some(release);
                        }
                        Ok(None) => notes.push(
                            "本项目当前没有公开稳定版更新包；不会下载原版 PCL 启动器。".into(),
                        ),
                        Err(error) => notes.push(format!("启动器更新检查失败：{error}")),
                    }
                    if next != self.settings {
                        match config::save_settings(&self.settings_path, &next) {
                            Ok(()) => self.settings = next,
                            Err(error) => {
                                notes.push(format!("更新记录保存失败：{error:#}"));
                                auto_asset = None;
                            }
                        }
                    }
                    self.setup_system.message = Some(notes.join("\n"));
                    for hint in hints {
                        self.push_hint(HintKind::Info, hint);
                    }
                    if let Some(asset) = auto_asset {
                        self.start_update_download(asset);
                    }
                }
            }
        }
        if self.setup_system.loading || self.setup_system.key_receiver.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    pub(super) fn system_settings_page(&mut self, ui: &mut egui::Ui) {
        if !self.setup_system.key_checked && self.setup_system.key_receiver.is_none() {
            self.start_key_action(KeyAction::Check);
        }
        let mut key_action = None;
        let previous = self.settings.clone();
        let mut pick_cache = false;
        let mut clear_cache = false;
        let mut open_cache = false;
        let mut check = false;
        let mut download = None;
        ui.spacing_mut().item_spacing.y = 0.0;
        let settings = &mut self.settings;
        setup_card(ui, "下载", 15, None, |ui| {
            system_row(ui, "文件下载源", |ui| {
                source_combo(
                    ui,
                    "system-file-source",
                    &mut settings.downloads.file_source,
                )
            });
            ui.add_space(7.0);
            system_row(ui, "版本列表源", |ui| {
                source_combo(
                    ui,
                    "system-version-source",
                    &mut settings.downloads.version_source,
                )
            });
            ui.add_space(7.0);
            system_row(ui, "最大线程数", |ui| {
                ui.add(egui::Slider::new(&mut settings.downloads.threads, 1..=256));
            });
            system_row(ui, "速度限制", |ui| {
                ui.add(
                    egui::DragValue::new(&mut settings.downloads.speed_limit_kib)
                        .range(0..=1_048_576)
                        .suffix(" KiB/s"),
                );
                ui.label("0 为不限速");
            });
            ui.add_space(5.0);
            ui.label(RichText::new("镜像服务由 BMCLAPI 提供；仅支持的下载地址参与镜像切换。下载目标请在版本选择的文件夹列表中更改。").size(12.0).color(super::MUTED));
        });
        ui.add_space(15.0);
        setup_card(ui, "社区资源", 17, None, |ui| {
            system_row(ui, "CurseForge", |ui| {
                ui.add_sized(
                    [ui.available_width().max(60.0), 28.0],
                    egui::TextEdit::singleline(&mut self.setup_system.key_input)
                        .password(true)
                        .hint_text("开发者 API Key，仅保存到系统安全存储"),
                );
            });
            ui.horizontal(|ui| {
                ui.add_enabled_ui(self.setup_system.key_receiver.is_none(), |ui| {
                    if ui
                        .add_enabled(
                            !self.setup_system.key_input.trim().is_empty(),
                            egui::Button::new("保存 API Key"),
                        )
                        .clicked()
                    {
                        key_action = Some(KeyAction::Save);
                    }
                    if ui.button("清除已保存的 API Key").clicked() {
                        key_action = Some(KeyAction::Clear);
                    }
                });
                if self.setup_system.key_receiver.is_some() {
                    ui.spinner();
                }
            });
            if let Some(message) = &self.setup_system.key_message {
                ui.label(message);
            }
            ui.label(RichText::new("环境变量 PCL_CURSEFORGE_API_KEY 优先；清除只删除系统安全存储的条目。API Key 不写入设置 JSON 或日志。").size(12.0).color(super::MUTED));
        });
        ui.add_space(15.0);
        setup_card(ui, "辅助功能", 17, None, |ui| {
            system_row(ui, "游戏更新提示", |ui| {
                ui.checkbox(&mut settings.system.notify_release, "正式版更新提示");
                ui.checkbox(&mut settings.system.notify_snapshot, "测试版更新提示");
            });
            ui.add_space(8.0);
            system_row(ui, "游戏语言", |ui| {
                ui.checkbox(&mut settings.system.auto_chinese,"自动设置为中文").on_hover_text("仅在本次游戏 options.txt 尚未设置语言时写入中文；保留已有语言选择。不会翻译启动器界面。");
            });
        });
        ui.add_space(15.0);
        setup_card(ui, "启动器", 20, None, |ui| {
            system_row(ui, "启动器更新", |ui| {
                ui_style::PclComboBox::from_id_salt("launcher-update-mode")
                    .width(ui.available_width())
                    .selected_text(update_label(settings.system.launcher_update))
                    .show_ui(ui, |ui| {
                        for mode in [
                            LauncherUpdateMode::Download,
                            LauncherUpdateMode::Notify,
                            LauncherUpdateMode::Disabled,
                        ] {
                            ui.selectable_value(
                                &mut settings.system.launcher_update,
                                mode,
                                update_label(mode),
                            );
                        }
                    });
            });
            ui.add_space(9.0);
            system_row(ui, "缓存文件夹", |ui| {
                let text = settings
                    .system
                    .cache_dir
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "默认".into());
                ui.add(egui::Label::new(text).truncate());
                if ui.button("选择").clicked() {
                    pick_cache = true;
                }
                if ui.button("恢复默认").clicked() {
                    clear_cache = true;
                }
            });
            ui.label(RichText::new("该位置用于本项目更新包缓存；游戏资源、已安装 Java 和原有用户文件不会移动。更新下载后需手动安装。启动器不发送匿名统计或上游公告请求。").size(12.0).color(super::MUTED));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                check = ui
                    .add_enabled(
                        !self.setup_system.loading,
                        egui::Button::new("检查更新").min_size(Vec2::new(140.0, 35.0)),
                    )
                    .clicked();
                open_cache = ui
                    .add_sized([140.0, 35.0], egui::Button::new("打开缓存文件夹"))
                    .clicked();
                if self.setup_system.loading && ui.button("取消").clicked() {
                    self.setup_system.cancel.store(true, Ordering::Relaxed);
                }
            });
            if self.setup_system.loading {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(if self.setup_system.downloading {
                        "正在下载更新包"
                    } else {
                        "正在检查更新"
                    });
                });
            }
            if let Some((done, total)) = self
                .setup_system
                .progress
                .filter(|_| self.setup_system.downloading)
            {
                ui.add(
                    egui::ProgressBar::new((done as f64 / total.max(1) as f64) as f32)
                        .text(format!("{done} / {total} 字节")),
                );
            }
            if let Some(message) = &self.setup_system.message {
                ui.add_space(10.0);
                ui.label(message);
            }
            if let Some(release) = &self.setup_system.release {
                ui.add_space(10.0);
                ui.hyperlink_to(format!("{} · 查看发布说明", release.name), &release.url);
                if !release.published_at.is_empty() {
                    ui.label(
                        RichText::new(&release.published_at)
                            .size(12.0)
                            .color(super::MUTED),
                    );
                }
                for asset in &release.assets {
                    if ui
                        .add_enabled(
                            !self.setup_system.loading && release.newer,
                            egui::Button::new(format!(
                                "下载 {}（{:.1} MiB）",
                                asset.name,
                                asset.size as f64 / 1_048_576.0
                            )),
                        )
                        .clicked()
                    {
                        download = Some(asset.clone());
                    }
                }
            } else {
                ui.hyperlink_to("本项目发布页面", system::RELEASES_PAGE);
            }
        });
        if let Some(action) = key_action {
            self.start_key_action(action);
        }
        if clear_cache {
            self.settings.system.cache_dir = None;
        }
        if pick_cache {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("选择更新包缓存文件夹")
                .pick_folder()
            {
                self.settings.system.cache_dir = Some(path);
            }
        }
        if self.settings != previous {
            match config::save_settings(&self.settings_path, &self.settings) {
                Ok(()) => {
                    if let Err(error) = pcl_core::network::configure(&self.settings.downloads) {
                        self.error = Some(format!("应用下载设置失败：{error:#}"));
                    }
                }
                Err(error) => {
                    self.settings = previous;
                    self.error = Some(format!("保存设置失败：{error:#}"));
                }
            }
        }
        if open_cache {
            let path = system::cache_directory(&self.settings.system);
            let result =
                std::fs::create_dir_all(&path).and_then(|()| crate::process::open_folder(&path));
            if let Err(error) = result {
                self.error = Some(format!("打开缓存文件夹失败：{error}"));
            }
        }
        if check {
            self.start_system_check(true);
        }
        if let Some(asset) = download {
            self.start_update_download(asset);
        }
    }
}
fn system_row(ui: &mut egui::Ui, label: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.add_sized([110.0, 28.0], egui::Label::new(label));
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 28.0),
            egui::Layout::left_to_right(egui::Align::Center),
            body,
        );
    });
}
fn source_combo(ui: &mut egui::Ui, id: &str, source: &mut SourcePreference) {
    ui_style::PclComboBox::from_id_salt(id)
        .width(ui.available_width())
        .selected_text(source_label(*source))
        .show_ui(ui, |ui| {
            for item in [
                SourcePreference::MirrorFirst,
                SourcePreference::OfficialFirst,
                SourcePreference::OfficialOnly,
            ] {
                ui.selectable_value(source, item, source_label(item));
            }
        });
}
fn source_label(value: SourcePreference) -> &'static str {
    match value {
        SourcePreference::MirrorFirst => "尽量使用镜像源（BMCLAPI）",
        SourcePreference::OfficialFirst => "优先使用官方源，在加载缓慢时换用镜像源",
        SourcePreference::OfficialOnly => "仅使用官方源",
    }
}
fn update_label(value: LauncherUpdateMode) -> &'static str {
    match value {
        LauncherUpdateMode::Download => "在有新版本时自动下载",
        LauncherUpdateMode::Notify => "在有新版本时显示提示",
        LauncherUpdateMode::Disabled => "关闭更新提示",
    }
}

fn check_was_cancelled<A, B>(first: &anyhow::Result<A>, second: &anyhow::Result<B>) -> bool {
    let mut errors = [first.as_ref().err(), second.as_ref().err()]
        .into_iter()
        .flatten()
        .peekable();
    errors.peek().is_some() && errors.all(|e| e.is::<pcl_core::model::OperationCancelled>())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_network_error_is_not_hidden_by_later_cancel() {
        let first: anyhow::Result<()> = Err(anyhow::anyhow!("fixture network failure"));
        let cancelled: anyhow::Result<()> = Err(pcl_core::model::OperationCancelled.into());
        assert!(!check_was_cancelled(&first, &cancelled));
        assert!(check_was_cancelled(&Ok(()), &cancelled));
        assert!(!check_was_cancelled(&Ok(()), &Ok(())));
    }
    #[test]
    fn finished_update_check_never_releases_an_unrelated_download_busy_gate() {
        let root = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(root.path());
        let (tx, rx) = mpsc::channel();
        app.setup_system.receiver = Some(rx);
        app.setup_system.loading = true;
        app.busy = Some("fixture download".into());
        tx.send(SystemMessage::Checked {
            launcher: Ok(None),
            game: Ok(None),
            cancelled: false,
        })
        .unwrap();
        app.system_tick(&egui::Context::default());
        assert_eq!(app.busy.as_deref(), Some("fixture download"));
        assert!(!app.setup_system.loading);
    }
}
