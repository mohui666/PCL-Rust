use super::install_ui::InstallKind;
use super::{loading_ui, Event, Launcher, MUTED};
use crate::theme;
use crate::ui_style;
use eframe::egui::{self, Color32, Rect, RichText, Vec2};
use pcl_core::{install, loaders::LoaderVersion, model::OperationCancelled};
use serde_json::Value;
use std::sync::atomic::Ordering;

#[derive(Default)]
pub(super) struct VersionLists {
    pub(super) manifest: VersionRequest,
    pub(super) loader: VersionRequest,
    pub(super) loader_target: Option<(String, InstallKind)>,
}
#[derive(Default)]
pub(super) struct VersionRequest {
    id: u64,
    pub(super) phase: Phase,
    pub(super) indicator: loading_ui::Indicator,
}
#[derive(Default)]
pub(super) enum Phase {
    #[default]
    Idle,
    Loading,
    Ready,
    Failed(String),
    Cancelled,
}
impl VersionRequest {
    pub(super) fn start(&mut self) -> u64 {
        self.id = self.id.wrapping_add(1);
        self.phase = Phase::Loading;
        self.indicator.start();
        self.id
    }
    fn accepts(&self, id: u64) -> bool {
        self.id == id && self.running()
    }
    pub(super) fn running(&self) -> bool {
        matches!(self.phase, Phase::Loading)
    }
    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        cancelling: bool,
        placement: loading_ui::Placement,
    ) -> Option<loading_ui::Action> {
        let status = match &self.phase {
            Phase::Loading => loading_ui::Status::Running { cancelling },
            Phase::Failed(error) => loading_ui::Status::Failed(error),
            Phase::Cancelled => loading_ui::Status::Cancelled,
            Phase::Idle | Phase::Ready => loading_ui::Status::Ready,
        };
        self.indicator
            .show_status(ui, "正在获取版本列表", status, placement)
    }
    fn finish(&mut self, failure: Option<ListFailure>) {
        self.phase = match failure {
            Some(error) if error.cancelled => Phase::Cancelled,
            Some(error) => Phase::Failed(error.message),
            None => Phase::Ready,
        };
    }
}
pub(crate) struct ListFailure {
    message: String,
    cancelled: bool,
}
impl ListFailure {
    pub(super) fn from_error(error: anyhow::Error) -> Self {
        Self {
            cancelled: error.is::<OperationCancelled>()
                || error.chain().any(|cause| cause.is::<OperationCancelled>()),
            message: format!("获取版本列表失败：{error:#}"),
        }
    }
}
pub(crate) enum VersionListEvent {
    Manifest(u64, Result<Value, ListFailure>),
    Loader(
        u64,
        String,
        InstallKind,
        Result<Vec<LoaderVersion>, ListFailure>,
    ),
}

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
    pub(super) fn version_list_request_active(&self) -> bool {
        self.version_lists.manifest.running()
            || self.version_lists.loader.running()
            || self.java_download.is_loading()
    }
    pub(super) fn load_manifest(&mut self) {
        let Some((tx, cancel)) = self.start_job("正在获取版本列表") else {
            return;
        };
        let request = self.version_lists.manifest.start();
        std::thread::spawn(move || {
            let result =
                install::fetch_manifest_with_cancel(&cancel).map_err(ListFailure::from_error);
            let _ = tx.send(Event::VersionList(VersionListEvent::Manifest(
                request, result,
            )));
        });
    }
    pub(super) fn handle_version_list_event(&mut self, event: VersionListEvent) {
        let (state, result) = match event {
            VersionListEvent::Manifest(id, result) => {
                if !self.version_lists.manifest.accepts(id) {
                    return;
                }
                let result = result.and_then(|value| {
                    let versions = value["versions"]
                        .as_array()
                        .filter(|v| !v.is_empty())
                        .ok_or_else(|| ListFailure {
                            message: "获取版本列表失败：服务未返回有效版本列表".into(),
                            cancelled: false,
                        })?;
                    self.manifest = versions.clone();
                    Ok(())
                });
                (&mut self.version_lists.manifest, result)
            }
            VersionListEvent::Loader(id, minecraft, kind, result) => {
                if !self.version_lists.loader.accepts(id) {
                    return;
                }
                let visible = self.download_selection.as_deref() == Some(&minecraft)
                    && self.loader_expanded == Some(kind);
                let result = result.map(|versions| {
                    if visible {
                        self.loader_versions = versions;
                    }
                });
                (&mut self.version_lists.loader, result)
            }
        };
        self.status = match &result {
            Ok(()) => "版本列表已更新".into(),
            Err(error) if error.cancelled => "版本列表获取已取消".into(),
            Err(error) => error.message.clone(),
        };
        state.finish(result.err());
        self.busy = None;
        self.progress = None;
    }
    pub(super) fn version_list_action(&mut self, action: loading_ui::Action) {
        if action == loading_ui::Action::Cancel {
            self.cancel.store(true, Ordering::Relaxed);
            self.status = "正在取消版本列表获取…".into();
        }
    }
    pub(super) fn downloads(&mut self, ui: &mut egui::Ui) {
        // Resource version filters share the official manifest, including when
        // a resource tab is opened before the vanilla download page.
        if self.manifest.is_empty()
            && matches!(self.version_lists.manifest.phase, Phase::Idle)
            && self.busy.is_none()
        {
            self.load_manifest();
        }
        if (1..=5).contains(&self.download_tab) {
            self.resource_page(ui);
            return;
        }
        if let Some(minecraft) = self.download_selection.clone() {
            self.install_selection_page(ui, minecraft);
            return;
        }
        if let Some(action) = self.version_lists.manifest.show(
            ui,
            self.cancel.load(Ordering::Relaxed),
            loading_ui::Placement::Detail,
        ) {
            self.version_list_action(action);
            if action == loading_ui::Action::Retry && self.busy.is_none() {
                self.load_manifest();
            }
            return;
        }
        if self.manifest.is_empty() {
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
                        ui.label(RichText::new("当前列表没有正式版").color(MUTED));
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
                if grouped[index].is_empty() {
                    continue;
                }
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
                        // PageDownloadInstall adds a StackPanel to each card;
                        // the outer page owns scrolling, including the oldest row.
                        for value in &grouped[index] {
                            if self.download_version_row(
                                ui,
                                value,
                                &format!("发布于 {}", release_time(value)),
                            ) {
                                selected = value["id"].as_str().map(str::to_owned);
                            }
                        }
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
        if refresh && self.busy.is_none() {
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
        if !ui.is_rect_visible(rect) {
            return false;
        }
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
            .halign(egui::Align::Min)
            .truncate(),
        );
        ui_style::place_left(
            ui,
            Rect::from_min_size(
                rect.min + Vec2::new(44.0, 23.0),
                Vec2::new(rect.width() - 54.0, 16.0),
            ),
            egui::Label::new(RichText::new(lore).size(12.0).color(MUTED))
                .halign(egui::Align::Min)
                .truncate(),
        );
        response.clicked() && ready
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn ui_context() -> egui::Context {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        fonts.families.insert(
            egui::FontFamily::Name("PCL Bold".into()),
            fonts.families[&egui::FontFamily::Proportional].clone(),
        );
        ctx.set_fonts(fonts);
        ctx
    }
    fn text_rect(output: &egui::FullOutput, label: &str) -> Option<Rect> {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                Some(Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        })
    }
    #[test]
    fn minecraft_groups_use_page_scroll_and_the_last_row_opens_its_own_id() {
        let root = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(root.path());
        app.manifest=(0..120).map(|i| json!({"id":format!("fixture-{i:03}"),"type":"release","releaseTime":format!("{}-01-01T00:00:00Z",2000+i)})).collect();
        app.version_lists.manifest.phase = Phase::Ready;
        let ctx = ui_context();
        let draw = |app: &mut Launcher, scroll, events| {
            let mut size = Vec2::ZERO;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(640.0, 360.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    theme::apply(ctx, &app.settings);
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let scroll = egui::ScrollArea::vertical()
                            .id_salt("manifest-test-page")
                            .max_height(260.0)
                            .vertical_scroll_offset(scroll)
                            .show(ui, |ui| app.downloads(ui));
                        size = scroll.content_size;
                    });
                },
            );
            (output, size)
        };
        let (closed, _) = draw(&mut app, 0.0, vec![]);
        for absent in ["预览版", "远古版", "愚人节版"] {
            assert!(text_rect(&closed, absent).is_none());
        }
        let header = text_rect(&closed, "正式版").unwrap();
        let click = |pos, pressed| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        for pressed in [true, false] {
            draw(&mut app, 0.0, click(header.center(), pressed));
        }
        let (top, size) = draw(&mut app, 0.0, vec![]);
        assert!(
            size.y > 5100.0,
            "outer page must own all 120 rows: {size:?}"
        );
        assert!(text_rect(&top, "fixture-000").is_none());
        let (bottom, _) = draw(&mut app, size.y - 260.0, vec![]);
        let last =
            text_rect(&bottom, "fixture-000").expect("oldest row is reachable by outer scroll");
        assert!(last.top() >= 0.0 && last.bottom() < 280.0);
        let pos = egui::pos2(500.0, last.center().y);
        for pressed in [true, false] {
            draw(&mut app, size.y - 260.0, click(pos, pressed));
        }
        assert_eq!(app.download_selection.as_deref(), Some("fixture-000"));
    }
    #[test]
    fn long_minecraft_names_and_descriptions_stay_inside_their_row() {
        let root = tempfile::tempdir().unwrap();
        let app = super::super::event_tests::fixture(root.path());
        let ctx = ui_context();
        let title = "long-special-version-".repeat(20);
        let lore = "Long release description ".repeat(20);
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(320.0, 200.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    app.download_version_row(ui, &json!({"id":title,"type":"release"}), &lore);
                });
            },
        );
        for label in [&title, &lore] {
            let rect = text_rect(&output, label).unwrap();
            assert!(
                rect.height() <= 20.0,
                "label must not wrap into the next row: {rect:?}"
            );
            assert!(rect.right() <= 320.0, "label must not escape row: {rect:?}");
        }
    }

    #[test]
    fn stale_and_duplicate_list_replies_cannot_release_the_current_request() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(temp.path());
        let old = app.version_lists.manifest.start();
        let current = app.version_lists.manifest.start();
        app.busy = Some("new list request".into());
        app.handle_version_list_event(VersionListEvent::Manifest(
            old,
            Ok(json!({"versions":[{"id":"old"}]})),
        ));
        assert!(app.manifest.is_empty());
        assert!(app.version_lists.manifest.running());
        assert_eq!(app.busy.as_deref(), Some("new list request"));
        app.handle_version_list_event(VersionListEvent::Manifest(
            current,
            Ok(json!({"versions":[{"id":"1.21.1","type":"release"}]})),
        ));
        assert_eq!(app.manifest[0]["id"], "1.21.1");
        assert!(matches!(app.version_lists.manifest.phase, Phase::Ready));
        assert!(app.busy.is_none());
        app.busy = Some("unrelated operation".into());
        app.handle_version_list_event(VersionListEvent::Manifest(
            current,
            Err(ListFailure::from_error(anyhow::anyhow!("late reply"))),
        ));
        assert_eq!(app.busy.as_deref(), Some("unrelated operation"));
        assert!(matches!(app.version_lists.manifest.phase, Phase::Ready));
    }
    #[test]
    fn cancellation_waits_for_worker_and_does_not_hide_a_real_failure_or_success() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(temp.path());
        let id = app.version_lists.manifest.start();
        app.busy = Some("list request".into());
        app.version_list_action(loading_ui::Action::Cancel);
        assert!(app.cancel.load(Ordering::Relaxed));
        assert!(app.version_lists.manifest.running());
        assert!(app.busy.is_some());
        app.handle_version_list_event(VersionListEvent::Manifest(
            id,
            Err(ListFailure::from_error(anyhow::anyhow!("HTTP 503"))),
        ));
        assert!(
            matches!(&app.version_lists.manifest.phase, Phase::Failed(message) if message.contains("503"))
        );
        assert!(app.busy.is_none());
        let retry = app.version_lists.manifest.start();
        app.handle_version_list_event(VersionListEvent::Manifest(
            retry,
            Err(ListFailure::from_error(
                anyhow::Error::new(OperationCancelled).context("request stopped"),
            )),
        ));
        assert!(matches!(app.version_lists.manifest.phase, Phase::Cancelled));
        let retry = app.version_lists.manifest.start();
        app.handle_version_list_event(VersionListEvent::Manifest(
            retry,
            Ok(json!({"versions":[{"id":"ready"}]})),
        ));
        assert!(matches!(app.version_lists.manifest.phase, Phase::Ready));
        assert_eq!(app.manifest[0]["id"], "ready");
    }
    #[test]
    fn invalid_manifest_and_loader_error_are_not_successful_empty_lists() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(temp.path());
        for data in [json!({}), json!({"versions":[]})] {
            let id = app.version_lists.manifest.start();
            app.handle_version_list_event(VersionListEvent::Manifest(id, Ok(data)));
            assert!(matches!(app.version_lists.manifest.phase, Phase::Failed(_)));
        }
        let id = app.version_lists.loader.start();
        app.handle_version_list_event(VersionListEvent::Loader(
            id,
            "1.21.1".into(),
            InstallKind::Fabric,
            Err(ListFailure::from_error(anyhow::anyhow!(
                "service unavailable"
            ))),
        ));
        assert!(matches!(app.version_lists.loader.phase, Phase::Failed(_)));
        let retry = app.version_lists.loader.start();
        assert_ne!(id, retry);
        app.handle_version_list_event(VersionListEvent::Loader(
            retry,
            "1.21.1".into(),
            InstallKind::Fabric,
            Ok(vec![]),
        ));
        assert!(matches!(app.version_lists.loader.phase, Phase::Ready));
    }
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
