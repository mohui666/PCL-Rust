//! PageSetupUI geometry and local background controls. Unported effects stay disabled.
use super::Launcher;
use crate::ui_style;
use anyhow::{bail, Context, Result};
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Vec2};
use pcl_core::config::{self, Settings};
use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
};

const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_IMAGE_SIDE: u32 = 4096;

#[derive(Default)]
pub(super) struct AppearanceState {
    initialized: bool,
    background: Option<LoadedBackground>,
}

struct LoadedBackground {
    texture: egui::TextureHandle,
    source: PathBuf,
    size: [usize; 2],
}

impl AppearanceState {
    /// Call once before painting; a failed startup read is reported, not retried every frame.
    pub(super) fn ensure_loaded(
        &mut self,
        ctx: &egui::Context,
        settings: &Settings,
        settings_path: &Path,
    ) -> Result<()> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        self.background = load_background(ctx, &background_folder(settings, settings_path), None)?;
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
        if let Some(background) = &self.background {
            // Upstream ImgBack occupies Grid.Row=1: the title keeps its own blue brush.
            let rect = Rect::from_min_max(screen.min + Vec2::new(0.0, 48.0), screen.max);
            if rect.width() <= 0.0 || rect.height() <= 0.0 {
                return;
            }
            let uv = cover_uv(background.size, rect.size());
            // Mesh clips bottom rounded corners as well as the title boundary.
            let points = rounded_body_points(rect, 6.0);
            let mut mesh = egui::Mesh::with_texture(background.texture.id());
            let center = rect.center();
            let mapped_uv = |point: Pos2| {
                Pos2::new(
                    uv.left() + (point.x - rect.left()) / rect.width() * uv.width(),
                    uv.top() + (point.y - rect.top()) / rect.height() * uv.height(),
                )
            };
            mesh.vertices.push(egui::epaint::Vertex {
                pos: center,
                uv: mapped_uv(center),
                color: Color32::WHITE,
            });
            for point in &points {
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: *point,
                    uv: mapped_uv(*point),
                    color: Color32::WHITE,
                });
            }
            for index in 0..points.len() {
                mesh.add_triangle(0, index as u32 + 1, ((index + 1) % points.len()) as u32 + 1);
            }
            painter.add(egui::Shape::mesh(mesh));
        }
    }
}

impl Launcher {
    pub(super) fn appearance_page(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut open_background = false;
        let mut choose_background = false;
        let mut refresh_background = false;
        let mut open_home = false;
        let mut colorful = self.settings.ui_background_colorful;
        let old_colorful = colorful;
        let mut theme_settings = self.settings.clone();
        let mut theme_changed = false;
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
                // MySlider: 16 DIP high, 2 DIP foreground, a 10 DIP dot. The
                // original UiLauncherTransparent=600 means 100% (v/1000+0.4).
                // Native window opacity is not implemented yet: preserve geometry
                // without pretending that dragging changes the operating-system window.
                let slider_color = crate::theme::palette(ui.ctx()).border;
                let line_y = slider_rect.center().y;
                ui.painter().line_segment(
                    [
                        Pos2::new(slider_rect.left() + 1.0, line_y),
                        Pos2::new(slider_rect.right() - 8.5, line_y),
                    ],
                    egui::Stroke::new(2.0_f32, slider_color),
                );
                ui.painter().circle_filled(
                    Pos2::new(slider_rect.right() - 5.0, line_y),
                    5.0,
                    slider_color,
                );
                ui.interact(
                    slider_rect,
                    ui.id().with("window-opacity"),
                    egui::Sense::hover(),
                )
                .on_hover_text(
                    "窗口整体不透明度尚未迁移；当前固定为 100%。原版范围为 40% 至 100%。",
                );

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
                let mut logo = true;
                let mut checkbox_ui =
                    ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                        rect.min + Vec2::new(24.0, 161.0 + theme_offset),
                        Vec2::new(rect.width() - 49.0, 22.0),
                    )));
                checkbox_ui.disable();
                checkbox_ui
                .checkbox(&mut logo, "打开启动器时显示 PCL 图标")
                .on_disabled_hover_text("原版 UiLauncherLogo 默认为开启；此处显示原默认勾选。Rust 启动图标动画尚未迁移，当前不会播放动画。");
            },
        );

        section(ui, "背景图片", 128.0, |ui, rect| {
            ui_style::place_left(
                ui,
                Rect::from_min_size(rect.min + Vec2::new(24.0, 40.0), Vec2::new(300.0, 22.0)),
                egui::Checkbox::new(&mut colorful, "叠加彩色背景"),
            )
            .on_hover_text("切换默认渐变底色；加载的图片位于底色上方，透明图片可透出底色。");
            let open = action_button(ui, rect.min + Vec2::new(25.0, 73.0), "打开文件夹", true);
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
            refresh_background =
                action_button(ui, rect.min + Vec2::new(185.0, 73.0), "刷新背景图片", true)
                    .on_hover_text(
                        "重新读取所选文件夹，按文件名依次切换图片；文件不会被修改或删除。",
                    )
                    .clicked();
        });

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
                    self.status = format!(
                        "已应用主题：{}",
                        crate::theme::theme_name(self.settings.ui_theme)
                    );
                    self.error = None;
                    ui.ctx().request_repaint();
                }
                Err(error) => self.error = Some(format!("保存主题设置失败：{error:#}")),
            }
        }
        if colorful != old_colorful {
            let mut next = self.settings.clone();
            next.ui_background_colorful = colorful;
            match config::save_settings(&self.settings_path, &next) {
                Ok(()) => {
                    self.settings = next;
                    self.status = "背景底色已保存".into();
                    self.error = None;
                }
                Err(error) => self.error = Some(format!("保存背景设置失败：{error:#}")),
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
    let palette = crate::theme::palette(ui.ctx());
    let before = *value;
    let mut response = ui.interact(
        rect,
        ui.id().with(("theme-slider", title)),
        egui::Sense::click_and_drag(),
    );
    let track = rect.shrink2(Vec2::new(5.0, 0.0));
    if let Some(pointer) = response.interact_pointer_pos() {
        *value = (((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0) * maximum).round();
        response.request_focus();
    }
    let key_step = if maximum == 360.0 { 10.0 } else { 5.0 };
    let (mut decrease, mut increase) = (0, 0);
    if response.has_focus() {
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
    *value = (*value + (increase as f32 - decrease as f32) * key_step).clamp(0.0, maximum);
    if before != *value {
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::slider(ui.is_enabled(), f64::from(*value), title));
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_min_numeric_value(0.0);
        node.set_max_numeric_value(f64::from(maximum));
        node.set_numeric_value_step(f64::from(key_step));
        if *value > 0.0 {
            node.add_action(egui::accesskit::Action::Decrement);
        }
        if *value < maximum {
            node.add_action(egui::accesskit::Action::Increment);
        }
    });
    let center = Pos2::new(
        track.left() + track.width() * *value / maximum,
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
    response.on_hover_text(format!("{title}：{value:.0}"))
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
        texture: ctx.load_texture("pcl-background", image, egui::TextureOptions::LINEAR),
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

fn cover_uv(image: [usize; 2], area: Vec2) -> Rect {
    let image_ratio = image[0] as f32 / image[1] as f32;
    let area_ratio = area.x / area.y;
    if image_ratio > area_ratio {
        let visible = area_ratio / image_ratio;
        Rect::from_min_max(
            Pos2::new((1.0 - visible) / 2.0, 0.0),
            Pos2::new((1.0 + visible) / 2.0, 1.0),
        )
    } else {
        let visible = image_ratio / area_ratio;
        Rect::from_min_max(
            Pos2::new(0.0, (1.0 - visible) / 2.0),
            Pos2::new(1.0, (1.0 + visible) / 2.0),
        )
    }
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
        let uv = cover_uv([400, 200], Vec2::new(100.0, 100.0));
        assert_eq!(
            uv,
            Rect::from_min_max(Pos2::new(0.25, 0.0), Pos2::new(0.75, 1.0))
        );
        let uv = cover_uv([200, 400], Vec2::new(100.0, 100.0));
        assert_eq!(
            uv,
            Rect::from_min_max(Pos2::new(0.0, 0.25), Pos2::new(1.0, 0.75))
        );
    }
}
