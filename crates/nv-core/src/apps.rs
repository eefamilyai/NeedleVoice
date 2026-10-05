//! Discover every launchable app on the PC.
//!
//! The main source is the shell's `AppsFolder` — the same list the Start menu
//! shows, covering classic desktop apps *and* Microsoft Store apps. Start-menu
//! shortcut folders and any user-configured folders are scanned on top, which
//! also tells us each app's `.exe` so it can be closed later.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use windows::Win32::UI::Shell::IShellItem;
use windows::core::Interface;

use crate::config::Config;
use crate::fuzzy;
use crate::win;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppEntry {
    /// Display name as shown in the Start menu.
    pub name: String,
    /// AppUserModelID; launched via `shell:AppsFolder\<id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// A `.lnk`, `.exe` or `ms-settings:`-style target to launch directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Executable file name, used to find the app's windows when closing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
}

impl AppEntry {
    pub fn launch(&self) -> Result<(), String> {
        if let Some(path) = &self.path {
            if win::shell_open(path, None).is_ok() {
                return Ok(());
            }
        }
        match &self.app_id {
            Some(id) => win::launch_app_id(id),
            None => Err(format!("don't know how to launch {}", self.name)),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AppIndex {
    pub apps: Vec<AppEntry>,
    /// Normalised and space-free forms of each name, parallel to `apps`, so
    /// scoring a spoken name against every installed app does not re-normalise
    /// every app name on every command. Rebuilt by [`AppIndex::reindex`] and
    /// not serialised: the cache stores `apps` alone.
    #[serde(skip)]
    pub normalized: Vec<String>,
    #[serde(skip)]
    pub squashed: Vec<String>,
}

/// Entries that are never what someone means by "open X".
const JUNK: &[&str] = &[
    "uninstall", "readme", "read me", "release notes", "documentation", "license",
    "website", "help", "manual", "changelog", "what's new", "support", "faq",
];

fn is_junk(name: &str) -> bool {
    let n = name.to_lowercase();
    JUNK.iter().any(|j| n.contains(j))
}

fn exe_from(path: &str) -> Option<String> {
    let p = Path::new(path);
    let ext = p.extension()?.to_str()?.to_ascii_lowercase();
    (ext == "exe").then(|| p.file_name()?.to_str().map(String::from)).flatten()
}

impl AppIndex {
    /// Load the cached list written by the last scan (fast, for startup).
    pub fn load_cache() -> Option<Self> {
        let text = std::fs::read_to_string(crate::paths::apps_cache_file()).ok()?;
        serde_json::from_str::<Self>(&text).ok().map(AppIndex::reindex)
    }

    pub fn save_cache(&self) {
        let Ok(text) = serde_json::to_string(self) else { return };
        // Write-then-rename: the agent reads this on startup, and a half-written
        // cache read or killed mid-write used to leave a file that never parsed
        // again, so every start fell back to a cold scan.
        let path = crate::paths::apps_cache_file();
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Full scan. Takes a few hundred milliseconds; run it off the UI thread.
    pub fn scan(cfg: &Config) -> Self {
        win::com_init();
        // Keyed on the normalised name, so "Visual Studio Code" from the Start
        // menu and from a shortcut folder end up as one entry.
        let mut by_name: HashMap<String, AppEntry> = HashMap::new();

        for app in scan_apps_folder() {
            by_name.entry(fuzzy::normalize(&app.name)).or_insert(app);
        }

        // Shortcut folders: fill in exe names, and add anything AppsFolder missed.
        let mut folders: Vec<PathBuf> = Vec::new();
        use windows::Win32::UI::Shell::{FOLDERID_CommonPrograms, FOLDERID_Programs};
        folders.extend(win::known_folder(&FOLDERID_Programs));
        folders.extend(win::known_folder(&FOLDERID_CommonPrograms));
        folders.extend(cfg.extra_app_folders.iter().map(PathBuf::from));
        for folder in folders {
            for (name, path) in scan_folder(&folder, 0) {
                let key = fuzzy::normalize(&name);
                // A shortcut we have not seen before.
                let Some(existing) = by_name.get_mut(&key) else {
                    let exe = exe_from(&path.to_string_lossy());
                    by_name.insert(
                        key,
                        AppEntry { name, app_id: None, path: Some(path.display().to_string()), exe },
                    );
                    continue;
                };
                // Known already: only worth a COM round-trip to resolve a .lnk
                // when we are still missing the exe name needed to close it.
                if existing.exe.is_some() {
                    continue;
                }
                existing.exe = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
                    win::shortcut_target(&path).and_then(|t| exe_from(&t.to_string_lossy()))
                } else {
                    exe_from(&path.to_string_lossy())
                };
            }
        }

        for app in builtin_apps() {
            by_name.entry(fuzzy::normalize(&app.name)).or_insert(app);
        }

        let mut apps: Vec<AppEntry> = by_name.into_values().filter(|a| !is_junk(&a.name)).collect();
        apps.sort_by_cached_key(|a| a.name.to_lowercase());
        AppIndex { apps, ..Default::default() }.reindex()
    }

    /// An index over a known list of names, with the lookup tables built.
    /// Used by the tests and by anything that does not need a real scan.
    pub fn from_names(names: impl IntoIterator<Item = AppEntry>) -> Self {
        AppIndex { apps: names.into_iter().collect(), ..Default::default() }.reindex()
    }

    /// Rebuild the parallel lookup tables [`AppIndex::best_match`] scans.
    pub fn reindex(mut self) -> Self {
        self.normalized = self.apps.iter().map(|a| fuzzy::normalize(&a.name)).collect();
        self.squashed = self.normalized.iter().map(|n| n.replace(' ', "")).collect();
        self
    }

    /// Best app for a spoken name, honouring aliases and exclusions.
    pub fn find(&self, spoken: &str, cfg: &Config) -> Option<(AppEntry, f64)> {
        let spoken_sq = fuzzy::squash(&fuzzy::strip_filler(spoken));
        if spoken_sq.is_empty() {
            return None;
        }
        // Aliases first: an exact alias always wins.
        for alias in &cfg.aliases {
            if fuzzy::squash(&alias.phrase) == spoken_sq {
                let t = alias.target.trim();
                if looks_like_target(t) {
                    return Some((AppEntry { name: alias.phrase.clone(), app_id: None, path: Some(t.into()), exe: None }, 1.0));
                }
                if let Some(hit) = self.best_match(t, cfg) {
                    return Some(hit);
                }
            }
        }
        self.best_match(spoken, cfg)
    }

    fn best_match(&self, spoken: &str, cfg: &Config) -> Option<(AppEntry, f64)> {
        // Exclusion matching used to re-normalise every app name for every
        // candidate, per command; the list is small but the index is not.
        let excluded: Vec<String> = cfg.excluded_apps.iter().map(|e| fuzzy::normalize(e)).collect();
        let mut best: Option<(&AppEntry, f64)> = None;
        for (i, app) in self.apps.iter().enumerate() {
            // The parallel tables are missing for an index built by hand (the
            // tests) rather than by `scan`, which normalises on demand instead.
            let owned;
            let (normal, squashed) = match (self.normalized.get(i), self.squashed.get(i)) {
                (Some(n), Some(s)) => (n.as_str(), s.as_str()),
                _ => {
                    owned = (fuzzy::normalize(&app.name), fuzzy::squash(&app.name));
                    (owned.0.as_str(), owned.1.as_str())
                }
            };
            if excluded.iter().any(|e| e == normal) {
                continue;
            }
            let s = fuzzy::app_score_prepared(spoken, normal, squashed);
            // On ties prefer the shorter name: "Chrome" over "Chrome Remote Desktop".
            let better = match best {
                None => true,
                Some((b, bs)) => s > bs + 1e-9 || ((s - bs).abs() < 1e-9 && app.name.len() < b.name.len()),
            };
            if better {
                best = Some((app, s));
            }
        }
        best.filter(|(_, s)| *s >= 0.78).map(|(a, s)| (a.clone(), s))
    }
}

/// Is an alias target a path / URL / protocol rather than an app name?
fn looks_like_target(t: &str) -> bool {
    t.contains(":\\") || t.contains("://") || t.starts_with("ms-") || t.starts_with("shell:") || t.ends_with(".exe")
}

/// Enumerate `shell:AppsFolder`, the Start menu's own app list.
fn scan_apps_folder() -> Vec<AppEntry> {
    use windows::core::HSTRING;
    use windows::Win32::UI::Shell::{BHID_EnumItems, IEnumShellItems, SHCreateItemFromParsingName};

    let mut out = Vec::new();
    unsafe {
        let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from("shell:AppsFolder"), None) else {
            return out;
        };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else {
            return out;
        };
        // Ask for the items in batches: `Next` is a COM round trip per call on
        // some shells, and the Start menu has a few hundred entries.
        const BATCH: usize = 32;
        let mut batch: [Option<IShellItem>; BATCH] = [const { None }; BATCH];
        loop {
            let mut fetched = 0u32;
            if items.Next(&mut batch, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            for slot in batch.iter_mut().take(fetched as usize) {
                let Some(item) = slot.take() else { continue };
                out.extend(read_app_item(&item));
            }
        }
    }
    out
}

/// One entry of `shell:AppsFolder` as an [`AppEntry`], or nothing when it isn't
/// a launchable app (no AppUserModelID, or no display name).
fn read_app_item(item: &IShellItem) -> Option<AppEntry> {
    use windows::Win32::Storage::EnhancedStorage::{PKEY_AppUserModel_ID, PKEY_Link_TargetParsingPath};
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{IShellItem2, SIGDN_NORMALDISPLAY};
    unsafe {
        let name_ptr = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?;
        let name = name_ptr.to_string().unwrap_or_default();
        CoTaskMemFree(Some(name_ptr.0 as *const _));
        let item2 = item.cast::<IShellItem2>().ok()?;
        let read = |key| -> Option<String> {
            let p = item2.GetString(key).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            s.filter(|s| !s.is_empty())
        };
        let app_id = read(&PKEY_AppUserModel_ID)?;
        let target = read(&PKEY_Link_TargetParsingPath);
        if name.trim().is_empty() {
            return None;
        }
        let exe = target.as_deref().and_then(exe_from).or_else(|| exe_from(&app_id));
        Some(AppEntry { name, app_id: Some(app_id), path: None, exe })
    }
}

/// Recursively collect `.lnk` / `.exe` files (max depth 3).
fn scan_folder(dir: &Path, depth: u32) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < 3 {
                out.extend(scan_folder(&path, depth + 1));
            }
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
        if matches!(ext.as_deref(), Some("lnk") | Some("exe") | Some("url")) {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.push((stem.to_string(), path.clone()));
            }
        }
    }
    out
}

/// System tools that aren't always in the Start menu list.
fn builtin_apps() -> Vec<AppEntry> {
    let e = |name: &str, path: &str, exe: Option<&str>| AppEntry {
        name: name.into(),
        app_id: None,
        path: Some(path.into()),
        exe: exe.map(String::from),
    };
    vec![
        e("Settings", "ms-settings:", Some("SystemSettings.exe")),
        e("Task Manager", "taskmgr.exe", Some("Taskmgr.exe")),
        e("Control Panel", "control.exe", None),
        e("File Explorer", "explorer.exe", None),
        e("Command Prompt", "cmd.exe", Some("cmd.exe")),
        e("PowerShell", "powershell.exe", Some("powershell.exe")),
        e("Notepad", "notepad.exe", Some("Notepad.exe")),
        e("Calculator", "calc.exe", Some("CalculatorApp.exe")),
        e("Paint", "mspaint.exe", Some("mspaint.exe")),
        e("Snipping Tool", "ms-screenclip:", None),
        e("Downloads", "shell:Downloads", None),
        e("Documents", "shell:Personal", None),
        e("Recycle Bin", "shell:RecycleBinFolder", None),
        e("Bluetooth Settings", "ms-settings:bluetooth", None),
        e("WiFi Settings", "ms-settings:network-wifi", None),
        e("Display Settings", "ms-settings:display", None),
        e("Sound Settings", "ms-settings:sound", None),
        e("Windows Update", "ms-settings:windowsupdate", None),
    ]
}
