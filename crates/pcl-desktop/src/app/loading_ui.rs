//! MyLoading.xaml / MyLoading.xaml.vb from the fixed upstream source snapshot.
//! Animation time is presentation only: it never generates task progress.
use crate::theme;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Vec2};
use std::time::{Duration, Instant};

const PICKAXE: &str = "M 963.6 858.2 410.816 305.504 C 508.116 213.304 609.204 196.8 711.104 128.6 837.11367 49.573762 879.34045 50.334062 751.5 49.5 611.3 52 471.8 96.2 353.3 182.4 309.8 155.7 252.1 161.2 214.5 198.9 176.9 236.6 171.3 294.2 198 337.7 111.8 456.3 67.6 595.8 65.1 735.9 63.315254 883.82034 65.077966 837.29308 144.2 695.488 212.4 593.588 228.888 492.4 321.088 395.2 L 873.9 948 c 0.60001 0.59999 1.6 0.6 2.2 0 l 87.5 -87.5 c 0.6 -0.7 0.6 -1.6 0 -2.3 z";
const CYCLE: f64 = 1.5;
const WAIT: f64 = 0.4;

/// Compact MyLoading for toolbars and dialogs. It shares the exact pickaxe path,
/// motion and chip timing with the full loader; elapsed time is never progress.
pub(super) fn inline(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.horizontal(|ui| {
        let (rect, response) = ui.allocate_exact_size(Vec2::new(30.0, 25.0), egui::Sense::hover());
        let texture_id = egui::Id::new("pcl-shared-inline-pickaxe");
        let texture = ui
            .ctx()
            .data_mut(|data| data.get_temp::<egui::TextureHandle>(texture_id))
            .unwrap_or_else(|| {
                let texture = pickaxe_texture(ui.ctx());
                ui.ctx()
                    .data_mut(|data| data.insert_temp(texture_id, texture.clone()));
                texture
            });
        let pose = motion(theme::animation_time(ui.ctx()));
        let color = theme::palette(ui.ctx()).accent;
        let origin = rect.min;
        rotated_image(
            ui,
            &texture,
            Rect::from_min_size(origin + Vec2::new(5.0, 3.0), Vec2::splat(17.5)),
            origin + Vec2::new(20.0, 18.0),
            pose.angle.to_radians(),
            color,
        );
        ui.painter().rect_filled(
            Rect::from_min_size(origin + Vec2::new(0.0, 22.5), Vec2::new(12.5, 1.0)),
            0,
            color,
        );
        if pose.chip_opacity > 0.0 {
            for (x, direction) in [(3.5, -1.0), (7.0, 1.0)] {
                let center = origin
                    + Vec2::new(
                        x + 0.75 + direction * pose.chip_offset.x * 0.5,
                        21.75 + pose.chip_offset.y * 0.5,
                    );
                let points = [
                    Vec2::new(-0.75, -1.25),
                    Vec2::new(0.75, -1.25),
                    Vec2::new(0.0, 1.25),
                ]
                .map(|p| center + rotate(p, direction * std::f32::consts::FRAC_PI_4));
                ui.painter().add(egui::Shape::convex_polygon(
                    points.to_vec(),
                    color.gamma_multiply(pose.chip_opacity),
                    egui::Stroke::NONE,
                ));
            }
        }
        if !label.is_empty() {
            ui.label(egui::RichText::new(label).color(color));
        }
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label));
        if theme::animations_enabled(ui.ctx()) {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
        response
    })
    .inner
}

/// The standalone MyLoading XAML control: 60×47 icon, then a 10 DIP gap and
/// centered 16 DIP text. Compact toolbar helpers deliberately use half scale.
pub(super) fn control(ui: &mut egui::Ui, label: &str, size: Vec2) -> egui::Response {
    let color = theme::palette(ui.ctx()).accent;
    let width = size.x.min(ui.available_width()).max(50.0);
    let text = ui
        .painter()
        .layout(label.into(), FontId::proportional(16.0), color, width);
    let content_height = 47.0 + 10.0 + text.size().y;
    let height = size.y.max(content_height).max(50.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
    let origin = Pos2::new(
        rect.center().x - 30.0,
        rect.top() + (height - content_height) / 2.0,
    );
    let id = egui::Id::new("pcl-shared-inline-pickaxe");
    let texture = ui
        .ctx()
        .data(|data| data.get_temp::<egui::TextureHandle>(id))
        .unwrap_or_else(|| {
            let texture = pickaxe_texture(ui.ctx());
            ui.ctx()
                .data_mut(|data| data.insert_temp(id, texture.clone()));
            texture
        });
    let pose = motion(theme::animation_time(ui.ctx()));
    rotated_image(
        ui,
        &texture,
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
    ui.painter().galley(
        Pos2::new(rect.center().x - text.size().x / 2.0, origin.y + 57.0),
        text,
        color,
    );
    if theme::animations_enabled(ui.ctx()) {
        ui.ctx().request_repaint_after(Duration::from_millis(16));
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label));
    response
}

/// PCL's thin theme-colored progress rail. None leaves an unfilled rail rather
/// than fabricating a percentage; callers can show MyLoading alongside it.
pub(super) fn progress(ui: &mut egui::Ui, fraction: Option<f32>, label: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(
            ui.available_width(),
            if label.is_empty() { 6.0 } else { 27.0 },
        ),
        egui::Sense::hover(),
    );
    let colors = theme::palette(ui.ctx());
    let rail = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 3.0));
    ui.painter().rect_filled(rail, 1.5, colors.light);
    if let Some(value) = fraction.filter(|value| value.is_finite()) {
        let fill = Rect::from_min_size(
            rail.min,
            Vec2::new(rail.width() * value.clamp(0.0, 1.0), 3.0),
        );
        ui.painter().rect_filled(fill, 1.5, colors.accent);
    }
    if !label.is_empty() {
        ui.painter().text(
            rect.min + Vec2::new(0.0, 9.0),
            egui::Align2::LEFT_TOP,
            label,
            FontId::proportional(12.0),
            colors.text,
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::ProgressIndicator, true, label)
    });
    response
}

#[derive(Clone, Copy)]
pub(super) enum Placement {
    List,
    Detail,
    Component,
    /// A modal already supplies its own frame and cancel button.
    Dialog,
}

#[derive(Clone, Copy)]
pub(super) enum Status<'a> {
    Ready,
    Running { cancelling: bool },
    Failed(&'a str),
    Cancelled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    None,
    Retry,
    Cancel,
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
    #[cfg(test)]
    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        error: Option<&str>,
        running: bool,
        placement: Placement,
    ) -> Option<bool> {
        self.show_status(
            ui,
            label,
            if let Some(error) = error {
                Status::Failed(error)
            } else if running {
                Status::Running { cancelling: false }
            } else {
                Status::Ready
            },
            placement,
        )
        .map(|action| action == Action::Retry)
    }
    pub(super) fn show_status(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        status: Status<'_>,
        placement: Placement,
    ) -> Option<Action> {
        let running = matches!(status, Status::Running { .. });
        let cancelled = matches!(status, Status::Cancelled);
        let cancelling = matches!(status, Status::Running { cancelling: true });
        let error = match status {
            Status::Failed(error) => Some(error),
            _ => None,
        };
        let elapsed = self.started.elapsed().as_secs_f64();
        let visible = if cancelled {
            self.visible_at.get_or_insert(elapsed);
            true
        } else {
            self.visible(elapsed, running, error.is_some())
        };
        if !visible {
            if running {
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f64((WAIT - elapsed).max(0.01)));
                return Some(Action::None);
            }
            return None;
        }
        let now = elapsed - self.visible_at.unwrap_or(elapsed);
        let failed_at = self.failed_at.map(|at| at - self.visible_at.unwrap_or(at));
        let error_elapsed = failed_at.map(|at| theme::animation_age(ui.ctx(), (now - at).max(0.0)));
        let color = match error_elapsed {
            Some(age) => theme::palette(ui.ctx())
                .accent
                .lerp_to_gamma(Color32::from_rgb(255, 76, 76), (age / 0.3).min(1.0) as f32),
            None => theme::palette(ui.ctx()).accent,
        };
        let max_width = (ui.available_width()
            - match placement {
                Placement::List => 80.0,
                Placement::Detail => 80.0,
                Placement::Component | Placement::Dialog => 0.0,
            })
        .max(100.0);
        let label = if cancelling {
            "正在取消…"
        } else if cancelled {
            "已取消，点击重新获取"
        } else {
            error.unwrap_or(label)
        };
        let mut text_job = egui::text::LayoutJob::simple(
            label.into(),
            FontId::proportional(16.0),
            color,
            (max_width - 40.0).max(60.0),
        );
        text_job.wrap.max_rows = if matches!(placement, Placement::Dialog) {
            ((ui.available_height() - 94.0) / 20.0).clamp(1.0, 5.0) as usize
        } else {
            5
        };
        let text = ui.fonts_mut(|fonts| fonts.layout_job(text_job));
        let card_size = Vec2::new(
            text.size().x.max(60.0) + 40.0,
            20.0 + 47.0 + 10.0 + text.size().y + 17.0,
        );
        let available = ui.available_width();
        let top = ui.cursor().min.y;
        let height = match placement {
            Placement::List => card_size.y + 100.0,
            Placement::Detail => (ui.clip_rect().bottom() - top - 10.0).max(card_size.y + 8.0),
            Placement::Component => card_size.y + 45.0,
            Placement::Dialog => ui.available_height().max(card_size.y),
        };
        let (area, _) = ui.allocate_exact_size(Vec2::new(available, height), egui::Sense::hover());
        let card = Rect::from_center_size(
            Pos2::new(
                area.center().x,
                match placement {
                    Placement::List => area.top() + 50.0 + card_size.y / 2.0,
                    Placement::Detail => area.center().y - 4.0,
                    Placement::Component => area.top() + card_size.y / 2.0,
                    Placement::Dialog => area.center().y,
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
            if error.is_some() || cancelled {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            },
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                if error.is_some() || cancelled {
                    egui::WidgetType::Button
                } else {
                    egui::WidgetType::Label
                },
                true,
                label,
            )
        });
        let response = if error.is_some() || cancelled {
            response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(format!("{label}\n点击重新获取"))
        } else {
            response
        };
        if matches!(placement, Placement::List | Placement::Detail) {
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
        }
        let origin = Pos2::new(card.center().x - 30.0, card.top() + 20.0);
        let pose_time = if cancelled || !theme::animations_enabled(ui.ctx()) {
            0.0
        } else {
            failed_at.map_or(now, |at| now.min((at / CYCLE).floor() * CYCLE + CYCLE))
                * f64::from(theme::animation_speed(ui.ctx()))
        };
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
        let cancel_clicked = if running && !matches!(placement, Placement::Dialog) {
            ui.place(
                Rect::from_center_size(
                    egui::pos2(card.center().x, card.bottom() + 22.0),
                    Vec2::new(70.0, 26.0),
                ),
                egui::Button::new(if cancelling { "取消中…" } else { "取消" })
                    .fill(Color32::TRANSPARENT)
                    .stroke(egui::Stroke::NONE),
            )
            .clicked()
                && !cancelling
        } else {
            false
        };
        if theme::animations_enabled(ui.ctx())
            && (running
                || error_elapsed.is_some_and(|age| age < CYCLE)
                || (!cancelled && error.is_none()))
        {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
        Some(if cancel_clicked {
            Action::Cancel
        } else if (error.is_some() || cancelled) && response.clicked() {
            Action::Retry
        } else {
            Action::None
        })
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
    fn shared_controls_render_finite_geometry_for_unknown_and_invalid_progress() {
        let ctx = egui::Context::default();
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                inline(ui, "loading");
                control(ui, "loading", Vec2::new(200.0, 100.0));
                for value in [
                    None,
                    Some(f32::NAN),
                    Some(f32::INFINITY),
                    Some(-1.0),
                    Some(0.5),
                    Some(2.0),
                ] {
                    progress(ui, value, "progress");
                }
            });
        });
        for shape in output.shapes {
            let rect = shape.shape.visual_bounding_rect();
            assert!(rect.is_finite(), "{rect:?}");
        }
    }
    #[test]
    fn cancelled_control_retries_and_modal_does_not_duplicate_its_cancel_button() {
        let ctx = egui::Context::default();
        let mut indicator = Indicator::default();
        let pos = Pos2::new(425.0, 110.0);
        let mut action = None;
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
                    action = indicator.show_status(
                        ui,
                        "正在获取版本列表",
                        Status::Cancelled,
                        Placement::List,
                    );
                });
            });
        }
        assert_eq!(action, Some(Action::Retry));
        assert!(indicator.failed_at.is_none());
        indicator.start();
        indicator.started -= Duration::from_secs(2);
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                indicator.show_status(
                    ui,
                    "正在获取版本列表",
                    Status::Running { cancelling: false },
                    Placement::Dialog,
                );
            });
        });
        assert!(!output.shapes.iter().any(
            |shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "取消")
        ));
    }
    #[test]
    fn running_cancel_button_emits_a_request_without_turning_into_retry() {
        let ctx = egui::Context::default();
        let mut indicator = Indicator {
            started: Instant::now() - Duration::from_secs(2),
            ..Default::default()
        };
        let mut action = None;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(850.0, 600.0))),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = indicator.show_status(
                    ui,
                    "正在获取版本列表",
                    Status::Running { cancelling: false },
                    Placement::List,
                );
            });
        });
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    (text.galley.text() == "取消").then(|| text.pos + text.galley.size() / 2.0)
                } else {
                    None
                }
            })
            .expect("the loading control exposes a real cancel button");
        for pressed in [true, false] {
            let input = egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    action = indicator.show_status(
                        ui,
                        "正在获取版本列表",
                        Status::Running { cancelling: false },
                        Placement::List,
                    );
                });
            });
        }
        assert_eq!(action, Some(Action::Cancel));
        assert!(indicator.failed_at.is_none());
    }
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
