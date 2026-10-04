//! MyLoading.xaml / MyLoading.xaml.vb, fixed PCL 2.13.1.1 source.
//! Animation time is presentation only: it never generates task progress.
use crate::theme;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Vec2};
use std::time::{Duration, Instant};

const PICKAXE: &str = "M 963.6 858.2 410.816 305.504 C 508.116 213.304 609.204 196.8 711.104 128.6 837.11367 49.573762 879.34045 50.334062 751.5 49.5 611.3 52 471.8 96.2 353.3 182.4 309.8 155.7 252.1 161.2 214.5 198.9 176.9 236.6 171.3 294.2 198 337.7 111.8 456.3 67.6 595.8 65.1 735.9 63.315254 883.82034 65.077966 837.29308 144.2 695.488 212.4 593.588 228.888 492.4 321.088 395.2 L 873.9 948 c 0.60001 0.59999 1.6 0.6 2.2 0 l 87.5 -87.5 c 0.6 -0.7 0.6 -1.6 0 -2.3 z";
const CYCLE: f64 = 1.5;
const WAIT: f64 = 0.4;

#[derive(Clone, Copy)]
pub(super) enum Placement {
    List,
    Detail,
}

pub(super) struct Indicator {
    started: Instant,
    visible_at: Option<f64>,
    failed_at: Option<f64>,
    texture: Option<egui::TextureHandle>,
}
impl Default for Indicator {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            visible_at: None,
            failed_at: None,
            texture: None,
        }
    }
}
impl Indicator {
    pub(super) fn start(&mut self) {
        self.started = Instant::now();
        self.visible_at = None;
        self.failed_at = None;
    }
    fn visible(&mut self, elapsed: f64, running: bool, failed: bool) -> bool {
        if failed {
            self.failed_at.get_or_insert(elapsed);
            self.visible_at.get_or_insert(elapsed);
            return true;
        }
        if running && elapsed >= WAIT {
            self.visible_at.get_or_insert(elapsed);
            return true;
        }
        // MyPageRight avoids flashing the loader for a fast request and keeps an
        // entered loader visible at least 400ms. No network/busy state is changed.
        self.visible_at
            .is_some_and(|visible| elapsed < visible + WAIT)
    }
    /// Some means the loading view owns this area; true is a click on a failed
    /// control. None means the real content can be shown.
    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        error: Option<&str>,
        running: bool,
        placement: Placement,
    ) -> Option<bool> {
        let elapsed = self.started.elapsed().as_secs_f64();
        let visible = self.visible(elapsed, running, error.is_some());
        if !visible {
            if running {
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f64((WAIT - elapsed).max(0.01)));
                return Some(false);
            }
            return None;
        }
        let now = elapsed - self.visible_at.unwrap_or(elapsed);
        let failed_at = self.failed_at.map(|at| at - self.visible_at.unwrap_or(at));
        let error_elapsed = failed_at.map(|at| (now - at).max(0.0));
        let color = match error_elapsed {
            Some(age) => theme::palette(ui.ctx())
                .accent
                .lerp_to_gamma(Color32::from_rgb(255, 76, 76), (age / 0.3).min(1.0) as f32),
            None => theme::palette(ui.ctx()).accent,
        };
        let max_width = (ui.available_width()
            - match placement {
                Placement::List => 80.0,
                Placement::Detail => 0.0,
            })
        .max(100.0);
        let text = ui.painter().layout(
            error.unwrap_or(label).into(),
            FontId::proportional(16.0),
            color,
            (max_width - 40.0).max(60.0),
        );
        let card_size = Vec2::new(
            text.size().x.max(60.0) + 40.0,
            20.0 + 47.0 + 10.0 + text.size().y + 17.0,
        );
        let available = ui.available_width();
        let top = ui.cursor().min.y;
        let height = match placement {
            Placement::List => card_size.y + 100.0,
            Placement::Detail => (ui.clip_rect().bottom() - top - 10.0).max(card_size.y + 8.0),
        };
        let (area, _) = ui.allocate_exact_size(Vec2::new(available, height), egui::Sense::hover());
        let card = Rect::from_center_size(
            Pos2::new(
                area.center().x,
                match placement {
                    Placement::List => area.top() + 50.0 + card_size.y / 2.0,
                    Placement::Detail => area.center().y - 4.0,
                },
            ),
            card_size,
        );
        let control = Rect::from_min_max(
            card.min + Vec2::new(20.0, 20.0),
            card.max - Vec2::new(20.0, 17.0),
        );
        let response = ui.interact(
            control,
            ui.id().with("pcl-resource-loader"),
            if error.is_some() {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            },
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                if error.is_some() {
                    egui::WidgetType::Button
                } else {
                    egui::WidgetType::Label
                },
                true,
                error.unwrap_or(label),
            )
        });
        let response = if error.is_some() {
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            response
        };
        ui.painter().add(
            egui::epaint::Shadow {
                offset: [0, 2],
                blur: 3,
                spread: 0,
                color: Color32::from_black_alpha(9),
            }
            .as_shape(card, 5),
        );
        ui.painter()
            .rect_filled(card, 5, Color32::from_rgba_unmultiplied(255, 255, 255, 245));
        let origin = Pos2::new(card.center().x - 30.0, card.top() + 20.0);
        let pose_time = failed_at.map_or(now, |at| now.min((at / CYCLE).floor() * CYCLE + CYCLE));
        let pose = motion(pose_time);
        if self.texture.is_none() {
            self.texture = Some(pickaxe_texture(ui.ctx()));
        }
        let texture = self.texture.as_ref().expect("fixed pickaxe texture");
        rotated_image(
            ui,
            texture,
            Rect::from_min_size(origin + Vec2::new(10.0, 6.0), Vec2::splat(35.0)),
            origin + Vec2::new(40.0, 36.0),
            pose.angle.to_radians(),
            color,
        );
        ui.painter().rect_filled(
            Rect::from_min_size(origin + Vec2::new(0.0, 45.0), Vec2::new(25.0, 2.0)),
            0,
            color,
        );
        if pose.chip_opacity > 0.0 {
            for (x, direction) in [(7.0, -1.0), (14.0, 1.0)] {
                let center = origin
                    + Vec2::new(
                        x + 1.5 + direction * pose.chip_offset.x,
                        43.5 + pose.chip_offset.y,
                    );
                let points = [
                    Vec2::new(-1.5, -2.5),
                    Vec2::new(1.5, -2.5),
                    Vec2::new(0.0, 2.5),
                ]
                .map(|p| center + rotate(p, direction * std::f32::consts::FRAC_PI_4));
                ui.painter().add(egui::Shape::convex_polygon(
                    points.to_vec(),
                    color.gamma_multiply(pose.chip_opacity),
                    egui::Stroke::NONE,
                ));
            }
        }
        if let Some(age) = error_elapsed {
            let wait = if failed_at.unwrap_or(0.0).rem_euclid(CYCLE) < 0.6 {
                0.4
            } else {
                0.0
            };
            let t = age - 0.3 - wait;
            if t > 0.0 {
                let scale = 0.6 + 0.4 * ease_out_back((t / 0.4) as f32);
                let opacity = (t / 0.1).min(1.0) as f32;
                let center = origin + Vec2::new(12.5, 31.5);
                // Exact PathError polygon, scaled from its 20x20 coordinate space.
                let points = [
                    (2., 0.),
                    (0., 2.),
                    (8., 10.),
                    (0., 18.),
                    (2., 20.),
                    (10., 12.),
                    (18., 20.),
                    (20., 18.),
                    (12., 10.),
                    (20., 2.),
                    (18., 0.),
                    (10., 8.),
                ];
                // The cross is concave; use its two diagonal quadrilaterals.
                for indices in [[0, 1, 6, 7], [3, 4, 9, 10]] {
                    let vertices = indices.map(|i| {
                        center
                            + Vec2::new(points[i].0 - 10.0, points[i].1 - 10.0)
                                * (17.0 / 20.0 * scale)
                    });
                    ui.painter().add(egui::Shape::convex_polygon(
                        vertices.to_vec(),
                        color.gamma_multiply(opacity),
                        egui::Stroke::NONE,
                    ));
                }
            }
        }
        ui.painter().galley(
            Pos2::new(card.center().x - text.size().x / 2.0, card.top() + 77.0),
            text,
            color,
        );
        if running || error_elapsed.is_some_and(|age| age < CYCLE) || error.is_none() {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
        Some(error.is_some() && response.clicked())
    }
}

#[derive(Debug)]
struct Motion {
    angle: f32,
    chip_opacity: f32,
    chip_offset: Vec2,
}
fn motion(elapsed: f64) -> Motion {
    let t = elapsed.rem_euclid(CYCLE) as f32;
    let angle = if t < 0.25 {
        55.0
    } else if t < 0.6 {
        let v = ((t - 0.25) / 0.35).clamp(0.0, 1.0);
        55.0 - 75.0 * v.powi(2) * (1.5 * std::f32::consts::PI * (1.0 - v)).cos()
    } else {
        let v = ((t - 0.6) / 0.9).clamp(0.0, 1.0);
        let out = 1.0 - (1.0 - v).powi(3);
        let elastic = 1.0 - (1.0 - v).powf(1.25) * (2.5 * std::f32::consts::PI * v.powf(1.5)).cos();
        -20.0 + 50.0 * out + 25.0 * elastic
    };
    let chip_time = t - 0.6;
    let travel = 1.0 - (1.0 - (chip_time / 0.18).clamp(0.0, 1.0)).powi(3);
    Motion {
        angle,
        chip_opacity: if chip_time < 0.0 {
            0.0
        } else {
            1.0 - ((chip_time - 0.05) / 0.1).clamp(0.0, 1.0)
        },
        chip_offset: Vec2::new(5.0 * travel, -6.0 * travel),
    }
}
fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powf(1.5) * (1.5 * std::f32::consts::PI * t).cos()
}
fn rotate(v: Vec2, angle: f32) -> Vec2 {
    Vec2::new(
        v.x * angle.cos() - v.y * angle.sin(),
        v.x * angle.sin() + v.y * angle.cos(),
    )
}
fn rotated_image(
    ui: &egui::Ui,
    texture: &egui::TextureHandle,
    rect: Rect,
    pivot: Pos2,
    angle: f32,
    color: Color32,
) {
    let mut mesh = egui::Mesh::with_texture(texture.id());
    for (pos, uv) in [
        (rect.left_top(), Pos2::new(0., 0.)),
        (rect.right_top(), Pos2::new(1., 0.)),
        (rect.right_bottom(), Pos2::new(1., 1.)),
        (rect.left_bottom(), Pos2::new(0., 1.)),
    ] {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: pivot + rotate(pos - pivot, angle),
            uv,
            color,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    ui.painter().add(egui::Shape::mesh(mesh));
}
fn pickaxe_texture(ctx: &egui::Context) -> egui::TextureHandle {
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024"><path d="{PICKAXE}"/></svg>"#
    );
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())
        .expect("fixed source pickaxe parses");
    let bounds = tree.root().abs_bounding_box();
    let scale = 33.0 / bounds.height();
    let x = 1.0 - bounds.x() * scale;
    let y = 1.0 - bounds.y() * scale;
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="35" height="35"><path d="{PICKAXE}" transform="matrix({scale} 0 0 {scale} {x} {y})" fill="none" stroke="white" stroke-width="{}"/></svg>"#,
        2.0 / scale
    );
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())
        .expect("fixed stroked pickaxe parses");
    let mut pixels = resvg::tiny_skia::Pixmap::new(140, 140).expect("bounded pickaxe raster");
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(4., 4.),
        &mut pixels.as_mut(),
    );
    ctx.load_texture(
        "source-loading-pickaxe",
        egui::ColorImage::from_rgba_premultiplied([140, 140], pixels.data()),
        egui::TextureOptions::LINEAR,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fast_requests_never_flash_and_visible_loader_has_source_minimum_stay() {
        let mut indicator = Indicator::default();
        assert!(!indicator.visible(0.2, true, false));
        assert!(!indicator.visible(0.3, false, false));
        assert!(indicator.visible(0.5, true, false));
        assert!(indicator.visible(0.7, false, false));
        assert!(!indicator.visible(0.91, false, false));
        indicator.start();
        assert!(indicator.visible(0.1, false, true));
    }
    #[test]
    fn pickaxe_strikes_then_returns_without_synthetic_percentage() {
        assert!((motion(0.0).angle - 55.0).abs() < 0.001);
        assert!((motion(0.6).angle + 20.0).abs() < 0.001);
        assert!((motion(1.5).angle - 55.0).abs() < 0.001);
        assert_eq!(motion(0.4).chip_opacity, 0.0);
        assert!(motion(0.65).chip_opacity > 0.9);
        assert_eq!(motion(0.8).chip_opacity, 0.0);
        assert!(motion(0.7).chip_offset.y < 0.0);
    }
    #[test]
    fn failure_card_click_retries_but_running_card_does_not() {
        for failed in [false, true] {
            let ctx = egui::Context::default();
            let mut indicator = Indicator {
                started: Instant::now() - Duration::from_secs(2),
                ..Default::default()
            };
            let pos = Pos2::new(425.0, 110.0);
            let mut retry = None;
            for pressed in [None, Some(true), Some(false)] {
                let mut input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(850.0, 600.0))),
                    ..Default::default()
                };
                if let Some(pressed) = pressed {
                    input.events = vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ];
                }
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        retry = indicator.show(
                            ui,
                            "正在获取 Mod 列表",
                            failed.then_some("服务暂不可用"),
                            !failed,
                            Placement::List,
                        );
                    });
                });
            }
            assert_eq!(retry, Some(failed));
        }
    }
    #[test]
    fn exact_source_pickaxe_renders_nonempty_pixels() {
        let ctx = egui::Context::default();
        let texture = pickaxe_texture(&ctx);
        assert_eq!(texture.size(), [140, 140]);
        let delta = ctx.tex_manager().write().take_delta();
        let image = &delta
            .set
            .iter()
            .find(|(id, _)| *id == texture.id())
            .unwrap()
            .1
            .image;
        let egui::ImageData::Color(image) = image;
        assert!(image.pixels.iter().filter(|c| c.a() > 0).count() > 100);
        assert!(image.pixels.iter().filter(|c| c.a() == 0).count() > 100);
    }
}
