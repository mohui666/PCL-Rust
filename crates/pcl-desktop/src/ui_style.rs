//! WPF measurements are kept in device-independent pixels (DIP).
use crate::theme;
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use std::collections::HashMap;
pub const GRAY: Color32 = Color32::from_rgb(140, 140, 140);

#[path = "combo_ui.rs"]
mod combo_ui;
#[allow(unused_imports)] // Also compiled by renderer-only examples.
pub use combo_ui::{editable_combo, PclComboBox};

pub fn card_title(text: &str) -> egui::RichText {
    egui::RichText::new(text).font(egui::FontId::new(
        13.0,
        egui::FontFamily::Name("PCL Bold".into()),
    ))
}

pub fn place_left(ui: &mut egui::Ui, rect: Rect, widget: impl egui::Widget) -> egui::Response {
    ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    )
    .add(widget)
}

/// MyCard's source path, rotated up for an expanded card.
pub fn card_chevron(ui: &egui::Ui, rect: Rect, open: bool, color: Color32) {
    let origin = egui::pos2(rect.right() - 26.0, rect.top() + 17.0);
    let mut mesh = egui::Mesh::default();
    for (x, y) in [(1., 0.), (0., 1.), (5., 6.), (10., 1.), (9., 0.), (5., 4.)] {
        let point = if open {
            Vec2::new(10. - x, 6. - y)
        } else {
            Vec2::new(x, y)
        };
        mesh.colored_vertex(origin + point, color);
    }
    mesh.indices
        .extend_from_slice(&[0, 1, 2, 0, 2, 5, 5, 2, 3, 5, 3, 4]);
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// MyCheckBox: 18 DIP box at x=1, 12 DIP source check path, text at x=26.
pub fn checkbox(
    ui: &mut egui::Ui,
    checked: &mut bool,
    title: &str,
    description: &str,
) -> egui::Response {
    let enabled = ui.is_enabled();
    let disabled = Color32::from_gray(204);
    let (rect, mut response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), egui::Sense::click());
    if response.is_pointer_button_down_on() {
        response.request_focus();
    }
    if response.clicked() {
        *checked = !*checked;
        response.mark_changed();
    }
    let title_color = if !enabled {
        disabled
    } else if response.hovered() || response.has_focus() {
        theme::palette(ui.ctx()).accent
    } else {
        theme::palette(ui.ctx()).text
    };
    let mut job = egui::text::LayoutJob {
        break_on_newline: false,
        wrap: egui::text::TextWrapping {
            max_rows: 1,
            break_anywhere: true,
            max_width: (rect.width() - 26.0).max(0.0),
            ..Default::default()
        },
        ..Default::default()
    };
    job.append(
        title,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(13.0),
            color: title_color,
            ..Default::default()
        },
    );
    if !description.is_empty() {
        job.append(
            &format!("   {description}"),
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(13.0),
                color: disabled,
                ..Default::default()
            },
        );
    }
    let full_text = job.text.clone();
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, *checked, &full_text)
    });
    if ui.is_rect_visible(rect) {
        let stroke = if !enabled {
            disabled
        } else if response.hovered() || response.has_focus() {
            theme::palette(ui.ctx()).accent
        } else if *checked {
            // Checked border follows ColorBrush2 of the current theme.
            theme::palette(ui.ctx()).dark
        } else {
            theme::palette(ui.ctx()).text
        };
        let box_rect = Rect::from_center_size(
            Pos2::new(rect.left() + 10.0, rect.center().y),
            Vec2::splat(18.0),
        );
        ui.painter().rect(
            box_rect,
            3.0,
            if response.is_pointer_button_down_on() {
                theme::palette(ui.ctx()).pressed
            } else {
                Color32::from_white_alpha(85)
            },
            egui::Stroke::new(1.1_f32, stroke),
            egui::StrokeKind::Inside,
        );
        if *checked {
            let origin = Pos2::new(rect.left() + 4.0, rect.center().y - 6.0);
            let mut mesh = egui::Mesh::default();
            for (x, y) in [
                (0., 6.),
                (1.5, 4.5),
                (4.5, 7.5),
                (10.5, 1.5),
                (12., 3.),
                (4.5, 10.5),
            ] {
                mesh.colored_vertex(origin + Vec2::new(x, y), stroke);
            }
            mesh.indices
                .extend_from_slice(&[0, 1, 2, 0, 2, 5, 2, 3, 4, 2, 4, 5]);
            ui.painter().add(egui::Shape::mesh(mesh));
        }
        ui.painter().galley(
            Pos2::new(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0),
            galley.clone(),
            theme::palette(ui.ctx()).text,
        );
    }
    if galley.elided {
        response.on_hover_text(full_text)
    } else {
        response
    }
}

pub struct Assets {
    pub icons: HashMap<&'static str, egui::TextureHandle>,
    pub skin: egui::TextureHandle,
}
impl Assets {
    pub fn new(ctx: &egui::Context) -> Self {
        let mut icons = HashMap::new();
        for (name, svg) in [
            ("game", include_str!("../assets/icons/game.svg")),
            ("mod", include_str!("../assets/icons/mod.svg")),
            ("pack", include_str!("../assets/icons/pack.svg")),
            ("launch", include_str!("../assets/icons/launch.svg")),
            ("download", include_str!("../assets/icons/download.svg")),
            (
                "multiplayer",
                include_str!("../assets/icons/multiplayer.svg"),
            ),
            ("settings", include_str!("../assets/icons/settings.svg")),
            ("more", include_str!("../assets/icons/more.svg")),
            ("logo", include_str!("../assets/icons/logo.svg")),
            ("account", include_str!("../assets/icons/account.svg")),
            ("offline", include_str!("../assets/icons/offline.svg")),
            ("appearance", include_str!("../assets/icons/appearance.svg")),
            ("overview", include_str!("../assets/icons/overview.svg")),
            ("wrench", include_str!("../assets/icons/wrench.svg")),
            ("datapack", include_str!("../assets/icons/datapack.svg")),
            (
                "resourcepack",
                include_str!("../assets/icons/resourcepack.svg"),
            ),
            ("shader", include_str!("../assets/icons/shader.svg")),
            ("tasks", include_str!("../assets/icons/tasks.svg")),
            ("shutdown", include_str!("../assets/icons/shutdown.svg")),
        ] {
            let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default())
                .expect("checked-in SVG must parse");
            let bounds = tree.root().abs_bounding_box();
            let scale = 3.0;
            let height = if name == "logo" { 18.0 } else { 16.0 };
            let factor = height * scale / bounds.height();
            let width = (bounds.width() * factor).ceil() as u32;
            let h = (height * scale).ceil() as u32;
            let mut pixels = resvg::tiny_skia::Pixmap::new(width, h).unwrap();
            let transform = resvg::tiny_skia::Transform::from_scale(factor, factor)
                .pre_translate(-bounds.x(), -bounds.y());
            resvg::render(&tree, transform, &mut pixels.as_mut());
            let image = egui::ColorImage::from_rgba_premultiplied(
                [width as usize, h as usize],
                pixels.data(),
            );
            icons.insert(
                name,
                ctx.load_texture(name, image, egui::TextureOptions::LINEAR),
            );
        }
        for (name, bytes) in [
            (
                "release-type-release",
                include_bytes!("../assets/upstream/Images/ReleaseTypes/Release.png").as_slice(),
            ),
            (
                "release-type-beta",
                include_bytes!("../assets/upstream/Images/ReleaseTypes/Beta.png").as_slice(),
            ),
            (
                "release-type-alpha",
                include_bytes!("../assets/upstream/Images/ReleaseTypes/Alpha.png").as_slice(),
            ),
            (
                "block-egg",
                include_bytes!("../assets/upstream/Images/Blocks/Egg.png").as_slice(),
            ),
            (
                "block-path",
                include_bytes!("../assets/upstream/Images/Blocks/GrassPath.png").as_slice(),
            ),
            (
                "block-redstone",
                include_bytes!("../assets/upstream/Images/Blocks/RedstoneBlock.png").as_slice(),
            ),
            (
                "block-lamp-on",
                include_bytes!("../assets/upstream/Images/Blocks/RedstoneLampOn.png").as_slice(),
            ),
            (
                "block-lamp-off",
                include_bytes!("../assets/upstream/Images/Blocks/RedstoneLampOff.png").as_slice(),
            ),
            (
                "block-cobblestone",
                include_bytes!("../assets/upstream/Images/Blocks/CobbleStone.png").as_slice(),
            ),
            (
                "block-gold",
                include_bytes!("../assets/upstream/Images/Blocks/GoldBlock.png").as_slice(),
            ),
            (
                "block-command",
                include_bytes!("../assets/upstream/Images/Blocks/CommandBlock.png").as_slice(),
            ),
            (
                "block-forge",
                include_bytes!("../assets/upstream/Images/Blocks/Anvil.png").as_slice(),
            ),
            (
                "block-neoforge",
                include_bytes!("../assets/upstream/Images/Blocks/NeoForge.png").as_slice(),
            ),
            (
                "block-fabric",
                include_bytes!("../assets/upstream/Images/Blocks/Fabric.png").as_slice(),
            ),
            (
                "block-grass",
                include_bytes!("../assets/upstream/Images/Blocks/Grass.png").as_slice(),
            ),
        ] {
            let pixels = image::load_from_memory(bytes)
                .expect("checked-in block icon must decode")
                .to_rgba8();
            icons.insert(
                name,
                ctx.load_texture(
                    name,
                    egui::ColorImage::from_rgba_unmultiplied(
                        [pixels.width() as usize, pixels.height() as usize],
                        &pixels,
                    ),
                    egui::TextureOptions::LINEAR,
                ),
            );
        }
        let decoded =
            image::load_from_memory(include_bytes!("../assets/upstream/Images/Skins/Steve.png"))
                .expect("checked-in skin must decode")
                .to_rgba8();
        let skin = ctx.load_texture(
            "Steve",
            egui::ColorImage::from_rgba_unmultiplied(
                [decoded.width() as usize, decoded.height() as usize],
                &decoded,
            ),
            egui::TextureOptions::NEAREST,
        );
        Self { icons, skin }
    }
    pub fn icon(&self, ui: &egui::Ui, name: &str, rect: Rect, color: Color32) {
        let texture = &self.icons[name];
        let size = texture.size_vec2();
        let factor = (rect.width() / size.x).min(rect.height() / size.y);
        let rect = Rect::from_center_size(rect.center(), size * factor);
        ui.painter().image(
            texture.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            color,
        );
    }
    pub fn head(&self, ui: &egui::Ui, rect: Rect) {
        let face = Rect::from_center_size(rect.center(), Vec2::splat(48.0));
        ui.painter().add(
            egui::epaint::Shadow {
                offset: [0, 0],
                blur: 10,
                spread: 0,
                color: theme::palette(ui.ctx()).dark.gamma_multiply(30.0 / 255.0),
            }
            .as_shape(face, egui::CornerRadius::ZERO),
        );
        ui.painter().image(
            self.skin.id(),
            face,
            Rect::from_min_max(
                Pos2::new(8.0 / 64.0, 8.0 / 64.0),
                Pos2::new(16.0 / 64.0, 16.0 / 64.0),
            ),
            Color32::WHITE,
        );
        ui.painter().image(
            self.skin.id(),
            Rect::from_center_size(rect.center(), Vec2::splat(56.0)),
            Rect::from_min_max(
                Pos2::new(40.0 / 64.0, 8.0 / 64.0),
                Pos2::new(48.0 / 64.0, 16.0 / 64.0),
            ),
            Color32::WHITE,
        );
    }
}

pub fn gradient(painter: &egui::Painter, rect: Rect, colors: [Color32; 4]) {
    let mut mesh = egui::Mesh::default();
    for (pos, color) in [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ]
    .into_iter()
    .zip(colors)
    {
        mesh.vertices.push(egui::epaint::Vertex {
            pos,
            uv: egui::epaint::WHITE_UV,
            color,
        });
    }
    mesh.indices.extend([0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

/// Draw in window coordinates so the original six-DIP corners remain transparent.
fn rounded_mesh(
    painter: &egui::Painter,
    rect: Rect,
    radius: f32,
    bottom: bool,
    color: impl Fn(Pos2) -> Color32,
) {
    let mut ys: Vec<f32> = (0..=12).map(|step| step as f32 * radius / 12.0).collect();
    let end = if bottom {
        rect.height() - radius
    } else {
        rect.height()
    };
    let rows = ((end - radius) / 8.0).ceil().max(1.0) as usize;
    ys.extend((1..=rows).map(|step| radius + (end - radius) * step as f32 / rows as f32));
    if bottom {
        ys.extend((1..=12).map(|step| end + step as f32 * radius / 12.0));
    }
    let mut mesh = egui::Mesh::default();
    for &y in &ys {
        let distance = if y < radius {
            radius - y
        } else if bottom && y > end {
            y - end
        } else {
            0.0
        };
        let inset = radius - (radius * radius - distance * distance).max(0.0).sqrt();
        for x in 0..=32 {
            let point =
                rect.min + Vec2::new(inset + (rect.width() - 2.0 * inset) * x as f32 / 32.0, y);
            mesh.vertices.push(egui::epaint::Vertex {
                pos: point,
                uv: egui::epaint::WHITE_UV,
                color: color(point),
            });
        }
    }
    for row in 0..ys.len() - 1 {
        for x in 0..32 {
            let a = (row * 33 + x) as u32;
            mesh.indices.extend([a, a + 1, a + 34, a, a + 34, a + 33]);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

pub fn title_background(painter: &egui::Painter, rect: Rect) {
    let stops = theme::palette(painter.ctx()).title_stops;
    rounded_mesh(painter, rect, 6.0, false, |point| {
        let fraction = ((point.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        if fraction < 0.5 {
            blend(stops[0], stops[1], fraction * 2.0)
        } else {
            blend(stops[1], stops[2], (fraction - 0.5) * 2.0)
        }
    });
}

fn blend(left: Color32, right: Color32, fraction: f32) -> Color32 {
    let a = left.to_array();
    let b = right.to_array();
    let channel = |index: usize| {
        (f32::from(a[index]) + (f32::from(b[index]) - f32::from(a[index])) * fraction).round() as u8
    };
    Color32::from_rgba_premultiplied(channel(0), channel(1), channel(2), channel(3))
}

pub fn pill(
    ui: &mut egui::Ui,
    assets: &Assets,
    rect: Rect,
    label: &str,
    icon: &str,
    selected: bool,
    title: bool,
) -> egui::Response {
    let response = ui.put(
        rect,
        egui::Button::new("")
            .fill(Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let color = if title {
        if selected {
            theme::palette(ui.ctx()).accent
        } else {
            Color32::WHITE
        }
    } else if selected {
        Color32::WHITE
    } else {
        theme::palette(ui.ctx()).accent
    };
    let fill = if selected {
        if title {
            Color32::WHITE
        } else {
            theme::palette(ui.ctx()).accent
        }
    } else if response.hovered() {
        Color32::from_white_alpha(if title { 30 } else { 120 })
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 13.5, fill);
    let icon_scale = if title {
        match icon {
            "launch" | "download" => 0.9,
            "multiplayer" => 1.05,
            "settings" => 1.1,
            "more" => 0.93,
            _ => 1.0,
        }
    } else {
        1.0
    };
    let padding = if title { 2.0 } else { 0.0 };
    assets.icon(
        ui,
        icon,
        Rect::from_center_size(
            rect.min + Vec2::new(20.0 + padding, 13.5),
            Vec2::splat(16.0 * icon_scale),
        ),
        color,
    );
    ui.painter().text(
        rect.min + Vec2::new(36.0 + padding, 13.5),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        color,
    );
    response
}

pub fn outline_button(
    ui: &mut egui::Ui,
    rect: Rect,
    label: &str,
    subtitle: Option<&str>,
    highlight: bool,
    enabled: bool,
) -> egui::Response {
    outline_button_impl(ui, rect, label, subtitle, highlight, enabled, false)
}

pub fn danger_button(ui: &mut egui::Ui, rect: Rect, label: &str, enabled: bool) -> egui::Response {
    outline_button_impl(ui, rect, label, None, false, enabled, true)
}

fn outline_button_impl(
    ui: &mut egui::Ui,
    rect: Rect,
    label: &str,
    subtitle: Option<&str>,
    highlight: bool,
    enabled: bool,
    danger: bool,
) -> egui::Response {
    let response = ui.put(
        rect,
        egui::Button::new("")
            .fill(Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .sense(if enabled {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            }),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    let color = if !enabled {
        Color32::from_gray(170)
    } else if danger {
        Color32::from_rgb(255, 0, 0)
    } else if response.hovered() {
        theme::palette(ui.ctx()).accent
    } else if highlight {
        theme::palette(ui.ctx()).dark
    } else {
        theme::palette(ui.ctx()).text
    };
    let fill = if enabled && danger && (response.hovered() || response.is_pointer_button_down_on())
    {
        Color32::from_rgb(255, 235, 235)
    } else if response.is_pointer_button_down_on() || response.hovered() {
        theme::palette(ui.ctx()).light
    } else {
        Color32::from_white_alpha(85)
    };
    ui.painter().rect(
        rect,
        3.0,
        fill,
        egui::Stroke::new(1.0_f32, color),
        egui::StrokeKind::Inside,
    );
    let center = rect.center() - Vec2::new(0.0, if subtitle.is_some() { 7.5 } else { 0.0 });
    ui.painter().text(
        center,
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(13.0),
        color,
    );
    if let Some(subtitle) = subtitle {
        ui.painter().text(
            Pos2::new(rect.center().x, rect.bottom() - 15.5),
            egui::Align2::CENTER_CENTER,
            subtitle,
            egui::FontId::proportional(11.0),
            GRAY,
        );
    }
    response
}

/// ThemeRefreshMain brush with its original relative endpoints and offsets.
/// WPF projects in physical coordinates after mapping the relative endpoints.
pub fn background(painter: &egui::Painter, rect: Rect) {
    let palette = theme::palette(painter.ctx());
    let stops = palette.background_stops;
    let offsets = palette.background_offsets;
    let start = rect.min
        + Vec2::new(
            rect.width() * palette.background_start.x,
            rect.height() * palette.background_start.y,
        );
    let end = rect.min
        + Vec2::new(
            rect.width() * palette.background_end.x,
            rect.height() * palette.background_end.y,
        );
    let direction = end - start;
    let color = |point: Pos2| {
        let t = (point - start).dot(direction) / direction.length_sq();
        let index = if t < offsets[1] { 0 } else { 1 };
        let fraction =
            ((t - offsets[index]) / (offsets[index + 1] - offsets[index])).clamp(0.0, 1.0);
        blend(stops[index], stops[index + 1], fraction)
    };
    rounded_mesh(painter, rect, 6.0, true, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_theme_repaints_shared_controls_and_window_gradients() {
        let ctx = egui::Context::default();
        let mut settings = pcl_core::config::Settings::default();
        let mut checked = true;
        let draw = |settings: &pcl_core::config::Settings, checked: &mut bool| {
            theme::apply(&ctx, settings);
            let palette = theme::palette(&ctx);
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(500., 300.))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        checkbox(ui, checked, "Checked", "");
                        let (button, _) =
                            ui.allocate_exact_size(Vec2::new(130., 35.), egui::Sense::hover());
                        outline_button(ui, button, "Launch", None, true, true);
                        PclComboBox::from_id_salt("theme-combo")
                            .width(150.)
                            .selected_text("Selection")
                            .show_ui(ui, |_| {});
                        title_background(
                            ui.painter(),
                            Rect::from_min_size(Pos2::new(0., 150.), Vec2::new(500., 48.)),
                        );
                        background(
                            ui.painter(),
                            Rect::from_min_size(Pos2::new(0., 200.), Vec2::new(500., 100.)),
                        );
                    });
                },
            );
            let strokes = output
                .shapes
                .iter()
                .filter_map(|shape| {
                    if let egui::Shape::Rect(rect) = &shape.shape {
                        Some(rect.stroke.color)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            let meshes = output
                .shapes
                .iter()
                .filter_map(|shape| {
                    if let egui::Shape::Mesh(mesh) = &shape.shape {
                        (mesh.vertices.len() > 100).then_some(mesh)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            assert!(
                strokes.contains(&palette.dark),
                "checkbox and launch border use Color2"
            );
            assert!(
                strokes.contains(&palette.control_border),
                "combo border uses ColorBg0"
            );
            assert_eq!(
                meshes.len(),
                2,
                "title and background are both theme meshes"
            );
            assert_eq!(meshes[0].vertices[16].color, palette.title_stops[1]);
            (
                strokes,
                meshes
                    .iter()
                    .map(|mesh| mesh.vertices[0].color)
                    .collect::<Vec<_>>(),
            )
        };
        let blue = draw(&settings, &mut checked);
        let old_dark = theme::palette(&ctx).dark;
        settings.ui_theme = 2;
        let green = draw(&settings, &mut checked);
        assert!(
            !green.0.contains(&old_dark),
            "repaint must not retain the old theme border"
        );
        assert_ne!(
            blue.1, green.1,
            "both window gradients must change in the same context"
        );
        assert!(checked, "a palette change must not alter checkbox state");
    }

    #[test]
    fn checkbox_full_row_click_and_keyboard_preserve_toggle_accessibility() {
        let ctx = egui::Context::default();
        let mut checked = false;
        let mut draw = |events, enabled| {
            let mut result = None;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(500., 200.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        result = Some(
                            ui.add_enabled_ui(enabled, |ui| {
                                ui.set_width(400.);
                                checkbox(ui, &mut checked, "Pack", "description")
                            })
                            .inner,
                        );
                    });
                },
            );
            (result.unwrap(), output, checked)
        };
        let (response, _, _) = draw(vec![], true);
        let point = Pos2::new(response.rect.right() - 2., response.rect.bottom() - 2.);
        let pointer = |pressed| egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let (response, _, state) =
            draw(vec![egui::Event::PointerMoved(point), pointer(true)], true);
        assert!(response.has_focus());
        assert!(!state);
        let (response, output, state) = draw(vec![pointer(false)], true);
        assert!(response.changed() && state);
        let info = output
            .platform_output
            .events
            .iter()
            .map(egui::output::OutputEvent::widget_info)
            .find(|info| info.typ == egui::WidgetType::Checkbox)
            .unwrap();
        assert_eq!(info.label.as_deref(), Some("Pack   description"));
        assert_eq!(info.selected, Some(true));
        assert!(info.enabled);
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let (response, _, state) = draw(vec![key(egui::Key::Space)], true);
        assert!(response.changed() && !state);
        let (response, _, state) = draw(vec![key(egui::Key::Enter)], true);
        assert!(response.changed() && state);
        let (_, _, state) = draw(vec![pointer(true), pointer(false)], false);
        assert!(state, "disabled row must not toggle");
    }

    #[test]
    fn long_selected_path_cannot_push_the_adjacent_control_outside_the_row() {
        let ctx = egui::Context::default();
        let mut combo = Rect::NOTHING;
        let mut adjacent = Rect::NOTHING;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        combo = PclComboBox::from_id_salt("long-java-path")
                            .width(250.0)
                            .selected_text("/very/long/path/to/a/java/runtime/".repeat(15))
                            .show_ui(ui, |_| {})
                            .response
                            .rect;
                        adjacent = ui.add_sized([28.0, 28.0], egui::Button::new("R")).rect;
                    });
                });
            },
        );
        assert!(
            combo.width() <= 250.5,
            "selected path expanded to {}",
            combo.width()
        );
        assert!(adjacent.right() - combo.left() <= 300.0);
    }
}
