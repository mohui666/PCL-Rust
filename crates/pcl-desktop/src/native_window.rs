//! Opacity of this launcher's own window, using its lifetime-bound native handle.
use anyhow::{bail, Context, Result};
use raw_window_handle::HasWindowHandle;
#[cfg(any(target_os = "macos", windows))]
use raw_window_handle::RawWindowHandle;

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

/// Only the PID returned by this launch's Child is accepted by the caller.
/// No permission prompt, privilege escalation or other application is touched.
pub(crate) fn game_window(pid: u32, title: Option<&str>, maximize: bool) -> Result<bool> {
    anyhow::ensure!(pid > 0 && pid != std::process::id(), "游戏窗口 PID 无效");
    #[cfg(windows)]
    {
        windows_game_window(pid, title, maximize)
    }
    #[cfg(target_os = "macos")]
    {
        mac_game_window::apply(pid, title, maximize)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (title, maximize);
        bail!("此系统暂不支持游戏窗口控制")
    }
}

/// Release reclaimable memory of THIS launcher only; this is not a system-wide
/// memory purge and promises neither a minimum byte count nor a speedup.
pub(crate) fn reclaim_launcher_memory() -> Result<Option<usize>> {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        }
        Ok(Some(unsafe {
            malloc_zone_pressure_relief(std::ptr::null_mut(), 0)
        }))
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCurrentProcess() -> *mut std::ffi::c_void;
        }
        #[link(name = "psapi")]
        unsafe extern "system" {
            fn EmptyWorkingSet(process: *mut std::ffi::c_void) -> i32;
        }
        anyhow::ensure!(
            unsafe { EmptyWorkingSet(GetCurrentProcess()) } != 0,
            "回收启动器工作集失败：{}",
            std::io::Error::last_os_error()
        );
        Ok(None)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        bail!("此系统没有已接入的启动器内存回收接口")
    }
}

#[cfg(windows)]
fn windows_game_window(pid: u32, title: Option<&str>, maximize: bool) -> Result<bool> {
    use std::ffi::c_void;
    type Hwnd = *mut c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumWindows(callback: unsafe extern "system" fn(Hwnd, isize) -> i32, data: isize)
            -> i32;
        fn GetWindowThreadProcessId(hwnd: Hwnd, pid: *mut u32) -> u32;
        fn IsWindowVisible(hwnd: Hwnd) -> i32;
        fn GetWindowTextW(hwnd: Hwnd, text: *mut u16, size: i32) -> i32;
        fn SendMessageTimeoutW(
            hwnd: Hwnd,
            message: u32,
            w: usize,
            l: isize,
            flags: u32,
            timeout: u32,
            result: *mut usize,
        ) -> isize;
        fn ShowWindowAsync(hwnd: Hwnd, command: i32) -> i32;
    }
    struct Search {
        pid: u32,
        window: Hwnd,
    }
    unsafe extern "system" fn inspect(hwnd: Hwnd, data: isize) -> i32 {
        let search = unsafe { &mut *(data as *mut Search) };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid != search.pid || unsafe { IsWindowVisible(hwnd) } == 0 {
            return 1;
        }
        let mut buffer = [0u16; 2048];
        let n = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
        let title = String::from_utf16_lossy(&buffer[..n.max(0) as usize]);
        if title.is_empty() || title.starts_with("FML") || title.starts_with("Quilt Loader") {
            return 1;
        }
        search.window = hwnd;
        0
    }
    let mut search = Search {
        pid,
        window: std::ptr::null_mut(),
    };
    unsafe { EnumWindows(inspect, &mut search as *mut _ as isize) };
    if search.window.is_null() {
        return Ok(false);
    }
    // Recheck ownership after enumeration before sending any mutation.
    let mut owner = 0;
    unsafe { GetWindowThreadProcessId(search.window, &mut owner) };
    if owner != pid {
        return Ok(false);
    }
    if let Some(title) = title {
        anyhow::ensure!(!title.contains('\0'), "游戏窗口标题不能包含 NUL");
        let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let mut result = 0;
        anyhow::ensure!(
            unsafe {
                SendMessageTimeoutW(
                    search.window,
                    0x000C,
                    0,
                    title.as_ptr() as isize,
                    2,
                    300,
                    &mut result,
                )
            } != 0
                && result != 0,
            "游戏窗口未接受标题设置（可能无响应或权限不足）"
        );
    }
    if maximize {
        anyhow::ensure!(
            unsafe { ShowWindowAsync(search.window, 3) } != 0,
            "请求游戏窗口最大化失败：{}",
            std::io::Error::last_os_error()
        );
    }
    Ok(true)
}

#[cfg(target_os = "macos")]
mod mac_game_window {
    use anyhow::{Context, Result};
    use std::ffi::{c_char, c_void};
    type Ref = *const c_void;
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXUIElementCreateApplication(pid: i32) -> Ref;
        fn AXUIElementSetMessagingTimeout(element: Ref, timeout: f32) -> i32;
        fn AXUIElementCopyAttributeValue(element: Ref, name: Ref, value: *mut Ref) -> i32;
        fn AXUIElementIsAttributeSettable(element: Ref, name: Ref, settable: *mut u8) -> i32;
        fn AXUIElementSetAttributeValue(element: Ref, name: Ref, value: Ref) -> i32;
        fn AXUIElementPerformAction(element: Ref, name: Ref) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            allocator: Ref,
            bytes: *const u8,
            length: isize,
            encoding: u32,
            external: u8,
        ) -> Ref;
        fn CFStringGetCString(value: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFArrayGetCount(array: Ref) -> isize;
        fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
        fn CFGetTypeID(value: Ref) -> usize;
        fn CFArrayGetTypeID() -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFRelease(value: Ref);
    }
    struct Owned(Ref);
    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) }
            }
        }
    }
    fn string(value: &str) -> Result<Owned> {
        let pointer = unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                value.as_ptr(),
                value.len() as isize,
                0x08000100,
                0,
            )
        };
        anyhow::ensure!(!pointer.is_null(), "分配系统窗口属性失败");
        Ok(Owned(pointer))
    }
    fn bounded(element: Ref) -> Result<()> {
        // Timeout is per AX object, not inherited by its children. Never change
        // the system-wide AX timeout or prompt the user for permission here.
        anyhow::ensure!(
            unsafe { AXUIElementSetMessagingTimeout(element, 0.25) } == 0,
            "无法限制游戏窗口控制的响应等待时间"
        );
        Ok(())
    }
    fn attribute(object: Ref, name: &str) -> Result<Option<Owned>> {
        bounded(object)?;
        let name = string(name)?;
        let mut output = std::ptr::null();
        let result = unsafe { AXUIElementCopyAttributeValue(object, name.0, &mut output) };
        if matches!(result, -25205 | -25212) {
            return Ok(None);
        }
        anyhow::ensure!(result == 0, "读取游戏窗口属性失败：AXError {result}");
        Ok((!output.is_null()).then_some(Owned(output)))
    }
    fn text(value: &Owned) -> String {
        if unsafe { CFGetTypeID(value.0) != CFStringGetTypeID() } {
            return String::new();
        }
        let mut bytes = vec![0i8; 8192];
        if unsafe {
            CFStringGetCString(
                value.0,
                bytes.as_mut_ptr(),
                bytes.len() as isize,
                0x08000100,
            )
        } == 0
        {
            return String::new();
        }
        unsafe { std::ffi::CStr::from_ptr(bytes.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }
    pub fn apply(pid: u32, title: Option<&str>, maximize: bool) -> Result<bool> {
        anyhow::ensure!(unsafe{AXIsProcessTrusted()},"macOS 未授予启动器辅助功能权限，不能控制该游戏窗口；可在系统设置→隐私与安全性→辅助功能手动授权。本次继续启动，未修改权限。");
        let app = Owned(unsafe { AXUIElementCreateApplication(pid as i32) });
        anyhow::ensure!(!app.0.is_null(), "无法访问本次游戏进程");
        let Some(windows) = attribute(app.0, "AXWindows")? else {
            return Ok(false);
        };
        anyhow::ensure!(
            unsafe { CFGetTypeID(windows.0) == CFArrayGetTypeID() },
            "游戏窗口列表格式无效"
        );
        let count = unsafe { CFArrayGetCount(windows.0) }.min(128);
        for index in 0..count {
            let window = unsafe { CFArrayGetValueAtIndex(windows.0, index) };
            if window.is_null() {
                continue;
            }
            let name = attribute(window, "AXTitle")?
                .as_ref()
                .map(text)
                .unwrap_or_default();
            if name.is_empty() || name.starts_with("FML") || name.starts_with("Quilt Loader") {
                continue;
            }
            if let Some(title) = title {
                let attribute_name = string("AXTitle")?;
                let mut settable = 0;
                anyhow::ensure!(
                    unsafe {
                        AXUIElementIsAttributeSettable(window, attribute_name.0, &mut settable)
                    } == 0
                        && settable != 0,
                    "macOS 或该游戏窗口不允许外部修改标题；未改变窗口"
                );
                let title = string(title)?;
                let result =
                    unsafe { AXUIElementSetAttributeValue(window, attribute_name.0, title.0) };
                anyhow::ensure!(result == 0, "设置游戏窗口标题失败：AXError {result}");
            }
            if maximize {
                let button =
                    attribute(window, "AXZoomButton")?.context("该游戏窗口没有可用的最大化控件")?;
                bounded(button.0)?;
                let action = string("AXPress")?;
                let result = unsafe { AXUIElementPerformAction(button.0, action.0) };
                anyhow::ensure!(result == 0, "请求游戏窗口最大化失败：AXError {result}");
            }
            return Ok(true);
        }
        Ok(false)
    }
}

/// Same per-application HKCU preference as PCL, retained only until this game
/// has initialized graphics (or exits). Never writes system/global GPU settings.
pub(crate) struct GpuPreference {
    #[cfg(windows)]
    state: Option<gpu_registry::State>,
}
impl GpuPreference {
    pub(crate) fn request(java: &std::path::Path) -> Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self {
                state: Some(gpu_registry::State::set(java)?),
            })
        }
        #[cfg(not(windows))]
        {
            let _ = java;
            bail!("此平台由系统管理 GPU；没有修改全局显卡设置")
        }
    }
    pub(crate) fn restore(&mut self) -> Result<()> {
        #[cfg(windows)]
        {
            if let Some(state) = &self.state {
                state.restore()?;
                self.state = None;
            }
        }
        Ok(())
    }
}
impl Drop for GpuPreference {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(any(windows, test))]
fn registry_executable_name(path: &[u16]) -> Vec<u16> {
    // Windows canonicalize returns the verbatim namespace, while DirectX's
    // per-application preference keys use ordinary DOS/UNC paths.
    let mut out = if let Some(rest) = path.strip_prefix(&[92, 92, 63, 92]) {
        if let Some(unc) = rest.strip_prefix(&[85, 78, 67, 92]) {
            let mut out = vec![92, 92];
            out.extend_from_slice(unc);
            out
        } else {
            rest.to_vec()
        }
    } else {
        path.to_vec()
    };
    out.push(0);
    out
}
#[cfg(test)]
mod tests {
    #[test]
    fn gpu_registry_path_preserves_unicode_and_normalizes_only_namespace_prefix() {
        for (input, expected) in [
            (r"\\?\C:\Java 测试\javaw.exe", r"C:\Java 测试\javaw.exe"),
            (r"\\?\UNC\server\share\java.exe", r"\\server\share\java.exe"),
            (r"C:\plain\java.exe", r"C:\plain\java.exe"),
        ] {
            let path: Vec<_> = input.encode_utf16().collect();
            let output = super::registry_executable_name(&path);
            assert_eq!(
                String::from_utf16(&output[..output.len() - 1]).unwrap(),
                expected
            );
            assert_eq!(output.last(), Some(&0));
        }
    }
}

#[cfg(windows)]
mod gpu_registry {
    use anyhow::{Context, Result};
    use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::Path};
    type Key = *mut c_void;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegCreateKeyExW(
            key: Key,
            subkey: *const u16,
            reserved: u32,
            class: *mut u16,
            options: u32,
            access: u32,
            security: *const c_void,
            out: *mut Key,
            disposition: *mut u32,
        ) -> i32;
        fn RegQueryValueExW(
            key: Key,
            name: *const u16,
            reserved: *mut u32,
            kind: *mut u32,
            data: *mut u8,
            size: *mut u32,
        ) -> i32;
        fn RegSetValueExW(
            key: Key,
            name: *const u16,
            reserved: u32,
            kind: u32,
            data: *const u8,
            size: u32,
        ) -> i32;
        fn RegDeleteValueW(key: Key, name: *const u16) -> i32;
        fn RegCloseKey(key: Key) -> i32;
    }
    struct Open(Key);
    impl Drop for Open {
        fn drop(&mut self) {
            unsafe { RegCloseKey(self.0) };
        }
    }
    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }
    fn open() -> Result<Open> {
        let mut key = std::ptr::null_mut();
        let path = wide("Software\\Microsoft\\DirectX\\UserGpuPreferences");
        let status = unsafe {
            RegCreateKeyExW(
                (0x80000001u32 as i32 as isize) as Key,
                path.as_ptr(),
                0,
                std::ptr::null_mut(),
                0,
                3,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        anyhow::ensure!(status == 0, "访问当前用户的应用 GPU 偏好失败：{status}");
        Ok(Open(key))
    }
    fn read(key: Key, name: &[u16]) -> Result<Option<(u32, Vec<u8>)>> {
        let mut kind = 0;
        let mut size = 0;
        let status = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                std::ptr::null_mut(),
                &mut size,
            )
        };
        if status == 2 {
            return Ok(None);
        }
        anyhow::ensure!(
            status == 0 && size <= 65536,
            "读取应用 GPU 偏好失败或数据过大：{status}"
        );
        let mut bytes = vec![0; size as usize];
        let result = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                bytes.as_mut_ptr(),
                &mut size,
            )
        };
        anyhow::ensure!(
            result == 0 && size as usize <= bytes.len(),
            "应用 GPU 偏好在读取时变化：{result}"
        );
        bytes.truncate(size as usize);
        Ok(Some((kind, bytes)))
    }
    fn write(key: Key, name: &[u16], kind: u32, bytes: &[u8]) -> Result<()> {
        let status = unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                kind,
                bytes.as_ptr(),
                bytes.len() as u32,
            )
        };
        anyhow::ensure!(status == 0, "写入应用 GPU 偏好失败：{status}");
        Ok(())
    }
    pub(super) struct State {
        name: Vec<u16>,
        previous: Option<(u32, Vec<u8>)>,
        written: Vec<u8>,
    }
    impl State {
        pub fn set(java: &Path) -> Result<Self> {
            let java = java.canonicalize().context("定位所选 Java 失败")?;
            let raw: Vec<u16> = java.as_os_str().encode_wide().collect();
            let name = super::registry_executable_name(&raw);
            let key = open()?;
            let previous = read(key.0, &name)?;
            let written: Vec<u8> = wide("GpuPreference=2;")
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect();
            write(key.0, &name, 1, &written)?;
            Ok(Self {
                name,
                previous,
                written,
            })
        }
        pub fn restore(&self) -> Result<()> {
            let key = open()?;
            if read(key.0, &self.name)?
                .as_ref()
                .is_none_or(|(kind, bytes)| *kind != 1 || bytes != &self.written)
            {
                return Ok(());
            }
            if let Some((kind, bytes)) = &self.previous {
                write(key.0, &self.name, *kind, bytes)
            } else {
                let result = unsafe { RegDeleteValueW(key.0, self.name.as_ptr()) };
                anyhow::ensure!(
                    result == 0 || result == 2,
                    "还原应用 GPU 偏好失败：{result}"
                );
                Ok(())
            }
        }
    }
}
