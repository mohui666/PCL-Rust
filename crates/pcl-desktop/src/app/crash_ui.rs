//! Local crash analysis. Every automatic report is tied to the actual spawned PID/cwd.
use super::{modal_ui, Launcher};
use eframe::egui::{self, RichText};
use pcl_core::crash::{self, CrashReport};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant, SystemTime},
};

struct GameSnapshot {
    pid: u32,
    directory: PathBuf,
    started: SystemTime,
    secret: String,
    lines: VecDeque<String>,
    bytes: usize,
}
impl GameSnapshot {
    fn line(&mut self, line: String) {
        self.bytes += line.len();
        self.lines.push_back(line);
        while self.lines.len() > 1000 || self.bytes > 1024 * 1024 {
            if let Some(old) = self.lines.pop_front() {
                self.bytes -= old.len();
            } else {
                break;
            }
        }
    }
}
enum Completion {
    Report(String, CrashReport),
    Exported(PathBuf),
}
#[derive(Default)]
pub(super) struct CrashUiState {
    current: Option<GameSnapshot>,
    delayed: Option<(Instant, GameSnapshot)>,
    pending: Option<mpsc::Receiver<Result<Completion, String>>>,
    report: Option<CrashReport>,
    title: String,
    message: String,
    open: bool,
}
impl CrashUiState {
    pub(super) fn context(
        &mut self,
        pid: u32,
        directory: PathBuf,
        started: SystemTime,
        secret: String,
    ) {
        self.current = Some(GameSnapshot {
            pid,
            directory,
            started,
            secret,
            lines: VecDeque::new(),
            bytes: 0,
        });
    }
    pub(super) fn line(&mut self, pid: u32, text: String) {
        if let Some(snapshot) = self.current.as_mut().filter(|snapshot| snapshot.pid == pid) {
            snapshot.line(text);
        } else if let Some((_, snapshot)) = self
            .delayed
            .as_mut()
            .filter(|(_, snapshot)| snapshot.pid == pid)
        {
            snapshot.line(text);
        }
    }
    pub(super) fn finished(&mut self, pid: u32, success: bool, stopped: bool) {
        if self
            .current
            .as_ref()
            .is_none_or(|snapshot| snapshot.pid != pid)
        {
            return;
        }
        let snapshot = self.current.take().expect("matched context");
        if !success && !stopped {
            self.delayed = Some((Instant::now() + Duration::from_secs(2), snapshot));
        }
    }
    fn worker(
        &mut self,
        ctx: &egui::Context,
        job: impl FnOnce() -> anyhow::Result<Completion> + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        self.open = true;
        self.message = "正在读取并分析本地日志……".into();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = job().map_err(|error| format!("{error:#}"));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }
    fn poll(&mut self, ctx: &egui::Context) {
        if self.pending.is_none()
            && self
                .delayed
                .as_ref()
                .is_some_and(|(when, _)| Instant::now() >= *when)
        {
            let (_, snapshot) = self.delayed.take().unwrap();
            let label = format!(
                "{} · PID {}",
                snapshot
                    .directory
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                snapshot.pid
            );
            self.report = None;
            self.worker(ctx, move || {
                let lines = snapshot.lines.into_iter().collect::<Vec<_>>();
                let report = crash::collect(
                    &snapshot.directory,
                    Some(snapshot.started),
                    &lines,
                    &[snapshot.secret],
                )?;
                Ok(Completion::Report(label, report))
            });
        }
        if self.delayed.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let result = self.pending.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("本地日志任务已中断".into())),
            Err(mpsc::TryRecvError::Empty) => None,
        });
        if let Some(result) = result {
            self.pending = None;
            match result {
                Ok(Completion::Report(title, report)) => {
                    self.title = title;
                    self.message = format!(
                        "已读取 {} 个日志文件；分析结果仅在本机生成。",
                        report.files.len()
                    );
                    self.report = Some(report);
                }
                Ok(Completion::Exported(path)) => {
                    self.message = format!("报告已导出：{}", path.display())
                }
                Err(error) => self.message = format!("操作未完成：{error}"),
            }
        }
    }
}
impl Launcher {
    pub(super) fn analyze_selected_logs(&mut self, ctx: &egui::Context) {
        if self.crash.pending.is_some() {
            self.crash.open = true;
            return;
        }
        let result = (|| {
            let id = self
                .settings
                .selected_version
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("请先选择要分析的游戏版本"))?;
            Ok::<_, anyhow::Error>((
                id.to_owned(),
                pcl_core::config::instance_game_dir(&self.settings.game_root, id)?,
            ))
        })();
        let (label, directory) = match result {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                return;
            }
        };
        let secrets = self
            .session
            .as_ref()
            .map(|session| vec![session.access_token.clone()])
            .unwrap_or_default();
        self.crash.report = None;
        self.crash.worker(ctx, move || {
            Ok(Completion::Report(
                label,
                crash::collect(&directory, None, &[], &secrets)?,
            ))
        });
    }
    pub(super) fn import_crash_logs(&mut self, ctx: &egui::Context) {
        if self.crash.pending.is_some() {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title("导入游戏日志或日志 ZIP")
            .add_filter("日志", &["log", "txt", "zip"])
            .pick_file()
        else {
            return;
        };
        let label = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let secrets = self
            .session
            .as_ref()
            .map(|session| vec![session.access_token.clone()])
            .unwrap_or_default();
        self.crash.report = None;
        self.crash.worker(ctx, move || {
            Ok(Completion::Report(label, crash::import(&path, &secrets)?))
        });
    }
    pub(super) fn export_runtime_logs(&mut self, ctx: &egui::Context) {
        if self.crash.pending.is_some() {
            return;
        }
        let secrets = self
            .session
            .as_ref()
            .map(|session| vec![session.access_token.clone()])
            .unwrap_or_default();
        let text = crash::redact(
            &self.logs.iter().cloned().collect::<Vec<_>>().join("\n"),
            &secrets,
        );
        // This is explicitly the launcher's in-memory log, never guessed to be a game's crash log.
        let report=CrashReport{files:vec![crash::LogEvidence{name:"launcher-output.log".into(),text,truncated:false}],warnings:vec!["本报告为启动器运行日志；未自动收集游戏目录。分享前请检查路径、用户名和服务器地址。".into()],..Default::default()};
        self.export_crash_report(ctx, report);
    }
    fn export_crash_report(&mut self, ctx: &egui::Context, report: CrashReport) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("导出本地分析报告")
            .set_file_name("PCL-Rust-logs.zip")
            .add_filter("ZIP", &["zip"])
            .save_file()
        else {
            return;
        };
        self.crash.worker(ctx, move || {
            Ok(Completion::Exported(crash::export(&report, &path)?))
        });
    }
    pub(super) fn crash_dialogs(&mut self, ctx: &egui::Context) {
        self.crash.poll(ctx);
        if !self.crash.open {
            return;
        }
        let height = (ctx.content_rect().height() - 225.0).clamp(120.0, 460.0);
        let busy = self.crash.pending.is_some();
        let mut export = false;
        let mut import = false;
        let action = modal_ui::modal_frame_with_options(
            ctx,
            "crash-analysis",
            "日志分析与导出",
            (ctx.content_rect().width() - 50.0).clamp(400.0, 800.0),
            height,
            &["关闭"],
            modal_ui::ModalOptions::default(),
            |ui| {
                ui.horizontal(|ui| {
                    export = ui
                        .add_enabled(
                            !busy && self.crash.report.is_some(),
                            egui::Button::new("导出报告"),
                        )
                        .clicked();
                    import = ui
                        .add_enabled(!busy, egui::Button::new("导入其他日志"))
                        .clicked();
                    if busy {
                        ui.spinner();
                    }
                });
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .id_salt("crash-report-body")
                    .max_height(height - 36.0)
                    .show(ui, |ui| {
                        ui.label(&self.crash.message);
                        if let Some(report) = &self.crash.report {
                            ui.label(RichText::new(&self.crash.title).strong());
                            ui.add_space(10.0);
                            if report.findings.is_empty() {
                                ui.label(report.summary());
                            }
                            for finding in &report.findings {
                                ui.label(RichText::new(&finding.title).strong());
                                ui.label(&finding.explanation);
                                egui::CollapsingHeader::new("查看日志证据")
                                    .id_salt(&finding.code)
                                    .show(ui, |ui| {
                                        for line in &finding.evidence {
                                            ui.label(RichText::new(line).monospace().size(12.0));
                                        }
                                    });
                                ui.add_space(10.0);
                            }
                            for warning in &report.warnings {
                                ui.label(RichText::new(warning).size(12.0).color(super::MUTED));
                            }
                            egui::CollapsingHeader::new("包含的文件").show(ui, |ui| {
                                for file in &report.files {
                                    ui.label(format!(
                                        "{} · {} 字节{}",
                                        file.name,
                                        file.text.len(),
                                        if file.truncated {
                                            " · 已截取尾部"
                                        } else {
                                            ""
                                        }
                                    ));
                                }
                            });
                        }
                    });
            },
        );
        if action.is_some() {
            self.crash.open = false;
        }
        if export {
            if let Some(report) = self.crash.report.clone() {
                self.export_crash_report(ctx, report);
            }
        }
        if import {
            self.import_crash_logs(ctx);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_analysis_keeps_original_context_and_ignores_other_pids() {
        let mut state = CrashUiState::default();
        state.context(
            1,
            PathBuf::from("/first"),
            SystemTime::now(),
            "secret-original".into(),
        );
        state.line(2, "wrong".into());
        state.line(1, "first".into());
        state.finished(2, false, false);
        assert!(state.current.is_some());
        state.finished(1, false, false);
        state.context(
            3,
            PathBuf::from("/second"),
            SystemTime::now(),
            "second".into(),
        );
        state.line(1, "tail".into());
        state.line(3, "new".into());
        let old = &state.delayed.as_ref().unwrap().1;
        assert_eq!(old.directory, PathBuf::from("/first"));
        assert_eq!(old.secret, "secret-original");
        assert_eq!(old.lines, VecDeque::from(["first".into(), "tail".into()]));
        assert_eq!(
            state.current.as_ref().unwrap().lines,
            VecDeque::from(["new".into()])
        );
    }
    #[test]
    fn normal_and_requested_exit_do_not_trigger_diagnosis_and_output_is_bounded() {
        for (success, stopped) in [(true, false), (false, true)] {
            let mut state = CrashUiState::default();
            state.context(7, PathBuf::new(), SystemTime::now(), String::new());
            for _ in 0..1300 {
                state.line(7, "x".repeat(1024));
            }
            assert_eq!(state.current.as_ref().unwrap().lines.len(), 1000);
            state.finished(7, success, stopped);
            assert!(state.current.is_none());
            assert!(state.delayed.is_none());
        }
    }
}
