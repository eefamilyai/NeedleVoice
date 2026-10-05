//! Thin wrappers over the Win32 calls the three executables share.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::core::{w, Interface, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eMultimedia, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile, CLSCTX_ALL, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::Shutdown::LockWorkStation;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT,
    VK_LWIN, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK, VK_MEDIA_STOP, VK_MENU,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP, VK_VOLUME_DOWN,
    VK_VOLUME_MUTE, VK_VOLUME_UP,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, KEY_WRITE,
    REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    CreateMutexW, OpenProcess, TerminateProcess, PROCESS_TERMINATE,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellExecuteW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetForegroundWindow, GetShellWindow, GetSystemMetrics, GetWindow, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, PostMessageW, SendMessageW, GW_OWNER, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOWNORMAL, WM_CLOSE, WM_COMMAND,
};

pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Initialise COM for this thread (single-threaded apartment). Safe to call repeatedly.
pub fn com_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

// ── Launching ────────────────────────────────────────────────────────────

/// `ShellExecute` "open" — works for exes, shortcuts, URLs, `shell:` paths
/// and protocol links like `ms-settings:`.
pub fn shell_open(target: &str, args: Option<&str>) -> Result<(), String> {
    let t = HSTRING::from(target);
    let a = args.map(HSTRING::from);
    let r = unsafe {
        ShellExecuteW(
            None,
            &HSTRING::from("open"),
            &t,
            a.as_ref().map_or(PCWSTR::null(), |a| PCWSTR(a.as_ptr())),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute returns a value > 32 on success.
    if r.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!("could not open {target} (code {})", r.0 as isize))
    }
}

/// Launch a Start-menu app by its AppUserModelID (works for Store apps too).
pub fn launch_app_id(app_id: &str) -> Result<(), String> {
    shell_open("explorer.exe", Some(&format!("shell:AppsFolder\\{app_id}")))
}

// ── Registry ─────────────────────────────────────────────────────────────

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\NeedleVoice";

/// Read a REG_DWORD value, e.g. whether Windows prefers light apps.
pub fn reg_get_dword(root: HKEY, key: &str, value: &str) -> Option<u32> {
    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let r = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(key),
            PCWSTR(HSTRING::from(value).as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut data as *mut u32).cast()),
            Some(&mut size),
        )
    };
    r.is_ok().then_some(data)
}

pub fn reg_get_string(root: HKEY, key: &str, value: Option<&str>) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let v = value.map(HSTRING::from);
    let r = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(key),
            v.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr())),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if r.is_err() {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn reg_set_string(root: HKEY, key: &str, name: &str, value: &str) -> Result<(), String> {
    unsafe {
        let mut hkey = HKEY::default();
        RegCreateKeyExW(
            root,
            &HSTRING::from(key),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
        .ok()
        .map_err(|e| e.to_string())?;
        let data = wide(value);
        let bytes = std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2);
        let r = RegSetValueExW(hkey, &HSTRING::from(name), None, REG_SZ, Some(bytes));
        let _ = RegCloseKey(hkey);
        r.ok().map_err(|e| e.to_string())
    }
}

fn reg_set_dword(root: HKEY, key: &str, name: &str, value: u32) -> Result<(), String> {
    unsafe {
        let mut hkey = HKEY::default();
        RegCreateKeyExW(
            root,
            &HSTRING::from(key),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
        .ok()
        .map_err(|e| e.to_string())?;
        let r = RegSetValueExW(hkey, &HSTRING::from(name), None, REG_DWORD, Some(&value.to_le_bytes()));
        let _ = RegCloseKey(hkey);
        r.ok().map_err(|e| e.to_string())
    }
}

/// Register (or remove) the agent under HKCU\...\Run so it starts at login.
pub fn set_autostart(enabled: bool, agent_exe: &Path) -> Result<(), String> {
    if enabled {
        let cmd = format!("\"{}\" --background", agent_exe.display());
        reg_set_string(HKEY_CURRENT_USER, RUN_KEY, crate::PRODUCT, &cmd)
    } else {
        unsafe {
            let mut hkey = HKEY::default();
            if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), None, KEY_SET_VALUE, &mut hkey)
                .is_ok()
            {
                let _ = RegDeleteValueW(hkey, &HSTRING::from(crate::PRODUCT));
                let _ = RegCloseKey(hkey);
            }
        }
        Ok(())
    }
}

pub fn autostart_enabled() -> bool {
    reg_get_string(HKEY_CURRENT_USER, RUN_KEY, Some(crate::PRODUCT)).is_some()
}

/// Add the "Apps & features" entry so Windows can uninstall us.
pub fn register_uninstaller(install_dir: &Path, version: &str, size_kb: u32) -> Result<(), String> {
    let uninst = install_dir.join(crate::UNINSTALL_EXE);
    let icon = install_dir.join(crate::AGENT_EXE);
    let k = UNINSTALL_KEY;
    let r = HKEY_CURRENT_USER;
    reg_set_string(r, k, "DisplayName", "NeedleVoice")?;
    reg_set_string(r, k, "DisplayVersion", version)?;
    reg_set_string(r, k, "Publisher", "NeedleVoice")?;
    reg_set_string(r, k, "DisplayIcon", &icon.display().to_string())?;
    reg_set_string(r, k, "InstallLocation", &install_dir.display().to_string())?;
    reg_set_string(r, k, "UninstallString", &format!("\"{}\" --uninstall", uninst.display()))?;
    reg_set_dword(r, k, "NoModify", 1)?;
    reg_set_dword(r, k, "NoRepair", 1)?;
    reg_set_dword(r, k, "EstimatedSize", size_kb)?;
    Ok(())
}

pub fn unregister_uninstaller() {
    unsafe {
        let _ = RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(UNINSTALL_KEY));
    }
}

pub fn installed_location() -> Option<PathBuf> {
    reg_get_string(HKEY_CURRENT_USER, UNINSTALL_KEY, Some("InstallLocation")).map(PathBuf::from)
}

/// Look up an executable registered under `App Paths` (e.g. `chrome.exe`).
pub fn app_path(exe: &str) -> Option<PathBuf> {
    let key = format!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\{exe}");
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        if let Some(p) = reg_get_string(root, &key, None) {
            let p = PathBuf::from(p.trim_matches('"'));
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

// ── Shortcuts ────────────────────────────────────────────────────────────

/// Create a `.lnk` shortcut. COM must be initialised on this thread.
pub fn create_shortcut(lnk: &Path, target: &Path, args: &str, description: &str) -> Result<(), String> {
    unsafe {
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        link.SetPath(&HSTRING::from(target.as_os_str())).map_err(|e| e.to_string())?;
        link.SetArguments(&HSTRING::from(args)).map_err(|e| e.to_string())?;
        link.SetDescription(&HSTRING::from(description)).map_err(|e| e.to_string())?;
        if let Some(dir) = target.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str())).map_err(|e| e.to_string())?;
        }
        link.SetIconLocation(&HSTRING::from(target.as_os_str()), 0).map_err(|e| e.to_string())?;
        let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        if let Some(dir) = lnk.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        file.Save(&HSTRING::from(lnk.as_os_str()), true).map_err(|e| e.to_string())
    }
}

/// Resolve a `.lnk` file to its target path (without touching the network).
pub fn shortcut_target(lnk: &Path) -> Option<PathBuf> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(&HSTRING::from(lnk.as_os_str()), STGM_READ).ok()?;
        let mut buf = [0u16; 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        if len == 0 {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
    }
}

/// The user's Documents folder, or None when it can't be resolved.
pub fn documents_folder() -> Option<PathBuf> {
    known_folder(&windows::Win32::UI::Shell::FOLDERID_Documents)
}

/// Known folders like the Start Menu, Desktop.
pub fn known_folder(id: &windows::core::GUID) -> Option<PathBuf> {
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        let p: PWSTR = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

// ── Processes ────────────────────────────────────────────────────────────

pub struct ProcessInfo {
    pub pid: u32,
    pub exe: String,
}

pub fn processes() -> Vec<ProcessInfo> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
                out.push(ProcessInfo {
                    pid: entry.th32ProcessID,
                    exe: String::from_utf16_lossy(&entry.szExeFile[..len]),
                });
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Politely close every top-level window owned by the given processes.
/// Returns how many windows were asked to close.
pub fn close_windows_of(pids: &[u32]) -> usize {
    struct Ctx<'a> {
        pids: &'a [u32],
        count: usize,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> windows::core::BOOL {
        let ctx = &mut *(lp.0 as *mut Ctx);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let owned = GetWindow(hwnd, GW_OWNER).map(|h| !h.is_invalid()).unwrap_or(false);
        if ctx.pids.contains(&pid) && IsWindowVisible(hwnd).as_bool() && !owned {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            ctx.count += 1;
        }
        true.into()
    }
    let mut ctx = Ctx { pids, count: 0 };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    ctx.count
}

pub fn kill_process(pid: u32) -> bool {
    unsafe {
        match OpenProcess(PROCESS_TERMINATE, false, pid) {
            Ok(h) => {
                let ok = TerminateProcess(h, 0).is_ok();
                let _ = CloseHandle(h);
                ok
            }
            Err(_) => false,
        }
    }
}

/// Kill every process with this exe name except ourselves.
pub fn kill_by_exe(exe: &str) -> usize {
    let me = std::process::id();
    processes()
        .into_iter()
        .filter(|p| p.pid != me && p.exe.eq_ignore_ascii_case(exe))
        .filter(|p| kill_process(p.pid))
        .count()
}

pub fn is_running(exe: &str) -> bool {
    let me = std::process::id();
    processes().iter().any(|p| p.pid != me && p.exe.eq_ignore_ascii_case(exe))
}

/// Hold a named mutex for the life of the process. Returns `None` when
/// another process already owns it.
pub struct SingleInstance(HANDLE);

impl SingleInstance {
    pub fn acquire(name: &str) -> Option<Self> {
        unsafe {
            let h = CreateMutexW(None, true, &HSTRING::from(name)).ok()?;
            if windows::Win32::Foundation::GetLastError() == windows::Win32::Foundation::ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(h);
                return None;
            }
            Some(SingleInstance(h))
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Spawn a process without a console window.
pub fn spawn_detached(exe: &Path, args: &[&str]) -> std::io::Result<()> {
    spawn_hidden(exe, args)
}

/// Same, but accepting a bare command name resolved through `PATH`.
pub fn spawn_hidden(exe: impl AsRef<OsStr>, args: &[&str]) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new(exe)
        .args(args)
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

// ── Keyboard, media keys and volume ──────────────────────────────────────

/// Press or release one virtual key. Returns how many events Windows accepted
/// (0 when the input couldn't be delivered — a locked or disconnected session).
fn send_key(vk: VIRTUAL_KEY, up: bool) -> u32 {
    unsafe {
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32)
    }
}

/// Tap a key: down then up. Media and volume keys listen for exactly this.
/// Returns true when Windows took both halves of the press.
pub fn tap_key(vk: VIRTUAL_KEY) -> bool {
    send_key(vk, false) == 1 && send_key(vk, true) == 1
}

/// The media-key meaning of each action in [`crate::brain::MediaKey`].
/// Play/pause is one key on Windows, so pause and resume share it — every
/// mainstream player treats it as a toggle.
pub fn media_vk(key: crate::brain::MediaKey) -> VIRTUAL_KEY {
    use crate::brain::MediaKey::*;
    match key {
        Pause | Resume => VK_MEDIA_PLAY_PAUSE,
        Next => VK_MEDIA_NEXT_TRACK,
        Previous => VK_MEDIA_PREV_TRACK,
        Stop => VK_MEDIA_STOP,
    }
}

/// Map one written key name ("ctrl", "esc", "f5", "a") to a virtual key.
fn key_vk(name: &str) -> Option<VIRTUAL_KEY> {
    let n = name.trim().to_lowercase();
    let fixed = match n.as_str() {
        "ctrl" | "control" => Some(VK_CONTROL),
        "shift" => Some(VK_SHIFT),
        "alt" => Some(VK_MENU),
        "win" | "windows" | "meta" | "super" | "cmd" => Some(VK_LWIN),
        "esc" | "escape" => Some(VK_ESCAPE),
        "tab" => Some(VK_TAB),
        "enter" | "return" => Some(VK_RETURN),
        "space" | "spacebar" => Some(VK_SPACE),
        "backspace" | "back" => Some(VK_BACK),
        "delete" | "del" => Some(VK_DELETE),
        "up" => Some(VK_UP),
        "down" => Some(VK_DOWN),
        "left" => Some(VK_LEFT),
        "right" => Some(VK_RIGHT),
        "home" => Some(VK_HOME),
        "end" => Some(VK_END),
        "pageup" | "page up" | "pgup" => Some(VK_PRIOR),
        "pagedown" | "page down" | "pgdn" => Some(VK_NEXT),
        "play" | "playpause" | "play/pause" => Some(VK_MEDIA_PLAY_PAUSE),
        "next" | "nexttrack" | "next track" => Some(VK_MEDIA_NEXT_TRACK),
        "previous" | "prev" | "previoustrack" => Some(VK_MEDIA_PREV_TRACK),
        "stop" | "stopmedia" => Some(VK_MEDIA_STOP),
        "mute" => Some(VK_VOLUME_MUTE),
        "volumeup" | "volume up" => Some(VK_VOLUME_UP),
        "volumedown" | "volume down" => Some(VK_VOLUME_DOWN),
        _ => None,
    };
    if fixed.is_some() {
        return fixed;
    }
    // Function keys.
    if let Some(rest) = n.strip_prefix('f') {
        if let Ok(i) = rest.parse::<u16>() {
            if (1..=24).contains(&i) {
                return Some(VIRTUAL_KEY(0x70 + i - 1));
            }
        }
        return None;
    }
    // A single letter or digit.
    let mut chars = n.chars();
    let first = chars.next()?;
    (chars.next().is_none() && first.is_ascii_alphanumeric())
        .then(|| VIRTUAL_KEY(first.to_ascii_uppercase() as u16))
}

/// Press a key combination like `ctrl+shift+esc` or `win+d`. Modifiers are
/// held while the last key is tapped.
pub fn press_keys(combo: &str) -> Result<String, String> {
    let names: Vec<&str> = combo.split(['+', '-']).map(str::trim).filter(|s| !s.is_empty()).collect();
    if names.is_empty() {
        return Err("no keys to press".into());
    }
    let keys: Vec<VIRTUAL_KEY> = names
        .iter()
        .map(|n| key_vk(n).ok_or_else(|| format!("I don't know the key \"{n}\"")))
        .collect::<Result<_, _>>()?;
    let mut sent = 0;
    for k in &keys {
        sent += send_key(*k, false);
    }
    std::thread::sleep(std::time::Duration::from_millis(30));
    for k in keys.iter().rev() {
        sent += send_key(*k, true);
    }
    if sent < keys.len() as u32 * 2 {
        return Err("Windows wouldn't take the keystrokes (is the screen locked?)".into());
    }
    Ok(format!("pressed {}", names.join(" + ")))
}

/// The audio endpoint whose volume the volume keys change.
fn endpoint_volume() -> Option<IAudioEndpointVolume> {
    unsafe {
        com_init();
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia).ok()?;
        device.Activate(CLSCTX_ALL, None).ok()
    }
}

/// Current master volume, 0–100.
pub fn volume_percent() -> Option<u8> {
    let v = endpoint_volume()?;
    let level = unsafe { v.GetMasterVolumeLevelScalar() }.ok()?;
    Some((level * 100.0).round().clamp(0.0, 100.0) as u8)
}

/// Set the master volume, 0–100.
pub fn set_volume_percent(percent: u8) -> Result<u8, String> {
    let percent = percent.min(100);
    match endpoint_volume() {
        Some(v) => {
            unsafe { v.SetMasterVolumeLevelScalar(percent as f32 / 100.0, std::ptr::null()) }
                .map_err(|e| format!("Windows refused the volume change: {e}"))?;
            Ok(percent)
        }
        None => {
            // No audio endpoint: fall back to the volume keys, a small step at
            // a time. Better than doing nothing at all.
            let steps = (percent as i16 - volume_percent().unwrap_or(50) as i16) / 2;
            nudge_volume(steps);
            Ok(percent)
        }
    }
}

/// Move the volume by `steps` × 2% using the volume keys.
pub fn nudge_volume(steps: i16) {
    let key = if steps >= 0 { VK_VOLUME_UP } else { VK_VOLUME_DOWN };
    for _ in 0..steps.unsigned_abs().min(50) {
        tap_key(key);
        std::thread::sleep(std::time::Duration::from_millis(12));
    }
}

pub fn muted() -> Option<bool> {
    let v = endpoint_volume()?;
    unsafe { v.GetMute() }.ok().map(|m| m.as_bool())
}

pub fn set_muted(on: bool) -> Result<(), String> {
    match endpoint_volume() {
        Some(v) => unsafe { v.SetMute(on, std::ptr::null()) }.map_err(|e| format!("Windows refused the mute: {e}")),
        None => {
            // Only a toggle is available without COM, so check first.
            if muted() != Some(on) {
                tap_key(VK_VOLUME_MUTE);
            }
            Ok(())
        }
    }
}

// ── Windows the user is looking at ───────────────────────────────────────

/// Ask the window in front to close (the X button, politely).
pub fn close_foreground_window() -> Result<String, String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return Err("there's no window in front to close".into());
        }
        if hwnd == GetShellWindow() {
            return Err("that's the desktop, not a window".into());
        }
        let title = window_title(hwnd);
        PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)).map_err(|e| e.to_string())?;
        Ok(if title.is_empty() { "closed the window".into() } else { format!("closed {title}") })
    }
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Minimize everything — the same as clicking the far edge of the taskbar.
pub fn show_desktop() -> Result<(), String> {
    // 419 is the long-standing "toggle desktop" command of the shell tray.
    const TOGGLE_DESKTOP: usize = 419;
    unsafe {
        let tray = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).map_err(|e| e.to_string())?;
        SendMessageW(tray, WM_COMMAND, Some(WPARAM(TOGGLE_DESKTOP)), Some(LPARAM(0)));
    }
    Ok(())
}

pub fn lock_workstation() -> Result<(), String> {
    unsafe { LockWorkStation() }.map_err(|e| e.to_string())
}

// ── Screenshots ──────────────────────────────────────────────────────────

/// Capture every monitor into a PNG and return its path.
pub fn screenshot() -> Result<PathBuf, String> {
    unsafe {
        let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (w, h) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        if w <= 0 || h <= 0 {
            return Err("couldn't measure the screen".into());
        }
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err("couldn't get the screen".into());
        }
        // Every GDI object is checked as it is made: creating a device context
        // or a bitmap can fail under memory pressure, and the old code carried
        // on regardless and blitted into a null surface.
        let mem = CreateCompatibleDC(Some(screen));
        let bitmap = if mem.is_invalid() { Default::default() } else { CreateCompatibleBitmap(screen, w, h) };
        if mem.is_invalid() || bitmap.is_invalid() {
            if !mem.is_invalid() {
                let _ = DeleteDC(mem);
            }
            ReleaseDC(None, screen);
            return Err("couldn't create the capture surface".into());
        }
        let old = SelectObject(mem, bitmap.into());
        let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let blit = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY);
        // `GetDIBits` refuses to read a bitmap that is still selected into a
        // device context, so it has to come out first — which is also the only
        // order in which the copy can be trusted to have happened.
        SelectObject(mem, old);
        let lines = if blit.is_ok() {
            GetDIBits(mem, bitmap, 0, h as u32, Some(pixels.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS)
        } else {
            0
        };
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);

        blit.map_err(|e| format!("couldn't copy the screen: {e}"))?;
        if lines == 0 {
            return Err("couldn't read the screen pixels".into());
        }
        // GDI hands back BGRA with the alpha byte left at zero, which as RGBA
        // would be a fully transparent PNG.
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        let path = screenshot_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let file = std::fs::File::create(&path).map_err(|e| format!("couldn't save the screenshot: {e}"))?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&pixels).map_err(|e| e.to_string())?;
        Ok(path)
    }
}

/// `Pictures\NeedleVoice\Screenshot 2026-10-02 08.36.24.png`, falling back to
/// the app's data folder when there is no Pictures library.
fn screenshot_path() -> PathBuf {
    use windows::Win32::UI::Shell::FOLDERID_Pictures;
    let dir = known_folder(&FOLDERID_Pictures)
        .unwrap_or_else(crate::paths::data_dir)
        .join(crate::PRODUCT);
    let t = unsafe { GetLocalTime() };
    dir.join(format!(
        "Screenshot {:04}-{:02}-{:02} {:02}.{:02}.{:02}.png",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    ))
}

// ── Running things and reading what they say ─────────────────────────────

/// Run a program, wait for it (up to `timeout`) and return its output.
///
/// Used by Python and PowerShell functions, where the script's answer is the
/// point. Both streams are captured, so an error message is visible rather
/// than lost in a window nobody sees.
pub fn run_capture(exe: impl AsRef<OsStr>, args: &[&str], timeout: std::time::Duration) -> Result<String, String> {
    run_capture_command(exe, args, &[], timeout)
}

/// [`run_capture`] with environment variables for the child.
pub fn run_capture_command(
    exe: impl AsRef<OsStr>,
    args: &[&str],
    env: &[(String, String)],
    timeout: std::time::Duration,
) -> Result<String, String> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mut command = std::process::Command::new(exe.as_ref());
    command.args(args);
    for (k, v) in env {
        command.env(k, v);
    }
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("couldn't start {}: {e}", exe.as_ref().to_string_lossy()))?;

    let collect = |mut pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            if let Some(p) = pipe.as_mut() {
                let mut bytes = Vec::new();
                let _ = p.read_to_end(&mut bytes);
                buf = String::from_utf8_lossy(&bytes).into_owned();
            }
            buf
        })
    };
    let out = collect(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let err = collect(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));

    let deadline = std::time::Instant::now() + timeout;
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(poll_gap(started.elapsed())),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Err(e) => return Err(format!("waiting for the program failed: {e}")),
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let text = if stdout.trim().is_empty() { stderr } else { stdout };
    match status {
        Some(s) if s.success() => Ok(text.trim().to_string()),
        Some(s) => Err(format!(
            "it exited with {}: {}",
            s.code().map(|c| c.to_string()).unwrap_or_else(|| "no code".into()),
            text.trim().lines().last().unwrap_or("no output").trim()
        )),
        None => Err(format!("it took longer than {} seconds", timeout.as_secs())),
    }
}

/// How long to wait before asking again whether a child has finished.
///
/// A Python or PowerShell function is short, and a flat 25 ms poll spent most
/// of its time asleep: a script that took 60 ms was reported as 100. The gap
/// starts short enough to notice an immediate exit and lengthens so a long
/// script still costs almost nothing.
fn poll_gap(elapsed: std::time::Duration) -> std::time::Duration {
    let ms = elapsed.as_millis();
    let gap = if ms < 50 {
        2
    } else if ms < 250 {
        5
    } else if ms < 1000 {
        10
    } else {
        25
    };
    std::time::Duration::from_millis(gap)
}

/// Stop a running process by image name. The agent has no IPC, so this is how
/// the settings app's Stop button and the tray's Exit both end it.
pub fn stop_process(image: &str) -> bool {
    run_capture("taskkill", &["/IM", image, "/F"], std::time::Duration::from_secs(10)).is_ok()
}

/// Where Python is, if it's installed.
///
/// Windows ships a 0-byte "app execution alias" for `python.exe` in
/// `WindowsApps` that just opens the Store, so those are skipped. The `py`
/// launcher is preferred: it always picks the newest real install.
pub fn find_python(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim().trim_matches('"');
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        return p.exists().then_some(p);
    }
    let usable = |p: &Path| -> bool {
        let Ok(meta) = std::fs::metadata(p) else { return false };
        // Store aliases are 0-byte reparse points.
        meta.len() > 0 && !p.to_string_lossy().to_ascii_lowercase().contains("\\windowsapps\\")
    };
    let named = |dir: &Path| -> Option<PathBuf> {
        ["py.exe", "python.exe"].iter().map(|n| dir.join(n)).find(|p| usable(p))
    };

    // The real installs first: a handful of directory reads, against walking
    // every entry on PATH. The settings app asks for this on every frame, and
    // walking PATH twice a frame on a machine without Python is what made the
    // window stutter.
    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = std::env::var_os(var) {
            let root = PathBuf::from(root);
            roots.push(root.join("Programs").join("Python"));
            roots.push(root.join("Python"));
        }
    }
    let mut installs: Vec<PathBuf> = Vec::new();
    for root in &roots {
        let Ok(entries) = std::fs::read_dir(root) else { continue };
        for entry in entries.flatten() {
            if let Some(found) = named(&entry.path()) {
                installs.push(found);
            }
        }
        // A bare `Python\py.exe`, with no version folder in between.
        if let Some(found) = named(root) {
            installs.push(found);
        }
    }
    // Newest first by path, which is how these sort: Python313 > Python312.
    installs.sort();
    if let Some(found) = installs.pop() {
        return Some(found);
    }

    // Then whatever PATH points at. `python3.exe` comes last: on Windows the
    // only thing that name usually resolves to is the Store alias.
    if let Some(path) = std::env::var_os("PATH") {
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        for name in ["py.exe", "python.exe", "python3.exe"] {
            for dir in &dirs {
                let candidate = dir.join(name);
                if usable(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

// ── Clock ────────────────────────────────────────────────────────────────

/// Wall-clock time in the user's timezone.
pub struct LocalTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    /// 0 = Sunday.
    pub weekday: u16,
}

pub fn local_time() -> LocalTime {
    let t = unsafe { GetLocalTime() };
    LocalTime {
        year: t.wYear,
        month: t.wMonth,
        day: t.wDay,
        hour: t.wHour,
        minute: t.wMinute,
        second: t.wSecond,
        weekday: t.wDayOfWeek,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_map() {
        assert_eq!(key_vk("ctrl"), Some(VK_CONTROL));
        assert_eq!(key_vk("WIN"), Some(VK_LWIN));
        assert_eq!(key_vk("esc"), Some(VK_ESCAPE));
        assert_eq!(key_vk("f5"), Some(VIRTUAL_KEY(0x74)));
        assert_eq!(key_vk("f24"), Some(VIRTUAL_KEY(0x87)));
        assert_eq!(key_vk("d"), Some(VIRTUAL_KEY(0x44)));
        assert_eq!(key_vk("play/pause"), Some(VK_MEDIA_PLAY_PAUSE));
        assert_eq!(key_vk("volume up"), Some(VK_VOLUME_UP));
        assert_eq!(key_vk("banana"), None);
        assert_eq!(key_vk("f25"), None);
        assert_eq!(key_vk("ab"), None);
    }

    #[test]
    fn bad_combos_are_refused() {
        assert!(press_keys("").is_err());
        assert!(press_keys("ctrl+nonsense").is_err());
    }

    /// The poll gap only ever grows, and starts short enough to notice a
    /// process that exits at once.
    #[test]
    fn child_polling_starts_quick_and_backs_off() {
        use std::time::Duration;
        assert!(poll_gap(Duration::ZERO) <= Duration::from_millis(2));
        let mut last = Duration::ZERO;
        for ms in [0u64, 10, 60, 300, 2_000, 30_000] {
            let gap = poll_gap(Duration::from_millis(ms));
            assert!(gap >= last, "{ms} ms went backwards");
            assert!(gap <= Duration::from_millis(25), "{ms} ms polled slower than the old flat 25");
            last = gap;
        }
        assert_eq!(poll_gap(Duration::from_secs(60)), Duration::from_millis(25));
    }

    #[test]
    fn media_keys_are_mapped() {
        use crate::brain::MediaKey::*;
        assert_eq!(media_vk(Pause), VK_MEDIA_PLAY_PAUSE);
        assert_eq!(media_vk(Resume), VK_MEDIA_PLAY_PAUSE);
        assert_eq!(media_vk(Next), VK_MEDIA_NEXT_TRACK);
        assert_eq!(media_vk(Previous), VK_MEDIA_PREV_TRACK);
        assert_eq!(media_vk(Stop), VK_MEDIA_STOP);
    }

    /// F24 is the polite test key: nothing on a normal desktop reacts to it.
    #[test]
    #[ignore]
    fn send_input_works() {
        assert!(tap_key(VIRTUAL_KEY(0x87)), "SendInput refused the key press");
        assert!(press_keys("ctrl+f24").is_ok());
    }

    /// Reads the master volume and puts it straight back, so nothing changes.
    #[test]
    #[ignore]
    fn volume_com_round_trips() {
        let before = volume_percent().expect("no default audio endpoint");
        set_volume_percent(before).expect("could not set the volume");
        let after = volume_percent().unwrap();
        assert!(after.abs_diff(before) <= 1, "{before} -> {after}");
        let _ = muted();
        println!("master volume {before}% (unchanged)");
    }

    /// Captures the screen, then reads the PNG back to prove it is a real image:
    /// the right size (which catches a capture surface that never got created),
    /// fully opaque (which catches the alpha byte GDI leaves at zero) and not
    /// one flat colour (which catches a blit that did not happen).
    #[test]
    #[ignore]
    fn screenshot_is_a_png() {
        let path = screenshot().expect("screenshot failed");
        let bytes = std::fs::read(&path).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
        let mut reader = decoder.read_info().unwrap();
        let info = reader.info();
        let (w, h) = (info.width, info.height);
        assert_eq!(info.color_type, png::ColorType::Rgba, "a screenshot must be RGBA");
        let expected = unsafe { (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN)) };
        assert_eq!((w, h), (expected.0 as u32, expected.1 as u32), "size mismatch");
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut buf).unwrap();

        let mut opaque = 0usize;
        let mut colours = std::collections::HashSet::new();
        for px in buf.chunks_exact(4).step_by(4096) {
            if px[3] == 255 {
                opaque += 1;
            }
            colours.insert([px[0], px[1], px[2]]);
        }
        let sampled = buf.len() / 4 / 4096 + 1;
        assert_eq!(opaque, sampled, "the capture came back transparent");
        assert!(colours.len() > 1, "the capture is a single flat colour — the blit did nothing");
        println!("{}: {w}x{h}, {} bytes, {} distinct colours", path.display(), bytes.len(), colours.len());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    #[ignore]
    fn clock_is_sane() {
        let t = local_time();
        assert!((1900..2200).contains(&t.year), "{t:?}", t = t.year);
        assert!((1..=12).contains(&t.month));
        assert!(t.hour < 24 && t.minute < 60);
    }
}
