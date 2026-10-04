//! PCL message boxes. Geometry/timings follow MyMsgText, MyMsgInput and MyMsgLogin.
//! Closing pictures live only in Context temporary memory; they never execute actions.
use crate::theme;
use eframe::egui::{self, Color32, FontId, Rect, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ModalOptions {
    pub warning: bool,
    pub primary_highlight: bool,
    /// Bit n keeps the dialog open after button n (e.g. opening a recovery URL).
    pub keep_open_buttons: u8,
    pub enter_first: bool,
    pub focus_first: bool,
}
impl Default for ModalOptions {
    fn default() -> Self {
        Self {
            warning: false,
            primary_highlight: true,
            keep_open_buttons: 0,
            enter_first: true,
            focus_first: true,
        }
    }
}
impl ModalOptions {
    pub fn warning() -> Self {
        Self {
            warning: true,
            ..Self::default()
        }
    }
    pub fn device() -> Self {
        Self {
            primary_highlight: false,
            keep_open_buttons: 0b11,
            enter_first: false,
            focus_first: false,
            ..Self::default()
        }
    }
    fn keeps_open(self, index: usize) -> bool {
        index < 8 && self.keep_open_buttons & (1 << index) != 0
    }
    fn mask(self) -> Color32 {
        if self.warning {
            Color32::from_rgba_unmultiplied(80, 0, 0, 140)
        } else {
            Color32::from_black_alpha(90)
        }
    }
}

pub(super) fn account_modal(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    caption: &str,
    buttons: &[&str],
) -> Option<usize> {
    // Legacy callers own visibility (some buttons refresh while keeping the box).
    account_modal_with_options(ctx, id, title, caption, buttons, caller_owned_options())
}
pub(super) fn account_modal_with_options(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    caption: &str,
    buttons: &[&str],
    options: ModalOptions,
) -> Option<usize> {
    let width = (ctx.content_rect().width() - 50.0).clamp(400.0, 600.0);
    let text = modal_text(ctx, caption, width - 66.0);
    let height = text
        .size()
        .y
        .min((ctx.content_rect().height() - 225.0).max(100.0));
    modal_frame_with_options(ctx, id, title, width, height, buttons, options, |ui| {
        egui::ScrollArea::vertical()
            .max_height(height)
            .id_salt(("account-caption", id))
            .show(ui, |ui| {
                ui.add(egui::Label::new(text));
            });
    })
}
pub(super) fn account_input_modal(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    caption: &str,
    value: &mut String,
    buttons: &[&str],
) -> Option<usize> {
    account_input_modal_with_options(
        ctx,
        id,
        title,
        caption,
        value,
        buttons,
        caller_owned_options(),
    )
}
#[allow(clippy::too_many_arguments)]
pub(super) fn account_input_modal_with_options(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    caption: &str,
    value: &mut String,
    buttons: &[&str],
    options: ModalOptions,
) -> Option<usize> {
    let width = (ctx.content_rect().width() - 50.0)
        .clamp(400.0, 600.0)
        .max(508.0);
    let text = modal_text(ctx, caption, width - 66.0);
    let text_height = text.size().y;
    let height = text_height + 7.0 + 28.0 - 5.0;
    let input_id = egui::Id::new(("pcl-modal-input", id));
    modal_frame_inner(
        ctx,
        id,
        title,
        width,
        height,
        buttons,
        options,
        Some(input_id),
        |ui| {
            let top = ui.max_rect().min;
            ui.painter()
                .galley(top, text, theme::palette(ui.ctx()).text);
            let rect = Rect::from_min_size(
                top + Vec2::new(0.0, text_height + 7.0),
                Vec2::new(width - 58.0, 28.0),
            );
            ui.place(
                rect,
                egui::TextEdit::singleline(value)
                    .id(input_id)
                    .char_limit(1000)
                    .font(egui::TextStyle::Body),
            );
        },
    )
}
fn caller_owned_options() -> ModalOptions {
    ModalOptions {
        keep_open_buttons: u8::MAX,
        ..Default::default()
    }
}
fn modal_text(ctx: &egui::Context, caption: &str, width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple(
        caption.into(),
        FontId::proportional(15.0),
        Color32::from_gray(92),
        width,
    );
    for section in &mut job.sections {
        section.format.line_height = Some(18.0);
    }
    ctx.fonts_mut(|fonts| fonts.layout_job(job))
}
pub(super) fn modal_frame(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    width: f32,
    content_height: f32,
    buttons: &[&str],
    body: impl FnOnce(&mut egui::Ui),
) -> Option<usize> {
    modal_frame_with_options(
        ctx,
        id,
        title,
        width,
        content_height,
        buttons,
        caller_owned_options(),
        body,
    )
}
#[allow(clippy::too_many_arguments)]
pub(super) fn modal_frame_with_options(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    width: f32,
    content_height: f32,
    buttons: &[&str],
    options: ModalOptions,
    body: impl FnOnce(&mut egui::Ui),
) -> Option<usize> {
    modal_frame_inner(
        ctx,
        id,
        title,
        width,
        content_height,
        buttons,
        options,
        None,
        body,
    )
}

#[derive(Clone, Copy, Debug)]
struct Pose {
    opacity: f32,
    y: f32,
    angle: f32,
}
fn unit(value: f64, delay: f64, duration: f64) -> f32 {
    ((value - delay) / duration).clamp(0.0, 1.0) as f32
}
fn entering(age: f64) -> Pose {
    let t = unit(age, 0.060, 0.300);
    // ModAnimation: OutBack Weak (power 2), OutFluent Weak (power 2).
    Pose {
        opacity: unit(age, 0.060, 0.120),
        y: 40.0 * (1.0 - t).powi(2) * (1.5 * std::f32::consts::PI * t).cos(),
        angle: -4.0 * (1.0 - t).powi(2),
    }
}
fn leaving(start: Pose, age: f64) -> Pose {
    let t = unit(age, 0.0, 0.150);
    Pose {
        opacity: start.opacity * (1.0 - unit(age, 0.020, 0.080)),
        y: start.y + (20.0 - start.y) * (1.0 - (1.0 - t).powi(3)),
        angle: start.angle + (6.0 - start.angle) * t.powi(2),
    }
}
#[derive(Clone)]
struct Picture {
    meshes: Vec<egui::epaint::Mesh>,
    panel: Rect,
    screen: Rect,
}
#[derive(Clone)]
struct Active {
    key: egui::Id,
    since: f64,
    seen: u64,
    picture: Option<Picture>,
    pose: Pose,
    mask: Color32,
    mask_from: Color32,
    focused: bool,
}
#[derive(Clone)]
struct Retiring {
    active: Active,
    since: f64,
    frame: u64,
}
#[derive(Clone, Default)]
struct Runtime {
    active: Option<Active>,
    retiring: Option<Retiring>,
    // A dismissed caller may remain present for one frame. Never replay its action.
    dismissed: Option<(egui::Id, u64)>,
    action_frame: Option<u64>,
}
fn runtime_id() -> egui::Id {
    egui::Id::new("pcl-modal-runtime")
}
fn load(ctx: &egui::Context) -> Runtime {
    ctx.data_mut(|data| data.get_temp(runtime_id()).unwrap_or_default())
}
fn save(ctx: &egui::Context, state: Runtime) {
    ctx.data_mut(|data| data.insert_temp(runtime_id(), state));
}
fn layer() -> egui::LayerId {
    egui::LayerId::new(egui::Order::Foreground, egui::Id::new("pcl-modal-layer"))
}
fn button_id(key: egui::Id, index: usize) -> egui::Id {
    key.with(("button", index))
}

/// Consume modal keys before children or the underlying launcher can handle them.
/// Repeated Enter/Escape are swallowed but never execute an action (FormMain 781).
fn keyboard(ctx: &egui::Context, count: usize, options: ModalOptions) -> Option<usize> {
    let mut result = None;
    ctx.input_mut(|input| {
        let ime = input
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::Ime(_)));
        input.events.retain(|event| {
            if let egui::Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
                ..
            } = event
            {
                let cancel = *key == egui::Key::Escape || (*key == egui::Key::W && modifiers.ctrl);
                if *key == egui::Key::Space && *repeat {
                    return false;
                }
                let submit = *key == egui::Key::Enter;
                if cancel || submit {
                    if !*repeat && count > 0 && result.is_none() {
                        if cancel {
                            result = Some(count - 1);
                        } else if options.enter_first && !ime {
                            result = Some(0);
                        }
                    }
                    return false;
                }
            }
            true
        });
    });
    result
}

#[allow(clippy::too_many_arguments)]
fn modal_frame_inner(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    width: f32,
    content_height: f32,
    buttons: &[&str],
    options: ModalOptions,
    input_focus: Option<egui::Id>,
    body: impl FnOnce(&mut egui::Ui),
) -> Option<usize> {
    let frame = ctx.cumulative_frame_nr();
    let now = ctx.input(|input| input.time);
    let key = egui::Id::new(("pcl-modal", id));
    let mut state = load(ctx);
    // Multiple independent producers may be pending. The first box rendered in
    // this pass owns it; deferred callers retain their own pending state.
    if state
        .active
        .as_ref()
        .is_some_and(|active| active.key != key && active.seen == frame)
    {
        return None;
    }
    if let Some((dismissed, _)) = state.dismissed {
        if dismissed == key {
            state.dismissed = Some((key, frame));
            save(ctx, state);
            keyboard(ctx, 0, options);
            return None;
        }
        state.dismissed = None;
    }
    if state.active.as_ref().is_none_or(|active| active.key != key) {
        // A fresh dialog supersedes a closing picture. No old action or focus survives.
        let mask_from = state
            .active
            .as_ref()
            .map(|active| active.mask)
            .or_else(|| {
                state.retiring.as_ref().map(|retiring| {
                    let t = unit(now - retiring.since, 0.030, 0.200);
                    retiring.active.mask.gamma_multiply((1.0 - t).powi(2))
                })
            })
            .unwrap_or(Color32::TRANSPARENT);
        // Multiple producers can replace a box in the same UI pass. Keep only
        // the replacement picture rather than doubling the shared backdrop.
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.seen == frame)
            || state
                .retiring
                .as_ref()
                .is_some_and(|retiring| retiring.frame == frame)
        {
            ctx.graphics_mut(|graphics| {
                let list = graphics.entry(layer());
                for index in 0..list.next_idx().0 {
                    list.reset_shape(egui::layers::ShapeIdx(index));
                }
            });
        }
        state.retiring = None;
        state.active = Some(Active {
            key,
            since: now,
            seen: frame,
            picture: None,
            pose: entering(0.0),
            mask: mask_from,
            mask_from,
            focused: false,
        });
    }
    let active = state.active.as_mut().expect("created above");
    active.seen = frame;
    let age = (now - active.since).max(0.0);
    let pose = entering(age);
    let pointer_ready = age >= 0.360;
    let mask = mix_color(active.mask_from, options.mask(), unit(age, 0.0, 0.200));
    let focus = pointer_ready && !active.focused;
    let mut action = keyboard(ctx, buttons.len(), options);
    let screen = ctx.content_rect();
    let mut panel = Rect::NOTHING;
    let mut shape_start = 0;
    ctx.memory_mut(|memory| memory.set_modal_layer(layer()));
    egui::Area::new(layer().id)
        .order(layer().order)
        .fixed_pos(screen.min)
        .movable(false)
        .fade_in(false)
        .show(ctx, |ui| {
            let (_, block) = ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
            ui.painter().rect_filled(screen, 0, mask);
            shape_start = ctx.graphics_mut(|graphics| graphics.entry(layer()).next_idx().0);
            let button_height = ui
                .painter()
                .layout_no_wrap(
                    "登录".into(),
                    FontId::proportional(13.0),
                    theme::palette(ctx).text,
                )
                .size()
                .y
                + 16.0;
            let height = 22.0 + 39.0 + 2.0 + 13.0 + content_height + 17.0 + button_height + 23.0;
            panel = Rect::from_center_size(screen.center(), Vec2::new(width, height));
            let palette = theme::palette(ctx);
            let title_color = if options.warning {
                Color32::from_rgb(255, 76, 76)
            } else {
                palette.dark
            };
            ui.painter().add(
                egui::epaint::Shadow {
                    offset: [0, 4],
                    blur: 20,
                    spread: 0,
                    color: Color32::from_rgba_unmultiplied(
                        palette.text.r(),
                        palette.text.g(),
                        palette.text.b(),
                        204,
                    ),
                }
                .as_shape(panel, 7),
            );
            ui.painter()
                .rect_filled(panel, 7, Color32::from_rgb(251, 251, 251));
            ui.painter().text(
                panel.min + Vec2::new(29.0, 21.0),
                egui::Align2::LEFT_TOP,
                title,
                FontId::proportional(23.0),
                title_color,
            );
            ui.painter().rect_filled(
                Rect::from_min_size(
                    panel.min + Vec2::new(22.0, 61.0),
                    Vec2::new(width - 44.0, 2.0),
                ),
                0,
                title_color,
            );
            let content = Rect::from_min_size(
                panel.min + Vec2::new(29.0, 76.0),
                Vec2::new(width - 66.0, content_height),
            );
            let mut child = ui.new_child(egui::UiBuilder::new().id_salt(key).max_rect(content));
            child.set_clip_rect(content.expand2(Vec2::new(8.0, 5.0)));
            // egui cannot rotate interactive hit regions. Suppress pointer input until
            // the entrance reaches its settled position; keyboard cancel stays immediate.
            if !pointer_ready {
                child.visuals_mut().disabled_alpha = 1.0;
                child.disable();
            }
            body(&mut child);
            if focus {
                if let Some(input_id) = input_focus {
                    ctx.memory_mut(|memory| memory.request_focus(input_id));
                    if let Some(mut edit) = egui::text_edit::TextEditState::load(ctx, input_id) {
                        edit.cursor
                            .set_char_range(Some(egui::text::CCursorRange::one(
                                egui::text::CCursor::new(usize::MAX),
                            )));
                        edit.store(ctx, input_id);
                    }
                }
            }
            let widths: Vec<f32> = buttons
                .iter()
                .map(|text| {
                    ui.painter()
                        .layout_no_wrap((*text).into(), FontId::proportional(13.0), palette.text)
                        .size()
                        .x
                        + 26.0
                })
                .collect();
            let mut x = panel.right()
                - 30.0
                - widths.iter().sum::<f32>()
                - 12.0 * buttons.len().saturating_sub(1) as f32;
            for (index, (text, width)) in buttons.iter().zip(widths).enumerate() {
                let rect = Rect::from_min_size(
                    egui::pos2(x, panel.bottom() - 23.0 - button_height),
                    Vec2::new(width, button_height),
                )
                .shrink2(Vec2::new(5.0, 0.0));
                let response = modal_button(
                    ui,
                    rect,
                    button_id(key, index),
                    text,
                    index == 0 && options.warning,
                    index == 0 && options.primary_highlight && buttons.len() > 1,
                    pointer_ready,
                );
                if focus && input_focus.is_none() && options.focus_first && index == 0 {
                    response.request_focus();
                }
                if response.clicked() && action.is_none() {
                    action = Some(index);
                }
                x += width + 12.0;
            }
            block.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Window, true, title));
        });
    let picture = capture(ctx, shape_start, panel, screen);
    paint_picture(ctx, &picture, pose);
    let active = state.active.as_mut().expect("active dialog");
    active.picture = Some(picture);
    active.pose = pose;
    active.mask = mask;
    active.focused |= focus;
    if age < 0.360 {
        ctx.request_repaint();
    }
    if action.is_some() && state.action_frame == Some(frame) {
        action = None;
    }
    if let Some(index) = action {
        state.action_frame = Some(frame);
        if !options.keeps_open(index) {
            state.retiring = state.active.take().map(|active| Retiring {
                active,
                since: now,
                frame,
            });
            state.dismissed = Some((key, frame));
        }
    }
    save(ctx, state);
    action
}

#[allow(clippy::too_many_arguments)]
fn modal_button(
    ui: &mut egui::Ui,
    rect: Rect,
    id: egui::Id,
    text: &str,
    warning: bool,
    highlighted: bool,
    enabled: bool,
) -> egui::Response {
    let response = ui.interact(
        rect,
        id,
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let palette = theme::palette(ui.ctx());
    let hovered = enabled && response.hovered();
    let color = if warning {
        if hovered {
            Color32::from_rgb(255, 76, 76)
        } else {
            Color32::from_rgb(206, 33, 17)
        }
    } else if hovered {
        palette.accent
    } else if highlighted {
        palette.dark
    } else {
        palette.text
    };
    let fill = if response.is_pointer_button_down_on() {
        if warning {
            Color32::from_rgba_unmultiplied(251, 221, 221, 128)
        } else {
            palette.light
        }
    } else {
        Color32::from_white_alpha(85)
    };
    ui.painter().rect(
        rect,
        3,
        fill,
        egui::Stroke::new(1.0_f32, color),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        FontId::proportional(13.0),
        color,
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, text));
    response
}

/// Call after all dialog producers each frame, including frames without a dialog.
/// A removed device prompt animates out here just like a user-dismissed text box.
pub(super) fn finish_frame(ctx: &egui::Context) {
    let mut state = load(ctx);
    let frame = ctx.cumulative_frame_nr();
    let now = ctx.input(|input| input.time);
    if state
        .active
        .as_ref()
        .is_some_and(|active| active.seen != frame)
    {
        state.retiring = state.active.take().map(|active| Retiring {
            active,
            since: now,
            frame: frame.saturating_sub(1),
        });
    }
    if state.dismissed.is_some_and(|(_, seen)| seen != frame) {
        state.dismissed = None;
    }
    if let Some(retiring) = &state.retiring {
        let age = (now - retiring.since).max(0.0);
        if age >= 0.230 {
            state.retiring = None;
        } else {
            ctx.request_repaint();
            ctx.memory_mut(|memory| memory.set_modal_layer(layer()));
            keyboard(ctx, 0, ModalOptions::default());
            // The action frame already contains its live drawing. Following frames
            // have only a non-interactive picture and the full-screen input barrier.
            if retiring.frame != frame {
                let screen = ctx.content_rect();
                egui::Area::new(layer().id)
                    .order(layer().order)
                    .fixed_pos(screen.min)
                    .movable(false)
                    .fade_in(false)
                    .show(ctx, |ui| {
                        ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
                        let t = unit(age, 0.030, 0.200);
                        ui.painter().rect_filled(
                            screen,
                            0,
                            retiring.active.mask.gamma_multiply((1.0 - t).powi(2)),
                        );
                    });
                if age < 0.150 {
                    if let Some(picture) = &retiring.active.picture {
                        paint_picture(ctx, picture, leaving(retiring.active.pose, age));
                    }
                }
            }
        }
    }
    save(ctx, state);
}

fn capture(ctx: &egui::Context, start: usize, panel: Rect, screen: Rect) -> Picture {
    let shapes = ctx.graphics_mut(|graphics| {
        let list = graphics.entry(layer());
        let result: Vec<_> = list.all_entries().skip(start).cloned().collect();
        for index in start..list.next_idx().0 {
            list.reset_shape(egui::layers::ShapeIdx(index));
        }
        result
    });
    let meshes = ctx
        .tessellate(shapes, ctx.pixels_per_point())
        .into_iter()
        .filter_map(|primitive| {
            match primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) => Some(clip_mesh(&mesh, primitive.clip_rect)),
                egui::epaint::Primitive::Callback(_) => None, // Our message box bodies contain no GPU callbacks.
            }
        })
        .collect();
    Picture {
        meshes,
        panel,
        screen,
    }
}
fn paint_picture(ctx: &egui::Context, picture: &Picture, pose: Pose) {
    if pose.opacity <= 0.0 {
        return;
    }
    let rotation = egui::emath::Rot2::from_angle(pose.angle.to_radians());
    let pivot = egui::pos2(picture.panel.left(), picture.panel.center().y);
    let painter = ctx.layer_painter(layer()).with_clip_rect(picture.screen);
    for original in &picture.meshes {
        let mut mesh = original.clone();
        mesh.rotate(rotation, pivot);
        for vertex in &mut mesh.vertices {
            vertex.pos.y += pose.y;
            vertex.color = vertex.color.gamma_multiply(pose.opacity);
        }
        painter.add(egui::Shape::mesh(mesh));
    }
}
// Clip triangles before rotating, so scrolling captions cannot bleed through the
// rotating content boundary. UV/color interpolation also preserves font edges.
fn clip_mesh(source: &egui::epaint::Mesh, clip: Rect) -> egui::epaint::Mesh {
    let mut result = egui::epaint::Mesh::with_texture(source.texture_id);
    for triangle in source.indices.as_chunks::<3>().0 {
        let mut vertices: Vec<_> = triangle
            .iter()
            .map(|index| source.vertices[*index as usize])
            .collect();
        for edge in 0..4 {
            let input = std::mem::take(&mut vertices);
            if input.is_empty() {
                break;
            }
            let distance = |vertex: &egui::epaint::Vertex| match edge {
                0 => vertex.pos.x - clip.left(),
                1 => clip.right() - vertex.pos.x,
                2 => vertex.pos.y - clip.top(),
                _ => clip.bottom() - vertex.pos.y,
            };
            let mut previous = *input.last().expect("nonempty polygon");
            for current in input {
                let a = distance(&previous);
                let b = distance(&current);
                if (a >= 0.0) != (b >= 0.0) {
                    vertices.push(interpolate(previous, current, a / (a - b)));
                }
                if b >= 0.0 {
                    vertices.push(current);
                }
                previous = current;
            }
        }
        if vertices.len() >= 3 {
            let offset = result.vertices.len() as u32;
            for index in 1..vertices.len() - 1 {
                result.indices.extend_from_slice(&[
                    offset,
                    offset + index as u32,
                    offset + index as u32 + 1,
                ]);
            }
            for vertex in &mut vertices {
                vertex.pos = clip.clamp(vertex.pos);
            }
            result.vertices.extend(vertices);
        }
    }
    result
}
fn interpolate(a: egui::epaint::Vertex, b: egui::epaint::Vertex, t: f32) -> egui::epaint::Vertex {
    egui::epaint::Vertex {
        pos: a.pos.lerp(b.pos, t),
        uv: a.uv.lerp(b.uv, t),
        color: mix_color(a.color, b.color, t),
    }
}
fn mix_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let ac = a.to_array();
    let bc = b.to_array();
    let color: [u8; 4] =
        std::array::from_fn(|i| egui::lerp(ac[i] as f32..=bc[i] as f32, t).round() as u8);
    Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3])
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(key: egui::Key, repeat: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn run(
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
        f: impl FnOnce(&egui::Context),
    ) -> egui::FullOutput {
        let mut f = Some(f);
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(989.0, 517.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| {
                f.take().expect("one pass")(ctx);
                finish_frame(ctx);
            },
        )
    }
    fn text(ctx: &egui::Context, options: ModalOptions) -> Option<usize> {
        account_modal_with_options(
            ctx,
            "test",
            "Title",
            "Caption",
            &["Accept", "Cancel"],
            options,
        )
    }
    #[test]
    fn original_easing_and_separate_mask_tail() {
        let initial = entering(0.0);
        assert_eq!(
            (initial.y, initial.angle, initial.opacity),
            (40.0, -4.0, 0.0)
        );
        let middle = entering(0.210);
        assert!((middle.y - (-7.071068)).abs() < 0.001);
        assert!((middle.angle + 1.0).abs() < 0.001);
        let settled = entering(0.360);
        assert_eq!((settled.y, settled.angle, settled.opacity), (0.0, 0.0, 1.0));
        let exit = leaving(settled, 0.100);
        assert_eq!(exit.opacity, 0.0);
        assert!(exit.angle > 0.0 && exit.angle < 6.0);
        assert!(unit(0.150, 0.030, 0.200) < 1.0);
    }
    #[test]
    fn first_focus_enter_once_and_repeated_key_is_consumed() {
        let ctx = egui::Context::default();
        run(&ctx, 0.0, vec![], |ctx| {
            assert_eq!(text(ctx, ModalOptions::default()), None);
        });
        run(&ctx, 0.4, vec![], |ctx| {
            text(ctx, ModalOptions::default());
        });
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(button_id(egui::Id::new(("pcl-modal", "test")), 0))
        );
        run(&ctx, 0.45, vec![event(egui::Key::Enter, false)], |ctx| {
            assert_eq!(
                text(
                    ctx,
                    ModalOptions {
                        keep_open_buttons: 1,
                        ..Default::default()
                    }
                ),
                Some(0)
            );
        });
        run(&ctx, 0.5, vec![event(egui::Key::Enter, true)], |ctx| {
            assert_eq!(text(ctx, ModalOptions::default()), None);
            assert!(!ctx.input(|input| input.key_pressed(egui::Key::Enter)));
        });
        let mut released = event(egui::Key::Enter, false);
        if let egui::Event::Key { pressed, .. } = &mut released {
            *pressed = false;
        }
        run(&ctx, 0.55, vec![released], |ctx| {
            text(ctx, ModalOptions::default());
        });
        run(&ctx, 0.6, vec![event(egui::Key::Enter, false)], |ctx| {
            assert_eq!(text(ctx, ModalOptions::default()), Some(0));
            assert_eq!(text(ctx, ModalOptions::default()), None);
        });
        run(&ctx, 0.7, vec![event(egui::Key::Enter, false)], |ctx| {
            assert_eq!(text(ctx, ModalOptions::default()), None);
        });
    }
    #[test]
    fn escape_ctrl_w_and_device_enter_follow_original_handlers() {
        let ctx = egui::Context::default();
        run(&ctx, 0.0, vec![event(egui::Key::Enter, false)], |ctx| {
            assert_eq!(
                account_modal_with_options(
                    ctx,
                    "device",
                    "Code",
                    "ABC",
                    &["Web", "Copy", "Cancel"],
                    ModalOptions::device()
                ),
                None
            );
        });
        let mut close = event(egui::Key::W, false);
        if let egui::Event::Key { modifiers, .. } = &mut close {
            modifiers.ctrl = true;
        }
        run(&ctx, 0.5, vec![close], |ctx| {
            assert_eq!(
                account_modal_with_options(
                    ctx,
                    "device",
                    "Code",
                    "ABC",
                    &["Web", "Copy", "Cancel"],
                    ModalOptions::device()
                ),
                Some(2)
            );
        });
        run(&ctx, 0.6, vec![event(egui::Key::Escape, false)], |ctx| {
            assert_eq!(
                account_modal(ctx, "other", "Another", "Message", &["OK"]),
                Some(0)
            );
        });
    }
    #[test]
    fn input_focus_caret_and_typed_text_do_not_select_existing_value() {
        let ctx = egui::Context::default();
        let mut value = "old".to_string();
        for time in [0.0, 0.4] {
            run(&ctx, time, vec![], |ctx| {
                account_input_modal(
                    ctx,
                    "input",
                    "Name",
                    "Caption",
                    &mut value,
                    &["OK", "Cancel"],
                );
            });
        }
        assert_eq!(
            ctx.memory(|m| m.focused()),
            Some(egui::Id::new(("pcl-modal-input", "input")))
        );
        run(&ctx, 0.5, vec![egui::Event::Text(" added".into())], |ctx| {
            assert_eq!(
                account_input_modal(
                    ctx,
                    "input",
                    "Name",
                    "Caption",
                    &mut value,
                    &["OK", "Cancel"]
                ),
                None
            );
        });
        assert_eq!(value, "old added");
    }
    #[test]
    fn ime_commit_enter_never_submits_prompt() {
        let ctx = egui::Context::default();
        run(
            &ctx,
            0.0,
            vec![
                egui::Event::Ime(egui::ImeEvent::Commit("text".into())),
                event(egui::Key::Enter, false),
            ],
            |ctx| {
                assert_eq!(text(ctx, ModalOptions::default()), None);
            },
        );
    }
    #[test]
    fn keep_open_action_then_external_close_leaves_only_picture() {
        let ctx = egui::Context::default();
        let options = ModalOptions {
            keep_open_buttons: 1,
            ..Default::default()
        };
        run(&ctx, 0.0, vec![], |ctx| {
            text(ctx, options);
        });
        run(&ctx, 0.4, vec![event(egui::Key::Enter, false)], |ctx| {
            assert_eq!(text(ctx, options), Some(0));
        });
        assert!(load(&ctx).active.is_some());
        assert!(load(&ctx).retiring.is_none());
        run(&ctx, 0.5, vec![], |_| {});
        assert!(load(&ctx).active.is_none());
        assert!(load(&ctx).retiring.is_some());
        run(&ctx, 0.66, vec![], |_| {});
        assert!(load(&ctx).retiring.is_some()); // Mask lasts longer than the panel.
        run(&ctx, 0.74, vec![], |_| {});
        assert!(load(&ctx).retiring.is_none());
    }
    #[test]
    fn new_dialog_replaces_retiring_action_and_keyboard_focus() {
        let ctx = egui::Context::default();
        run(&ctx, 0.0, vec![event(egui::Key::Escape, false)], |ctx| {
            assert_eq!(text(ctx, ModalOptions::default()), Some(1));
        });
        run(&ctx, 0.1, vec![], |ctx| {
            assert_eq!(account_modal(ctx, "new", "New", "New", &["OK"]), None);
        });
        assert!(load(&ctx).retiring.is_none());
        assert_eq!(
            load(&ctx).active.unwrap().key,
            egui::Id::new(("pcl-modal", "new"))
        );
        run(&ctx, 0.5, vec![], |ctx| {
            account_modal(ctx, "new", "New", "New", &["OK"]);
        });
        assert_eq!(
            ctx.memory(|m| m.focused()),
            Some(button_id(egui::Id::new(("pcl-modal", "new")), 0))
        );
    }
    #[test]
    fn warning_title_and_primary_use_original_red_not_theme_accent() {
        let ctx = egui::Context::default();
        run(&ctx, 0.0, vec![], |ctx| {
            text(ctx, ModalOptions::warning());
        });
        run(&ctx, 0.4, vec![], |ctx| {
            text(ctx, ModalOptions::warning());
        });
        let picture = load(&ctx).active.unwrap().picture.unwrap();
        let colors: Vec<_> = picture
            .meshes
            .iter()
            .flat_map(|mesh| mesh.vertices.iter().map(|v| v.color))
            .collect();
        assert!(colors.contains(&Color32::from_rgb(255, 76, 76)));
        assert!(colors.contains(&Color32::from_rgb(206, 33, 17)));
        assert_eq!(
            ModalOptions::warning().mask(),
            Color32::from_rgba_unmultiplied(80, 0, 0, 140)
        );
    }
    #[test]
    fn clipping_precedes_rotation_for_scrolling_content() {
        let mut source = egui::epaint::Mesh::default();
        source.add_colored_rect(
            Rect::from_min_max(egui::pos2(-10.0, -10.0), egui::pos2(20.0, 20.0)),
            Color32::WHITE,
        );
        let clip = Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(10.0, 10.0));
        let mesh = clip_mesh(&source, clip);
        assert!(!mesh.indices.is_empty());
        assert!(mesh.vertices.iter().all(|vertex| clip.contains(vertex.pos)));
        assert!(mesh.is_valid());
    }
    #[test]
    fn pointer_dismissal_and_exit_mask_never_click_underlying_controls() {
        let ctx = egui::Context::default();
        let key = button_id(egui::Id::new(("pcl-modal", "test")), 0);
        let mut open = true;
        let mut actions = 0;
        let mut underneath = 0;
        let mut draw = |time, events| {
            run(&ctx, time, events, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if ui
                        .interact(
                            ctx.content_rect(),
                            egui::Id::new("underneath"),
                            egui::Sense::click(),
                        )
                        .clicked()
                    {
                        underneath += 1;
                    }
                });
                if open && text(ctx, ModalOptions::default()).is_some() {
                    actions += 1;
                    open = false;
                }
            });
        };
        draw(0.0, vec![]);
        draw(0.4, vec![]);
        let point = ctx.read_response(key).unwrap().rect.center();
        let pointer = |pressed| egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        draw(0.5, vec![egui::Event::PointerMoved(point), pointer(true)]);
        draw(0.52, vec![pointer(false)]);
        draw(0.55, vec![pointer(true)]);
        draw(0.56, vec![pointer(false)]);
        assert_eq!(actions, 1);
        assert_eq!(underneath, 0);
        assert!(load(&ctx).retiring.is_some());
    }
    #[test]
    fn accesskit_click_and_legacy_refresh_keep_the_dialog_usable() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        run(&ctx, 0.0, vec![], |ctx| {
            account_modal(ctx, "refresh", "Refresh", "Caption", &["Reload", "Close"]);
        });
        let output = run(&ctx, 0.4, vec![], |ctx| {
            account_modal(ctx, "refresh", "Refresh", "Caption", &["Reload", "Close"]);
        });
        let key = button_id(egui::Id::new(("pcl-modal", "refresh")), 0);
        let tree = output.platform_output.accesskit_update.unwrap();
        let (_, node) = tree
            .nodes
            .iter()
            .find(|(id, _)| *id == key.value().into())
            .unwrap();
        assert_eq!(node.role(), egui::accesskit::Role::Button);
        assert_eq!(node.label(), Some("Reload"));
        let click = egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::Click,
            target: key.value().into(),
            data: None,
        });
        run(&ctx, 0.5, vec![click.clone()], |ctx| {
            assert_eq!(
                account_modal(ctx, "refresh", "Refresh", "Caption", &["Reload", "Close"]),
                Some(0)
            );
        });
        assert!(load(&ctx).active.is_some());
        assert!(load(&ctx).retiring.is_none());
        run(&ctx, 0.6, vec![click], |ctx| {
            assert_eq!(
                account_modal(ctx, "refresh", "Refresh", "Caption", &["Reload", "Close"]),
                Some(0)
            );
        });
    }
    #[test]
    fn next_dialog_preserves_the_shared_mask_instead_of_flashing_clear() {
        let ctx = egui::Context::default();
        run(&ctx, 0.0, vec![], |ctx| {
            text(ctx, ModalOptions::default());
        });
        run(&ctx, 0.4, vec![], |ctx| {
            text(ctx, ModalOptions::default());
        });
        run(&ctx, 0.5, vec![event(egui::Key::Escape, false)], |ctx| {
            assert_eq!(text(ctx, ModalOptions::default()), Some(1));
        });
        run(&ctx, 0.51, vec![], |ctx| {
            account_modal_with_options(
                ctx,
                "next",
                "Next",
                "Warning",
                &["OK"],
                ModalOptions::warning(),
            );
        });
        assert_eq!(
            load(&ctx).active.as_ref().unwrap().mask,
            Color32::from_black_alpha(90)
        );
        run(&ctx, 0.8, vec![], |ctx| {
            account_modal_with_options(
                ctx,
                "next",
                "Next",
                "Warning",
                &["OK"],
                ModalOptions::warning(),
            );
        });
        assert_eq!(
            load(&ctx).active.as_ref().unwrap().mask,
            ModalOptions::warning().mask()
        );
    }
    #[test]
    fn simultaneous_producers_do_not_restart_entrance_forever() {
        let ctx = egui::Context::default();
        for time in [0.0, 0.2, 0.4] {
            run(&ctx, time, vec![], |ctx| {
                assert_eq!(text(ctx, ModalOptions::default()), None);
                assert_eq!(
                    account_modal_with_options(
                        ctx,
                        "queued",
                        "Queued",
                        "Message",
                        &["OK"],
                        ModalOptions::default()
                    ),
                    None
                );
            });
        }
        let active = load(&ctx).active.unwrap();
        assert_eq!(active.key, egui::Id::new(("pcl-modal", "test")));
        assert_eq!(active.pose.opacity, 1.0);
        assert_eq!(active.pose.angle, 0.0);
        run(&ctx, 0.5, vec![event(egui::Key::Escape, false)], |ctx| {
            assert_eq!(text(ctx, ModalOptions::default()), Some(1));
            assert_eq!(
                account_modal_with_options(
                    ctx,
                    "queued",
                    "Queued",
                    "Message",
                    &["OK"],
                    ModalOptions::default()
                ),
                None
            );
        });
        assert_eq!(
            load(&ctx).active.unwrap().key,
            egui::Id::new(("pcl-modal", "queued"))
        );
        run(&ctx, 0.9, vec![], |ctx| {
            account_modal_with_options(
                ctx,
                "queued",
                "Queued",
                "Message",
                &["OK"],
                ModalOptions::default(),
            );
        });
        assert_eq!(load(&ctx).active.unwrap().pose.opacity, 1.0);
    }
}
