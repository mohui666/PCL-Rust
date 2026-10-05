#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
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
        std::process::exit(pcl_cli::main_entry());
    }
    let icon = image::load_from_memory(include_bytes!("../assets/icon.png"))?.to_rgba8();
    let icon = eframe::egui::IconData {
        width: icon.width(),
        height: icon.height(),
        rgba: icon.into_raw(),
    };
    let saved =
        pcl_core::config::load_settings(&pcl_core::config::settings_path()).unwrap_or_default();
    let window = saved.launcher_window.bounded(None);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("PCL Rust")
            .with_icon(icon)
            .with_inner_size([window.width, window.height])
            .with_min_inner_size([810.0, 470.0])
            .with_transparent(true)
            .with_decorations(false),
        ..Default::default()
    };
    let splash_enabled = saved.ui_launcher_logo;
    if let Err(error) = startup_splash::show(splash_enabled) {
        eprintln!("启动画面无法显示：{error:#}");
    }
    let result = eframe::run_native(
        "PCL Rust",
        options,
        Box::new(|cc| Ok(Box::new(app::Launcher::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("窗口启动失败：{e}"));
    startup_splash::close();
    result
}
