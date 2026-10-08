//! Notification-area icon with a small menu, plus a 2-second timer that
//! notices when the config app saved new settings.

use std::sync::mpsc::Sender;
use std::time::SystemTime;

use nv_core::{win, Config};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_NONE, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::listener::Ctl;

const WM_TRAY: u32 = WM_APP + 1;
const TIMER_CONFIG: usize = 7;
const ID_PAUSE: usize = 100;
const ID_SETTINGS: usize = 101;
const ID_RESCAN: usize = 102;
const ID_QUIT: usize = 103;
const ID_LOG: usize = 104;
const ID_SCHEDULE: usize = 105;

struct Tray {
    hwnd: HWND,
    ctl: Sender<Ctl>,
    cfg: Config,
    cfg_mtime: Option<SystemTime>,
    taskbar_created: u32,
    /// Set when the agent should restart itself to apply new settings.
    restart: bool,
}

static mut TRAY: Option<Box<Tray>> = None;

#[allow(static_mut_refs)]
fn tray() -> Option<&'static mut Tray> {
    unsafe { TRAY.as_deref_mut() }
}

fn mtime() -> Option<SystemTime> {
    std::fs::metadata(nv_core::paths::config_file()).and_then(|m| m.modified()).ok()
}

fn copy_wide(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

pub fn create(ctl: Sender<Ctl>, cfg: Config, greet: bool) -> windows::core::Result<()> {
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("NeedleVoiceTray");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("NeedleVoice"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        TRAY = Some(Box::new(Tray { hwnd, ctl, cfg, cfg_mtime: mtime(), taskbar_created, restart: false }));
        add_icon(greet);
        SetTimer(Some(hwnd), TIMER_CONFIG, 2000, None);
    }
    Ok(())
}

/// True when the message loop exited because settings changed.
pub fn wants_restart() -> bool {
    tray().is_some_and(|t| t.restart)
}

fn base_nid(t: &Tray) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: t.hwnd,
        uID: 1,
        ..Default::default()
    }
}

fn tooltip(t: &Tray) -> String {
    if t.cfg.paused {
        "NeedleVoice — paused".into()
    } else {
        format!("NeedleVoice — say \"{}\"", t.cfg.wake_phrase())
    }
}

fn add_icon(greet: bool) {
    let Some(t) = tray() else { return };
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let icon = LoadIconW(Some(hinst.into()), PCWSTR(1 as *const u16))
            .or_else(|_| LoadIconW(None, IDI_APPLICATION))
            .unwrap_or_default();
        let mut nid = base_nid(t);
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = icon;
        copy_wide(&mut nid.szTip, &tooltip(t));
        if greet {
            nid.uFlags |= NIF_INFO;
            nid.dwInfoFlags = NIIF_NONE;
            copy_wide(&mut nid.szInfoTitle, "NeedleVoice is listening");
            copy_wide(
                &mut nid.szInfo,
                &format!("Try: \"{}, open Chrome\" or \"{}, what is the Haber process?\"", t.cfg.wake_phrase(), t.cfg.wake_phrase()),
            );
        }
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

fn update_tip() {
    let Some(t) = tray() else { return };
    let mut nid = base_nid(t);
    nid.uFlags = NIF_TIP;
    copy_wide(&mut nid.szTip, &tooltip(t));
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

pub fn remove_icon() {
    if let Some(t) = tray() {
        let nid = base_nid(t);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
    }
}

fn open_settings() {
    open_settings_tab(None);
}

/// Open the settings app, optionally straight on one tab.
fn open_settings_tab(tab: Option<&str>) {
    let exe = nv_core::paths::sibling_exe(nv_core::CONFIG_EXE);
    let result = match tab {
        Some(tab) => win::spawn_detached(&exe, &["--tab", tab]).map_err(|e| e.to_string()),
        None => win::shell_open(&exe.display().to_string(), None),
    };
    if let Err(e) = result {
        log::warn!("{e}");
    }
}

fn show_menu(hwnd: HWND) {
    let Some(t) = tray() else { return };
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let title = HSTRING::from(format!("Say \"{}\"", t.cfg.wake_phrase()));
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &title);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let pause = if t.cfg.paused { w!("Resume listening") } else { w!("Pause listening") };
        let _ = AppendMenuW(menu, MF_STRING, ID_PAUSE, pause);
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, w!("Settings…"));
        let _ = AppendMenuW(menu, MF_STRING, ID_SCHEDULE, w!("Alarms and reminders…"));
        let _ = AppendMenuW(menu, MF_STRING, ID_RESCAN, w!("Rescan apps"));
        let _ = AppendMenuW(menu, MF_STRING, ID_LOG, w!("Open log"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quit"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when clicking elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        on_command(cmd.0 as usize);
    }
}

fn on_command(id: usize) {
    let Some(t) = tray() else { return };
    match id {
        ID_PAUSE => {
            t.cfg.paused = !t.cfg.paused;
            let _ = t.ctl.send(Ctl::SetPaused(t.cfg.paused));
            // Persist so the config app shows it; record mtime so we don't react to our own write.
            let mut on_disk = Config::load();
            on_disk.paused = t.cfg.paused;
            let _ = on_disk.save();
            t.cfg_mtime = mtime();
            update_tip();
        }
        ID_SETTINGS => open_settings(),
        ID_SCHEDULE => open_settings_tab(Some("schedule")),
        ID_RESCAN => {
            let _ = t.ctl.send(Ctl::Rescan);
        }
        ID_LOG => {
            let _ = win::shell_open(&nv_core::paths::log_file().display().to_string(), None);
        }
        ID_QUIT => {
            let _ = t.ctl.send(Ctl::Quit);
            unsafe { PostQuitMessage(0) };
        }
        _ => {}
    }
}

/// Settings saved by the config app: apply pause instantly, restart for anything else.
fn check_config() {
    let Some(t) = tray() else { return };
    let m = mtime();
    if m == t.cfg_mtime {
        return;
    }
    t.cfg_mtime = m;
    let new = Config::load();
    let mut a = new.clone();
    let mut b = t.cfg.clone();
    a.paused = false;
    b.paused = false;
    if a != b {
        log::info!("settings changed, restarting");
        t.restart = true;
        unsafe { PostQuitMessage(0) };
        return;
    }
    if new.paused != t.cfg.paused {
        t.cfg.paused = new.paused;
        let _ = t.ctl.send(Ctl::SetPaused(new.paused));
        update_tip();
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if let Some(t) = tray() {
        if msg == t.taskbar_created && msg != 0 {
            // Explorer restarted; put the icon back.
            add_icon(false);
            return LRESULT(0);
        }
    }
    match msg {
        WM_TRAY => {
            match (lp.0 as u32) & 0xFFFF {
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                WM_LBUTTONDBLCLK | WM_LBUTTONUP => open_settings(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_CONFIG => {
            check_config();
            LRESULT(0)
        }
        // Closing the window means "stop". The installer and the settings app
        // both use that instead of terminating the process: it lets the
        // listener stop the microphone and the voice release the speakers
        // first. Without these two arms the agent could not be asked to quit at
        // all — the message loop has no other way out.
        WM_CLOSE => {
            if let Some(t) = tray() {
                let _ = t.ctl.send(Ctl::Quit);
            }
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_ENDSESSION => {
            remove_icon();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
