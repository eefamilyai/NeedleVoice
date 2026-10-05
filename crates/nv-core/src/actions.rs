//! Carry out the actions the brain decided on.

use std::path::PathBuf;
use std::time::Duration;

use crate::apps::AppIndex;
use crate::brain::{known_site, looks_like_domain, Action, MediaKey, VolumeChange};
use crate::config::{Browser, Config};
use crate::fuzzy;
use crate::schedule::{describe_when, Item, ItemKind, Schedule, Stamp};
use crate::schedule_parse;
use crate::tools::{self, CustomKind, StepKind};
use crate::win;

/// Something that can say a line out loud. The agent hands in its voice; the
/// settings app hands in [`Silent`].
pub trait Speaker: Send + Sync {
    fn say(&self, text: &str);
}

/// A speaker for contexts with nobody listening: it only logs.
pub struct Silent;

impl Speaker for Silent {
    fn say(&self, text: &str) {
        log::info!("(not speaking) {text}");
    }
}

/// Run one action. `Ok` carries a short description of what happened — for
/// "what time is it" that description *is* the answer.
pub fn execute(action: &Action, cfg: &Config, apps: &AppIndex) -> Result<String, String> {
    execute_with(action, cfg, apps, &Silent)
}

/// Run one action, letting multi-step functions speak as they go.
pub fn execute_with(action: &Action, cfg: &Config, apps: &AppIndex, speaker: &dyn Speaker) -> Result<String, String> {
    match action {
        Action::OpenApp(name) => open_app(name, cfg, apps),
        Action::CloseApp(name) => close_app(name, cfg, apps),
        Action::WebSearch(q) => {
            let url = cfg.search_url.replace("{}", &fuzzy::url_encode(q));
            open_url(&url, cfg).map(|_| format!("searched for \"{q}\""))
        }
        Action::YoutubeSearch(q) => {
            let url = format!("https://www.youtube.com/results?search_query={}", fuzzy::url_encode(q));
            open_url(&url, cfg).map(|_| format!("searched YouTube for \"{q}\""))
        }
        Action::OpenWebsite(site) => {
            let url = to_url(site);
            open_url(&url, cfg).map(|_| format!("opened {url}"))
        }
        Action::Media(key) => media(*key),
        Action::Volume(change) => set_volume(*change),
        Action::Mute(on) => win::set_muted(*on).map(|_| {
            if *on { "muted the sound".to_string() } else { "turned the sound back on".to_string() }
        }),
        Action::TellTime => Ok(format!("it's {}", spoken_time())),
        Action::TellDate => Ok(format!("today is {}", spoken_date())),
        Action::LockPc => win::lock_workstation().map(|_| "locked the computer".into()),
        Action::ShowDesktop => win::show_desktop().map(|_| "minimized everything".into()),
        Action::Screenshot => win::screenshot().map(|p| format!("saved a screenshot to {}", p.display())),
        Action::CloseWindow => win::close_foreground_window(),
        Action::OpenSettings => win::shell_open("ms-settings:", None).map(|_| "opened Windows Settings".into()),
        // Nothing to undo: the listener goes back to waiting on its own. This
        // just gives the personality something to say.
        Action::Disengage => Ok("stood down".into()),
        Action::Custom { name, params } => run_custom(cfg, name, params, apps, speaker),

        // ── The schedule ───────────────────────────────────────────────
        Action::Alarm { when, label } => add_item(ItemKind::Alarm, label, when, true),
        Action::Reminder { text, when } => {
            if when.trim().is_empty() {
                add_item(ItemKind::Todo, text, "", false)
            } else {
                add_item(ItemKind::Reminder, text, when, true)
            }
        }
        Action::CalendarEvent { text, when } => add_item(ItemKind::Event, text, when, false),
        Action::Todo { text } => add_item(ItemKind::Todo, text, "", false),
        Action::ShowSchedule { what } => show_schedule(what),
        Action::CancelSchedule { what } => cancel_item(what),
        Action::ClearSchedule { what } => clear_items(what),
        Action::CompleteTodo { what } => complete_item(what),
    }
}

/// Which kind of item a spoken word means: "alarms" → Alarm, and "everything"
/// (or nothing at all) → all of them.
fn kind_from_words(what: &str) -> Option<ItemKind> {
    let w = nv_core_text(what);
    let single = |words: &[&str]| words.iter().any(|k| w.contains(k));
    if single(&["everything", "all of it", "the lot", "schedule", "everythin"]) {
        return None;
    }
    if single(&["alarm", "timer", "wake"]) {
        Some(ItemKind::Alarm)
    } else if single(&["reminder", "remind"]) {
        Some(ItemKind::Reminder)
    } else if single(&["todo", "to do", "to-do", "task", "list", "milk"]) {
        Some(ItemKind::Todo)
    } else if single(&["event", "calendar", "appointment", "meeting"]) {
        Some(ItemKind::Event)
    } else {
        None
    }
}

/// Lowercase and squeeze spaces, for matching spoken words.
fn nv_core_text(s: &str) -> String {
    s.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Delete every alarm, reminder, to-do or event — all of them when the words
/// don't name one kind.
fn clear_items(what: &str) -> Result<String, String> {
    let mut schedule = Schedule::open();
    let kind = kind_from_words(what);
    if schedule.items.is_empty() {
        return Err("there's nothing on your schedule to delete".to_string());
    }
    let gone = schedule.clear(kind);
    if gone.is_empty() {
        return Err(match kind {
            Some(k) => format!("there were no {} to delete", k.label().to_lowercase()),
            None => "there's nothing on your schedule to delete".to_string(),
        });
    }
    schedule.save().map_err(|e| e.to_string())?;
    let noun = match kind {
        Some(k) => k.label().to_lowercase(),
        None => "items".to_string(),
    };
    let names: Vec<String> = gone.iter().take(3).map(|i| i.text.clone()).collect();
    let mut said = format!("deleted {} {noun}", gone.len());
    if !names.is_empty() {
        said.push_str(&format!(" ({})", names.join(", ")));
        if gone.len() > names.len() {
            said.push_str(", …");
        }
    }
    Ok(said)
}

/// Add an alarm, reminder, event or to-do from a spoken time.
fn add_item(kind: ItemKind, text: &str, when: &str, chime: bool) -> Result<String, String> {
    let now = Stamp::now();
    let text = text.trim().to_string();
    let mut schedule = Schedule::open();
    let item = if when.trim().is_empty() {
        Item { kind, text: text.clone(), at: None, chime, ..Default::default() }
    } else {
        let (at, repeat) = schedule_parse::parse(when, now)?;
        Item { kind, text: text.clone(), at: Some(at), repeat, chime, speak: true, ..Default::default() }
    };
    let when_text = item.at.map(|at| describe_when(at, item.repeat, now)).unwrap_or_default();
    let times_only = kind == ItemKind::Todo || item.at.is_none();
    schedule.add(item);
    schedule.save().map_err(|e| format!("couldn't save the schedule: {e}"))?;

    let what = if text.is_empty() { "it".to_string() } else { format!("\"{text}\"") };
    if times_only {
        // Counted from the copy already in hand: re-opening the file here read
        // and re-parsed the whole schedule just to say how long the list is.
        let left = schedule.todos(false).len();
        return Ok(match left {
            0 => format!("added {what} to your list"),
            1 => format!("added {what} to your list. That's the only thing on it"),
            n => format!("added {what} to your list. {n} things on it now"),
        });
    }
    Ok(format!("{} set for {when_text}", kind.label().to_lowercase()))
}

/// "what's on my schedule" — the kind asked about, if any.
fn asked_kind(what: &str) -> Option<ItemKind> {
    let w = fuzzy::normalize(what);
    // Read the whole question, not just the captured words: "what are my alarms".
    if w.contains("alarm") {
        Some(ItemKind::Alarm)
    } else if w.contains("reminder") {
        Some(ItemKind::Reminder)
    } else if w.contains("event") || w.contains("meeting") || w.contains("appointment") {
        Some(ItemKind::Event)
    } else if w.contains("list") || w.contains("todo") || w.contains("to do") || w.contains("task") {
        Some(ItemKind::Todo)
    } else {
        None
    }
}

fn show_schedule(what: &str) -> Result<String, String> {
    let now = Stamp::now();
    let schedule = Schedule::open();
    let kind = asked_kind(what);
    let summary = schedule.spoken_summary(now, kind);
    // Say when the next thing is, too: it is the useful half of the answer.
    // The sentence has to be joined on whatever punctuation the summary ended
    // with — "Your schedule is clear." and "…: buy milk" both happen — and the
    // line is kept even when the summary is empty of a full stop, which is how
    // this used to be computed and then silently thrown away.
    let Some((at, item)) = schedule.next_up(now) else { return Ok(summary) };
    let extra = format!("Next up: {} at {}", item.text, describe_when(at, item.repeat, now));
    if summary.trim().is_empty() {
        return Ok(format!("{extra}."));
    }
    let sep = if summary.ends_with(['.', '!', '?']) { " " } else { ". " };
    Ok(format!("{summary}{sep}{extra}."))
}

fn cancel_item(what: &str) -> Result<String, String> {
    let mut schedule = Schedule::open();
    let id = if what.trim().is_empty() {
        // "cancel my alarm": the next thing due is what they mean.
        schedule.next_up(Stamp::now()).map(|(_, i)| i.id)
    } else {
        schedule.find(what).map(|i| i.id)
    };
    let Some(id) = id else {
        return Err(if what.trim().is_empty() {
            if schedule.items.is_empty() {
                "there's nothing on your schedule to cancel".to_string()
            } else {
                "I couldn't tell which one you meant".to_string()
            }
        } else {
            format!("I couldn't find anything like \"{}\"", what.trim())
        });
    };
    // `find` and `remove` both look the id up; the second lookup can only fail
    // if the schedule changed underneath, which is not worth a panic.
    let Some(removed) = schedule.remove(id) else {
        return Err("I couldn't tell which one you meant".to_string());
    };
    schedule.save().map_err(|e| format!("couldn't save the schedule: {e}"))?;
    Ok(match removed.kind {
        ItemKind::Todo => format!("took \"{}\" off your list", removed.text),
        _ => format!("cancelled the {}: {}", removed.kind.label().to_lowercase(), removed.text),
    })
}

fn complete_item(what: &str) -> Result<String, String> {
    let mut schedule = Schedule::open();
    let Some(id) = schedule.find(what).map(|i| i.id) else {
        return Err(format!("couldn't find \"{}\" on your list", what.trim()));
    };
    if schedule.get(id).is_some_and(|i| i.done) {
        return Ok("that's already ticked off".into());
    }
    let text = schedule.get(id).map(|i| i.text.clone()).unwrap_or_default();
    schedule.set_done(id, true);
    schedule.save().map_err(|e| format!("couldn't save the schedule: {e}"))?;
    let left = schedule.todos(false).len();
    Ok(if left == 0 {
        format!("ticked off \"{text}\". That was everything")
    } else {
        format!("ticked off \"{text}\". {left} to go")
    })
}

// ── Media and sound ──────────────────────────────────────────────────────

/// Media players are driven by the same four keys a keyboard sends, so this
/// works with whatever happens to be playing.
fn media(key: MediaKey) -> Result<String, String> {
    if !win::tap_key(win::media_vk(key)) {
        return Err("couldn't send the media key (is the screen locked?)".into());
    }
    Ok(match key {
        MediaKey::Pause => "paused the media",
        MediaKey::Resume => "resumed the media",
        MediaKey::Next => "skipped to the next track",
        MediaKey::Previous => "went back a track",
        MediaKey::Stop => "stopped the media",
    }
    .to_string())
}

/// How far one "turn it up" moves the volume, in 2% key presses.
const VOLUME_STEPS: i16 = 4;

fn set_volume(change: VolumeChange) -> Result<String, String> {
    match change {
        VolumeChange::Set(percent) => {
            let percent = win::set_volume_percent(percent)?;
            Ok(format!("set the volume to {percent}%"))
        }
        VolumeChange::Up => {
            win::nudge_volume(VOLUME_STEPS);
            Ok("turned the volume up".into())
        }
        VolumeChange::Down => {
            win::nudge_volume(-VOLUME_STEPS);
            Ok("turned the volume down".into())
        }
    }
}

// ── Time and date ────────────────────────────────────────────────────────

const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October",
    "November", "December",
];
const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// "3:42 pm" — the way people say it, not 15:42.
fn spoken_time() -> String {
    let t = win::local_time();
    let (hour, suffix) = match t.hour {
        0 => (12, "am"),
        1..=11 => (t.hour, "am"),
        12 => (12, "pm"),
        _ => (t.hour - 12, "pm"),
    };
    if t.minute == 0 {
        format!("{hour} {suffix}")
    } else {
        format!("{hour}:{:02} {suffix}", t.minute)
    }
}

/// "Friday, 2 October 2026".
fn spoken_date() -> String {
    let t = win::local_time();
    let month = MONTHS.get(t.month.saturating_sub(1) as usize).copied().unwrap_or("");
    let weekday = WEEKDAYS.get(t.weekday as usize % 7).copied().unwrap_or("");
    format!("{weekday}, {} {month} {}", t.day, t.year)
}

// ── The user's own functions ─────────────────────────────────────────────

/// Look the function up again (it may have been edited since the command was
/// spoken), fill in the parameters and do what it says.
fn run_custom(
    cfg: &Config,
    name: &str,
    params: &[(String, String)],
    apps: &AppIndex,
    speaker: &dyn Speaker,
) -> Result<String, String> {
    let tool = cfg
        .custom_tools
        .iter()
        .find(|t| t.name == name)
        .ok_or_else(|| format!("\"{}\" is no longer set up", name.replace('_', " ")))?
        .clone();
    if !tool.enabled {
        return Err(format!("\"{}\" is switched off", tool.label()));
    }
    match tool.kind {
        CustomKind::Run => {
            let target = tools::substitute(&tool.target, params, false);
            if target.trim().is_empty() {
                return Err(format!("\"{}\" has nothing to run", tool.label()));
            }
            let args = tools::substitute(&tool.args, params, false);
            win::shell_open(&target, (!args.trim().is_empty()).then_some(args.as_str()))?;
            Ok(format!("ran {}", tool.label()))
        }
        CustomKind::OpenUrl => {
            let url = tools::substitute(&tool.target, params, true);
            open_checked(&url, cfg, &tool.label())
        }
        CustomKind::Keys => win::press_keys(&tools::substitute(&tool.target, params, false)),
        CustomKind::PowerShell => {
            let script = tools::substitute(&tool.target, params, false);
            run_powershell(&script)
        }
        CustomKind::Python => run_python(cfg, &tool.target, &tool.args, params),
        CustomKind::Sequence => run_sequence(cfg, &tool, params, apps, speaker),
        CustomKind::SayOnly => Ok(format!("said the reply for {}", tool.label())),
    }
}

fn open_checked(url: &str, cfg: &Config, label: &str) -> Result<String, String> {
    if !url.trim().starts_with("http") && !url.contains(':') {
        return Err(format!("\"{label}\" needs a full web address"));
    }
    open_url(url, cfg)?;
    Ok(format!("opened {url}"))
}

/// Run a PowerShell snippet with no window and no profile, returning what it
/// printed. Startup is ~400 ms, so this is for occasional system tweaks.
fn run_powershell(script: &str) -> Result<String, String> {
    if script.trim().is_empty() {
        return Err("there's no script to run".into());
    }
    win::run_capture_command(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script],
        &[],
        Duration::from_secs(30),
    )
}

/// Run a Python script from the scripts folder, passing the parameters in and
/// speaking whatever it prints.
fn run_python(cfg: &Config, target: &str, args: &str, params: &[(String, String)]) -> Result<String, String> {
    let script = tools::substitute(target, params, false);
    let extra = tools::substitute(args, params, false);
    let path = resolve_script(cfg, &script)?;
    let python = win::find_python(&cfg.python_path).ok_or_else(|| {
        "Python isn't installed. Install it, or point Settings at python.exe, and try again.".to_string()
    })?;
    let mut argv: Vec<String> = vec![path.display().to_string()];
    if extra.trim().is_empty() {
        // No argument template: hand over the declared parameters in order.
        argv.extend(params.iter().map(|(_, v)| v.clone()));
    } else {
        argv.extend(extra.split_whitespace().map(String::from));
    }
    // Parameters are also available as NV_PARAM_<NAME>, which survives spaces.
    let env: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (format!("NV_PARAM_{}", k.to_uppercase()), v.clone()))
        .collect();
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let output = win::run_capture_command(&python, &refs, &env, Duration::from_secs(30))
        .map_err(|e| format!("{}
{e}", path.file_name().unwrap_or_default().to_string_lossy()))?;
    let trimmed = last_line(&output);
    Ok(if trimmed.is_empty() { format!("ran {}", path.file_name().unwrap_or_default().to_string_lossy()) } else { trimmed })
}

/// Absolute paths are used as they are; a bare name is looked up in the
/// scripts folder.
fn resolve_script(cfg: &Config, script: &str) -> Result<PathBuf, String> {
    let script = script.trim().trim_matches('"');
    if script.is_empty() {
        return Err("that function has no script".into());
    }
    let direct = PathBuf::from(script);
    if direct.is_absolute() || script.contains(['/', '\\']) {
        return direct.exists().then_some(direct).ok_or_else(|| format!("I can't find {script}"));
    }
    let dir = cfg.scripts_dir();
    for candidate in [dir.join(script), dir.join(format!("{script}.py"))] {
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(format!("I can't find \"{script}\" in {}", dir.display()))
}

/// The one line of a script's output that is worth saying out loud.
fn last_line(text: &str) -> String {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or_default().to_string()
}

/// Run a sequence's steps in order. Steps that print something leave it in
/// `{output}` for the steps after them.
fn run_sequence(
    cfg: &Config,
    tool: &tools::CustomTool,
    params: &[(String, String)],
    apps: &AppIndex,
    speaker: &dyn Speaker,
) -> Result<String, String> {
    if tool.steps.is_empty() {
        return Err(format!("\"{}\" has no steps yet", tool.label()));
    }
    if nesting() > 4 {
        return Err("that function calls itself in a loop".into());
    }
    let _guard = Nesting::new();
    let mut output = String::new();
    for (i, step) in tool.steps.iter().enumerate() {
        let fill = |s: &str| {
            let with_params = tools::substitute(s, params, false);
            tools::substitute(&with_params, &[("output".to_string(), output.clone())], false)
        };
        let where_ = format!("step {} of {}", i + 1, tool.label());
        match step.kind {
            StepKind::Say => speaker.say(&fill(&step.target)),
            StepKind::Run => {
                let target = fill(&step.target);
                let args = fill(&step.args);
                win::shell_open(&target, (!args.trim().is_empty()).then_some(args.as_str()))
                    .map_err(|e| format!("{where_}: {e}"))?;
            }
            StepKind::OpenUrl => {
                open_checked(&fill(&step.target), cfg, &where_)?;
            }
            StepKind::Keys => {
                win::press_keys(&fill(&step.target)).map_err(|e| format!("{where_}: {e}"))?;
            }
            StepKind::PowerShell => output = run_powershell(&fill(&step.target)).map_err(|e| format!("{where_}: {e}"))?,
            StepKind::Python => {
                output = run_python(cfg, &fill(&step.target), &fill(&step.args), params)
                    .map_err(|e| format!("{where_}: {e}"))?
            }
            StepKind::Wait => {
                let ms: u64 = fill(&step.target).trim().parse().unwrap_or(500).min(30_000);
                std::thread::sleep(Duration::from_millis(ms));
            }
            StepKind::Call => {
                let name = tools::sanitize_name(&fill(&step.target));
                run_custom(cfg, &name, params, apps, speaker).map_err(|e| format!("{where_}: {e}"))?;
            }
            StepKind::Schedule => {
                let text = fill(&step.target);
                let when = fill(&step.args);
                add_item(ItemKind::Reminder, &text, &when, false)?;
            }
        }
    }
    Ok(format!("ran {} steps", tool.steps.len()))
}

thread_local! {
    static NESTING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn nesting() -> u32 {
    NESTING.with(|n| n.get())
}

/// Counts one level of function-calling-function nesting.
struct Nesting;

impl Nesting {
    fn new() -> Nesting {
        NESTING.with(|n| n.set(n.get() + 1));
        Nesting
    }
}

impl Drop for Nesting {
    fn drop(&mut self) {
        NESTING.with(|n| n.set(n.get().saturating_sub(1)));
    }
}

// ── Apps, sites and the browser ──────────────────────────────────────────

fn open_app(name: &str, cfg: &Config, apps: &AppIndex) -> Result<String, String> {
    if let Some((app, score)) = apps.find(name, cfg) {
        log::info!("open \"{name}\" -> {} (score {score:.2})", app.name);
        app.launch()?;
        return Ok(format!("opened {}", app.name));
    }
    // Not installed — maybe it's a website ("open youtube", "open reddit.com").
    if let Some(site) = known_site(name) {
        return open_url(&to_url(site), cfg).map(|_| format!("opened {site}"));
    }
    if looks_like_domain(name) {
        let url = to_url(name);
        return open_url(&url, cfg).map(|_| format!("opened {url}"));
    }
    if cfg.search_when_app_missing {
        let url = cfg.search_url.replace("{}", &fuzzy::url_encode(name));
        return open_url(&url, cfg).map(|_| format!("no app called \"{name}\", searched the web instead"));
    }
    Err(format!("couldn't find an app called \"{name}\""))
}

fn close_app(name: &str, cfg: &Config, apps: &AppIndex) -> Result<String, String> {
    let procs = win::processes();
    let mut exe_names: Vec<String> = Vec::new();
    let mut display = name.to_string();
    if let Some((app, _)) = apps.find(name, cfg) {
        display = app.name.clone();
        if let Some(exe) = app.exe {
            exe_names.push(exe);
        }
    }
    // Also match running process names directly: "close spotify" -> Spotify.exe.
    // The squared spoken name and each process's stem are built once each,
    // rather than squashing the exe name inside a loop that runs over every
    // process on the machine.
    let spoken = fuzzy::squash(&fuzzy::strip_filler(name));
    for p in &procs {
        let stem = fuzzy::squash(without_exe_suffix(&p.exe));
        if !stem.is_empty() && (stem == spoken || strsim::jaro_winkler(&stem, &spoken) > 0.92) {
            exe_names.push(p.exe.clone());
        }
    }
    // Never close the shell or ourselves.
    exe_names.retain(|e| !e.eq_ignore_ascii_case("explorer.exe") && !e.eq_ignore_ascii_case(crate::AGENT_EXE));
    let pids: Vec<u32> = procs
        .iter()
        .filter(|p| exe_names.iter().any(|e| e.eq_ignore_ascii_case(&p.exe)))
        .map(|p| p.pid)
        .collect();
    if pids.is_empty() {
        return Err(format!("{display} isn't running"));
    }
    let closed = win::close_windows_of(&pids);
    if closed == 0 {
        return Err(format!("{display} has no windows to close"));
    }
    Ok(format!("closed {display}"))
}

/// A process name without its `.exe`, whatever case the extension is in.
/// `trim_end_matches` cannot be used for this: it strips every matching
/// character, not one occurrence, so "latex.exe" became "lat".
fn without_exe_suffix(exe: &str) -> &str {
    match exe.len().checked_sub(4) {
        Some(at) if exe[at..].eq_ignore_ascii_case(".exe") => &exe[..at],
        _ => exe,
    }
}

fn to_url(site: &str) -> String {
    let s = site.trim();
    if s.starts_with("http://") || s.starts_with("https://") {
        return s.to_string();
    }
    let s = s.trim_start_matches("www.");
    if s.contains('.') {
        format!("https://{s}")
    } else {
        format!("https://{}.com", fuzzy::squash(s))
    }
}

/// Path to the configured browser, if it isn't "system default".
pub fn browser_exe(cfg: &Config) -> Option<PathBuf> {
    if cfg.browser == Browser::Custom {
        let p = PathBuf::from(cfg.browser_path.trim().trim_matches('"'));
        return p.exists().then_some(p);
    }
    let exe = cfg.browser.exe_name()?;
    if let Some(p) = win::app_path(exe) {
        return Some(p);
    }
    // Common install spots in case App Paths is missing.
    let roots = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"];
    let rel = match cfg.browser {
        Browser::Chrome => r"Google\Chrome\Application\chrome.exe",
        Browser::Edge => r"Microsoft\Edge\Application\msedge.exe",
        Browser::Firefox => r"Mozilla Firefox\firefox.exe",
        Browser::Brave => r"BraveSoftware\Brave-Browser\Application\brave.exe",
        _ => return None,
    };
    roots
        .iter()
        .filter_map(|r| std::env::var_os(r))
        .map(|r| PathBuf::from(r).join(rel))
        .find(|p| p.exists())
}

/// Open a URL in a new tab of the chosen browser (falls back to the default).
pub fn open_url(url: &str, cfg: &Config) -> Result<(), String> {
    if let Some(exe) = browser_exe(cfg) {
        // Passing the URL as an argument opens it as a new tab in the
        // existing window for Chrome / Edge / Brave / Firefox.
        match std::process::Command::new(&exe).arg(url).spawn() {
            Ok(_) => return Ok(()),
            Err(e) => log::warn!("couldn't start {}: {e}", exe.display()),
        }
    }
    win::shell_open(url, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain::Action;
    use crate::tools::{CustomTool, Step, StepKind};
    use std::sync::Mutex;

    /// Collects what a function said, so tests can check the words.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl Speaker for Recorder {
        fn say(&self, text: &str) {
            self.0.lock().unwrap().push(text.to_string());
        }
    }

    /// The override is an environment variable, so tests that use it have to
    /// take turns.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// A schedule file of our own, so tests never touch the real list.
    struct TempSchedule {
        path: PathBuf,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl TempSchedule {
        fn new(tag: &str) -> TempSchedule {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir().join("nv-actions-test");
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(format!("{tag}-{}.json", std::process::id()));
            let _ = std::fs::remove_file(&path);
            std::env::set_var("NV_SCHEDULE_FILE", &path);
            TempSchedule { path, _lock: lock }
        }
    }

    impl Drop for TempSchedule {
        fn drop(&mut self) {
            std::env::remove_var("NV_SCHEDULE_FILE");
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn run(action: Action) -> Result<String, String> {
        let cfg = Config::default();
        let apps = AppIndex::from_names(Vec::new());
        execute(&action, &cfg, &apps)
    }

    #[test]
    fn schedule_lifecycle() {
        let _guard = TempSchedule::new("lifecycle");
        let alarm = run(Action::Alarm { when: "in 20 minutes".into(), label: "pizza".into() }).unwrap();
        assert!(alarm.contains("alarm set for"), "{alarm}");
        assert_eq!(Schedule::open().items.len(), 1);

        let reminder = run(Action::Reminder { text: "call mum".into(), when: "4 pm".into() }).unwrap();
        assert!(reminder.contains("reminder set for"), "{reminder}");
        // A reminder with no time is really a to-do.
        let no_time = run(Action::Reminder { text: "water plants".into(), when: String::new() }).unwrap();
        assert!(no_time.contains("added \"water plants\" to your list"), "{no_time}");

        run(Action::Todo { text: "buy milk".into() }).unwrap();
        let event = run(Action::CalendarEvent { text: "team lunch".into(), when: "in 45 minutes".into() }).unwrap();
        assert!(event.contains("event set for"), "{event}");
        let store = Schedule::open();
        assert_eq!(store.items.len(), 5);
        // Each one landed with the right kind.
        let kinds: Vec<ItemKind> = store.items.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            vec![ItemKind::Alarm, ItemKind::Reminder, ItemKind::Todo, ItemKind::Todo, ItemKind::Event]
        );

        // Reading the list only mentions to-dos.
        let list = run(Action::ShowSchedule { what: "read my list".into() }).unwrap();
        assert!(list.contains("buy milk") && list.contains("water plants"), "{list}");
        assert!(!list.contains("team lunch"), "{list}");
        // Reading everything mentions the event and says what is next.
        let all = run(Action::ShowSchedule { what: "whats on my schedule".into() }).unwrap();
        assert!(all.contains("team lunch"), "{all}");
        assert!(all.contains("Next up"), "{all}");

        let done = run(Action::CompleteTodo { what: "milk".into() }).unwrap();
        assert!(done.contains("ticked off \"buy milk\""), "{done}");
        assert_eq!(Schedule::open().todos(false).len(), 1);

        let cancelled = run(Action::CancelSchedule { what: "mum".into() }).unwrap();
        assert!(cancelled.contains("call mum"), "{cancelled}");
        assert_eq!(Schedule::open().items.len(), 4);
        // Nothing matches a nonsense query.
        assert!(run(Action::CancelSchedule { what: "quantum flux".into() }).is_err());
        // A bad time is reported rather than silently accepted.
        assert!(run(Action::Alarm { when: "whenever".into(), label: String::new() }).is_err());
    }

    #[test]
    fn sequence_steps_run_in_order() {
        let _guard = TempSchedule::new("sequence");
        let mut cfg = Config::default();
        let speaker = Recorder::default();
        let tool = CustomTool {
            name: "sequence_test".into(),
            description: "test".into(),
            kind: CustomKind::Sequence,
            steps: vec![
                Step { kind: StepKind::Say, target: "starting {thing}".into(), args: String::new() },
                Step { kind: StepKind::PowerShell, target: "Write-Output (6*7)".into(), args: String::new() },
                Step { kind: StepKind::Say, target: "the answer is {output}".into(), args: String::new() },
                Step { kind: StepKind::Schedule, target: "check the oven".into(), args: "in 30 minutes".into() },
            ],
            ..Default::default()
        };
        cfg.custom_tools = vec![tool];
        let apps = AppIndex::from_names(Vec::new());
        let msg = execute_with(
            &Action::Custom { name: "sequence_test".into(), params: vec![("thing".into(), "the oven".into())] },
            &cfg,
            &apps,
            &speaker,
        )
        .unwrap();
        assert_eq!(msg, "ran 4 steps");
        let said = speaker.0.lock().unwrap().clone();
        assert_eq!(said[0], "starting the oven");
        assert_eq!(said[1], "the answer is 42");
        // The schedule step added the reminder.
        let items = Schedule::open();
        assert_eq!(items.items.len(), 1);
        assert_eq!(items.items[0].text, "check the oven");
        assert_eq!(items.items[0].kind, ItemKind::Reminder);
    }

    #[test]
    fn python_output_becomes_the_answer() {
        let Some(python) = crate::win::find_python("") else {
            eprintln!("python not installed; skipping");
            return;
        };
        let dir = std::env::temp_dir().join("nv-python-test");
        let _ = std::fs::create_dir_all(&dir);
        let script = dir.join("answer.py");
        std::fs::write(&script, "import sys, os\nprint('param', os.environ.get('NV_PARAM_QUESTION','?'))\nprint('the answer is 42', sys.argv[1] if len(sys.argv)>1 else '')\n").unwrap();

        let mut cfg = Config::default();
        cfg.python_path = python.display().to_string();
        cfg.custom_tools = vec![CustomTool {
            name: "python_test".into(),
            description: "test".into(),
            params: vec!["question".into()],
            kind: CustomKind::Python,
            target: script.display().to_string(),
            ..Default::default()
        }];
        let apps = AppIndex::from_names(Vec::new());
        let out = execute(
            &Action::Custom { name: "python_test".into(), params: vec![("question".into(), "life".into())] },
            &cfg,
            &apps,
        )
        .unwrap();
        assert_eq!(out, "the answer is 42 life", "python said: {out}");
        let _ = std::fs::remove_file(&script);
    }

    #[test]
    fn broken_python_is_reported() {
        let mut cfg = Config::default();
        cfg.custom_tools = vec![CustomTool {
            name: "nope".into(),
            kind: CustomKind::Python,
            target: "definitely-missing.py".into(),
            ..Default::default()
        }];
        let apps = AppIndex::from_names(Vec::new());
        let err = execute(&Action::Custom { name: "nope".into(), params: vec![] }, &cfg, &apps).unwrap_err();
        assert!(err.contains("definitely-missing.py"), "{err}");
    }

    /// Process names lose exactly their `.exe`, not every trailing "e"/"x".
    #[test]
    fn process_names_lose_one_suffix() {
        assert_eq!(without_exe_suffix("Spotify.exe"), "Spotify");
        assert_eq!(without_exe_suffix("SPOTIFY.EXE"), "SPOTIFY");
        assert_eq!(without_exe_suffix("latex.exe"), "latex");
        assert_eq!(without_exe_suffix("code.exe"), "code");
        assert_eq!(without_exe_suffix("NeedleVoice"), "NeedleVoice");
        assert_eq!(without_exe_suffix(".exe"), "");
        assert_eq!(without_exe_suffix("exe"), "exe");
    }

    /// "open youtube" and "go to reddit.com" both have to end up at a URL the
    /// browser can open, without turning a bare word into one.
    #[test]
    fn url_shapes() {
        assert_eq!(to_url("reddit.com"), "https://reddit.com");
        assert_eq!(to_url("https://example.com/x"), "https://example.com/x");
        assert_eq!(to_url("www.example.com"), "https://example.com");
        assert_eq!(to_url("YouTube"), "https://youtube.com");
    }
}
