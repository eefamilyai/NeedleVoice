//! NeedleVoice settings app.
#![windows_subsystem = "windows"]

mod jobs;
mod miccheck;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use eframe::egui::{self, Color32, RichText, Stroke};
use jobs::Job;
use nv_core::apps::AppIndex;
use nv_core::brain::Brain;
use nv_core::config::{parse_hex, Alias, Browser, ACCENT_PRESETS};
use nv_core::schedule::{describe_when, Item, ItemKind, Repeat, Schedule, Stamp};
use nv_core::tools::{self, CustomKind, CustomTool, Step, StepKind};
use nv_core::personality::{self, Moment, Persona};
use nv_core::voices::{self, Engine};
use nv_core::{win, Config};

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    General,
    Voice,
    Listening,
    Schedule,
    Brain,
    Browser,
    Apps,
    Functions,
    Test,
}

const TABS: [(Tab, &str); 9] = [
    (Tab::General, "✨  General"),
    (Tab::Voice, "🗣  Voice & personality"),
    (Tab::Listening, "🎙  Listening"),
    (Tab::Schedule, "⏰  Alarms & reminders"),
    (Tab::Brain, "🧠  Brain"),
    (Tab::Browser, "🌐  Browser"),
    (Tab::Apps, "📦  Apps"),
    (Tab::Functions, "⚙  Functions"),
    (Tab::Test, "🧪  Test a command"),
];

/// The half-filled "add to the schedule" form.
struct NewItem {
    kind: ItemKind,
    text: String,
    when: String,
    repeat: Repeat,
    action: String,
    chime: bool,
    speak: bool,
}

impl Default for NewItem {
    fn default() -> Self {
        NewItem {
            kind: ItemKind::Alarm,
            text: String::new(),
            when: String::new(),
            repeat: Repeat::Once,
            action: String::new(),
            chime: true,
            speak: true,
        }
    }
}

const SEARCH_ENGINES: [(&str, &str); 4] = [
    ("Google", "https://www.google.com/search?q={}"),
    ("Bing", "https://www.bing.com/search?q={}"),
    ("DuckDuckGo", "https://duckduckgo.com/?q={}"),
    ("Brave Search", "https://search.brave.com/search?q={}"),
];

const WHISPER_MODELS: [(&str, &str, u32); 3] = [
    ("ggml-tiny.en-q5_1.bin", "Tiny — fastest (default)", 31),
    ("ggml-base.en-q5_1.bin", "Base — more accurate, ~2× slower", 57),
    ("ggml-small.en-q5_1.bin", "Small — most accurate, ~6× slower", 181),
];

struct App {
    cfg: Config,
    /// Where settings are read from and written to. `--config <file>` overrides
    /// the usual `%APPDATA%\NeedleVoice\config.toml`, which is handy for
    /// testing a set of functions without touching the live ones.
    cfg_file: PathBuf,
    saved: Config,
    /// Set when the settings file exists but couldn't be read, so it is
    /// obvious why everything suddenly looks like a fresh install.
    load_error: Option<String>,
    tab: Tab,
    status: Option<(String, Instant, bool)>,
    agent_running: bool,
    last_poll: Instant,

    mics: Vec<String>,
    system_voices: Vec<String>,
    apps: AppIndex,
    app_filter: String,
    new_alias: (String, String),

    jobs: Vec<(String, Job)>,
    test_input: String,
    test_result: Option<String>,
    test_job: Option<Job>,
    brain: std::sync::Arc<std::sync::Mutex<Option<Brain>>>,
    mic_check: Option<miccheck::MicCheck>,

    /// The wake-word sub-word tokeniser, loaded once (None when the model
    /// isn't installed).
    bpe: Option<nv_core::bpe::Bpe>,
    /// The "add to the schedule" form.
    new_item: NewItem,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let args: Vec<String> = std::env::args().collect();
        let cfg_file = args
            .iter()
            .position(|a| a == "--config")
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
            .unwrap_or_else(nv_core::paths::config_file);
        let (cfg, load_error) = match Config::try_load_from(&cfg_file) {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(e)),
        };
        apply_theme(&cc.egui_ctx, cfg.accent_rgb());
        install_fonts(&cc.egui_ctx);
        let tab = match args.iter().skip_while(|a| *a != "--tab").nth(1).map(String::as_str) {
            Some("voice") => Tab::Voice,
            Some("listening") => Tab::Listening,
            Some("schedule") | Some("alarms") => Tab::Schedule,
            Some("brain") => Tab::Brain,
            Some("browser") => Tab::Browser,
            Some("apps") => Tab::Apps,
            Some("functions") => Tab::Functions,
            Some("test") => Tab::Test,
            _ => Tab::General,
        };
        let mics = cpal::default_host()
            .input_devices()
            .map(|it| it.filter_map(|d| d.description().ok().map(|d| d.name().to_string())).collect())
            .unwrap_or_default();
        win::com_init();
        let system_voices = nv_core::voice::list_voices();
        Self {
            saved: cfg.clone(),
            cfg,
            load_error,
            cfg_file,
            tab,
            status: None,
            agent_running: win::is_running(nv_core::AGENT_EXE),
            last_poll: Instant::now(),
            mics,
            system_voices,
            apps: AppIndex::load_cache().unwrap_or_default(),
            app_filter: String::new(),
            new_alias: Default::default(),
            jobs: Vec::new(),
            test_input: "what is the haber process".into(),
            test_result: None,
            test_job: None,
            brain: Default::default(),
            mic_check: None,
            bpe: nv_core::bpe::Bpe::load(&nv_core::wake_model::dir().join("bpe.model")),
            new_item: NewItem::default(),
        }
    }

    fn toast(&mut self, msg: impl Into<String>, ok: bool) {
        self.status = Some((msg.into(), Instant::now(), ok));
    }

    fn agent_exe() -> PathBuf {
        nv_core::paths::sibling_exe(nv_core::AGENT_EXE)
    }

    fn save(&mut self) {
        self.cfg = self.cfg.clone().sanitized();
        match self.cfg.save_to(&self.cfg_file) {
            Ok(()) => {
                if let Err(e) = win::set_autostart(self.cfg.start_with_windows, &Self::agent_exe()) {
                    self.toast(format!("Saved, but couldn't set auto-start: {e}"), false);
                } else if self.agent_running {
                    self.toast("Saved — the assistant is restarting with your changes", true);
                } else {
                    self.toast("Saved", true);
                }
                self.saved = self.cfg.clone();
                self.load_error = None;
            }
            Err(e) => self.toast(format!("Couldn't save: {e}"), false),
        }
    }

    fn start_agent(&mut self) {
        match win::spawn_detached(&Self::agent_exe(), &[]) {
            Ok(()) => self.toast("Assistant started", true),
            Err(e) => self.toast(format!("Couldn't start {}: {e}", Self::agent_exe().display()), false),
        }
    }

    fn job_running(&self, key: &str) -> Option<&Job> {
        self.jobs.iter().find(|(k, j)| k == key && !j.done()).map(|(_, j)| j)
    }

    fn start_job(
        &mut self,
        ctx: &egui::Context,
        key: &str,
        label: &str,
        work: impl FnOnce(&dyn Fn(f32)) -> Result<String, String> + Send + 'static,
    ) {
        if self.job_running(key).is_some() {
            return;
        }
        self.jobs.retain(|(k, _)| k != key);
        self.jobs.push((key.into(), Job::spawn(ctx, label, work)));
    }

    /// Collect finished jobs into toasts.
    fn poll_jobs(&mut self) {
        let mut finished = Vec::new();
        self.jobs.retain(|(k, j)| {
            let st = j.0.lock().unwrap();
            match &st.result {
                Some(r) => {
                    finished.push((k.clone(), r.clone()));
                    false
                }
                None => true,
            }
        });
        for (key, r) in finished {
            match r {
                Ok(msg) => {
                    if key == "scan" {
                        self.apps = AppIndex::load_cache().unwrap_or_default();
                    }
                    if key == "wake_model" {
                        // Pick up the freshly installed tokeniser right away.
                        self.bpe = nv_core::bpe::Bpe::load(&nv_core::wake_model::dir().join("bpe.model"));
                    }
                    self.toast(msg, true)
                }
                Err(e) => self.toast(e, false),
            }
        }
    }

    fn preview_voice(&mut self, text: Option<String>) {
        let text = text.unwrap_or_else(|| {
            let a = nv_core::brain::Action::OpenApp("Google Chrome".into());
            personality::line(self.cfg.personality, &Moment::Done(&a))
        });
        let tmp = std::env::temp_dir().join("needlevoice-preview.toml");
        let mut c = self.cfg.clone();
        c.voice_enabled = true;
        if let Err(e) = c.save_to(&tmp) {
            self.toast(format!("Preview failed: {e}"), false);
            return;
        }
        let tmp_s = tmp.display().to_string();
        if let Err(e) = win::spawn_detached(&Self::agent_exe(), &["--say", &text, "--config", &tmp_s]) {
            self.toast(format!("Preview failed: {e}"), false);
        } else {
            self.toast("Playing preview… (neural voices take a second to load)", true);
        }
    }
}

fn accent32((r, g, b): (f32, f32, f32)) -> Color32 {
    Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

fn apply_theme(ctx: &egui::Context, accent: (f32, f32, f32)) {
    let a = accent32(accent);
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::from_rgb(13, 15, 20);
    v.window_fill = Color32::from_rgb(18, 21, 28);
    v.extreme_bg_color = Color32::from_rgb(8, 9, 13);
    v.faint_bg_color = Color32::from_rgb(22, 25, 33);
    v.selection.bg_fill = a.linear_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, a);
    v.hyperlink_color = a;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, a.linear_multiply(0.7));
    v.widgets.active.bg_stroke = Stroke::new(1.5, a);
    v.widgets.inactive.corner_radius = 6.0.into();
    v.widgets.hovered.corner_radius = 6.0.into();
    v.widgets.active.corner_radius = 6.0.into();
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(10.0, 9.0);
        s.spacing.button_padding = egui::vec2(12.0, 5.0);
    });
}

/// `media_pause` → "Media pause": the technical name stays in the tooltip.
fn pretty_name(s: &str) -> String {
    let spaced = s.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => spaced,
    }
}

fn heading(ui: &mut egui::Ui, text: &str, accent: Color32) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).size(20.0).strong().color(accent));
    ui.add_space(2.0);
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(Color32::from_gray(140)));
}

fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .fill(Color32::from_rgb(18, 21, 28))
        .stroke(Stroke::new(1.0, Color32::from_rgb(34, 38, 48)))
        .corner_radius(10.0)
        .inner_margin(14.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).strong().size(15.0));
            ui.add_space(4.0);
            body(ui);
        });
    ui.add_space(8.0);
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.last_poll.elapsed() > Duration::from_secs(2) {
            self.agent_running = win::is_running(nv_core::AGENT_EXE);
            self.last_poll = Instant::now();
        }
        self.poll_jobs();
        // Mic meters run only while the Listening tab is open.
        if self.tab == Tab::Listening {
            if self.mic_check.is_none() {
                self.mic_check = Some(miccheck::MicCheck::open());
            }
            ctx.request_repaint_after(miccheck::repaint());
        } else if self.mic_check.as_ref().is_some_and(|m| m.phase == miccheck::Phase::Idle) {
            self.mic_check = None;
        }
        if !self.jobs.is_empty() || self.test_job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else {
            ctx.request_repaint_after(Duration::from_secs(2));
        }
        let accent = accent32(self.cfg.accent_rgb());

        // ── sidebar ──
        egui::Panel::left("tabs").exact_size(220.0).resizable(false).show(ui, |ui| {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::hover());
                orb(ui, rect, accent);
                ui.label(RichText::new("NeedleVoice").size(19.0).strong());
            });
            hint(ui, &format!("Say \"{}\"", self.cfg.wake_phrase()));
            ui.add_space(14.0);
            for (tab, label) in TABS {
                let sel = self.tab == tab;
                let text = RichText::new(label).size(14.5).color(if sel { accent } else { Color32::from_gray(200) });
                if ui.add_sized([200.0, 32.0], egui::Button::selectable(sel, text)).clicked() {
                    self.tab = tab;
                }
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if self.agent_running {
                        if ui.button("Stop").clicked() {
                            win::kill_by_exe(nv_core::AGENT_EXE);
                            self.agent_running = false;
                        }
                        if ui.button("Restart").clicked() {
                            win::kill_by_exe(nv_core::AGENT_EXE);
                            std::thread::sleep(Duration::from_millis(300));
                            self.start_agent();
                        }
                    } else if ui.button("▶ Start assistant").clicked() {
                        self.start_agent();
                        self.agent_running = true;
                    }
                });
                let (dot, txt) = if self.agent_running {
                    (accent, if self.cfg.paused { "Running (paused)" } else { "Running & listening" })
                } else {
                    (Color32::from_rgb(255, 80, 90), "Not running")
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(dot));
                    ui.label(txt);
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Brain: Needle 3 by Cactus Compute (Apache-2.0)").small().color(Color32::from_gray(120)),
                );
                ui.label(
                    RichText::new("Speech: whisper.cpp · sherpa-onnx · Piper · Kokoro").small().color(Color32::from_gray(120)),
                );
                if ui
                    .add(egui::Button::new(RichText::new("Credits").small()).frame(false))
                    .on_hover_text("Full list, licences and the citation in CREDITS.md")
                    .clicked()
                {
                    let _ = win::shell_open("https://huggingface.co/Cactus-Compute/needle3", None);
                }
            });
        });

        // ── bottom bar ──
        egui::Panel::bottom("bar").exact_size(52.0).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let dirty = self.cfg != self.saved;
                let save = egui::Button::new(RichText::new("💾  Save & apply").strong().color(Color32::BLACK))
                    .fill(if dirty { accent } else { accent.linear_multiply(0.4) });
                if ui.add_enabled(dirty, save).clicked() {
                    self.save();
                    apply_theme(&ctx, self.cfg.accent_rgb());
                }
                if dirty && ui.button("Undo changes").clicked() {
                    self.cfg = self.saved.clone();
                }
                if let Some(e) = &self.load_error {
                    ui.label(
                        RichText::new(format!("⚠ {e} — showing defaults. Saving will overwrite it."))
                            .color(Color32::from_rgb(255, 95, 105)),
                    );
                }
                if let Some((msg, at, ok)) = &self.status {
                    if at.elapsed() < Duration::from_secs(8) {
                        let c = if *ok { accent } else { Color32::from_rgb(255, 95, 105) };
                        ui.label(RichText::new(msg).color(c));
                    }
                }
                for (_, job) in &self.jobs {
                    let st = job.0.lock().unwrap();
                    ui.separator();
                    ui.label(&st.label);
                    if st.progress >= 0.0 {
                        ui.add(egui::ProgressBar::new(st.progress).desired_width(140.0).show_percentage());
                    } else {
                        ui.spinner();
                    }
                }
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(6.0);
                match self.tab {
                    Tab::General => self.tab_general(ui, accent),
                    Tab::Voice => self.tab_voice(ui, &ctx, accent),
                    Tab::Listening => self.tab_listening(ui, &ctx, accent),
                    Tab::Schedule => self.tab_schedule(ui, accent),
                    Tab::Brain => self.tab_brain(ui, accent),
                    Tab::Browser => self.tab_browser(ui, accent),
                    Tab::Apps => self.tab_apps(ui, &ctx, accent),
                    Tab::Functions => self.tab_functions(ui, &ctx, accent),
                    Tab::Test => self.tab_test(ui, &ctx, accent),
                }
            });
        });
    }
}

/// A tiny neon orb for the header and colour swatches.
fn orb(ui: &egui::Ui, rect: egui::Rect, c: Color32) {
    let p = ui.painter();
    let center = rect.center();
    let r = rect.width().min(rect.height()) / 2.0;
    p.circle_filled(center, r, c.linear_multiply(0.18));
    p.circle_filled(center, r * 0.78, Color32::from_rgb(10, 11, 16));
    p.circle_filled(center + egui::vec2(-r * 0.2, -r * 0.15), r * 0.42, c.linear_multiply(0.75));
    p.circle_stroke(center, r * 0.78, Stroke::new(1.6, c));
}

impl App {
    fn tab_general(&mut self, ui: &mut egui::Ui, accent: Color32) {
        heading(ui, "General", accent);
        section(ui, "Wake word", |ui| {
            ui.horizontal(|ui| {
                ui.label("Assistant name");
                ui.add(egui::TextEdit::singleline(&mut self.cfg.agent_name).desired_width(160.0));
            });
            hint(ui, "Pick something distinctive with 2+ syllables (e.g. Nova, Jarvis, Echo) — it's harder to trigger by accident.");
            let mut prefixes = self.cfg.wake_prefixes.join(", ");
            ui.horizontal(|ui| {
                ui.label("Words before the name");
                if ui.add(egui::TextEdit::singleline(&mut prefixes).desired_width(220.0)).changed() {
                    self.cfg.wake_prefixes =
                        prefixes.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
                }
            });
            ui.checkbox(&mut self.cfg.allow_name_only, "Also wake on just the name (no \"hey\")");
            ui.horizontal(|ui| {
                ui.label("Also answer to");
                let mut extra = self.cfg.wake_extra_names.join(", ");
                if ui
                    .add(egui::TextEdit::singleline(&mut extra).hint_text("no va, hey novah").desired_width(250.0))
                    .changed()
                {
                    self.cfg.wake_extra_names =
                        extra.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
                }
            });
            hint(ui, "Comma-separated extra spellings. If it keeps mis-hearing your accent, add what it hears here — no retraining needed.");
            ui.add(egui::Slider::new(&mut self.cfg.wake_sensitivity, 0.0..=1.0).text("Sensitivity"));
            hint(ui, "Higher = wakes more easily, but also more false alarms.");
            ui.checkbox(&mut self.cfg.wake_sound, "Play a chime the moment it hears you");
            ui.checkbox(&mut self.cfg.wake_reply, "Say a quick \"Yes?\" when it wakes");
            hint(ui, "The bubble and the chime appear the instant the name is recognised. A spoken reply is even clearer, but it talks over the first word of your command.");
            self.wake_advanced(ui);
        });
        section(ui, "Bubble", |ui| {
            ui.checkbox(&mut self.cfg.show_overlay, "Show the listening bubble at the bottom of the screen");
            ui.label("Neon colour");
            ui.horizontal_wrapped(|ui| {
                for (name, hex) in ACCENT_PRESETS {
                    let (r, g, b) = parse_hex(hex).unwrap();
                    let c = accent32((r, g, b));
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(46.0, 46.0), egui::Sense::click());
                    orb(ui, rect.shrink(4.0), c);
                    if self.cfg.accent_color.eq_ignore_ascii_case(hex) {
                        ui.painter().circle_stroke(rect.center(), 22.0, Stroke::new(2.0, Color32::WHITE));
                    }
                    if resp.on_hover_text(*name).clicked() {
                        self.cfg.accent_color = hex.to_string();
                    }
                }
                let (r, g, b) = self.cfg.accent_rgb();
                let mut rgb = [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8];
                if ui.color_edit_button_srgb(&mut rgb).on_hover_text("Custom colour").changed() {
                    self.cfg.accent_color = format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
                }
            });
            ui.add(egui::Slider::new(&mut self.cfg.overlay_size, 40..=160).text("Size (px)"));
            ui.add(egui::Slider::new(&mut self.cfg.overlay_margin, 0..=300).text("Distance from bottom (px)"));
        });
        section(ui, "System", |ui| {
            ui.checkbox(&mut self.cfg.start_with_windows, "Start automatically when I sign in to Windows");
            ui.checkbox(&mut self.cfg.paused, "Paused (microphone off)");
            ui.horizontal(|ui| {
                if ui.button("Open log file").clicked() {
                    let _ = win::shell_open(&nv_core::paths::log_file().display().to_string(), None);
                }
                if ui.button("Open settings folder").clicked() {
                    let _ = win::shell_open(&nv_core::paths::data_dir().display().to_string(), None);
                }
            });
        });
    }

    fn tab_voice(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, accent: Color32) {
        heading(ui, "Voice & personality", accent);
        section(ui, "Personality", |ui| {
            for p in Persona::ALL {
                ui.radio_value(&mut self.cfg.personality, p, p.label());
            }
            let a = nv_core::brain::Action::WebSearch("the Haber process".into());
            let sample = personality::example(self.cfg.personality, &Moment::Done(&a));
            ui.label(RichText::new(format!("e.g. \"{sample}\"")).italics().color(Color32::from_gray(170)));
        });

        let mut preview = false;
        section(ui, "Voice", |ui| {
            ui.checkbox(&mut self.cfg.voice_enabled, "Talk back out loud");
            ui.add_enabled_ui(self.cfg.voice_enabled, |ui| {
                for e in Engine::ALL {
                    ui.radio_value(&mut self.cfg.voice_engine, e, e.label());
                }
                ui.add(egui::Slider::new(&mut self.cfg.voice_speed, 0.6..=1.6).text("Speed"));
                ui.add(egui::Slider::new(&mut self.cfg.voice_volume, 0..=100).text("Volume"));
                if ui.button("▶  Preview voice").clicked() {
                    preview = true;
                }
            });
        });
        if preview {
            self.preview_voice(None);
        }

        if !self.cfg.voice_enabled {
            return;
        }
        match self.cfg.voice_engine {
            Engine::Piper => self.piper_list(ui, ctx, accent),
            Engine::Kokoro => self.kokoro_panel(ui, ctx, accent),
            Engine::System => {
                section(ui, "Windows voice", |ui| {
                    egui::ComboBox::from_id_salt("sysvoice")
                        .width(360.0)
                        .selected_text(if self.cfg.system_voice.is_empty() { "System default" } else { &self.cfg.system_voice })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.cfg.system_voice, String::new(), "System default");
                            for v in &self.system_voices {
                                ui.selectable_value(&mut self.cfg.system_voice, v.clone(), v);
                            }
                        });
                    hint(ui, "These are the classic Windows voices. For a human-sounding voice choose Natural or Ultra-realistic above.");
                });
            }
        }
    }

    fn piper_list(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, accent: Color32) {
        let mut to_download: Option<&'static str> = None;
        let mut to_preview: Option<String> = None;
        let mut to_remove: Option<&'static str> = None;
        section(ui, "Natural voices", |ui| {
            hint(ui, "Click Get to download a voice (about 64 MB each). Downloaded voices work offline.");
            for v in voices::PIPER_VOICES {
                let installed = voices::is_installed(v.id);
                let selected = self.cfg.piper_voice == v.id;
                ui.horizontal(|ui| {
                    let label = RichText::new(v.label).color(if selected { accent } else { Color32::from_gray(220) });
                    if ui.add_enabled(installed, egui::Button::selectable(selected, label)).clicked() {
                        self.cfg.piper_voice = v.id.into();
                        to_preview = Some(v.id.into());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if installed {
                            if v.id != voices::DEFAULT_PIPER && !selected && ui.small_button("🗑").on_hover_text("Delete").clicked() {
                                to_remove = Some(v.id);
                            }
                            ui.label(RichText::new("✔ installed").small().color(accent));
                        } else if let Some(job) = self.job_running(v.id) {
                            let p = job.0.lock().unwrap().progress;
                            ui.add(egui::ProgressBar::new(p.max(0.0)).desired_width(110.0).show_percentage());
                        } else if ui.button(format!("⬇ Get ({} MB)", v.size_mb)).clicked() {
                            to_download = Some(v.id);
                        }
                    });
                });
            }
        });
        if let Some(id) = to_download {
            self.start_job(ctx, id, &format!("Downloading {id}"), move |p| {
                voices::download(id, |d, t| p(if t > 0 { d as f32 / t as f32 } else { -1.0 }))
                    .map(|_| "Voice downloaded — click it to select".to_string())
            });
        }
        if let Some(id) = to_remove {
            let _ = voices::remove(id);
        }
        if to_preview.is_some() {
            self.preview_voice(None);
        }
    }

    fn kokoro_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, accent: Color32) {
        let installed = voices::is_installed(voices::KOKORO_PACK);
        let mut download = false;
        let mut preview = false;
        section(ui, "Ultra-realistic voices (Kokoro)", |ui| {
            if !installed {
                ui.label(format!(
                    "Kokoro is the most human-sounding offline voice engine. It's a one-time {} MB download with {} English voices.",
                    voices::KOKORO_SIZE_MB,
                    voices::KOKORO_VOICES.len()
                ));
                hint(ui, "It needs more CPU: expect about 1–3 seconds before it starts talking on an average PC.");
                if let Some(job) = self.job_running(voices::KOKORO_PACK) {
                    let p = job.0.lock().unwrap().progress;
                    ui.add(egui::ProgressBar::new(p.max(0.0)).show_percentage());
                } else if ui.button(format!("⬇ Download Kokoro ({} MB)", voices::KOKORO_SIZE_MB)).clicked() {
                    download = true;
                }
                return;
            }
            hint(ui, "★ = community favourites. Click a voice to select it and hear a preview.");
            egui::Grid::new("kokoro").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                for (i, v) in voices::KOKORO_VOICES.iter().enumerate() {
                    let sel = self.cfg.kokoro_voice == v.name;
                    let text = RichText::new(v.label).color(if sel { accent } else { Color32::from_gray(220) });
                    if ui.add(egui::Button::selectable(sel, text)).clicked() {
                        self.cfg.kokoro_voice = v.name.into();
                        preview = true;
                    }
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
        });
        if download {
            self.start_job(ctx, voices::KOKORO_PACK, "Downloading Kokoro voices", |p| {
                voices::download(voices::KOKORO_PACK, |d, t| p(if t > 0 { d as f32 / t as f32 } else { -1.0 }))
                    .map(|_| "Kokoro installed — pick a voice".to_string())
            });
        }
        if preview {
            self.preview_voice(None);
        }
    }

    fn tab_listening(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, accent: Color32) {
        heading(ui, "Listening", accent);
        self.wake_model_ui(ui, ctx, accent);
        self.mic_check_ui(ui, ctx, accent);
        section(ui, "Microphone", |ui| {
            egui::ComboBox::from_id_salt("mic")
                .width(360.0)
                .selected_text(if self.cfg.microphone.is_empty() { "Windows default microphone" } else { &self.cfg.microphone })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.cfg.microphone, String::new(), "Windows default microphone");
                    for m in &self.mics {
                        ui.selectable_value(&mut self.cfg.microphone, m.clone(), m);
                    }
                });
            ui.add(egui::Slider::new(&mut self.cfg.vad_aggressiveness, 0..=3).text("Noise filtering"));
            hint(ui, "Higher ignores more background noise (TV, fans) but may miss quiet speech.");
            ui.add(egui::Slider::new(&mut self.cfg.mic_gain_db, 0.0..=40.0).text("Mic boost (dB)"));
            ui.add(egui::Slider::new(&mut self.cfg.min_speech_db, -75.0..=-20.0).text("Minimum loudness (dB)"));
            hint(ui, "Tip: the Microphone check above sets these for you.");
            ui.add(egui::Slider::new(&mut self.cfg.end_silence_ms, 300..=2500).text("Pause that ends a command (ms)"));
            ui.add(egui::Slider::new(&mut self.cfg.command_timeout_secs, 2.0..=15.0).text("Wait for a command after waking (s)"));
        });
        let models = nv_core::paths::models_dir();
        let mut download: Option<(&'static str, u32)> = None;
        section(ui, "Speech recognition (Whisper)", |ui| {
            for (file, label, mb) in WHISPER_MODELS {
                let have = models.join(file).exists();
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(have, |ui| {
                        ui.radio_value(&mut self.cfg.whisper_model, file.to_string(), label);
                    });
                    if !have {
                        if let Some(job) = self.job_running(file) {
                            let p = job.0.lock().unwrap().progress;
                            ui.add(egui::ProgressBar::new(p.max(0.0)).desired_width(110.0).show_percentage());
                        } else if ui.small_button(format!("⬇ Get ({mb} MB)")).clicked() {
                            download = Some((file, mb));
                        }
                    }
                });
            }
        });
        if let Some((file, _)) = download {
            let dest = models.join(file);
            self.start_job(ctx, file, &format!("Downloading {file}"), move |p| {
                let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}");
                jobs::download_file(&url, &dest, p).map(|_| format!("{file} downloaded"))
            });
        }
    }

    fn tab_brain(&mut self, ui: &mut egui::Ui, accent: Color32) {
        heading(ui, "Brain (Needle 3)", accent);
        section(ui, "Speed vs. smarts", |ui| {
            ui.add(egui::Slider::new(&mut self.cfg.needle_depth, 6..=20).text("Needle depth (layers)"));
            hint(ui, "20 = full model (best). 12 ≈ 35% faster with nearly the same accuracy. Below 8 it starts making mistakes.");
            ui.checkbox(&mut self.cfg.instant_commands, "Instant commands — skip the model for obvious requests like \"open chrome\"");
            ui.checkbox(&mut self.cfg.search_when_app_missing, "If an app isn't installed, search the web for it");
        });
        section(ui, "Performance", |ui| {
            let max = std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(8);
            ui.add(egui::Slider::new(&mut self.cfg.threads, 1..=max).text("CPU threads"));
            ui.add(egui::Slider::new(&mut self.cfg.unload_after_secs, 10..=600).text("Free memory after idle (s)"));
            hint(ui, "Models are loaded only while needed. Idle, the assistant uses ~40 MB of RAM and almost no CPU.");
        });
    }

    /// Alarms, reminders, to-dos and calendar events.
    fn tab_schedule(&mut self, ui: &mut egui::Ui, accent: Color32) {
        heading(ui, "Alarms & reminders", accent);
        let now = Stamp::now();
        let mut schedule = Schedule::open();
        let names: Vec<String> = self.cfg.custom_tools.iter().map(|t| t.name.clone()).collect();
        let mut dirty = false;

        // ── what's next ────────────────────────────────────────────────
        let next = schedule.next_up(now).map(|(at, item)| {
            format!("{} — {} at {}", item.kind.label(), item.text, describe_when(at, item.repeat, now))
        });
        section(ui, "Next up", |ui| {
            match &next {
                Some(line) => {
                    ui.label(RichText::new(line).color(accent).size(15.0));
                }
                None => hint(ui, "Nothing scheduled. Say \"hey <name>, set an alarm for 7:30 am\" or add one below."),
            }
            let todos = schedule.todos(false).len();
            if todos > 0 {
                hint(ui, &format!("{todos} thing(s) on the to-do list"));
            }
        });

        // ── add one ────────────────────────────────────────────────────
        section(ui, "Add", |ui| {
            ui.horizontal(|ui| {
                for kind in ItemKind::ALL {
                    ui.selectable_value(&mut self.new_item.kind, kind, kind.label());
                }
            });
            ui.horizontal(|ui| {
                ui.label("what");
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_item.text)
                        .hint_text(match self.new_item.kind {
                            ItemKind::Alarm => "wake up",
                            ItemKind::Reminder => "call mum",
                            ItemKind::Todo => "buy milk",
                            ItemKind::Event => "team lunch",
                        })
                        .desired_width(300.0),
                );
            });
            if self.new_item.kind.timed() {
                ui.horizontal(|ui| {
                    ui.label("when");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_item.when)
                            .hint_text("7:30 am · in 20 minutes · tomorrow at 9 · every monday at 8")
                            .desired_width(340.0),
                    );
                });
                // Say what we understood, as it is typed.
                if self.new_item.when.trim().is_empty() {
                    hint(ui, "Spoken times work as written: \"in 20 minutes\", \"tonight\", \"every weekday at 8\".");
                } else {
                    match nv_core::schedule_parse::parse(&self.new_item.when, now) {
                        Ok((at, repeat)) => {
                            let text = describe_when(at, repeat, now);
                            ui.label(RichText::new(format!("→ {text}")).color(accent));
                        }
                        Err(e) => {
                            ui.label(RichText::new(format!("→ {e}")).color(Color32::from_rgb(255, 150, 80)));
                        }
                    }
                }
            }
            if self.new_item.kind.timed() {
                ui.horizontal(|ui| {
                    ui.label("repeat");
                    egui::ComboBox::from_id_salt("new-repeat")
                        .width(180.0)
                        .selected_text(self.new_item.repeat.label())
                        .show_ui(ui, |ui| {
                            for r in Repeat::ALL {
                                if ui.selectable_value(&mut self.new_item.repeat, r, r.label()).clicked() {
                                    // A repeating item always has a time.
                                    if self.new_item.when.trim().is_empty() {
                                        self.new_item.when = "9 am".into();
                                    }
                                }
                            }
                        });
                });
            }
            ui.horizontal(|ui| {
                ui.label("run a function");
                egui::ComboBox::from_id_salt("new-action")
                    .width(220.0)
                    .selected_text(if self.new_item.action.is_empty() { "(nothing)" } else { &self.new_item.action })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.new_item.action, String::new(), "(nothing)");
                        for name in &names {
                            ui.selectable_value(&mut self.new_item.action, name.clone(), pretty_name(name));
                        }
                    });
                ui.checkbox(&mut self.new_item.chime, "chime");
                ui.checkbox(&mut self.new_item.speak, "speak");
            });
            hint(ui, "A function runs when it goes off, e.g. an alarm that opens your work apps.");

            let needs_time = self.new_item.kind.timed();
            let ready = !self.new_item.text.trim().is_empty()
                && (!needs_time || !self.new_item.when.trim().is_empty())
                && nv_core::schedule_parse::parse(&self.new_item.when, now).is_ok();
            if ui.add_enabled(ready, egui::Button::new("＋  Add")).clicked() {
                match nv_core::schedule_parse::parse(&self.new_item.when, now) {
                    Ok((at, repeat)) => {
                        let repeat = if needs_time { repeat } else { Repeat::Once };
                        let id = schedule.add(Item {
                            kind: self.new_item.kind,
                            text: self.new_item.text.trim().to_string(),
                            at: needs_time.then_some(at),
                            repeat,
                            action: self.new_item.action.clone(),
                            chime: self.new_item.chime,
                            speak: self.new_item.speak,
                            ..Default::default()
                        });
                        let _ = id;
                        dirty = true;
                        let label = self.new_item.text.trim().to_string();
                        self.new_item = NewItem { kind: self.new_item.kind, ..Default::default() };
                        self.toast(format!("Added \"{label}\""), true);
                    }
                    Err(e) => self.toast(e, false),
                }
            }
        });

        // ── the list ───────────────────────────────────────────────────
        let title = if schedule.items.is_empty() {
            "Nothing yet".to_string()
        } else {
            format!("Scheduled ({})", schedule.items.len())
        };
        section(ui, &title, |ui| {
            // Soonest first; finished to-dos at the bottom.
            let mut ids: Vec<u32> = schedule.items.iter().map(|i| i.id).collect();
            ids.sort_by_key(|id| {
                let item = schedule.get(*id).unwrap();
                (item.done, item.next_after(now).unwrap_or(Stamp::new(9999, 1, 1, 0, 0)))
            });
            let mut remove: Option<u32> = None;
            let mut test: Option<String> = None;
            ui.set_width(ui.available_width());
            for id in ids {
                let Some(index) = schedule.items.iter().position(|i| i.id == id) else { continue };
                let item = &mut schedule.items[index];
                // Two lines per item: when and how on top, what and which
                // function underneath. One line would need a wider window than
                // most people have.
                ui.horizontal(|ui| {
                    let mut done = item.done;
                    if ui.checkbox(&mut done, "").on_hover_text("Done").changed() {
                        item.done = done;
                        item.fired = done.then_some(now);
                        dirty = true;
                    }
                    ui.add_sized([70.0, 18.0], egui::Label::new(RichText::new(item.kind.label()).color(accent).strong()));

                    if item.kind.timed() {
                        let mut when = item.at.map(|at| at.date() + " " + &at.clock()).unwrap_or_default();
                        let resp = ui.add(egui::TextEdit::singleline(&mut when).desired_width(115.0));
                        if resp.changed() {
                            let text = when.clone();
                            if let Some(at) = Stamp::parse_iso(&text) {
                                item.at = Some(at);
                                dirty = true;
                            } else if let Ok((at, repeat)) = nv_core::schedule_parse::parse(&text, now) {
                                item.at = Some(at);
                                if repeat != Repeat::Once || item.repeat != Repeat::Once {
                                    item.repeat = repeat;
                                }
                                dirty = true;
                            }
                        }
                        let label = match item.next_after(now) {
                            Some(at) => describe_when(at, item.repeat, now),
                            None => "gone".into(),
                        };
                        ui.add_sized(
                            [170.0, 18.0],
                            egui::Label::new(RichText::new(label).small().color(Color32::from_gray(150))),
                        );
                        egui::ComboBox::from_id_salt(format!("rep{id}"))
                            .width(170.0)
                            .selected_text(item.repeat.label())
                            .show_ui(ui, |ui| {
                                for r in Repeat::ALL {
                                    if ui.selectable_value(&mut item.repeat, r, r.label()).clicked() {
                                        dirty = true;
                                    }
                                }
                            });
                    } else {
                        ui.add_sized([355.0, 18.0], egui::Label::new(""));
                    }
                    if ui.small_button("▶").on_hover_text("Say it now").clicked() {
                        test = Some(nv_core::schedule::announcement(item, now));
                    }
                    if ui.small_button("🗑").on_hover_text("Delete").clicked() {
                        remove = Some(id);
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    let mut text = item.text.clone();
                    if ui
                        .add(egui::TextEdit::singleline(&mut text).hint_text("what to say").desired_width(260.0))
                        .changed()
                    {
                        item.text = text;
                        dirty = true;
                    }
                    let action_label = if item.action.is_empty() {
                        "run no function".to_string()
                    } else {
                        pretty_name(&item.action)
                    };
                    egui::ComboBox::from_id_salt(format!("act{id}"))
                        .width(190.0)
                        .selected_text(action_label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut item.action, String::new(), "run no function");
                            for name in &names {
                                if ui.selectable_value(&mut item.action, name.clone(), pretty_name(name)).clicked() {
                                    dirty = true;
                                }
                            }
                        });
                    if item.chime {
                        ui.label(RichText::new("🔔").small().color(Color32::from_gray(150))).on_hover_text("Chimes");
                    }
                });
                ui.add_space(2.0);
            }
            if let Some(id) = remove {
                schedule.remove(id);
                dirty = true;
            }
            if let Some(line) = test {
                self.preview_voice(Some(line));
            }
        });

        if dirty {
            match schedule.save() {
                Ok(()) => {}
                Err(e) => self.toast(format!("Couldn't save: {e}"), false),
            }
        }

        let mut export = false;
        section(ui, "Take it with you", |ui| {
            hint(ui, "Alarms, reminders and events can be exported as an .ics file — import it into Outlook, Google Calendar or your phone.");
            if ui.button("📅  Export as calendar file").clicked() {
                export = true;
            }
        });
        if export {
            let dir = nv_core::win::documents_folder().unwrap_or_else(nv_core::paths::data_dir);
            let path = dir.join("NeedleVoice.ics");
            match std::fs::write(&path, schedule.to_ics(now)) {
                Ok(()) => {
                    let _ = win::shell_open(&dir.display().to_string(), None);
                    self.toast(format!("Wrote {}", path.display()), true);
                }
                Err(e) => self.toast(format!("Couldn't write the calendar file: {e}"), false),
            }
        }
        hint(
            ui,
            &format!(
                "Kept in {} — the assistant announces these even while the microphone is paused.",
                Schedule::path().display()
            ),
        );
    }

    /// Where Python is, and where its scripts go. Only shown when a Python
    /// function exists or the interpreter needs attention.
    fn python_ui(&mut self, ui: &mut egui::Ui) {
        let wants_python = self.cfg.custom_tools.iter().any(|t| t.kind == CustomKind::Python);
        let found = nv_core::win::find_python(&self.cfg.python_path);
        if !wants_python && found.is_some() {
            return; // nothing to say: it just works
        }
        section(ui, "Python scripts", |ui| {
            match &found {
                Some(path) => {
                    ui.label(RichText::new(format!("✔ Python found: {}", path.display())).color(Color32::from_gray(200)));
                }
                None => {
                    ui.label(
                        RichText::new("Python wasn't found. Install it, or point me at python.exe below.")
                            .color(Color32::from_rgb(255, 170, 80)),
                    );
                    hint(ui, "The Windows Store alias for python.exe doesn't count — install Python from python.org.");
                }
            }
            ui.horizontal(|ui| {
                ui.label("python.exe");
                ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.python_path)
                        .hint_text("leave empty to find it automatically")
                        .desired_width(400.0),
                );
            });
            ui.horizontal(|ui| {
                if ui.button("📂  Open scripts folder").clicked() {
                    let _ = win::shell_open(&self.cfg.scripts_dir().display().to_string(), None);
                }
                if ui.button("🔍  Detect again").clicked() {
                    self.cfg.python_path = String::new();
                }
            });
            hint(ui, "A script prints its answer; the last line is spoken. Parameters arrive as arguments and as NV_PARAM_<NAME>.");
        });
    }

    /// The tuning numbers, folded away. The defaults suit almost everyone, but
    /// a quiet voice or a noisy room can want more.
    fn wake_advanced(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Advanced wake tuning").id_salt("wakeadv").show(ui, |ui| {
            let automatic = nv_core::wake_model::Tuning::is_automatic(&self.cfg);
            let mut manual = !automatic;
            if ui.checkbox(&mut manual, "Set the trigger by hand").changed() {
                if manual {
                    let t = nv_core::wake_model::Tuning::automatic(&self.cfg);
                    self.cfg.wake_score = t.boost;
                    self.cfg.wake_threshold = t.threshold;
                } else {
                    self.cfg.wake_score = 0.0;
                    self.cfg.wake_threshold = 0.0;
                }
            }
            ui.add_enabled_ui(manual, |ui| {
                ui.add(egui::Slider::new(&mut self.cfg.wake_score, 0.5..=6.0).text("Keyword boost"));
                ui.add(egui::Slider::new(&mut self.cfg.wake_threshold, 0.02..=0.9).text("Trigger threshold"));
                hint(ui, "Higher boost / lower threshold = easier to trigger. If the chime never comes, raise the boost first.");
                if ui.button("Reset to automatic").clicked() {
                    self.cfg.wake_score = 0.0;
                    self.cfg.wake_threshold = 0.0;
                }
            });
            let t = nv_core::wake_model::Tuning::for_config(&self.cfg);
            hint(ui, &format!("Currently: boost {:.2}, threshold {:.3}", t.boost, t.threshold));
        });
    }

    /// What the spotter is actually listening for, straight from the tokeniser.
    fn wake_preview(&self, ui: &mut egui::Ui) {
        let Some(bpe) = &self.bpe else {
            ui.label(
                RichText::new("Wake-word model not installed — the assistant is falling back to slower, less reliable detection.")
                    .color(Color32::from_rgb(255, 170, 80)),
            );
            return;
        };
        let phrases = nv_core::wake_model::wake_phrases(&self.cfg);
        hint(ui, &format!("Listening for: {}", phrases.join(", ")));
        // Showed the way sherpa writes tokens, with a leading space for a word start.
        let tokens = nv_core::wake_model::keyword_lines(&self.cfg, bpe).replace('▁', " ").replace('\n', "  |  ");
        ui.label(RichText::new(format!("It listens for: {tokens}")).small().monospace().color(Color32::from_gray(150)));
        let odd = nv_core::wake_model::unpronounceable(&self.cfg, bpe);
        if !odd.is_empty() {
            ui.label(
                RichText::new(format!(
                    "The model has no sound for {} — waking will be unreliable. Try a simpler name.",
                    odd.join(", ")
                ))
                .color(Color32::from_rgb(255, 170, 80)),
            );
        }
    }

    /// Install or remove the small keyword model the instant wake-up needs.
    fn wake_model_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, accent: Color32) {
        let installed = nv_core::wake_model::is_installed();
        let mut install = false;
        let mut remove = false;
        section(
            ui,
            if installed { "Wake word — instant" } else { "Wake word — needs the keyword model" },
            |ui| {
                if installed {
                    ui.label(RichText::new("✔ Installed — the name is heard the moment you say it, without Whisper.").color(accent));
                } else {
                    ui.label(
                        RichText::new("Without this, waking waits for Whisper to finish transcribing the whole sentence.")
                            .color(Color32::from_rgb(255, 170, 80)),
                    );
                    hint(ui, "It's a one-time 5 MB download and works offline afterwards.");
                }
                self.wake_preview(ui);
                ui.horizontal(|ui| {
                    if !installed {
                        if let Some(job) = self.job_running("wake_model") {
                            let p = job.0.lock().unwrap().progress;
                            ui.add(egui::ProgressBar::new(p.max(0.0)).desired_width(220.0).show_percentage());
                        } else if ui.button("⬇  Get the wake-word model (5 MB)").clicked() {
                            install = true;
                        }
                    } else if ui.button("🗑  Remove the wake-word model").clicked() {
                        remove = true;
                    }
                    if ui
                        .button("🔔  Hear the wake chime")
                        .on_hover_text("It also pops the bubble and starts listening at the same moment.")
                        .clicked()
                    {
                        let _ = win::spawn_detached(&Self::agent_exe(), &["--chime"]);
                    }
                });
            },
        );
        if install {
            self.start_job(ctx, "wake_model", "Downloading the wake-word model", |p| {
                nv_core::wake_model::download(|d, t| p(if t > 0 { d as f32 / t as f32 } else { -1.0 }))
                    .map(|_| "Wake-word model installed — waking is instant now".to_string())
            });
        }
        if remove {
            match nv_core::wake_model::remove() {
                Ok(()) => {
                    self.bpe = None;
                    self.toast("Removed the wake-word model", true);
                }
                Err(e) => self.toast(format!("Couldn't remove it: {e}"), false),
            }
        }
    }

    fn tab_browser(&mut self, ui: &mut egui::Ui, accent: Color32) {
        heading(ui, "Browser & search", accent);
        section(ui, "Browser", |ui| {
            for b in Browser::ALL {
                ui.radio_value(&mut self.cfg.browser, b, b.label());
            }
            if self.cfg.browser == Browser::Custom {
                ui.horizontal(|ui| {
                    ui.label("Path to browser .exe");
                    ui.add(egui::TextEdit::singleline(&mut self.cfg.browser_path).desired_width(360.0));
                });
            }
            let found = nv_core::actions::browser_exe(&self.cfg);
            match (&self.cfg.browser, found) {
                (Browser::Default, _) => hint(ui, "Searches open in your Windows default browser."),
                (_, Some(p)) => hint(ui, &format!("Found: {}", p.display())),
                (_, None) => {
                    ui.label(RichText::new("Not found on this PC — the default browser will be used.").color(Color32::from_rgb(255, 170, 80)));
                }
            }
        });
        section(ui, "Search engine", |ui| {
            ui.horizontal_wrapped(|ui| {
                for (name, url) in SEARCH_ENGINES {
                    ui.selectable_value(&mut self.cfg.search_url, url.to_string(), name);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Custom URL");
                ui.add(egui::TextEdit::singleline(&mut self.cfg.search_url).desired_width(380.0));
            });
            hint(ui, "{} is replaced with what you asked, e.g. \"what is the haber process\".");
        });
    }

    fn tab_apps(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, accent: Color32) {
        heading(ui, "Apps", accent);
        let mut rescan = false;
        section(ui, "Custom names", |ui| {
            hint(ui, "Teach it your own words: say \"open code\" for Visual Studio Code. Targets can also be a URL or a path.");
            let mut remove = None;
            for (i, a) in self.cfg.aliases.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("\"{}\"  →  {}", a.phrase, a.target));
                    if ui.small_button("✖").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                self.cfg.aliases.remove(i);
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_alias.0).hint_text("when I say…").desired_width(140.0));
                ui.label("→");
                ui.add(egui::TextEdit::singleline(&mut self.new_alias.1).hint_text("open this app / URL").desired_width(220.0));
                if ui.button("Add").clicked() && !self.new_alias.0.trim().is_empty() && !self.new_alias.1.trim().is_empty() {
                    self.cfg.aliases.push(Alias {
                        phrase: self.new_alias.0.trim().to_lowercase(),
                        target: self.new_alias.1.trim().into(),
                    });
                    self.new_alias = Default::default();
                }
            });
        });
        section(ui, &format!("Detected apps ({})", self.apps.apps.len()), |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.app_filter).hint_text("🔍 filter").desired_width(220.0));
                if self.job_running("scan").is_some() {
                    ui.spinner();
                } else if ui.button("🔄 Rescan now").clicked() {
                    rescan = true;
                }
            });
            hint(ui, "Untick an app to stop it being opened by voice. New apps are picked up automatically every 30 minutes.");
            let filter = self.app_filter.to_lowercase();
            egui::ScrollArea::vertical().max_height(380.0).id_salt("apps").show(ui, |ui| {
                for app in self.apps.apps.iter().filter(|a| a.name.to_lowercase().contains(&filter)) {
                    let mut enabled = !self.cfg.excluded_apps.iter().any(|e| e.eq_ignore_ascii_case(&app.name));
                    if ui.checkbox(&mut enabled, &app.name).changed() {
                        if enabled {
                            self.cfg.excluded_apps.retain(|e| !e.eq_ignore_ascii_case(&app.name));
                        } else {
                            self.cfg.excluded_apps.push(app.name.clone());
                        }
                    }
                }
            });
        });
        if rescan {
            let cfg = self.cfg.clone();
            self.start_job(ctx, "scan", "Scanning apps", move |_| {
                let idx = AppIndex::scan(&cfg);
                let n = idx.apps.len();
                idx.save_cache();
                Ok(format!("Found {n} apps"))
            });
        }
    }

    /// Everything Needle is allowed to do: the built-ins it may call, and the
    /// functions you wrote yourself.
    fn tab_functions(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, accent: Color32) {
        heading(ui, "Functions", accent);
        hint(
            ui,
            "These are the things Needle can actually do. Untick one to take it away; add your own at the bottom.",
        );
        self.python_ui(ui);

        let off = self.cfg.disabled_tools.len();
        let title = if off == 0 {
            format!("Built-in functions ({})", tools::BUILTINS.len())
        } else {
            format!("Built-in functions ({}/{} on)", tools::BUILTINS.len() - off, tools::BUILTINS.len())
        };
        section(ui, &title, |ui| {
            hint(ui, "Each one is understood instantly — no waiting for the model.");
            for group in tools::GROUPS {
                let items: Vec<&'static tools::Builtin> = tools::BUILTINS.iter().filter(|t| t.group == group).collect();
                let disabled = items.iter().filter(|t| self.cfg.disabled_tools.iter().any(|d| d == t.name)).count();
                let header = if disabled == 0 {
                    format!("{group} — {} available", items.len())
                } else {
                    format!("{group} — {} on, {disabled} off", items.len() - disabled)
                };
                egui::CollapsingHeader::new(header).id_salt(group).default_open(true).show(ui, |ui| {
                    for t in items {
                        let mut on = !self.cfg.disabled_tools.iter().any(|d| d == t.name);
                        ui.horizontal(|ui| {
                            let box_resp = ui.checkbox(&mut on, pretty_name(t.name));
                            box_resp.clone().on_hover_text(format!("Needle calls this {}", t.name));
                            if box_resp.changed() {
                                if on {
                                    self.cfg.disabled_tools.retain(|d| d != t.name);
                                } else {
                                    self.cfg.disabled_tools.push(t.name.to_string());
                                }
                            }
                            ui.label(RichText::new(format!("e.g. \"{}\"", t.example)).small().color(Color32::from_gray(130)));
                        });
                        ui.indent(t.name, |ui| {
                            hint(ui, t.description);
                        });
                    }
                });
            }
        });

        // ── your own functions ──────────────────────────────────────────
        let mut delete: Option<usize> = None;
        let mut try_it: Option<usize> = None;
        let count = self.cfg.custom_tools.len();
        let function_names: Vec<String> = self.cfg.custom_tools.iter().map(|t| t.name.clone()).collect();
        let scripts_dir = self.cfg.scripts_dir();
        let custom_title = if count == 0 { "Your functions".to_string() } else { format!("Your functions ({count})") };
        section(ui, &custom_title, |ui| {
            hint(
                ui,
                "A trigger phrase runs these without the model at all, so they work even offline. Put {query} in a phrase to capture what you said.",
            );
            if count == 0 {
                hint(ui, "Nothing here yet — add one from a template below.");
            }
            for i in 0..count {
                let tool = &mut self.cfg.custom_tools[i];
                let name = tool.name.clone();
                let label =
                    if tool.enabled { pretty_name(&tool.label()) } else { format!("{} (off)", pretty_name(&tool.label())) };
                egui::CollapsingHeader::new(label)
                    .id_salt(format!("fn{name}"))
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut tool.enabled, "Enabled");
                            ui.separator();
                            ui.label("name");
                            ui.add(egui::TextEdit::singleline(&mut tool.name).desired_width(150.0));
                        });
                        hint(ui, "Use it as the name Needle calls; keep it snake_case, e.g. focus_mode.");
                        ui.horizontal(|ui| {
                            ui.label("what it does");
                            ui.add(egui::TextEdit::singleline(&mut tool.description).desired_width(360.0));
                        });
                        hint(ui, "Needle reads this to decide when to use it, so say it plainly.");
                        ui.horizontal(|ui| {
                            ui.label("parameters");
                            let mut params = tool.params.join(", ");
                            if ui.add(egui::TextEdit::singleline(&mut params).hint_text("query").desired_width(240.0)).changed() {
                                tool.params = params
                                    .split(',')
                                    .map(tools::sanitize_name)
                                    .filter(|p| !p.is_empty())
                                    .collect();
                            }
                        });
                        hint(ui, "Comma-separated. Use them below as {name}.");
                        ui.horizontal(|ui| {
                            ui.label("action");
                            egui::ComboBox::from_id_salt(format!("kind{i}"))
                                .width(280.0)
                                .selected_text(tool.kind.label())
                                .show_ui(ui, |ui| {
                                    for k in CustomKind::ALL {
                                        ui.selectable_value(&mut tool.kind, k, k.label());
                                    }
                                });
                        });
                        hint(ui, tool.kind.hint());
                        if tool.kind.uses_target() {
                            ui.horizontal(|ui| {
                                ui.label(match tool.kind {
                                    CustomKind::OpenUrl => "URL",
                                    CustomKind::Python => "script",
                                    _ => "target",
                                });
                                ui.add(
                                    egui::TextEdit::singleline(&mut tool.target)
                                        .hint_text(tool.kind.target_hint())
                                        .desired_width(400.0),
                                );
                            });
                        }
                        if tool.kind == CustomKind::Run {
                            ui.horizontal(|ui| {
                                ui.label("arguments");
                                ui.add(egui::TextEdit::singleline(&mut tool.args).desired_width(400.0));
                            });
                        }
                        if tool.kind == CustomKind::Python {
                            ui.horizontal(|ui| {
                                ui.label("arguments");
                                ui.add(
                                    egui::TextEdit::singleline(&mut tool.args)
                                        .hint_text("optional — {param} placeholders, or empty to pass the parameters in order")
                                        .desired_width(400.0),
                                );
                            });
                            hint(
                                ui,
                                &format!(
                                    "Scripts live in {} — parameters also arrive as NV_PARAM_<NAME>. Whatever the script prints last is spoken.",
                                    scripts_dir.display()
                                ),
                            );
                        }
                        if tool.kind == CustomKind::Sequence {
                            ui.label("steps, in order");
                            let mut remove_step: Option<usize> = None;
                            let mut move_step: Option<(usize, i32)> = None;
                            let steps = tool.steps.len();
                            for si in 0..steps {
                                let step = &mut tool.steps[si];
                                ui.horizontal(|ui| {
                                    egui::ComboBox::from_id_salt(format!("step{name}-{si}"))
                                        .width(160.0)
                                        .selected_text(step.kind.label())
                                        .show_ui(ui, |ui| {
                                            for k in StepKind::ALL {
                                                ui.selectable_value(&mut step.kind, k, k.label());
                                            }
                                        });
                                    ui.add(
                                        egui::TextEdit::singleline(&mut step.target)
                                            .hint_text(step.kind.hint())
                                            .desired_width(300.0),
                                    );
                                    if step.kind.needs_args() {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut step.args)
                                                .hint_text("arguments")
                                                .desired_width(150.0),
                                        );
                                    }
                                    if si > 0 && ui.small_button("↑").clicked() {
                                        move_step = Some((si, -1));
                                    }
                                    if si + 1 < steps && ui.small_button("↓").clicked() {
                                        move_step = Some((si, 1));
                                    }
                                    if ui.small_button("✖").clicked() {
                                        remove_step = Some(si);
                                    }
                                });
                            }
                            if let Some(si) = remove_step {
                                tool.steps.remove(si);
                            }
                            if let Some((si, delta)) = move_step {
                                let to = (si as i32 + delta).clamp(0, tool.steps.len() as i32 - 1) as usize;
                                tool.steps.swap(si, to);
                            }
                            if ui.button("＋  Add step").clicked() {
                                tool.steps.push(Step::default());
                            }
                            hint(
                                ui,
                                &format!(
                                    "Use {{output}} for what the previous script printed, and {{param}} for your parameters. Functions you can call: {}",
                                    if function_names.is_empty() { "(none yet)".to_string() } else { function_names.join(", ") }
                                ),
                            );
                        }
                        let mut phrases = tool.phrases.join("\n");
                        ui.label("say any of these");
                        if ui
                            .add(egui::TextEdit::multiline(&mut phrases).desired_rows(2).desired_width(400.0))
                            .changed()
                        {
                            tool.phrases =
                                phrases.lines().map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty()).collect();
                        }
                        hint(ui, "One per line. A missing parameter captures the rest of the sentence.");
                        let mut reply = tool.reply.clone();
                        ui.horizontal(|ui| {
                            ui.label("reply");
                            if ui
                                .add(egui::TextEdit::singleline(&mut reply).hint_text("say this afterwards").desired_width(360.0))
                                .changed()
                            {
                                tool.reply = reply;
                            }
                        });
                        hint(ui, "Leave empty for a normal \"done\" line.");
                        ui.horizontal(|ui| {
                            if ui.button("▶  Try it").clicked() {
                                try_it = Some(i);
                            }
                            if ui.button("🗑  Delete").clicked() {
                                delete = Some(i);
                            }
                        });
                    });
            }
        });
        if let Some(i) = delete {
            let removed = self.cfg.custom_tools.remove(i);
            self.toast(format!("Removed \"{}\" — save to apply", pretty_name(&removed.label())), true);
        }
        if let Some(i) = try_it {
            self.try_function(ctx, i);
        }
        // ── start from a template ───────────────────────────────────────
        let mut add: Option<CustomTool> = None;
        section(ui, "Add a function", |ui| {
            hint(ui, "Templates are ready to use — add one, then tweak it. They respect the switches in your config, so shutdown and sleep still ask nothing of Needle.");
            ui.horizontal_wrapped(|ui| {
                for t in tools::templates() {
                    if ui.button(format!("+ {}", pretty_name(&t.label()))).on_hover_text(&t.description).clicked() {
                        add = Some(t);
                    }
                }
                if ui.button("+ Blank function").clicked() {
                    add = Some(CustomTool { name: "new_function".into(), description: String::new(), ..Default::default() });
                }
            });
        });
        if let Some(t) = add {
            let label = pretty_name(&t.label());
            self.add_function(t);
            self.toast(format!("Added \"{label}\" — its settings are open above. Save to apply."), true);
        }
    }

    /// Add a function, renaming it if the name is taken.
    fn add_function(&mut self, mut tool: CustomTool) {
        if tool.kind == CustomKind::Python {
            let target = tool.target.clone();
            self.write_starter_script(&target);
        }
        let base = tools::sanitize_name(if tool.name.is_empty() { "new_function" } else { &tool.name });
        let mut name = if base.is_empty() { "new_function".to_string() } else { base };
        let mut n = 2;
        while self.cfg.custom_tools.iter().any(|t| t.name == name) || tools::is_builtin(&name) {
            name = format!("{}{n}", name.trim_end_matches(|c: char| c.is_ascii_digit()));
            n += 1;
        }
        tool.name = name;
        self.cfg.custom_tools.push(tool);
    }

    /// Give a new Python function something that actually runs: a small script
    /// in the scripts folder that shows how parameters arrive.
    fn write_starter_script(&mut self, target: &str) {
        let name = target.trim().trim_matches('"');
        if name.is_empty() || name.contains(['/', '\\']) {
            return;
        }
        let path = self.cfg.scripts_dir().join(if name.ends_with(".py") { name.to_string() } else { format!("{name}.py") });
        if path.exists() {
            return;
        }
        let starter = "# Called by NeedleVoice.\n\
             # Every parameter arrives as an argument and as NV_PARAM_<NAME>.\n\
             # The last line you print is the answer that gets spoken.\n\
             import os\n\
             import sys\n\
             \n\
             question = sys.argv[1] if len(sys.argv) > 1 else os.environ.get(\"NV_PARAM_QUESTION\", \"\")\n\
             print(\"You asked:\", question)\n\
             print(\"Change me: this is what gets said.\")\n";
        if let Err(e) = std::fs::write(&path, starter) {
            self.toast(format!("Couldn't write {}: {e}", path.display()), false);
        } else {
            self.toast(format!("Created {} — edit it to do something useful", path.display()), true);
        }
    }

    /// Run one of the user's functions with sample arguments, so the result
    /// (and any error) can be seen without talking to the microphone.
    fn try_function(&mut self, ctx: &egui::Context, index: usize) {
        let Some(tool) = self.cfg.custom_tools.get(index).cloned() else { return };
        let cfg = self.cfg.clone();
        let params: Vec<(String, String)> = tool.params.iter().map(|p| (p.clone(), format!("<{p}>"))).collect();
        let name = tool.name.clone();
        let label = format!("Running {}", pretty_name(&tool.label()));
        self.start_job(ctx, "try_function", &label, move |_| {
            let apps = AppIndex::load_cache().unwrap_or_default();
            // A function that only speaks has no action to run — its reply is
            // the whole result.
            if tool.kind == CustomKind::SayOnly {
                let reply = nv_core::personality::reply(
                    &cfg,
                    &[nv_core::personality::Outcome::Done(
                        nv_core::brain::Action::Custom { name: name.clone(), params },
                        String::new(),
                    )],
                );
                return Ok(format!("It would say: {reply}"));
            }
            let action = nv_core::brain::Action::Custom { name: name.clone(), params };
            match nv_core::actions::execute(&action, &cfg, &apps) {
                Ok(msg) => Ok(format!("✓ {msg}")),
                Err(e) => Ok(format!("✗ {e}")),
            }
        });
    }

    fn tab_test(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, accent: Color32) {
        heading(ui, "Test a command", accent);
        hint(ui, "Type what you'd say after the wake word to see how Needle 3 understands it — without using the microphone.");
        let mut run = false;
        let mut execute = false;
        section(ui, "Command", |ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.test_input).desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                run = ui.button("🧠 Understand").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                execute = ui.button("▶ Understand & do it").clicked();
                if self.test_job.is_some() {
                    ui.spinner();
                }
            });
        });
        if (run || execute) && self.test_job.is_none() {
            let cfg = self.cfg.clone();
            let cmd = self.test_input.clone();
            let brain = self.brain.clone();
            self.test_job = Some(Job::spawn(ctx, "Thinking", move |_| {
                let apps = AppIndex::load_cache().unwrap_or_default();
                let mut guard = brain.lock().unwrap();
                let b = guard.get_or_insert_with(|| {
                    Brain::new(nv_core::paths::models_dir().join(nv_core::NEEDLE_MODEL), cfg.needle_depth)
                });
                let d = b.decide(&cmd, &cfg, &apps);
                let mut out = format!("Decided via {:?} in {} ms:\n", d.via, d.millis);
                for a in &d.actions {
                    out.push_str(&format!("  • {a}\n"));
                }
                if let Some(r) = &d.reasoning {
                    out.push_str(&format!("\nNeedle's reasoning: {r}\n"));
                }
                let reply = match d.actions.as_slice() {
                    [one] => personality::line(cfg.personality, &Moment::Done(one)),
                    many => personality::line(cfg.personality, &Moment::Multi(many.len())),
                };
                out.push_str(&format!("\nIt would say: \"{reply}\"\n"));
                if execute {
                    for a in &d.actions {
                        match nv_core::actions::execute(a, &cfg, &apps) {
                            Ok(m) => out.push_str(&format!("  ✔ {m}\n")),
                            Err(e) => out.push_str(&format!("  ✖ {e}\n")),
                        }
                    }
                }
                Ok(out)
            }));
        }
        if let Some(job) = &self.test_job {
            if let Some(r) = job.0.lock().unwrap().result.clone() {
                self.test_result = Some(r.unwrap_or_else(|e| e));
            }
        }
        if self.test_job.as_ref().is_some_and(|j| j.done()) {
            self.test_job = None;
        }
        if let Some(r) = &self.test_result {
            section(ui, "Result", |ui| {
                ui.label(RichText::new(r).monospace());
            });
        }
    }
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("NeedleVoice Settings")
            .with_inner_size([980.0, 720.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native("NeedleVoice Settings", opts, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

/// Segoe UI for text, with Windows' symbol/emoji fonts as fallbacks so icons render.
fn install_fonts(ctx: &egui::Context) {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let mut fonts = egui::FontDefinitions::default();
    let mut add = |name: &str, file: &str, primary: bool| {
        if let Ok(bytes) = std::fs::read(format!(r"{windir}\Fonts\{file}")) {
            fonts.font_data.insert(name.into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            let list = fonts.families.entry(egui::FontFamily::Proportional).or_default();
            if primary {
                list.insert(0, name.into());
            } else {
                list.push(name.into());
            }
        }
    };
    add("segoe", "segoeui.ttf", true);
    add("segoe-symbol", "seguisym.ttf", false);
    add("segoe-emoji", "seguiemj.ttf", false);
    ctx.set_fonts(fonts);
}

impl App {
    fn mic_check_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, accent: Color32) {
        let Some(mc) = self.mic_check.as_mut() else { return };
        mc.tick(ctx, &self.cfg);
        let wake = self.cfg.wake_phrase();
        let mut apply: Option<miccheck::Calibration> = None;
        section(ui, "🎤  Microphone check", |ui| {
            hint(ui, "Talk and watch the bars — the mic you speak into should jump. Then run the check so NeedleVoice can tune itself to your voice and room.");
            ui.add_space(4.0);
            for m in &mc.mics {
                let lvl = m.level();
                let frac = ((lvl + 70.0) / 60.0).clamp(0.0, 1.0);
                ui.horizontal(|ui| {
                    let mut name = m.name.clone();
                    if m.is_default {
                        name.push_str("  (Windows default)");
                    }
                    if self.cfg.microphone == m.name || (self.cfg.microphone.is_empty() && m.is_default) {
                        name.push_str("  ← in use");
                    }
                    ui.add_sized([330.0, 18.0], egui::Label::new(RichText::new(name).small()).truncate());
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(260.0, 12.0), egui::Sense::hover());
                    let p = ui.painter();
                    p.rect_filled(rect, 4.0, Color32::from_rgb(28, 31, 40));
                    let mut fill = rect;
                    fill.set_width(rect.width() * frac);
                    p.rect_filled(fill, 4.0, accent);
                    ui.label(RichText::new(format!("{lvl:>4.0} dB")).monospace().small());
                });
            }
            ui.add_space(8.0);
            match &mc.phase {
                miccheck::Phase::Idle => {
                    if ui.add(egui::Button::new(RichText::new("▶  Run microphone check").strong())).clicked() {
                        mc.start();
                    }
                }
                miccheck::Phase::Quiet(_) => {
                    let s = miccheck::remaining(&mc.phase).unwrap_or(0.0);
                    ui.label(RichText::new(format!("Step 1 of 2 — stay quiet… {s:.0}")).size(16.0).color(accent));
                    hint(ui, "Measuring your room's background noise.");
                }
                miccheck::Phase::Speak(_) => {
                    ui.label(RichText::new(format!("Step 2 of 2 — now say: \"{wake}, open Notepad\"")).size(16.0).strong().color(accent));
                    hint(ui, "Speak normally, like you would to the assistant. It stops automatically when you finish.");
                }
                miccheck::Phase::Analyzing => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Listening back to what you said…");
                    });
                }
            }

            if let Some(cal) = &mc.result {
                ui.separator();
                for r in &cal.mics {
                    let good = cal.best.as_deref() == Some(r.name.as_str());
                    let text = format!(
                        "{} {}  — background {:.0} dB, your voice {:.0} dB (+{:.0} dB clearer)",
                        if good { "✔" } else { "•" },
                        r.name,
                        r.noise_db,
                        r.speech_db,
                        r.snr().max(0.0)
                    );
                    ui.label(RichText::new(text).color(if good { accent } else { Color32::from_gray(150) }));
                }
                match &cal.best {
                    None => {
                        ui.label(
                            RichText::new("Couldn't hear you on any microphone.").strong().color(Color32::from_rgb(255, 95, 105)),
                        );
                        hint(ui, "Check the mic isn't muted (Windows Settings → System → Sound → Input) and that desktop apps are allowed to use it (Privacy & security → Microphone), then try again.");
                    }
                    Some(best) => {
                        match &cal.transcript {
                            Some(t) if cal.wake_ok => {
                                ui.label(RichText::new(format!("✔ Heard: \"{t}\" — wake word recognised!")).strong().color(accent));
                            }
                            Some(t) => {
                                ui.label(RichText::new(format!("Heard: \"{t}\"")).strong());
                                match cal.sensitivity {
                                    Some(s) => hint(ui, &format!("The name wasn't recognised at your current sensitivity — it will be raised to {:.2}.", s + 0.05)),
                                    None => hint(ui, &format!("It didn't catch \"{wake}\". Try again speaking a little slower, or pick a more distinctive name on the General tab.")),
                                }
                            }
                            None => hint(ui, "Couldn't run speech recognition — the levels below will still be applied."),
                        }
                        ui.label(format!(
                            "Recommended: use \"{best}\", boost +{:.0} dB, speech threshold {:.0} dB, noise filtering {}",
                            cal.gain_db, cal.min_speech_db, cal.vad
                        ));
                        if ui.add(egui::Button::new(RichText::new("✔  Apply & save these settings").strong().color(Color32::BLACK)).fill(accent)).clicked() {
                            apply = Some(cal.clone());
                        }
                    }
                }
            }
        });
        if let Some(cal) = apply {
            if let Some(best) = cal.best {
                self.cfg.microphone = best;
            }
            self.cfg.mic_gain_db = cal.gain_db;
            self.cfg.min_speech_db = cal.min_speech_db;
            self.cfg.vad_aggressiveness = cal.vad;
            if !cal.wake_ok {
                if let Some(s) = cal.sensitivity {
                    self.cfg.wake_sensitivity = (s + 0.05).min(1.0);
                }
            }
            self.save();
        }
    }
}
