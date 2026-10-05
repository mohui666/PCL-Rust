//! Hash-based Mod update selection. Browsing owns a receiver; writes use JobId.
use super::{
    hint_ui::HintKind,
    modal_ui::{modal_frame_with_options, ModalOptions},
    Event, Launcher,
};
use crate::theme;
use eframe::egui::{self, RichText};
use pcl_core::{
    metadata,
    mod_updates::{self, UpdatePlan, UpdateReport},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
        Arc,
    },
};
#[derive(Default)]
pub(super) struct ModUpdateState {
    request: Option<CheckRequest>,
    plan: Option<Arc<UpdatePlan>>,
    selected: BTreeSet<String>,
    root: PathBuf,
    target: String,
    error: Option<String>,
    open: bool,
    confirm: bool,
}
struct CheckRequest {
    receiver: Receiver<Result<UpdatePlan, CheckError>>,
    cancel: Arc<AtomicBool>,
}
struct CheckError {
    message: String,
    cancelled: bool,
}
impl Launcher {
    pub(super) fn open_mod_updates(&mut self, id: &str, instance: &Path) {
        if self.busy.is_some()
            || self.game_pid.is_some()
            || self.jobs.conflicts_with(&self.settings.game_root)
        {
            return;
        }
        match pcl_core::config::load_instance_settings(&self.settings.game_root, id) {
            Ok(s) if !s.disable_mod_updates => (),
            Ok(_) => {
                self.error = Some("该版本已禁用 Mod 更新".into());
                return;
            }
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        }
        if let Some(old) = self.mod_update.request.take() {
            old.cancel.store(true, Ordering::Relaxed);
        }
        let root = self.settings.game_root.clone();
        let target = id.to_owned();
        let instance = instance.to_owned();
        let (tx, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        self.mod_update = ModUpdateState {
            request: Some(CheckRequest { receiver, cancel }),
            root: root.clone(),
            target: target.clone(),
            open: true,
            ..Default::default()
        };
        std::thread::spawn(move || {
            let result = (|| {
                let resolved = metadata::resolve_version(&root, &target)?;
                let (minecraft, loader) = target_info(&resolved)?;
                mod_updates::check_updates(&instance, &minecraft, &loader, &worker_cancel)
            })()
            .map_err(|e: anyhow::Error| CheckError {
                cancelled: e.is::<pcl_core::model::OperationCancelled>(),
                message: format!("检查 Mod 更新失败：{e:#}"),
            });
            let _ = tx.send(result);
        });
    }
    pub(super) fn mod_update_tick(&mut self, ctx: &egui::Context) {
        let current = self.mod_update.root == self.settings.game_root
            && self.settings.selected_version.as_deref() == Some(self.mod_update.target.as_str());
        if !current {
            self.close_mod_updates();
            return;
        }
        let result = self
            .mod_update
            .request
            .as_ref()
            .and_then(|r| r.receiver.try_recv().ok());
        if let Some(result) = result {
            self.mod_update.request = None;
            match result {
                Ok(plan) => {
                    self.mod_update.selected =
                        plan.updates().iter().map(|u| u.file_name.clone()).collect();
                    self.mod_update.plan = Some(Arc::new(plan));
                }
                Err(error) if error.cancelled => {
                    self.mod_update.open = false;
                    self.status = "检查 Mod 更新已取消".into();
                }
                Err(error) => self.mod_update.error = Some(error.message),
            }
        }
        if self.mod_update.request.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    fn close_mod_updates(&mut self) {
        if let Some(request) = self.mod_update.request.take() {
            request.cancel.store(true, Ordering::Relaxed);
        }
        self.mod_update.open = false;
        self.mod_update.confirm = false;
        self.mod_update.plan = None;
    }
    fn start_selected_mod_updates(&mut self) {
        if self.game_pid.is_some()
            || self.mod_update.root != self.settings.game_root
            || self.settings.selected_version.as_deref() != Some(self.mod_update.target.as_str())
        {
            self.close_mod_updates();
            return;
        }
        let Some(plan) = self.mod_update.plan.clone() else {
            return;
        };
        let selected = self.mod_update.selected.clone();
        if selected.is_empty() {
            return;
        }
        let id = self.mod_update.target.clone();
        let Some((tx, _)) = self.start_download_job("正在更新 Mod", Some(id.clone())) else {
            return;
        };
        self.close_mod_updates();
        tx.spawn(move |tx| {
            let cancel = tx.cancel_token();
            let result = mod_updates::apply_updates(&plan, &selected, &cancel, |p| {
                let _ = tx.send(Event::Progress(p));
            });
            let event = match result {
                Ok(report) => Event::ModsUpdated { target: id, report },
                Err(e) => Event::download_failed("Mod 更新未完成", e),
            };
            let _ = tx.send(event);
        });
    }
    pub(super) fn complete_mod_updates(
        &mut self,
        target: String,
        report: UpdateReport,
        current_root: bool,
    ) {
        self.finish_download_task();
        self.status = format!(
            "已更新 {} 个 Mod，新增 {} 个依赖；原文件备份：{}",
            report.updated,
            report.added_dependencies,
            report.backup_directory.display()
        );
        self.record(self.status.clone());
        self.push_hint(
            HintKind::Success,
            format!("已更新 {} 个 Mod", report.updated),
        );
        if current_root && self.settings.selected_version.as_deref() == Some(target.as_str()) {
            self.refresh_mods();
        }
    }
    pub(super) fn mod_update_dialog(&mut self, ctx: &egui::Context) {
        if !self.mod_update.open {
            return;
        }
        if self.mod_update.confirm {
            let action=super::modal_ui::account_modal_with_options(ctx,"mod-update-warning","Mod 更新警告",
                "新版本 Mod 可能不兼容旧存档或其他 Mod。更新前请先备份存档，并检查更新日志；游玩整合包时请勿随意更新。\n\n旧 Mod 将保留在该实例的 PCL-Rust/mod-backups 目录，已禁用的 Mod 仍保持禁用。",
                &["继续更新","取消"],ModalOptions::warning());
            if let Some(action) = action {
                self.mod_update.confirm = false;
                if action == 0 {
                    self.start_selected_mod_updates();
                }
            }
            return;
        }
        let checking = self.mod_update.request.is_some();
        let has_updates = self
            .mod_update
            .plan
            .as_ref()
            .is_some_and(|p| !p.updates().is_empty());
        let buttons: &[&str] = if checking {
            &["取消"]
        } else if has_updates && !self.mod_update.selected.is_empty() {
            &["更新所选 Mod", "取消"]
        } else {
            &["关闭"]
        };
        let height = if checking {
            45.0
        } else {
            (ctx.content_rect().height() - 240.0).clamp(120.0, 350.0)
        };
        let action = modal_frame_with_options(
            ctx,
            "mod-updates",
            "检查 Mod 更新",
            (ctx.content_rect().width() - 100.0).min(600.0),
            height,
            buttons,
            ModalOptions::default(),
            |ui| {
                if checking {
                    super::loading_ui::inline(ui, "正在检查 Mod 更新……");
                    return;
                }
                update_results(ui, &mut self.mod_update, height);
            },
        );
        if let Some(action) = action {
            if !checking && has_updates && !self.mod_update.selected.is_empty() && action == 0 {
                self.mod_update.confirm = true;
            } else {
                self.close_mod_updates();
            }
        }
    }
}
fn update_results(
    ui: &mut egui::Ui,
    state: &mut ModUpdateState,
    height: f32,
) -> egui::scroll_area::ScrollAreaOutput<()> {
    egui::ScrollArea::vertical()
        .id_salt("mod-update-results")
        .max_height(height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if let Some(error) = &state.error {
                ui.colored_label(egui::Color32::from_rgb(255, 76, 76), error);
                return;
            }
            let Some(plan) = &state.plan else {
                return;
            };
            if plan.updates().is_empty() {
                ui.label("没有找到可更新的 Mod。");
            }
            for update in plan.updates() {
                let mut selected = state.selected.contains(&update.file_name);
                if crate::ui_style::checkbox(
                    ui,
                    &mut selected,
                    &format!(
                        "{}  {} → {}{}",
                        update.name,
                        update.current_version,
                        update.new_version,
                        if update.enabled {
                            ""
                        } else {
                            "（已禁用）"
                        }
                    ),
                    "",
                )
                .changed()
                {
                    if selected {
                        state.selected.insert(update.file_name.clone());
                    } else {
                        state.selected.remove(&update.file_name);
                    }
                }
                ui.label(
                    RichText::new(format!("{} · {}", update.source, update.file_name))
                        .size(12.0)
                        .color(theme::palette(ui.ctx()).dark),
                );
            }
            for issue in plan.issues() {
                ui.label(issue);
            }
            if !plan.unmatched().is_empty() {
                ui.collapsing(
                    format!("未识别的文件（{}）", plan.unmatched().len()),
                    |ui| {
                        for name in plan.unmatched() {
                            ui.label(name);
                        }
                        ui.label("无法匹配官方哈希，不代表已经是最新版本。");
                    },
                );
            }
        })
}
fn target_info(version: &serde_json::Value) -> anyhow::Result<(String, String)> {
    let libraries = version["libraries"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("版本缺少加载器库信息"))?;
    let has = |prefix: &str| {
        libraries
            .iter()
            .any(|v| v["name"].as_str().is_some_and(|s| s.starts_with(prefix)))
    };
    let loader = if has("net.neoforged:neoforge:") || has("net.neoforged.fancymodloader:loader:") {
        "neoforge"
    } else if has("net.minecraftforge:forge:") {
        "forge"
    } else if has("org.quiltmc:quilt-loader:") {
        "quilt"
    } else if has("net.fabricmc:fabric-loader:") {
        "fabric"
    } else {
        anyhow::bail!("当前版本没有支持的 Mod 加载器")
    };
    let minecraft = version["_pcl_jar_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("无法确定原版 Minecraft 版本"))?;
    Ok((minecraft.into(), loader.into()))
}
#[cfg(test)]
mod tests {
    #[test]
    fn long_provider_error_can_be_scrolled_to_its_last_line() {
        let ctx = egui::Context::default();
        let mut state = ModUpdateState {
            error: Some(format!(
                "{}\nlast error detail",
                "download failure\n".repeat(60)
            )),
            ..Default::default()
        };
        let mut viewport = egui::Rect::NOTHING;
        let mut last = None;
        for frame in 0..6 {
            let mut events = if frame > 0 {
                vec![egui::Event::PointerMoved(viewport.center())]
            } else {
                vec![]
            };
            if frame >= 2 {
                events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -10000.0),
                    modifiers: egui::Modifiers::NONE,
                });
            }
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 300.0),
                    )),
                    events,
                    time: Some(frame as f64 * 0.1),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let output = update_results(ui, &mut state, 120.0);
                        viewport = output.inner_rect;
                        last = Some(output);
                    });
                },
            );
        }
        let output = last.unwrap();
        assert!(output.content_size.y > 120.0);
        assert!(output.inner_rect.height() <= 120.0);
        assert!(output.state.offset.y + output.inner_rect.height() >= output.content_size.y - 1.0);
        assert!(state.error.as_ref().unwrap().ends_with("last error detail"));
    }

    #[test]
    fn disabled_instance_updates_do_not_start_network_request() {
        let d = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(d.path());
        let root = app.settings.game_root.clone();
        std::fs::create_dir_all(root.join("versions/v")).unwrap();
        std::fs::write(root.join("versions/v/v.json"), b"{}").unwrap();
        pcl_core::config::save_instance_settings(
            &root,
            "v",
            &pcl_core::config::InstanceSettings {
                disable_mod_updates: true,
                ..Default::default()
            },
        )
        .unwrap();
        app.open_mod_updates("v", &root);
        assert!(app.mod_update.request.is_none());
        assert!(app.error.as_ref().unwrap().contains("禁用"));
    }

    use super::*;
    #[test]
    fn stale_check_is_cancelled_without_clearing_another_job() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        let (_, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        app.mod_update = ModUpdateState {
            root: dir.path().join("old"),
            target: "old".into(),
            open: true,
            request: Some(CheckRequest {
                receiver,
                cancel: cancel.clone(),
            }),
            ..Default::default()
        };
        app.busy = Some("existing download".into());
        app.mod_update_tick(&egui::Context::default());
        assert!(cancel.load(Ordering::Relaxed));
        assert!(!app.mod_update.open);
        assert_eq!(app.busy.as_deref(), Some("existing download"));
    }
    #[test]
    fn provider_metadata_must_have_a_real_loader() {
        assert!(target_info(&serde_json::json!({"libraries":[],"_pcl_jar_id":"1.21.1"})).is_err());
        assert_eq!(target_info(&serde_json::json!({"libraries":[{"name":"net.fabricmc:fabric-loader:0.19.5"}],"_pcl_jar_id":"1.21.1"})).unwrap(),("1.21.1".into(),"fabric".into()));
    }
}
