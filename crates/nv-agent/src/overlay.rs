//! The floating bubble window: a click-through, always-on-top, per-pixel-alpha
//! layered window at the bottom centre of the screen the cursor is on.
//! It only animates (and only uses CPU) while visible.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tiny_skia::Pixmap;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW, MonitorFromPoint,
    ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::bubble::{self, Frame, Mode};

pub const WM_NV_STATE: u32 = WM_APP + 10;
const TIMER_ID: usize = 1;

/// State shared between the listener thread and the overlay.
pub struct Shared {
    mode: AtomicU8,
    level: AtomicU32,
    hwnd: AtomicIsize,
    speaking: AtomicBool,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            mode: AtomicU8::new(0),
            level: AtomicU32::new(0),
            hwnd: AtomicIsize::new(0),
            speaking: AtomicBool::new(false),
        })
    }

    pub fn set_speaking(&self, s: bool) {
        self.speaking.store(s, Ordering::SeqCst);
    }

    pub fn speaking(&self) -> bool {
        self.speaking.load(Ordering::SeqCst)
    }

    pub fn set_mode(&self, m: Mode) {
        self.mode.store(m as u8, Ordering::SeqCst);
        let h = self.hwnd.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_NV_STATE, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub fn mode(&self) -> Mode {
        Mode::from_u8(self.mode.load(Ordering::SeqCst))
    }

    pub fn set_level(&self, l: f32) {
        self.level.store(l.to_bits(), Ordering::Relaxed);
    }

    fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
}

struct Overlay {
    hwnd: HWND,
    shared: Arc<Shared>,
    enabled: bool,
    diameter: u32,
    margin: u32,
    accent: (f32, f32, f32),
    // animation
    shown_mode: Mode,
    visible: bool,
    born: Instant,
    mode_since: Instant,
    hiding_since: Option<Instant>,
    level: f32,
    // drawing surface
    px: u32,
    dpi_scale: f32,
    pix: Option<Pixmap>,
    mem_dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
}

static mut OVERLAY: Option<Box<Overlay>> = None;

#[allow(static_mut_refs)]
fn ov() -> Option<&'static mut Overlay> {
    unsafe { OVERLAY.as_deref_mut() }
}

#[allow(static_mut_refs)]
pub fn create(shared: Arc<Shared>, enabled: bool, diameter: u32, margin: u32, accent: (f32, f32, f32)) -> windows::core::Result<HWND> {
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("NeedleVoiceOverlay");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("NeedleVoice"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        shared.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
        // A second overlay would leak the first one's window, timer and GDI
        // surface, and leave its timer firing into a replaced static.
        if let Some(mut previous) = OVERLAY.take() {
            previous.free_surface();
            let _ = DestroyWindow(previous.hwnd);
        }
        OVERLAY = Some(Box::new(Overlay {
            hwnd,
            shared,
            enabled,
            diameter,
            margin,
            accent,
            shown_mode: Mode::Hidden,
            visible: false,
            born: Instant::now(),
            mode_since: Instant::now(),
            hiding_since: None,
            level: 0.0,
            px: 0,
            dpi_scale: 1.0,
            pix: None,
            mem_dc: HDC::default(),
            bmp: HBITMAP::default(),
            old: HGDIOBJ::default(),
            bits: std::ptr::null_mut(),
        }));
        Ok(hwnd)
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NV_STATE => {
            if let Some(o) = ov() {
                o.on_state();
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_ID => {
            if let Some(o) = ov() {
                o.tick();
            }
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

impl Overlay {
    fn on_state(&mut self) {
        let m = self.shared.mode();
        if !self.enabled {
            return;
        }
        if m == Mode::Hidden {
            if self.visible && self.hiding_since.is_none() {
                self.hiding_since = Some(Instant::now());
            }
            return;
        }
        if m != self.shown_mode {
            self.mode_since = Instant::now();
        }
        self.shown_mode = m;
        self.hiding_since = None;
        if !self.visible {
            self.show();
        }
    }

    fn show(&mut self) {
        unsafe {
            // Follow the user: bottom centre of the monitor the cursor is on.
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            self.dpi_scale = dx as f32 / 96.0;
            let px = ((self.diameter as f32 * self.dpi_scale) * 2.2) as u32;
            self.ensure_surface(px);
            let work = mi.rcWork;
            let x = work.left + (work.right - work.left - px as i32) / 2;
            let orb_px = (self.diameter as f32 * self.dpi_scale) as i32;
            // Glow extends below the orb; keep the orb itself `margin` above the taskbar.
            let y = work.bottom - (self.margin as f32 * self.dpi_scale) as i32 - orb_px - (px as i32 - orb_px) / 2;
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, px as i32, px as i32, SWP_NOACTIVATE);
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        self.visible = true;
        self.born = Instant::now();
        self.mode_since = Instant::now();
        self.level = 0.0;
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_ID, 16, None);
        }
        self.tick();
    }

    fn hide_now(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_ID);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.visible = false;
        self.hiding_since = None;
        self.shown_mode = Mode::Hidden;
        // Give the memory back while hidden.
        self.free_surface();
    }

    fn ensure_surface(&mut self, px: u32) {
        if self.px == px && self.pix.is_some() {
            return;
        }
        self.free_surface();
        unsafe {
            let screen = GetDC(None);
            self.mem_dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: px as i32,
                    biHeight: -(px as i32), // top-down
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
        self.px = px;
        self.pix = Pixmap::new(px, px);
    }

    fn free_surface(&mut self) {
        unsafe {
            // Only when the device context is real: the old code selected the
            // bitmap into a null DC on the way to releasing one that failed.
            if !self.mem_dc.is_invalid() {
                SelectObject(self.mem_dc, self.old);
                if !self.bmp.is_invalid() {
                    let _ = DeleteObject(self.bmp.into());
                }
                let _ = DeleteDC(self.mem_dc);
            }
        }
        self.mem_dc = HDC::default();
        self.bmp = HBITMAP::default();
        self.bits = std::ptr::null_mut();
        self.pix = None;
        self.px = 0;
    }

    fn tick(&mut self) {
        if !self.visible {
            return;
        }
        let now = Instant::now();
        let speaking = self.shared.speaking();
        // Success / error flash, stay while the reply is spoken, then fade out.
        if matches!(self.shown_mode, Mode::Success | Mode::Error)
            && self.hiding_since.is_none()
            && !speaking
            && now.duration_since(self.mode_since).as_millis() > 650
        {
            self.hiding_since = Some(now);
        }
        let mut opacity = 1.0;
        if let Some(h) = self.hiding_since {
            let f = now.duration_since(h).as_secs_f32() / 0.28;
            if f >= 1.0 {
                self.hide_now();
                return;
            }
            opacity = 1.0 - f;
        }
        let age = now.duration_since(self.born).as_secs_f32();
        let appear = (age / 0.32).min(1.0);
        opacity *= (age / 0.15).min(1.0);

        // Fast attack, slow release so the orb "breathes" with speech.
        let target = if self.shown_mode == Mode::Listening || speaking { self.shared.level() } else { 0.0 };
        let k = if target > self.level { 0.45 } else { 0.12 };
        self.level += (target - self.level) * k;

        let frame = Frame {
            t: age,
            mode_t: now.duration_since(self.mode_since).as_secs_f32(),
            level: self.level,
            mode: self.shown_mode,
            opacity,
            scale: ease_out_back(appear),
            accent: self.accent,
            speaking,
        };
        let diameter = self.diameter as f32 * self.dpi_scale;
        let Some(pix) = self.pix.as_mut() else { return };
        bubble::render(pix, &frame, diameter);
        self.present();
    }

    fn present(&mut self) {
        if self.bits.is_null() {
            return;
        }
        let Some(pix) = self.pix.as_ref() else { return };
        // tiny-skia is premultiplied RGBA; GDI wants premultiplied BGRA. Chunked
        // so each pixel arrives as one array with a single bounds check instead
        // of four — this copies about a megabyte, sixty times a second, for as
        // long as the bubble is on screen.
        let src = pix.data();
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, src.len()) };
        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
        }
        unsafe {
            let size = SIZE { cx: self.px as i32, cy: self.px as i32 };
            let src_pt = POINT::default();
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                None,
                Some(&size),
                Some(self.mem_dc),
                Some(&src_pt),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }
}

