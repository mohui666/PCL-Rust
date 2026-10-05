#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod cli;
mod native_window;
mod process;
mod startup_splash;
mod theme;
mod ui_style;

fn main() -> anyhow::Result<()> {
    if std::env::args_os().len() > 1 {
        #[cfg(windows)]
        unsafe {
            #[link(name = "kernel32")]
            extern "system" {
                fn AttachConsole(process: u32) -> i32;
            }
            AttachConsole(u32::MAX);
        }
        return cli::run();
    }
    let icon = image::load_from_memory(include_bytes!("../assets/icon.png"))?.to_rgba8();
    let icon = eframe::egui::IconData {
        width: icon.width(),
        height: icon.height(),
        rgba: icon.into_raw(),
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Plain Craft Launcher (PCL) Rust 第三方重构版")
            .with_icon(icon)
            .with_inner_size([989.0, 517.0])
            .with_min_inner_size([810.0, 470.0])
            .with_transparent(true)
            .with_decorations(false),
        ..Default::default()
    };
    let splash_enabled = pcl_core::config::load_settings(&pcl_core::config::settings_path())
        .map(|s| s.ui_launcher_logo)
        .unwrap_or(false);
    if let Err(error) = startup_splash::show(splash_enabled) {
        eprintln!("启动画面无法显示：{error:#}");
    }
    let result = eframe::run_native(
        "Plain Craft Launcher (PCL) Rust 第三方重构版",
        options,
        Box::new(|cc| Ok(Box::new(app::Launcher::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("窗口启动失败：{e}"));
    startup_splash::close();
    result
}
