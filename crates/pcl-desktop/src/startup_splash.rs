//! A native, non-interactive image shown before eframe constructs the main window.
//! It lives on the GUI thread, fades for 400 ms after the first real frame, and
//! releases its own resources. It never delays launch or drives task progress.
use anyhow::Result;
use std::{cell::RefCell, time::Instant};

thread_local! {
    static SPLASH: RefCell<Option<(native::Splash, Option<Instant>)>> = const { RefCell::new(None) };
}
static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn show(enabled: bool) -> Result<bool> {
    if !enabled {
        return Ok(false);
    }
    let Some(splash) = native::Splash::new(include_bytes!("../assets/icon.png"))? else {
        return Ok(false);
    };
    SPLASH.with(|slot| *slot.borrow_mut() = Some((splash, None)));
    SHOWN.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(true)
}
pub(crate) fn was_shown() -> bool {
    SHOWN.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn tick(ctx: &eframe::egui::Context) -> Result<()> {
    SPLASH.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some((window, started)) = slot.as_mut() {
            let age = started
                .get_or_insert_with(Instant::now)
                .elapsed()
                .as_secs_f32()
                * crate::theme::animation_speed(ctx);
            if age >= 0.4 || !crate::theme::animations_enabled(ctx) {
                *slot = None;
            } else {
                if let Err(error) = window.opacity(1.0 - age / 0.4) {
                    *slot = None;
                    return Err(error);
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
            }
        }
        Ok(())
    })
}
pub(crate) fn close() {
    SPLASH.with(|slot| *slot.borrow_mut() = None);
}

#[cfg(target_os = "macos")]
mod native {
    use anyhow::{Context, Result};
    use objc2::{
        class, msg_send,
        rc::{Allocated, Retained},
        runtime::AnyObject,
        MainThreadMarker,
    };
    use objc2_foundation::{NSData, NSPoint, NSRect, NSSize};
    pub(super) struct Splash {
        window: Retained<AnyObject>,
    }
    impl Splash {
        pub(super) fn new(bytes: &[u8]) -> Result<Option<Self>> {
            anyhow::ensure!(
                MainThreadMarker::new().is_some(),
                "启动图标必须在 GUI 主线程创建"
            );
            let data = NSData::with_bytes(bytes);
            unsafe {
                let _: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
                let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(220.0, 220.0));
                let allocated: Allocated<AnyObject> = msg_send![class!(NSWindow), alloc];
                let window: Option<Retained<AnyObject>> = msg_send![allocated, initWithContentRect: frame, styleMask: 0usize, backing: 2usize, defer: false];
                let window = window.context("无法创建启动图标窗口")?;
                let _: () = msg_send![&*window,setReleasedWhenClosed:false];
                let _: () = msg_send![&*window,setOpaque:false];
                let _: () = msg_send![&*window,setHasShadow:false];
                let _: () = msg_send![&*window,setIgnoresMouseEvents:true];
                let color: *mut AnyObject = msg_send![class!(NSColor), clearColor];
                let _: () = msg_send![&*window,setBackgroundColor:color];
                let allocated: Allocated<AnyObject> = msg_send![class!(NSImage), alloc];
                let image: Option<Retained<AnyObject>> = msg_send![allocated,initWithData:&*data];
                let image = image.context("启动图标无法解码")?;
                let allocated: Allocated<AnyObject> = msg_send![class!(NSImageView), alloc];
                let view: Option<Retained<AnyObject>> = msg_send![allocated,initWithFrame:frame];
                let view = view.context("无法创建启动图标视图")?;
                let _: () = msg_send![&*view,setImage:&*image];
                let _: () = msg_send![&*view,setImageScaling:3usize];
                let _: () = msg_send![&*window,setContentView:&*view];
                let _: () = msg_send![&*window, center];
                let _: () = msg_send![&*window, orderFrontRegardless];
                let _: () = msg_send![&*window, displayIfNeeded];
                Ok(Some(Self { window }))
            }
        }
        pub(super) fn opacity(&self, value: f32) -> Result<()> {
            unsafe {
                let _: () = msg_send![&*self.window,setAlphaValue:f64::from(value.clamp(0.0,1.0))];
            }
            Ok(())
        }
    }
    impl Drop for Splash {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![&*self.window, close];
            }
        }
    }
}

#[cfg(windows)]
mod native {
    use anyhow::{Context, Result};
    use std::ffi::c_void;
    type Handle = *mut c_void;
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }
    #[repr(C)]
    struct Size {
        cx: i32,
        cy: i32,
    }
    #[repr(C)]
    struct Blend {
        operation: u8,
        flags: u8,
        alpha: u8,
        format: u8,
    }
    #[repr(C)]
    struct BitmapHeader {
        size: u32,
        width: i32,
        height: i32,
        planes: u16,
        bits: u16,
        compression: u32,
        image_size: u32,
        x: i32,
        y: i32,
        used: u32,
        important: u32,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn CreateWindowExW(
            ex: u32,
            class: *const u16,
            title: *const u16,
            style: u32,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            parent: Handle,
            menu: Handle,
            instance: Handle,
            param: Handle,
        ) -> Handle;
        fn DestroyWindow(window: Handle) -> i32;
        fn ShowWindow(window: Handle, command: i32) -> i32;
        fn GetSystemMetrics(index: i32) -> i32;
        fn UpdateLayeredWindow(
            window: Handle,
            destination_dc: Handle,
            destination: *const Point,
            size: *const Size,
            source_dc: Handle,
            source: *const Point,
            key: u32,
            blend: *const Blend,
            flags: u32,
        ) -> i32;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateCompatibleDC(dc: Handle) -> Handle;
        fn DeleteDC(dc: Handle) -> i32;
        fn CreateDIBSection(
            dc: Handle,
            info: *const BitmapHeader,
            usage: u32,
            bits: *mut Handle,
            section: Handle,
            offset: u32,
        ) -> Handle;
        fn SelectObject(dc: Handle, object: Handle) -> Handle;
        fn DeleteObject(object: Handle) -> i32;
    }
    pub(super) struct Splash {
        window: Handle,
        dc: Handle,
        bitmap: Handle,
        old: Handle,
        point: Point,
        size: Size,
    }
    impl Splash {
        pub(super) fn new(bytes: &[u8]) -> Result<Option<Self>> {
            let image = image::load_from_memory(bytes)
                .context("启动图标无法解码")?
                .resize_exact(220, 220, image::imageops::FilterType::Lanczos3)
                .to_rgba8();
            unsafe {
                let null = std::ptr::null_mut();
                let point = Point {
                    x: (GetSystemMetrics(0) - 220) / 2,
                    y: (GetSystemMetrics(1) - 220) / 2,
                };
                let size = Size { cx: 220, cy: 220 };
                let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
                let title: Vec<u16> = "PCL startup\0".encode_utf16().collect();
                let window = CreateWindowExW(
                    0x0008_00a0 | 0x0800_0000,
                    class.as_ptr(),
                    title.as_ptr(),
                    0x8000_0000,
                    point.x,
                    point.y,
                    220,
                    220,
                    null,
                    null,
                    null,
                    null,
                );
                if window.is_null() {
                    return Err(std::io::Error::last_os_error()).context("无法创建启动图标窗口");
                }
                let dc = CreateCompatibleDC(null);
                if dc.is_null() {
                    DestroyWindow(window);
                    return Err(std::io::Error::last_os_error()).context("无法创建启动图标画布");
                }
                let header = BitmapHeader {
                    size: std::mem::size_of::<BitmapHeader>() as u32,
                    width: 220,
                    height: -220,
                    planes: 1,
                    bits: 32,
                    compression: 0,
                    image_size: 220 * 220 * 4,
                    x: 0,
                    y: 0,
                    used: 0,
                    important: 0,
                };
                let mut bits = null;
                let bitmap = CreateDIBSection(dc, &header, 0, &mut bits, null, 0);
                if bitmap.is_null() || bits.is_null() {
                    DeleteDC(dc);
                    DestroyWindow(window);
                    return Err(std::io::Error::last_os_error()).context("无法创建启动图标位图");
                }
                let out = std::slice::from_raw_parts_mut(bits.cast::<u8>(), 220 * 220 * 4);
                for (to, from) in out.chunks_exact_mut(4).zip(image.as_raw().chunks_exact(4)) {
                    let a = u16::from(from[3]);
                    to[0] = (u16::from(from[2]) * a / 255) as u8;
                    to[1] = (u16::from(from[1]) * a / 255) as u8;
                    to[2] = (u16::from(from[0]) * a / 255) as u8;
                    to[3] = from[3];
                }
                let old = SelectObject(dc, bitmap);
                if old.is_null() || old as isize == -1 {
                    DeleteObject(bitmap);
                    DeleteDC(dc);
                    DestroyWindow(window);
                    return Err(std::io::Error::last_os_error()).context("无法选择启动图标位图");
                }
                let splash = Self {
                    window,
                    dc,
                    bitmap,
                    old,
                    point,
                    size,
                };
                splash.paint(255)?;
                ShowWindow(window, 8);
                Ok(Some(splash))
            }
        }
        fn paint(&self, alpha: u8) -> Result<()> {
            unsafe {
                let origin = Point { x: 0, y: 0 };
                let blend = Blend {
                    operation: 0,
                    flags: 0,
                    alpha,
                    format: 1,
                };
                if UpdateLayeredWindow(
                    self.window,
                    std::ptr::null_mut(),
                    &self.point,
                    &self.size,
                    self.dc,
                    &origin,
                    0,
                    &blend,
                    2,
                ) == 0
                {
                    return Err(std::io::Error::last_os_error()).context("绘制启动图标失败");
                }
                Ok(())
            }
        }
        pub(super) fn opacity(&self, value: f32) -> Result<()> {
            self.paint((value.clamp(0.0, 1.0) * 255.0).round() as u8)
        }
    }
    impl Drop for Splash {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.dc, self.old);
                DeleteObject(self.bitmap);
                DeleteDC(self.dc);
                DestroyWindow(self.window);
            }
        }
    }
}
#[cfg(not(any(target_os = "macos", windows)))]
mod native {
    use anyhow::Result;
    pub(super) struct Splash;
    impl Splash {
        pub(super) fn new(_: &[u8]) -> Result<Option<Self>> {
            Ok(None)
        }
        pub(super) fn opacity(&self, _: f32) -> Result<()> {
            Ok(())
        }
    }
}
