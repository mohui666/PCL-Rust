//! Global settings geometry follows upstream PageSetupLaunch.xaml (PCL 2.13.1.1).
use super::{
    instance_setup_ui::{memory_bar, memory_slider, slider_memory, text_edit},
    java_ui, Launcher, Page, MUTED,
};
use crate::theme;
use crate::ui_style;
use eframe::egui::{self, Color32, Rect, RichText, Vec2};
use pcl_core::config::{
    self, GcMode, IsolationPolicy, LauncherVisibility, ProcessPriority, WindowMode,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct SetupLaunchState {
    initialized: bool,
    advanced_open: bool,
    skin_open: bool,
    extras_open: bool,
    width: String,
    height: String,
    error: Option<String>,
    dimension_error: Option<String>,
    memory_updated: Option<Instant>,
    memory: Option<config::MemorySnapshot>,
    automatic_mb: Option<u32>,
}
enum JavaAction {
    Pick,
    Refresh,
    Download,
    Move(usize, isize),
    Exclude(PathBuf),
    Restore(PathBuf),
    Explain,
    Open(PathBuf),
}

impl Launcher {
    pub(super) fn open_java_settings(&mut self) {
        self.page = Page::Settings;
        self.settings_tab = 0;
        self.setup_launch.advanced_open = true;
    }
    pub(super) fn open_client_id_settings(&mut self) {
        self.open_java_settings();
        self.setup_launch.extras_open = true;
    }
    pub(super) fn launch_settings_page(&mut self, ui: &mut egui::Ui) {
        let state = &mut self.setup_launch;
        if !state.initialized {
            state.initialized = true;
            state.width = self.settings.width.to_string();
            state.height = self.settings.height.to_string();
        }
        if state
            .memory_updated
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
        {
            state.memory_updated = Some(Instant::now());
            match config::memory_snapshot() {
                Ok(memory) => {
                    state.automatic_mb = match self.settings.selected_version.as_ref() {
                        Some(id) => {
                            config::automatic_memory_mb(&self.settings.game_root, id, memory).ok()
                        }
                        None => Some(config::auto_memory_from_inputs(
                            memory.available_mb,
                            false,
                            false,
                            0,
                        )),
                    };
                    state.memory = Some(memory);
                }
                Err(_) => {
                    state.memory = None;
                    state.automatic_mb = None;
                }
            }
        }
        let previous = serde_json::to_value(&self.settings).ok();
        let mut java_action = None;
        let mut pick_root = false;
        let mut open_root = false;
        let mut save_paths = false;
        let mut preview = false;
        let mut version_settings = false;
        let enabled = self.busy.is_none() && self.game_pid.is_none();
        let settings = &mut self.settings;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.spacing_mut().interact_size.y = 28.0;
            ui.add_enabled_ui(enabled,|ui| {
                setup_card(ui,"启动选项",25,None,|ui| {
                    argument_row(ui,"默认版本隔离",|ui| {
                        ui_style::PclComboBox::from_id_salt("global-isolation").width(ui.available_width()).selected_text(isolation_label(settings.default_isolation)).show_ui(ui,|ui|{
                            for value in [IsolationPolicy::Off,IsolationPolicy::Modded,IsolationPolicy::NonRelease,IsolationPolicy::ModdedOrNonRelease,IsolationPolicy::All] {
                                ui.selectable_value(&mut settings.default_isolation,value,isolation_label(value));
                            }
                        }).response.on_hover_text("当安装新版本时，据此自动设置新版本的隔离选项。若想调整已有版本，请前往它的版本设置。");
                    });
                    ui.add_space(9.0);
                    argument_row(ui,"游戏窗口标题",|ui| { unavailable_text(ui,"默认","游戏窗口标题修改尚未迁移。"); });
                    ui.add_space(9.0);
                    argument_row(ui,"自定义信息",|ui| {
                        text_edit(ui,&mut settings.custom_info,"默认").on_hover_text("在支持此选项的游戏中显示于主界面与 F3 信息。Minecraft 26.1 起不再显示这项自定义信息。");
                    });
                    ui.add_space(9.0);
                    argument_row(ui,"启动器可见性",|ui| {
                        ui_style::PclComboBox::from_id_salt("launcher-visibility").width(ui.available_width()).selected_text(visibility_label(settings.launcher_visibility)).show_ui(ui,|ui| {
                            for value in [LauncherVisibility::CloseOnLaunch,LauncherVisibility::HideThenClose,LauncherVisibility::HideThenRestore,LauncherVisibility::Minimize,LauncherVisibility::Keep] {
                                ui.selectable_value(&mut settings.launcher_visibility,value,visibility_label(value));
                            }
                        }).response.on_hover_text("检测到游戏加载完成后执行。隐藏后若游戏异常退出或由启动器关闭，会重新显示启动器。");
                    });
                    ui.add_space(9.0);
                    argument_row(ui,"进程优先级",|ui| {
                        ui_style::PclComboBox::from_id_salt("process-priority").width(ui.available_width()).selected_text(priority_label(settings.process_priority)).show_ui(ui,|ui| {
                            for value in [ProcessPriority::High,ProcessPriority::Normal,ProcessPriority::Low] { ui.selectable_value(&mut settings.process_priority,value,priority_label(value)); }
                        }).response.on_hover_text("只调整本次启动的游戏进程。macOS/Linux提高优先级可能被系统拒绝，届时会提示并继续使用允许的优先级，不申请提权。");
                    });
                    ui.add_space(9.0);
                    argument_row(ui,"窗口大小",|ui| {
                        let custom = settings.window_mode == WindowMode::Custom;
                        let width = if custom { (ui.available_width()-177.0).max(110.0) } else {ui.available_width()};
                        ui_style::PclComboBox::from_id_salt("global-window").width(width).selected_text(window_label(settings.window_mode)).show_ui(ui,|ui| {
                            for mode in [WindowMode::Fullscreen,WindowMode::Default,WindowMode::LauncherSize,WindowMode::Custom,WindowMode::Maximized] {
                                let available = !matches!(mode,WindowMode::Maximized);
                                let response = ui.selectable_label_enabled(available,settings.window_mode==mode,window_label(mode));
                                if response.clicked(){settings.window_mode=mode;}
                                if !available {response.on_hover_text("跨平台游戏窗口控制尚未迁移。");}
                            }
                        });
                        if custom {
                            ui.add_space(30.0);
                            let a=ui.add_sized([60.0,28.0],egui::TextEdit::singleline(&mut state.width).horizontal_align(egui::Align::Center).char_limit(5));
                            ui.add(egui::Label::new(RichText::new(" × ").size(18.0)));
                            let b=ui.add_sized([60.0,28.0],egui::TextEdit::singleline(&mut state.height).horizontal_align(egui::Align::Center).char_limit(5));
                            if a.changed()||b.changed() {
                                match dimensions(&state.width,&state.height) {
                                    Ok((width,height))=>{settings.width=width;settings.height=height;state.dimension_error=None;}
                                    Err(error)=>state.dimension_error=Some(error.into()),
                                }
                            }
                        }
                    });
                    ui.add_space(8.0);
                    disabled_checkbox(ui,"在正版登录时验证 SSL 证书",true,"当前实现始终验证官方服务的 TLS 证书，不能关闭。",22.0);
                    ui.add_space(4.0);
                });
                setup_card(ui,"内存分配",15,None,|ui| {
                    if radio_row(ui,settings.memory_auto,"自动配置").clicked(){settings.memory_auto=true;}
                    ui.add_space(9.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x=0.0;
                        if radio_row(ui,!settings.memory_auto,"自定义").clicked(){settings.memory_auto=false;}
                        ui.add_space(20.0);
                        let maximum = memory_slider_maximum(state.memory);
                        let mut value=(0..=maximum).min_by_key(|index|slider_memory(*index).abs_diff(settings.memory_mb)).unwrap_or(0);
                        if memory_slider(ui,&mut value,maximum,!settings.memory_auto).changed(){settings.memory_mb=slider_memory(value).clamp(512,65536);}
                    });
                    ui.add_space(9.0);
                    disabled_checkbox(ui,"启动游戏前进行内存优化",false,"操作系统内存优化尚未迁移；不会清空其他程序的工作集。",22.0);
                    ui.add_space(14.0);
                    let allocation=if settings.memory_auto {state.automatic_mb} else {Some(settings.memory_mb)};
                    if let (Some(memory),Some(allocated))=(state.memory,allocation){memory_bar(ui,memory,allocated);}
                    else{ui.label(RichText::new("暂时无法读取内存分配信息").color(MUTED));}
                });
                setup_card(ui,"离线皮肤",15,Some(&mut state.skin_open),|ui| {
                    hint(ui,"离线皮肤替换尚未迁移；当前由游戏决定离线角色的默认皮肤。",true);
                    ui.add_space(10.0);
                    ui.columns(5,|columns| {
                        for (column,label) in columns.iter_mut().zip(["随机","Steve","Alex","正版皮肤","自定义"]) {
                            column.add_enabled(false,egui::RadioButton::new(false,label)).on_hover_text("此离线皮肤模式尚未迁移。");
                        }
                    });
                });
                setup_card(ui,"高级选项",15,Some(&mut state.advanced_open),|ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x=0.0;
                        let label_width=advanced_label_width(ui);
                        let (r,_) = ui.allocate_exact_size(Vec2::new(label_width,100.0),egui::Sense::hover());
                        ui_style::place_left(ui,Rect::from_min_size(r.min+Vec2::new(0.0,36.0),Vec2::new(label_width-48.0,28.0)),egui::Label::new("Java 虚拟机参数"));
                        if ui.place(Rect::from_min_size(r.min+Vec2::new(label_width-48.0,38.0),Vec2::splat(23.0)),egui::Button::new("↶").frame(false)).on_hover_text("还原 Java 虚拟机参数").clicked(){settings.jvm_arguments.clear();}
                        ui.add_sized([ui.available_width(),100.0],egui::TextEdit::multiline(&mut settings.jvm_arguments).char_limit(4000).margin(Vec2::new(6.0,5.0))).on_hover_text("启动时追加的 JVM 参数。使用引号将含空格的单个参数括起。");
                    });
                    ui.add_space(9.0);
                    advanced_row(ui,"游戏参数",|ui|{text_edit(ui,&mut settings.game_arguments,"");});
                    ui.add_space(9.0);
                    advanced_row(ui,"启动前执行命令",|ui|{text_edit(ui,&mut settings.pre_launch_command,"").on_hover_text(PRE_LAUNCH_HELP);});
                    if !settings.pre_launch_command.trim().is_empty() {
                        ui.add_space(8.0);
                        advanced_row(ui,"",|ui|{ui.checkbox(&mut settings.pre_launch_wait,"等待命令执行完成后再继续启动");});
                    }
                    ui.add_space(9.0);
                    advanced_row(ui,"Java 列表",|ui| {
                        let detecting=self.java_download.is_detecting();
                        let current=if detecting {"搜索中…".to_owned()} else if self.java.is_empty() {"未找到 Java，点击以导入已有的 Java".to_owned()} else {format!("共有 {} 个 Java…",self.java.len())};
                        ui.add_enabled_ui(!detecting,|ui| {
                            ui_style::PclComboBox::from_id_salt("global-java-list").width((ui.available_width()-29.0).max(100.0)).selected_text(current).show_ui(ui,|ui|{
                                for (index,runtime) in self.java.iter().enumerate() {
                                    if let Some(action)=java_list_row(ui,runtime,index,self.java.len()) {java_action=Some(action);}
                                }
                                if !self.java.is_empty(){ui.separator();}
                                if ui.button("导入电脑中已有的 Java…").clicked(){java_action=Some(JavaAction::Pick);ui.close();}
                                if ui.button("下载 Java…").clicked(){java_action=Some(JavaAction::Download);ui.close();}
                                if !settings.java_excluded.is_empty() {
                                    egui::CollapsingHeader::new(format!("已移除的 Java（{}）",settings.java_excluded.len())).show(ui,|ui| {
                                        for path in &settings.java_excluded {
                                            if ui.button(format!("恢复：{}",path.display())).clicked(){java_action=Some(JavaAction::Restore(path.clone()));ui.close();}
                                        }
                                    });
                                }
                            }).response.on_hover_text("启动时选择列表中第一个兼容当前 Minecraft 的 Java。点击右侧箭头调整优先顺序。移除只排除自动候选，不删除文件；实例指定 Java 需在版本设置中修改。");
                            if ui.button("↻").on_hover_text("重新搜索 Java；保留已排序项目及移除名单").clicked(){java_action=Some(JavaAction::Refresh);}
                        });
                    });
                    ui.add_space(9.0);
                    advanced_row(ui,"内存管理",|ui| {
                        ui_style::PclComboBox::from_id_salt("global-gc").width(ui.available_width()).selected_text(gc_label(settings.gc_mode)).show_ui(ui,|ui|{
                            for mode in [GcMode::PreferZgc,GcMode::PreferGenerationalZgc,GcMode::G1,GcMode::TunedG1,GcMode::Custom] {ui.selectable_value(&mut settings.gc_mode,mode,gc_label(mode));}
                        });
                    });
                    ui.add_space(12.0);
                    disabled_checkbox(ui,"禁用 Java Launch Wrapper",true,"Java Launch Wrapper 尚未接入，当前不会注入。",28.0);
                    disabled_checkbox(ui,"禁用 LWJGL Unsafe Agent",true,"LWJGL Unsafe Agent 尚未接入，当前不会注入。",28.0);
                    disabled_checkbox(ui,"使用高性能显卡",false,"Windows 显卡偏好设置尚未迁移。",28.0);
                    ui.add_space(9.0);
                    hint(ui,"版本独立设置中还有更多高级选项可供调整。",false);
                    ui.add_space(10.0);
                    let extra = egui::CollapsingHeader::new("游戏目录与正版登录").id_salt("launch-extra-settings").open(Some(state.extras_open)).show(ui,|ui| {
                        advanced_row(ui,"游戏目录",|ui| {
                            ui.add_sized([(ui.available_width()-60.0).max(100.0),28.0],egui::TextEdit::singleline(&mut self.root_text));
                            if ui.button("浏览").clicked(){pick_root=true;}
                        });
                        ui.add_space(9.0);
                        advanced_row(ui,"客户端 ID",|ui|{text_edit(ui,&mut settings.microsoft_client_id,"公共客户端应用 ID");});
                        ui.add_space(9.0);
                        ui.label(RichText::new("账号凭据通过系统安全存储保存，可在下次启动时恢复登录。").size(12.0).color(MUTED));
                        ui.add_space(9.0);
                        ui.horizontal(|ui| {
                            if ui.button("应用游戏目录").clicked(){save_paths=true;}
                            if ui.button("打开目录").clicked(){open_root=true;}
                            if ui.button("检查启动参数").clicked(){preview=true;}
                        });
                    });
                    if extra.header_response.clicked(){state.extras_open = !state.extras_open;}
                });
                if let Some(error)=state.dimension_error.as_ref().or(state.error.as_ref()) {
                    ui.colored_label(Color32::from_rgb(194,70,60),error);
                    ui.add_space(15.0);
                }
                if settings.selected_version.is_some() {
                    ui.vertical_centered(|ui| {
                        let (rect,_)=ui.allocate_exact_size(Vec2::new(150.0,35.0),egui::Sense::hover());
                        if ui_style::outline_button(ui,rect,"版本独立设置  →",None,false,true).clicked(){version_settings=true;}
                    });
                    ui.add_space(15.0);
                }
            });
        });
        if let Some(action) = java_action {
            match action {
                JavaAction::Refresh => self.detect_java(),
                JavaAction::Download => self.open_java_downloads(),
                JavaAction::Move(index, offset) => self.move_java_priority(index, offset),
                JavaAction::Exclude(path) => self.exclude_java(&path),
                JavaAction::Restore(path) => self.restore_excluded_java(&path),
                JavaAction::Explain => {
                    self.status = "点击选项右侧的箭头可以调整 Java 优先顺序。".into()
                }
                JavaAction::Pick => self.pick_global_java(),
                JavaAction::Open(path) => {
                    if let Some(parent) = path.parent() {
                        if let Err(error) = crate::process::open_folder(parent) {
                            self.error = Some(format!("打开 Java 目录失败：{error}"));
                        }
                    }
                }
            }
        }
        if pick_root {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("选择 Minecraft 根目录")
                .pick_folder()
            {
                self.root_text = path.display().to_string();
            }
        }
        let mut saved_paths = false;
        if save_paths {
            let mut next = self.settings.clone();
            next.game_root = PathBuf::from(self.root_text.trim());
            match config::save_settings(&self.settings_path, &next) {
                Ok(()) => {
                    let changed = self.settings.game_root != next.game_root;
                    self.settings = next;
                    if changed {
                        self.invalidate_root_views();
                        self.refresh_versions();
                    }
                    self.setup_launch.error = None;
                    self.status = "启动设置已保存".into();
                    saved_paths = true;
                }
                Err(error) => self.setup_launch.error = Some(format!("游戏目录未应用：{error:#}")),
            }
        }
        if !saved_paths && previous != serde_json::to_value(&self.settings).ok() {
            match config::save_settings(&self.settings_path, &self.settings) {
                Ok(()) => {
                    self.setup_launch.error = None;
                    self.status = "启动设置已保存".into();
                }
                Err(error) => self.setup_launch.error = Some(format!("设置未保存：{error:#}")),
            }
        }
        if open_root {
            self.open_root();
        }
        if preview {
            self.launch(true);
            self.show_logs = true;
        }
        if version_settings {
            self.page = Page::Launch;
            self.version_view = false;
            self.version_tools = true;
            self.tools_tab = 2;
        }
    }
    fn pick_global_java(&mut self) {
        let selected = java_ui::pick_java_path();
        match selected {
            Some(Ok(path)) => {
                self.import_global_java(path);
            }
            Some(Err(error)) => self.setup_launch.error = Some(error),
            None => (),
        }
    }
}

fn java_list_row(
    ui: &mut egui::Ui,
    runtime: &pcl_core::java::JavaRuntime,
    index: usize,
    count: usize,
) -> Option<JavaAction> {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().max(220.0), 24.0),
        egui::Sense::hover(),
    );
    let label_rect = Rect::from_min_max(rect.min, egui::pos2(rect.right() - 100.0, rect.bottom()));
    let title = format!(
        "Java {}（{}）：{}",
        runtime.version,
        runtime.architecture,
        runtime.path.display()
    );
    let response = ui
        .place(
            label_rect,
            egui::Label::new(&title)
                .truncate()
                .show_tooltip_when_elided(false)
                .sense(egui::Sense::click()),
        )
        .on_hover_text(&title);
    let mut action = response.clicked().then_some(JavaAction::Explain);
    for (offset, (symbol, tooltip, enabled)) in [
        ("↑", "提高优先级", index > 0),
        ("↓", "降低优先级", index + 1 < count),
        (
            "×",
            if java_ui::is_official_runtime(&runtime.path) {
                "无法移除官方 Java"
            } else {
                "从列表中移除；不会删除文件"
            },
            !java_ui::is_official_runtime(&runtime.path),
        ),
        ("↗", "打开文件夹", true),
    ]
    .into_iter()
    .enumerate()
    {
        let button = Rect::from_min_size(
            egui::pos2(rect.right() - 96.0 + offset as f32 * 24.0, rect.top()),
            Vec2::splat(24.0),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(button));
        if !enabled {
            child.disable();
        }
        let response = child
            .place(button, egui::Button::new(symbol).frame(false))
            .on_hover_text(tooltip)
            .on_disabled_hover_text(tooltip);
        if response.clicked() {
            action = Some(match offset {
                0 => JavaAction::Move(index, -1),
                1 => JavaAction::Move(index, 1),
                2 => JavaAction::Exclude(runtime.path.clone()),
                _ => JavaAction::Open(runtime.path.clone()),
            });
        }
    }
    action
}

fn setup_card(
    ui: &mut egui::Ui,
    title: &str,
    bottom: i8,
    mut open: Option<&mut bool>,
    body: impl FnOnce(&mut egui::Ui),
) {
    let expanded = open.as_deref().copied().unwrap_or(true);
    let frame = egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 245))
        .corner_radius(5)
        .shadow(egui::epaint::Shadow {
            offset: [0, 2],
            blur: 3,
            spread: 0,
            color: Color32::from_black_alpha(9),
        });
    let rect = if expanded {
        frame
            .inner_margin(egui::Margin {
                left: 25,
                right: 25,
                top: 40,
                bottom,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                body(ui);
            })
            .response
            .rect
    } else {
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), egui::Sense::hover());
        ui.painter().add(frame.paint(rect));
        rect
    };
    ui_style::place_left(
        ui,
        Rect::from_min_size(
            rect.min + Vec2::new(15.0, 10.0),
            Vec2::new(rect.width() - 50.0, 20.0),
        ),
        egui::Label::new(ui_style::card_title(title)),
    );
    if let Some(open) = open.as_mut() {
        let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 40.0));
        let response = ui.interact(
            header,
            ui.id().with(("launch-card", title)),
            egui::Sense::click(),
        );
        if response.clicked() {
            **open = !**open;
        }
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::CollapsingHeader, true, title)
        });
        let c = egui::pos2(rect.right() - 20.0, rect.top() + 20.0);
        let points = if expanded {
            vec![
                c + Vec2::new(-4.0, 2.0),
                c + Vec2::new(0.0, -2.0),
                c + Vec2::new(4.0, 2.0),
            ]
        } else {
            vec![
                c + Vec2::new(-4.0, -2.0),
                c + Vec2::new(0.0, 2.0),
                c + Vec2::new(4.0, -2.0),
            ]
        };
        ui.painter().add(egui::Shape::line(
            points,
            egui::Stroke::new(1.0_f32, theme::palette(ui.ctx()).text),
        ));
    }
    ui.add_space(15.0);
}
fn text_width(ui: &egui::Ui, text: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(
            text.into(),
            egui::FontId::proportional(13.0),
            theme::palette(ui.ctx()).text,
        )
        .size()
        .x
}
fn advanced_label_width(ui: &egui::Ui) -> f32 {
    (text_width(ui, "Java 虚拟机参数") + 23.0).max(text_width(ui, "启动前执行命令")) + 25.0
}
fn argument_row(ui: &mut egui::Ui, label: &str, body: impl FnOnce(&mut egui::Ui)) {
    setup_row(ui, label, text_width(ui, "默认版本隔离") + 25.0, body);
}
fn advanced_row(ui: &mut egui::Ui, label: &str, body: impl FnOnce(&mut egui::Ui)) {
    setup_row(ui, label, advanced_label_width(ui), body);
}
fn setup_row(ui: &mut egui::Ui, label: &str, width: f32, body: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 28.0), egui::Sense::hover());
        ui_style::place_left(ui, rect, egui::Label::new(label));
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 28.0),
            egui::Layout::left_to_right(egui::Align::Center),
            body,
        );
    });
}
fn unavailable_text(ui: &mut egui::Ui, hint: &str, reason: &str) {
    ui.add_enabled_ui(false, |ui| {
        text_edit(ui, &mut String::new(), hint);
    })
    .response
    .on_hover_text(reason);
}
fn disabled_checkbox(ui: &mut egui::Ui, text: &str, checked: bool, reason: &str, height: f32) {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let mut value = checked;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    child.disable();
    child
        .place(rect, egui::Checkbox::new(&mut value, text))
        .on_hover_text(reason);
}
fn radio_row(ui: &mut egui::Ui, selected: bool, label: &str) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(110.0, 22.0), egui::Sense::hover());
    ui_style::place_left(ui, rect, egui::RadioButton::new(selected, label))
}
pub(super) fn hint(ui: &mut egui::Ui, text: &str, yellow: bool) {
    super::hint_ui::inline(ui, text, yellow);
}

fn dimensions(width: &str, height: &str) -> Result<(u32, u32), &'static str> {
    let a = width
        .parse::<u32>()
        .ok()
        .filter(|v| (1..=16384).contains(v));
    let b = height
        .parse::<u32>()
        .ok()
        .filter(|v| (1..=16384).contains(v));
    match (a, b) {
        (Some(a), Some(b)) => Ok((a, b)),
        _ => Err("窗口宽高尚未保存：请输入 1 到 16384 的整数。"),
    }
}
fn memory_slider_maximum(memory: Option<config::MemorySnapshot>) -> u32 {
    memory
        .map_or(49, |memory| {
            let gb = memory.total_mb as f64 / 1024.0;
            if gb <= 1.5 {
                ((gb - 0.3) / 0.1).floor().max(1.0) as u32
            } else if gb <= 8.0 {
                ((gb - 1.5) / 0.5).floor() as u32 + 12
            } else if gb <= 16.0 {
                (gb - 8.0).floor() as u32 + 25
            } else {
                ((gb - 16.0) / 2.0).floor() as u32 + 33
            }
        })
        .min(57)
}
fn isolation_label(value: IsolationPolicy) -> &'static str {
    match value {
        IsolationPolicy::Off => "关闭",
        IsolationPolicy::Modded => "隔离可安装 Mod 的版本",
        IsolationPolicy::NonRelease => "隔离非正式版",
        IsolationPolicy::ModdedOrNonRelease => "隔离可安装 Mod 的版本与非正式版",
        IsolationPolicy::All => "隔离所有版本",
    }
}
pub(super) const PRE_LAUNCH_HELP: &str = "仅运行你在本机设置中填写的命令；不会执行下载元数据中的命令。先执行全局命令，再执行版本命令，工作目录为游戏根目录。Windows 使用 cmd /D /V:ON；macOS 使用 /bin/sh。支持 {minecraft}、{verpath}/{version_path}、{verindie}/{version_indie}、{java}、{name}、{version}、{path}、{path_with_name}、{pcl_version}。路径标记安全传入环境变量；命令输出不写入启动器日志。非零退出会提示后继续，取消启动会请求终止本次命令树。";

fn visibility_label(value: LauncherVisibility) -> &'static str {
    match value {
        LauncherVisibility::CloseOnLaunch => "游戏启动后立即关闭",
        LauncherVisibility::HideThenClose => "游戏启动后隐藏，游戏退出后自动关闭",
        LauncherVisibility::HideThenRestore => "游戏启动后隐藏，游戏退出后重新打开",
        LauncherVisibility::Minimize => "游戏启动后最小化",
        LauncherVisibility::Keep => "游戏启动后仍保持不变",
    }
}
fn priority_label(value: ProcessPriority) -> &'static str {
    match value {
        ProcessPriority::High => "高（优先保证游戏运行）",
        ProcessPriority::Normal => "中（平衡）",
        ProcessPriority::Low => "低（优先保证其他程序运行）",
    }
}

fn window_label(value: WindowMode) -> &'static str {
    match value {
        WindowMode::Default => "默认",
        WindowMode::Fullscreen => "全屏",
        WindowMode::LauncherSize => "与启动器尺寸一致",
        WindowMode::Custom => "自定义尺寸",
        WindowMode::Maximized => "最大化",
    }
}
fn gc_label(value: GcMode) -> &'static str {
    match value {
        GcMode::PreferZgc => "尽量使用 ZGC",
        GcMode::PreferGenerationalZgc => "尽量使用分代 ZGC",
        GcMode::G1 => "标准 G1GC",
        GcMode::TunedG1 => "调优 G1GC",
        GcMode::Custom => "不指定（可自定义）",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_radio_rows_align_in_vertical_and_horizontal_layouts() {
        let ctx = egui::Context::default();
        let mut rects = Vec::new();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                rects.push(radio_row(ui, false, "Automatic").rect);
                ui.horizontal(|ui| {
                    rects.push(radio_row(ui, true, "Custom").rect);
                    ui.add(egui::Slider::new(&mut 5.0, 0.0..=10.0));
                });
            });
        });
        assert_eq!(rects[0].left(), rects[1].left());
    }
    #[test]
    fn source_card_padding_and_collapsed_spacing_survive_absolute_title_placement() {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let fallback = fonts.families[&egui::FontFamily::Proportional].clone();
        fonts
            .families
            .insert(egui::FontFamily::Name("PCL Bold".into()), fallback);
        ctx.set_fonts(fonts);
        let mut content = None;
        let mut next_y = 0.0;
        let mut collapsed_end = 0.0;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(800.0, 700.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        setup_card(ui, "启动选项", 25, None, |ui| {
                            let (rect, _) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 247.0),
                                egui::Sense::hover(),
                            );
                            content = Some(rect);
                        });
                        next_y = ui.next_widget_position().y;
                        let mut open = false;
                        setup_card(ui, "高级选项", 15, Some(&mut open), |_| {
                            panic!("collapsed body must not render")
                        });
                        collapsed_end = ui.next_widget_position().y;
                    });
            },
        );
        let rect = content.unwrap();
        assert_eq!(rect.min, egui::pos2(25.0, 40.0));
        assert_eq!(rect.width(), 750.0);
        assert_eq!(next_y, 327.0); // Original card 40 + 247 + 25, then outside margin 15.
        assert_eq!(collapsed_end - next_y, 55.0); // Original collapsed card 40 + margin 15.
    }
    #[test]
    fn java_row_buttons_preserve_source_height_and_truncate_long_path() {
        let ctx = egui::Context::default();
        let runtime = pcl_core::java::JavaRuntime {
            path: PathBuf::from(format!(
                "/fixture/{}/bin/java",
                "very-long-folder-".repeat(40)
            )),
            major: 21,
            version: pcl_core::java::JavaVersion::new(21, 0, 7, 0),
            architecture: "aarch64".into(),
        };
        let mut height = 0.0;
        let mut width = 0.0;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(400.0, 200.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let start = ui.next_widget_position().y;
                        assert!(java_list_row(ui, &runtime, 0, 1).is_none());
                        height = ui.next_widget_position().y - start;
                        width = ui.min_rect().width();
                    });
            },
        );
        assert_eq!(height, 24.0);
        assert_eq!(width, 400.0);
    }

    #[test]
    fn invalid_window_edit_does_not_yield_a_launch_dimension() {
        assert_eq!(dimensions("854", "480"), Ok((854, 480)));
        assert!(dimensions("0", "480").is_err());
        assert!(dimensions("16385", "480").is_err());
        assert!(dimensions("", "480").is_err());
        assert!(dimensions("854", "1; quit").is_err());
    }
}
