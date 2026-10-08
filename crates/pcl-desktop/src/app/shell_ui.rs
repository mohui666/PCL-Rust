use super::{Launcher, Page};
use crate::theme;
use crate::ui_style;
use eframe::egui::{self, Color32, RichText, Vec2};
use std::sync::atomic::Ordering;

impl Launcher {
    fn normalize_visible_navigation(&mut self, ctx: &egui::Context) {
        let page_key = match self.page {
            Page::Launch => "",
            Page::Download => "download",
            Page::Settings => "setup",
            Page::More => "more",
        };
        if !super::appearance_ui::feature_visible(ctx, &self.settings, page_key) {
            self.page = Page::Launch;
            self.version_view = false;
            self.version_tools = false;
        }
        let (keys, current): (&[&str], usize) = match self.page {
            Page::Settings => (
                &["setup_launch", "setup_ui", "setup_system"],
                self.settings_tab,
            ),
            Page::More => (&["help", "about"], self.more.tab),
            _ => (&[], 0),
        };
        if !keys.is_empty()
            && keys
                .get(current)
                .is_none_or(|key| !super::appearance_ui::feature_visible(ctx, &self.settings, key))
        {
            if let Some(next) = keys
                .iter()
                .position(|key| super::appearance_ui::feature_visible(ctx, &self.settings, key))
            {
                if self.page == Page::Settings {
                    self.settings_tab = next;
                } else {
                    self.more_navigation(next);
                }
            } else {
                self.page = Page::Launch;
                self.version_view = false;
                self.version_tools = false;
            }
        }
        if !super::appearance_ui::feature_visible(ctx, &self.settings, "version") {
            self.version_view = false;
            self.version_tools = false;
        }
    }
    pub(super) fn page_motion_key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{:?}:{:?}",
            self.page as u8,
            self.task_view,
            self.version_view,
            self.version_tools,
            self.tools_tab,
            self.settings_tab,
            self.download_tab,
            self.resource_browser.scroll_key(),
            self.more_scroll_key(),
            self.download_selection
        )
    }
    fn sidebar_motion_key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.page as u8,
            self.task_view,
            self.version_view,
            self.version_tools,
            self.launch_ui.visible()
        )
    }
    fn title_motion_key(&self) -> String {
        if self.task_view {
            "任务管理".into()
        } else if self.page == Page::More {
            self.more_detail_title().unwrap_or("").into()
        } else if self.page == Page::Download {
            self.resource_detail_title().unwrap_or("").into()
        } else if self.version_tools {
            format!("版本设置{:?}", self.settings.selected_version)
        } else if self.version_view {
            "版本选择".into()
        } else {
            String::new()
        }
    }
    pub(super) fn settings_page(&mut self, ui: &mut egui::Ui) {
        if self.settings_tab == 1 {
            self.appearance_page(ui);
        } else if self.settings_tab == 2 {
            self.system_settings_page(ui);
        } else {
            self.launch_settings_page(ui);
        }
    }
    pub(super) fn floating_entries(&mut self, ctx: &egui::Context) {
        let task_visible = !self.task_view && (self.task.is_some() || self.task_hub.has_history());
        let game_visible = self.game_pid.is_some();
        let music = self.appearance.music_info();
        if !task_visible && !game_visible && music.is_none() {
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
                if let Some((title, playing, progress)) = &music {
                    let label = format!(
                        "背景音乐：{title}\n左键{}，右键下一首",
                        if *playing { "暂停" } else { "播放" }
                    );
                    let response = extra_button(
                        ui,
                        &self.assets,
                        "music",
                        &label,
                        1.0,
                        Some(f64::from(*progress)),
                    );
                    super::appearance_ui::AppearanceState::paint_music_icon(
                        ui,
                        egui::Rect::from_center_size(
                            response.rect.center(),
                            Vec2::splat(if *playing { 16.0 } else { 12.8 }),
                        ),
                        *playing,
                    );
                    let result = if response.clicked() {
                        self.appearance.toggle_music()
                    } else if response.secondary_clicked() {
                        self.appearance.next_music(&self.settings)
                    } else {
                        Ok(())
                    };
                    if let Err(error) = result {
                        self.error = Some(format!("背景音乐操作失败：{error:#}"));
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
        if self.page == Page::Launch && self.launch_ui.visible() {
            return 300.0;
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
        self.normalize_visible_navigation(ctx);
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
                let motion = super::page_motion::begin(
                    ui,
                    "title",
                    self.title_motion_key(),
                    super::page_motion::Kind::Title,
                );
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
                    // 2.13.1.1 HiddenRefresh unconditionally collapses the Link tab.
                    let mut items = vec![
                        (Page::Launch, "启动", "launch"),
                        (Page::Download, "下载", "download"),
                        (Page::Settings, "设置", "settings"),
                        (Page::More, "更多", "more"),
                    ];
                    items.retain(|(page, _, _)| {
                        super::appearance_ui::feature_visible(
                            ctx,
                            &self.settings,
                            match page {
                                Page::Launch => "launch",
                                Page::Download => "download",
                                Page::Settings => "setup",
                                Page::More => "more",
                            },
                        )
                    });
                    if !items.iter().any(|(page, _, _)| *page == self.page) {
                        self.page = Page::Launch;
                    }
                    let start = if self.settings.ui_title_mode == 0 && self.settings.ui_title_left {
                        13.0
                    } else {
                        rect.center().x - items.len() as f32 * 44.0
                    };
                    let title_rect = custom_title_rect(rect, start, self.settings.ui_title_mode);
                    match self
                        .appearance
                        .paint_custom_title(ui, title_rect, &self.settings)
                    {
                        Ok(true) => (),
                        Ok(false) => {
                            ui.painter().with_clip_rect(title_rect).text(
                                egui::pos2(title_rect.left(), rect.top() + 24.0),
                                egui::Align2::LEFT_CENTER,
                                "PCL Rust",
                                egui::FontId::proportional(20.0),
                                Color32::WHITE,
                            );
                        }
                        Err(error) => self.error = Some(format!("标题栏绘制失败：{error:#}")),
                    }
                    for (i, (page, text, icon)) in items.into_iter().enumerate() {
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
                super::page_motion::finish(ui, motion);
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
                            self.jobs.cancel_all();
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        } else {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                    }
                }
            });
    }
    pub(super) fn sidebar(&mut self, ctx: &egui::Context) {
        let width = super::page_motion::sidebar_width(ctx, self.sidebar_width());
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
                let motion = super::page_motion::begin(
                    ui,
                    "sidebar",
                    self.sidebar_motion_key(),
                    if self.page == Page::Launch && !self.version_tools {
                        super::page_motion::Kind::SidebarScale
                    } else {
                        super::page_motion::Kind::SidebarRows
                    },
                );
                if self.task_view {
                    self.task_sidebar(ui);
                } else if self.page == Page::Launch && self.launch_ui.visible() {
                    self.launch_progress_sidebar(ui, rect);
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
                        if ui
                            .add_enabled_ui(ms, |ui| {
                                ui_style::pill(
                                    ui,
                                    &self.assets,
                                    r,
                                    name,
                                    icon,
                                    self.microsoft == ms,
                                    false,
                                )
                            })
                            .inner
                            .on_disabled_hover_text(
                                "离线登录已禁用，请使用拥有 Minecraft Java 版的微软账号。",
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
                        self.offline_head(ui, head_rect);
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
                    let text = if self.settings.selected_version.is_some() {
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
                        self.busy.is_none(),
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
                    if super::appearance_ui::feature_visible(ui.ctx(), &self.settings, "version")
                        && ui_style::outline_button(ui, r, "版本选择", None, false, true).clicked()
                    {
                        self.version_view = !self.version_view;
                        self.refresh_versions();
                    }
                    let r = r.translate(Vec2::new(135.0, 0.0));
                    if super::appearance_ui::feature_visible(ui.ctx(), &self.settings, "version")
                        && ui_style::outline_button(ui, r, "版本设置", None, false, true).clicked()
                    {
                        self.version_tools = true;
                        self.tools_tab = 0;
                        self.refresh_mods();
                    }
                } else {
                    self.navigation_sidebar(ui, rect);
                }
                super::page_motion::finish(ui, motion);
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
                ("其他", "more", 2, true),
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
        let mut visible_index = 0;
        for &(text, icon, tab, enabled) in rows {
            let key = match (self.page, tab) {
                (Page::Settings, 0) => "setup_launch",
                (Page::Settings, 1) => "setup_ui",
                (Page::Settings, 2) => "setup_system",
                (Page::More, 0) => "help",
                (Page::More, 1) => "about",
                _ => "",
            };
            if !super::appearance_ui::feature_visible(ui.ctx(), &self.settings, key) {
                continue;
            }
            let i = visible_index;
            visible_index += 1;
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
            if response.contains_pointer() && enabled {
                ui.painter().rect_filled(
                    r,
                    0,
                    theme::palette(ui.ctx()).light.gamma_multiply(100.0 / 255.0),
                );
            }
            if self.page == Page::Launch && tab == 2 {
                self.instance_settings_sidebar_action(ui, r, &response);
            }
            if self.page == Page::Settings {
                self.settings_sidebar_action(ui, r, &response, tab);
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

    fn settings_sidebar_action(
        &mut self,
        ui: &mut egui::Ui,
        row: egui::Rect,
        row_response: &egui::Response,
        tab: usize,
    ) {
        use pcl_core::config::LauncherResetScope;
        let scope = match tab {
            0 => LauncherResetScope::Launch,
            1 => LauncherResetScope::Personalization,
            _ => LauncherResetScope::Other,
        };
        let enabled = self.busy.is_none() && !self.jobs.is_active() && self.game_pid.is_none();
        let rect = egui::Rect::from_center_size(
            egui::pos2(row.right() - 17.5, row.center().y),
            Vec2::splat(25.0),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        if !enabled {
            child.disable();
        }
        let response = child.interact(
            rect,
            ui.id().with(("settings-page-reset", tab)),
            egui::Sense::click(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                enabled,
                format!("初始化{}设置", scope.label()),
            )
        });
        let mut all = false;
        response.context_menu(|ui| {
            if ui
                .add_enabled(enabled, egui::Button::new("初始化全部偏好…"))
                .clicked()
            {
                all = true;
                ui.close();
            }
        });
        if row_response.contains_pointer()
            || row_response.has_focus()
            || response.has_focus()
            || response.context_menu_opened()
        {
            let palette = theme::palette(ui.ctx());
            paint_instance_reset(
                ui,
                rect.shrink(5.75),
                if enabled {
                    palette.accent
                } else {
                    super::MUTED
                },
            );
        }
        if response.on_hover_text("初始化").clicked() {
            self.request_settings_reset(scope);
        }
        if all {
            self.request_settings_reset(LauncherResetScope::All);
        }
    }

    fn instance_settings_sidebar_action(
        &mut self,
        ui: &mut egui::Ui,
        row: egui::Rect,
        row_response: &egui::Response,
    ) {
        let instance = self.settings.selected_version.clone();
        let writable = self.busy.is_none()
            && self.game_pid.is_none()
            && !self.jobs.conflicts_with(&self.settings.game_root)
            && instance.is_some();
        // MyListItem.Buttons: 25 DIP button, 5 DIP right margin, vertically
        // centered in the 36 DIP row. Its reserved MinPaddingRight is 35 DIP.
        let rect = egui::Rect::from_center_size(
            egui::pos2(row.right() - 17.5, row.center().y),
            Vec2::splat(25.0),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        if !writable {
            child.disable();
        }
        let response = child.interact(
            rect,
            ui.id().with((
                "instance-settings-reset",
                &self.settings.game_root,
                &instance,
            )),
            egui::Sense::click(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, writable, "初始化版本设置")
        });
        let mut restore = false;
        response.context_menu(|ui| {
            if ui
                .add_enabled(writable, egui::Button::new("恢复版本设置…"))
                .clicked()
            {
                restore = true;
                ui.close();
            }
        });
        let visible = row_response.contains_pointer()
            || row_response.has_focus()
            || response.has_focus()
            || response.context_menu_opened();
        if visible {
            let palette = theme::palette(ui.ctx());
            let color = if !writable {
                Color32::from_gray(160)
            } else if response.hovered() || response.has_focus() {
                palette.accent
            } else {
                palette.text
            };
            if response.hovered() && writable {
                ui.painter()
                    .circle_filled(rect.center(), 12.5, palette.light);
            }
            paint_instance_reset(ui, rect.shrink(5.75), color);
        }
        let response = response
            .on_hover_text("初始化")
            .on_disabled_hover_text("当前有任务或游戏正在使用此版本，暂时无法初始化或恢复设置。");
        if response.clicked() && writable {
            self.tools_tab = 2;
            self.confirm_instance_reset(ui.ctx(), instance.as_deref().unwrap());
        }
        if restore && writable {
            self.restore_instance_preferences(instance.as_deref().unwrap());
        }
    }

    fn folder_sidebar(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let mut roots = vec![self.settings.game_root.clone()];
        for path in &self.settings.game_roots {
            if !roots.contains(path) {
                roots.push(path.clone());
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
                            let title = pcl_core::config::game_root_label(&self.settings, root);
                            response.context_menu(|ui| {
                                let can_edit =
                                    !self.jobs.conflicts_with(root) && self.game_pid.is_none();
                                if ui
                                    .add_enabled(can_edit, egui::Button::new("重命名"))
                                    .clicked()
                                {
                                    self.folder_ui.name = title.clone();
                                    self.folder_ui.pending = Some((root.clone(), false));
                                    ui.close();
                                }
                                if ui
                                    .add_enabled(can_edit, egui::Button::new("从列表移除"))
                                    .clicked()
                                {
                                    self.folder_ui.pending = Some((root.clone(), true));
                                    ui.close();
                                }
                                if ui.button("打开文件夹").clicked() {
                                    if let Err(e) = crate::process::open_folder(root) {
                                        self.error = Some(e.to_string());
                                    }
                                    ui.close();
                                }
                            });
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
                        for (label, import) in [
                            ("新建文件夹", false),
                            ("添加已有文件夹", false),
                            ("导入整合包", true),
                        ] {
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
                                if label == "新建文件夹" {
                                    self.new_game_folder();
                                } else if import {
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
fn custom_title_rect(rect: egui::Rect, navigation_start: f32, mode: u8) -> egui::Rect {
    let min = rect.min + Vec2::new(if mode == 3 { 7.0 } else { 18.0 }, 6.0);
    egui::Rect::from_min_size(
        min,
        Vec2::new((navigation_start - 5.0 - min.x).max(0.0), 36.0),
    )
}

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

// PageInstanceLeft.xaml ItemSetup Reset logo, using its original 0.9 scale.
fn paint_instance_reset(ui: &egui::Ui, rect: egui::Rect, color: Color32) {
    const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1051" height="1022" viewBox="0 0 1051 1022"><path fill="white" d="M530 0c287 0 521 229 521 511s-233 511-521 511c-233 0-436-151-500-368a63 63 0 0 1 44-79 65 65 0 0 1 80 43c48 162 200 276 375 276 215 0 390-171 390-383s-174-383-390-383c-103 0-199 39-270 106l21-5a63 63 0 0 1 33 123l-157 42a65 65 0 0 1-90-42l-49-183a65 65 0 1 1 126-33l6 26A524 524 0 0 1 530 0z"/></svg>"#;
    let key = egui::Id::new("instance-sidebar-reset-texture");
    let texture = ui
        .ctx()
        .data(|data| data.get_temp::<egui::TextureHandle>(key))
        .unwrap_or_else(|| {
            let tree = resvg::usvg::Tree::from_str(SVG, &resvg::usvg::Options::default())
                .expect("fixed upstream reset SVG");
            let bounds = tree.root().abs_bounding_box();
            let scale = 64.0 / bounds.width().max(bounds.height());
            let mut pixels = resvg::tiny_skia::Pixmap::new(64, 64).unwrap();
            resvg::render(
                &tree,
                resvg::usvg::Transform::from_scale(scale, scale)
                    .pre_translate(-bounds.x(), -bounds.y()),
                &mut pixels.as_mut(),
            );
            let texture = ui.ctx().load_texture(
                "instance-sidebar-reset",
                egui::ColorImage::from_rgba_premultiplied([64, 64], pixels.data()),
                egui::TextureOptions::LINEAR,
            );
            ui.ctx()
                .data_mut(|data| data.insert_temp(key, texture.clone()));
            texture
        });
    ui.painter().image(
        texture.id(),
        rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        color,
    );
}

#[cfg(test)]
mod navigation_tests {
    use super::*;

    fn instance_sidebar_context() -> egui::Context {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        fonts.families.insert(
            egui::FontFamily::Name("PCL Bold".into()),
            fonts.families[&egui::FontFamily::Proportional].clone(),
        );
        ctx.set_fonts(fonts);
        ctx
    }

    fn sidebar_frame(
        app: &mut Launcher,
        ctx: &egui::Context,
        frame: usize,
        events: Vec<egui::Event>,
        dialog: bool,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(989.0, 517.0),
                )),
                time: Some(frame as f64 * 0.25),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::TopBottomPanel::top("source-title")
                    .exact_height(48.0)
                    .frame(egui::Frame::NONE)
                    .show(ctx, |_| {});
                app.sidebar(ctx);
                if dialog {
                    app.version_management_dialog(ctx);
                }
            },
        )
    }

    fn sidebar_pointer(
        point: egui::Pos2,
        button: egui::PointerButton,
        pressed: bool,
    ) -> egui::Event {
        egui::Event::PointerButton {
            pos: point,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn has_reset_icon(output: &egui::FullOutput) -> bool {
        output.shapes.iter().any(|shape| match &shape.shape {
            egui::Shape::Mesh(mesh) => {
                let rect = mesh.calc_bounds();
                rect.contains(egui::pos2(120.5, 114.0)) && (rect.width() - 13.5).abs() < 0.1
            }
            _ => false,
        })
    }

    fn has_text(output: &egui::FullOutput, expected: &str) -> bool {
        fn contains(shape: &egui::Shape, expected: &str) -> bool {
            match shape {
                egui::Shape::Text(text) => text.galley.text().contains(expected),
                egui::Shape::Vec(shapes) => shapes.iter().any(|shape| contains(shape, expected)),
                _ => false,
            }
        }
        output
            .shapes
            .iter()
            .any(|shape| contains(&shape.shape, expected))
    }

    fn reset_confirmation_is_focused(ctx: &egui::Context) -> bool {
        // The animated modal tessellates its text into meshes. Its settled
        // confirmation button is a real focusable widget, not a text shape.
        let button =
            egui::Id::new(("pcl-modal", "version-management-warning")).with(("button", 0_usize));
        ctx.memory(|memory| memory.has_focus(button))
    }

    #[test]
    fn instance_reset_hover_survives_child_hit_and_opens_confirmation_without_writing() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(directory.path());
        app.version_tools = true;
        app.settings.selected_version = Some("fixture".into());
        let before = std::fs::read(&app.settings_path).unwrap();
        let ctx = instance_sidebar_context();
        let point = egui::pos2(120.5, 114.0);
        let first = sidebar_frame(&mut app, &ctx, 0, vec![], false);
        assert!(
            !has_reset_icon(&first),
            "reset remains hidden outside the row"
        );
        let row = sidebar_frame(
            &mut app,
            &ctx,
            1,
            vec![egui::Event::PointerMoved(egui::pos2(62.0, 114.0))],
            false,
        );
        assert!(has_reset_icon(&row));
        for frame in 2..=3 {
            let child = sidebar_frame(
                &mut app,
                &ctx,
                frame,
                vec![egui::Event::PointerMoved(point)],
                false,
            );
            assert!(
                has_reset_icon(&child),
                "hovering the child must retain the row action"
            );
        }
        let _ = sidebar_frame(
            &mut app,
            &ctx,
            4,
            vec![sidebar_pointer(point, egui::PointerButton::Primary, true)],
            false,
        );
        let _ = sidebar_frame(
            &mut app,
            &ctx,
            5,
            vec![sidebar_pointer(point, egui::PointerButton::Primary, false)],
            true,
        );
        let _ = sidebar_frame(&mut app, &ctx, 6, vec![], true);
        let _ = sidebar_frame(&mut app, &ctx, 7, vec![], true);
        assert_eq!(app.tools_tab, 2);
        assert!(reset_confirmation_is_focused(&ctx));
        assert!(
            !app.jobs.is_active(),
            "confirmation cannot start a writer before approval"
        );
        assert_eq!(std::fs::read(&app.settings_path).unwrap(), before);
        assert!(!app.settings.game_root.join("versions").exists());
    }

    #[test]
    fn instance_reset_right_click_keeps_restore_in_the_menu_without_a_footer_action() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(directory.path());
        app.version_tools = true;
        app.settings.selected_version = Some("fixture".into());
        let ctx = instance_sidebar_context();
        let point = egui::pos2(120.5, 114.0);
        let _ = sidebar_frame(
            &mut app,
            &ctx,
            0,
            vec![egui::Event::PointerMoved(point)],
            false,
        );
        let _ = sidebar_frame(
            &mut app,
            &ctx,
            1,
            vec![sidebar_pointer(point, egui::PointerButton::Secondary, true)],
            false,
        );
        let _ = sidebar_frame(
            &mut app,
            &ctx,
            2,
            vec![sidebar_pointer(
                point,
                egui::PointerButton::Secondary,
                false,
            )],
            false,
        );
        let menu = sidebar_frame(
            &mut app,
            &ctx,
            3,
            vec![egui::Event::PointerMoved(egui::pos2(190.0, 140.0))],
            false,
        );
        assert!(has_text(&menu, "恢复版本设置"));
        assert!(
            has_reset_icon(&menu),
            "the action remains visible while its context menu is open"
        );
        assert!(!app.jobs.is_active());
    }

    #[test]
    fn instance_reset_is_disabled_for_busy_game_or_conflicting_writer() {
        for gate in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let mut app = super::super::event_tests::fixture(directory.path());
            app.version_tools = true;
            app.settings.selected_version = Some("fixture".into());
            match gate {
                0 => app.busy = Some("fixture operation".into()),
                1 => app.game_pid = Some(123),
                _ => {
                    let _ = app
                        .start_download_job("fixture writer", Some("fixture".into()))
                        .unwrap();
                }
            }
            let ctx = instance_sidebar_context();
            let point = egui::pos2(120.5, 114.0);
            let _ = sidebar_frame(
                &mut app,
                &ctx,
                0,
                vec![egui::Event::PointerMoved(point)],
                false,
            );
            let _ = sidebar_frame(
                &mut app,
                &ctx,
                1,
                vec![sidebar_pointer(point, egui::PointerButton::Primary, true)],
                false,
            );
            let _ = sidebar_frame(
                &mut app,
                &ctx,
                2,
                vec![sidebar_pointer(point, egui::PointerButton::Primary, false)],
                false,
            );
            app.busy = None;
            app.game_pid = None;
            app.jobs = Default::default();
            let _ = sidebar_frame(&mut app, &ctx, 3, vec![], true);
            let _ = sidebar_frame(&mut app, &ctx, 4, vec![], true);
            let _ = sidebar_frame(&mut app, &ctx, 5, vec![], true);
            assert!(
                !reset_confirmation_is_focused(&ctx),
                "gate {gate} must not create pending confirmation"
            );
            assert!(!app.settings.game_root.join("versions").exists());
        }
    }

    #[test]
    fn hiding_active_subpages_falls_back_and_f12_restores_access() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        let ctx = egui::Context::default();
        app.page = Page::Settings;
        app.settings_tab = 1;
        app.settings.ui_hidden_pages = vec!["setup_ui".into()];
        app.normalize_visible_navigation(&ctx);
        assert_eq!(app.settings_tab, 0);
        app.settings
            .ui_hidden_pages
            .extend(["setup_launch".into(), "setup_system".into()]);
        app.normalize_visible_navigation(&ctx);
        assert!(app.page == Page::Launch);
        app.page = Page::More;
        app.more.tab = 1;
        app.settings.ui_hidden_pages.push("about".into());
        app.normalize_visible_navigation(&ctx);
        assert_eq!(app.more.tab, 0);
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("pcl-reveal-hidden"), true));
        app.page = Page::Settings;
        app.settings_tab = 1;
        app.normalize_visible_navigation(&ctx);
        assert!(app.page == Page::Settings);
        assert_eq!(app.settings_tab, 1);
    }
    #[test]
    fn title_uses_all_available_space_without_overlapping_navigation() {
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 20.0), Vec2::new(900.0, 48.0));
        let title = custom_title_rect(rect, 330.0, 2);
        assert!(title.width() > 150.0);
        assert_eq!(title.right(), 325.0);
        assert_eq!(title.height(), 36.0);
        assert_eq!(custom_title_rect(rect, 45.0, 2).width(), 0.0);
    }
}
