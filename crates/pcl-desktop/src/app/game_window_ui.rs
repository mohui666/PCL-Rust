//! Visibility follows a confirmed game-ready event, never a mere successful spawn.
use eframe::egui::{self, ViewportCommand};
use pcl_core::config::LauncherVisibility;

#[derive(Default)]
pub(super) struct GameWindowState {
    ready: Option<(u32, LauncherVisibility)>,
    pending: Vec<ViewportCommand>,
}

impl GameWindowState {
    pub(super) fn started(&mut self) {
        self.ready = None;
    }

    pub(super) fn ready(&mut self, pid: u32, visibility: LauncherVisibility) {
        if self.ready.is_some_and(|(active, _)| active == pid) {
            return;
        }
        self.ready = Some((pid, visibility));
        match visibility {
            LauncherVisibility::CloseOnLaunch => self.pending.push(ViewportCommand::Close),
            LauncherVisibility::HideThenClose | LauncherVisibility::HideThenRestore => {
                self.pending.push(ViewportCommand::Visible(false));
            }
            LauncherVisibility::Minimize => self.pending.push(ViewportCommand::Minimized(true)),
            LauncherVisibility::Keep => {}
        }
    }

    pub(super) fn finished(&mut self, pid: u32, success: bool, stopped: bool) {
        let Some((active, visibility)) = self.ready else {
            return;
        };
        if active != pid {
            return;
        }
        self.ready = None;
        match visibility {
            LauncherVisibility::HideThenClose if success && !stopped => {
                self.pending.push(ViewportCommand::Close);
            }
            LauncherVisibility::HideThenClose | LauncherVisibility::HideThenRestore => {
                self.pending.extend([
                    ViewportCommand::Visible(true),
                    ViewportCommand::Minimized(false),
                    ViewportCommand::Focus,
                ]);
            }
            _ => {}
        }
    }

    pub(super) fn apply(&mut self, ctx: &egui::Context) {
        for command in self.pending.drain(..) {
            ctx.send_viewport_cmd(command);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_launcher_restores_on_crash_and_ignores_old_processes() {
        let mut state = GameWindowState::default();
        state.started();
        assert!(state.pending.is_empty()); // Merely spawning never hides the launcher.
        state.ready(20, LauncherVisibility::HideThenClose);
        state.pending.clear();
        state.finished(19, true, false);
        assert!(state.pending.is_empty());
        assert!(state.ready.is_some());
        state.finished(20, false, false);
        assert!(matches!(
            state.pending.as_slice(),
            [
                ViewportCommand::Visible(true),
                ViewportCommand::Minimized(false),
                ViewportCommand::Focus
            ]
        ));
    }

    #[test]
    fn ready_is_once_and_exit_uses_this_launchs_frozen_policy() {
        let mut state = GameWindowState::default();
        state.ready(20, LauncherVisibility::HideThenRestore);
        state.ready(20, LauncherVisibility::CloseOnLaunch);
        assert!(matches!(
            state.pending.as_slice(),
            [ViewportCommand::Visible(false)]
        ));
        state.pending.clear();
        state.finished(20, true, false);
        assert!(matches!(
            state.pending.first(),
            Some(ViewportCommand::Visible(true))
        ));
        state.pending.clear();
        state.ready(21, LauncherVisibility::HideThenClose);
        state.pending.clear();
        state.finished(21, false, true);
        assert!(matches!(
            state.pending.first(),
            Some(ViewportCommand::Visible(true))
        ));
        state.pending.clear();
        state.ready(22, LauncherVisibility::HideThenClose);
        state.pending.clear();
        state.finished(22, true, false);
        assert!(matches!(state.pending.as_slice(), [ViewportCommand::Close]));
    }
}
