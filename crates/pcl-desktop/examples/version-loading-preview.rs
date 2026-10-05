//! Local visual fixture. No network, launcher settings, account, or game access.
#![allow(dead_code)]
#[path = "../src/app/loading_ui.rs"]
mod loading_ui;
#[path = "../src/theme.rs"]
mod theme;

use eframe::egui::{self, FontFamily};

#[derive(Default)]
enum State {
    #[default]
    Running,
    Cancelling,
    Failed,
    Cancelled,
    Ready,
}
#[derive(Default)]
struct Preview {
    indicator: loading_ui::Indicator,
    state: State,
    component: bool,
    result: String,
}
impl eframe::App for Preview {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        theme::apply(ctx, &pcl_core::config::Settings::default());
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("版本列表加载验证（本地模拟）");
            ui.label("仅使用生产加载渲染器；不联网，不读取设置，不安装游戏。");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.component, false, "原版 / 资源版本列表");
                ui.selectable_value(&mut self.component, true, "组件版本列表");
            });
            ui.horizontal(|ui| {
                for (label, state) in [
                    ("开始加载", State::Running),
                    ("返回错误", State::Failed),
                    ("完成取消", State::Cancelled),
                    ("返回列表", State::Ready),
                ] {
                    if ui.button(label).clicked() {
                        self.indicator.start();
                        self.state = state;
                        self.result.clear();
                    }
                }
            });
            ui.label(&self.result);
            ui.separator();
            if self.component {
                ui.label("Fabric · 模拟组件加载");
            }
            let status = match self.state {
                State::Running => loading_ui::Status::Running { cancelling: false },
                State::Cancelling => loading_ui::Status::Running { cancelling: true },
                State::Failed => {
                    loading_ui::Status::Failed("获取版本列表失败：模拟 HTTP 503，点击重新获取")
                }
                State::Cancelled => loading_ui::Status::Cancelled,
                State::Ready => loading_ui::Status::Ready,
            };
            let action = self.indicator.show_status(
                ui,
                "正在获取版本列表",
                status,
                if self.component {
                    loading_ui::Placement::Component
                } else {
                    loading_ui::Placement::Detail
                },
            );
            match action {
                Some(loading_ui::Action::Retry) => {
                    self.state = State::Running;
                    self.indicator.start();
                    self.result = "模拟重试已开始；没有网络请求。".into();
                }
                Some(loading_ui::Action::Cancel) => {
                    self.state = State::Cancelling;
                    self.result = "模拟取消请求已发出；点击“完成取消”模拟工作线程返回。".into();
                }
                None => {
                    ui.label("模拟列表已返回；此处不展示伪造版本。 ");
                }
                _ => (),
            }
        });
    }
}
fn main() -> eframe::Result {
    eframe::run_native(
        "PCL 版本加载本地模拟",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([989.0, 600.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::light());
            let mut fonts = egui::FontDefinitions::default();
            let path = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
                .map(|dir| dir.join("../Resources/PingFang-Regular.otf"))
                .unwrap_or_default();
            if let Ok(bytes) = std::fs::read(path) {
                fonts.font_data.insert(
                    "local-pingfang".into(),
                    egui::FontData::from_owned(bytes).into(),
                );
                fonts
                    .families
                    .get_mut(&FontFamily::Proportional)
                    .unwrap()
                    .insert(0, "local-pingfang".into());
            }
            cc.egui_ctx.set_fonts(fonts);
            Ok(Box::<Preview>::default())
        }),
    )
}
