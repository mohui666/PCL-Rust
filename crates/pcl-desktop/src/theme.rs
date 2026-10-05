//! Palette formulas and IDs follow PCL 2.13.1.1, commit 0e0d12fdce6a2804916fb2be60e41144da637c18:
//! ModSecret.vb:64-128, ModBase.vb:339-394, PageSetupUI.xaml and Settings.vb:131-135.
//! The public ModSecret omits the per-theme parameter switch and rainbow phase.
//! Presets 1..13 below are explicitly Rust approximations by theme name, not
//! recovered private parameters. Theme 0, HSL2, and gradient geometry are source-bound.
//! Custom slider ranges/defaults are public; subtracting 20 from its brightness
//! slider supplies a neutral midpoint. The rainbow cycle is a Rust implementation.
use eframe::egui::{Color32, Context, Id, Pos2, Stroke};
use pcl_core::config::Settings;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub accent: Color32,
    pub dark: Color32,
    pub text: Color32,
    pub light: Color32,
    pub border: Color32,
    pub hover: Color32,
    pub pale: Color32,
    pub lightest: Color32,
    pub control_border: Color32,
    pub pressed: Color32,
    pub title_stops: [Color32; 3],
    pub background_stops: [Color32; 3],
    pub background_offsets: [f32; 3],
    pub background_start: Pos2,
    pub background_end: Pos2,
}

pub fn theme_name(id: u8) -> &'static str {
    match id {
        0 => "龙猫蓝",
        1 => "甜柠青",
        2 => "小草绿",
        3 => "菠萝黄",
        4 => "橡木棕",
        5 => "玄素黑",
        6 => "铁杆粉",
        7 => "神秘紫",
        8 => "秋仪金",
        9 => "活跃橙",
        10 => "跳票红",
        11 => "极客蓝",
        12 => "滑稽彩",
        13 => "欧皇彩",
        14 => "自定义",
        _ => "龙猫蓝",
    }
}

pub fn palette(ctx: &Context) -> Palette {
    ctx.data(|data| data.get_temp::<Palette>(Id::new("pcl-theme-palette")))
        .unwrap_or_else(|| from_settings(&Settings::default(), 0.0))
}

pub fn apply(ctx: &Context, settings: &Settings) {
    let colors = from_settings(settings, ctx.input(|input| input.time));
    let changed =
        ctx.data(|data| data.get_temp::<Palette>(Id::new("pcl-theme-palette"))) != Some(colors);
    ctx.data_mut(|data| data.insert_temp(Id::new("pcl-theme-palette"), colors));
    if changed {
        ctx.style_mut(|style| {
            let visuals = &mut style.visuals;
            visuals.override_text_color = Some(colors.text);
            visuals.hyperlink_color = colors.accent;
            // egui shares this stroke between the focused TextEdit border and
            // selected glyphs. Keep Color3 visible against both white inputs
            // and the pale selection fill; a white stroke hides the border.
            visuals.selection.bg_fill = colors.pale;
            visuals.selection.stroke = Stroke::new(1.0_f32, colors.accent);
            visuals.text_cursor.stroke.color = colors.accent;
            visuals.widgets.noninteractive.fg_stroke.color = colors.text;
            visuals.widgets.inactive.fg_stroke.color = colors.text;
            visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, colors.control_border);
            for widget in [
                &mut visuals.widgets.hovered,
                &mut visuals.widgets.active,
                &mut visuals.widgets.open,
            ] {
                widget.fg_stroke.color = colors.accent;
                widget.bg_stroke = Stroke::new(1.0_f32, colors.accent);
                widget.weak_bg_fill = colors.light;
                widget.bg_fill = colors.pale;
            }
            // MyTextBox.vb: idle ColorBg0, hover Color4, focus Color3.
            // Custom buttons and combo boxes paint their own borders.
            visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, colors.border);
        });
    }
    if settings.ui_theme == 12 {
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

fn from_settings(settings: &Settings, time: f64) -> Palette {
    // h, saturation, lightness adjustment, symmetric title hue spread.
    let (h, s, l, delta) = match settings.ui_theme {
        1 => (180.0, 85.0, 0.0, 0.0),
        2 => (120.0, 85.0, 0.0, 0.0),
        3 => (60.0, 85.0, 0.0, 0.0),
        4 => (30.0, 65.0, -10.0, 0.0),
        5 => (0.0, 0.0, -20.0, 0.0),
        6 => (330.0, 75.0, 10.0, 0.0),
        7 => (270.0, 70.0, 0.0, 0.0),
        8 => (45.0, 85.0, 0.0, 15.0),
        9 => (30.0, 90.0, 0.0, 0.0),
        10 => (0.0, 85.0, 0.0, 0.0),
        11 => (220.0, 95.0, -5.0, 0.0),
        12 => (
            (time.max(0.0) * 4.0).floor().rem_euclid(240.0) * 1.5,
            85.0,
            0.0,
            60.0,
        ),
        13 => (45.0, 85.0, 0.0, 120.0),
        14 => (
            settings.ui_theme_hue as f64,
            settings.ui_theme_saturation as f64,
            settings.ui_theme_lightness as f64 - 20.0,
            settings.ui_theme_gradient as f64,
        ),
        _ => (210.0, 85.0, 0.0, 0.0),
    };
    let border = hsl2_raw(h, s, 65.0 + l);
    let hover = hsl2_raw(h, s, 80.0 + l * 0.4);
    let light = hsl2(h, s, 95.0);
    let title_hues = if settings.ui_theme == 13 {
        [0.0, 120.0, 240.0]
    } else {
        [h - delta, h, h + delta]
    };
    Palette {
        text: hsl2(h, s * 0.2, 25.0 + l * 0.3),
        dark: hsl2(h, s, 45.0 + l),
        accent: hsl2(h, s, 55.0 + l),
        border: rgb(border),
        hover: rgb(hover),
        pale: hsl2(h, s, 91.0 + l * 0.1),
        light,
        lightest: hsl2(h, s, 97.0),
        control_border: rgb(std::array::from_fn(|i| {
            border[i] * 0.4 + hover[i] * 0.4 + 166.0 * 0.2
        })),
        pressed: Color32::from_rgba_unmultiplied(light.r(), light.g(), light.b(), 190),
        title_stops: [
            hsl2(title_hues[0], s, 48.0 + l),
            hsl2(title_hues[1], s, 54.0 + l),
            hsl2(title_hues[2], s, 48.0 + l),
        ],
        background_stops: if settings.ui_background_colorful {
            [
                hsl2(h - 15.0, s * 0.8, 91.0),
                hsl2(h, s * 0.8, 91.0),
                hsl2(h + 15.0, s * 0.8, 91.0),
            ]
        } else {
            [Color32::from_gray(245); 3]
        },
        background_offsets: [-0.1, 0.4, 1.1],
        background_start: Pos2::new(0.9, 0.0),
        background_end: Pos2::new(0.1, 1.0),
    }
}

fn rgb(channels: [f64; 3]) -> Color32 {
    let [r, g, b] = channels.map(|value| value.clamp(0.0, 255.0).round_ties_even() as u8);
    Color32::from_rgb(r, g, b)
}
fn hsl2(h: f64, s: f64, l: f64) -> Color32 {
    rgb(hsl2_raw(h, s, l))
}

fn hsl2_raw(h: f64, s: f64, l: f64) -> [f64; 3] {
    if s == 0.0 {
        return [l * 2.55; 3];
    }
    let hue = h.rem_euclid(360.0);
    let centers = [
        0.1, -0.06, -0.3, -0.19, -0.15, -0.24, -0.32, -0.09, 0.18, 0.05, -0.12, -0.02, 0.1, -0.06,
    ];
    let segment = hue / 30.0;
    let index = segment.floor() as usize;
    let fraction = segment - index as f64;
    let center = 50.0 - ((1.0 - fraction) * centers[index] + fraction * centers[index + 1]) * s;
    let light = if l < center {
        l / center
    } else {
        1.0 + (l - center) / (100.0 - center)
    } * 0.5;
    let saturation = s / 100.0;
    let upper = if light < 0.5 {
        saturation * light + light
    } else {
        saturation * (1.0 - light) + light
    };
    let lower = 2.0 * light - upper;
    let channel = |offset: f64| {
        let mut component = hue / 360.0 + offset;
        if component < 0.0 {
            component += 1.0;
        }
        if component > 1.0 {
            component -= 1.0;
        }
        255.0
            * if component < 0.16667 {
                lower + (upper - lower) * 6.0 * component
            } else if component < 0.5 {
                upper
            } else if component < 0.66667 {
                lower + (upper - lower) * (4.0 - component * 6.0)
            } else {
                lower
            }
    };
    [channel(1.0 / 3.0), channel(0.0), channel(-1.0 / 3.0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_blue_keeps_the_source_palette_and_gradient_geometry() {
        let p = from_settings(&Settings::default(), 0.0);
        assert_eq!(p.text, Color32::from_rgb(51, 62, 72));
        assert_eq!(p.dark, Color32::from_rgb(15, 100, 184));
        assert_eq!(p.accent, Color32::from_rgb(18, 122, 225));
        assert_eq!(p.border, Color32::from_rgb(60, 150, 239));
        assert_eq!(p.hover, Color32::from_rgb(144, 195, 246));
        assert_eq!(p.pale, Color32::from_rgb(205, 228, 251));
        assert_eq!(p.light, Color32::from_rgb(227, 240, 253));
        assert_eq!(p.lightest, Color32::from_rgb(238, 246, 254));
        assert_eq!(
            p.pressed,
            Color32::from_rgba_unmultiplied(227, 240, 253, 190)
        );
        assert_eq!(
            p.title_stops,
            [[16, 106, 196], [18, 119, 221], [16, 106, 196]]
                .map(|[r, g, b]| Color32::from_rgb(r, g, b))
        );
        assert_eq!(
            p.background_stops,
            [[202, 234, 245], [211, 229, 247], [219, 226, 248]]
                .map(|[r, g, b]| Color32::from_rgb(r, g, b))
        );
        assert_eq!(p.background_offsets, [-0.1, 0.4, 1.1]);
        assert_eq!(p.background_start, Pos2::new(0.9, 0.0));
        assert_eq!(p.background_end, Pos2::new(0.1, 1.0));
    }

    #[test]
    fn theme_ids_follow_the_source_tags_and_all_fifteen_choices_change_the_palette() {
        let names: Vec<_> = [0, 1, 2, 3, 4, 5, 12, 6, 7, 13, 8, 9, 10, 11, 14]
            .into_iter()
            .map(theme_name)
            .collect();
        assert_eq!(
            names,
            [
                "龙猫蓝",
                "甜柠青",
                "小草绿",
                "菠萝黄",
                "橡木棕",
                "玄素黑",
                "滑稽彩",
                "铁杆粉",
                "神秘紫",
                "欧皇彩",
                "秋仪金",
                "活跃橙",
                "跳票红",
                "极客蓝",
                "自定义"
            ]
        );
        let mut seen = Vec::new();
        for id in 0..=14 {
            let settings = Settings {
                ui_theme: id,
                ..Default::default()
            };
            let current = from_settings(&settings, 0.0);
            assert!(
                !seen.contains(&current),
                "theme {id} duplicated another palette"
            );
            seen.push(current);
        }
    }

    #[test]
    fn custom_wrap_grayscale_and_background_switch_do_not_change_presets() {
        assert_eq!(hsl2(-15.0, 80.0, 50.0), hsl2(345.0, 80.0, 50.0));
        assert_eq!(hsl2(360.0, 80.0, 50.0), hsl2(0.0, 80.0, 50.0));
        assert_eq!(hsl2(120.0, 0.0, 80.0), Color32::from_gray(204));
        let settings = Settings {
            ui_theme: 14,
            ui_theme_hue: 210.0,
            ui_theme_saturation: 85.0,
            ui_theme_lightness: 20.0,
            ui_theme_gradient: 0.0,
            ..Default::default()
        };
        assert_eq!(
            from_settings(&settings, 0.0),
            from_settings(&Settings::default(), 0.0)
        );
        let plain = from_settings(
            &Settings {
                ui_background_colorful: false,
                ..settings.clone()
            },
            0.0,
        );
        assert_eq!(plain.background_stops, [Color32::from_gray(245); 3]);
        assert_eq!(plain.accent, from_settings(&settings, 0.0).accent);
        let changed = Settings {
            ui_theme: 0,
            ui_theme_hue: 12.0,
            ui_theme_saturation: 0.0,
            ..settings
        };
        assert_eq!(
            from_settings(&changed, 0.0),
            from_settings(&Settings::default(), 0.0)
        );
    }

    #[test]
    fn rust_rainbow_is_time_based_and_contexts_do_not_share_theme_state() {
        let rainbow = Settings {
            ui_theme: 12,
            ..Default::default()
        };
        assert_eq!(from_settings(&rainbow, 0.0), from_settings(&rainbow, 0.249));
        assert_ne!(from_settings(&rainbow, 0.0), from_settings(&rainbow, 0.25));
        assert_eq!(from_settings(&rainbow, 0.0), from_settings(&rainbow, 60.0));
        let a = Context::default();
        let b = Context::default();
        let selected = Settings {
            ui_theme: 7,
            ..Default::default()
        };
        apply(&a, &selected);
        assert_eq!(palette(&a), from_settings(&selected, 0.0));
        assert_eq!(a.style().visuals.hyperlink_color, palette(&a).accent);
        assert_eq!(a.style().visuals.selection.bg_fill, palette(&a).pale);
        assert_eq!(
            a.style().visuals.override_text_color,
            Some(palette(&a).text)
        );
        assert_eq!(palette(&b), from_settings(&Settings::default(), 0.0));
    }

    #[test]
    fn text_edit_keeps_visible_state_borders_while_typing_selecting_and_changing_theme() {
        use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Rect, Vec2};

        let ctx = Context::default();
        ctx.set_visuals(egui::Visuals::light());
        let mut settings = Settings::default();
        let mut text = String::from("Resource");
        let draw = |settings: &Settings, text: &mut String, events| {
            apply(&ctx, settings);
            let mut response = None;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 120.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        response = Some(ui.add_sized(
                            [240.0, 28.0],
                            egui::TextEdit::singleline(text).id_salt("resource-name"),
                        ));
                    });
                },
            );
            (response.unwrap(), output)
        };
        let assert_border = |response: &egui::Response, output: &egui::FullOutput, color| {
            let border = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect)
                    if rect.rect.contains_rect(response.rect)
                        && rect.rect.width() <= response.rect.width() + 3.0 =>
                {
                    Some(rect.stroke)
                }
                _ => None,
            });
            assert_eq!(border, Some(Stroke::new(1.0_f32, color)));
        };
        let (idle, output) = draw(&settings, &mut text, vec![]);
        assert_border(&idle, &output, palette(&ctx).control_border);
        let point = idle.rect.center();
        let (hovered, output) = draw(&settings, &mut text, vec![Event::PointerMoved(point)]);
        assert!(hovered.hovered());
        assert_border(&hovered, &output, palette(&ctx).border);
        let pointer = |pressed| Event::PointerButton {
            pos: point,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let _ = draw(&settings, &mut text, vec![pointer(true)]);
        let (focused, output) = draw(&settings, &mut text, vec![pointer(false)]);
        assert!(focused.has_focus());
        assert_border(&focused, &output, palette(&ctx).accent);
        let (focused, output) = draw(&settings, &mut text, vec![Event::Text("Na".into())]);
        assert!(focused.changed() && text.contains("Na"));
        assert_border(&focused, &output, palette(&ctx).accent);
        let (focused, output) = draw(
            &settings,
            &mut text,
            vec![Event::Key {
                key: Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers {
                    command: true,
                    ..Modifiers::NONE
                },
            }],
        );
        assert_border(&focused, &output, palette(&ctx).accent);
        let selection_colors = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(&text.galley),
                _ => None,
            })
            .flat_map(|galley| galley.rows.iter())
            .flat_map(|row| row.visuals.mesh.vertices.iter())
            .map(|vertex| vertex.color)
            .collect::<Vec<_>>();
        assert!(
            selection_colors.contains(&palette(&ctx).pale),
            "select-all must paint a visible selection background"
        );
        assert!(
            selection_colors.contains(&palette(&ctx).accent),
            "selected glyphs must retain a contrasting theme color"
        );
        assert_ne!(
            ctx.style().visuals.selection.stroke.color,
            ctx.style().visuals.selection.bg_fill
        );
        let blue = palette(&ctx).accent;
        settings.ui_theme = 2;
        let (focused, output) = draw(&settings, &mut text, vec![]);
        assert!(focused.has_focus());
        assert_ne!(palette(&ctx).accent, blue);
        assert_border(&focused, &output, palette(&ctx).accent);
    }
}
