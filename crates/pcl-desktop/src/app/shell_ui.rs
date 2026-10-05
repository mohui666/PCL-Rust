use super::{Launcher, Page};
use crate::theme;
use crate::ui_style;
use eframe::egui::{self, Color32, RichText, Vec2};
use std::{path::PathBuf, sync::atomic::Ordering};

impl Launcher {
    pub(super) fn settings_page(&mut self, ui: &mut egui::Ui) {
        if self.settings_tab == 1 {
            self.appearance_page(ui);
        } else {
            self.launch_settings_page(ui);
        }
    }
    pub(super) fn floating_entries(&mut self, ctx: &egui::Context) {
        let task_visible =
            !self.task_view && self.task.as_ref().is_some_and(|task| !task.is_finished());
        let game_visible = self.game_pid.is_some();
        if !task_visible && !game_visible {
            return;
        }
        // FormMain.PanExtra: right/bottom margin 15; MyExtraButton is a
        // 40-DIP circle in a 50-DIP row (10-DIP top margin). Shutdown is below tasks.
        egui::Area::new(egui::Id::new("launcher-extra-buttons"))
            .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-15.0, -15.0))
            .order(egui::Order::Foreground)
            .movable(false)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                if task_visible {
                    let progress = self.task.as_ref().and_then(|task| task.overall_progress());
                    if extra_button(ui, &self.assets, "tasks", "任务管理", 1.1, progress).clicked()
                    {
                        self.task_view = true;
                    }
                }
                if game_visible
                    && extra_button(ui, &self.assets, "shutdown", "关闭 Minecraft", 1.0, None)
                        .clicked()
                {
                    // Only the Child owned by this launch is terminated. Keep
                    // the entry visible until GameFinished confirms its exit.
                    self.game_stop.store(true, Ordering::Relaxed);
                }
            });
    }
    pub(super) fn sidebar_width(&self) -> f32 {
        if self.task_view {
            return 200.0;
        }
        match self.page {
            Page::Launch if !self.version_tools => 300.0,
            Page::Launch => 138.0,
            Page::Download => 135.0,
            Page::Settings => 121.0,
            Page::More => 149.0,
        }
    }

    pub(super) fn title_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("title")
            .exact_height(48.0)
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                ui_style::title_background(ui.painter(), rect);
                let drag = ui.interact(rect, ui.id().with("drag"), egui::Sense::drag());
                if drag.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                let resource_title = if self.page == Page::Download {
                    self.resource_detail_title().map(str::to_owned)
                } else {
                    None
                };
                let help_title = if self.page == Page::More {
                    self.more_detail_title().map(str::to_owned)
                } else {
                    None
                };
                if self.task_view
                    || help_title.is_some()
                    || resource_title.is_some()
                    || (self.page == Page::Launch && (self.version_view || self.version_tools))
                {
                    let r = egui::Rect::from_min_size(
                        rect.min + Vec2::new(8.0, 8.0),
                        Vec2::new(34.0, 32.0),
                    );
                    let can_back =
                        self.task_view || resource_title.is_none() || self.busy.is_none();
                    let back = ui
                        .add_enabled_ui(can_back, |ui| {
                            ui.place(
                                r,
                                egui::Button::new("")
                                    .fill(Color32::TRANSPARENT)
                                    .stroke(egui::Stroke::NONE),
                            )
                        })
                        .inner;
                    back.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, can_back, "返回")
                    });
                    let center = r.center();
                    let stroke = egui::Stroke::new(1.8_f32, Color32::WHITE);
                    ui.painter().line_segment(
                        [center + Vec2::new(7.0, 0.0), center - Vec2::new(6.0, 0.0)],
                        stroke,
                    );
                    ui.painter().line_segment(
                        [center + Vec2::new(0.0, -6.0), center - Vec2::new(6.0, 0.0)],
                        stroke,
                    );
                    ui.painter().line_segment(
                        [center + Vec2::new(0.0, 6.0), center - Vec2::new(6.0, 0.0)],
                        stroke,
                    );
                    let title = if self.task_view {
                        "任务管理".to_owned()
                    } else if let Some(title) = help_title.as_ref() {
                        title.clone()
                    } else if let Some(title) = resource_title.as_ref() {
                        title.clone()
                    } else if self.version_tools {
                        format!(
                            "版本设置 - {}",
                            self.settings
                                .selected_version
                                .as_deref()
                                .unwrap_or("未选择")
                        )
                    } else {
                        "版本选择".to_owned()
                    };
                    ui.painter()
                        .with_clip_rect(egui::Rect::from_min_max(
                            rect.min,
                            rect.right_bottom() - Vec2::new(92.0, 0.0),
                        ))
                        .text(
                            rect.min + Vec2::new(46.0, 24.0),
                            egui::Align2::LEFT_CENTER,
                            title,
                            egui::FontId::proportional(15.0),
                            Color32::WHITE,
                        );
                    if back.clicked() {
                        if self.task_view {
                            self.task_view = false;
                        } else if help_title.is_some() {
                            self.leave_more_detail();
                        } else if resource_title.is_some() {
                            self.leave_resource_detail();
                        } else {
                            self.version_view = false;
                            self.version_tools = false;
                        }
                    }
                } else {
                    self.assets.icon(
                        ui,
                        "logo",
                        egui::Rect::from_min_size(
                            rect.min + Vec2::new(19.0, 15.5),
                            Vec2::new(39.0, 17.0),
                        ),
                        Color32::WHITE,
                    );
                    // 2.13.1.1 HiddenRefresh unconditionally collapses the Link tab.
                    // The full third-party product name remains in the window title and About.
                    let start = rect.center().x - 176.0;
                    for (i, (page, text, icon)) in [
                        (Page::Launch, "启动", "launch"),
                        (Page::Download, "下载", "download"),
                        (Page::Settings, "设置", "settings"),
                        (Page::More, "更多", "more"),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let r = egui::Rect::from_min_size(
                            egui::pos2(start + i as f32 * 88.0 + 5.0, rect.top() + 10.5),
                            Vec2::new(78.0, 27.0),
                        );
                        if ui_style::pill(ui, &self.assets, r, text, icon, self.page == page, true)
                            .clicked()
                        {
                            self.page = page;
                            self.version_view = false;
                            self.version_tools = false;
                            if self.page == Page::Download
                                && self.manifest.is_empty()
                                && self.busy.is_none()
                            {
                                self.load_manifest();
                            }
                        }
                    }
                }
                for (offset, close) in [(26.0, true), (58.0, false)] {
                    let r = egui::Rect::from_center_size(
                        egui::pos2(rect.right() - offset, rect.center().y),
                        Vec2::splat(28.0),
                    );
                    let response = ui.put(
                        r,
                        egui::Button::new("")
                            .fill(Color32::TRANSPARENT)
                            .stroke(egui::Stroke::NONE),
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            true,
                            if close { "关闭" } else { "最小化" },
                        )
                    });
                    if response.hovered() {
                        ui.painter()
                            .rect_filled(r, 3, Color32::from_white_alpha(30));
                    }
                    let p = r.center();
                    let stroke = egui::Stroke::new(1.6_f32, Color32::WHITE);
                    if close {
                        ui.painter()
                            .line_segment([p - Vec2::splat(5.0), p + Vec2::splat(5.0)], stroke);
                        ui.painter().line_segment(
                            [p + Vec2::new(-5.0, 5.0), p + Vec2::new(5.0, -5.0)],
                            stroke,
                        );
                    } else {
                        ui.painter().line_segment(
                            [p - Vec2::new(6.0, 0.0), p + Vec2::new(6.0, 0.0)],
                            stroke,
                        );
                    }
                    if response.clicked() {
                        if close {
                            self.cancel.store(true, Ordering::Relaxed);
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        } else {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                    }
                }
            });
    }
    pub(super) fn sidebar(&mut self, ctx: &egui::Context) {
        let width = self.sidebar_width();
        egui::SidePanel::left("sidebar")
            .exact_width(width)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                ui.painter().rect_filled(
                    rect,
                    egui::CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: 6,
                        se: 0,
                    },
                    Color32::from_white_alpha(241),
                );
                if self.task_view {
                    self.task_sidebar(ui);
                } else if self.page == Page::Launch && self.version_view && !self.version_tools {
                    self.folder_sidebar(ui, rect);
                } else if self.page == Page::Launch && !self.version_tools {
                    for (i, (name, icon, ms)) in
                        [("正版", "account", true), ("离线", "offline", false)]
                            .into_iter()
                            .enumerate()
                    {
                        let r = egui::Rect::from_min_size(
                            rect.min + Vec2::new(72.0 + i as f32 * 82.0, 22.0),
                            Vec2::new(74.0, 27.0),
                        );
                        if ui_style::pill(
                            ui,
                            &self.assets,
                            r,
                            name,
                            icon,
                            self.microsoft == ms,
                            false,
                        )
                        .clicked()
                        {
                            self.select_account_mode(ms);
                        }
                    }
                    let bottom = rect.bottom() - 20.0;
                    let small_y = bottom - 35.0;
                    let launch_y = small_y - 10.0 - 54.0;
                    let login_center = (rect.top() + 49.0 + launch_y) * 0.5;
                    let head_rect = egui::Rect::from_center_size(
                        egui::pos2(rect.center().x, login_center - 24.0),
                        Vec2::splat(64.0),
                    );
                    if self.microsoft {
                        self.account_sidebar(ui, rect, login_center);
                    } else {
                        self.assets.head(ui, head_rect);
                        let r = egui::Rect::from_min_size(
                            egui::pos2(rect.left() + 20.0, login_center + 23.0),
                            Vec2::new(260.0, 28.0),
                        );
                        let history: Vec<&str> = self
                            .settings
                            .offline_history
                            .iter()
                            .map(String::as_str)
                            .collect();
                        ui_style::editable_combo(
                            ui,
                            r,
                            "offline-history",
                            &mut self.settings.offline_name,
                            &history,
                            "游戏用户名",
                        );
                    }
                    let version = self
                        .settings
                        .selected_version
                        .as_deref()
                        .unwrap_or("未找到可用的游戏版本");
                    let text = if self.game_pid.is_some() {
                        "游戏运行中"
                    } else if self.settings.selected_version.is_some() {
                        "启动游戏"
                    } else {
                        "下载游戏"
                    };
                    let r = egui::Rect::from_min_size(
                        egui::pos2(rect.left() + 20.0, launch_y),
                        Vec2::new(260.0, 54.0),
                    );
                    if ui_style::outline_button(
                        ui,
                        r,
                        text,
                        Some(version),
                        true,
                        self.busy.is_none() && self.game_pid.is_none(),
                    )
                    .clicked()
                    {
                        if self.settings.selected_version.is_none() {
                            self.page = Page::Download;
                        } else {
                            self.launch(false);
                        }
                    }
                    let r = egui::Rect::from_min_size(
                        egui::pos2(rect.left() + 20.0, small_y),
                        Vec2::new(125.0, 35.0),
                    );
                    if ui_style::outline_button(ui, r, "版本选择", None, false, true).clicked()
                    {
                        self.version_view = !self.version_view;
                        self.refresh_versions();
                    }
                    let r = r.translate(Vec2::new(135.0, 0.0));
                    if ui_style::outline_button(ui, r, "版本设置", None, false, true).clicked()
                    {
                        self.version_tools = true;
                        self.tools_tab = 0;
                        self.refresh_mods();
                    }
                } else {
                    self.navigation_sidebar(ui, rect);
                }
            });
    }
    fn navigation_sidebar(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let rows: &[(&str, &str, usize, bool)] = match self.page {
            Page::Launch => &[
                ("概览", "overview", 0, true),
                ("设置", "wrench", 2, true),
                ("Mod 管理", "mod", 1, true),
                ("导出", "pack", 3, true),
            ],
            Page::Download => &[
                ("原版游戏", "game", 0, true),
                ("Mod", "mod", 1, true),
                ("整合包", "pack", 2, true),
                ("数据包", "datapack", 3, true),
                ("资源包", "resourcepack", 4, true),
                ("光影包", "shader", 5, true),
            ],
            Page::Settings => &[
                ("启动", "launch", 0, true),
                ("个性化", "appearance", 1, true),
                ("其他", "more", 2, false),
            ],
            Page::More => &[("帮助", "help", 0, true), ("关于与鸣谢", "about", 1, true)],
        };
        if self.page == Page::Download {
            ui.painter().text(
                rect.min + Vec2::new(13.0, 68.0),
                egui::Align2::LEFT_TOP,
                "社区资源",
                egui::FontId::proportional(12.0),
                Color32::from_gray(140),
            );
        }
        for (i, &(text, icon, tab, enabled)) in rows.iter().enumerate() {
            let gap = if self.page == Page::Download && i > 0 {
                38.0
            } else {
                0.0
            };
            let r = egui::Rect::from_min_size(
                rect.min + Vec2::new(0.0, 12.0 + i as f32 * 36.0 + gap),
                Vec2::new(rect.width(), 36.0),
            );
            let selected = match self.page {
                Page::Launch => self.tools_tab == tab,
                Page::Download => self.download_tab == tab,
                Page::Settings => self.settings_tab == tab,
                Page::More => self.more.tab == tab,
            };
            let response = ui.interact(
                r,
                ui.id().with(("sidebar", i)),
                if enabled {
                    egui::Sense::click()
                } else {
                    egui::Sense::hover()
                },
            );
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, text));
            if !enabled {
                response.clone().on_hover_text("此功能尚未迁移");
            }
            if response.hovered() && enabled {
                ui.painter().rect_filled(
                    r,
                    0,
                    theme::palette(ui.ctx()).light.gamma_multiply(100.0 / 255.0),
                );
            }
            if response.clicked() && enabled {
                match self.page {
                    Page::Launch => {
                        if tab == 3 && self.tools_tab != 3 {
                            self.pack_export = Default::default();
                        }
                        self.tools_tab = tab;
                        if tab == 1 {
                            self.refresh_mods();
                        }
                    }
                    Page::Download => {
                        self.download_tab = tab;
                        if (1..=5).contains(&tab) {
                            self.ensure_resource_target();
                        } else if tab == 0 && self.manifest.is_empty() && self.busy.is_none() {
                            self.load_manifest();
                        }
                    }
                    Page::Settings => self.settings_tab = tab,
                    Page::More => self.more_navigation(tab),
                }
            }
            let color = if selected {
                theme::palette(ui.ctx()).accent
            } else if enabled {
                theme::palette(ui.ctx()).text
            } else {
                Color32::from_gray(150)
            };
            if selected {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(r.min + Vec2::new(0.0, 6.0), Vec2::new(3.0, 24.0)),
                    2,
                    theme::palette(ui.ctx()).accent,
                );
            }
            let scale = match icon {
                "launch" => 0.8,
                "appearance" => 0.9,
                "more" => 0.8,
                "mod" => 0.97,
                "pack" => 0.98,
                "datapack" => 0.96,
                "resourcepack" => 0.81,
                "shader" => 1.04,
                "help" => 0.97,
                "about" => 0.98,
                _ => 1.0,
            };
            let icon_rect = egui::Rect::from_center_size(
                r.min + Vec2::new(24.0, 18.0),
                Vec2::new(24.0, 20.0) * scale,
            );
            if self.page == Page::More {
                self.more_icon(ui, icon, icon_rect, color);
            } else {
                self.assets.icon(ui, icon, icon_rect, color);
            }
            if self.page == Page::More && tab == 0 {
                let refresh_rect = egui::Rect::from_center_size(
                    egui::pos2(r.right() - 19.0, r.center().y),
                    Vec2::splat(24.0),
                );
                let refresh = ui.place(refresh_rect, egui::Button::new("").frame(false));
                self.more_icon(ui, "refresh", refresh_rect.shrink(6.0), color);
                refresh.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "刷新本地帮助")
                });
                if refresh.on_hover_text("重新载入本地帮助").clicked() {
                    self.refresh_help();
                }
            }
            ui.painter().text(
                r.min + Vec2::new(44.0, 18.0),
                egui::Align2::LEFT_CENTER,
                text,
                egui::FontId::proportional(14.0),
                color,
            );
        }
    }

    fn folder_sidebar(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let mut roots = vec![self.settings.game_root.clone()];
        for path in &self.settings.game_roots {
            if !roots.contains(path) {
                roots.push(path.clone());
            }
        }
        let home =
            std::env::var_os(if cfg!(windows) { "APPDATA" } else { "HOME" }).map(PathBuf::from);
        if let Some(home) = home {
            let vanilla = if cfg!(windows) {
                home.join(".minecraft")
            } else {
                home.join("Library/Application Support/minecraft")
            };
            if vanilla.is_dir() && !roots.contains(&vanilla) {
                roots.push(vanilla);
            }
        }
        let mut selected = None;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect.shrink2(Vec2::new(0.0, 12.0)))
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("game-folder-list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.add_space(7.0);
                        ui.horizontal(|ui| {
                            ui.add_space(13.0);
                            ui.label(
                                RichText::new("文件夹列表")
                                    .size(12.0)
                                    .color(Color32::from_gray(140)),
                            );
                        });
                        for (i, root) in roots.iter().enumerate() {
                            let (r, response) = ui.allocate_exact_size(
                                Vec2::new(rect.width(), 40.0),
                                egui::Sense::click(),
                            );
                            let title = if i == 0 {
                                "当前文件夹".to_owned()
                            } else {
                                root.file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned()
                            };
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &title)
                            });
                            if response.clicked() && self.busy.is_none() && self.game_pid.is_none()
                            {
                                selected = Some(root.clone());
                            }
                            if i == 0 {
                                ui.painter().rect_filled(
                                    egui::Rect::from_min_size(
                                        r.min + Vec2::new(0.0, 7.0),
                                        Vec2::new(3.0, 26.0),
                                    ),
                                    2,
                                    theme::palette(ui.ctx()).accent,
                                );
                            }
                            let clip = r.shrink2(Vec2::new(10.0, 0.0));
                            ui.painter().with_clip_rect(clip).text(
                                r.min + Vec2::new(13.0, 3.0),
                                egui::Align2::LEFT_TOP,
                                title,
                                egui::FontId::proportional(14.0),
                                if i == 0 {
                                    theme::palette(ui.ctx()).accent
                                } else {
                                    theme::palette(ui.ctx()).text
                                },
                            );
                            ui.painter().with_clip_rect(clip).text(
                                r.min + Vec2::new(13.0, 23.0),
                                egui::Align2::LEFT_TOP,
                                root.display().to_string(),
                                egui::FontId::proportional(12.0),
                                Color32::from_gray(150),
                            );
                        }
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.add_space(13.0);
                            ui.label(
                                RichText::new("添加或导入")
                                    .size(12.0)
                                    .color(Color32::from_gray(140)),
                            );
                        });
                        for (label, import) in [("添加已有文件夹", false), ("导入整合包", true)]
                        {
                            let (r, response) = ui.allocate_exact_size(
                                Vec2::new(rect.width(), 34.0),
                                egui::Sense::click(),
                            );
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    self.busy.is_none() && self.game_pid.is_none(),
                                    label,
                                )
                            });
                            self.assets.icon(
                                ui,
                                if import { "pack" } else { "game" },
                                egui::Rect::from_min_size(
                                    r.min + Vec2::new(14.0, 8.0),
                                    Vec2::splat(18.0),
                                ),
                                theme::palette(ui.ctx()).text,
                            );
                            ui.painter().text(
                                r.min + Vec2::new(42.0, 17.0),
                                egui::Align2::LEFT_CENTER,
                                label,
                                egui::FontId::proportional(14.0),
                                theme::palette(ui.ctx()).text,
                            );
                            if response.clicked() && self.busy.is_none() && self.game_pid.is_none()
                            {
                                if import {
                                    self.page = Page::Download;
                                    self.download_tab = 2;
                                    self.version_view = false;
                                } else if let Some(path) = rfd::FileDialog::new()
                                    .set_title("选择已有的 Minecraft 文件夹")
                                    .pick_folder()
                                {
                                    selected = Some(path);
                                }
                            }
                        }
                    });
            },
        );
        if let Some(root) = selected {
            if root != self.settings.game_root {
                let mut next = self.settings.clone();
                if !next.game_roots.contains(&next.game_root) {
                    next.game_roots.push(next.game_root.clone());
                }
                if !next.game_roots.contains(&root) {
                    next.game_roots.push(root.clone());
                }
                next.game_root = root;
                next.selected_version = None;
                if let Err(error) = pcl_core::config::save_settings(&self.settings_path, &next) {
                    self.error = Some(format!("游戏目录切换失败：{error:#}"));
                    return;
                }
                self.settings = next;
                self.root_text = self.settings.game_root.display().to_string();
                self.invalidate_root_views();
                self.refresh_versions();
            }
        }
    }
}

/// MyExtraButton.xaml: Color3 circle, Color4 hover, Color8 glyph, 12-DIP inset.
fn extra_button(
    ui: &mut egui::Ui,
    assets: &crate::ui_style::Assets,
    icon: &str,
    label: &str,
    logo_scale: f32,
    progress: Option<f64>,
) -> egui::Response {
    let (_, rect) = ui.allocate_space(Vec2::splat(40.0));
    let response = ui.interact(rect, ui.id().with(icon), egui::Sense::click());
    let palette = theme::palette(ui.ctx());
    let hover = ui.ctx().animate_bool_with_time(
        response.id.with("hover"),
        response.hovered(),
        if response.hovered() { 0.12 } else { 0.15 },
    );
    let pressed = ui.ctx().animate_bool_with_time(
        response.id.with("pressed"),
        response.is_pointer_button_down_on(),
        0.06,
    );
    let painted = egui::Rect::from_center_size(rect.center(), rect.size() * (1.0 - 0.2 * pressed));
    let fill = Color32::from_rgb(
        egui::lerp(palette.accent.r() as f32..=palette.border.r() as f32, hover) as u8,
        egui::lerp(palette.accent.g() as f32..=palette.border.g() as f32, hover) as u8,
        egui::lerp(palette.accent.b() as f32..=palette.border.b() as f32, hover) as u8,
    );
    ui.painter().add(
        egui::epaint::Shadow {
            offset: [0, 0],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(51),
        }
        .as_shape(painted, 20),
    );
    ui.painter().rect_filled(painted, 20, fill);
    if let Some(progress) = progress.filter(|value| *value >= 0.0001) {
        let fill = egui::Rect::from_min_max(
            egui::pos2(
                painted.left(),
                painted.bottom() - painted.height() * progress.clamp(0.0, 1.0) as f32,
            ),
            painted.right_bottom(),
        );
        ui.painter()
            .with_clip_rect(fill.intersect(ui.clip_rect()))
            .rect_filled(painted, 20, Color32::from_white_alpha(47));
    }
    assets.icon(
        ui,
        icon,
        egui::Rect::from_center_size(
            painted.center(),
            Vec2::splat(16.0 * logo_scale * (1.0 - 0.2 * pressed)),
        ),
        palette.lightest,
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    response.on_hover_text(label)
}
