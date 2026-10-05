//! Per-version settings; geometry follows PageInstanceSetup.xaml (PCL 2.13.1.1).
use super::{version_ui::titled_card, Launcher, MUTED};
use crate::theme;
use crate::ui_style;
use eframe::egui::{self, Color32, RichText, Vec2};
use pcl_core::config::{self, InstanceSettings, LoginRequirement};
use pcl_core::java_selection::{
    self, JavaSelectionMode, JavaSelectionRequest, JavaSelectionResult,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct InstanceSetupState {
    target: Option<(PathBuf, String)>,
    value: InstanceSettings,
    java_text: String,
    error: Option<String>,
    load_failed: bool,
    advanced_open: bool,
    memory_updated: Option<Instant>,
    memory: Option<config::MemorySnapshot>,
    automatic_mb: Option<u32>,
    java_preview: JavaPreview,
}

#[derive(Default)]
struct JavaPreview {
    key: String,
    receiver: Option<mpsc::Receiver<Result<JavaSelectionResult, String>>>,
    cancel: Option<Arc<AtomicBool>>,
    message: Option<(String, Option<Color32>)>,
    details: String,
}
impl Drop for JavaPreview {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}
impl JavaPreview {
    fn update(
        &mut self,
        request: JavaSelectionRequest,
        candidates: &[pcl_core::java::JavaRuntime],
        ctx: &egui::Context,
    ) {
        let key = format!("{request:?}/{candidates:?}");
        if self.key != key {
            if let Some(cancel) = &self.cancel {
                cancel.store(true, Ordering::Relaxed);
            }
            self.key = key;
            self.message = None;
            let cancel = Arc::new(AtomicBool::new(false));
            self.cancel = Some(cancel.clone());
            let (sender, receiver) = mpsc::channel();
            self.receiver = Some(receiver);
            self.details.clear();
            let candidates = candidates.to_vec();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let result = java_selection::select_java(
                    &request,
                    &candidates,
                    &pcl_core::model::Platform::current(),
                    &cancel,
                )
                .map_err(|error| format!("{error:#}"));
                let _ = sender.send(result);
                ctx.request_repaint();
            });
        }
        if let Some(result) = self
            .receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            self.receiver = None;
            self.message = Some(match result {
                Ok(JavaSelectionResult::Selected {
                    runtime, warnings, ..
                }) => {
                    self.details = warnings.join("\n");
                    (
                        format!(
                            "将会使用：Java {} ({})",
                            runtime.version,
                            runtime.path.display()
                        ),
                        None,
                    )
                }
                Ok(JavaSelectionResult::NeedsDownload { requirement, .. }) => (
                    format!(
                        "没有找到符合 {} 的 Java。启动时可前往下载 Java。",
                        requirement.range
                    ),
                    Some(Color32::from_rgb(170, 110, 0)),
                ),
                Err(message) => (message, Some(Color32::from_rgb(205, 65, 65))),
            });
        }
    }
}

impl Launcher {
    pub(super) fn instance_setup_page(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.settings.selected_version.clone() else {
            return;
        };
        // PageInstanceSetup specifies each gap explicitly, like the global
        // settings page; egui's default spacing must not add another 8 DIP.
        ui.spacing_mut().item_spacing.y = 0.0;
        let root = self.settings.game_root.clone();
        let target = (root.clone(), id.clone());
        if self.instance_setup.target.as_ref() != Some(&target) {
            self.instance_setup = InstanceSetupState {
                target: Some(target),
                ..Default::default()
            };
            match config::load_instance_settings(&root, &id) {
                Ok(settings) => {
                    self.instance_setup.java_text = settings
                        .java_path
                        .as_ref()
                        .map_or_else(String::new, |path| path.display().to_string());
                    self.instance_setup.value = settings;
                }
                Err(error) => {
                    self.instance_setup.error = Some(format!("读取版本设置失败：{error:#}"));
                    self.instance_setup.load_failed = true;
                }
            }
        }
        let writable = self.game_pid.is_none() && !self.jobs.conflicts_with(&root);
        let state = &mut self.instance_setup;
        if state
            .memory_updated
            .is_none_or(|time| time.elapsed() >= Duration::from_secs(1))
        {
            state.memory_updated = Some(Instant::now());
            match config::memory_snapshot()
                .and_then(|memory| Ok((memory, config::automatic_memory_mb(&root, &id, memory)?)))
            {
                Ok((memory, allocated)) => {
                    state.memory = Some(memory);
                    state.automatic_mb = Some(allocated);
                }
                Err(error) => {
                    state.memory = None;
                    state.automatic_mb = None;
                    state.error = Some(format!("读取内存分配信息失败：{error:#}"));
                }
            }
        }
        let previous = state.value.clone();
        egui::Frame::new()
            .fill(theme::palette(ui.ctx()).light)
            .corner_radius(3)
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("这些设置只对该游戏版本生效，不影响其他版本。")
                        .color(theme::palette(ui.ctx()).accent),
                );
            });
        ui.add_space(15.0);
        ui.add_enabled_ui(self.busy.is_none() && writable && !state.load_failed, |ui| {
            titled_card(ui, "启动选项", |ui| {
                row(ui, "版本隔离", |ui| {
                    ui_style::PclComboBox::from_id_salt("instance-isolation").width(ui.available_width())
                        .selected_text(if state.value.isolated { "开启" } else { "关闭" }).show_ui(ui, |ui| {
                            ui.selectable_value(&mut state.value.isolated, true, "开启");
                            ui.selectable_value(&mut state.value.isolated, false, "关闭");
                        }).response.on_hover_text("开启时，各版本使用独立的存档和 Mod；关闭时，共享游戏根目录中的内容。切换不会移动或删除现有文件。");
                });
                ui.add_space(9.0);
                row(ui, "游戏窗口标题", |ui| {
                    text_edit(ui, &mut state.value.game_window_title, "跟随全局设置").on_hover_text(super::setup_launch_ui::WINDOW_HELP);
                });
                ui.add_space(9.0);
                row(ui, "自定义信息", |ui| {
                    text_edit(ui, &mut state.value.custom_info, "跟随全局设置");
                });
                ui.add_space(9.0);
                let mut mode = java_selection::effective_mode(state.value.java_mode, state.value.java_path.as_deref());
                row(ui, "Java", |ui| {
                    let has_range = mode == JavaSelectionMode::VersionRange;
                    let width = (ui.available_width() - if has_range { 207.0 } else { 0.0 }).max(80.0);
                    ui_style::PclComboBox::from_id_salt("instance-java-mode").width(width)
                        .selected_text(java_mode_label(mode))
                        .show_ui(ui, |ui| {
                            for value in [JavaSelectionMode::Automatic, JavaSelectionMode::VersionRange, JavaSelectionMode::VersionFolder, JavaSelectionMode::Specific] {
                                if ui.selectable_value(&mut mode, value, java_mode_label(value)).changed() {
                                    state.value.java_mode = Some(mode);
                                    if mode == JavaSelectionMode::VersionRange && state.value.java_range.is_empty() {
                                        match java_selection::resolve_requirement(&root, &id) {
                                            Ok(requirement) => state.value.java_range = requirement.range.to_string(),
                                            Err(error) => state.error = Some(format!("读取 Java 要求失败：{error:#}")),
                                        }
                                    }
                                }
                            }
                        });
                    if has_range {
                        ui.add_space(7.0);
                        ui.add_sized(Vec2::new(200.0, 28.0), egui::TextEdit::singleline(&mut state.value.java_range).hint_text("版本区间…").char_limit(100).margin(Vec2::new(6.0, 5.0)))
                            .on_hover_text("例如 [17.0.1,25.0)。方括号包含端点，圆括号不包含；留空的一侧表示不限制。Java 8u81 写作 8.0.81。");
                    }
                });
                if mode == JavaSelectionMode::Specific {
                    ui.add_space(9.0);
                    row(ui, "", |ui| {
                        let selected = state.value.java_path.as_ref().map_or_else(|| "选择 Java…".to_owned(), |path| path.display().to_string());
                        ui_style::PclComboBox::from_id_salt("instance-java-specific").width(ui.available_width())
                            .selected_text(selected).show_ui(ui, |ui| {
                                for runtime in &self.java {
                                    if ui.selectable_label(state.value.java_path.as_ref() == Some(&runtime.path), format!("Java {} · {}", runtime.version, runtime.path.display())).clicked() {
                                        state.value.java_path = Some(runtime.path.clone());
                                        state.java_text = runtime.path.display().to_string();
                                    }
                                }
                                if ui.selectable_label(false, "手动导入 Java…").clicked() {
                                    match super::java_ui::pick_java_path() {
                                        Some(Ok(file)) => { state.java_text = file.display().to_string(); state.value.java_path = Some(file); },
                                        Some(Err(error)) => state.error = Some(error),
                                        None => {},
                                    }
                                }
                            });
                    });
                }
                let mut priority = self.settings.java_priority.clone();
                if let Some(path) = &self.settings.java_path { priority.insert(0, path.clone()); }
                state.java_preview.update(JavaSelectionRequest {
                    root: root.clone(), version_id: id.clone(), mode,
                    version_range: state.value.java_range.clone(), specified_path: state.value.java_path.clone(),
                    priority, excluded: self.settings.java_excluded.clone(),
                }, &self.java, ui.ctx());
                if mode != JavaSelectionMode::Specific || state.java_preview.message.as_ref().is_some_and(|(_, color)| *color == Some(Color32::from_rgb(205, 65, 65))) {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.add_space(123.0);
                        egui::Frame::new().fill(theme::palette(ui.ctx()).light).corner_radius(3).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                            ui.set_max_width(ui.available_width());
                            let (message, color) = state.java_preview.message.as_ref().map(|(text, color)| (text.as_str(), color.unwrap_or_else(|| theme::palette(ui.ctx()).accent))).unwrap_or(("正在查找 Java……", theme::palette(ui.ctx()).accent));
                            let response = ui.add(egui::Label::new(RichText::new(message).color(color)).wrap().sense(egui::Sense::click()));
                            let response = if state.java_preview.details.is_empty() { response } else { response.on_hover_text(&state.java_preview.details) };
                            if mode == JavaSelectionMode::VersionFolder && response.clicked() {
                                match pcl_core::metadata::confined_path(&root, &std::path::Path::new("versions").join(&id)).and_then(|folder| Ok(crate::process::open_folder(&folder)?)) {
                                    Ok(()) => {}, Err(error) => state.error = Some(format!("打开版本文件夹失败：{error:#}")),
                                }
                            }
                        });
                    });
                }

            });
            titled_card(ui, "内存分配", |ui| {
                ui.spacing_mut().interact_size.y = 22.0;
                if super::setup_launch_ui::radio_row(ui, !state.value.memory_auto && state.value.memory_mb.is_none(), "跟随全局设置").clicked() {
                    state.value.memory_mb = None;
                    state.value.memory_auto = false;
                }
                ui.add_space(9.0);
                if super::setup_launch_ui::radio_row(ui, state.value.memory_auto, "自动配置").on_hover_text("根据安装的 Mod 量与电脑剩余内存动态调整为游戏分配的内存。").clicked() {
                    state.value.memory_auto = true;
                    state.value.memory_mb = None;
                }
                ui.add_space(9.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if super::setup_launch_ui::radio_row(ui, !state.value.memory_auto && state.value.memory_mb.is_some(), "自定义").clicked() {
                        state.value.memory_mb = Some(self.settings.memory_mb);
                        state.value.memory_auto = false;
                    }
                    ui.add_space(20.0);
                    let memory = state.value.memory_mb.unwrap_or(self.settings.memory_mb);
                    let maximum = state.memory.map_or(49, |memory| {
                        let gb = memory.total_mb as f64 / 1024.0;
                        if gb <= 1.5 { ((gb - 0.3) / 0.1).floor().max(1.0) as u32 }
                        else if gb <= 8.0 { ((gb - 1.5) / 0.5).floor() as u32 + 12 }
                        else if gb <= 16.0 { (gb - 8.0).floor() as u32 + 25 }
                        else { ((gb - 16.0) / 2.0).floor() as u32 + 33 }
                    }).min(153);
                    let mut value = (0..=maximum).min_by_key(|index| slider_memory(*index).abs_diff(memory)).unwrap_or(0);
                    let enabled = !state.value.memory_auto && state.value.memory_mb.is_some();
                    if memory_slider(ui, &mut value, maximum, enabled).changed() { state.value.memory_mb = Some(slider_memory(value)); }
                });
                ui.add_space(9.0);
                row(ui,"启动器内存回收",|ui| {
                    let options=[(None,"跟随全局设置"),(Some(true),"启动前回收启动器自身内存"),(Some(false),"关闭")];
                    let label=options.iter().find(|(v,_)|*v==state.value.memory_optimize).unwrap().1;
                    ui_style::PclComboBox::from_id_salt("instance-memory-reclaim").width(ui.available_width()).selected_text(label).show_ui(ui,|ui|{for (value,label) in options {ui.selectable_value(&mut state.value.memory_optimize,value,label);}}).response.on_hover_text(super::setup_launch_ui::MEMORY_HELP);
                });
                ui.add_space(11.0);
                let allocated = if state.value.memory_auto || (state.value.memory_mb.is_none() && self.settings.memory_auto) { state.automatic_mb } else { Some(state.value.memory_mb.unwrap_or(self.settings.memory_mb)) };
                if let (Some(memory), Some(allocated)) = (state.memory, allocated) { memory_bar(ui, memory, allocated); }
                else { ui.label(RichText::new("暂时无法读取内存信息").color(MUTED)); }
            });
            titled_card(ui, "服务器", |ui| {
                row(ui, "登录方式", |ui| {
                    let label = login_label(state.value.login_requirement);
                    ui_style::PclComboBox::from_id_salt("instance-login-mode").width(ui.available_width()).selected_text(label).show_ui(ui, |ui| {
                        for value in [LoginRequirement::Any, LoginRequirement::Microsoft, LoginRequirement::Offline] {
                            ui.selectable_value(&mut state.value.login_requirement, value, login_label(value));
                        }
                    });
                });
                ui.add_space(9.0);
                row(ui, "自动进入服务器", |ui| {
                    text_edit(ui, &mut state.value.server, "").on_hover_text("主机名或 IP，可附加 :端口。IPv6 使用 [地址]:端口 格式。");
                });
            });
            super::setup_launch_ui::setup_card(ui, "高级选项", 15, Some(&mut state.advanced_open), |ui| {
                    row(ui, "Java 虚拟机参数", |ui| { text_edit(ui, &mut state.value.jvm_arguments, "跟随全局设置"); });
                    ui.add_space(9.0);
                    row(ui, "游戏参数", |ui| { text_edit(ui, &mut state.value.game_arguments, "跟随全局设置"); });
                    ui.add_space(9.0);
                    row(ui, "启动前执行命令", |ui| {
                        text_edit(ui, &mut state.value.pre_launch_command, "")
                            .on_hover_text(super::setup_launch_ui::PRE_LAUNCH_HELP);
                    });
                    if !state.value.pre_launch_command.trim().is_empty() {
                        ui.add_space(5.0);
                        row(ui, "", |ui| { ui.checkbox(&mut state.value.pre_launch_wait, "等待命令执行完成后再继续启动"); });
                    }
                    ui.add_space(9.0);
                    ui.checkbox(&mut state.value.disable_mod_updates,"禁用此版本的 Mod 更新");
                    ui.checkbox(&mut state.value.disable_java_wrapper,"禁用 Java Launch Wrapper").on_hover_text(super::setup_launch_ui::JLW_HELP);
                    ui.checkbox(&mut state.value.disable_lwjgl_unsafe_agent,"禁用 LWJGL Unsafe Agent").on_hover_text(super::setup_launch_ui::LUA_HELP);
                    ui.add_space(9.0);
                    row(ui, "垃圾回收器", |ui| {
                        let choices = [
                            (None, "跟随全局设置"),
                            (Some(config::GcMode::PreferZgc), "尽量使用 ZGC"),
                            (Some(config::GcMode::PreferGenerationalZgc), "尽量使用分代 ZGC"),
                            (Some(config::GcMode::G1), "标准 G1GC"),
                            (Some(config::GcMode::TunedG1), "调优 G1GC"),
                            (Some(config::GcMode::Custom), "不指定（可自定义）"),
                        ];
                        let label = choices.iter().find(|(value, _)| *value == state.value.gc_mode).unwrap().1;
                        ui_style::PclComboBox::from_id_salt("instance-gc-mode").width(ui.available_width()).selected_text(label).show_ui(ui, |ui| {
                            for (value, label) in choices { ui.selectable_value(&mut state.value.gc_mode, value, label); }
                        });
                    });
            });
        });
        if state.value != previous && !state.load_failed && writable {
            match config::save_instance_settings(&root, &id, &state.value) {
                Ok(()) => {
                    state.error = None;
                    self.status = "版本设置已保存".into();
                }
                Err(error) => state.error = Some(format!("设置未保存：{error:#}")),
            }
        }
        if let Some(error) = &state.error {
            ui.add_space(10.0);
            ui.colored_label(Color32::from_rgb(205, 65, 65), error);
        }
        ui.add_space(15.0);
        let (reset_rect, _) = ui.allocate_exact_size(Vec2::new(140.0, 35.0), egui::Sense::hover());
        if ui_style::outline_button(ui, reset_rect, "初始化版本设置", None, false, writable)
            .clicked()
        {
            self.confirm_instance_reset(ui.ctx(), &id);
        }
        let (restore_rect, _) =
            ui.allocate_exact_size(Vec2::new(140.0, 35.0), egui::Sense::hover());
        if ui_style::outline_button(ui, restore_rect, "恢复版本设置", None, false, writable)
            .clicked()
        {
            self.restore_instance_preferences(&id);
        }
        self.version_management_dialog(ui.ctx());
        let (rect, _) = ui.allocate_exact_size(Vec2::new(140.0, 35.0), egui::Sense::hover());
        if ui_style::outline_button(ui, rect, "全局设置", None, false, true).clicked() {
            self.page = super::Page::Settings;
            self.settings_tab = 0;
        }
    }
}

fn java_mode_label(mode: JavaSelectionMode) -> &'static str {
    match mode {
        JavaSelectionMode::Automatic => "自动选择",
        JavaSelectionMode::VersionRange => "自动选择指定版本的 Java",
        JavaSelectionMode::VersionFolder => "使用版本文件夹中的 Java",
        JavaSelectionMode::Specific => "使用指定的 Java",
    }
}

fn login_label(requirement: LoginRequirement) -> &'static str {
    match requirement {
        LoginRequirement::Any => "正版登录或离线登录",
        LoginRequirement::Microsoft => "仅正版登录",
        LoginRequirement::Offline => "仅离线登录",
    }
}

pub(super) fn slider_memory(index: u32) -> u32 {
    let gb = if index <= 12 {
        f64::from(index) * 0.1 + 0.3
    } else if index <= 25 {
        f64::from(index - 12) * 0.5 + 1.5
    } else if index <= 33 {
        f64::from(index - 25) + 8.0
    } else {
        f64::from(index - 33) * 2.0 + 16.0
    };
    (gb * 1024.0).round() as u32
}

pub(super) fn memory_slider(
    ui: &mut egui::Ui,
    value: &mut u32,
    maximum: u32,
    enabled: bool,
) -> egui::Response {
    let before = *value;
    let (rect, mut response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 16.0),
        if enabled {
            egui::Sense::click_and_drag()
        } else {
            egui::Sense::hover()
        },
    );
    let track = rect.shrink2(Vec2::new(5.0, 0.0));
    if enabled {
        if let Some(pointer) = response.interact_pointer_pos() {
            *value = (((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0) * maximum as f32)
                .round() as u32;
            response.request_focus();
        }
        if response.has_focus() {
            if ui.input(|input| input.key_pressed(egui::Key::ArrowRight)) {
                *value = value.saturating_add(1).min(maximum);
            }
            if ui.input(|input| input.key_pressed(egui::Key::ArrowLeft)) {
                *value = value.saturating_sub(1);
            }
        }
    }
    if before != *value {
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::slider(enabled, f64::from(slider_memory(*value)), "游戏内存 MB")
    });
    let center = egui::pos2(
        track.left() + track.width() * *value as f32 / maximum.max(1) as f32,
        track.center().y,
    );
    let accent = if enabled {
        theme::palette(ui.ctx()).accent
    } else {
        Color32::from_gray(190)
    };
    ui.painter().line_segment(
        [track.left_center(), track.right_center()],
        egui::Stroke::new(2.0_f32, Color32::from_gray(225)),
    );
    ui.painter().line_segment(
        [track.left_center(), center],
        egui::Stroke::new(2.0_f32, accent),
    );
    ui.painter().circle_filled(
        center,
        if response.hovered() && enabled {
            6.0
        } else {
            5.0
        },
        if enabled {
            theme::palette(ui.ctx()).control_border
        } else {
            Color32::from_gray(190)
        },
    );
    response
}

pub(super) fn memory_bar(ui: &mut egui::Ui, memory: config::MemorySnapshot, allocated: u32) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 49.0), egui::Sense::hover());
    let used = memory.total_mb.saturating_sub(memory.available_mb);
    let used_width = rect.width() * used as f32 / memory.total_mb as f32;
    let game_width = rect.width() * u64::from(allocated).min(memory.available_mb) as f32
        / memory.total_mb as f32;
    let bar = egui::Rect::from_min_size(
        rect.min + Vec2::new(0.0, 19.0),
        Vec2::new(rect.width(), 4.0),
    );
    ui.painter()
        .rect_filled(bar, 0, theme::palette(ui.ctx()).pale);
    ui.painter().rect_filled(
        egui::Rect::from_min_size(bar.min, Vec2::new(used_width, 4.0)),
        0,
        theme::palette(ui.ctx()).accent,
    );
    ui.painter().rect_filled(
        egui::Rect::from_min_size(
            bar.min + Vec2::new(used_width, 0.0),
            Vec2::new(game_width, 4.0),
        ),
        0,
        theme::palette(ui.ctx()).hover,
    );
    let game_x = used_width.max(155.0).min((rect.width() - 100.0).max(0.0));
    for (offset, text, size, color) in [
        (Vec2::new(2.0, 0.0), "已使用内存".into(), 11.0, MUTED),
        (Vec2::new(game_x, 0.0), "游戏分配".into(), 11.0, MUTED),
        (
            Vec2::new(2.0, 26.0),
            format!(
                "{:.1} / {:.1} GB",
                used as f64 / 1024.0,
                memory.total_mb as f64 / 1024.0
            ),
            16.0,
            Color32::BLACK,
        ),
        (
            Vec2::new(game_x, 26.0),
            format!("{:.1} GB", f64::from(allocated) / 1024.0),
            16.0,
            Color32::BLACK,
        ),
    ] {
        ui.painter().text(
            rect.min + offset,
            egui::Align2::LEFT_TOP,
            text,
            egui::FontId::proportional(size),
            color,
        );
    }
}

pub(super) fn row(ui: &mut egui::Ui, label: &str, contents: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(123.0, 28.0), egui::Sense::hover());
        ui_style::place_left(ui, rect, egui::Label::new(label));
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 28.0),
            egui::Layout::left_to_right(egui::Align::Center),
            contents,
        );
    });
}

pub(super) fn text_edit(ui: &mut egui::Ui, value: &mut String, hint: &str) -> egui::Response {
    ui.add_sized(
        Vec2::new(ui.available_width(), 28.0),
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .margin(Vec2::new(6.0, 5.0)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect};

    #[test]
    fn instance_rows_keep_source_spacing_and_memory_choices_share_the_control_column() {
        let temporary = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(temporary.path());
        let root = app.settings.game_root.clone();
        app.settings.selected_version = Some("fixture".into());
        app.instance_setup.target = Some((root.clone(), "fixture".into()));
        app.instance_setup.memory_updated = Some(Instant::now());
        app.instance_setup.memory = Some(config::MemorySnapshot {
            total_mb: 8192,
            available_mb: 4096,
        });
        app.instance_setup.automatic_mb = Some(2048);
        app.instance_setup.advanced_open = true;
        let request = JavaSelectionRequest {
            root,
            version_id: "fixture".into(),
            mode: JavaSelectionMode::Automatic,
            version_range: String::new(),
            specified_path: None,
            priority: Vec::new(),
            excluded: Vec::new(),
        };
        app.instance_setup.java_preview.key = format!("{request:?}/{:?}", app.java);
        app.instance_setup.java_preview.message = Some(("Fixture Java".into(), None));
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        fonts.families.insert(
            egui::FontFamily::Name("PCL Bold".into()),
            fonts.families[&egui::FontFamily::Proportional].clone(),
        );
        ctx.set_fonts(fonts);
        ctx.style_mut(|style| {
            style.spacing.item_spacing = Vec2::new(10.0, 8.0);
            style.spacing.interact_size.y = 28.0;
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
        });
        for size in [Vec2::new(989.0, 517.0), Vec2::new(810.0, 470.0)] {
            let mut draw = |offset, events| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::TopBottomPanel::top("source-title")
                            .exact_height(48.0)
                            .frame(egui::Frame::NONE)
                            .show(ctx, |_| {});
                        egui::SidePanel::left("source-sidebar")
                            .exact_width(138.0)
                            .frame(egui::Frame::NONE)
                            .show(ctx, |_| {});
                        egui::CentralPanel::default()
                            .frame(egui::Frame::NONE)
                            .show(ctx, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt(size.x.to_bits())
                                    .auto_shrink([false, false])
                                    .vertical_scroll_offset(offset)
                                    .show(ui, |ui| {
                                        egui::Frame::NONE
                                            .inner_margin(25)
                                            .show(ui, |ui| app.instance_setup_page(ui));
                                    });
                            });
                    },
                )
            };
            let text_rect = |output: &egui::FullOutput, title, minimum_y| {
                output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text)
                            if text.galley.text() == title && text.pos.y >= minimum_y =>
                        {
                            Some(Rect::from_min_size(text.pos, text.galley.size()))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("missing {title} at {size:?}"))
            };
            let output = draw(0.0, vec![]);
            let isolation = text_rect(&output, "版本隔离", 0.0);
            let title = text_rect(&output, "游戏窗口标题", 0.0);
            assert_eq!(
                title.top() - isolation.top(),
                37.0,
                "28 DIP fields plus a single 9 DIP gap"
            );
            let output = draw(300.0, vec![]);
            let memory_top = text_rect(&output, "内存分配", 0.0).bottom();
            let choices = ["跟随全局设置", "自动配置", "自定义"]
                .map(|title| text_rect(&output, title, memory_top));
            assert_eq!(choices[0].left(), choices[1].left());
            assert_eq!(choices[1].left(), choices[2].left());
            assert_eq!(choices[1].center().y - choices[0].center().y, 31.0);
            assert_eq!(choices[2].center().y - choices[1].center().y, 31.0);
            let memory_row = choices[2].center().y;
            let slider_left = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::LineSegment { points, .. }
                        if (points[0].y - memory_row).abs() < 0.1
                            && points[1].x - points[0].x > 100.0 =>
                    {
                        Some(points[0].x)
                    }
                    _ => None,
                })
                .unwrap();
            assert_eq!(slider_left, 138.0 + 25.0 + 25.0 + 110.0 + 20.0 + 5.0);
            let point = choices[2].center();
            let pointer = |pressed| egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            let _ = draw(300.0, vec![egui::Event::PointerMoved(point), pointer(true)]);
            let _ = draw(300.0, vec![pointer(false)]);
            let advanced = draw(600.0, vec![]);
            let heading = text_rect(&advanced, "高级选项", 0.0);
            let first_field = text_rect(&advanced, "Java 虚拟机参数", heading.bottom());
            assert_eq!(
                first_field.center().y - heading.center().y,
                34.0,
                "one source card header, without a second empty header"
            );
        }

        assert!(app.instance_setup.value.memory_mb.is_some());
        assert!(!app.instance_setup.value.memory_auto);
        assert!(
            app.instance_setup.java_preview.receiver.is_none(),
            "fixture must not spawn Java probing"
        );
    }
}
