//! Task selection, retry and local history. History never resumes writes without a click.
use super::{
    job::{JobContext, JobId},
    task_ui::TaskState,
    Launcher,
};
use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct TaskRecord {
    key: String,
    title: String,
    status: String,
    when: u64,
    root: PathBuf,
    target: Option<String>,
    steps: Vec<String>,
    error: Option<String>,
}
#[derive(Default)]
pub(super) struct TaskHub {
    pub selected: Option<JobId>,
    pub others: BTreeMap<JobId, TaskState>,
    contexts: BTreeMap<JobId, JobContext>,
    keys: BTreeMap<JobId, String>,
    records: Vec<TaskRecord>,
}
impl TaskHub {
    pub(super) fn has_history(&self) -> bool {
        !self.records.is_empty()
    }
}
impl Launcher {
    pub(super) fn load_task_history(&mut self) {
        let path = self.settings_path.with_file_name("task-history.json");
        let result = (|| -> anyhow::Result<Vec<TaskRecord>> {
            if !path.exists() {
                return Ok(Vec::new());
            }
            anyhow::ensure!(
                std::fs::metadata(&path)?.len() <= 4 * 1024 * 1024,
                "任务历史过大"
            );
            let mut records: Vec<TaskRecord> = serde_json::from_slice(&std::fs::read(path)?)?;
            records.truncate(200);
            for r in &mut records {
                r.title = pcl_core::crash::redact(&r.title, &[]);
                r.steps = r
                    .steps
                    .iter()
                    .map(|s| pcl_core::crash::redact(s, &[]))
                    .collect();
                r.error = r.error.as_deref().map(|s| pcl_core::crash::redact(s, &[]));
                if r.status == "运行中" || r.status == "等待中" || r.status == "正在取消"
                {
                    r.status = "已中断".into();
                }
            }
            Ok(records)
        })();
        match result {
            Ok(records) => self.task_hub.records = records,
            Err(e) => self.error = Some(format!("任务历史读取失败，文件已保留：{e:#}")),
        }
    }
    fn save_task_history(&mut self) {
        let path = self.settings_path.with_file_name("task-history.json");
        let result = (|| -> anyhow::Result<()> {
            let parent = path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("任务历史路径没有父目录"))?;
            std::fs::create_dir_all(parent)?;
            let bytes = serde_json::to_vec_pretty(&self.task_hub.records)?;
            use std::io::Write;
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            temp.write_all(&bytes)?;
            temp.as_file().sync_all()?;
            temp.persist(path).map_err(|e| e.error)?;
            Ok(())
        })();
        if let Err(e) = result {
            self.error = Some(format!("任务历史保存失败：{e:#}"));
        }
    }
    pub(super) fn register_task(&mut self, id: JobId, context: JobContext) {
        if let (Some(previous), Some(task)) = (self.task_hub.selected.take(), self.task.take()) {
            self.task_hub.others.insert(previous, task);
        }
        self.task_hub.selected = Some(id);
        while self.task_hub.others.len() > 100 {
            let Some(expired) = self
                .task_hub
                .others
                .keys()
                .find(|id| self.jobs.get(**id).is_none())
                .copied()
            else {
                break;
            };
            self.task_hub.others.remove(&expired);
            self.task_hub.contexts.remove(&expired);
            self.task_hub.keys.remove(&expired);
            self.jobs.forget(expired);
        }
        self.task_hub.contexts.insert(id, context);
        self.task_hub.keys.insert(
            id,
            format!(
                "{}-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
                id.0
            ),
        );
    }
    pub(super) fn select_task(&mut self, id: JobId) {
        if self.task_hub.selected == Some(id) {
            return;
        }
        let Some(next) = self.task_hub.others.remove(&id) else {
            return;
        };
        if let (Some(previous), Some(task)) =
            (self.task_hub.selected.replace(id), self.task.replace(next))
        {
            self.task_hub.others.insert(previous, task);
        }
        if self.busy.is_none() {
            if let Some(active) = self.jobs.get(id) {
                self.cancel = active.cancel.clone();
            }
        }
    }
    pub(super) fn record_current_task(&mut self) {
        let Some(id) = self.task_hub.selected else {
            return;
        };
        let (Some(task), Some(context), Some(key)) = (
            self.task.as_ref(),
            self.task_hub.contexts.get(&id),
            self.task_hub.keys.get(&id),
        ) else {
            return;
        };
        let (title, status, steps, error) = task.history_summary();
        let secrets = self
            .session
            .as_ref()
            .map(|s| vec![s.access_token.clone()])
            .unwrap_or_default();
        let record = TaskRecord {
            key: key.clone(),
            title: pcl_core::crash::redact(&title, &secrets),
            status,
            steps: steps
                .iter()
                .map(|s| pcl_core::crash::redact(s, &secrets))
                .collect(),
            error: error
                .as_deref()
                .map(|s| pcl_core::crash::redact(s, &secrets)),
            root: context.root.clone(),
            target: context.target.clone(),
            when: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        self.task_hub.records.retain(|r| r.key != record.key);
        self.task_hub.records.insert(0, record);
        self.task_hub.records.truncate(200);
        self.save_task_history();
    }
    pub(super) fn task_list(&mut self, ui: &mut egui::Ui) {
        let mut summaries = self
            .task_hub
            .others
            .iter()
            .map(|(&id, t)| {
                (
                    id,
                    t.short_summary(),
                    t.is_running(),
                    t.is_failed() || t.was_cancelled(),
                )
            })
            .collect::<Vec<_>>();
        if let (Some(id), Some(task)) = (self.task_hub.selected, self.task.as_ref()) {
            summaries.push((
                id,
                task.short_summary(),
                task.is_running(),
                task.is_failed() || task.was_cancelled(),
            ));
        }
        summaries.sort_by_key(|(id, _, _, _)| *id);
        let mut select = None;
        let mut cancel = None;
        let mut retry = None;
        if summaries.len() > 1 {
            super::card(ui, "任务列表", |ui| {
                for (id, label, running, retryable) in summaries {
                    let can_retry = retryable && self.jobs.retry(id).is_some();
                    ui.horizontal(|ui| {
                        let height = ui.spacing().interact_size.y;
                        let action_width = 44.0;
                        let title_width = (ui.available_width()
                            - if running || can_retry {
                                action_width + ui.spacing().item_spacing.x
                            } else {
                                0.0
                            })
                        .max(0.0);
                        if ui
                            .add_sized(
                                [title_width, height],
                                egui::Button::selectable(
                                    self.task_hub.selected == Some(id),
                                    &label,
                                )
                                .truncate(),
                            )
                            .on_hover_text(&label)
                            .clicked()
                        {
                            select = Some(id);
                        }
                        if running
                            && ui
                                .add_sized([action_width, height], egui::Button::new("取消"))
                                .clicked()
                        {
                            cancel = Some(id);
                        }
                        if can_retry
                            && ui
                                .add_sized([action_width, height], egui::Button::new("重试"))
                                .clicked()
                        {
                            retry = Some(id);
                        }
                    });
                }
            });
        } else if let Some((id, _, false, _)) = summaries.first() {
            if self
                .task
                .as_ref()
                .is_some_and(|t| t.is_failed() || t.was_cancelled())
                && self.jobs.retry(*id).is_some()
                && ui.button("重试任务").clicked()
            {
                retry = Some(*id);
            }
        }
        if let Some(id) = select {
            self.select_task(id);
        }
        if let Some(id) = cancel {
            if let Some(job) = self.jobs.get(id) {
                job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        if let Some(id) = retry {
            self.retry_task(id);
        }
    }
    fn retry_task(&mut self, id: JobId) {
        let task = if self.task_hub.selected == Some(id) {
            self.task.as_ref()
        } else {
            self.task_hub.others.get(&id)
        };
        if !task.is_some_and(|task| task.is_failed() || task.was_cancelled()) {
            return;
        }
        let (Some(work), Some(context)) = (
            self.jobs.retry(id),
            self.task_hub.contexts.get(&id).cloned(),
        ) else {
            return;
        };
        let title = if self.task_hub.selected == Some(id) {
            self.task.as_ref()
        } else {
            self.task_hub.others.get(&id)
        }
        .map(|t| t.title().to_owned())
        .unwrap_or_else(|| "重试任务".into());
        if let Some((sender, _)) = self.start_download_job_with_context(&title, context) {
            sender.launch(work);
        }
    }
    pub(super) fn task_history_ui(&mut self, ui: &mut egui::Ui) {
        if self.task_hub.records.is_empty() {
            return;
        }
        super::card(ui, "任务历史", |ui| {
            for record in self.task_hub.records.iter() {
                ui.horizontal(|ui| {
                    let status_width = ui
                        .painter()
                        .layout_no_wrap(
                            record.status.clone(),
                            egui::TextStyle::Body.resolve(ui.style()),
                            ui.visuals().text_color(),
                        )
                        .size()
                        .x;
                    let title_width =
                        (ui.available_width() - status_width - ui.spacing().item_spacing.x)
                            .max(0.0);
                    let (title_rect, _) = ui.allocate_exact_size(
                        egui::vec2(title_width, ui.spacing().interact_size.y),
                        egui::Sense::hover(),
                    );
                    crate::ui_style::place_left(
                        ui,
                        title_rect,
                        egui::Label::new(
                            RichText::new(&record.title)
                                .strong()
                                .color(crate::theme::palette(ui.ctx()).text),
                        )
                        .truncate(),
                    )
                    .on_hover_text(&record.title);
                    ui.label(&record.status);
                });
                let location = format!("{} · {}", record.root.display(), history_age(record.when));
                ui.add(
                    egui::Label::new(RichText::new(&location).size(11.0).color(super::MUTED))
                        .truncate(),
                )
                .on_hover_text(&location);
                if let Some(error) = &record.error {
                    ui.add(egui::Label::new(error).wrap());
                }
                for step in &record.steps {
                    ui.label(RichText::new(step).size(12.0).color(super::MUTED));
                }
                ui.add_space(8.0);
            }
        });
    }
}

fn history_age(when: u64) -> String {
    let age = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(when);
    if age < 60 {
        "刚刚".into()
    } else if age < 3600 {
        format!("{} 分钟前", age / 60)
    } else if age < 86400 {
        format!("{} 小时前", age / 3600)
    } else {
        format!("{} 天前", age / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_title_and_details_share_left_edge() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        let title = "Mod download: fabric-api.jar";
        app.task_hub.records.push(TaskRecord {
            key: "fixture".into(),
            title: title.into(),
            status: "Failed".into(),
            when: 0,
            root: dir.path().join("long-directory-name/".repeat(30)),
            target: None,
            steps: vec![],
            error: Some("Network unavailable".into()),
        });
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let fallback = fonts.families[&egui::FontFamily::Proportional].clone();
        fonts
            .families
            .insert(egui::FontFamily::Name("PCL Bold".into()), fallback);
        ctx.set_fonts(fonts);
        for width in [420.0, 989.0] {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 300.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| app.task_history_ui(ui));
                },
            );
            let text = |prefix: &str| {
                output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.text().starts_with(prefix) => {
                            Some(text)
                        }
                        _ => None,
                    })
                    .unwrap()
            };
            let heading = text(title);
            let path = text(&dir.path().display().to_string());
            let error = text("Network unavailable");
            assert!((heading.pos.x - path.pos.x).abs() <= 1.0);
            assert!((heading.pos.x - error.pos.x).abs() <= 1.0);
            assert!(path.galley.elided, "long paths must not expand the card");
            let heading_right = heading.pos.x + heading.galley.rect.right();
            assert!(
                heading_right < text("Failed").pos.x,
                "title must leave room for status"
            );
        }
    }

    #[test]
    fn long_history_title_does_not_hide_its_terminal_status() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        app.task_hub.records.push(TaskRecord {
            key: "fixture".into(),
            title: "very long instance name ".repeat(60),
            status: "已失败".into(),
            when: 0,
            root: dir.path().to_owned(),
            target: None,
            steps: vec![],
            error: None,
        });
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let fallback = fonts.families[&egui::FontFamily::Proportional].clone();
        fonts
            .families
            .insert(egui::FontFamily::Name("PCL Bold".into()), fallback);
        ctx.set_fonts(fonts);
        let mut bounds = None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(420.0, 300.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    bounds = Some(ui.max_rect());
                    app.task_history_ui(ui);
                });
            },
        );
        let status = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "已失败" => {
                    Some(text.visual_bounding_rect())
                }
                _ => None,
            })
            .expect("terminal status was painted");
        assert!(bounds.unwrap().contains_rect(status), "{status:?}");
    }

    #[test]
    fn long_task_names_leave_cancel_buttons_inside_the_card() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        app.task_hub.selected = Some(JobId(1));
        app.task = Some(TaskState::new("long instance title ".repeat(50)));
        app.task_hub
            .others
            .insert(JobId(2), TaskState::new("another task"));
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let fallback = fonts.families[&egui::FontFamily::Proportional].clone();
        fonts
            .families
            .insert(egui::FontFamily::Name("PCL Bold".into()), fallback);
        ctx.set_fonts(fonts);
        let mut bounds = None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(420.0, 300.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    bounds = Some(ui.max_rect());
                    app.task_list(ui);
                    assert!(ui.min_rect().right() <= ui.max_rect().right());
                });
            },
        );
        let buttons = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "取消" => {
                    Some(text.visual_bounding_rect())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(buttons.len(), 2);
        assert!(
            buttons
                .iter()
                .all(|rect| bounds.unwrap().contains_rect(*rect)),
            "{buttons:?}"
        );
    }
}
