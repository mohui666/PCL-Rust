//! Opacity of this launcher's own window, using its lifetime-bound native handle.
use anyhow::{bail, Context, Result};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[derive(Default)]
pub(crate) struct WindowOpacity {
    attempted: Option<u16>,
}

impl WindowOpacity {
    pub(crate) fn update(&mut self, frame: &eframe::Frame, percent: u16) -> Result<()> {
        if self.attempted == Some(percent) {
            return Ok(());
        }
        // One report per requested value; do not open an error modal every frame.
        self.attempted = Some(percent);
        if !supported() {
            return Ok(());
        }
        apply(frame, percent)
    }
}

pub(crate) const fn supported() -> bool {
    cfg!(any(target_os = "macos", windows))
}

fn apply(frame: &eframe::Frame, percent: u16) -> Result<()> {
    anyhow::ensure!(
        (40..=100).contains(&percent),
        "窗口不透明度必须介于 40% 和 100% 之间"
    );
    let handle = frame.window_handle().context("无法取得启动器窗口")?;
    match handle.as_raw() {
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(handle) => {
            anyhow::ensure!(
                objc2::MainThreadMarker::new().is_some(),
                "必须在主线程调整窗口"
            );
            // eframe guarantees this pointer is a live NSView for the borrowed Frame.
            // We call only AppKit APIs on its owning main thread and retain its window
            // only for this synchronous operation.
            let view = unsafe { handle.ns_view.cast::<objc2_app_kit::NSView>().as_ref() };
            let window = view.window().context("启动器视图尚未关联窗口")?;
            window.setAlphaValue(f64::from(percent) / 100.0);
            Ok(())
        }
        #[cfg(windows)]
        RawWindowHandle::Win32(handle) => windows_opacity(handle.hwnd.get(), percent),
        _ => bail!("当前窗口系统不支持整体不透明度"),
    }
}

#[cfg(windows)]
fn windows_opacity(hwnd: isize, percent: u16) -> Result<()> {
    use std::ffi::c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetWindowLongPtrW(hwnd: *mut c_void, index: i32) -> isize;
        fn SetWindowLongPtrW(hwnd: *mut c_void, index: i32, value: isize) -> isize;
        fn SetLayeredWindowAttributes(hwnd: *mut c_void, color: u32, alpha: u8, flags: u32) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetLastError(error: u32);
        fn GetLastError() -> u32;
    }
    const EXSTYLE: i32 = -20;
    const LAYERED: isize = 0x0008_0000;
    let hwnd = hwnd as *mut c_void;
    // The handle belongs to eframe's current window, used on the GUI thread only.
    unsafe {
        SetLastError(0);
        let previous = GetWindowLongPtrW(hwnd, EXSTYLE);
        if previous == 0 && GetLastError() != 0 {
            return Err(std::io::Error::last_os_error()).context("读取窗口样式失败");
        }
        if previous & LAYERED == 0 {
            SetLastError(0);
            if SetWindowLongPtrW(hwnd, EXSTYLE, previous | LAYERED) == 0 && GetLastError() != 0 {
                return Err(std::io::Error::last_os_error()).context("设置窗口透明样式失败");
            }
        }
        let alpha = ((u32::from(percent) * 255 + 50) / 100) as u8;
        if SetLayeredWindowAttributes(hwnd, 0, alpha, 2) == 0 {
            let error = std::io::Error::last_os_error();
            if previous & LAYERED == 0 {
                SetWindowLongPtrW(hwnd, EXSTYLE, previous);
            }
            return Err(error).context("设置窗口不透明度失败");
        }
    }
    Ok(())
}
