//! Turns a spoken command into actions.
//!
//! Needle 3 is the brain: it gets the command plus the tool list from
//! [`crate::tools`] (built-ins minus the ones switched off, plus the user's
//! own functions) and answers with JSON tool calls. Installed apps are *not*
//! put in the prompt (hundreds of names would make every command slow) —
//! Needle names the app as spoken and [`crate::apps::AppIndex`] fuzzy-matches it.
//!
//! Obvious commands can skip the model entirely ("instant commands"): every
//! function in the catalogue carries the exact phrases that trigger it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use needle_infer::v3_engine::{extract_tool_call, KvPrecision, V3Engine, V3Options};
use serde::Serialize;
use serde_json::Value;

use crate::apps::AppIndex;
use crate::config::Config;
use crate::fuzzy;
use crate::tools;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKey {
    Pause,
    Resume,
    Next,
    Previous,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeChange {
    Set(u8),
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Action {
    OpenApp(String),
    CloseApp(String),
    WebSearch(String),
    OpenWebsite(String),
    YoutubeSearch(String),
    Media(MediaKey),
    Volume(VolumeChange),
    Mute(bool),
    TellTime,
    TellDate,
    LockPc,
    ShowDesktop,
    Screenshot,
    CloseWindow,
    OpenSettings,
    /// One of the user's own functions, with the parameters it was given.
    Custom { name: String, params: Vec<(String, String)> },

    // ── The schedule ───────────────────────────────────────────────────
    /// "when" is the spoken time, parsed when it runs.
    Alarm { when: String, label: String },
    Reminder { text: String, when: String },
    CalendarEvent { text: String, when: String },
    Todo { text: String },
    ShowSchedule { what: String },
    CancelSchedule { what: String },
    CompleteTodo { what: String },
}

impl Action {
    /// Actions whose whole point is the answer itself ("what time is it",
    /// "alarm set for 7:30 am"): what the runner reports back *is* the reply.
    pub fn informational(&self) -> bool {
        matches!(
            self,
            Action::TellTime
                | Action::TellDate
                | Action::Alarm { .. }
                | Action::Reminder { .. }
                | Action::CalendarEvent { .. }
                | Action::Todo { .. }
                | Action::ShowSchedule { .. }
                | Action::CancelSchedule { .. }
                | Action::CompleteTodo { .. }
        )
    }

    /// The human wording used inside spoken lines ("{x}").
    pub fn subject(&self) -> String {
        match self {
            Action::OpenApp(a) | Action::CloseApp(a) => a.clone(),
            Action::WebSearch(q) | Action::YoutubeSearch(q) => q.clone(),
            Action::OpenWebsite(u) => u.clone(),
            Action::Media(_) => "the music".into(),
            Action::Volume(_) => "the volume".into(),
            Action::Mute(_) => "the sound".into(),
            Action::LockPc => "the computer".into(),
            Action::ShowDesktop => "the desktop".into(),
            Action::Screenshot => "the screenshot".into(),
            Action::CloseWindow => "the window".into(),
            Action::OpenSettings => "Settings".into(),
            Action::TellTime => "the time".into(),
            Action::TellDate => "the date".into(),
            Action::Alarm { .. } => "the alarm".into(),
            Action::Reminder { .. } => "the reminder".into(),
            Action::CalendarEvent { .. } => "the event".into(),
            Action::Todo { .. } => "the to-do".into(),
            Action::ShowSchedule { .. } | Action::CancelSchedule { .. } | Action::CompleteTodo { .. } => {
                "the schedule".into()
            }
            Action::Custom { name, .. } => name.replace('_', " "),
        }
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Action::OpenApp(a) => write!(f, "open app \"{a}\""),
            Action::CloseApp(a) => write!(f, "close app \"{a}\""),
            Action::WebSearch(q) => write!(f, "web search \"{q}\""),
            Action::OpenWebsite(u) => write!(f, "open website \"{u}\""),
            Action::YoutubeSearch(q) => write!(f, "YouTube search \"{q}\""),
            Action::Media(k) => write!(f, "media {k:?}"),
            Action::Volume(VolumeChange::Set(p)) => write!(f, "volume {p}%"),
            Action::Volume(VolumeChange::Up) => write!(f, "volume up"),
            Action::Volume(VolumeChange::Down) => write!(f, "volume down"),
            Action::Mute(true) => write!(f, "mute"),
            Action::Mute(false) => write!(f, "unmute"),
            Action::TellTime => write!(f, "tell the time"),
            Action::TellDate => write!(f, "tell the date"),
            Action::LockPc => write!(f, "lock the pc"),
            Action::ShowDesktop => write!(f, "show the desktop"),
            Action::Screenshot => write!(f, "take a screenshot"),
            Action::CloseWindow => write!(f, "close the window"),
            Action::OpenSettings => write!(f, "open settings"),
            Action::Alarm { when, label } => write!(f, "set an alarm for {when:?} ({label:?})"),
            Action::Reminder { text, when } => write!(f, "remind me to {text:?} at {when:?}"),
            Action::CalendarEvent { text, when } => write!(f, "add {text:?} to the calendar at {when:?}"),
            Action::Todo { text } => write!(f, "add {text:?} to the list"),
            Action::ShowSchedule { what } => write!(f, "read the schedule ({what:?})"),
            Action::CancelSchedule { what } => write!(f, "cancel {what:?}"),
            Action::CompleteTodo { what } => write!(f, "tick off {what:?}"),
            Action::Custom { name, params } => {
                let args: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v:?}")).collect();
                write!(f, "{name}({})", args.join(", "))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Via {
    Instant,
    Needle,
    /// Needle produced no call; the whole command was searched instead.
    Fallback,
}

#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub actions: Vec<Action>,
    pub via: Via,
    pub reasoning: Option<String>,
    pub millis: u128,
}

pub struct Brain {
    model_path: PathBuf,
    depth: usize,
    engine: Option<V3Engine>,
    last_used: Instant,
}

impl Brain {
    pub fn new(model_path: PathBuf, depth: usize) -> Self {
        Self { model_path, depth, engine: None, last_used: Instant::now() }
    }

    pub fn model_exists(&self) -> bool {
        self.model_path.exists()
    }

    /// Load Needle now (it takes ~30 ms, so normally this happens lazily).
    pub fn ensure_loaded(&mut self) -> Result<&V3Engine, String> {
        if self.engine.is_none() {
            let t = Instant::now();
            let engine = V3Engine::load_with_depth(&self.model_path, self.depth)
                .map_err(|e| format!("could not load Needle from {}: {e:?}", self.model_path.display()))?;
            log::info!("Needle 3 loaded (depth {}) in {:?}", self.depth, t.elapsed());
            self.engine = Some(engine);
        }
        Ok(self.engine.as_ref().unwrap())
    }

    /// Free the model if it hasn't been used for `idle`.
    pub fn unload_if_idle(&mut self, idle: Duration) {
        if self.engine.is_some() && self.last_used.elapsed() >= idle {
            self.engine = None;
            log::info!("Needle 3 unloaded (idle)");
        }
    }

    pub fn decide(&mut self, command: &str, cfg: &Config, apps: &AppIndex) -> Decision {
        let t = Instant::now();
        self.last_used = Instant::now();
        let command = clean_command(command);

        if cfg.instant_commands {
            if let Some(actions) = instant(&command, cfg, apps) {
                return Decision { actions, via: Via::Instant, reasoning: None, millis: t.elapsed().as_millis() };
            }
        }

        let (actions, reasoning) = match self.ask_needle(&command, cfg) {
            Ok(r) => r,
            Err(e) => {
                log::error!("{e}");
                (Vec::new(), None)
            }
        };
        self.last_used = Instant::now();
        if actions.is_empty() {
            return Decision {
                actions: vec![Action::WebSearch(command)],
                via: Via::Fallback,
                reasoning,
                millis: t.elapsed().as_millis(),
            };
        }
        Decision { actions, via: Via::Needle, reasoning, millis: t.elapsed().as_millis() }
    }

    fn ask_needle(&mut self, command: &str, cfg: &Config) -> Result<(Vec<Action>, Option<String>), String> {
        let tools_json = tools::tools_json(cfg);
        let engine = self.ensure_loaded()?;
        let opts = V3Options {
            constrain: true,
            kv_precision: KvPrecision::Int8,
            max_new_tokens: 160,
            ..Default::default()
        };
        let result = engine.generate(command, &tools_json, &opts);
        let reasoning = V3Engine::reasoning(&result.text).map(String::from);
        log::info!("needle: {:?}", result.text);
        let Some(json) = extract_tool_call(&result.text) else {
            return Ok((Vec::new(), reasoning));
        };
        Ok((parse_calls(&json, cfg), reasoning))
    }
}

/// Turn one tool call into an action. Shared by Needle's JSON and the instant
/// phrase matcher, so both always agree on what a function means.
pub fn action_from_call(name: &str, args: &Value, cfg: &Config) -> Option<Action> {
    let arg = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let action = match name {
        "open_app" => arg("app").map(|a| {
            if looks_like_domain(&a) {
                Action::OpenWebsite(a)
            } else {
                Action::OpenApp(a)
            }
        }),
        "close_app" => arg("app").map(Action::CloseApp),
        "web_search" => arg("query").map(Action::WebSearch),
        "open_website" => arg("url").map(Action::OpenWebsite),
        "youtube_search" => arg("query").map(Action::YoutubeSearch),
        "media_pause" => Some(Action::Media(MediaKey::Pause)),
        "media_resume" => Some(Action::Media(MediaKey::Resume)),
        "media_next" => Some(Action::Media(MediaKey::Next)),
        "media_previous" => Some(Action::Media(MediaKey::Previous)),
        "media_stop" => Some(Action::Media(MediaKey::Stop)),
        "volume_set" => Some(Action::Volume(VolumeChange::Set(percent(arg("percent"))))),
        "volume_up" => Some(Action::Volume(VolumeChange::Up)),
        "volume_down" => Some(Action::Volume(VolumeChange::Down)),
        "mute" => Some(Action::Mute(true)),
        "unmute" => Some(Action::Mute(false)),
        "tell_time" => Some(Action::TellTime),
        "tell_date" => Some(Action::TellDate),
        "lock_pc" => Some(Action::LockPc),
        "show_desktop" => Some(Action::ShowDesktop),
        "screenshot" => Some(Action::Screenshot),
        "close_window" => Some(Action::CloseWindow),
        "open_settings" => Some(Action::OpenSettings),
        "set_alarm" => Some(Action::Alarm { when: arg("when").unwrap_or_default(), label: arg("label").unwrap_or_default() }),
        "set_reminder" => Some(Action::Reminder {
            text: arg("text").unwrap_or_default(),
            when: arg("when").unwrap_or_default(),
        }),
        "add_event" => Some(Action::CalendarEvent {
            text: arg("text").unwrap_or_default(),
            when: arg("when").unwrap_or_default(),
        }),
        "add_todo" => Some(Action::Todo { text: arg("text").unwrap_or_default() }),
        "show_schedule" => Some(Action::ShowSchedule { what: arg("what").unwrap_or_default() }),
        "cancel_schedule" => Some(Action::CancelSchedule { what: arg("what").unwrap_or_default() }),
        "complete_todo" => {
            Some(Action::CompleteTodo { what: arg("what").unwrap_or_else(|| arg("text").unwrap_or_default()) })
        }
        // The user's own functions: keep exactly the parameters they declared.
        _ => cfg
            .custom_tools
            .iter()
            .find(|c| c.enabled && c.name == name)
            .map(|c| Action::Custom {
                name: c.name.clone(),
                params: c.params.iter().filter_map(|p| arg(p).map(|v| (p.clone(), v))).collect(),
            }),
    };
    action.filter(|a| match a {
        Action::OpenApp(s) | Action::CloseApp(s) => !s.is_empty(),
        Action::Todo { text } => !text.is_empty(),
        _ => true,
    })
}

/// "30", "30 percent" and "thirty" all mean 30.
fn percent(value: Option<String>) -> u8 {
    let Some(v) = value else { return 50 };
    let digits: String = v.chars().filter(|c| c.is_ascii_digit()).collect();
    if let Ok(n) = digits.parse::<u32>() {
        return n.min(100) as u8;
    }
    match v.to_lowercase().as_str() {
        "max" | "maximum" | "full" | "all the way up" => 100,
        "half" => 50,
        "min" | "minimum" | "zero" | "off" => 0,
        _ => 50,
    }
}

/// Parse `[{"name":..., "arguments":{...}}, ...]` into actions.
pub fn parse_calls(json: &str, cfg: &Config) -> Vec<Action> {
    let Ok(Value::Array(calls)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for call in calls {
        let name = call["name"].as_str().unwrap_or_default();
        if let Some(a) = action_from_call(name, &call["arguments"], cfg) {
            if !out.contains(&a) {
                out.push(a);
            }
        }
    }
    out
}

pub fn looks_like_domain(s: &str) -> bool {
    let s = s.trim().to_lowercase();
    !s.contains(' ')
        && [".com", ".org", ".net", ".io", ".dev", ".ai", ".gg", ".tv", ".co", ".edu", ".gov", ".uk", ".app"]
            .iter()
            .any(|tld| s.ends_with(tld) || s.contains(&format!("{tld}/")))
}

/// Strip politeness and the trailing punctuation Whisper adds.
fn clean_command(s: &str) -> String {
    let mut c = s.trim().trim_end_matches(['.', '!', '?', ',']).trim().to_string();
    let lower = c.to_lowercase();
    for p in ["can you please ", "could you please ", "can you ", "could you ", "would you ", "please ", "and ", "um ", "uh "] {
        if lower.starts_with(p) {
            c = c[p.len()..].to_string();
            break;
        }
    }
    c.trim_end_matches(" please").trim().to_string()
}

const OPEN_VERBS: &[&str] = &["open up", "bring up", "pull up", "fire up", "open", "launch", "start", "run", "load"];
const CLOSE_VERBS: &[&str] = &["shut down", "close", "quit", "exit", "kill"];
const SEARCH_VERBS: &[&str] = &["search the web for", "search google for", "search for", "search up", "look up", "google", "search"];
const QUESTION_WORDS: &[&str] = &[
    "what", "whats", "who", "whos", "where", "when", "why", "how", "which", "define", "is", "are", "does", "do", "did",
    "can", "should", "will", "tell me",
];

/// Websites people say as if they were apps.
pub const KNOWN_SITES: &[(&str, &str)] = &[
    ("youtube", "youtube.com"), ("gmail", "mail.google.com"), ("google", "google.com"), ("reddit", "reddit.com"),
    ("twitter", "x.com"), ("github", "github.com"), ("netflix", "netflix.com"), ("facebook", "facebook.com"),
    ("instagram", "instagram.com"), ("amazon", "amazon.com"), ("wikipedia", "wikipedia.org"), ("chatgpt", "chatgpt.com"),
    ("claude", "claude.ai"), ("twitch", "twitch.tv"), ("google drive", "drive.google.com"), ("google docs", "docs.google.com"),
    ("google maps", "maps.google.com"), ("linkedin", "linkedin.com"), ("tiktok", "tiktok.com"), ("outlook", "outlook.com"),
];

pub fn known_site(name: &str) -> Option<&'static str> {
    let n = fuzzy::squash(&fuzzy::strip_filler(name));
    KNOWN_SITES.iter().find(|(k, _)| fuzzy::squash(k) == n).map(|(_, u)| *u)
}

fn strip_prefix_word<'a>(text: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    prefixes.iter().find_map(|p| {
        text.strip_prefix(p).filter(|rest| rest.starts_with(' ')).map(str::trim)
    })
}

/// Deterministic handling for commands with only one sensible reading.
/// Returns `None` to hand the command to Needle.
fn instant(command: &str, cfg: &Config, apps: &AppIndex) -> Option<Vec<Action>> {
    let text = fuzzy::normalize(command);
    if text.is_empty() {
        return None;
    }

    // Phrases from the function catalogue. Only the ones with an unambiguous
    // meaning short-circuit here — "open {app}" still needs the app index to
    // agree, so those keep their hand-written rules further down.
    if let Some(hit) = tools::match_phrases(cfg, &text, &|tool: &str| !needs_lookup(tool)) {
        let args = Value::Object(hit.captures.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect());
        if let Some(a) = action_from_call(&hit.tool, &args, cfg) {
            return Some(vec![with_query_text(a, &text)]);
        }
    }
    // Anything that mentions the schedule is a question about it, however
    // Whisper spelled the first word ("words on my schedule"). Setting and
    // cancelling were already claimed by the phrase list above.
    if ["schedule", "calendar", "alarm", "reminder", "to do list", "todo list", "to do"]
        .iter()
        .any(|k| text.contains(k))
    {
        return Some(vec![Action::ShowSchedule { what: command.trim().to_string() }]);
    }

    // "play X on youtube" / "search youtube for X"
    if let Some(rest) = text.strip_prefix("search youtube for ").or_else(|| text.strip_prefix("youtube ")) {
        return Some(vec![Action::YoutubeSearch(rest.to_string())]);
    }
    if let Some(q) = text.strip_suffix(" on youtube") {
        let q = strip_prefix_word(q, &["play", "watch", "find", "search for", "search"]).unwrap_or(q);
        return Some(vec![Action::YoutubeSearch(q.to_string())]);
    }
    // "play some lofi music" / "watch cat videos" → YouTube.
    if let Some(q) = strip_prefix_word(&text, &["play me", "play", "put on", "watch"]) {
        let q = strip_prefix_word(q, &["some", "a", "the"]).unwrap_or(q);
        return Some(vec![Action::YoutubeSearch(q.to_string())]);
    }

    // Work on the lowercased raw text here so "reddit.com" keeps its dot.
    let raw = command.to_lowercase();
    if let Some(rest) = strip_prefix_word(&raw, OPEN_VERBS) {
        let mut out = Vec::new();
        for part in rest.split(" and ") {
            let part = part.trim();
            if looks_like_domain(part) {
                out.push(Action::OpenWebsite(part.to_string()));
                continue;
            }
            if let Some((app, score)) = apps.find(part, cfg) {
                if score >= 0.9 {
                    out.push(Action::OpenApp(app.name));
                    continue;
                }
            }
            match known_site(part) {
                Some(site) => out.push(Action::OpenWebsite(site.into())),
                None => return None, // unsure — let Needle think about it
            }
        }
        return Some(out);
    }

    if let Some(rest) = strip_prefix_word(&text, CLOSE_VERBS) {
        let (app, score) = apps.find(rest, cfg)?;
        return (score >= 0.88).then(|| vec![Action::CloseApp(app.name)]);
    }

    if let Some(rest) = strip_prefix_word(&text, &["go to", "visit", "navigate to"]) {
        if let Some(site) = known_site(rest) {
            return Some(vec![Action::OpenWebsite(site.into())]);
        }
        let raw = command.split_whitespace().last().unwrap_or_default();
        if looks_like_domain(raw) {
            return Some(vec![Action::OpenWebsite(raw.to_string())]);
        }
    }

    if let Some(rest) = strip_prefix_word(&text, SEARCH_VERBS) {
        return Some(vec![Action::WebSearch(rest.to_string())]);
    }

    let first = text.split(' ').next().unwrap_or_default();
    if QUESTION_WORDS.contains(&first) || text.starts_with("tell me") {
        return Some(vec![Action::WebSearch(command.trim().to_string())]);
    }
    None
}

/// "what are my alarms" is matched by a literal phrase, which captures nothing:
/// hand the whole question to the action so it can still tell alarms from todos.
fn with_query_text(action: Action, text: &str) -> Action {
    match action {
        Action::ShowSchedule { what } if what.trim().is_empty() => Action::ShowSchedule { what: text.to_string() },
        other => other,
    }
}

/// Functions whose phrases name something the app index or the browser has to
/// resolve, so they can't be taken at face value.
fn needs_lookup(tool: &str) -> bool {
    matches!(tool, "open_app" | "close_app" | "open_website" | "web_search" | "youtube_search")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::AppEntry;

    fn index() -> AppIndex {
        let e = |n: &str| AppEntry { name: n.into(), app_id: Some(n.into()), path: None, exe: None };
        AppIndex { apps: vec![e("Google Chrome"), e("Spotify"), e("Discord"), e("Visual Studio Code"), e("Steam")] }
    }

    #[test]
    fn instant_rules() {
        let cfg = Config::default();
        let apps = index();
        let run = |c: &str| instant(&clean_command(c), &cfg, &apps);
        assert_eq!(run("Open Chrome."), Some(vec![Action::OpenApp("Google Chrome".into())]));
        assert_eq!(run("open discord and spotify"), Some(vec![Action::OpenApp("Discord".into()), Action::OpenApp("Spotify".into())]));
        assert_eq!(run("open code"), Some(vec![Action::OpenApp("Visual Studio Code".into())]));
        assert_eq!(run("What is the Haber process?"), Some(vec![Action::WebSearch("What is the Haber process".into())]));
        assert_eq!(run("close spotify"), Some(vec![Action::CloseApp("Spotify".into())]));
        assert_eq!(run("open youtube"), Some(vec![Action::OpenWebsite("youtube.com".into())]));
        assert_eq!(run("go to reddit.com"), Some(vec![Action::OpenWebsite("reddit.com".into())]));
        assert_eq!(run("play lofi hip hop on youtube"), Some(vec![Action::YoutubeSearch("lofi hip hop".into())]));
        assert_eq!(run("open the thing i was using"), None);
        assert_eq!(run("play some low-fi music"), Some(vec![Action::YoutubeSearch("low fi music".into())]));
        assert_eq!(run("can you please search for rust tutorials"), Some(vec![Action::WebSearch("rust tutorials".into())]));
    }

    #[test]
    fn instant_media_and_system() {
        let cfg = Config::default();
        let apps = index();
        let run = |c: &str| instant(&clean_command(c), &cfg, &apps);
        assert_eq!(run("pause the music"), Some(vec![Action::Media(MediaKey::Pause)]));
        assert_eq!(run("stop media"), Some(vec![Action::Media(MediaKey::Pause)]));
        assert_eq!(run("resume the music"), Some(vec![Action::Media(MediaKey::Resume)]));
        assert_eq!(run("next song"), Some(vec![Action::Media(MediaKey::Next)]));
        assert_eq!(run("skip this song."), Some(vec![Action::Media(MediaKey::Next)]));
        assert_eq!(run("previous track"), Some(vec![Action::Media(MediaKey::Previous)]));
        assert_eq!(run("set the volume to 30"), Some(vec![Action::Volume(VolumeChange::Set(30))]));
        assert_eq!(run("turn it up"), Some(vec![Action::Volume(VolumeChange::Up)]));
        assert_eq!(run("turn it down"), Some(vec![Action::Volume(VolumeChange::Down)]));
        assert_eq!(run("mute"), Some(vec![Action::Mute(true)]));
        assert_eq!(run("unmute"), Some(vec![Action::Mute(false)]));
        assert_eq!(run("what time is it"), Some(vec![Action::TellTime]));
        assert_eq!(run("what's the date?"), Some(vec![Action::TellDate]));
        assert_eq!(run("lock my pc"), Some(vec![Action::LockPc]));
        assert_eq!(run("show desktop"), Some(vec![Action::ShowDesktop]));
        assert_eq!(run("take a screenshot"), Some(vec![Action::Screenshot]));
        assert_eq!(run("close this window"), Some(vec![Action::CloseWindow]));
        assert_eq!(run("open windows settings"), Some(vec![Action::OpenSettings]));
    }

    #[test]
    fn instant_custom_phrases() {
        let mut cfg = Config::default();
        cfg.custom_tools = tools::sanitize_tools(tools::templates(), &[]);
        let apps = index();
        let run = |c: &str| instant(&clean_command(c), &cfg, &apps);
        assert_eq!(
            run("search spotify for daft punk"),
            Some(vec![Action::Custom {
                name: "spotify_search".into(),
                params: vec![("query".into(), "daft punk".into())]
            }])
        );
        assert_eq!(
            run("shut down my pc"),
            Some(vec![Action::Custom { name: "shutdown_pc".into(), params: vec![] }])
        );
    }

    #[test]
    fn disabled_functions_stop_matching() {
        let mut cfg = Config::default();
        cfg.disabled_tools = vec!["media_pause".into(), "tell_time".into()];
        let apps = index();
        // Nothing claims it, so it goes to the model (and then the web).
        assert_eq!(instant("pause the music", &cfg, &apps), None);
        // "what time is it" is a question, so it falls through to a web search.
        assert_eq!(instant("what time is it", &cfg, &apps), Some(vec![Action::WebSearch("what time is it".into())]));
        // And it is gone from the tool list handed to Needle.
        let tools: Value = serde_json::from_str(&tools::tools_json(&cfg)).unwrap();
        assert!(!tools.to_string().contains("media_pause"));
    }

    #[test]
    fn parses_needle_json() {
        let cfg = Config::default();
        let a = parse_calls(
            r#"[{"name":"open_app","arguments":{"app":"reddit.com"}},{"name":"web_search","arguments":{"query":"x"}},{"name":"volume_set","arguments":{"percent":"40"}}]"#,
            &cfg,
        );
        assert_eq!(
            a,
            vec![
                Action::OpenWebsite("reddit.com".into()),
                Action::WebSearch("x".into()),
                Action::Volume(VolumeChange::Set(40))
            ]
        );
    }

    #[test]
    fn parses_custom_tool_calls() {
        let mut cfg = Config::default();
        cfg.custom_tools = tools::sanitize_tools(tools::templates(), &[]);
        let a = parse_calls(r#"[{"name":"maps_directions","arguments":{"place":"eiffel tower","junk":"x"}}]"#, &cfg);
        assert_eq!(
            a,
            vec![Action::Custom { name: "maps_directions".into(), params: vec![("place".into(), "eiffel tower".into())] }]
        );
        // A name nobody knows is dropped rather than guessed at.
        assert!(parse_calls(r#"[{"name":"nonsense","arguments":{}}]"#, &cfg).is_empty());
    }

    #[test]
    fn instant_schedule_commands() {
        let cfg = Config::default();
        let apps = index();
        let run = |c: &str| instant(&clean_command(c), &cfg, &apps);
        assert_eq!(
            run("set an alarm for 7:30 am"),
            Some(vec![Action::Alarm { when: "7 30 am".into(), label: String::new() }])
        );
        // The phrase pattern eats the "in", so "when" arrives as "20 minutes".
        assert_eq!(
            run("wake me up in 20 minutes"),
            Some(vec![Action::Alarm { when: "20 minutes".into(), label: String::new() }])
        );
        assert_eq!(
            run("remind me to call mum at 4 pm"),
            Some(vec![Action::Reminder { text: "call mum".into(), when: "4 pm".into() }])
        );
        assert_eq!(
            run("remind me about the dentist at 3 pm"),
            Some(vec![Action::Reminder { text: "the dentist".into(), when: "3 pm".into() }])
        );
        assert_eq!(run("add buy milk to my list"), Some(vec![Action::Todo { text: "buy milk".into() }]));
        // Whisper mis-hearings of the question word still land on the schedule.
        assert_eq!(
            run("Words on my schedule"),
            Some(vec![Action::ShowSchedule { what: "Words on my schedule".into() }])
        );
        assert_eq!(
            run("what is on my calendar"),
            Some(vec![Action::ShowSchedule { what: "what is on my calendar".into() }])
        );
        assert_eq!(run("remember to water the plants"), Some(vec![Action::Todo { text: "water the plants".into() }]));
        // A literal phrase captures nothing, so the question itself is kept.
        assert_eq!(
            run("what's on my schedule"),
            Some(vec![Action::ShowSchedule { what: "whats on my schedule".into() }])
        );
        assert_eq!(run("what are my alarms"), Some(vec![Action::ShowSchedule { what: "what are my alarms".into() }]));
        assert_eq!(run("read my list"), Some(vec![Action::ShowSchedule { what: "read my list".into() }]));
        assert_eq!(run("cancel my alarm"), Some(vec![Action::CancelSchedule { what: String::new() }]));
        assert_eq!(run("mark buy milk as done"), Some(vec![Action::CompleteTodo { what: "buy milk".into() }]));
        assert_eq!(
            run("put the dentist on my calendar at 3 pm"),
            Some(vec![Action::CalendarEvent { text: "the dentist".into(), when: "3 pm".into() }])
        );
        // And through the model's own JSON.
        let calls = parse_calls(r#"[{"name":"set_alarm","arguments":{"when":"7:30 am","label":"wake up"}}]"#, &cfg);
        assert_eq!(calls, vec![Action::Alarm { when: "7:30 am".into(), label: "wake up".into() }]);
    }

    #[test]
    fn percent_words() {
        assert_eq!(percent(Some("30 percent".into())), 30);
        assert_eq!(percent(Some("max".into())), 100);
        assert_eq!(percent(None), 50);
    }
}
