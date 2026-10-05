//! PageSetupUI geometry and local, persisted background controls.
#[path = "background_effect.rs"]
mod background_effect;
#[path = "music.rs"]
mod music;
use super::Launcher;
use crate::theme;
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
    music: music::MusicState,
    music_poll: Option<std::time::Instant>,
    clear_music: bool,
    title_logo_key: Option<PathBuf>,
    title_logo: Option<egui::TextureHandle>,
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
    animation: Option<BackgroundAnimation>,
}

struct BackgroundAnimation {
    frames: Vec<Arc<egui::ColorImage>>,
    ends_ms: Vec<u64>,
    loops: Option<u32>,
    started: Option<f64>,
    index: usize,
}

impl BackgroundAnimation {
    fn at(&self, elapsed_ms: u64) -> (usize, Option<std::time::Duration>) {
        let total = *self.ends_ms.last().unwrap_or(&1);
        if self
            .loops
            .is_some_and(|loops| elapsed_ms >= total.saturating_mul(u64::from(loops)))
        {
            return (self.frames.len() - 1, None);
        }
        let within = elapsed_ms % total;
        let index = self.ends_ms.partition_point(|end| *end <= within);
        (
            index,
            Some(std::time::Duration::from_millis(
                self.ends_ms[index] - within,
            )),
        )
    }
}

/// Hiding is presentation only; F12 temporarily reveals controls without writing
/// the saved preferences, including the settings page needed to restore them.
pub(super) fn feature_visible(ctx: &egui::Context, settings: &Settings, key: &str) -> bool {
    ctx.data(|data| data.get_temp::<bool>(egui::Id::new("pcl-reveal-hidden")))
        .unwrap_or(false)
        || !settings.ui_hidden_pages.iter().any(|hidden| hidden == key)
}

impl AppearanceState {
    /// Returns false only for the original logo, which the shell already draws.
    pub(super) fn paint_custom_title(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        settings: &Settings,
    ) -> Result<bool> {
        if settings.ui_title_mode == 1 {
            return Ok(false);
        }
        if settings.ui_title_mode == 0 {
            return Ok(true);
        }
        let painter = ui.painter().with_clip_rect(rect);
        if settings.ui_title_mode == 2 {
            painter.text(
                rect.left_center(),
                egui::Align2::LEFT_CENTER,
                &settings.ui_title_text,
                egui::FontId::proportional(17.0),
                Color32::WHITE,
            );
        } else if settings.ui_title_mode == 3 {
            if self.title_logo_key != settings.ui_title_logo {
                self.title_logo_key = settings.ui_title_logo.clone();
                self.title_logo = None;
                if let Some(path) = &settings.ui_title_logo {
                    let mut bytes = Vec::new();
                    fs::File::open(path)?
                        .take(8 * 1024 * 1024 + 1)
                        .read_to_end(&mut bytes)?;
                    if bytes.len() > 8 * 1024 * 1024 {
                        bail!("标题栏图片超过 8 MiB");
                    }
                    let image = decode_background(&bytes).context("标题栏图片无法读取")?;
                    self.title_logo = Some(ui.ctx().load_texture(
                        "pcl-title-logo",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
            if let Some(texture) = &self.title_logo {
                let original = texture.size_vec2();
                let scale = (rect.width() / original.x)
                    .min(rect.height() / original.y)
                    .min(1.0);
                let size = original * scale;
                let center = Pos2::new(rect.left() + size.x / 2.0, rect.center().y);
                painter.image(
                    texture.id(),
                    Rect::from_center_size(center, size),
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        }
        Ok(true)
    }

    /// Poll one background worker, then schedule only the newest requested effect.
    /// Image/fit/size changes cannot apply the previous image's result.
    pub(super) fn ensure_loaded(
        &mut self,
        ctx: &egui::Context,
        settings: &Settings,
        settings_path: &Path,
    ) -> Result<()> {
        if self
            .music_poll
            .is_none_or(|last| last.elapsed() >= std::time::Duration::from_millis(100))
        {
            self.music_poll = Some(std::time::Instant::now());
            self.music
                .tick(&settings_directory(settings_path).join("musics"), settings)?;
        }
        if self.music.visible() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if !self.initialized {
            self.initialized = true;
            self.background =
                load_background(ctx, &background_folder(settings, settings_path), None)?;
        }
        if let Some(background) = &mut self.background {
            if let Some(animation) = &mut background.animation {
                let now = theme::animation_time(ctx);
                let started = *animation.started.get_or_insert(now);
                let (index, next) = animation.at(((now - started).max(0.0) * 1000.0) as u64);
                if index != animation.index {
                    animation.index = index;
                    background.original = Arc::clone(&animation.frames[index]);
                    // A new texture identity also invalidates an in-flight blur of the old frame.
                    background.texture = ctx.load_texture(
                        "pcl-background-frame",
                        (*background.original).clone(),
                        egui::TextureOptions::LINEAR_REPEAT,
                    );
                }
                if let Some(next) = next.filter(|_| theme::animations_enabled(ctx)) {
                    ctx.request_repaint_after(next.div_f32(theme::animation_speed(ctx)));
                }
            }
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

    pub(super) fn paint_music_icon(ui: &egui::Ui, rect: Rect, playing: bool) {
        music::paint_icon(ui, rect, playing);
    }

    pub(super) fn music_info(&self) -> Option<(String, bool, f32)> {
        self.music.visible().then(|| {
            (
                self.music.title(),
                self.music.playing(),
                self.music.progress,
            )
        })
    }
    pub(super) fn toggle_music(&mut self) -> Result<()> {
        self.music.toggle()
    }
    pub(super) fn next_music(&mut self, settings: &Settings) -> Result<()> {
        self.music.next(settings, true)
    }
    pub(super) fn music_game_changed(&mut self, started: bool, settings: &Settings) -> Result<()> {
        self.music.game_changed(started, settings)
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
        if crate::startup_splash::was_shown() {
            return;
        }
        let now = theme::animation_time(ctx);
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
        let opacity = if theme::animations_enabled(ctx) {
            startup_opacity(now - started)
        } else {
            0.0
        };
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
        let mut refresh_home = false;
        let mut tutorial_home = false;
        let mut generate_home = false;
        let mut open_music = false;
        let mut choose_title_logo = false;
        let mut refresh_music = false;
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
                "{}\n放入 PNG、JPEG、WebP 或 GIF 后点击刷新。\n右键可选择其他文件夹；GIF 按帧延时播放。\n图片限制：32 MiB、4096 × 4096 像素；动画解码总量最多 96 MiB。",
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

        let has_music = self.appearance.music.visible();
        section(
            ui,
            "背景音乐",
            if has_music { 240.0 } else { 97.0 },
            |ui, rect| {
                let offset = if has_music { 143.0 } else { 0.0 };
                if has_music {
                    let mut volume = f32::from(theme_settings.ui_music_volume);
                    label(ui, rect.min + Vec2::new(25.0, 39.0), 72.0, "音量", 13.0);
                    theme_changed |= slider_track(
                        ui,
                        Rect::from_min_size(
                            rect.min + Vec2::new(97.0, 42.0),
                            Vec2::new((rect.width() - 122.0).max(20.0), 16.0),
                        ),
                        "音乐音量",
                        &mut volume,
                        0.0,
                        1000.0,
                        10.0,
                    )
                    .changed();
                    theme_settings.ui_music_volume = volume as u16;
                    for (index, title) in [
                        "随机播放",
                        "打开启动器自动开始播放",
                        "游戏启动后自动开始播放，游戏退出后自动暂停播放",
                        "游戏启动后自动暂停播放，游戏退出后自动开始播放",
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let checked = match index {
                            0 => &mut theme_settings.ui_music_random,
                            1 => &mut theme_settings.ui_music_auto,
                            2 => &mut theme_settings.ui_music_start,
                            _ => &mut theme_settings.ui_music_stop,
                        };
                        let changed = appearance_checkbox(
                            ui,
                            Rect::from_min_size(
                                rect.min + Vec2::new(24.0, 71.0 + index as f32 * 27.0),
                                Vec2::new(rect.width() - 49.0, 22.0),
                            ),
                            checked,
                            title,
                        )
                        .changed();
                        theme_changed |= changed;
                        if changed && index == 2 && theme_settings.ui_music_start {
                            theme_settings.ui_music_stop = false;
                        }
                        if changed && index == 3 && theme_settings.ui_music_stop {
                            theme_settings.ui_music_start = false;
                        }
                    }
                }
                open_music = action_button(
                    ui,
                    rect.min + Vec2::new(25.0, 42.0 + offset),
                    "打开文件夹",
                    true,
                )
                .on_hover_text(
                    "将本地 WAV、MP3、FLAC 等音频放入后刷新；支持格式取决于系统音频解码器。",
                )
                .clicked();
                refresh_music = action_button(
                    ui,
                    rect.min + Vec2::new(185.0, 42.0 + offset),
                    "刷新背景音乐",
                    true,
                )
                .clicked();
                if has_music {
                    self.appearance.clear_music |= action_button(
                        ui,
                        rect.min + Vec2::new(345.0, 42.0 + offset),
                        "清空背景音乐",
                        true,
                    )
                    .clicked();
                }
            },
        );

        let title_mode = theme_settings.ui_title_mode;
        section(
            ui,
            "标题栏",
            if title_mode == 0 {
                110.0
            } else if title_mode == 2 {
                124.0
            } else if title_mode == 3 {
                131.0
            } else {
                77.0
            },
            |ui, rect| {
                for (index, (id, title)) in [(0, "无"), (1, "默认"), (2, "文本"), (3, "图片")]
                    .into_iter()
                    .enumerate()
                {
                    if appearance_radio(
                        ui,
                        source_radio_rect(rect, index),
                        ui.id().with(("title-mode", id)),
                        title_mode == id,
                        title,
                    )
                    .clicked()
                    {
                        theme_settings.ui_title_mode = id;
                        theme_changed = true;
                    }
                }
                if title_mode == 0 {
                    theme_changed |= appearance_checkbox(
                        ui,
                        Rect::from_min_size(
                            rect.min + Vec2::new(24.0, 70.0),
                            Vec2::new(rect.width() - 49.0, 22.0),
                        ),
                        &mut theme_settings.ui_title_left,
                        "标题栏居左",
                    )
                    .changed();
                }
                if title_mode >= 2 {
                    if title_mode == 2 {
                        label(
                            ui,
                            rect.min + Vec2::new(25.0, 79.0),
                            90.0,
                            "标题栏文本",
                            13.0,
                        );
                        theme_changed |= ui
                            .place(
                                Rect::from_min_size(
                                    rect.min + Vec2::new(115.0, 76.0),
                                    Vec2::new(rect.width() - 140.0, 28.0),
                                ),
                                egui::TextEdit::singleline(&mut theme_settings.ui_title_text)
                                    .char_limit(100),
                            )
                            .changed();
                    } else {
                        choose_title_logo =
                            action_button(ui, rect.min + Vec2::new(25.0, 76.0), "更改图片", true)
                                .clicked();
                        if action_button(
                            ui,
                            rect.min + Vec2::new(185.0, 76.0),
                            "清空图片",
                            theme_settings.ui_title_logo.is_some(),
                        )
                        .clicked()
                        {
                            theme_settings.ui_title_logo = None;
                            theme_changed = true;
                        }
                    }
                }
            },
        );

        let home_mode = theme_settings.ui_custom_type;
        section(
            ui,
            "主页",
            match home_mode {
                0 => 77.0,
                3 => 122.0,
                1 => 193.0,
                _ => 189.0,
            },
            |ui, rect| {
                for (index, (id, title)) in [
                    (0, "空白"),
                    (3, "预设"),
                    (1, "读取本地文件"),
                    (2, "联网更新"),
                ]
                .into_iter()
                .enumerate()
                {
                    let response = appearance_radio(
                        ui,
                        source_radio_rect(rect, index),
                        ui.id().with(("home-mode", id)),
                        theme_settings.ui_custom_type == id,
                        title,
                    );
                    if response.clicked() {
                        theme_settings.ui_custom_type = id;
                        theme_changed = true;
                    }
                }
                if home_mode == 3 {
                    label(ui, rect.min + Vec2::new(25.0, 77.0), 90.0, "主页预设", 13.0);
                    let mut child =
                        ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                            rect.min + Vec2::new(115.0, 74.0),
                            Vec2::new(rect.width() - 150.0, 28.0),
                        )));
                    let before = theme_settings.ui_custom_preset;
                    ui_style::PclComboBox::from_id_salt("home-preset")
                        .width(rect.width() - 150.0)
                        .selected_text(
                            super::home_ui::PRESETS
                                .iter()
                                .find(|(id, _, _)| *id == before)
                                .map_or("请选择", |(_, title, _)| *title),
                        )
                        .show_ui(&mut child, |ui| {
                            for (id, title, _) in super::home_ui::PRESETS {
                                ui.selectable_value(
                                    &mut theme_settings.ui_custom_preset,
                                    *id,
                                    *title,
                                );
                            }
                        });
                    theme_changed |= before != theme_settings.ui_custom_preset;
                } else if home_mode == 1 || home_mode == 2 {
                    let message = if home_mode == 1 {
                        "从主页文件夹下的 Custom.xaml 读取主页内容。\n你可以手动编辑该文件，向主页添加文本、图片、常用网站、快捷启动等功能。"
                    } else {
                        "从指定网址联网获取主页内容。\n服主也可以用于动态更新服务器公告。"
                    };
                    let hint = Rect::from_min_size(
                        rect.min + Vec2::new(25.0, 77.0),
                        Vec2::new(rect.width() - 50.0, 58.0),
                    );
                    ui.painter()
                        .rect_filled(hint, 3, crate::theme::palette(ui.ctx()).light);
                    ui_style::place_left(
                        ui,
                        hint.shrink(10.0),
                        egui::Label::new(RichText::new(message).size(13.0)).wrap(),
                    );
                    if home_mode == 1 {
                        for (index, title) in
                            ["刷新主页", "生成教学文件", "查看教程", "打开主页文件夹"]
                                .into_iter()
                                .enumerate()
                        {
                            let width = ((rect.width() - 50.0 - 60.0) / 4.0).min(140.0);
                            let response = ui_style::outline_button(
                                ui,
                                Rect::from_min_size(
                                    rect.min
                                        + Vec2::new(25.0 + index as f32 * (width + 20.0), 141.0),
                                    Vec2::new(width, 32.0),
                                ),
                                title,
                                None,
                                index == 0,
                                true,
                            );
                            if response.clicked() {
                                match index {
                                    0 => refresh_home = true,
                                    1 => generate_home = true,
                                    2 => tutorial_home = true,
                                    _ => open_home = true,
                                }
                            }
                        }
                    } else {
                        label(
                            ui,
                            rect.min + Vec2::new(25.0, 144.0),
                            90.0,
                            "下载地址",
                            13.0,
                        );
                        theme_changed |= ui
                            .place(
                                Rect::from_min_size(
                                    rect.min + Vec2::new(115.0, 141.0),
                                    Vec2::new(rect.width() - 140.0, 28.0),
                                ),
                                egui::TextEdit::singleline(&mut theme_settings.ui_custom_net)
                                    .hint_text("https://…/Custom.xaml")
                                    .char_limit(4096),
                            )
                            .changed();
                    }
                }
            },
        );

        if feature_visible(ui.ctx(), &self.settings, "hidden") {
            theme_changed |= hidden_section(ui, &mut theme_settings.ui_hidden_pages);
        }
        if choose_title_logo {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("选择标题栏图片")
                .add_filter("图片", &["png", "jpg", "jpeg", "webp", "gif"])
                .pick_file()
            {
                let result = (|| {
                    let mut bytes = Vec::new();
                    fs::File::open(&path)?
                        .take(8 * 1024 * 1024 + 1)
                        .read_to_end(&mut bytes)?;
                    if bytes.len() > 8 * 1024 * 1024 {
                        bail!("标题栏图片超过 8 MiB");
                    }
                    decode_background(&bytes)?;
                    anyhow::Ok(())
                })();
                match result {
                    Ok(()) => {
                        theme_settings.ui_title_logo = Some(path);
                        self.appearance.title_logo_key = None;
                        self.appearance.title_logo = None;
                        theme_changed = true;
                    }
                    Err(error) => self.error = Some(format!("标题栏图片无法读取：{error:#}")),
                }
            }
        }

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
        let music_folder = settings_directory(&self.settings_path).join("musics");
        if open_music {
            if let Err(error) = fs::create_dir_all(&music_folder)
                .and_then(|_| crate::process::open_folder(&music_folder))
            {
                self.error = Some(format!("打开音乐文件夹失败：{error}"));
            }
        }
        if refresh_music {
            match self
                .appearance
                .music
                .refresh(&music_folder, &self.settings, true)
            {
                Ok(()) => {
                    self.status = if self.appearance.music.visible() {
                        "背景音乐已刷新"
                    } else {
                        "未检测到可播放的背景音乐"
                    }
                    .into();
                    self.error = None;
                }
                Err(error) => self.error = Some(format!("刷新背景音乐：{error:#}")),
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
        if refresh_home {
            self.refresh_custom_home();
            self.status = "主页将在显示时重新读取".into();
        }
        if generate_home {
            match super::home_ui::generate_tutorial(&self.settings_path) {
                Ok(path) => self.status = format!("已生成教学文件：{}", path.display()),
                Err(error) => self.error = Some(format!("生成教学文件失败：{error:#}")),
            }
        }
        if tutorial_home {
            self.home.message=Some(("主页自定义教程".into(),"1. 点击生成教学文件。\n2. 用文本编辑器修改主页文件夹的 Custom.xaml 并保存。\n3. 点击刷新主页后返回启动页。\n\n支持卡片、文本、图片、按钮与常用布局；按钮操作只在点击时执行。文件、程序、下载和设置操作会显示确认内容。已有 Custom.xaml 不会被教学文件覆盖。".into()));
        }
    }

    pub(super) fn appearance_music_dialog(&mut self, ctx: &egui::Context) {
        if !self.appearance.clear_music {
            return;
        }
        let folder = settings_directory(&self.settings_path).join("musics");
        let caption=format!("停止播放并清空音乐列表。\n原音乐文件夹将移至同目录的 music-removed-* 备份文件夹，保留全部文件，随后创建空音乐文件夹。\n\n{}",folder.display());
        if let Some(action) = super::modal_ui::account_modal_with_options(
            ctx,
            "clear-background-music",
            "清空背景音乐",
            &caption,
            &["清空并保留备份", "取消"],
            super::modal_ui::ModalOptions::warning(),
        ) {
            self.appearance.clear_music = false;
            if action == 0 {
                match self.appearance.music.clear_preserving_files(&folder) {
                    Ok(path) => {
                        self.status = format!("背景音乐已清空，原文件保留在 {}", path.display())
                    }
                    Err(error) => self.error = Some(format!("清空背景音乐失败：{error:#}")),
                }
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

fn source_radio_rect(card: Rect, index: usize) -> Rect {
    // PageSetupUI: left margin 25 minus Grid margin 1; four 1* columns and a 0.2* tail.
    let column = (card.width() - 49.0) / 4.2;
    Rect::from_min_size(
        card.min + Vec2::new(24.0 + index as f32 * column, 40.0),
        Vec2::new(column, 22.0),
    )
}

fn theme_radio(ui: &mut egui::Ui, rect: Rect, id: u8, selected: bool) -> egui::Response {
    appearance_radio(
        ui,
        rect,
        ui.id().with(("theme", id)),
        selected,
        crate::theme::theme_name(id),
    )
}

fn appearance_radio(
    ui: &mut egui::Ui,
    rect: Rect,
    id: egui::Id,
    selected: bool,
    name: &str,
) -> egui::Response {
    let response = ui.interact(rect, id, egui::Sense::click());
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

fn hidden_section(ui: &mut egui::Ui, hidden_pages: &mut Vec<String>) -> bool {
    const NOTE: &str =
        "你可以隐藏不需要的页面或关闭特定功能。在任意界面按 F12 可以暂时显示被隐藏的功能。";
    type HiddenChoice = (&'static str, &'static str, usize);
    const GROUPS: [(&str, &[HiddenChoice]); 4] = [
        (
            "主页面",
            &[
                ("download", "下载", 0),
                ("setup", "设置", 2),
                ("more", "更多", 3),
            ],
        ),
        (
            "设置 子页面",
            &[
                ("setup_launch", "启动", 0),
                ("setup_ui", "个性化", 2),
                ("setup_system", "其他", 3),
            ],
        ),
        (
            "更多 子页面",
            &[("help", "帮助", 0), ("about", "关于与鸣谢", 1)],
        ),
        (
            "特定功能",
            &[
                ("version", "版本管理", 1),
                ("mod_update", "Mod 更新", 2),
                ("hidden", "功能隐藏", 3),
            ],
        ),
    ];
    let font = egui::FontId::proportional(13.0);
    let text_color = crate::theme::palette(ui.ctx()).text;
    let note = ui.painter().layout(
        NOTE.into(),
        font.clone(),
        text_color,
        (ui.available_width() - 40.0).max(1.0),
    );
    let label_width = GROUPS
        .iter()
        .map(|(name, _)| {
            ui.painter()
                .layout_no_wrap((*name).into(), font.clone(), text_color)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    let grid_top = 39.0 + note.size().y + 1.0 + 4.0;
    let mut changed = false;
    section(ui, "功能隐藏", grid_top + 120.0 + 15.0, |ui, card| {
        ui.painter()
            .galley(card.min + Vec2::new(25.0, 39.0), note, text_color);
        // Preserve the upstream 0.8*,0.9*,0.8*,0.8*,1.0* grid. Removed features
        // leave their original cells empty, rather than stretching the remaining controls.
        let grid_left = 25.0 + label_width + 18.0;
        let unit = (card.width() - grid_left - 15.0) / 4.3;
        let column_starts = [0.0, 0.8, 1.7, 2.5, 3.3];
        let column_widths = [0.8, 0.9, 0.8, 0.8, 1.0];
        for (row, (group, choices)) in GROUPS.into_iter().enumerate() {
            let y = grid_top + row as f32 * 30.0 + 4.0;
            label(ui, card.min + Vec2::new(25.0, y), label_width, group, 13.0);
            for &(key, title, column) in choices {
                let mut hidden = hidden_pages.iter().any(|saved| saved == key);
                if appearance_checkbox(
                    ui,
                    Rect::from_min_size(
                        card.min + Vec2::new(grid_left + column_starts[column] * unit, y),
                        Vec2::new(column_widths[column] * unit, 22.0),
                    ),
                    &mut hidden,
                    title,
                )
                .changed()
                {
                    hidden_pages.retain(|saved| saved != key);
                    if hidden {
                        hidden_pages.push(key.into());
                    }
                    changed = true;
                }
            }
        }
    });
    changed
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
    let response = slider_control(ui, rect, title, value, minimum, maximum, key_step);
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

/// PCL MySlider track and keyboard behavior; callers supply domain-specific hints.
pub(super) fn slider_control(
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
    response
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
    let animation =
        decode_animation(&bytes).with_context(|| format!("解码背景动画 {}", path.display()))?;
    let image = match &animation {
        Some(animation) => (*animation.frames[0]).clone(),
        None => {
            decode_background(&bytes).with_context(|| format!("解码背景图片 {}", path.display()))?
        }
    };
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
        animation,
    }))
}

fn decode_animation(bytes: &[u8]) -> Result<Option<BackgroundAnimation>> {
    use image::{AnimationDecoder, ImageDecoder};
    if !bytes.starts_with(b"GIF87a") && !bytes.starts_with(b"GIF89a") {
        return Ok(None);
    }
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        bail!("背景图片超过 32 MiB");
    }
    let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
        bail!("背景动画尺寸必须在 1 至 4096 像素之间");
    }
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(96 * 1024 * 1024);
    decoder.set_limits(limits)?;
    let loops = match decoder.loop_count() {
        image::metadata::LoopCount::Infinite => None,
        image::metadata::LoopCount::Finite(count) => Some(count.get()),
    };
    let frame_bytes = u64::from(width) * u64::from(height) * 4;
    let mut frames = Vec::new();
    let mut ends_ms = Vec::new();
    let mut elapsed = 0_u64;
    for frame in decoder.into_frames() {
        if frames.len() >= 500 || (frames.len() as u64 + 1) * frame_bytes > 96 * 1024 * 1024 {
            bail!("背景动画超过 500 帧或 96 MiB 解码上限");
        }
        let frame = frame.context("GIF 动画帧解码失败")?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        elapsed =
            elapsed.saturating_add((u64::from(numerator) / u64::from(denominator.max(1))).max(10));
        ends_ms.push(elapsed);
        frames.push(Arc::new(egui::ColorImage::from_rgba_unmultiplied(
            [width as usize, height as usize],
            frame.buffer().as_raw(),
        )));
    }
    if frames.is_empty() {
        bail!("GIF 动画没有图片帧");
    }
    Ok(Some(BackgroundAnimation {
        frames,
        ends_ms,
        loops,
        started: None,
        index: 0,
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

    #[test]
    fn personalization_radios_stay_left_aligned_and_respond_at_the_visible_control() {
        let ctx = egui::Context::default();
        let card = Rect::from_min_size(Pos2::new(20.0, 20.0), Vec2::new(818.0, 77.0));
        let mut selected = 0;
        let mut draw = |events| {
            ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        for (index, name) in
                            ["None", "Default", "Text", "Image"].into_iter().enumerate()
                        {
                            if appearance_radio(
                                ui,
                                source_radio_rect(card, index),
                                ui.id().with(index),
                                selected == index,
                                name,
                            )
                            .clicked()
                            {
                                selected = index;
                            }
                        }
                    });
                },
            )
        };
        let output = draw(vec![]);
        let centers: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Circle(circle) if (circle.radius - 8.45).abs() < 0.01 => {
                    Some(circle.center)
                }
                _ => None,
            })
            .collect();
        assert_eq!(centers.len(), 4);
        assert_eq!(centers[0], Pos2::new(54.0, 71.0));
        assert!((centers[1].x - centers[0].x - 769.0 / 4.2).abs() < 0.01);
        let point = centers[2];
        let pointer = |pressed| egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = draw(vec![egui::Event::PointerMoved(point), pointer(true)]);
        let _ = draw(vec![pointer(false)]);
        assert_eq!(selected, 2);
    }

    #[test]
    fn personalization_hidden_rows_share_left_edge_and_toggle_without_reordering_settings() {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let fallback = fonts.families[&egui::FontFamily::Proportional].clone();
        fonts
            .families
            .insert(egui::FontFamily::Name("PCL Bold".into()), fallback);
        ctx.set_fonts(fonts);
        let mut hidden = vec!["help".to_owned()];
        let mut draw = |events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(818.0, 400.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ctx, |ui| {
                            hidden_section(ui, &mut hidden);
                        });
                },
            )
        };
        let output = draw(vec![]);
        let text_position = |text: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(shape) if shape.galley.text() == text => Some(shape.pos),
                    _ => None,
                })
                .unwrap()
        };
        let group = text_position("主页面");
        for (index, title) in ["设置 子页面", "更多 子页面", "特定功能"]
            .into_iter()
            .enumerate()
        {
            let position = text_position(title);
            assert_eq!(position.x, group.x);
            assert!((position.y - group.y - (index + 1) as f32 * 30.0).abs() < 0.01);
        }
        assert_eq!(group.x, 25.0);
        let download = text_position("下载");
        let point = download + Vec2::new(5.0, 7.0);
        let pointer = |pressed| egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = draw(vec![egui::Event::PointerMoved(point), pointer(true)]);
        let _ = draw(vec![pointer(false)]);
        assert_eq!(hidden, ["help", "download"]);
    }

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
            animation: None,
        }
    }

    #[test]
    fn custom_title_modes_load_a_real_local_image_and_release_selection() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("logo.png");
        image::RgbaImage::from_pixel(24, 12, image::Rgba([255, 0, 0, 255]))
            .save(&path)
            .unwrap();
        let ctx = egui::Context::default();
        let mut state = AppearanceState::default();
        let mut settings = Settings {
            ui_title_mode: 3,
            ui_title_logo: Some(path),
            ..Default::default()
        };
        let mut handled = false;
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                handled = state
                    .paint_custom_title(
                        ui,
                        Rect::from_min_size(Pos2::new(18.0, 0.0), Vec2::new(200.0, 48.0)),
                        &settings,
                    )
                    .unwrap();
            });
        });
        assert!(handled);
        let id = state.title_logo.as_ref().unwrap().id();
        assert!(output
            .shapes
            .iter()
            .any(|shape| matches!(&shape.shape,egui::Shape::Mesh(mesh) if mesh.texture_id==id)));
        settings.ui_title_logo = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                state
                    .paint_custom_title(ui, ui.max_rect(), &settings)
                    .unwrap();
            });
        });
        assert!(state.title_logo.is_none());
        assert!(directory.path().join("logo.png").exists());
        settings.ui_title_mode = 1;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                handled = state
                    .paint_custom_title(ui, ui.max_rect(), &settings)
                    .unwrap();
            });
        });
        assert!(!handled);
    }
    #[test]
    fn temporary_visibility_override_does_not_change_saved_choices() {
        let ctx = egui::Context::default();
        let settings = Settings {
            ui_hidden_pages: vec!["setup".into(), "hidden".into()],
            ..Default::default()
        };
        assert!(!feature_visible(&ctx, &settings, "setup"));
        assert!(feature_visible(&ctx, &settings, "download"));
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("pcl-reveal-hidden"), true));
        assert!(feature_visible(&ctx, &settings, "setup"));
        assert!(feature_visible(&ctx, &settings, "hidden"));
        assert_eq!(settings.ui_hidden_pages, vec!["setup", "hidden"]);
    }
    #[test]
    fn gif_uses_actual_frames_delays_and_finite_loop_instead_of_first_frame() {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder
                .set_repeat(image::codecs::gif::Repeat::Finite(2))
                .unwrap();
            for (color, delay) in [([255, 0, 0, 255], 40), ([0, 255, 0, 255], 120)] {
                encoder
                    .encode_frame(image::Frame::from_parts(
                        image::RgbaImage::from_pixel(2, 2, image::Rgba(color)),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(delay, 1),
                    ))
                    .unwrap();
            }
        }
        let animation = decode_animation(&bytes).unwrap().unwrap();
        assert_eq!(animation.frames.len(), 2);
        assert_eq!(animation.frames[0].pixels[0], Color32::RED);
        assert_eq!(animation.frames[1].pixels[0], Color32::GREEN);
        assert_eq!(animation.at(39).0, 0);
        assert_eq!(animation.at(40).0, 1);
        assert_eq!(animation.at(160).0, 0);
        assert_eq!(animation.at(320), (1, None));
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
