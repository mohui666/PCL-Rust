//! Native window test surface. Simulated game events, no account or settings I/O.
#![allow(dead_code)]
#[path = "../src/app/game_window_ui.rs"]
mod game_window_ui;
#[path = "../src/native_window.rs"]
mod native_window;

use eframe::egui;
use pcl_core::config::LauncherVisibility;
use std::time::{Duration, Instant};

struct Preview {
    opacity: u16,
    native: native_window::WindowOpacity,
    window: game_window_ui::GameWindowState,
    finish_at: Option<Instant>,
    outcome: String,
}
impl Default for Preview {
    fn default() -> Self {
        Self {
            opacity: 100,
            native: Default::default(),
            window: Default::default(),
            finish_at: None,
            outcome: String::new(),
        }
    }
}
impl eframe::App for Preview {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Err(error) = self.native.update(frame, self.opacity) {
            self.outcome = format!("系统接口失败：{error:#}");
        }
        if self.finish_at.is_some_and(|at| Instant::now() >= at) {
            self.finish_at = None;
            self.window.finished(1, true, false);
            self.outcome = "已处理模拟游戏退出，恢复预览窗口".into();
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("窗口行为本地验证（模拟游戏事件）");
            ui.label("只操作此预览窗口，不启动游戏，不读写用户设置或账号。");
            ui.horizontal(|ui| {
                if ui.button("不透明度 60%").clicked() {
                    self.opacity = 60;
                }
                if ui.button("不透明度 100%").clicked() {
                    self.opacity = 100;
                }
                if ui.button("隐藏后恢复").clicked() {
                    self.window.started();
                    self.window.ready(1, LauncherVisibility::HideThenRestore);
                    self.finish_at = Some(Instant::now() + Duration::from_secs(2));
                    self.outcome = "模拟运行中，2 秒后恢复窗口".into();
                }
            });
            ui.label(format!("当前窗口不透明度：{}%", self.opacity));
            ui.label(&self.outcome);
        });
        self.window.apply(ctx);
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}
fn main() -> eframe::Result {
    eframe::run_native(
        "PCL 窗口行为本地验证",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([700.0, 240.0])
                .with_transparent(true),
            ..Default::default()
        },
        Box::new(|cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::light());
            let mut fonts = egui::FontDefinitions::default();
            let path = std::env::current_exe().ok().and_then(|p| {
                p.parent()
                    .map(|dir| dir.join("../Resources/PingFang-Regular.otf"))
            });
            if let Some(bytes) = path.and_then(|p| std::fs::read(p).ok()) {
                fonts.font_data.insert(
                    "local-pingfang".into(),
                    egui::FontData::from_owned(bytes).into(),
                );
                fonts
                    .families
                    .get_mut(&egui::FontFamily::Proportional)
                    .unwrap()
                    .insert(0, "local-pingfang".into());
            }
            cc.egui_ctx.set_fonts(fonts);
            Ok(Box::<Preview>::default())
        }),
    )
}
