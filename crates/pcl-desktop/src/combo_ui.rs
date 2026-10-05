//! MyComboBox / MyComboBoxItem from the fixed upstream Application.xaml.
//! Popup selection is provisional while navigating; Enter commits, Escape cancels.
use crate::theme;
use eframe::egui::{self, Color32, FontId, Id, Key, Pos2, Rect, Response, Sense, Vec2};

const HEIGHT: f32 = 28.0;
const ROW: f32 = 26.0;
const MAX_POPUP: f32 = 320.0;

#[derive(Clone)]
struct Item {
    text: String,
    enabled: bool,
    selected: bool,
}
#[derive(Clone, Default)]
struct State {
    items: Vec<Item>,
    highlight: Option<usize>,
    was_open: bool,
    opened_on_pointer_press: bool,
    search: String,
    search_at: f64,
}
#[derive(Clone, Copy)]
struct ColorAnimation {
    from: Color32,
    to: Color32,
    since: f64,
    duration: f64,
    palette: theme::Palette,
}
fn animated_color(ctx: &egui::Context, id: Id, target: Color32, duration: f64) -> Color32 {
    let now = ctx.input(|i| i.time);
    let palette = theme::palette(ctx);
    let mut animation = ctx
        .data_mut(|d| d.get_temp::<ColorAnimation>(id))
        .unwrap_or(ColorAnimation {
            from: target,
            to: target,
            since: now,
            duration,
            palette,
        });
    if animation.palette != palette {
        animation = ColorAnimation {
            from: target,
            to: target,
            since: now,
            duration,
            palette,
        };
    }
    let progress = ((now - animation.since) / animation.duration).clamp(0.0, 1.0) as f32;
    let current = animation.from.lerp_to_gamma(animation.to, progress);
    if animation.to != target {
        animation = ColorAnimation {
            from: current,
            to: target,
            since: now,
            duration,
            palette,
        };
    }
    if now - animation.since < animation.duration {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(id, animation));
    current
}

pub struct PclComboBox {
    id: Id,
    width: Option<f32>,
    text: egui::WidgetText,
    arrow_only: bool,
    editing: bool,
}
impl PclComboBox {
    pub fn from_id_salt(id: impl std::hash::Hash) -> Self {
        Self {
            id: Id::new(id),
            width: None,
            text: "".into(),
            arrow_only: false,
            editing: false,
        }
    }
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }
    pub fn selected_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.text = text.into();
        self
    }
    pub fn show_ui<R>(
        self,
        ui: &mut egui::Ui,
        contents: impl FnOnce(&mut ComboUi<'_>) -> R,
    ) -> egui::InnerResponse<Option<R>> {
        let id = ui.make_persistent_id(self.id);
        let popup_id = id.with("popup");
        let state_id = id.with("state");
        let mut state = ui
            .ctx()
            .data_mut(|d| d.get_temp::<State>(state_id))
            .unwrap_or_default();
        let enabled =
            ui.is_enabled() && ui.memory(|memory| memory.is_above_modal_layer(ui.layer_id()));
        let width = self
            .width
            .unwrap_or(ui.spacing().combo_width)
            .min(ui.available_width())
            .max(30.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, HEIGHT), Sense::hover());
        let hit = if self.arrow_only {
            Rect::from_min_max(Pos2::new(rect.right() - 21.5, rect.top()), rect.max)
        } else {
            rect
        };
        let mut response = ui.interact(hit, id, Sense::click());
        response.rect = rect;
        response.widget_info(|| {
            let mut info = egui::WidgetInfo::new(egui::WidgetType::ComboBox);
            info.enabled = enabled;
            info.current_text_value = Some(self.text.text().to_owned());
            info
        });
        let mut open = egui::Popup::is_id_open(ui.ctx(), popup_id) && enabled;
        let mut activate = None;
        let mut scroll_to = None;
        let mut tab = false;
        let mut keyboard_handled = false;
        // A native click can arrive as a release-only activation after the OS
        // changes first responder. Keep the release fallback and remember only
        // presses this selector actually owns, so a press is never toggled twice.
        let pressed = enabled
            && response.is_pointer_button_down_on()
            && ui.input(|i| i.pointer.primary_pressed());
        if pressed {
            state.opened_on_pointer_press = true;
            response.request_focus();
            open = !open;
        }
        if enabled && (response.has_focus() || open) {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..Default::default()
                    },
                )
            });
            let owns_focus = response.has_focus();
            let keys = ui.input_mut(|input| {
                let mut keys = Vec::new();
                for key in [
                    Key::ArrowUp,
                    Key::ArrowDown,
                    Key::ArrowLeft,
                    Key::ArrowRight,
                    Key::Home,
                    Key::End,
                    Key::PageUp,
                    Key::PageDown,
                    Key::Enter,
                    Key::Space,
                    Key::Escape,
                    Key::F4,
                ] {
                    if open
                        && state.items.is_empty()
                        && !owns_focus
                        && matches!(
                            key,
                            Key::Enter
                                | Key::Space
                                | Key::ArrowUp
                                | Key::ArrowDown
                                | Key::Home
                                | Key::End
                        )
                    {
                        continue;
                    }
                    if input.consume_key(input.modifiers, key) {
                        keys.push(key);
                    }
                }
                keys
            });
            let alt = ui.input(|i| i.modifiers.alt);
            for key in keys {
                keyboard_handled = true;
                if key == Key::F4
                    || (alt && matches!(key, Key::ArrowUp | Key::ArrowDown))
                    || (!open && matches!(key, Key::Enter | Key::Space))
                {
                    if open {
                        activate = state.highlight;
                    }
                    open = !open;
                } else if open && matches!(key, Key::Enter | Key::Tab) {
                    activate = state.highlight;
                    tab = key == Key::Tab;
                    open = false;
                } else if key == Key::Escape && open {
                    open = false;
                    response.request_focus();
                } else if matches!(
                    key,
                    Key::ArrowUp
                        | Key::ArrowDown
                        | Key::ArrowLeft
                        | Key::ArrowRight
                        | Key::Home
                        | Key::End
                        | Key::PageUp
                        | Key::PageDown
                ) {
                    let base = if open {
                        state.highlight
                    } else {
                        state.items.iter().position(|item| item.selected)
                    };
                    let next = navigate(&state.items, base, key);
                    state.highlight = next;
                    scroll_to = next;
                    if !open {
                        activate = next;
                    }
                }
            }
            let typed: String = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|event| {
                        if let egui::Event::Text(text) = event {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect()
            });
            if !typed.is_empty()
                && !ui.input(|i| i.modifiers.command || i.modifiers.ctrl || i.modifiers.alt)
            {
                let now = ui.input(|i| i.time);
                if now - state.search_at > 1.0 {
                    state.search.clear();
                }
                state.search.push_str(&typed.to_lowercase());
                state.search_at = now;
                let found = state.items.iter().position(|item| {
                    item.enabled && item.text.to_lowercase().starts_with(&state.search)
                });
                if let Some(index) = found {
                    state.highlight = Some(index);
                    scroll_to = Some(index);
                    if !open {
                        activate = Some(index);
                    }
                }
            }
        }
        if enabled
            && open
            && !state.items.is_empty()
            && ui.input(|input| input.key_pressed(Key::Tab))
        {
            activate = state.highlight;
            open = false;
            tab = true;
        }
        if enabled && response.clicked() && !keyboard_handled && !state.opened_on_pointer_press {
            open = !open;
            response.request_focus();
        }
        if ui.input(|input| input.pointer.primary_released()) {
            state.opened_on_pointer_press = false;
        }
        if open && !state.was_open {
            state.highlight = state
                .items
                .iter()
                .position(|item| item.selected && item.enabled)
                .or_else(|| state.items.iter().position(|item| item.enabled));
            scroll_to = state.highlight;
            egui::Popup::open_id(ui.ctx(), popup_id);
        } else if !open {
            egui::Popup::close_id(ui.ctx(), popup_id);
        }
        let palette = theme::palette(ui.ctx());
        let mut painter = ui.painter().clone();
        if !ui.is_enabled() && ui.visuals().disabled_alpha() > 0.0 {
            // Disabled colors below already implement the source state. Undo
            // Egui's inherited disabled alpha to avoid applying it twice.
            painter.set_opacity(painter.opacity() / ui.visuals().disabled_alpha());
        }
        let (border, fill, duration) = if !enabled {
            (Color32::from_gray(204), Color32::from_gray(235), 0.2)
        } else if open || self.editing || response.is_pointer_button_down_on() {
            (palette.accent, palette.light, 0.01)
        } else if ui.rect_contains_pointer(rect) {
            (palette.border, palette.light, 0.1)
        } else {
            (palette.control_border, Color32::from_white_alpha(85), 0.1)
        };
        let border = animated_color(ui.ctx(), id.with("border"), border, duration);
        let fill = animated_color(ui.ctx(), id.with("fill"), fill, duration);
        painter.rect(
            rect,
            3,
            fill,
            egui::Stroke::new(1.0_f32, border),
            egui::StrokeKind::Inside,
        );
        let galley = self.text.into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            (width - 30.0).max(0.0),
            FontId::proportional(13.0),
        );
        painter.galley_with_override_text_color(
            Pos2::new(rect.left() + 8.5, rect.center().y - galley.size().y / 2.0),
            galley,
            if enabled {
                palette.text
            } else {
                palette.text.gamma_multiply(0.4)
            },
        );
        let phase =
            ui.ctx()
                .animate_bool_with_time_and_easing(id.with("chevron"), open, 0.2, |t| {
                    (1.0 - (1.0 - t).powi(2)).sqrt()
                });
        let rotation = egui::emath::Rot2::from_angle(phase * std::f32::consts::PI);
        let pivot = Pos2::new(rect.right() - 12.0, rect.center().y);
        let points = [
            Vec2::new(-3.5, -1.75),
            Vec2::new(0.0, 1.75),
            Vec2::new(3.5, -1.75),
        ]
        .map(|p| pivot + rotation * p);
        painter.add(egui::Shape::line(
            points.to_vec(),
            egui::Stroke::new(1.5_f32, border),
        ));
        let mut contents = Some(contents);
        let mut changed = false;
        let mut next_items = Vec::new();
        let mut committed = None;
        let inner = if open {
            egui::Popup::from_response(&response)
                // Editable combos only hit-test the arrow, but their popup is
                // aligned to the complete field, including on transformed layers.
                .anchor(
                    ui.ctx()
                        .layer_transform_to_global(ui.layer_id())
                        .map_or(rect, |transform| transform * rect),
                )
                .id(popup_id)
                .open_memory(None)
                .kind(egui::PopupKind::Menu)
                .gap(-1.5)
                .width(width)
                .close_behavior(egui::PopupCloseBehavior::IgnoreClicks)
                .layout(egui::Layout::top_down_justified(egui::Align::Min))
                .frame(
                    egui::Frame::new()
                        .fill(Color32::WHITE)
                        .corner_radius(3)
                        .stroke(egui::Stroke::new(1.0_f32, palette.accent)),
                )
                .show(|ui| {
                    popup_style(ui, width - 2.0);
                    egui::ScrollArea::vertical()
                        .id_salt(id.with("scroll"))
                        .max_height(MAX_POPUP - 2.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.set_min_height(24.0);
                            let mut rows = ComboUi {
                                ui,
                                id,
                                hidden: false,
                                restore_focus: !tab,
                                activate,
                                highlight: &mut state.highlight,
                                scroll_to,
                                items: &mut next_items,
                                changed: &mut changed,
                                committed: &mut committed,
                            };
                            contents.take().expect("one popup pass")(&mut rows)
                        })
                        .inner
                })
                .map(|result| {
                    // The source opens on press, so releasing on the anchor must
                    // not be mistaken for an outside click that closes it again.
                    let outside = ui.input(|input| {
                        input.pointer.any_click()
                            && input
                                .pointer
                                .interact_pos()
                                .is_some_and(|pos| !rect.contains(pos))
                    });
                    if outside && result.response.clicked_elsewhere() {
                        egui::Popup::close_id(ui.ctx(), popup_id);
                    }
                    result.inner
                })
        } else {
            None
        };
        if let Some(contents) = contents {
            // Collect rows without paint or pointer interaction. This lets a focused,
            // closed WPF-style selector handle arrows/type-ahead without opening it.
            let mut hidden = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(id.with("catalog"))
                    .max_rect(Rect::from_min_size(
                        rect.min,
                        Vec2::new(width - 2.0, 10000.0),
                    ))
                    .layout(egui::Layout::top_down_justified(egui::Align::Min)),
            );
            hidden.set_invisible();
            hidden
                .ctx()
                .accesskit_node_builder(hidden.unique_id(), |node| node.set_hidden());
            popup_style(&mut hidden, width - 2.0);
            let mut rows = ComboUi {
                ui: &mut hidden,
                id,
                hidden: true,
                restore_focus: !tab,
                activate: enabled.then_some(activate).flatten(),
                highlight: &mut state.highlight,
                scroll_to: None,
                items: &mut next_items,
                changed: &mut changed,
                committed: &mut committed,
            };
            contents(&mut rows);
        }
        if changed {
            response.mark_changed();
            ui.ctx().request_repaint();
        }
        if let Some(index) = committed {
            for (position, item) in next_items.iter_mut().enumerate() {
                item.selected = position == index;
            }
        }

        state.items = next_items;
        state.was_open = egui::Popup::is_id_open(ui.ctx(), popup_id);
        ui.ctx().data_mut(|d| d.insert_temp(state_id, state));
        egui::InnerResponse { inner, response }
    }
}

fn popup_style(ui: &mut egui::Ui, width: f32) {
    ui.set_width(width.max(1.0));
    ui.set_max_width(width.max(1.0));
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
    ui.style_mut().override_font_id = Some(FontId::proportional(13.0));
    // ScrollViewer clips to its viewport. Egui's default 3-DIP expansion lets
    // a partially scrolled row draw through the popup border and its anchor.
    ui.visuals_mut().clip_rect_margin = 0.0;
    ui.spacing_mut().item_spacing = Vec2::ZERO;
    ui.spacing_mut().button_padding = Vec2::new(6.0, 4.0);
    ui.spacing_mut().interact_size.y = ROW;
    let scroll = &mut ui.spacing_mut().scroll;
    scroll.floating = false;
    scroll.bar_width = 4.0;
    scroll.bar_inner_margin = 4.0;
    scroll.bar_outer_margin = 3.0;
    let palette = theme::palette(ui.ctx());
    ui.visuals_mut().selection.bg_fill = palette.pale;
    ui.visuals_mut().selection.stroke = egui::Stroke::new(0.0_f32, palette.text);
    {
        let visuals = &mut ui.visuals_mut().widgets.inactive;
        visuals.weak_bg_fill = Color32::TRANSPARENT;
        visuals.bg_stroke = egui::Stroke::NONE;
        visuals.corner_radius = egui::CornerRadius::ZERO;
    }
}

/// Source IsEditable keeps a genuine text editor and restricts the toggle hit
/// region to the trailing arrow; choosing a preset never disables free input.
pub fn editable_combo(
    ui: &mut egui::Ui,
    rect: Rect,
    id: impl std::hash::Hash,
    value: &mut String,
    choices: &[&str],
    hint: &str,
) -> Response {
    let id = Id::new(id);
    let edit_id = ui.make_persistent_id((id, "editor"));
    let editing = ui.memory(|memory| memory.has_focus(edit_id));
    let mut chosen = value.clone();
    let response = ui
        .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            let mut combo = PclComboBox::from_id_salt(id).width(rect.width());
            combo.arrow_only = true;
            combo.editing = editing;
            let toggle = ui.input(|input| {
                input.key_pressed(Key::F4)
                    || (input.modifiers.alt
                        && (input.key_pressed(Key::ArrowDown) || input.key_pressed(Key::ArrowUp)))
            });
            if editing && toggle {
                let combo_id = ui.make_persistent_id(combo.id);
                ui.memory_mut(|memory| memory.request_focus(combo_id));
            }
            combo
                .show_ui(ui, |ui| {
                    for choice in choices {
                        ui.selectable_value(
                            &mut chosen,
                            (*choice).to_owned(),
                            if choice.is_empty() { hint } else { choice },
                        );
                    }
                })
                .response
        })
        .inner;
    if response.has_focus() && !egui::Popup::is_id_open(ui.ctx(), response.id.with("popup")) {
        ui.memory_mut(|memory| memory.request_focus(edit_id));
    }
    let changed = chosen != *value;
    if changed {
        *value = chosen;
    }
    let mut text = ui.place(
        Rect::from_min_max(
            rect.min + Vec2::new(1.5, 0.0),
            Pos2::new(rect.right() - 21.5, rect.bottom()),
        ),
        egui::TextEdit::singleline(value)
            .id(edit_id)
            .frame(false)
            .margin(Vec2::new(4.5, 5.0))
            .font(FontId::proportional(13.0))
            .hint_text(hint),
    );
    if changed {
        text.mark_changed();
    }
    text | response
}
fn navigate(items: &[Item], current: Option<usize>, key: Key) -> Option<usize> {
    let enabled: Vec<_> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| item.enabled.then_some(index))
        .collect();
    if enabled.is_empty() {
        return None;
    }
    let current =
        current.and_then(|index| enabled.iter().position(|candidate| *candidate == index));
    let next = match key {
        Key::Home => 0,
        Key::End => enabled.len() - 1,
        Key::ArrowUp | Key::ArrowLeft => current.unwrap_or(enabled.len()).saturating_sub(1),
        Key::PageUp => current.unwrap_or(enabled.len()).saturating_sub(12),
        Key::PageDown => current.map_or(0, |index| (index + 12).min(enabled.len() - 1)),
        _ => current.map_or(0, |index| (index + 1).min(enabled.len() - 1)),
    };
    Some(enabled[next])
}

/// Existing custom popup contents (Java actions) still receive the underlying Ui.
/// Standard selection rows use the original full-width item renderer below.
pub struct ComboUi<'a> {
    ui: &'a mut egui::Ui,
    id: Id,
    hidden: bool,
    restore_focus: bool,
    activate: Option<usize>,
    highlight: &'a mut Option<usize>,
    scroll_to: Option<usize>,
    items: &'a mut Vec<Item>,
    changed: &'a mut bool,
    committed: &'a mut Option<usize>,
}
impl std::ops::Deref for ComboUi<'_> {
    type Target = egui::Ui;
    fn deref(&self) -> &Self::Target {
        self.ui
    }
}
impl std::ops::DerefMut for ComboUi<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ui
    }
}
impl ComboUi<'_> {
    pub fn selectable_label(
        &mut self,
        selected: bool,
        text: impl Into<egui::WidgetText>,
    ) -> Response {
        self.selectable_label_enabled(true, selected, text)
    }
    pub fn selectable_label_enabled(
        &mut self,
        enabled: bool,
        selected: bool,
        text: impl Into<egui::WidgetText>,
    ) -> Response {
        self.selectable_row(enabled, selected, text, false).0
    }
    /// Account history keeps the delete action separate from selection and focus.
    pub fn selectable_label_with_remove(
        &mut self,
        selected: bool,
        text: impl Into<egui::WidgetText>,
    ) -> (Response, bool) {
        self.selectable_row(true, selected, text, true)
    }
    fn selectable_row(
        &mut self,
        enabled: bool,
        selected: bool,
        text: impl Into<egui::WidgetText>,
        remove: bool,
    ) -> (Response, bool) {
        let text = text.into();
        let index = self.items.len();
        self.items.push(Item {
            text: text.text().into(),
            enabled,
            selected,
        });
        let id = self.id.with(("item", index));
        let (rect, _) = self
            .ui
            .allocate_exact_size(Vec2::new(self.ui.available_width(), ROW), Sense::hover());
        let hit = if remove {
            Rect::from_min_max(rect.min, rect.max - Vec2::new(26.0, 0.0))
        } else {
            rect
        };
        let mut response = self.ui.interact(
            hit,
            id,
            if enabled && !self.hidden {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                enabled && !self.hidden,
                selected,
                text.text(),
            )
        });
        if !self.hidden
            && enabled
            && response.hovered()
            && self.ui.input(|i| i.pointer.delta() != Vec2::ZERO)
        {
            *self.highlight = Some(index);
        }
        if self.scroll_to == Some(index) && !self.hidden {
            response.scroll_to_me(Some(egui::Align::Center));
        }
        let activated = enabled && self.activate == Some(index);
        if activated {
            response
                .flags
                .insert(egui::response::Flags::FAKE_PRIMARY_CLICKED);
        }
        if enabled && response.clicked() {
            *self.changed = true;
            *self.committed = Some(index);
            if self.restore_focus {
                self.ui.memory_mut(|memory| memory.request_focus(self.id));
            }
            egui::Popup::close_id(self.ui.ctx(), self.id.with("popup"));
        }
        if !self.hidden {
            let palette = theme::palette(self.ui.ctx());
            let highlighted = enabled && (response.hovered() || *self.highlight == Some(index));
            let color = if selected {
                palette.pale
            } else if highlighted {
                palette.lightest
            } else {
                Color32::TRANSPARENT
            };
            let color = animated_color(
                self.ui.ctx(),
                id.with("background"),
                color,
                if selected || highlighted { 0.1 } else { 0.3 },
            );
            self.ui.painter().rect_filled(rect, 0, color);
            let galley = text.into_galley(
                self.ui,
                Some(egui::TextWrapMode::Truncate),
                (rect.width() - if remove { 38.0 } else { 12.0 }).max(0.0),
                FontId::proportional(13.0),
            );
            let color = if enabled {
                palette.text
            } else {
                palette.text.gamma_multiply(0.4)
            };
            self.ui.painter().galley_with_override_text_color(
                Pos2::new(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0),
                galley,
                color,
            );
        }
        let mut removed = false;
        if remove && !self.hidden {
            let action = Rect::from_min_max(Pos2::new(rect.right() - 26.0, rect.top()), rect.max);
            let response = self.ui.interact(action, id.with("remove"), Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, "移除此设备保存的账号")
            });
            self.ui.painter().text(
                action.center(),
                egui::Align2::CENTER_CENTER,
                "×",
                FontId::proportional(16.0),
                if response.hovered() {
                    theme::palette(self.ui.ctx()).accent
                } else {
                    theme::palette(self.ui.ctx()).text
                },
            );
            removed = enabled && response.clicked();
            response.on_hover_text("移除此设备保存的账号");
            if removed {
                egui::Popup::close_id(self.ui.ctx(), self.id.with("popup"));
            }
        }
        (response, removed)
    }
    pub fn selectable_value<Value: PartialEq>(
        &mut self,
        current: &mut Value,
        value: Value,
        text: impl Into<egui::WidgetText>,
    ) -> Response {
        let mut response = self.selectable_label(*current == value, text);
        if response.clicked() && *current != value {
            *current = value;
            response.mark_changed();
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        ctx: egui::Context,
        selected: usize,
        time: f64,
        rect: Rect,
        id: Id,
        before: Id,
        after: Id,
    }
    impl Fixture {
        fn new() -> Self {
            Self {
                ctx: Default::default(),
                selected: 0,
                time: 0.0,
                rect: Rect::NOTHING,
                id: Id::NULL,
                before: Id::NULL,
                after: Id::NULL,
            }
        }
        fn frame(
            &mut self,
            events: Vec<egui::Event>,
            enabled: bool,
            count: usize,
        ) -> egui::FullOutput {
            self.time += 0.1;
            let modifiers = events
                .iter()
                .find_map(|event| {
                    if let egui::Event::Key { modifiers, .. } = event {
                        Some(*modifiers)
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            let input = egui::RawInput {
                modifiers,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 500.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    self.before = ui.button("Before").id;
                    ui.add_enabled_ui(enabled, |ui| {
                        let response = PclComboBox::from_id_salt("fixture")
                            .width(210.0)
                            .selected_text(format!("Item {}", self.selected))
                            .show_ui(ui, |ui| {
                                for index in 0..count {
                                    let response = ui.selectable_label_enabled(
                                        index != 1,
                                        self.selected == index,
                                        format!("Item {index} {}", "long path ".repeat(20)),
                                    );
                                    if response.clicked() {
                                        self.selected = index;
                                    }
                                }
                            })
                            .response;
                        self.rect = response.rect;
                        self.id = response.id;
                    });
                    self.after = ui.button("After").id;
                });
            })
        }
        fn key(&mut self, key: Key) {
            self.frame(
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                true,
                30,
            );
        }
        fn pointer(&mut self, pos: Pos2, pressed: bool) {
            self.frame(
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                true,
                30,
            );
        }
        fn open(&self) -> bool {
            egui::Popup::is_id_open(&self.ctx, self.id.with("popup"))
        }
    }
    #[test]
    fn mouse_press_opens_release_keeps_open_and_outside_click_closes() {
        let mut f = Fixture::new();
        f.frame(vec![], true, 30);
        let pos = f.rect.center();
        f.pointer(pos, true);
        assert!(f.open());
        f.pointer(pos, false);
        assert!(
            f.open(),
            "release on the anchor must not close the press-open popup"
        );
        f.pointer(Pos2::new(550.0, 400.0), true);
        f.pointer(Pos2::new(550.0, 400.0), false);
        assert!(!f.open());
        assert_eq!(f.selected, 0);
    }
    #[test]
    fn keyboard_navigation_skips_disabled_and_escape_does_not_commit() {
        let mut f = Fixture::new();
        f.frame(vec![], true, 30);
        f.ctx.memory_mut(|m| m.request_focus(f.id));
        f.frame(vec![], true, 30);
        f.key(Key::ArrowDown);
        assert_eq!(f.selected, 2);
        assert!(!f.open());
        f.key(Key::F4);
        assert!(f.open());
        f.key(Key::ArrowDown);
        assert_eq!(f.selected, 2);
        f.key(Key::Escape);
        assert!(!f.open());
        assert_eq!(f.selected, 2);
        f.key(Key::F4);
        assert!(
            f.open(),
            "Escape must return keyboard focus to the closed combo"
        );
        f.key(Key::End);
        assert_eq!(f.selected, 2);
        f.key(Key::Enter);
        assert_eq!(f.selected, 29);
        assert!(!f.open());
        f.key(Key::Home);
        assert_eq!(f.selected, 0);
    }
    #[test]
    fn popup_is_equal_width_border_only_and_scroll_height_is_bounded() {
        let mut f = Fixture::new();
        f.frame(vec![], true, 30);
        f.pointer(f.rect.center(), true);
        f.pointer(f.rect.center(), false);
        let output = f.frame(vec![], true, 30);
        let popup = f.ctx.read_response(f.id.with("popup")).unwrap().rect;
        assert!((popup.width() - f.rect.width()).abs() <= 1.0, "{popup:?}");
        assert!(popup.height() <= MAX_POPUP + 1.0, "{popup:?}");
        assert!((popup.top() - (f.rect.bottom() - 1.5)).abs() <= 1.0);
        let palette = theme::palette(&f.ctx);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == palette.pale && rect.stroke.width == 0.0)));
        f.frame(vec![], false, 30);
        assert!(!f.open());
        f.pointer(f.rect.center(), true); // next enabled frame opens only from an explicit click
    }
    #[test]
    fn disabled_text_uses_actual_override_and_scrolled_rows_stay_inside_border() {
        let mut f = Fixture::new();
        f.frame(vec![], true, 30);
        f.pointer(f.rect.center(), true);
        f.pointer(f.rect.center(), false);
        let output = f.frame(vec![], true, 30);
        let disabled = theme::palette(&f.ctx).text.gamma_multiply(0.4);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.job.text.starts_with("Item 1 ")
                && text.override_text_color == Some(disabled))));
        f.key(Key::End);
        for _ in 0..5 {
            f.frame(vec![], true, 30);
        }
        let output = f.frame(vec![], true, 30);
        let popup = f.ctx.read_response(f.id.with("popup")).unwrap().rect;
        let mut count = 0;
        for shape in &output.shapes {
            if let egui::Shape::Text(text) = &shape.shape {
                if text.galley.job.text.contains("long path") {
                    count += 1;
                    assert!(shape.clip_rect.top() >= popup.top() + 0.99);
                    assert!(shape.clip_rect.bottom() <= popup.bottom() - 0.99);
                }
            }
        }
        assert!(count > 0);
        let output = f.frame(vec![], false, 30);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.job.text == "Item 0"
                && text.override_text_color == Some(disabled))));
    }
    #[test]
    fn closed_collection_does_not_run_actions_and_typeahead_is_user_driven() {
        let mut f = Fixture::new();
        for _ in 0..3 {
            f.frame(vec![], true, 30);
        }
        assert_eq!(f.selected, 0);
        assert!(!f.open());
        f.ctx.memory_mut(|m| m.request_focus(f.id));
        f.frame(vec![egui::Event::Text("Item 2".into())], true, 30);
        assert_eq!(f.selected, 2);
        assert!(!f.open());
        f.frame(vec![], false, 30);
        f.frame(
            vec![egui::Event::Key {
                key: Key::End,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            false,
            30,
        );
        assert_eq!(f.selected, 2);
    }
    #[test]
    fn accessibility_click_opens_and_closed_catalog_is_hidden_from_the_tree() {
        let mut f = Fixture::new();
        f.ctx.enable_accesskit();
        let output = f.frame(vec![], true, 30);
        let tree = output.platform_output.accesskit_update.unwrap();
        let row = egui::accesskit::NodeId(f.id.with(("item", 0_usize)).value());
        assert!(
            tree.nodes
                .iter()
                .any(|(_, node)| node.is_hidden() && node.children().contains(&row)),
            "closed catalog must be under an AX-hidden container"
        );
        f.frame(
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Click,
                    target: egui::accesskit::NodeId(f.id.value()),
                    data: None,
                },
            )],
            true,
            30,
        );
        assert!(f.open());
    }
    #[test]
    fn custom_java_style_button_retains_enter_action() {
        let ctx = egui::Context::default();
        let mut action_count = 0;
        let mut combo_id = Id::NULL;
        let mut button_id = Id::NULL;
        let mut time = 0.0;
        let mut draw = |events, focus: Option<Id>| {
            if let Some(id) = focus {
                ctx.memory_mut(|memory| memory.request_focus(id));
            }
            time += 0.1;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 500.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    combo_id = PclComboBox::from_id_salt("custom-java")
                        .width(250.0)
                        .selected_text("Java actions")
                        .show_ui(ui, |ui| {
                            let response = ui.button("Import Java");
                            button_id = response.id;
                            if response.clicked() {
                                action_count += 1;
                                ui.close();
                            }
                        })
                        .response
                        .id;
                });
            });
            (combo_id, button_id, action_count)
        };
        let (combo, _, count) = draw(vec![], None);
        assert_eq!(count, 0);
        let key = |key| {
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]
        };
        let (_, button, _) = draw(key(Key::F4), Some(combo));
        let (_, _, count) = draw(key(Key::Enter), Some(button));
        assert_eq!(
            count, 1,
            "combo must not swallow a custom popup button's Enter"
        );
    }
    #[test]
    fn editable_version_keeps_typed_values_and_f4_escape_returns_to_editor() {
        let ctx = egui::Context::default();
        let mut value = String::new();
        let mut time = 0.0;
        let mut draw = |events: Vec<egui::Event>, focus: Option<Id>| {
            if let Some(id) = focus {
                ctx.memory_mut(|m| m.request_focus(id));
            }
            time += 0.1;
            let mut editor = Id::NULL;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 500.0))),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::new(250.0, 28.0), Sense::hover());
                        editor = editable_combo(
                            ui,
                            rect,
                            "versions",
                            &mut value,
                            &["", "1.21.1", "1.20.1"],
                            "All",
                        )
                        .id;
                    });
                },
            );
            (editor, value.clone())
        };
        let (editor, _) = draw(vec![], None);
        let (_, value) = draw(
            vec![egui::Event::Text("custom-version".into())],
            Some(editor),
        );
        assert_eq!(value, "custom-version");
        let key = |key| {
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]
        };
        draw(key(Key::F4), None);
        assert!(egui::Popup::is_any_open(&ctx));
        let (_, value) = draw(key(Key::Escape), None);
        assert!(!egui::Popup::is_any_open(&ctx));
        assert_eq!(value, "custom-version");
        assert!(ctx.memory(|memory| memory.has_focus(editor)));
    }
    #[test]
    fn editable_popup_anchors_to_full_field_not_trailing_arrow() {
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(Pos2::new(48.0, 120.0), Vec2::new(260.0, 28.0));
        let mut value = String::from("Player");
        let mut toggle_id = Id::NULL;
        let mut draw = |events| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                            let mut combo =
                                PclComboBox::from_id_salt("editable-anchor").width(rect.width());
                            combo.arrow_only = true;
                            toggle_id = combo
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut value, "Player".into(), "Player");
                                })
                                .response
                                .id;
                        });
                    });
                },
            );
            toggle_id
        };
        draw(vec![]);
        let pos = Pos2::new(rect.right() - 10.0, rect.center().y);
        let id = draw(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        draw(vec![]);
        let popup = ctx
            .read_response(id.with("popup"))
            .expect("opened popup")
            .rect;
        assert!((popup.left() - rect.left()).abs() < 1.0, "{popup:?}");
        assert!((popup.right() - rect.right()).abs() < 1.0, "{popup:?}");
        assert!(
            (popup.top() - (rect.bottom() - 1.5)).abs() < 1.0,
            "{popup:?}"
        );
    }
    #[test]
    fn higher_modal_keeps_its_enter_key_and_closes_the_background_popup() {
        let mut f = Fixture::new();
        f.frame(vec![], true, 30);
        f.pointer(f.rect.center(), true);
        f.pointer(f.rect.center(), false);
        f.key(Key::ArrowDown);
        let layer = egui::LayerId::new(egui::Order::Foreground, Id::new("test-modal"));
        let _ = f.ctx.run(egui::RawInput::default(), |ctx| {
            ctx.memory_mut(|memory| memory.set_modal_layer(layer))
        });
        f.key(Key::Enter);
        assert_eq!(f.selected, 0);
        assert!(!f.open());
        assert!(
            f.ctx.input(|input| input.key_pressed(Key::Enter)),
            "background combo must not consume modal input"
        );
    }
    #[test]
    fn tab_commits_and_preserves_forward_and_reverse_focus_navigation() {
        for shift in [false, true] {
            let mut f = Fixture::new();
            f.frame(vec![], true, 30);
            f.ctx.memory_mut(|memory| memory.request_focus(f.id));
            f.key(Key::F4);
            f.key(Key::ArrowDown);
            f.frame(
                vec![egui::Event::Key {
                    key: Key::Tab,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers {
                        shift,
                        ..Default::default()
                    },
                }],
                true,
                30,
            );
            assert!(!f.open());
            assert_eq!(f.selected, 2);
            // Egui schedules reverse traversal for the next frame so that the
            // earlier widget receives gained_focus when it is rendered again.
            f.frame(vec![], true, 30);
            let target = if shift { f.before } else { f.after };
            assert!(
                f.ctx.memory(|memory| memory.has_focus(target)),
                "Tab direction must remain with egui, shift={shift}, focused={:?}, before={:?}, combo={:?}, after={:?}",
                f.ctx.memory(|m| m.focused()),
                f.before,
                f.id,
                f.after
            );
        }
    }
    #[test]
    fn positioned_form_keeps_text_and_combo_mouse_targets_after_idle_frames() {
        let ctx = egui::Context::default();
        let mut text = String::new();
        let mut selected = 0;
        let mut anchor = Rect::NOTHING;
        let mut combo_id = Id::NULL;
        let mut edit_id = Id::NULL;
        let mut render = |events: Vec<egui::Event>| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let start = ui.cursor().min;
                        ui.allocate_space(Vec2::new(700.0, 65.0));
                        edit_id = ui
                            .place(
                                Rect::from_min_size(start, Vec2::new(400.0, 28.0)),
                                egui::TextEdit::singleline(&mut text),
                            )
                            .id;
                        for (index, y) in [0.0, 37.0].into_iter().enumerate() {
                            let rect = Rect::from_min_size(
                                start + Vec2::new(450.0, y),
                                Vec2::new(200.0, 28.0),
                            );
                            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                                let response = PclComboBox::from_id_salt(("positioned", index))
                                    .width(188.0)
                                    .selected_text("Choice")
                                    .show_ui(ui, |ui| {
                                        for value in 0..30 {
                                            ui.selectable_value(
                                                &mut selected,
                                                value,
                                                format!("Choice {value}"),
                                            );
                                        }
                                    })
                                    .response;
                                if index == 1 {
                                    anchor = response.rect;
                                    combo_id = response.id;
                                }
                            });
                        }
                        let _ = ui.button("Search");
                    });
                },
            );
            (anchor, combo_id, edit_id)
        };
        for _ in 0..4 {
            render(vec![]);
        }
        let (rect, id, _) = render(vec![]);
        let pos = rect.center();
        render(vec![egui::Event::PointerMoved(pos)]);
        render(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(
            egui::Popup::is_id_open(&ctx, id.with("popup")),
            "positioned selector must open on a real pointer press"
        );
    }
}
