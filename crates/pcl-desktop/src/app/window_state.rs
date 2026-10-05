use super::Launcher;
use eframe::egui;
use pcl_core::config::LauncherWindowSize;

#[derive(Default)]
pub(super) struct WindowState {
    initialized: bool,
    pending: Option<(LauncherWindowSize, f64)>,
}

impl WindowState {
    fn observe(
        &mut self,
        size: LauncherWindowSize,
        now: f64,
        flush: bool,
    ) -> Option<LauncherWindowSize> {
        match self.pending {
            Some((previous, since)) if previous == size => {
                (flush || now - since >= 0.5).then_some(size)
            }
            _ => {
                self.pending = Some((size, now));
                flush.then_some(size)
            }
        }
    }
}

impl Launcher {
    pub(super) fn remember_window(&mut self, ctx: &egui::Context) {
        let (viewport, now) = ctx.input(|i| (i.viewport().clone(), i.time));
        if !self.window_state.initialized {
            self.window_state.initialized = true;
            let bounded = self
                .settings
                .launcher_window
                .bounded(viewport.monitor_size.map(|m| [m.x, m.y]));
            if bounded != self.settings.launcher_window {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    bounded.width,
                    bounded.height,
                )));
                self.settings.launcher_window = bounded;
                self.persist();
                return;
            }
        }
        if viewport.minimized == Some(true)
            || viewport.maximized == Some(true)
            || viewport.fullscreen == Some(true)
        {
            return;
        }
        let Some(rect) = viewport.inner_rect else {
            return;
        };
        if !rect.is_finite() || rect.width() < 810.0 || rect.height() < 470.0 {
            return;
        }
        let size = LauncherWindowSize {
            width: rect.width().round(),
            height: rect.height().round(),
        }
        .bounded(None);
        if let Some(size) = self
            .window_state
            .observe(size, now, viewport.close_requested())
        {
            if self.settings.launcher_window != size {
                self.settings.launcher_window = size;
                self.persist();
            }
        }
    }

    pub(super) fn flush_window_size(&mut self) {
        if let Some((size, _)) = self.window_state.pending {
            if self.settings.launcher_window != size {
                self.settings.launcher_window = size;
                self.persist();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_settles_before_write_but_close_flushes_last_size() {
        let mut state = WindowState::default();
        let mut size = LauncherWindowSize {
            width: 1100.0,
            height: 700.0,
        };
        assert_eq!(state.observe(size, 0.0, false), None);
        size.width = 1200.0;
        assert_eq!(state.observe(size, 0.3, false), None);
        assert_eq!(state.observe(size, 0.6, false), None);
        assert_eq!(state.observe(size, 0.9, false), Some(size));
        size.height = 800.0;
        assert_eq!(state.observe(size, 1.0, true), Some(size));
    }

    #[test]
    fn monitor_clamp_is_persisted_and_maximize_never_replaces_normal_size() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = super::super::event_tests::fixture(dir.path());
        app.settings.launcher_window = LauncherWindowSize {
            width: 3000.0,
            height: 2000.0,
        };
        app.persist();
        let ctx = egui::Context::default();
        let draw = |app: &mut Launcher, time: f64, size: [f32; 2], maximized: bool| {
            let mut input = egui::RawInput {
                time: Some(time),
                ..Default::default()
            };
            let viewport = input.viewports.entry(egui::ViewportId::ROOT).or_default();
            viewport.monitor_size = Some(egui::vec2(1200.0, 900.0));
            viewport.inner_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(size[0], size[1]),
            ));
            viewport.maximized = Some(maximized);
            let _ = ctx.run(input, |ctx| app.remember_window(ctx));
        };
        draw(&mut app, 0.0, [3000.0, 2000.0], false);
        assert_eq!(
            pcl_core::config::load_settings(&app.settings_path)
                .unwrap()
                .launcher_window,
            LauncherWindowSize {
                width: 1200.0,
                height: 900.0
            }
        );
        draw(&mut app, 0.1, [1000.0, 650.0], false);
        draw(&mut app, 0.7, [1000.0, 650.0], false);
        draw(&mut app, 0.8, [1200.0, 900.0], true);
        draw(&mut app, 1.4, [1200.0, 900.0], true);
        app.flush_window_size();
        assert_eq!(
            pcl_core::config::load_settings(&app.settings_path)
                .unwrap()
                .launcher_window,
            LauncherWindowSize {
                width: 1000.0,
                height: 650.0
            }
        );
    }

    #[test]
    fn launcher_size_survives_reload_without_changing_game_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let settings = pcl_core::config::Settings {
            launcher_window: LauncherWindowSize {
                width: 1200.0,
                height: 800.0,
            },
            width: 854,
            height: 480,
            ..Default::default()
        };
        pcl_core::config::save_settings(&path, &settings).unwrap();
        let restored = pcl_core::config::load_settings(&path).unwrap();
        assert_eq!(restored.launcher_window, settings.launcher_window);
        assert_eq!((restored.width, restored.height), (854, 480));
        assert_eq!(
            restored.launcher_window.bounded(Some([1000.0, 750.0])),
            LauncherWindowSize {
                width: 1000.0,
                height: 750.0
            }
        );
    }
}
