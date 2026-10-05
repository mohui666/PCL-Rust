//! Source timings from MyPageLeft/MyPageRight and FormMain. Outgoing pages are
//! paint-only snapshots: their controls cannot receive clicks after navigation.
use eframe::egui::{self, epaint::ClippedShape, layers::ShapeIdx, Rect, Vec2};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Content,
    SidebarRows,
    SidebarScale,
    Title,
}
#[derive(Clone)]
struct MotionState {
    key: String,
    previous_key: String,
    started: f64,
    previous: Vec<ClippedShape>,
    displayed: Vec<ClippedShape>,
    running: bool,
    duration: f32,
    previous_textures: Vec<egui::TextureHandle>,
    displayed_textures: Vec<egui::TextureHandle>,
}
pub(super) struct Capture {
    id: egui::Id,
    start: ShapeIdx,
    kind: Kind,
    age: f32,
    state: MotionState,
    bounds: Rect,
}

pub(super) fn begin(ui: &mut egui::Ui, channel: &'static str, key: String, kind: Kind) -> Capture {
    let id = egui::Id::new(("page-motion", channel));
    let now = ui.input(|i| i.time);
    let speed = crate::theme::animation_speed(ui.ctx());
    let mut state = ui
        .ctx()
        .data(|d| d.get_temp::<MotionState>(id))
        .unwrap_or_else(|| MotionState {
            key: key.clone(),
            previous_key: key.clone(),
            started: now,
            previous: Vec::new(),
            displayed: Vec::new(),
            running: false,
            duration: 1.2,
            previous_textures: Vec::new(),
            displayed_textures: Vec::new(),
        });
    if state.key != key {
        state.previous_key = std::mem::replace(&mut state.key, key);
        state.previous = state.displayed.clone();
        state.previous_textures = state.displayed_textures.clone();
        state.started = now;
        state.duration = 1.2;
        state.running = !state.previous.is_empty();
    }
    let age = ((now - state.started).max(0.0) as f32) * speed;
    if speed >= 200.0 || age >= state.duration {
        state.running = false;
        state.previous.clear();
        state.previous_textures.clear();
    }
    // Keep title navigation interruptible; content/old controls are inert until
    // their visual and hit-test positions agree. Disable never mutates values.
    if state.running && kind != Kind::Title {
        ui.visuals_mut().disabled_alpha = 1.0;
        ui.disable();
    }
    let start = ui.painter().add(egui::Shape::Noop);
    Capture {
        id,
        start,
        kind,
        age,
        state,
        bounds: ui.clip_rect(),
    }
}

pub(super) fn finish(ui: &egui::Ui, mut capture: Capture) {
    let layer = ui.layer_id();
    let (end, current) = ui.ctx().graphics(|graphics| {
        let list = graphics.get(layer).expect("capture painter exists");
        (
            list.next_idx(),
            list.all_entries()
                .skip(capture.start.0)
                .cloned()
                .collect::<Vec<_>>(),
        )
    });
    let duration = match capture.kind {
        Kind::Title => 0.55,
        Kind::SidebarScale => 0.54,
        Kind::SidebarRows => 0.44 + (capture.bounds.height() / 36.0).min(16.0) * 0.024,
        Kind::Content => {
            0.49 + card_rectangles(&current, capture.bounds).len().min(31) as f32 * 0.025
        }
    };
    capture.state.duration = duration;
    if capture.age >= duration {
        capture.state.running = false;
        capture.state.previous.clear();
        capture.state.previous_textures.clear();
    }
    let output = if capture.state.running {
        ui.ctx().request_repaint();
        let title_sub_to_sub = capture.kind == Kind::Title
            && !capture.state.key.is_empty()
            && !capture.state.previous_key.is_empty();
        let mut output = animated(
            &capture.state.previous,
            capture.kind,
            capture.age,
            false,
            capture.bounds,
            title_sub_to_sub,
            capture.state.key.is_empty(),
        );
        output.extend(animated(
            &current,
            capture.kind,
            capture.age,
            true,
            capture.bounds,
            title_sub_to_sub,
            capture.state.key.is_empty(),
        ));
        output
    } else {
        current
    };
    if capture.state.running {
        ui.ctx().graphics_mut(|graphics| {
            let list = graphics.entry(layer);
            for index in capture.start.0..end.0 {
                list.reset_shape(ShapeIdx(index));
            }
            for shape in &output {
                list.add(shape.clip_rect, shape.shape.clone());
            }
        });
    }
    capture.state.displayed_textures = retain_textures(ui.ctx(), &output);
    capture.state.displayed = output;
    ui.ctx()
        .data_mut(|data| data.insert_temp(capture.id, capture.state));
}

// Snapshot geometry outlives page state (including its image owners). Retain the
// managed textures while a displayed or outgoing snapshot can still paint them.
fn retain_textures(ctx: &egui::Context, shapes: &[ClippedShape]) -> Vec<egui::TextureHandle> {
    fn visit(shape: &egui::Shape, ids: &mut std::collections::HashSet<egui::TextureId>) {
        if let egui::Shape::Vec(children) = shape {
            for child in children {
                visit(child, ids);
            }
        } else {
            ids.insert(shape.texture_id());
        }
    }
    let mut ids = std::collections::HashSet::new();
    for shape in shapes {
        visit(&shape.shape, &mut ids);
    }
    let manager = ctx.tex_manager();
    let mut locked = manager.write();
    ids.into_iter()
        .filter_map(|id| {
            if locked.meta(id).is_some() {
                locked.retain(id);
                Some(egui::TextureHandle::new(manager.clone(), id))
            } else {
                None
            }
        })
        .collect()
}

fn ease_out(t: f32, p: i32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(p)
}
fn back(t: f32, p: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powf(3.0 - p * 0.5) * (1.5 * std::f32::consts::PI * t).cos()
}
fn part(age: f32, duration: f32) -> f32 {
    (age / duration).clamp(0.0, 1.0)
}

fn card_rectangles(shapes: &[ClippedShape], bounds: Rect) -> Vec<Rect> {
    let mut cards: Vec<Rect> = shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Rect(r)
                if r.rect.width() > bounds.width() * 0.55
                    && r.rect.height() >= 34.0
                    && r.corner_radius.nw >= 3 =>
            {
                Some(r.rect)
            }
            _ => None,
        })
        .collect();
    cards.sort_by(|a, b| a.top().total_cmp(&b.top()));
    cards.dedup_by(|a, b| (a.top() - b.top()).abs() < 6.0);
    cards
}

#[derive(Clone)]
struct WidthMotion {
    from: f32,
    target: f32,
    started: f64,
}
pub(super) fn sidebar_width(ctx: &egui::Context, target: f32) -> f32 {
    let id = egui::Id::new("sidebar-width-motion");
    let now = ctx.input(|i| i.time);
    let speed = crate::theme::animation_speed(ctx);
    let mut state = ctx
        .data(|d| d.get_temp::<WidthMotion>(id))
        .unwrap_or(WidthMotion {
            from: target,
            target,
            started: now,
        });
    let current = state.from
        + (state.target - state.from)
            * ease_out(((now - state.started) as f32 * speed - 0.11) / 0.18, 5);
    if state.target != target {
        state = WidthMotion {
            from: current,
            target,
            started: now,
        };
    }
    let age = (now - state.started) as f32 * speed;
    let value = if speed >= 200.0 {
        target
    } else {
        state.from + (target - state.from) * ease_out((age - 0.11) / 0.18, 5)
    };
    if (value - target).abs() > 0.01 {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(id, state));
    value
}

fn animated(
    shapes: &[ClippedShape],
    kind: Kind,
    age: f32,
    enter: bool,
    bounds: Rect,
    sub_to_sub: bool,
    returning_main: bool,
) -> Vec<ClippedShape> {
    // Card rectangles provide the same element boundary as GetAllAnimControls.
    // Shadows and all child text/images stay in their card's animation group.
    let cards = card_rectangles(shapes, bounds);
    let rows = ((bounds.height() / 36.0).ceil() as usize).clamp(1, 32);
    let mut result = Vec::new();
    for shape in shapes {
        if matches!(shape.shape, egui::Shape::Noop) {
            continue;
        }
        let center = shape.shape.visual_bounding_rect().center();
        let index = match kind {
            Kind::SidebarRows => {
                (((center.y - bounds.top() - 12.0) / 36.0).floor().max(0.0) as usize).min(31)
            }
            Kind::Content => cards
                .iter()
                .position(|r| r.expand(8.0).contains(center))
                .unwrap_or(cards.len())
                .min(31),
            _ => 0,
        };
        let (opacity, offset, scale) = match kind {
            Kind::Title if sub_to_sub => {
                if enter {
                    (part(age - 0.16, 0.15), Vec2::ZERO, 1.0)
                } else {
                    (1.0 - part(age, 0.13), Vec2::ZERO, 1.0)
                }
            }
            Kind::Title => {
                if enter {
                    (
                        part(age - 0.2, 0.15),
                        Vec2::new(
                            if returning_main {
                                12.0 * (1.0 - back((age - 0.2) / 0.35, 2.0))
                            } else {
                                -18.0 * (1.0 - back((age - 0.2) / 0.35, 3.0))
                            },
                            0.0,
                        ),
                        1.0,
                    )
                } else {
                    (
                        1.0 - part(age, 0.15),
                        Vec2::new(
                            if returning_main {
                                -18.0 * part(age, 0.15).powi(3)
                            } else {
                                12.0 * part(age, 0.15).powi(2)
                            },
                            0.0,
                        ),
                        1.0,
                    )
                }
            }
            Kind::SidebarScale => {
                if enter {
                    let t = age - 0.14;
                    (part(t, 0.1), Vec2::ZERO, 0.96 + 0.04 * back(t / 0.4, 2.0))
                } else {
                    (
                        1.0 - part(age - 0.03, 0.08),
                        Vec2::ZERO,
                        1.0 - 0.05 * part(age, 0.11).powi(2),
                    )
                }
            }
            Kind::SidebarRows => {
                if enter {
                    let delay = (0..index)
                        .map(|i| (15.0 - i as f32).max(7.0) * 0.002)
                        .sum::<f32>();
                    let t = age - 0.14 - delay;
                    (
                        ease_out(t / 0.1, 2),
                        Vec2::new(
                            -25.0 + 5.0 * ease_out(t / 0.2, 3) + 20.0 * back(t / 0.3, 2.0),
                            0.0,
                        ),
                        1.0,
                    )
                } else {
                    let t = part(age - 0.07 / rows as f32 * index as f32, 0.05);
                    (1.0 - t, Vec2::new(-6.0 * t, 0.0), 1.0)
                }
            }
            Kind::Content => {
                if enter {
                    let t = age - 0.14 - index as f32 * 0.025;
                    (
                        ease_out(t / 0.1, 2),
                        Vec2::new(
                            0.0,
                            -16.0 + 5.0 * ease_out(t / 0.25, 3) + 11.0 * back(t / 0.35, 3.0),
                        ),
                        1.0,
                    )
                } else {
                    let t = part(age - index as f32 * 0.015, 0.07);
                    (1.0 - t, Vec2::new(0.0, -6.0 * t), 1.0)
                }
            }
        };
        if opacity <= 0.0 {
            continue;
        }
        let mut shape = shape.clone();
        shape.shape.transform(egui::emath::TSTransform::new(
            bounds.center().to_vec2() * (1.0 - scale) + offset,
            scale,
        ));
        egui::epaint::shape_transform::adjust_colors(&mut shape.shape, move |color| {
            *color = color.gamma_multiply(opacity)
        });
        shape.clip_rect = shape.clip_rect.intersect(bounds);
        result.push(shape);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Vec<ClippedShape> {
        vec![ClippedShape {
            clip_rect: Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(900.0, 500.0)),
            shape: egui::Shape::rect_filled(
                Rect::from_min_size(egui::pos2(25.0, 25.0), Vec2::new(800.0, 80.0)),
                5,
                egui::Color32::WHITE,
            ),
        }]
    }
    #[test]
    fn content_exit_enter_and_back_ease_use_real_separate_phases() {
        let shape = sample();
        let bounds = shape[0].clip_rect;
        assert!(animated(&shape, Kind::Content, 0.1, false, bounds, false, false).is_empty());
        assert!(animated(&shape, Kind::Content, 0.1, true, bounds, false, false).is_empty());
        let settled = animated(&shape, Kind::Content, 1.2, true, bounds, false, false);
        assert_eq!(settled[0].shape, shape[0].shape);
        assert!(back(0.7, 3.0) > 1.0);
        let entering = animated(&shape, Kind::SidebarRows, 0.18, true, bounds, false, false);
        assert!(
            entering[0].shape.visual_bounding_rect().left()
                < shape[0].shape.visual_bounding_rect().left()
        );
    }
    #[test]
    fn outgoing_snapshot_keeps_image_alive_until_transition_finishes() {
        let ctx = egui::Context::default();
        let texture = ctx.load_texture(
            "page-image",
            egui::ColorImage::filled([2, 2], egui::Color32::WHITE),
            Default::default(),
        );
        let texture_id = texture.id();
        let draw = |key: &str, time: f64, image: bool| {
            let _ = ctx.run(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let capture = begin(ui, "image-test", key.into(), Kind::Content);
                        if image {
                            ui.image((texture_id, egui::vec2(20.0, 20.0)));
                        } else {
                            ui.label(key);
                        }
                        finish(ui, capture);
                    });
                },
            );
        };
        draw("A", 0.0, true);
        drop(texture);
        assert!(ctx.tex_manager().read().meta(texture_id).is_some());
        draw("B", 0.01, false);
        assert!(ctx.tex_manager().read().meta(texture_id).is_some());
        draw("B", 1.0, false);
        assert!(ctx.tex_manager().read().meta(texture_id).is_none());
    }
    #[test]
    fn rapid_navigation_replaces_outgoing_snapshot_and_animation_off_is_immediate() {
        let ctx = egui::Context::default();
        let draw = |key: &str, time: f64, off: bool| {
            let _ = ctx.run(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ctx| {
                    let mut settings = pcl_core::config::Settings::default();
                    if off {
                        settings.system.debug_animation = 30;
                    }
                    crate::theme::apply(ctx, &settings);
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let capture = begin(ui, "test", key.into(), Kind::Content);
                        ui.label(key);
                        finish(ui, capture);
                    });
                },
            );
        };
        draw("A", 0.0, false);
        draw("B", 0.1, false);
        draw("C", 0.12, false);
        let id = egui::Id::new(("page-motion", "test"));
        let state = ctx.data(|d| d.get_temp::<MotionState>(id)).unwrap();
        assert_eq!(state.key, "C");
        assert!(state.running);
        draw("D", 0.13, true);
        let state = ctx.data(|d| d.get_temp::<MotionState>(id)).unwrap();
        assert!(!state.running);
        assert!(state.previous.is_empty());
    }
}
