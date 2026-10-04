//! Source-bound ModMain.Hint queue, colors and motion; hints never steal focus.
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Vec2};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HintKind {
    Info,
    Success,
    Error,
}
impl HintKind {
    fn colors(self, weight: f32) -> (Color32, Color32) {
        let (top, bottom) = match self {
            Self::Info => ([37, 155, 252], [10, 142, 252]),
            Self::Success => ([33, 177, 33], [29, 160, 29]),
            Self::Error => ([255, 53, 11], [255, 43, 0]),
        };
        let color = |c: [u8; 3]| {
            let channel = |v: u8| (255.0 + (v as f32 - 255.0) * weight).round() as u8;
            Color32::from_rgba_unmultiplied(
                channel(c[0]),
                channel(c[1]),
                channel(c[2]),
                (255.0 - 40.0 * weight).round() as u8,
            )
        };
        (color(top), color(bottom))
    }
}
fn hold(text: &str) -> Duration {
    // VB String.Length counts UTF-16 code units.
    Duration::from_millis(800 + text.encode_utf16().count().clamp(5, 23) as u64 * 180)
}
fn duration(text: &str) -> Duration {
    hold(text) + Duration::from_millis(300) // 200 ms slide, then 100 ms collapse.
}
#[derive(Clone)]
struct Hint {
    id: u64,
    text: String,
    kind: HintKind,
    entered: Instant,
    refreshed: Option<Instant>,
    grow: bool,
    shake_start_x: f32,
}
impl Hint {
    fn epoch(&self) -> Instant {
        self.refreshed.unwrap_or(self.entered)
    }
    fn layout(&self, now: Instant) -> HintLayout {
        let elapsed = now.saturating_duration_since(self.entered).as_secs_f32();
        let age = now.saturating_duration_since(self.epoch()).as_secs_f32();
        let delay = hold(&self.text).as_secs_f32();
        let leaving = ((age - delay) / 0.2).clamp(0.0, 1.0);
        let opacity = (elapsed / 0.1).min(1.0) * (1.0 - ((age - delay) / 0.15).clamp(0.0, 1.0));
        let out = |t: f32| 1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3);
        let elastic = |t: f32| {
            let t = 1.0 - t.clamp(0.0, 1.0);
            1.0 - t.powf(1.25) * (2.5 * std::f32::consts::PI * (1.0 - t).powf(1.5)).cos()
        };
        let mut x = -70.0 + 30.0 * elastic(elapsed / 0.4) + 20.0 * out(elapsed / 0.2);
        let flash = if self.refreshed.is_some() {
            // Four 50 ms source-bound duplicate-message nudges, settling at -20.
            let k = (age / 0.05).floor() as usize;
            let p = (age / 0.05).fract();
            let points = [self.shake_start_x, -12.0, -20.0, -12.0, -20.0];
            if k < 4 {
                let eased = if k.is_multiple_of(2) {
                    out(p)
                } else {
                    p.powi(3)
                };
                x = points[k] + (points[k + 1] - points[k]) * eased;
            }
            (age / 0.25).clamp(0.0, 1.0)
        } else {
            ((elapsed - 0.1) / 0.25).clamp(0.0, 1.0)
        };
        let growth = if self.grow { out(elapsed / 0.15) } else { 1.0 };
        HintLayout {
            x: x - 50.0 * leaving.powi(3),
            height: 26.0 * growth * (1.0 - out((age - delay - 0.2) / 0.1)),
            opacity,
            color_weight: 0.3 + 0.7 * flash,
        }
    }
}
struct HintLayout {
    x: f32,
    height: f32,
    opacity: f32,
    color_weight: f32,
}
#[derive(Default)]
pub(super) struct HintQueue {
    active: Vec<Hint>,
    next_id: u64,
}
impl HintQueue {
    #[cfg(test)]
    pub(super) fn contains(&self, kind: HintKind, text: &str) -> bool {
        self.active
            .iter()
            .any(|hint| hint.kind == kind && hint.text == text)
    }

    pub(super) fn push(&mut self, kind: HintKind, text: impl Into<String>) {
        self.push_at(kind, text.into(), Instant::now());
    }
    fn expire(&mut self, now: Instant) {
        self.active
            .retain(|hint| now.saturating_duration_since(hint.epoch()) < duration(&hint.text));
    }
    fn push_at(&mut self, kind: HintKind, text: String, now: Instant) {
        self.expire(now);
        let text = text
            .split(['\r', '\n'])
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if text.is_empty() || self.active.len() >= 20 {
            return;
        }
        if let Some(hint) = self.active.iter_mut().find(|hint| {
            hint.text == text && now.saturating_duration_since(hint.epoch()) < hold(&hint.text)
        }) {
            if now.saturating_duration_since(hint.entered) >= Duration::from_millis(400) {
                hint.shake_start_x = hint.layout(now).x;
                hint.kind = kind;
                hint.refreshed = Some(now);
            }
            return;
        }
        self.next_id = self.next_id.wrapping_add(1);
        self.active.push(Hint {
            id: self.next_id,
            text,
            kind,
            entered: now,
            refreshed: None,
            grow: !self.active.is_empty(),
            shake_start_x: -20.0,
        });
    }
    pub(super) fn show(&mut self, ctx: &egui::Context) {
        self.show_at(ctx, Instant::now());
    }
    fn show_at(&mut self, ctx: &egui::Context, now: Instant) {
        self.expire(now);
        let screen = ctx.content_rect();
        let mut y = screen.bottom()
            - 20.0
            - self
                .active
                .iter()
                .map(|h| h.layout(now).height)
                .sum::<f32>();
        for hint in &self.active {
            let layout = hint.layout(now);
            draw_hint(ctx, screen, hint, &layout, y);
            y += layout.height;
        }
        if !self.active.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}
fn draw_hint(ctx: &egui::Context, screen: Rect, hint: &Hint, layout: &HintLayout, y: f32) {
    egui::Area::new(egui::Id::new(("notification", hint.id)))
        .fixed_pos(Pos2::new(screen.left() + layout.x, y))
        .order(egui::Order::Middle)
        .movable(false)
        .fade_in(false)
        .constrain(false)
        .interactable(false)
        .show(ctx, |ui| {
            ui.set_clip_rect(Rect::from_min_max(
                egui::pos2(screen.left(), y),
                egui::pos2(screen.right(), y + layout.height),
            ));
            let mut job = egui::text::LayoutJob::simple(
                hint.text.clone(),
                FontId::proportional(13.0),
                Color32::WHITE.linear_multiply(layout.opacity),
                (screen.width() - 61.0).max(1.0),
            );
            job.wrap.max_rows = 1;
            let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
            let (rect, _) = ui.allocate_exact_size(
                Vec2::new(galley.size().x + 41.0, 26.0),
                egui::Sense::hover(),
            );
            let (top, bottom) = hint.kind.colors(layout.color_weight);
            let (top, bottom) = (
                top.linear_multiply(layout.opacity),
                bottom.linear_multiply(layout.opacity),
            );
            let mut mesh = egui::Mesh::default();
            let mut points = vec![rect.left_top()];
            for (center, angle) in [
                (
                    rect.right_top() + Vec2::new(-6.0, 6.0),
                    -std::f32::consts::FRAC_PI_2,
                ),
                (rect.right_bottom() - Vec2::splat(6.0), 0.0),
            ] {
                for step in 0..=6 {
                    points.push(
                        center
                            + Vec2::angled(angle + step as f32 / 6.0 * std::f32::consts::FRAC_PI_2)
                                * 6.0,
                    );
                }
            }
            points.push(rect.left_bottom());
            mesh.colored_vertex(rect.center(), top.lerp_to_gamma(bottom, 0.5));
            for point in &points {
                mesh.colored_vertex(
                    *point,
                    top.lerp_to_gamma(bottom, (point.y - rect.top()) / rect.height()),
                );
            }
            for index in 0..points.len() {
                mesh.add_triangle(0, index as u32 + 1, ((index + 1) % points.len()) as u32 + 1);
            }
            ui.painter().add(egui::Shape::mesh(mesh));
            ui.painter().galley(
                Pos2::new(rect.left() + 33.0, rect.center().y - galley.size().y / 2.0),
                galley,
                Color32::WHITE,
            );
        });
}

pub(super) fn inline(ui: &mut egui::Ui, text: &str, yellow: bool) {
    let (fill, border, foreground) = if yellow {
        (
            Color32::from_rgb(255, 235, 215),
            Color32::from_rgb(245, 122, 0),
            Color32::from_rgb(216, 108, 0),
        )
    } else {
        (
            Color32::from_rgb(217, 236, 255),
            Color32::from_rgb(17, 114, 212),
            Color32::from_rgb(15, 100, 184),
        )
    };
    let mut job = egui::text::LayoutJob::simple(
        text.into(),
        FontId::proportional(13.0),
        foreground,
        (ui.available_width() - 27.0).max(1.0),
    );
    for section in &mut job.sections {
        section.format.line_height = Some(16.0);
    }
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), galley.size().y + 18.0),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(rect, 2, fill);
    ui.painter().rect_filled(
        Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())),
        0,
        border,
    );
    ui.painter()
        .galley(rect.min + Vec2::new(15.0, 9.0), galley, foreground);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_results_keep_their_severity_and_text_after_later_progress() {
        let now = Instant::now();
        let mut hints = HintQueue::default();
        hints.push_at(HintKind::Success, "网页登录成功！".into(), now);
        hints.push_at(HintKind::Error, "应用访问被拒绝\r\n请检查配置".into(), now);
        hints.push_at(HintKind::Info, "后续状态".into(), now);
        assert_eq!(hints.active.len(), 3);
        assert!(hints.contains(HintKind::Success, "网页登录成功！"));
        assert!(hints.contains(HintKind::Error, "应用访问被拒绝 请检查配置"));
        assert_eq!(
            HintKind::Success.colors(1.0).0,
            Color32::from_rgba_unmultiplied(33, 177, 33, 215)
        );
        assert_eq!(
            HintKind::Error.colors(1.0).1,
            Color32::from_rgba_unmultiplied(255, 43, 0, 215)
        );
        assert_eq!(HintKind::Info.colors(0.3).0.a(), 243);
    }
    #[test]
    fn repeats_refresh_after_entry_but_expired_hints_cannot_swallow_new_events() {
        let now = Instant::now();
        let mut hints = HintQueue::default();
        hints.push_at(HintKind::Info, "重复事件".into(), now);
        let id = hints.active[0].id;
        hints.push_at(
            HintKind::Error,
            "重复事件".into(),
            now + Duration::from_millis(200),
        );
        assert_eq!(hints.active[0].kind, HintKind::Info);
        assert!(hints.active[0].refreshed.is_none());
        let later = now + Duration::from_millis(600);
        hints.push_at(HintKind::Error, "重复事件".into(), later);
        assert_eq!(hints.active.len(), 1);
        assert_eq!(hints.active[0].id, id);
        assert_eq!(hints.active[0].refreshed, Some(later));
        assert!(hints.contains(HintKind::Error, "重复事件"));
        let during_shake = later + Duration::from_millis(75);
        let x_before = hints.active[0].layout(during_shake).x;
        hints.push_at(HintKind::Error, "重复事件".into(), during_shake);
        assert!((hints.active[0].layout(during_shake).x - x_before).abs() < 0.001);
        let later = during_shake;
        hints.expire(later + duration("重复事件"));
        assert!(hints.active.is_empty());
        hints.push_at(
            HintKind::Success,
            "重复事件".into(),
            later + duration("重复事件"),
        );
        assert_ne!(hints.active[0].id, id);
    }
    #[test]
    fn queue_bound_and_utf16_lifetime_include_exit_collapse() {
        let now = Instant::now();
        let mut hints = HintQueue::default();
        for index in 0..21 {
            hints.push_at(HintKind::Info, format!("事件{index}"), now);
        }
        assert_eq!(hints.active.len(), 20);
        assert_eq!(duration("短"), Duration::from_millis(2000));
        assert_eq!(duration("😀😀😀"), Duration::from_millis(2180));
        let hint = &hints.active[0];
        let leaving = now + hold(&hint.text);
        assert_eq!(hint.layout(leaving).height, 26.0);
        assert!((hint.layout(leaving + Duration::from_millis(250)).height - 3.25).abs() < 0.001);
        assert!(
            hint.layout(leaving + Duration::from_millis(150))
                .opacity
                .abs()
                < 0.001
        );
        hints.expire(now + Duration::from_secs(10));
        assert!(hints.active.is_empty());
    }
    #[test]
    fn rendering_all_three_hints_never_steals_keyboard_focus() {
        let now = Instant::now();
        let mut hints = HintQueue::default();
        for (kind, text) in [
            (HintKind::Info, "信息"),
            (HintKind::Success, "成功"),
            (HintKind::Error, "失败"),
        ] {
            hints.push_at(kind, text.into(), now);
        }
        let ctx = egui::Context::default();
        let focus = egui::Id::new("underlying-input");
        for frame in 0..3 {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(850.0, 600.0))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let mut value = String::new();
                        ui.add(egui::TextEdit::singleline(&mut value).id(focus));
                    });
                    if frame == 0 {
                        ctx.memory_mut(|memory| memory.request_focus(focus));
                    }
                    hints.show_at(ctx, now + Duration::from_millis(500));
                },
            );
        }
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(focus));
        assert_eq!(hints.active.len(), 3);
    }
    #[test]
    fn active_login_hint_is_painted_behind_the_message_box() {
        let now = Instant::now();
        let mut hints = HintQueue::default();
        hints.push_at(HintKind::Success, "login succeeded".into(), now);
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        for frame in 0..3 {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(850.0, 600.0))),
                    time: Some(frame as f64),
                    ..Default::default()
                },
                |ctx| {
                    hints.show_at(ctx, now + Duration::from_millis(500));
                    super::super::modal_ui::account_modal(
                        ctx,
                        "hint-layer-test",
                        "Login failed",
                        "Service denied access",
                        &["Close"],
                    );
                    super::super::modal_ui::finish_frame(ctx);
                },
            );
            shapes = output.shapes;
        }
        let has_color = |shape: &egui::Shape, color| matches!(shape, egui::Shape::Mesh(mesh) if mesh.vertices.iter().any(|v| v.color == color));
        let hint = shapes
            .iter()
            .position(|s| has_color(&s.shape, HintKind::Success.colors(1.0).0))
            .expect("hint mesh is visible");
        let panel = shapes
            .iter()
            .rposition(|s| has_color(&s.shape, Color32::from_rgb(251, 251, 251)))
            .expect("message panel mesh is visible");
        assert!(
            hint < panel,
            "message box must cover the hint just as PanMsg covers PanHint"
        );
    }
}
