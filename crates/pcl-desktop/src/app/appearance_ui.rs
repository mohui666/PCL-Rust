//! PageSetupUI geometry and local, persisted background controls.
#[path = "background_effect.rs"]
mod background_effect;
use super::Launcher;
use crate::ui_style;
use anyhow::{bail, Context, Result};
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Vec2};
use pcl_core::config::{self, Settings};
use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
};

const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_IMAGE_SIDE: u32 = 4096;

#[derive(Default)]
pub(super) struct AppearanceState {
    initialized: bool,
    background: Option<LoadedBackground>,
    pending_blur: Option<PendingBlur>,
    blurred: Option<(background_effect::BlurKey, egui::TextureHandle)>,
    failed_blur: Option<background_effect::BlurKey>,
    startup_initialized: bool,
    startup: Option<(f64, egui::TextureHandle)>,
}

struct PendingBlur {
    key: background_effect::BlurKey,
    receiver: mpsc::Receiver<egui::ColorImage>,
}

struct LoadedBackground {
    original: Arc<egui::ColorImage>,
    texture: egui::TextureHandle,
    source: PathBuf,
    size: [usize; 2],
}

impl AppearanceState {
    /// Poll one background worker, then schedule only the newest requested effect.
    /// Image/fit/size changes cannot apply the previous image's result.
    pub(super) fn ensure_loaded(
        &mut self,
        ctx: &egui::Context,
        settings: &Settings,
        settings_path: &Path,
    ) -> Result<()> {
        if !self.initialized {
            self.initialized = true;
            self.background =
                load_background(ctx, &background_folder(settings, settings_path), None)?;
        }
        let desired = self
            .background
            .as_ref()
            .map(|background| background_key(background, ctx.content_rect(), settings));
        if let Some(pending) = &self.pending_blur {
            match pending.receiver.try_recv() {
                Ok(image) => {
                    if Some(pending.key) == desired && pending.key.blur > 0 {
                        self.blurred = Some((
                            pending.key,
                            ctx.load_texture(
                                "pcl-background-blurred",
                                image,
                                egui::TextureOptions::LINEAR,
                            ),
                        ));
                    }
                    self.pending_blur = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    let failed = self.pending_blur.take().expect("pending worker").key;
                    self.failed_blur = Some(failed);
                    if Some(failed) == desired {
                        bail!("背景模糊处理未完成，已保留原图片");
                    }
                }
            }
        }
        if let (Some(key), Some(background)) = (desired, &self.background) {
            if key.blur > 0
                && self.pending_blur.is_none()
                && self
                    .blurred
                    .as_ref()
                    .is_none_or(|(current, _)| *current != key)
                && self.failed_blur != Some(key)
            {
                let source = Arc::clone(&background.original);
                let (sender, receiver) = mpsc::channel();
                let repaint = ctx.clone();
                if let Err(error) = std::thread::Builder::new()
                    .name("pcl-background-blur".into())
                    .spawn(move || {
                        let result = background_effect::blurred(&source, key);
                        let _ = sender.send(result);
                        repaint.request_repaint();
                    })
                {
                    self.failed_blur = Some(key);
                    return Err(error).context("无法开始背景模糊处理，已保留原图片");
                }
                self.pending_blur = Some(PendingBlur { key, receiver });
            }
        }
        Ok(())
    }

    /// Replaces ui_style::background. Draw before the title/sidebar/content layers.
    pub(super) fn paint_background(
        &self,
        painter: &egui::Painter,
        screen: Rect,
        settings: &Settings,
    ) {
        if settings.ui_background_colorful {
            ui_style::background(painter, screen);
        } else {
            painter.rect_filled(screen, 6.0, Color32::from_gray(245));
        }
        let Some(background) = &self.background else {
            return;
        };
        // Upstream ImgBack occupies Grid.Row=1: the title keeps its own brush.
        let body = Rect::from_min_max(screen.min + Vec2::new(0.0, 48.0), screen.max);
        if body.width() <= 0.0 || body.height() <= 0.0 {
            return;
        }
        let key = background_key(background, screen, settings);
        let area = body.expand(key.margin());
        let (texture, map) = match self
            .blurred
            .as_ref()
            .filter(|(current, _)| *current == key && key.blur > 0)
        {
            Some((_, texture)) => (
                texture.id(),
                background_effect::Placement {
                    rect: area,
                    uv: Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    repeat: false,
                },
            ),
            None => (
                background.texture.id(),
                background_effect::placement(
                    background.size,
                    area,
                    background_effect::resolved_fit(
                        background.size,
                        body.size(),
                        settings.ui_background_fit,
                    ),
                ),
            ),
        };
        let points = background_effect::clip_polygon(rounded_body_points(body, 6.0), map.rect);
        if points.len() < 3 {
            return;
        }
        let center = points
            .iter()
            .fold(Vec2::ZERO, |sum, point| sum + point.to_vec2())
            / points.len() as f32;
        let center = Pos2::new(center.x, center.y);
        let color =
            Color32::WHITE.gamma_multiply(f32::from(settings.ui_background_opacity) / 1000.0);
        let mut mesh = egui::Mesh::with_texture(texture);
        mesh.vertices.push(egui::epaint::Vertex {
            pos: center,
            uv: map.uv_at(center),
            color,
        });
        for point in &points {
            mesh.vertices.push(egui::epaint::Vertex {
                pos: *point,
                uv: map.uv_at(*point),
                color,
            });
        }
        for index in 0..points.len() {
            mesh.add_triangle(0, index as u32 + 1, ((index + 1) % points.len()) as u32 + 1);
        }
        painter.add(egui::Shape::mesh(mesh));
    }

    /// Upstream closes its native pre-window SplashScreen over 400 ms. Here the
    /// existing icon fades once after the Rust window opens; this painter neither
    /// registers hit regions nor delays launch, cancellation, or modal actions.
    pub(super) fn paint_startup_logo(&mut self, ctx: &egui::Context, settings: &Settings) {
        let now = ctx.input(|input| input.time);
        if !self.startup_initialized {
            self.startup_initialized = true;
            if settings.ui_launcher_logo {
                let bytes = include_bytes!("../../assets/icon.png");
                if let Ok(image) = decode_background(bytes) {
                    self.startup = Some((
                        now,
                        ctx.load_texture("pcl-startup-logo", image, egui::TextureOptions::LINEAR),
                    ));
                }
            }
        }
        let Some((started, texture)) = &self.startup else {
            return;
        };
        let opacity = startup_opacity(now - started);
        if opacity <= 0.0 {
            self.startup = None;
            return;
        }
        let screen = ctx.content_rect();
        let size = 256.0_f32.min(screen.width()).min(screen.height()).max(0.0);
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("pcl-startup-logo"),
        ))
        .image(
            texture.id(),
            Rect::from_center_size(screen.center(), Vec2::splat(size)),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE.gamma_multiply(opacity),
        );
        ctx.request_repaint();
    }
}

fn startup_opacity(age: f64) -> f32 {
    (1.0 - age / 0.4).clamp(0.0, 1.0) as f32
}

fn background_key(
    background: &LoadedBackground,
    screen: Rect,
    settings: &Settings,
) -> background_effect::BlurKey {
    background_effect::BlurKey {
        source: background.texture.id(),
        body: [
            screen.width().round().max(1.0) as u32,
            (screen.height() - 48.0).round().max(1.0) as u32,
        ],
        fit: settings.ui_background_fit,
        blur: settings.ui_background_blur,
    }
}

impl Launcher {
    pub(super) fn appearance_page(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut open_background = false;
        let mut choose_background = false;
        let mut refresh_background = false;
        let mut open_home = false;
        let mut theme_settings = self.settings.clone();
        let mut theme_changed = false;
        let has_background = self.appearance.background.is_some();
        let custom = theme_settings.ui_theme == 14;
        let folder = background_folder(&self.settings, &self.settings_path);

        section(
            ui,
            "基础",
            if custom { 260.0 } else { 200.0 },
            |ui, rect| {
                label(ui, rect.min + Vec2::new(25.0, 40.0), 90.0, "不透明度", 13.0);
                let slider_rect = Rect::from_min_size(
                    rect.min + Vec2::new(97.0, 40.0),
                    Vec2::new((rect.width() - 122.0).max(20.0), 16.0),
                );
                let mut opacity = f32::from(theme_settings.ui_launcher_opacity);
                let mut opacity_ui = ui.new_child(egui::UiBuilder::new().max_rect(slider_rect));
                if !cfg!(any(target_os = "macos", target_os = "windows")) {
                    opacity_ui.disable();
                }
                theme_changed |= slider_track(
                    &mut opacity_ui,
                    slider_rect,
                    "窗口不透明度",
                    &mut opacity,
                    40.0,
                    100.0,
                    1.0,
                )
                .on_disabled_hover_text("此系统暂不支持设置窗口整体不透明度。")
                .changed();
                theme_settings.ui_launcher_opacity = opacity as u16;

                let theme_offset = if custom { 60.0 } else { 0.0 };
                if custom {
                    let half = (rect.width() - 50.0) / 2.0;
                    for (row, label_text, value, maximum) in [
                        (0, "色调", &mut theme_settings.ui_theme_hue, 360.0),
                        (1, "色调渐变", &mut theme_settings.ui_theme_gradient, 180.0),
                    ] {
                        theme_changed |= theme_slider(
                            ui,
                            rect.min + Vec2::new(25.0, 69.0 + row as f32 * 30.0),
                            half - 15.0,
                            label_text,
                            value,
                            maximum,
                        )
                        .changed();
                    }
                    for (row, label_text, value, maximum) in [
                        (0, "饱和度", &mut theme_settings.ui_theme_saturation, 100.0),
                        (1, "亮度", &mut theme_settings.ui_theme_lightness, 40.0),
                    ] {
                        theme_changed |= theme_slider(
                            ui,
                            rect.min + Vec2::new(25.0 + half, 69.0 + row as f32 * 30.0),
                            half,
                            label_text,
                            value,
                            maximum,
                        )
                        .changed();
                    }
                }
                let theme_left = rect.left() + 97.0;
                let theme_column = (rect.width() - 122.0) / 5.0;
                for (row, ids) in [[0, 1, 2, 3, 4], [5, 12, 6, 7, 13], [8, 9, 10, 11, 14]]
                    .iter()
                    .enumerate()
                {
                    let y = rect.top() + 77.0 + theme_offset + row as f32 * 30.0;
                    if row < 2 {
                        ui.painter().text(
                            Pos2::new(rect.left() + 25.0, y),
                            egui::Align2::LEFT_CENTER,
                            if row == 0 { "主题" } else { "隐藏主题" },
                            egui::FontId::proportional(13.0),
                            crate::theme::palette(ui.ctx()).text,
                        );
                    }
                    for (column, id) in ids.iter().enumerate() {
                        let radio_rect = Rect::from_min_size(
                            Pos2::new(theme_left + column as f32 * theme_column, y - 13.0),
                            Vec2::new(theme_column, 26.0),
                        );
                        if theme_radio(ui, radio_rect, *id, theme_settings.ui_theme == *id)
                            .clicked()
                        {
                            theme_settings.ui_theme = *id;
                            theme_changed = true;
                        }
                    }
                }
                theme_changed |= appearance_checkbox(
                    ui,
                    Rect::from_min_size(
                        rect.min + Vec2::new(24.0, 161.0 + theme_offset),
                        Vec2::new(rect.width() - 49.0, 22.0),
                    ),
                    &mut theme_settings.ui_launcher_logo,
                    "打开启动器时显示 PCL 图标",
                )
                .on_hover_text("下次打开启动器时生效。")
                .changed();
            },
        );

        section(
            ui,
            "背景图片",
            if has_background { 227.0 } else { 128.0 },
            |ui, rect| {
                if has_background {
                    label(
                        ui,
                        rect.min + Vec2::new(25.0, 43.0),
                        90.0,
                        "自适应方式",
                        13.0,
                    );
                    let mut combo_ui =
                        ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                            rect.min + Vec2::new(115.0, 40.0),
                            Vec2::new((rect.width() - 140.0).max(80.0), 28.0),
                        )));
                    let selected = background_effect::FITS
                        .iter()
                        .find(|(id, _)| *id == theme_settings.ui_background_fit)
                        .map_or("自动", |(_, name)| *name);
                    ui_style::PclComboBox::from_id_salt("background-fit")
                        .width((rect.width() - 140.0).max(80.0))
                        .selected_text(selected)
                        .show_ui(&mut combo_ui, |ui| {
                            for (id, name) in background_effect::FITS {
                                theme_changed |= ui
                                    .selectable_value(
                                        &mut theme_settings.ui_background_fit,
                                        id,
                                        name,
                                    )
                                    .changed();
                            }
                        });
                    for (title, y, maximum, step, value) in [
                        (
                            "不透明度",
                            81.0,
                            1000.0,
                            10.0,
                            f32::from(theme_settings.ui_background_opacity),
                        ),
                        (
                            "背景模糊",
                            110.0,
                            40.0,
                            1.0,
                            f32::from(theme_settings.ui_background_blur),
                        ),
                    ] {
                        let mut value = value;
                        label(ui, rect.min + Vec2::new(25.0, y - 3.0), 90.0, title, 13.0);
                        let changed = slider_track(
                            ui,
                            Rect::from_min_size(
                                rect.min + Vec2::new(115.0, y),
                                Vec2::new((rect.width() - 140.0).max(25.0), 16.0),
                            ),
                            title,
                            &mut value,
                            0.0,
                            maximum,
                            step,
                        )
                        .changed();
                        theme_changed |= changed;
                        if maximum == 1000.0 {
                            theme_settings.ui_background_opacity = value as u16;
                        } else {
                            theme_settings.ui_background_blur = value as u8;
                        }
                    }
                }
                let offset = if has_background { 99.0 } else { 0.0 };
                theme_changed |= appearance_checkbox(
                    ui,
                    Rect::from_min_size(
                        rect.min + Vec2::new(24.0, 40.0 + offset),
                        Vec2::new(300.0, 22.0),
                    ),
                    &mut theme_settings.ui_background_colorful,
                    "叠加彩色背景",
                )
                .on_hover_text("切换默认渐变底色；加载的图片位于底色上方，透明图片可透出底色。")
                .changed();
                let open = action_button(
                    ui,
                    rect.min + Vec2::new(25.0, 73.0 + offset),
                    "打开文件夹",
                    true,
                );
                open_background = open.clicked();
                open.clone().on_hover_text(format!(
                "{}\n放入 PNG、JPEG、WebP 或 GIF 后点击刷新。\n右键可选择其他文件夹；GIF 当前只显示首帧。\n图片限制：32 MiB、4096 × 4096 像素。",
                folder.display()
            ));
                open.context_menu(|ui| {
                    if ui.button("选择其他背景文件夹…").clicked() {
                        choose_background = true;
                        ui.close();
                    }
                });
                refresh_background = action_button(
                    ui,
                    rect.min + Vec2::new(185.0, 73.0 + offset),
                    "刷新背景图片",
                    true,
                )
                .on_hover_text("重新读取所选文件夹，按文件名依次切换图片；文件不会被修改或删除。")
                .clicked();
            },
        );

        section(ui, "背景音乐", 97.0, |ui, rect| {
            action_button(ui, rect.min + Vec2::new(25.0, 42.0), "打开文件夹", false)
                .on_hover_text("背景音乐播放尚未迁移，当前不创建音乐目录或假报播放成功。");
            action_button(ui, rect.min + Vec2::new(185.0, 42.0), "刷新背景音乐", false)
                .on_hover_text("背景音乐播放尚未迁移。");
        });

        section(ui, "主页", 112.0, |ui, rect| {
            label(
                ui,
                rect.min + Vec2::new(25.0, 39.0),
                rect.width() - 50.0,
                "自定义主页渲染尚未迁移；可打开本地文件夹管理素材。",
                13.0,
            );
            open_home = action_button(ui, rect.min + Vec2::new(25.0, 62.0), "打开主页文件夹", true)
                .clicked();
        });

        if theme_changed {
            match config::save_settings(&self.settings_path, &theme_settings) {
                Ok(()) => {
                    self.settings = theme_settings;
                    crate::theme::apply(ui.ctx(), &self.settings);
                    self.status = "外观设置已保存".into();
                    self.error = None;
                    ui.ctx().request_repaint();
                }
                Err(error) => self.error = Some(format!("保存外观设置失败：{error:#}")),
            }
        }
        if open_background {
            let result =
                fs::create_dir_all(&folder).and_then(|_| crate::process::open_folder(&folder));
            if let Err(error) = result {
                self.error = Some(format!("打开背景文件夹失败：{error}"));
            }
        }
        if choose_background {
            if let Some(selected) = rfd::FileDialog::new()
                .set_title("选择背景图片文件夹")
                .set_directory(&folder)
                .pick_folder()
            {
                let result = (|| {
                    let selected = selected.canonicalize().context("无法访问所选背景文件夹")?;
                    let loaded = load_background(ui.ctx(), &selected, None)?;
                    let mut next = self.settings.clone();
                    next.ui_background_folder = Some(selected.clone());
                    config::save_settings(&self.settings_path, &next)?;
                    self.settings = next;
                    self.appearance.background = loaded;
                    self.status = format!("背景文件夹已保存：{}", selected.display());
                    self.error = None;
                    anyhow::Ok(())
                })();
                if let Err(error) = result {
                    self.error = Some(format!("更改背景文件夹失败：{error:#}"));
                }
            }
        }
        if refresh_background {
            let previous = self
                .appearance
                .background
                .as_ref()
                .map(|background| background.source.as_path());
            match load_background(ui.ctx(), &folder, previous) {
                Ok(loaded) => {
                    self.status = loaded.as_ref().map_or_else(
                        || "未检测到背景图片，已显示默认底色".into(),
                        |background| format!("背景图片已刷新：{}", background.source.display()),
                    );
                    self.appearance.background = loaded;
                    self.error = None;
                }
                Err(error) => {
                    self.error = Some(format!("刷新背景图片失败，保留当前图片：{error:#}"))
                }
            }
        }
        if open_home {
            let folder = settings_directory(&self.settings_path).join("homepage");
            if let Err(error) =
                fs::create_dir_all(&folder).and_then(|_| crate::process::open_folder(&folder))
            {
                self.error = Some(format!("打开主页文件夹失败：{error}"));
            }
        }
    }
}

fn appearance_checkbox(
    ui: &mut egui::Ui,
    rect: Rect,
    checked: &mut bool,
    title: &str,
) -> egui::Response {
    // Shared source checkbox has a 26-DIP row; center it in PageSetupUI's 22-DIP slot.
    let mut child = ui.new_child(egui::UiBuilder::new().id_salt(title).max_rect(
        Rect::from_min_size(
            rect.min - Vec2::new(0.0, 2.0),
            Vec2::new(rect.width(), 26.0),
        ),
    ));
    ui_style::checkbox(&mut child, checked, title, "")
}

fn theme_radio(ui: &mut egui::Ui, rect: Rect, id: u8, selected: bool) -> egui::Response {
    let response = ui.interact(rect, ui.id().with(("theme", id)), egui::Sense::click());
    let name = crate::theme::theme_name(id);
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::RadioButton,
            ui.is_enabled(),
            selected,
            name,
        )
    });
    if response.is_pointer_button_down_on() {
        response.request_focus();
    }
    let palette = crate::theme::palette(ui.ctx());
    let color = if selected || response.hovered() || response.has_focus() {
        palette.accent
    } else {
        palette.text
    };
    let center = Pos2::new(rect.left() + 10.0, rect.center().y);
    ui.painter().circle(
        center,
        8.45,
        Color32::from_white_alpha(85),
        egui::Stroke::new(1.1_f32, color),
    );
    if selected {
        ui.painter().circle_filled(center, 4.5, color);
    }
    ui.painter().text(
        Pos2::new(rect.left() + 26.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(13.0),
        color,
    );
    response
}

fn theme_slider(
    ui: &mut egui::Ui,
    origin: Pos2,
    width: f32,
    title: &str,
    value: &mut f32,
    maximum: f32,
) -> egui::Response {
    label(ui, origin, 72.0, title, 13.0);
    let rect = Rect::from_min_size(
        origin + Vec2::new(72.0, 0.0),
        Vec2::new((width - 72.0).max(25.0), 16.0),
    );
    slider_track(
        ui,
        rect,
        title,
        value,
        0.0,
        maximum,
        if maximum == 360.0 { 10.0 } else { 5.0 },
    )
}

fn slider_track(
    ui: &mut egui::Ui,
    rect: Rect,
    title: &str,
    value: &mut f32,
    minimum: f32,
    maximum: f32,
    key_step: f32,
) -> egui::Response {
    let palette = crate::theme::palette(ui.ctx());
    let before = *value;
    let mut response = ui.interact(
        rect,
        ui.id().with(("appearance-slider", title)),
        egui::Sense::click_and_drag(),
    );
    let track = rect.shrink2(Vec2::new(5.0, 0.0));
    if let Some(pointer) = response.interact_pointer_pos().filter(|_| ui.is_enabled()) {
        *value = (minimum
            + ((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0) * (maximum - minimum))
            .round();
        response.request_focus();
    }
    let (mut decrease, mut increase) = (0, 0);
    if ui.is_enabled() && response.has_focus() {
        ui.ctx().memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    ..Default::default()
                },
            )
        });
        ui.input(|input| {
            decrease += input.num_presses(egui::Key::ArrowLeft);
            increase += input.num_presses(egui::Key::ArrowRight);
        });
    }
    ui.input(|input| {
        decrease +=
            input.num_accesskit_action_requests(response.id, egui::accesskit::Action::Decrement);
        increase +=
            input.num_accesskit_action_requests(response.id, egui::accesskit::Action::Increment);
    });
    if ui.is_enabled() {
        *value = (*value + (increase as f32 - decrease as f32) * key_step).clamp(minimum, maximum);
    }
    if before != *value {
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::slider(ui.is_enabled(), f64::from(*value), title));
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_min_numeric_value(f64::from(minimum));
        node.set_max_numeric_value(f64::from(maximum));
        node.set_numeric_value_step(f64::from(key_step));
        if ui.is_enabled() && *value > minimum {
            node.add_action(egui::accesskit::Action::Decrement);
        }
        if ui.is_enabled() && *value < maximum {
            node.add_action(egui::accesskit::Action::Increment);
        }
    });
    let center = Pos2::new(
        track.left() + track.width() * (*value - minimum) / (maximum - minimum),
        track.center().y,
    );
    let stroke = if response.hovered() || response.has_focus() {
        palette.accent
    } else {
        palette.control_border
    };
    ui.painter().line_segment(
        [track.left_center(), track.right_center()],
        egui::Stroke::new(1.0_f32, palette.control_border.gamma_multiply(0.3)),
    );
    ui.painter().line_segment(
        [track.left_center(), center],
        egui::Stroke::new(2.0_f32, stroke),
    );
    ui.painter().circle(
        center,
        4.4,
        palette.control_border,
        egui::Stroke::new(1.2_f32, stroke),
    );
    response.on_hover_text(if title == "不透明度" && maximum == 1000.0 {
        format!("{title}：{:.0}%", *value / 10.0)
    } else if title == "窗口不透明度" {
        format!("{title}：{value:.0}%")
    } else if title == "背景模糊" {
        format!("{title}：{value:.0} 像素")
    } else {
        format!("{title}：{value:.0}")
    })
}

fn settings_directory(settings_path: &Path) -> &Path {
    settings_path.parent().unwrap_or_else(|| Path::new("."))
}

fn background_folder(settings: &Settings, settings_path: &Path) -> PathBuf {
    settings
        .ui_background_folder
        .clone()
        .unwrap_or_else(|| settings_directory(settings_path).join("backgrounds"))
}

fn load_background(
    ctx: &egui::Context,
    folder: &Path,
    previous: Option<&Path>,
) -> Result<Option<LoadedBackground>> {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("读取背景文件夹 {}", folder.display()))
        }
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.context("读取背景文件夹条目失败")?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "webp" | "gif"
                )
            })
        {
            files.push(path);
        }
    }
    files.sort();
    if files.is_empty() {
        return Ok(None);
    }
    let index = previous
        .and_then(|path| files.iter().position(|candidate| candidate == path))
        .map_or(0, |index| (index + 1) % files.len());
    let path = files.swap_remove(index);
    let file = fs::File::open(&path).with_context(|| format!("打开背景图片 {}", path.display()))?;
    if file.metadata()?.len() > MAX_IMAGE_BYTES {
        bail!("背景图片超过 32 MiB：{}", path.display());
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES + 1).read_to_end(&mut bytes)?;
    let image =
        decode_background(&bytes).with_context(|| format!("解码背景图片 {}", path.display()))?;
    let size = image.size;
    Ok(Some(LoadedBackground {
        texture: ctx.load_texture(
            "pcl-background",
            image.clone(),
            egui::TextureOptions::LINEAR_REPEAT,
        ),
        original: Arc::new(image),
        source: path,
        size,
    }))
}

fn decode_background(bytes: &[u8]) -> Result<egui::ColorImage> {
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        bail!("背景图片超过 32 MiB");
    }
    let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let (width, height) = reader.into_dimensions().context("无法识别图片格式或尺寸")?;
    if width == 0 || height == 0 || width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
        bail!("背景图片尺寸必须在 1 至 4096 像素之间");
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(96 * 1024 * 1024);
    reader.limits(limits);
    let rgba = reader.decode().context("背景图片解码失败")?.to_rgba8();
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        &rgba,
    ))
}

fn rounded_body_points(rect: Rect, radius: f32) -> Vec<Pos2> {
    let radius = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let mut points = vec![rect.left_top(), rect.right_top()];
    for (center, start) in [
        (rect.right_bottom() - Vec2::splat(radius), 0.0_f32),
        (
            rect.left_bottom() + Vec2::new(radius, -radius),
            std::f32::consts::FRAC_PI_2,
        ),
    ] {
        for step in 0..=6 {
            let angle = start + step as f32 / 6.0 * std::f32::consts::FRAC_PI_2;
            points.push(center + Vec2::angled(angle) * radius);
        }
    }
    points
}

fn section(ui: &mut egui::Ui, title: &str, height: f32, body: impl FnOnce(&mut egui::Ui, Rect)) {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let frame = egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 245))
        .corner_radius(5)
        .shadow(egui::epaint::Shadow {
            offset: [0, 2],
            blur: 3,
            spread: 0,
            color: Color32::from_black_alpha(9),
        });
    ui.painter().add(frame.paint(rect));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(("appearance", title))
            .max_rect(rect),
    );
    ui_style::place_left(
        &mut child,
        Rect::from_min_size(
            rect.min + Vec2::new(15.0, 10.0),
            Vec2::new(rect.width() - 30.0, 20.0),
        ),
        egui::Label::new(ui_style::card_title(title)),
    );
    body(&mut child, rect);
    ui.add_space(15.0);
}

fn label(ui: &mut egui::Ui, position: Pos2, width: f32, text: &str, size: f32) {
    ui_style::place_left(
        ui,
        Rect::from_min_size(position, Vec2::new(width, 22.0)),
        egui::Label::new(
            RichText::new(text)
                .size(size)
                .color(crate::theme::palette(ui.ctx()).text),
        )
        .truncate(),
    );
}

fn action_button(ui: &mut egui::Ui, position: Pos2, text: &str, enabled: bool) -> egui::Response {
    ui_style::outline_button(
        ui,
        Rect::from_min_size(position, Vec2::new(140.0, 35.0)),
        text,
        None,
        false,
        enabled,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_background(ctx: &egui::Context) -> LoadedBackground {
        let original = egui::ColorImage::filled([32, 16], Color32::RED);
        LoadedBackground {
            texture: ctx.load_texture(
                "synthetic-background",
                original.clone(),
                egui::TextureOptions::LINEAR_REPEAT,
            ),
            original: Arc::new(original),
            source: PathBuf::from("synthetic.png"),
            size: [32, 16],
        }
    }

    #[test]
    fn background_worker_ignores_stale_results_then_reuses_matching_cache() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(128.0, 112.0));
        let mut state = AppearanceState {
            initialized: true,
            background: Some(synthetic_background(&ctx)),
            ..Default::default()
        };
        let settings = Settings {
            ui_background_blur: 3,
            ..Default::default()
        };
        let expected = background_key(state.background.as_ref().unwrap(), screen, &settings);
        let stale = background_effect::BlurKey { fit: 8, ..expected };
        let (sender, receiver) = mpsc::channel();
        sender
            .send(egui::ColorImage::filled([2, 2], Color32::BLUE))
            .unwrap();
        state.pending_blur = Some(PendingBlur {
            key: stale,
            receiver,
        });
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                state
                    .ensure_loaded(ctx, &settings, Path::new("unused.json"))
                    .unwrap();
            },
        );
        assert!(
            state.blurred.is_none(),
            "stale image must not become visible"
        );
        assert_eq!(state.pending_blur.as_ref().unwrap().key, expected);
        // The real worker computes a bounded image. Deliver its result back through
        // the normal polling path without relying on a timing-sensitive sleep.
        let pending = state.pending_blur.take().unwrap();
        let computed = pending
            .receiver
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        let (sender, receiver) = mpsc::channel();
        sender.send(computed).unwrap();
        state.pending_blur = Some(PendingBlur {
            key: expected,
            receiver,
        });
        for _ in 0..2 {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ctx| {
                    state
                        .ensure_loaded(ctx, &settings, Path::new("unused.json"))
                        .unwrap();
                },
            );
            assert_eq!(state.blurred.as_ref().unwrap().0, expected);
            assert!(
                state.pending_blur.is_none(),
                "unchanged effects must not spawn another worker"
            );
        }
    }

    #[test]
    fn background_mesh_uses_opacity_fit_and_body_only_clip() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(128.0, 112.0));
        let state = AppearanceState {
            initialized: true,
            background: Some(synthetic_background(&ctx)),
            ..Default::default()
        };
        let texture = state.background.as_ref().unwrap().texture.id();
        let settings = Settings {
            ui_background_opacity: 250,
            ui_background_fit: 1,
            ..Default::default()
        };
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                let painter = ctx.layer_painter(egui::LayerId::background());
                state.paint_background(&painter, screen, &settings);
            },
        );
        let mesh = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Mesh(mesh) if mesh.texture_id == texture => Some(mesh),
                _ => None,
            })
            .unwrap();
        assert!(mesh
            .vertices
            .iter()
            .all(|v| v.color.a() == 64 && v.pos.y >= 48.0));
        let bounds = mesh.calc_bounds();
        assert_eq!(
            bounds.size(),
            Vec2::new(32.0, 16.0),
            "center mode must retain native image size"
        );
        assert!(mesh
            .vertices
            .iter()
            .all(|v| (0.0..=1.0).contains(&v.uv.x) && (0.0..=1.0).contains(&v.uv.y)));
    }

    #[test]
    fn startup_fades_once_without_consuming_input_or_replaying_setting_changes() {
        let ctx = egui::Context::default();
        let mut state = AppearanceState::default();
        let mut settings = Settings::default();
        let mut draw = |time: f64, settings: &Settings| {
            ctx.run(
                egui::RawInput {
                    time: Some(time),
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let response = ui.button("available action");
                        assert!(response.enabled());
                    });
                    state.paint_startup_logo(ctx, settings);
                    assert!(!ctx.memory(|memory| memory.focused().is_some()));
                },
            )
        };
        let initial = draw(1.0, &settings);
        let fading = draw(1.2, &settings);
        assert!(initial.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Mesh(mesh) if mesh.vertices.iter().all(|v|v.color.a()==255))));
        assert!(fading.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Mesh(mesh) if mesh.vertices.iter().all(|v|v.color.a()==128))));
        let ended = draw(1.5, &settings);
        assert!(!ended
            .shapes
            .iter()
            .any(|shape| matches!(&shape.shape, egui::Shape::Mesh(_))));
        settings.ui_launcher_logo = false;
        let _ = draw(2.0, &settings);
        settings.ui_launcher_logo = true;
        let later = draw(3.0, &settings);
        assert!(!later
            .shapes
            .iter()
            .any(|shape| matches!(&shape.shape, egui::Shape::Mesh(_))));
        assert!(state.startup.is_none());
        let mut disabled = AppearanceState::default();
        settings.ui_launcher_logo = false;
        let _ = ctx.run(Default::default(), |ctx| {
            disabled.paint_startup_logo(ctx, &settings)
        });
        assert!(disabled.startup.is_none());
    }

    #[test]
    fn appearance_pointer_changes_persist_without_touching_other_settings() {
        let temporary = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(temporary.path());
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let fallback = fonts.families[&egui::FontFamily::Proportional].clone();
        fonts
            .families
            .insert(egui::FontFamily::Name("PCL Bold".into()), fallback);
        ctx.set_fonts(fonts);
        app.appearance.background = Some(synthetic_background(&ctx));
        let previous = app.settings.clone();
        let mut frame = |events| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(700.0, 800.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| app.appearance_page(ui));
                },
            );
        };
        frame(vec![]);
        // CentralPanel starts at (8,8), base height=200 + 15 gap, then bg card.
        // Click background opacity at track midpoint (x128..662, y312).
        let point = Pos2::new(395.0, 312.0);
        frame(vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        frame(vec![egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert_eq!(app.settings.ui_background_opacity, 500);
        let saved = config::load_settings(&app.settings_path).unwrap();
        assert_eq!(saved.ui_background_opacity, 500);
        let mut expected = previous;
        expected.ui_background_opacity = 500;
        assert_eq!(
            serde_json::to_value(&saved).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
    }

    #[test]
    fn custom_theme_slider_keeps_integer_pointer_values_and_source_keyboard_steps() {
        for (maximum, step) in [(360.0, 10.0), (180.0, 5.0), (100.0, 5.0), (40.0, 5.0)] {
            let ctx = egui::Context::default();
            let mut value = 13.0;
            let mut draw = |events, focus| {
                let mut response = None;
                let _ = ctx.run(
                    egui::RawInput {
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            let current = theme_slider(
                                ui,
                                Pos2::new(10., 10.),
                                300.,
                                "Slider",
                                &mut value,
                                maximum,
                            );
                            if focus {
                                current.request_focus();
                            }
                            response = Some(current);
                        });
                    },
                );
                (response.unwrap(), value)
            };
            let (response, _) = draw(vec![], true);
            assert_eq!(response.rect.height(), 16.0);
            let right = egui::Event::Key {
                key: egui::Key::ArrowRight,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            let (response, current) = draw(vec![right], false);
            assert!(response.changed());
            assert_eq!(current, 13.0 + step);
            let point = response.rect.right_center();
            let (_, current) = draw(
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                false,
            );
            assert_eq!(current, maximum);
        }
    }

    #[test]
    fn background_decode_preserves_dimensions_and_transparency_and_rejects_invalid_data() {
        let rgba = image::RgbaImage::from_pixel(2, 1, image::Rgba([24, 48, 72, 128]));
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let decoded = decode_background(encoded.get_ref()).unwrap();
        assert_eq!(decoded.size, [2, 1]);
        assert_eq!(
            decoded.pixels[0],
            Color32::from_rgba_unmultiplied(24, 48, 72, 128)
        );
        assert!(decode_background(b"not a picture").is_err());
        assert!(decode_background(&encoded.get_ref()[..20]).is_err());
        // A valid, highly compressible oversized PNG must be rejected before RGBA allocation.
        let rgba = image::RgbaImage::new(4097, 1);
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        assert!(decode_background(encoded.get_ref())
            .unwrap_err()
            .to_string()
            .contains("4096"));
    }

    #[test]
    fn cover_crop_keeps_aspect_and_centers_landscape_and_portrait() {
        let uv = background_effect::placement(
            [400, 200],
            Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)),
            2,
        )
        .uv;
        assert_eq!(
            uv,
            Rect::from_min_max(Pos2::new(0.25, 0.0), Pos2::new(0.75, 1.0))
        );
        let uv = background_effect::placement(
            [200, 400],
            Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)),
            2,
        )
        .uv;
        assert_eq!(
            uv,
            Rect::from_min_max(Pos2::new(0.0, 0.25), Pos2::new(1.0, 0.75))
        );
    }
}
