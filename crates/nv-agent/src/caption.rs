//! The live transcript: a small caption above the bubble showing what is being
//! heard, as it is said.
//!
//! It is a window of its own. The bubble writes its pixels by hand at a fixed
//! diameter, and a line of text wants a surface shaped for text — so this is a
//! layered pill, drawn with GDI, shown only while there is something to say.
//!
//! `create` runs on the thread that pumps messages (the one the bubble and tray
//! already use), because a window's messages are delivered there. The listener
//! only ever calls `set`, which is safe from any thread.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::OnceLock;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteObject, DrawTextW, GetDC, ReleaseDC,
    SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CLEARTYPE_QUALITY,
    DIB_RGB_COLORS, DT_CENTER, DT_END_ELLIPSIS, DT_SINGLELINE, DT_VCENTER, FW_SEMIBOLD, HDC, HGDIOBJ, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetSystemMetrics, RegisterClassW, ShowWindow,
    UpdateLayeredWindow, SW_HIDE, SW_SHOWNOACTIVATE, SM_CXSCREEN, SM_CYSCREEN, ULW_ALPHA, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// Logical size at 96 dpi.
const WIDTH: i32 = 620;
const HEIGHT: i32 = 54;

/// The latest thing heard. Written by the listener, read by the window.
static TEXT: OnceLock<std::sync::Mutex<String>> = OnceLock::new();
static HWND_ATOM: AtomicIsize = AtomicIsize::new(0);

fn text() -> &'static std::sync::Mutex<String> {
    TEXT.get_or_init(|| std::sync::Mutex::new(String::new()))
}

/// Show this line above the bubble. Empty hides the caption.
pub fn set(heard: &str) {
    let trimmed = heard.trim();
    if let Ok(mut t) = text().lock() {
        if t.as_str() == trimmed {
            return;
        }
        t.clear();
        t.push_str(trimmed);
    }
}

/// Hide it and forget the line.
pub fn clear() {
    set("");
}

/// Start the caption: it makes its own window on its own thread and repaints
/// when there is something new to show. Nothing here relies on window messages,
/// because only the thread that pumps them would ever receive them.
pub fn create(bottom_margin: u32) -> windows::core::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("caption".into())
        .spawn(move || {
            unsafe {
                let Ok(hinst) = GetModuleHandleW(None) else { return };
                let class = w!("NeedleVoiceCaption");
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(wndproc),
                    hInstance: hinst.into(),
                    lpszClassName: class,
                    ..Default::default()
                };
                RegisterClassW(&wc);
                let Ok(hwnd) = CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    class,
                    w!("NeedleVoiceCaption"),
                    WS_POPUP,
                    0,
                    0,
                    1,
                    1,
                    None,
                    None,
                    Some(hinst.into()),
                    None,
                ) else {
                    return;
                };
                HWND_ATOM.store(hwnd.0 as isize, Ordering::SeqCst);
                let mut caption = Caption {
                    hwnd,
                    mem_dc: HDC::default(),
                    bmp: Default::default(),
                    old: HGDIOBJ::default(),
                    bits: std::ptr::null_mut(),
                    scale: 1.0,
                    bottom_margin: bottom_margin as i32,
                };
                caption.make_surface();
                log::info!("caption window ready");
                let _ = tx.send(());
                let mut shown: Option<String> = None;
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(60));
                    let line = text().lock().map(|t| t.clone()).unwrap_or_default();
                    if shown.as_deref() == Some(line.as_str()) {
                        continue;
                    }
                    shown = Some(line.clone());
                    if line.is_empty() {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    } else {
                        caption.redraw(&line);
                        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    }
                }
            }
        })
        .ok();
    // Wait until the window exists, so a caller can show text immediately.
    let _ = rx.recv_timeout(std::time::Duration::from_secs(3));
    Ok(())
}

struct Caption {
    hwnd: HWND,
    mem_dc: HDC,
    bmp: windows::Win32::Graphics::Gdi::HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    scale: f32,
    bottom_margin: i32,
}

impl Caption {
    fn make_surface(&mut self) {
        let scale = dpi_scale();
        self.scale = scale;
        let w = (WIDTH as f32 * scale) as i32;
        let h = (HEIGHT as f32 * scale) as i32;
        unsafe {
            let screen = GetDC(None);
            self.mem_dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            if let Ok(bmp) = CreateDIBSection(Some(self.mem_dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
                self.bmp = bmp;
                self.bits = bits.cast();
                self.old = SelectObject(self.mem_dc, bmp.into());
            }
        }
    }

    /// Paint the pill and the line, then hand the surface to the compositor.
    fn redraw(&mut self, line: &str) {
        let scale = self.scale;
        let w = (WIDTH as f32 * scale) as i32;
        let h = (HEIGHT as f32 * scale) as i32;
        if self.bits.is_null() {
            return;
        }
        let buf = unsafe { std::slice::from_raw_parts_mut(self.bits, (w * h) as usize) };
        buf.fill(0);
        // Rounded pill, drawn by hand: premultiplied ARGB, so the numbers below
        // are (alpha, colour times alpha).
        let radius = 15.0 * scale;
        let pad = 2.0 * scale;
        for y in 0..h as usize {
            for x in 0..w as usize {
                let fx = x as f32 + 0.5;
                let fy = y as f32 + 0.5;
                let dx = (pad + radius - fx).max(fx - (w as f32 - pad - radius)).max(0.0);
                let dy = (pad + radius - fy).max(fy - (h as f32 - pad - radius)).max(0.0);
                let dist = (dx * dx + dy * dy).sqrt();
                let alpha = if dist <= radius { 240.0 } else { (240.0 * (radius - dist).max(0.0)).min(240.0) };
                if alpha < 2.0 {
                    continue;
                }
                // A brighter ring at the edge, so the pill reads as a panel
                // instead of vanishing into a dark screen.
                let edge = (radius - dist).abs() < 1.6;
                let (r, g, b) = if edge { (0u32, 190u32, 90u32) } else { (30u32, 34u32, 42u32) };
                let a = alpha as u32;
                buf[y * w as usize + x] =
                    (a << 24) | (((r * a) / 255) << 16) | (((g * a) / 255) << 8) | ((b * a) / 255);
            }
        }

        unsafe {
            let font = CreateFontW(
                -(21.0 * scale) as i32,
                0,
                0,
                0,
                FW_SEMIBOLD.0 as i32,
                0,
                0,
                0,
                Default::default(),
                Default::default(),
                Default::default(),
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            );
            let old_font = SelectObject(self.mem_dc, font.into());
            SetBkMode(self.mem_dc, TRANSPARENT);
            SetTextColor(self.mem_dc, COLORREF(0x00F2F5F7));
            let mut wide: Vec<u16> = line.encode_utf16().collect();
            let mut rect = RECT {
                left: (20.0 * scale) as i32,
                top: 0,
                right: w - (20.0 * scale) as i32,
                bottom: h,
            };
            DrawTextW(
                self.mem_dc,
                &mut wide,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );
            SelectObject(self.mem_dc, old_font);
            let _ = DeleteObject(font.into());

            let x = (GetSystemMetrics(SM_CXSCREEN) - w) / 2;
            let y = GetSystemMetrics(SM_CYSCREEN) - h - self.bottom_margin;
            let dst = POINT { x, y };
            let size = SIZE { cx: w, cy: h };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: 1 };
            let screen = GetDC(None);
            let _ = UpdateLayeredWindow(
                self.hwnd,
                Some(screen),
                Some(&dst),
                Some(&size),
                Some(self.mem_dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            ReleaseDC(None, screen);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // The repainting happens on the caption's own thread, so there is nothing to
    // do here but the default.
    let _ = (hwnd, msg, wp, lp);
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// Screen scale, so the caption reads at the same size as everything else.
fn dpi_scale() -> f32 {
    unsafe {
        let h = HWND_ATOM.load(Ordering::SeqCst);
        let dpi = if h != 0 {
            let d = GetDpiForWindow(HWND(h as *mut _));
            if d > 0 {
                d
            } else {
                GetDpiForSystem()
            }
        } else {
            GetDpiForSystem()
        };
        if dpi == 0 {
            1.0
        } else {
            dpi as f32 / 96.0
        }
    }
}

// The caption window lives for the life of the process; the operating system
// reclaims everything it holds, so there is nothing to tear down. An earlier
// `destroy()` here did nothing but clear the text, and nothing called it.
