use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::tools::CustomTool;

pub use crate::personality::Persona;
pub use crate::voices::Engine;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Browser {
    /// Whatever Windows has set as the default browser.
    Default,
    Chrome,
    Edge,
    Firefox,
    Brave,
    /// Use `browser_path`.
    Custom,
}

impl Browser {
    pub const ALL: [Browser; 6] = [
        Browser::Default,
        Browser::Chrome,
        Browser::Edge,
        Browser::Firefox,
        Browser::Brave,
        Browser::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Browser::Default => "System default",
            Browser::Chrome => "Google Chrome",
            Browser::Edge => "Microsoft Edge",
            Browser::Firefox => "Firefox",
            Browser::Brave => "Brave",
            Browser::Custom => "Custom (path below)",
        }
    }

    /// Executable name registered under `App Paths`.
    pub fn exe_name(self) -> Option<&'static str> {
        match self {
            Browser::Chrome => Some("chrome.exe"),
            Browser::Edge => Some("msedge.exe"),
            Browser::Firefox => Some("firefox.exe"),
            Browser::Brave => Some("brave.exe"),
            Browser::Default | Browser::Custom => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alias {
    /// What you say, e.g. "code".
    pub phrase: String,
    /// The app it should open, e.g. "Visual Studio Code" — or a URL / path.
    pub target: String,
}

/// How the settings app is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    /// Follow the Windows "app mode" setting.
    System,
    Dark,
    Light,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Dark, Appearance::Light];

    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "System",
            Appearance::Dark => "Dark",
            Appearance::Light => "Light",
        }
    }

    pub fn is_dark(self) -> bool {
        match self {
            Appearance::Dark => true,
            Appearance::Light => false,
            // Unreadable means dark, which is what this app has always been.
            Appearance::System => !windows_prefers_light_apps().unwrap_or(false),
        }
    }
}

/// Windows' "Choose your default app mode": true when apps should be light.
fn windows_prefers_light_apps() -> Option<bool> {
    use windows::Win32::System::Registry::HKEY_CURRENT_USER;
    let key = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
    crate::win::reg_get_dword(HKEY_CURRENT_USER, key, "AppsUseLightTheme").map(|v| v != 0)
}


#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    // ── Wake word ──────────────────────────────────────────────
    /// The agent's name: "hey <agent_name>".
    pub agent_name: String,
    /// Words accepted before the name ("hey", "ok", ...).
    pub wake_prefixes: Vec<String>,
    /// Also wake on the bare name, without a prefix.
    pub allow_name_only: bool,
    /// Play a short chime the moment the wake word is heard.
    pub wake_sound: bool,
    /// Say a short "Yes?" the moment the wake word is heard.
    pub wake_reply: bool,
    /// 0.0 – 1.0. Higher wakes more easily (and falsely).
    pub wake_sensitivity: f32,
    /// Extra spellings that also count as the name (e.g. "no va").
    pub wake_extra_names: Vec<String>,
    /// Keyword-spotter boost (`:2.0`). 0 = derive it from the sensitivity.
    pub wake_score: f32,
    /// Keyword-spotter threshold (`#0.2`). 0 = derive it from the sensitivity.
    pub wake_threshold: f32,

    // ── Audio ──────────────────────────────────────────────────
    /// Input device name; empty = system default microphone.
    pub microphone: String,
    /// 0 – 3, WebRTC VAD aggressiveness. Higher ignores more background noise.
    pub vad_aggressiveness: u8,
    /// Software boost applied to the microphone, in dB (set by calibration).
    pub mic_gain_db: f32,
    /// Minimum loudness (dBFS, after boost) for speech to count. Raise in noisy rooms.
    pub min_speech_db: f32,
    /// Silence that ends a command, in milliseconds.
    pub end_silence_ms: u32,
    /// How long to wait for a command after just "hey <name>", in seconds.
    pub command_timeout_secs: f32,
    /// Whisper model file in the models folder.
    pub whisper_model: String,
    /// CPU threads used for speech recognition and Needle.
    pub threads: u32,

    // ── Brain ──────────────────────────────────────────────────
    /// Needle 3 depth (6 – 20). Lower is faster but less accurate.
    pub needle_depth: usize,
    /// Handle obvious commands ("open chrome", "what is ...") instantly
    /// without running Needle.
    pub instant_commands: bool,
    /// When an app can't be found, search the web for it instead.
    pub search_when_app_missing: bool,
    /// Seconds before unused models are unloaded from memory.
    pub unload_after_secs: u64,

    // ── Browser ────────────────────────────────────────────────
    pub browser: Browser,
    pub browser_path: String,
    /// `{}` is replaced by the URL-encoded query.
    pub search_url: String,

    // ── Personality ────────────────────────────────────────────
    pub personality: Persona,
    /// Speak replies out loud.
    pub voice_enabled: bool,
    pub voice_engine: Engine,
    /// Piper voice pack id (see `voices::PIPER_VOICES`).
    pub piper_voice: String,
    /// Kokoro speaker name, e.g. "af_heart".
    pub kokoro_voice: String,
    /// Windows voice name; empty = system default.
    pub system_voice: String,
    /// 0.6 (slow) – 1.6 (fast).
    pub voice_speed: f32,
    /// 0 – 100.
    pub voice_volume: u8,

    // ── Overlay ────────────────────────────────────────────────
    pub show_overlay: bool,
    /// Neon accent colour, `#RRGGBB`.
    pub accent_color: String,
    /// Which look the settings app uses.
    pub appearance: Appearance,
    /// Diameter of the bubble, in pixels (before DPI scaling).
    pub overlay_size: u32,
    /// Gap between the bubble and the bottom of the screen / taskbar.
    pub overlay_margin: u32,

    // ── Apps ───────────────────────────────────────────────────
    pub aliases: Vec<Alias>,
    /// Apps never opened by voice (matched by display name).
    pub excluded_apps: Vec<String>,
    /// Extra folders scanned for shortcuts / executables.
    pub extra_app_folders: Vec<String>,

    // ── Functions ──────────────────────────────────────────────
    /// Built-in function names switched off in the settings app.
    pub disabled_tools: Vec<String>,
    /// Functions you wrote yourself.
    pub custom_tools: Vec<CustomTool>,
    /// `python.exe` for Python functions; empty = find it automatically.
    pub python_path: String,
    /// Where Python functions look for scripts; empty = the default folder.
    pub scripts_dir: String,

    // ── System ─────────────────────────────────────────────────
    pub start_with_windows: bool,
    /// Pause listening (toggle from the tray icon too).
    pub paused: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            agent_name: "Nova".into(),
            wake_prefixes: vec!["hey".into(), "hi".into(), "ok".into(), "okay".into()],
            allow_name_only: false,
            wake_sensitivity: 0.5,
            wake_sound: true,
            wake_reply: false,
            wake_extra_names: Vec::new(),
            wake_score: 0.0,
            wake_threshold: 0.0,
            microphone: String::new(),
            vad_aggressiveness: 2,
            mic_gain_db: 0.0,
            min_speech_db: -50.0,
            end_silence_ms: 800,
            command_timeout_secs: 5.0,
            whisper_model: crate::DEFAULT_WHISPER_MODEL.into(),
            threads: 4,
            needle_depth: 20,
            instant_commands: true,
            search_when_app_missing: true,
            unload_after_secs: 60,
            browser: Browser::Chrome,
            browser_path: String::new(),
            search_url: "https://www.google.com/search?q={}".into(),
            personality: Persona::Cheerful,
            voice_enabled: true,
            voice_engine: Engine::Piper,
            piper_voice: crate::voices::DEFAULT_PIPER.into(),
            kokoro_voice: crate::voices::DEFAULT_KOKORO.into(),
            system_voice: String::new(),
            voice_speed: 1.05,
            voice_volume: 90,
            show_overlay: true,
            accent_color: "#B6FF2E".into(),
            appearance: Appearance::System,
            overlay_size: 72,
            overlay_margin: 36,
            aliases: vec![
                Alias { phrase: "code".into(), target: "Visual Studio Code".into() },
                Alias { phrase: "vs code".into(), target: "Visual Studio Code".into() },
                Alias { phrase: "files".into(), target: "File Explorer".into() },
            ],
            excluded_apps: Vec::new(),
            extra_app_folders: Vec::new(),
            disabled_tools: Vec::new(),
            custom_tools: Vec::new(),
            python_path: String::new(),
            scripts_dir: String::new(),
            start_with_windows: true,
            paused: false,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        Self::load_from(&crate::paths::config_file())
    }

    pub fn load_from(path: &Path) -> Self {
        match Self::try_load_from(path) {
            Ok(c) => c,
            Err(e) => {
                // Falling back to defaults silently is how someone's whole
                // setup disappears; the settings app shows this message.
                log::warn!("config could not be read, using defaults: {e}");
                Self::default()
            }
        }
    }

    /// Read the settings, saying why when it doesn't work. A missing file is
    /// not an error — that just means defaults.
    pub fn try_load_from(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("couldn't read {}: {e}", path.display())),
        };
        // Notepad and PowerShell both like to add a byte-order mark, which TOML
        // chokes on.
        let text = text.trim_start_matches('\u{feff}');
        toml::from_str::<Config>(text)
            .map(Config::sanitized)
            .map_err(|e| format!("{} couldn't be read: {e}", path.display()))
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&crate::paths::config_file())
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        // Write-then-rename so the agent never reads a half-written file.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, path)
    }

    /// Clamp values into ranges the agent can work with.
    pub fn sanitized(mut self) -> Self {
        self.wake_sensitivity = self.wake_sensitivity.clamp(0.0, 1.0);
        self.vad_aggressiveness = self.vad_aggressiveness.min(3);
        self.min_speech_db = self.min_speech_db.clamp(-80.0, -10.0);
        self.mic_gain_db = self.mic_gain_db.clamp(0.0, 40.0);
        self.end_silence_ms = self.end_silence_ms.clamp(300, 3000);
        self.command_timeout_secs = self.command_timeout_secs.clamp(1.0, 30.0);
        self.threads = self.threads.clamp(1, 32);
        self.needle_depth = self.needle_depth.clamp(6, 20);
        self.overlay_size = self.overlay_size.clamp(32, 256);
        self.overlay_margin = self.overlay_margin.min(500);
        self.voice_speed = self.voice_speed.clamp(0.6, 1.6);
        self.voice_volume = self.voice_volume.min(100);
        if parse_hex(&self.accent_color).is_none() {
            self.accent_color = Self::default().accent_color;
        }
        if self.agent_name.trim().is_empty() {
            self.agent_name = "Nova".into();
        }
        if !self.search_url.contains("{}") {
            self.search_url = Self::default().search_url;
        }
        if self.wake_score != 0.0 {
            self.wake_score = self.wake_score.clamp(0.5, 6.0);
        }
        if self.wake_threshold != 0.0 {
            self.wake_threshold = self.wake_threshold.clamp(0.02, 0.9);
        }
        self.wake_extra_names = self
            .wake_extra_names
            .iter()
            .map(|n| n.trim().to_lowercase())
            .filter(|n| !n.is_empty() && n != &self.agent_name.to_lowercase())
            .collect();
        self.wake_extra_names.dedup();
        // Functions: names must be valid, unique and not shadow a built-in.
        self.custom_tools = crate::tools::sanitize_tools(self.custom_tools, &self.disabled_tools);
        self
    }

    /// Where Python functions keep their scripts. Created on demand.
    pub fn scripts_dir(&self) -> std::path::PathBuf {
        let dir = if self.scripts_dir.trim().is_empty() {
            crate::paths::data_dir().join("scripts")
        } else {
            std::path::PathBuf::from(self.scripts_dir.trim().trim_matches('"'))
        };
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// Every name the assistant answers to, the main one first.
    pub fn wake_names(&self) -> Vec<String> {
        let mut names = vec![self.agent_name.clone()];
        names.extend(self.wake_extra_names.iter().cloned());
        names
    }

    /// Accent colour as 0–1 floats.
    pub fn accent_rgb(&self) -> (f32, f32, f32) {
        parse_hex(&self.accent_color).unwrap_or((0.71, 1.0, 0.18))
    }

    /// Human-readable wake phrase, e.g. "Hey Nova".
    pub fn wake_phrase(&self) -> String {
        match self.wake_prefixes.first() {
            Some(p) if !self.allow_name_only || !p.is_empty() => {
                let mut p = p.clone();
                if let Some(f) = p.get_mut(0..1) {
                    f.make_ascii_uppercase();
                }
                format!("{p} {}", self.agent_name)
            }
            _ => self.agent_name.clone(),
        }
    }
}

/// Neon presets offered in the config app.
pub const ACCENT_PRESETS: &[(&str, &str)] = &[
    ("Acid Lime", "#B6FF2E"),
    ("Hot Magenta", "#FF2BD6"),
    ("Electric Cyan", "#22F3FF"),
    ("Ultraviolet", "#9D5CFF"),
    ("Laser Orange", "#FF7A1A"),
    ("Plasma Pink", "#FF4F8B"),
];

/// Parse `#RRGGBB` into 0–1 floats.
pub fn parse_hex(s: &str) -> Option<(f32, f32, f32)> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some((
        ((v >> 16) & 0xFF) as f32 / 255.0,
        ((v >> 8) & 0xFF) as f32 / 255.0,
        (v & 0xFF) as f32 / 255.0,
    ))
}
