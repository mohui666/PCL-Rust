//! PageLaunchLeft.PanLaunching, driven by actual launcher and process events.
//! The bar counts completed preparation stages; elapsed time never advances it.
use super::{loading_ui, modal_ui, LaunchAction, Launcher, Page};
use crate::{theme, ui_style};
use eframe::egui::{self, Align2, Color32, FontId, Rect, Vec2};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Login,
    Java,
    Skin,
    Arguments,
    PreRun,
    Commands,
    Spawn,
    Waiting,
    Export,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::Login => "登录",
            Self::Java => "获取 Java",
            Self::Skin => "准备皮肤",
            Self::Arguments => "获取启动参数",
            Self::PreRun => "预启动处理",
            Self::Commands => "执行自定义命令",
            Self::Spawn => "启动进程",
            Self::Waiting => "等待游戏就绪",
            Self::Export => "导出启动脚本",
        }
    }
}

pub(crate) enum LaunchEvent {
    Stage {
        request: u64,
        stage: Stage,
    },
    Finished {
        request: u64,
        message: String,
    },
    Failed {
        request: u64,
        message: String,
        cancelled: bool,
    },
    Unavailable {
        request: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Game,
    Preview,
    Export,
}

struct Operation {
    request: u64,
    mode: Mode,
    version: String,
    method: &'static str,
    plan: Vec<Stage>,
    stage: Option<Stage>,
    completed: usize,
    cancel: Arc<AtomicBool>,
    pid: Option<u32>,
}

struct RunningGame {
    pid: u32,
    request: Option<u64>,
    version: String,
    method: &'static str,
    ready: bool,
}

#[derive(Default)]
pub(super) struct LaunchState {
    sequence: u64,
    operation: Option<Operation>,
    running: Option<RunningGame>,
    details: bool,
    pub(super) manage: bool,
}

impl LaunchState {
    pub(super) fn visible(&self) -> bool {
        self.operation.is_some() || (self.details && self.running.is_some())
    }

    pub(super) fn preparing(&self) -> bool {
        self.operation.as_ref().is_some_and(|op| op.pid.is_none())
    }

    fn begin(
        &mut self,
        action: &LaunchAction,
        version: String,
        microsoft: bool,
        cancel: Arc<AtomicBool>,
        login: bool,
    ) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        let mode = match action {
            LaunchAction::Run => Mode::Game,
            LaunchAction::Preview => Mode::Preview,
            LaunchAction::Export { .. } => Mode::Export,
        };
        let plan = if login {
            vec![Stage::Login]
        } else {
            match mode {
                Mode::Game => vec![
                    Stage::Java,
                    Stage::Skin,
                    Stage::Arguments,
                    Stage::PreRun,
                    Stage::Commands,
                    Stage::Spawn,
                ],
                Mode::Preview => vec![Stage::Java, Stage::Arguments],
                Mode::Export => vec![Stage::Java, Stage::Arguments, Stage::Export],
            }
        };
        self.operation = Some(Operation {
            request: self.sequence,
            mode,
            version,
            method: if microsoft {
                "正版登录"
            } else {
                "离线登录"
            },
            plan,
            stage: login.then_some(Stage::Login),
            completed: 0,
            cancel,
            pid: None,
        });
        self.details = false;
        self.manage = false;
        self.sequence
    }

    pub(super) fn authenticating(&self) -> bool {
        self.operation
            .as_ref()
            .is_some_and(|op| op.plan == [Stage::Login])
    }

    pub(super) fn authentication_ended(&mut self) {
        if self.authenticating() {
            self.operation = None;
        }
    }

    pub(super) fn authentication_token(&mut self, cancel: Arc<AtomicBool>) {
        if self.authenticating() {
            // Automatic reauthentication starts a new account worker. Keep the
            // launch panel attached to that worker's actual cancellation flag.
            if let Some(op) = self.operation.as_mut() {
                op.cancel = cancel;
            }
        }
    }

    fn advance(&mut self, request: u64, stage: Stage) -> bool {
        let Some(op) = self.operation.as_mut().filter(|op| op.request == request) else {
            return false;
        };
        if op.cancel.load(Ordering::Relaxed) {
            return false;
        }
        let Some(index) = op.plan.iter().position(|part| *part == stage) else {
            return false;
        };
        if index < op.completed {
            return false;
        }
        op.completed = index;
        op.stage = Some(stage);
        true
    }

    fn finish(&mut self, request: u64) -> Option<bool> {
        let op = self.operation.as_ref().filter(|op| op.request == request)?;
        // GameStarted releases the global busy slot. A later process failure
        // must not clear an unrelated operation that has since acquired it.
        let held_busy = op.pid.is_none();
        self.operation = None;
        Some(held_busy)
    }

    pub(super) fn started(&mut self, pid: u32, fallback_version: &str, microsoft: bool) {
        let (version, method, request) =
            if let Some(op) = self.operation.as_mut().filter(|op| op.mode == Mode::Game) {
                op.pid = Some(pid);
                op.stage = Some(Stage::Waiting);
                op.completed = op.plan.len();
                (op.version.clone(), op.method, Some(op.request))
            } else {
                (
                    fallback_version.into(),
                    if microsoft {
                        "正版登录"
                    } else {
                        "离线登录"
                    },
                    None,
                )
            };
        self.running = Some(RunningGame {
            pid,
            request,
            version,
            method,
            ready: false,
        });
    }

    pub(super) fn ready(&mut self, pid: u32) {
        if let Some(game) = self.running.as_mut().filter(|game| game.pid == pid) {
            game.ready = true;
            if self
                .operation
                .as_ref()
                .is_some_and(|op| op.pid == Some(pid))
            {
                self.operation = None;
            }
            self.details = false;
        }
    }

    pub(super) fn exited(&mut self, pid: u32) {
        if self.running.as_ref().is_some_and(|game| game.pid == pid) {
            self.running = None;
            self.details = false;
            self.manage = false;
        }
        if self
            .operation
            .as_ref()
            .is_some_and(|op| op.pid == Some(pid))
        {
            self.operation = None;
        }
    }

    fn cancel(&self, current_pid: Option<u32>, stop: &AtomicBool) {
        if let Some(op) = &self.operation {
            op.cancel.store(true, Ordering::Relaxed);
            if op.pid.is_some() && op.pid == current_pid {
                stop.store(true, Ordering::Relaxed);
            }
        }
    }

    pub(super) fn cancellation_requested(&self, pid: u32) -> bool {
        self.operation
            .as_ref()
            .is_some_and(|op| op.pid == Some(pid) && op.cancel.load(Ordering::Relaxed))
    }

    pub(super) fn stop_failed(&mut self, pid: u32) {
        if let Some(op) = self.operation.as_ref().filter(|op| op.pid == Some(pid)) {
            // The process monitor confirmed the child is still alive and reset
            // its stop flag. Re-enable the panel's cancel action for a retry.
            op.cancel.store(false, Ordering::Relaxed);
        }
    }
}

impl Launcher {
    pub(super) fn begin_launch_panel(
        &mut self,
        action: &LaunchAction,
        version: String,
        cancel: Arc<AtomicBool>,
        login: bool,
    ) -> u64 {
        self.page = Page::Launch;
        self.task_view = false;
        self.version_view = false;
        self.version_tools = false;
        self.launch_ui
            .begin(action, version, self.microsoft, cancel, login)
    }

    pub(super) fn handle_launch_event(&mut self, event: LaunchEvent) {
        match event {
            LaunchEvent::Stage { request, stage } => {
                if self.launch_ui.advance(request, stage) {
                    self.status = stage.label().into();
                }
            }
            LaunchEvent::Finished { request, message } => {
                if let Some(held_busy) = self.launch_ui.finish(request) {
                    if held_busy {
                        self.busy = None;
                        self.progress = None;
                    }
                    self.status = message;
                }
            }
            LaunchEvent::Failed {
                request,
                message,
                cancelled,
            } => {
                if let Some(held_busy) = self.launch_ui.finish(request) {
                    if held_busy {
                        self.busy = None;
                        self.progress = None;
                    }
                    self.status = message.clone();
                    self.record(message.clone());
                    if !cancelled {
                        self.error = Some(message);
                    }
                } else if self.launch_ui.running.as_ref().is_some_and(|game| {
                    game.request == Some(request) && Some(game.pid) == self.game_pid
                }) {
                    // Ready hides the progress operation, but its monitor still
                    // owns this child. Surface a later monitor error without
                    // releasing another task's busy slot or claiming an exit.
                    self.record(message.clone());
                    if !cancelled {
                        self.error = Some(message);
                    }
                }
            }
            LaunchEvent::Unavailable { request } => {
                if self.launch_ui.finish(request) == Some(true) {
                    self.busy = None;
                    self.progress = None;
                }
            }
        }
    }

    pub(super) fn launch_progress_sidebar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let palette = theme::palette(ui.ctx());
        let op = self.launch_ui.operation.as_ref();
        let game = self.launch_ui.running.as_ref();
        let cancelled = op.is_some_and(|op| op.cancel.load(Ordering::Relaxed));
        let launched = op.is_some_and(|op| op.pid.is_some()) || (op.is_none() && game.is_some());
        let title = if cancelled {
            "正在取消启动"
        } else if launched {
            "已启动游戏"
        } else {
            match op.map(|op| op.mode) {
                Some(Mode::Export) => "正在导出启动脚本",
                Some(Mode::Preview) => "正在检查启动详情",
                _ => "正在启动游戏",
            }
        };
        let version = op
            .map(|op| op.version.as_str())
            .or_else(|| game.map(|game| game.version.as_str()))
            .unwrap_or("");
        let method = op
            .map(|op| op.method)
            .or_else(|| game.map(|game| game.method))
            .unwrap_or("");
        let stage = if cancelled {
            "正在取消"
        } else if let Some(op) = op {
            op.stage.map(Stage::label).unwrap_or("正在检测启动详情")
        } else if game.is_some_and(|game| game.ready) {
            "已完成"
        } else {
            "等待游戏就绪"
        };
        let (completed, total) = op.map_or((1, 1), |op| (op.completed, op.plan.len()));
        let fraction = completed as f32 / total.max(1) as f32;
        // Original: centered stack in the area above the bottom 35 px button,
        // title 20, version 13.5, 30 px track margins and 12.5 px info rows.
        let height = if launched { 291.0 } else { 250.0 };
        let top = rect.top() + ((rect.height() - 55.0 - height) * 0.5).max(10.0) - 7.0;
        let center = rect.center().x;
        let loading = Rect::from_center_size(egui::pos2(center, top + 31.0), Vec2::new(80.0, 62.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(loading), |ui| {
            loading_ui::control(ui, "", loading.size());
        });
        ui.painter().text(
            egui::pos2(center, top + 77.0),
            Align2::CENTER_TOP,
            title,
            FontId::proportional(20.0),
            palette.dark,
        );
        let name_rect = Rect::from_min_size(
            egui::pos2(rect.left() + 40.0, top + 105.0),
            Vec2::new(rect.width() - 80.0, 20.0),
        );
        ui.put(
            name_rect,
            egui::Label::new(egui::RichText::new(version).size(13.5).color(palette.dark))
                .truncate(),
        )
        .on_hover_text(version);
        let track = Rect::from_min_size(
            egui::pos2(rect.left() + 30.0, top + 137.0),
            Vec2::new(rect.width() - 60.0, 4.0),
        );
        ui.painter().rect_filled(track, 0, palette.pale);
        if fraction > 0.0 {
            let filled = Rect::from_min_size(
                track.min,
                Vec2::new(track.width() * fraction, track.height()),
            );
            ui_style::gradient(
                ui.painter(),
                filled,
                [palette.accent, palette.dark, palette.dark, palette.accent],
            );
        }
        let y = top + 168.0;
        let progress = format!("{:.2} %", fraction * 100.0);
        let rows = [
            ("当前步骤", stage),
            ("登录方式", method),
            ("启动进度", progress.as_str()),
        ];
        let row_count = if launched { 2 } else { 3 };
        let font = FontId::proportional(12.5);
        let value_width = rows[..row_count]
            .iter()
            .map(|(_, value)| {
                ui.painter()
                    .layout_no_wrap((*value).into(), font.clone(), palette.text)
                    .size()
                    .x
            })
            .fold(0.0, f32::max);
        let label_right = center - (50.0 + 15.0 + value_width) * 0.5 + 50.0;
        for (i, (label, value)) in rows[..row_count].iter().enumerate() {
            let pos = egui::pos2(label_right, y + i as f32 * 23.0);
            ui.painter().text(
                pos,
                Align2::RIGHT_TOP,
                label,
                font.clone(),
                palette.text.gamma_multiply(0.5),
            );
            ui.painter().text(
                pos + Vec2::new(15.0, 0.0),
                Align2::LEFT_TOP,
                value,
                font.clone(),
                palette.text,
            );
        }
        ui.interact(
            track.expand(5.0),
            ui.id().with("launch-stage-progress"),
            egui::Sense::hover(),
        )
        .on_hover_text(format!(
            "已完成 {completed}/{total} 个启动准备阶段；阶段进度不表示剩余时间。"
        ));
        if launched {
            let hint = Rect::from_min_size(
                egui::pos2(rect.left() + 20.0, y + 62.0),
                Vec2::new(260.0, 58.0),
            );
            ui.painter().rect_stroke(
                hint,
                3,
                egui::Stroke::new(1.0_f32, Color32::from_gray(190)),
                egui::StrokeKind::Inside,
            );
            let heading = ui.painter().layout_no_wrap(
                "你知道吗".into(),
                font.clone(),
                Color32::from_gray(150),
            );
            let heading_rect = Rect::from_center_size(
                egui::pos2(center, hint.top()),
                heading.size() + Vec2::new(18.0, 0.0),
            );
            ui.painter()
                .rect_filled(heading_rect, 0, Color32::from_white_alpha(250));
            ui.painter().galley(
                heading_rect.min + Vec2::new(9.0, 0.0),
                heading,
                Color32::from_gray(150),
            );
            let tip = ui.painter().layout(
                "首次启动或加载较多 Mod 时，游戏可能需要稍等才能就绪。".into(),
                font,
                palette.text,
                hint.width() - 22.0,
            );
            ui.painter()
                .galley(hint.min + Vec2::new(11.0, 15.0), tip, palette.text);
        }
        let cancel_rect = Rect::from_min_size(
            egui::pos2(rect.left() + 20.0, rect.bottom() - 55.0),
            Vec2::new(260.0, 35.0),
        );
        let logs_rect = Rect::from_center_size(
            egui::pos2(center, (top + height + 18.0).min(cancel_rect.top() - 18.0)),
            Vec2::new(120.0, 24.0),
        );
        if ui
            .put(
                logs_rect,
                egui::Button::new(egui::RichText::new("查看启动日志").size(12.5)).frame(false),
            )
            .clicked()
        {
            self.show_logs = true;
        }
        let active = self.launch_ui.operation.is_some();
        if ui_style::outline_button(
            ui,
            cancel_rect,
            if active { "取消" } else { "返回" },
            None,
            false,
            !cancelled,
        )
        .clicked()
        {
            if active {
                self.launch_ui.cancel(self.game_pid, &self.game_stop);
                self.status = "正在取消启动…".into();
            } else {
                self.launch_ui.details = false;
            }
        }
    }

    pub(super) fn launch_running_dialog(&mut self, ctx: &egui::Context) {
        if !self.launch_ui.manage {
            return;
        }
        if self.game_pid.is_none() {
            self.launch_ui.manage = false;
            return;
        }
        let name = self
            .launch_ui
            .running
            .as_ref()
            .map(|game| game.version.as_str())
            .unwrap_or("Minecraft");
        let caption =
            format!("{name} 仍在运行。\n可以查看启动详情或关闭当前游戏，退出后再启动所选版本。");
        if let Some(action) = modal_ui::account_modal_with_options(
            ctx,
            "running-game-actions",
            "游戏仍在运行",
            &caption,
            &["查看启动详情", "关闭游戏", "返回"],
            modal_ui::ModalOptions::default(),
        ) {
            self.launch_ui.manage = false;
            match action {
                0 => {
                    self.launch_ui.details = true;
                    self.page = Page::Launch;
                    self.task_view = false;
                    self.version_view = false;
                    self.version_tools = false;
                }
                1 => {
                    self.game_stop.store(true, Ordering::Relaxed);
                    self.status = "正在关闭 Minecraft…".into();
                }
                _ => (),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{event_tests::fixture, Event};
    use pcl_core::config::LauncherVisibility;

    fn begin(app: &mut Launcher) -> (u64, Arc<AtomicBool>) {
        let cancel = Arc::new(AtomicBool::new(false));
        app.busy = Some("正在检查启动环境".into());
        let request = app.begin_launch_panel(
            &LaunchAction::Run,
            "fixture-game".into(),
            cancel.clone(),
            false,
        );
        (request, cancel)
    }

    #[test]
    fn stale_launch_events_cannot_advance_or_release_a_new_operation() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        let (old, _) = begin(&mut app);
        let (current, _) = begin(&mut app);
        for event in [
            LaunchEvent::Stage {
                request: old,
                stage: Stage::Spawn,
            },
            LaunchEvent::Failed {
                request: old,
                message: "old error".into(),
                cancelled: false,
            },
        ] {
            app.handle_event(Event::Launch(event));
        }
        assert!(app.busy.is_some() && app.error.is_none());
        assert_eq!(app.launch_ui.operation.as_ref().unwrap().completed, 0);
        app.handle_event(Event::Launch(LaunchEvent::Stage {
            request: current,
            stage: Stage::Arguments,
        }));
        assert_eq!(app.launch_ui.operation.as_ref().unwrap().completed, 2);
        app.handle_event(Event::Launch(LaunchEvent::Stage {
            request: current,
            stage: Stage::Java,
        }));
        assert_eq!(
            app.launch_ui.operation.as_ref().unwrap().stage,
            Some(Stage::Arguments)
        );
    }

    #[test]
    fn cancel_between_spawn_and_started_still_stops_only_the_owned_game() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        let (_, launch_cancel) = begin(&mut app);
        // A newly allocated generic operation token is not the launch token.
        app.cancel = Arc::new(AtomicBool::new(false));
        app.launch_ui.cancel(None, &app.game_stop);
        assert!(launch_cancel.load(Ordering::Relaxed));
        assert!(!app.cancel.load(Ordering::Relaxed));
        assert!(!app.game_stop.load(Ordering::Relaxed));
        app.handle_event(Event::GameStarted(711));
        assert!(app.game_stop.load(Ordering::Relaxed));
        app.handle_event(Event::GameReady {
            pid: 711,
            visibility: LauncherVisibility::Keep,
        });
        assert!(
            app.launch_ui.visible(),
            "late ready must not replace cancellation with success"
        );
        app.handle_event(Event::GameFinished {
            pid: 711,
            success: false,
            stopped: true,
            message: "已关闭".into(),
        });
        assert!(app.game_pid.is_none() && !app.launch_ui.visible());
    }

    #[test]
    fn ready_restores_launch_button_without_losing_the_monitored_child() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        begin(&mut app);
        app.handle_event(Event::GameStarted(712));
        assert!(app.launch_ui.visible());
        assert_eq!(
            app.launch_ui.operation.as_ref().unwrap().stage,
            Some(Stage::Waiting)
        );
        app.handle_event(Event::GameReady {
            pid: 712,
            visibility: LauncherVisibility::Keep,
        });
        assert!(!app.launch_ui.visible());
        assert_eq!(app.game_pid, Some(712));
        let token = app.cancel.clone();
        app.launch(false);
        assert!(app.launch_ui.manage);
        assert!(app.busy.is_none() && app.error.is_none());
        assert!(
            Arc::ptr_eq(&token, &app.cancel),
            "click must not start another worker"
        );
        assert_eq!(app.game_pid, Some(712));
    }

    #[test]
    fn old_game_exit_preserves_a_current_preview_and_new_busy_owner() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        let (game_request, _) = begin(&mut app);
        app.handle_event(Event::GameStarted(713));
        app.handle_event(Event::GameReady {
            pid: 713,
            visibility: LauncherVisibility::Keep,
        });
        app.busy = Some("另一个版本的启动检查".into());
        let preview = app.begin_launch_panel(
            &LaunchAction::Preview,
            "another-version".into(),
            Arc::new(AtomicBool::new(false)),
            false,
        );
        app.handle_event(Event::Launch(LaunchEvent::Failed {
            request: game_request.wrapping_sub(1),
            message: "old worker".into(),
            cancelled: false,
        }));
        app.handle_event(Event::GameFinished {
            pid: 713,
            success: true,
            stopped: false,
            message: "游戏已正常退出".into(),
        });
        assert_eq!(app.launch_ui.operation.as_ref().unwrap().request, preview);
        assert_eq!(app.busy.as_deref(), Some("另一个版本的启动检查"));
        assert!(app.error.is_none());
    }

    #[test]
    fn monitor_failure_after_ready_is_reported_without_releasing_a_new_preview() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        let (game_request, _) = begin(&mut app);
        app.handle_event(Event::GameStarted(716));
        app.handle_event(Event::GameReady {
            pid: 716,
            visibility: LauncherVisibility::Keep,
        });
        assert!(app.launch_ui.operation.is_none());
        app.busy = Some("preview busy".into());
        let preview = app.begin_launch_panel(
            &LaunchAction::Preview,
            "preview".into(),
            Arc::new(AtomicBool::new(false)),
            false,
        );
        app.handle_event(Event::Launch(LaunchEvent::Failed {
            request: game_request,
            message: "monitor read failed".into(),
            cancelled: false,
        }));
        assert_eq!(app.error.as_deref(), Some("monitor read failed"));
        assert!(app.logs.iter().any(|line| line == "monitor read failed"));
        assert_eq!(app.busy.as_deref(), Some("preview busy"));
        assert_eq!(app.launch_ui.operation.as_ref().unwrap().request, preview);
        assert_eq!(
            app.game_pid,
            Some(716),
            "monitor error does not confirm child exit"
        );
        app.handle_event(Event::GameFinished {
            pid: 716,
            success: true,
            stopped: false,
            message: "exited".into(),
        });
        app.error = None;
        app.handle_event(Event::Launch(LaunchEvent::Failed {
            request: game_request,
            message: "stale monitor".into(),
            cancelled: false,
        }));
        assert!(app.error.is_none());
    }

    #[test]
    fn unavailable_java_and_prelaunch_cancel_restore_normal_panel() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        let (request, _) = begin(&mut app);
        app.handle_event(Event::Launch(LaunchEvent::Unavailable { request }));
        assert!(!app.launch_ui.visible() && app.busy.is_none());
        let (request, _) = begin(&mut app);
        app.handle_event(Event::Launch(LaunchEvent::Failed {
            request,
            message: "启动已取消".into(),
            cancelled: true,
        }));
        assert!(!app.launch_ui.visible() && app.busy.is_none() && app.error.is_none());
        assert_eq!(app.status, "启动已取消");
    }

    #[test]
    fn authentication_retry_rebinds_the_actual_token_and_has_no_phantom_progress() {
        let mut state = LaunchState::default();
        let old = Arc::new(AtomicBool::new(false));
        state.begin(
            &LaunchAction::Run,
            "fixture".into(),
            true,
            old.clone(),
            true,
        );
        let retry = Arc::new(AtomicBool::new(false));
        state.authentication_token(retry.clone());
        state.cancel(None, &AtomicBool::new(false));
        assert!(retry.load(Ordering::Relaxed) && !old.load(Ordering::Relaxed));
        assert_eq!(state.operation.as_ref().unwrap().completed, 0);
        state.authentication_ended();
        assert!(!state.visible());
    }

    #[test]
    fn failed_game_stop_keeps_preview_busy_and_allows_owned_cancel_retry() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        let (_, cancel) = begin(&mut app);
        app.handle_event(Event::GameStarted(715));
        app.launch_ui.cancel(app.game_pid, &app.game_stop);
        assert!(cancel.load(Ordering::Relaxed));
        app.handle_event(Event::GameStopFailed {
            pid: 715,
            message: "still alive".into(),
        });
        assert!(!cancel.load(Ordering::Relaxed));
        assert!(app.launch_ui.visible());
        app.busy = Some("preview busy".into());
        let preview = app.begin_launch_panel(
            &LaunchAction::Preview,
            "preview".into(),
            Arc::new(AtomicBool::new(false)),
            false,
        );
        app.handle_event(Event::GameStopFailed {
            pid: 715,
            message: "close failed".into(),
        });
        assert_eq!(app.busy.as_deref(), Some("preview busy"));
        assert_eq!(app.launch_ui.operation.as_ref().unwrap().request, preview);
        assert_eq!(app.game_pid, Some(715));
        assert_eq!(app.error.as_deref(), Some("close failed"));
        app.error = None;
        app.handle_event(Event::GameStopFailed {
            pid: 999,
            message: "stale".into(),
        });
        assert!(app.error.is_none());
    }

    #[test]
    fn launch_sidebar_renders_original_panel_then_a_clickable_normal_button() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        app.settings.selected_version = Some("fixture-game".into());
        let (request, _) = begin(&mut app);
        let ctx = egui::Context::default();
        let mut next_time = 0.0;
        let mut frame = |app: &mut Launcher, events| {
            let time = next_time;
            next_time += 0.6;
            ctx.run(
                egui::RawInput {
                    time: Some(time),
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(810.0, 470.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    theme::apply(ctx, &app.settings);
                    app.sidebar(ctx);
                },
            )
        };
        let words = |output: &egui::FullOutput| {
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.job.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let output = frame(&mut app, vec![]);
        let text = words(&output);
        for label in [
            "正在启动游戏",
            "正在检测启动详情",
            "当前步骤",
            "登录方式",
            "启动进度",
            "取消",
        ] {
            assert!(
                text.iter().any(|value| value == label),
                "missing {label}: {text:?}"
            );
        }
        app.handle_event(Event::Launch(LaunchEvent::Stage {
            request,
            stage: Stage::Java,
        }));
        app.handle_event(Event::GameStarted(714));
        let text = words(&frame(&mut app, vec![]));
        assert!(text.iter().any(|value| value == "已启动游戏"));
        assert!(text.iter().any(|value| value == "等待游戏就绪"));
        assert!(text.iter().any(|value| value == "你知道吗"));
        assert!(!text.iter().any(|value| value == "启动进度"));
        app.handle_event(Event::GameReady {
            pid: 714,
            visibility: LauncherVisibility::Keep,
        });
        // Ready swaps the source sidebar through its exit/entry animation.
        // Settle it before checking the normal button and its real hit target.
        frame(&mut app, vec![]);
        let text = words(&frame(&mut app, vec![]));
        assert!(text.iter().any(|value| value == "启动游戏"));
        assert!(!text.iter().any(|value| value == "游戏运行中"));
        // Without a title panel, launch_y = 470 - 20 - 35 - 10 - 54.
        let pos = egui::pos2(150.0, 375.0);
        for pressed in [true, false] {
            frame(
                &mut app,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert!(
            app.launch_ui.manage,
            "normal button must remain clickable while one child is monitored"
        );
        assert_eq!(app.game_pid, Some(714));
    }
}
