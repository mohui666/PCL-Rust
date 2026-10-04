use super::{Launcher, MUTED};
use crate::theme;
use crate::ui_style;
use eframe::egui::{self, Color32, Rect, RichText, Vec2};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    Release,
    Preview,
    Ancient,
    April,
}
impl Category {
    fn title(self) -> &'static str {
        match self {
            Self::Release => "正式版",
            Self::Preview => "预览版",
            Self::Ancient => "远古版",
            Self::April => "愚人节版",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Self::Release => "block-grass",
            Self::Preview => "block-command",
            Self::Ancient => "block-cobblestone",
            Self::April => "block-gold",
        }
    }
}
fn category(value: &Value) -> Category {
    let id = value["id"].as_str().unwrap_or("").to_ascii_lowercase();
    let kind = value["type"].as_str().unwrap_or("");
    if kind == "special"
        || (kind == "snapshot"
            && (matches!(
                id.as_str(),
                "20w14infinite"
                    | "20w14∞"
                    | "3d shareware v1.34"
                    | "1.rv-pre1"
                    | "15w14a"
                    | "2.0"
                    | "22w13oneblockatatime"
                    | "23w13a_or_b"
                    | "24w14potato"
                    | "25w14craftmine"
                    | "26w14a"
            ) || value["releaseTime"].as_str().is_some_and(april_first)))
    {
        return Category::April;
    }
    match kind {
        "release" => Category::Release,
        "snapshot"
            if id.starts_with("1.")
                && !["combat", "rc", "experimental", "pre"]
                    .iter()
                    .any(|part| id.contains(part)) =>
        {
            Category::Release
        }
        "snapshot" => Category::Preview,
        _ => Category::Ancient,
    }
}
fn april_first(timestamp: &str) -> bool {
    // Upstream classifies the announcement date in UTC+2, including late March 31 UTC.
    let Ok(time) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
        return false;
    };
    use chrono::Datelike;
    let date = time.with_timezone(&chrono::FixedOffset::east_opt(2 * 3600).unwrap());
    date.month() == 4 && date.day() == 1
}
fn release_time(value: &Value) -> String {
    value["releaseTime"]
        .as_str()
        .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y/%m/%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "时间未知".into())
}
fn frame() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 245))
        .corner_radius(5)
        .shadow(egui::epaint::Shadow {
            offset: [0, 2],
            blur: 3,
            spread: 0,
            color: Color32::from_black_alpha(9),
        })
}
impl Launcher {
    pub(super) fn downloads(&mut self, ui: &mut egui::Ui) {
        if (1..=5).contains(&self.download_tab) {
            self.resource_page(ui);
            return;
        }
        if let Some(minecraft) = self.download_selection.clone() {
            self.install_selection_page(ui, minecraft);
            return;
        }
        let mut selected = None;
        let mut refresh = false;
        let mut grouped = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        let categories = [
            Category::Release,
            Category::Preview,
            Category::Ancient,
            Category::April,
        ];
        for value in &self.manifest {
            if let Some(index) = categories.iter().position(|c| *c == category(value)) {
                grouped[index].push(value);
            }
        }
        for values in &mut grouped {
            values.sort_by(|a, b| b["releaseTime"].as_str().cmp(&a["releaseTime"].as_str()));
        }
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            frame().show(ui, |ui| {
                let (rect, response) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 40.0),
                    egui::Sense::hover(),
                );
                ui_style::place_left(
                    ui,
                    Rect::from_min_size(
                        rect.min + Vec2::new(15.0, 12.0),
                        Vec2::new(rect.width() - 30.0, 18.0),
                    ),
                    egui::Label::new(ui_style::card_title("最新版本")).halign(egui::Align::Min),
                );
                response.context_menu(|ui| {
                    if ui
                        .add_enabled(self.busy.is_none(), egui::Button::new("刷新版本列表"))
                        .clicked()
                    {
                        refresh = true;
                        ui.close();
                    }
                });
                if grouped[0].is_empty() {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        ui.label(
                            RichText::new(if self.busy.is_some() {
                                "正在获取版本列表…"
                            } else {
                                "尚未获取版本列表"
                            })
                            .color(MUTED),
                        );
                        if self.busy.is_none() && ui.small_button("重新获取").clicked() {
                            refresh = true;
                        }
                    });
                } else {
                    let release = grouped[0][0];
                    if self.download_version_row(
                        ui,
                        release,
                        &format!("最新正式版，发布于 {}", release_time(release)),
                    ) {
                        selected = release["id"].as_str().map(str::to_owned);
                    }
                    if let Some(snapshot) = grouped[1].first().filter(|snapshot| {
                        snapshot["releaseTime"].as_str() > release["releaseTime"].as_str()
                    }) {
                        if self.download_version_row(
                            ui,
                            snapshot,
                            &format!("最新预览版，发布于 {}", release_time(snapshot)),
                        ) {
                            selected = snapshot["id"].as_str().map(str::to_owned);
                        }
                    }
                }
                ui.add_space(18.0);
            });
            ui.add_space(15.0);
            for (index, kind) in categories.into_iter().enumerate() {
                let id = ui.make_persistent_id(("download-group", index));
                let mut expanded = ui
                    .ctx()
                    .data_mut(|data| data.get_temp::<bool>(id).unwrap_or(false));
                frame().show(ui, |ui| {
                    let (rect, response) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 40.0),
                        egui::Sense::click(),
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, kind.title())
                    });
                    if response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            5,
                            theme::palette(ui.ctx()).light.gamma_multiply(100.0 / 255.0),
                        );
                    }
                    ui_style::place_left(
                        ui,
                        Rect::from_min_size(
                            rect.min + Vec2::new(15.0, 12.0),
                            Vec2::new(rect.width() - 45.0, 18.0),
                        ),
                        egui::Label::new(ui_style::card_title(kind.title()))
                            .halign(egui::Align::Min),
                    );
                    let center = rect.right_center() + Vec2::new(-20.0, 0.0);
                    let dy = if expanded { -3.0 } else { 3.0 };
                    ui.painter().add(egui::Shape::line(
                        vec![
                            center + Vec2::new(-4.0, -dy / 2.0),
                            center + Vec2::new(0.0, dy / 2.0),
                            center + Vec2::new(4.0, -dy / 2.0),
                        ],
                        egui::Stroke::new(1.3_f32, theme::palette(ui.ctx()).text),
                    ));
                    if response.clicked() {
                        expanded = !expanded;
                        ui.ctx().data_mut(|data| data.insert_temp(id, expanded));
                    }
                    if expanded {
                        if grouped[index].is_empty() {
                            ui.label(RichText::new("暂无版本").color(MUTED));
                        }
                        egui::ScrollArea::vertical()
                            .id_salt(("download-versions", index))
                            .max_height(300.0)
                            .show(ui, |ui| {
                                for value in &grouped[index] {
                                    if self.download_version_row(
                                        ui,
                                        value,
                                        &format!("发布于 {}", release_time(value)),
                                    ) {
                                        selected = value["id"].as_str().map(str::to_owned);
                                    }
                                }
                            });
                        ui.add_space(18.0);
                    }
                });
                ui.add_space(15.0);
            }
        });
        if let Some(id) = selected {
            ui.ctx()
                .data_mut(|data| data.insert_temp(egui::Id::new("install-page-opened"), true));
            self.install_name = id.clone();
            self.install_name_edited = false;
            self.download_selection = Some(id);
            self.loader_kind = None;
            self.loader_expanded = None;
            self.loader_versions.clear();
            self.loader_version = None;
        }
        let attempted = egui::Id::new("download-manifest-first-request");
        if self.manifest.is_empty()
            && self.busy.is_none()
            && !ui
                .ctx()
                .data_mut(|d| d.get_temp::<bool>(attempted).unwrap_or(false))
        {
            refresh = true;
        }
        if refresh && self.busy.is_none() {
            ui.ctx().data_mut(|d| d.insert_temp(attempted, true));
            self.load_manifest();
        }
    }
    fn download_version_row(&self, ui: &mut egui::Ui, value: &Value, lore: &str) -> bool {
        let (whole, response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 42.0), egui::Sense::hover());
        let rect = Rect::from_min_max(
            whole.min + Vec2::new(20.0, 0.0),
            whole.max - Vec2::new(18.0, 0.0),
        );
        let response = ui.interact(rect, response.id, egui::Sense::click());
        let id = value["id"].as_str().unwrap_or("未知版本");
        let ready = self.busy.is_none() && self.game_pid.is_none();
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ready, id));
        if response.hovered() && ready {
            ui.painter()
                .rect_filled(rect, 4, theme::palette(ui.ctx()).light);
        }
        self.assets.icon(
            ui,
            category(value).icon(),
            Rect::from_min_size(rect.min + Vec2::new(6.0, 5.0), Vec2::new(31.0, 32.0)),
            Color32::WHITE,
        );
        ui_style::place_left(
            ui,
            Rect::from_min_size(
                rect.min + Vec2::new(44.0, 4.0),
                Vec2::new(rect.width() - 54.0, 19.0),
            ),
            egui::Label::new(
                RichText::new(id)
                    .size(14.0)
                    .color(if response.hovered() && ready {
                        theme::palette(ui.ctx()).accent
                    } else {
                        theme::palette(ui.ctx()).text
                    }),
            )
            .halign(egui::Align::Min),
        );
        ui_style::place_left(
            ui,
            Rect::from_min_size(
                rect.min + Vec2::new(44.0, 23.0),
                Vec2::new(rect.width() - 54.0, 16.0),
            ),
            egui::Label::new(RichText::new(lore).size(12.0).color(MUTED)).halign(egui::Align::Min),
        );
        response.clicked() && ready
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn real_manifest_ids_are_classified_without_rewriting_download_ids() {
        for id in [
            "20w14infinite",
            "3D Shareware v1.34",
            "1.RV-Pre1",
            "25w14craftmine",
            "26w14a",
        ] {
            let entry =
                json!({"id":id,"type":"snapshot","releaseTime":"2026-04-01T08:00:00+00:00"});
            assert_eq!(category(&entry), Category::April);
            assert_eq!(entry["id"], id);
        }
        assert_eq!(
            category(&json!({"id":"1.21.1","type":"release"})),
            Category::Release
        );
        assert_eq!(
            category(&json!({"id":"26.4-snapshot-2","type":"snapshot"})),
            Category::Preview
        );
        assert_eq!(
            category(&json!({"id":"b1.7.3","type":"old_beta"})),
            Category::Ancient
        );
        assert_eq!(
            category(&json!({"id":"1.2.1","type":"snapshot"})),
            Category::Release
        );
        assert!(april_first("2026-03-31T23:30:00Z"));
        assert!(!april_first("2026-04-01T23:30:00Z"));
    }
}
