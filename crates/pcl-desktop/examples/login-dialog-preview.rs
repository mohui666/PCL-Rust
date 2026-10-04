//! Local visual fixture: uses production renderers, never signs in or edits accounts.
#![allow(dead_code)]
#[path = "../src/app/hint_ui.rs"]
mod hint_ui;
#[path = "../src/app/modal_ui.rs"]
mod modal_ui;
#[path = "../src/theme.rs"]
mod theme;
#[path = "../src/ui_style.rs"]
mod ui_style;

use eframe::egui::{self, FontFamily};

#[derive(Default)]
struct Preview {
    kind: Option<usize>,
    hints: hint_ui::HintQueue,
    value: String,
    outcome: String,
}
impl eframe::App for Preview {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        theme::apply(ctx, &pcl_core::config::Settings::default());
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("登录消息框本地验证（模拟数据）");
            ui.label("此窗口仅验证实际渲染组件，不发出网络请求，不读取或修改账号。");
            ui.horizontal(|ui| {
                for (index, label) in ["登录失败", "警告", "设备码", "输入框"].iter().enumerate()
                {
                    if ui.button(*label).clicked() {
                        self.kind = Some(index);
                    }
                }
                if ui.button("三色提示").clicked() {
                    self.hints
                        .push(hint_ui::HintKind::Info, "正在验证 Xbox 账户（2/6）…");
                    self.hints
                        .push(hint_ui::HintKind::Success, "网页登录成功！");
                    self.hints.push(
                        hint_ui::HintKind::Error,
                        "登录尝试太过频繁，请等待几分钟后再试。",
                    );
                }
            });
            ui.label(&self.outcome);
        });
        let action = match self.kind {
            Some(0) => modal_ui::account_modal(ctx, "preview-error", "登录失败", "Minecraft 服务拒绝了此应用的登录请求（HTTP 403）。\n请检查客户端 ID 配置和 Minecraft API 访问资格；如应用尚未获批，请先等待审核结果。", &["检查应用配置", "取消"]),
            Some(1) => modal_ui::account_modal_with_options(ctx, "preview-warning", "登录失败", "此账号需要完成 Microsoft 安全检查，请前往微软账户页处理。", &["微软账户", "我知道了"], modal_ui::ModalOptions { warning: true, ..Default::default() }),
            Some(2) => modal_ui::account_modal_with_options(ctx, "preview-device", "登录 Minecraft", "登录网页将自动开启，请在网页中输入 DEMO-CODE（仅模拟）。\n\n如果网络环境不佳，网页可能一直加载不出来，届时请检查网络连接。", &["重新打开网页", "复制代码", "取消"], modal_ui::ModalOptions::device()),
            Some(3) => modal_ui::account_input_modal(ctx, "preview-input", "输入框", "验证输入焦点与取消行为：", &mut self.value, &["确定", "取消"]),
            _ => None,
        };
        if let Some(index) = action {
            self.outcome = format!("模拟操作结果：按钮 {}，仅记录，不执行外部操作。", index + 1);
            self.kind = None;
        }
        self.hints.show(ctx);
        modal_ui::finish_frame(ctx);
    }
}
fn main() -> eframe::Result {
    eframe::run_native(
        "PCL 登录 UI 本地验证",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([989.0, 517.0]),
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
