use super::{Launcher, Page};
use eframe::egui;
use pcl_core::config::{self, LauncherResetScope};

impl Launcher {
    pub(super) fn request_settings_reset(&mut self, scope: LauncherResetScope) {
        if self.busy.is_none() && !self.jobs.is_active() && self.game_pid.is_none() {
            self.pending_settings_reset = Some(scope);
        }
    }

    pub(super) fn settings_reset_dialog(&mut self, ctx: &egui::Context) {
        let Some(scope) = self.pending_settings_reset else {
            return;
        };
        let message = format!(
            "是否要初始化 {} 页面的所有设置？\n背景图片、音乐、主页、游戏文件及账号不会被删除。",
            scope.label()
        );
        if let Some(action) = super::modal_ui::account_modal_with_options(
            ctx,
            "reset-settings-page",
            "初始化确认",
            &message,
            &["确定", "取消"],
            super::modal_ui::ModalOptions::warning(),
        ) {
            self.pending_settings_reset = None;
            if action != 0 {
                return;
            }
            if self.busy.is_some() || self.jobs.is_active() || self.game_pid.is_some() {
                self.error = Some("当前有任务或游戏正在运行，暂时无法初始化设置。".into());
                return;
            }
            match config::reset_launcher_page(&self.settings_path, &self.settings, scope) {
                Ok((next, backup)) => {
                    self.settings = next;
                    if matches!(
                        scope,
                        LauncherResetScope::Personalization | LauncherResetScope::All
                    ) {
                        self.appearance.invalidate_background();
                    }
                    self.setup_launch = Default::default();
                    self.instance_setup = Default::default();
                    // External files and account state remain owned by their existing managers.
                    self.page = Page::Settings;
                    self.settings_tab = match scope {
                        LauncherResetScope::Launch => 0,
                        LauncherResetScope::Personalization => 1,
                        _ => 2,
                    };
                    self.java_text = self
                        .settings
                        .java_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    pcl_core::system::configure_debug(&self.settings.system);
                    if let Err(error) = pcl_core::network::configure(&self.settings.downloads) {
                        self.error = Some(format!("应用下载设置失败：{error:#}"));
                    }
                    if matches!(scope, LauncherResetScope::Launch | LauncherResetScope::All) {
                        self.detect_java();
                    }
                    self.record(format!("初始化设置的备份：{}", backup.display()));
                    self.hints.push(
                        super::hint_ui::HintKind::Success,
                        format!("已初始化{}设置！", scope.label()),
                    );
                }
                Err(error) => self.error = Some(format!("初始化失败：{error:#}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelling_page_reset_preserves_disk_and_running_game_blocks_request() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        let before = std::fs::read(&app.settings_path).unwrap();
        app.request_settings_reset(LauncherResetScope::Personalization);
        let ctx = egui::Context::default();
        for (time, events) in [
            (0.0, vec![]),
            (
                0.5,
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            ),
        ] {
            let _ = ctx.run(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    app.settings_reset_dialog(ctx);
                    super::super::modal_ui::finish_frame(ctx);
                },
            );
        }
        assert!(app.pending_settings_reset.is_none());
        assert_eq!(std::fs::read(&app.settings_path).unwrap(), before);
        app.game_pid = Some(1);
        app.request_settings_reset(LauncherResetScope::Other);
        assert!(app.pending_settings_reset.is_none());
        assert_eq!(std::fs::read(&app.settings_path).unwrap(), before);
    }
}
