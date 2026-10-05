//! Production dropdown renderer with local values only; no settings or network.
#![allow(dead_code)]
#[path = "../src/theme.rs"]
mod theme;
#[path = "../src/ui_style.rs"]
mod ui_style;
use eframe::egui::{self, FontFamily};
#[derive(Default)]
struct Preview {
    selected: usize,
    long: usize,
    editable: String,
    disabled: bool,
    alternate: bool,
}
impl eframe::App for Preview {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        let settings = pcl_core::config::Settings {
            ui_theme: if self.alternate { 13 } else { 0 },
            ..Default::default()
        };
        theme::apply(ctx, &settings);
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("下拉框本地验证（模拟数据）");
            ui.label("生产控件；不联网、不读取或保存设置。Tab 聚焦；方向键暂选、Enter 确定、Esc 取消。");
            ui.checkbox(&mut self.disabled, "禁用控件");
            ui.checkbox(&mut self.alternate, "使用第 13 号主题");
            ui.add_space(20.0);
            ui.label("普通选项（第二项禁用）");
            ui.add_enabled_ui(!self.disabled, |ui| {
                let choices = ["自动选择", "不可用选项", "全部版本", "仅正式版本"];
                ui_style::PclComboBox::from_id_salt("simple").width(320.0).selected_text(choices[self.selected]).show_ui(ui, |ui| {
                    for (index, text) in choices.iter().enumerate() {
                        if ui.selectable_label_enabled(index != 1, self.selected == index, *text).clicked() { self.selected = index; }
                    }
                });
                ui.add_space(20.0);
                ui.label("长文本与滚动（40 项）");
                ui_style::PclComboBox::from_id_salt("long").width(320.0).selected_text(format!("选项 {} / 中文很长的路径/Java/运行时/目录/应用程序/Contents/Home/bin/java", self.long + 1)).show_ui(ui, |ui| {
                    for index in 0..40 { ui.selectable_value(&mut self.long, index, format!("选项 {} / 中文很长的路径/Java/运行时/目录/应用程序/Contents/Home/bin/java", index + 1)); }
                });
                ui.add_space(20.0);
                ui.label("可编辑版本（保留自由输入）");
                let (rect, _) = ui.allocate_exact_size(egui::vec2(320.0, 28.0), egui::Sense::hover());
                ui_style::editable_combo(ui, rect, "editable", &mut self.editable, &["", "26.2", "1.21.1", "1.20.1", "1.12.2"], "全部 (也可自行输入)");
                ui.add_space(20.0);
                ui.button("下一个焦点控件").on_hover_text("用来验证 Tab / Shift+Tab");
            });
        });
    }
}
fn main() -> eframe::Result {
    eframe::run_native(
        "PCL 下拉框本地模拟",
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
