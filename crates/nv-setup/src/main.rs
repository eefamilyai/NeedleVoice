//! NeedleVoice installer. Per-user install, no admin needed.
//!
//! Installing is all this program does. Removing an installation is the
//! uninstaller's job, and that is a program of its own under `crates/nv-uninstall`
//! — see `docs/installer.md` for why it must not be this file under another
//! name, which is what it used to be.
#![windows_subsystem = "windows"]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, RichText, Stroke};
use nv_core::{win, Config};

static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.tar.zst"));
const ACCENT: Color32 = Color32::from_rgb(182, 255, 46);

/// Name the uninstaller travels under inside the payload. It is written into the
/// install folder as `Uninstall.exe`, which is the name Add/Remove Programs and
/// the shortcut both expect.
const UNINSTALLER_IN_PAYLOAD: &str = "NeedleVoiceUninstall.exe";

/// A size a person can read, for the out-of-space message.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[derive(Clone, PartialEq)]
enum Stage {
    Welcome,
    Working,
    Done,
    Failed(String),
}

#[derive(Default)]
struct Progress {
    fraction: f32,
    step: String,
    result: Option<Result<(), String>>,
}

struct Setup {
    stage: Stage,
    dir: String,
    name: String,
    autostart: bool,
    desktop: bool,
    launch: bool,
    progress: Arc<Mutex<Progress>>,
}

impl Setup {
    fn new() -> Self {
        let existing = Config::load();
        let dir = win::installed_location().unwrap_or_else(nv_core::paths::default_install_dir);
        Self {
            stage: Stage::Welcome,
            dir: dir.display().to_string(),
            name: existing.agent_name.clone(),
            autostart: true,
            desktop: true,
            launch: true,
            progress: Default::default(),
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        self.stage = Stage::Working;
        let p = self.progress.clone();
        let ctx = ctx.clone();
        let dir = PathBuf::from(self.dir.trim());
        let (name, autostart, desktop, launch) =
            (self.name.trim().to_string(), self.autostart, self.desktop, self.launch);
        std::thread::spawn(move || {
            win::com_init();
            let report = |f: f32, s: &str| {
                let mut g = p.lock().unwrap();
                g.fraction = f;
                g.step = s.to_string();
                ctx.request_repaint();
            };
            let r = do_install(&dir, &name, autostart, desktop, launch, &report);
            p.lock().unwrap().result = Some(r);
            ctx.request_repaint();
        });
    }
}

fn start_menu_dir() -> PathBuf {
    win::start_menu_dir()
}

fn desktop_link() -> PathBuf {
    win::desktop_link()
}

/// Free space on the volume `dir` is (or would be) on. Asked before a single
/// byte is written, so a machine without room is told so instead of being left
/// with a half-copied installation.
fn free_bytes(dir: &Path) -> Option<u64> {
    let mut probe = dir;
    while !probe.exists() {
        probe = probe.parent()?;
    }
    let free = unsafe {
        let mut available = 0u64;
        windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            &windows::core::HSTRING::from(probe.as_os_str()),
            Some(&mut available),
            None,
            None,
        )
        .ok()?;
        available
    };
    Some(free)
}

/// Ask anything already running to close, before any file is laid down.
///
/// Not `taskkill`: an installer that reaches straight for force-termination is
/// both worse for the user — the agent loses whatever it was doing — and one of
/// the shapes a behaviour heuristic reads as hostile. Only a copy that ignores
/// the request is terminated.
fn stop_running() {
    for exe in [nv_core::AGENT_EXE, nv_core::CONFIG_EXE] {
        if !win::close_processes_named(exe, std::time::Duration::from_secs(5)) {
            report_step(&format!("{exe} did not close and was stopped"));
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
}

/// Progress notice from a place that has no reporter to hand (the steps that
/// run before the worker thread owns one). Prints, so a silent install says
/// what happened.
fn report_step(line: &str) {
    let _ = std::io::Write::write_all(&mut std::io::stdout(), format!("{line}\n").as_bytes());
}

/// Total size of a folder in bytes. The caller divides once, at the top: doing
/// it per level multiplied every nested folder's total by 1024 again, so a
/// single-level install reported its size as terabytes.
fn dir_size_bytes(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                dir_size_bytes(&p)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

fn do_install(dir: &Path, name: &str, autostart: bool, desktop: bool, launch: bool, report: &dyn Fn(f32, &str)) -> Result<(), String> {
    report(0.02, "Closing any running copy…");
    stop_running();

    // Room for the payload before anything is written. The archive is
    // compressed, so this is only a guide, but it turns "the install failed
    // half way with a disk-full error" into a plain message up front.
    if let Some(free) = free_bytes(dir) {
        if free < PAYLOAD.len() as u64 {
            return Err(format!(
                "Not enough space on that drive: {} is free and the install needs about {}.",
                human_size(free),
                human_size(PAYLOAD.len() as u64)
            ));
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;

    // The uninstaller that is about to be laid down is a separate program in
    // the payload. An older install left the *installer* here under this name,
    // and it has to go before the new one can be written — as well as being the
    // file that was being flagged, so removing it is the point.
    let uninst = dir.join(nv_core::UNINSTALL_EXE);
    let _ = std::fs::remove_file(&uninst);

    // Extract the payload, reporting progress by compressed bytes consumed.
    struct Counting<'a> {
        data: &'a [u8],
        pos: usize,
        report: &'a dyn Fn(f32, &str),
    }
    impl Read for Counting<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            // Reading past the end is normal; the subtraction is the one that
            // has to be guarded, or an empty payload underflows and panics.
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            let n = buf.len().min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            if self.pos % (4 << 20) < n {
                (self.report)(0.05 + 0.8 * self.pos as f32 / self.data.len() as f32, "Copying files…");
            }
            Ok(n)
        }
    }
    let reader = Counting { data: PAYLOAD, pos: 0, report };
    let dec = zstd::stream::Decoder::new(reader).map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(dec);
    // Unpacked file by file rather than with `unpack`: `unpack` follows whatever
    // paths the archive holds, and a payload entry called `..\..\Windows\…`
    // would be written there. Only relative names reach the disk here.
    let entries = archive.entries().map_err(|e| format!("Couldn't read the payload: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("Couldn't read the payload: {e}"))?;
        let rel = entry.path().map_err(|e| e.to_string())?.into_owned();
        if rel.is_absolute() || rel.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
            return Err(format!("The installer payload contains an unsafe path: {}", rel.display()));
        }
        let to = dir.join(&rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't create {}: {e}", parent.display()))?;
        }
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&to).map_err(|e| format!("Couldn't create {}: {e}", to.display()))?;
        } else if entry.header().entry_type().is_file() {
            let mut out = std::fs::File::create(&to).map_err(|e| format!("Couldn't write {}: {e}", to.display()))?;
            std::io::copy(&mut entry, &mut out).map_err(|e| format!("Couldn't write {}: {e}", to.display()))?;
        }
    }

    report(0.88, "Creating shortcuts…");
    let agent = dir.join(nv_core::AGENT_EXE);
    let config_exe = dir.join(nv_core::CONFIG_EXE);
    // Written under the name Add/Remove Programs expects, from the program the
    // payload carried — which is where the old build copied *itself*.
    let shipped = dir.join(UNINSTALLER_IN_PAYLOAD);
    if !shipped.exists() {
        return Err(format!(
            "The payload is missing {UNINSTALLER_IN_PAYLOAD}, so there would be no way to uninstall. \
             Rebuild the installer."
        ));
    }
    std::fs::copy(&shipped, &uninst).map_err(|e| format!("Couldn't write the uninstaller: {e}"))?;
    let sm = start_menu_dir();
    win::create_shortcut(&sm.join("NeedleVoice.lnk"), &agent, "", "Start the NeedleVoice assistant")?;
    win::create_shortcut(&sm.join("NeedleVoice Settings.lnk"), &config_exe, "", "NeedleVoice settings")?;
    win::create_shortcut(&sm.join("Uninstall NeedleVoice.lnk"), &uninst, "--uninstall", "Remove NeedleVoice")?;
    if desktop {
        win::create_shortcut(&desktop_link(), &config_exe, "", "NeedleVoice settings")?;
    }

    report(0.93, "Saving settings…");
    let mut cfg = Config::load();
    if !name.is_empty() {
        cfg.agent_name = name.to_string();
    }
    cfg.start_with_windows = autostart;
    cfg.save().map_err(|e| format!("Couldn't save settings: {e}"))?;
    win::set_autostart(autostart, &agent)?;
    win::register_uninstaller(dir, env!("CARGO_PKG_VERSION"), (dir_size_bytes(dir) / 1024).min(u32::MAX as u64) as u32)?;

    if launch {
        report(0.98, "Starting the assistant…");
        win::spawn_detached(&agent, &[]).map_err(|e| e.to_string())?;
    }
    report(1.0, "Done");
    Ok(())
}

/// Uninstalling is not this program's job any more.
///
/// Handing the request to the installed uninstaller is both the ordinary way a
/// setup program behaves — "uninstall" from the installer just runs the
/// uninstaller — and the reason this file no longer has to contain an
/// uninstaller's routines. See `docs/installer.md`.
fn do_uninstall(dir: &Path, delete_settings: bool, report: &dyn Fn(f32, &str)) -> Result<(), String> {
    let uninst = dir.join(nv_core::UNINSTALL_EXE);
    if !uninst.exists() {
        return Err(format!(
            "{} is not there, so there is nothing installed to remove. \
             Use Apps & features, or delete {} by hand.",
            uninst.display(),
            dir.display()
        ));
    }
    report(0.3, "Handing over to the uninstaller…");
    let mut args: Vec<String> = vec!["--uninstall".into(), "--dir".into(), dir.display().to_string()];
    if delete_settings {
        args.push("--delete-settings".into());
    }
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    win::spawn_detached(&uninst, &borrowed).map_err(|e| format!("Couldn't start the uninstaller: {e}"))?;
    report(1.0, "Done");
    Ok(())
}

fn orb(ui: &egui::Ui, center: egui::Pos2, r: f32, t: f32) {
    let p = ui.painter();
    for i in 0..6 {
        let k = i as f32 / 6.0;
        p.circle_filled(center, r * (1.9 - k * 0.8), ACCENT.linear_multiply(0.03 + k * 0.02));
    }
    p.circle_filled(center, r, Color32::from_rgb(9, 10, 15));
    let blobs = [(ACCENT, 1.0, 0.0), (Color32::from_rgb(40, 230, 255), -0.8, 2.1), (Color32::from_rgb(255, 60, 160), 0.6, 4.2)];
    for (c, spd, ph) in blobs {
        let a = t * spd + ph;
        let off = egui::vec2(a.cos(), a.sin()) * r * 0.3;
        p.circle_filled(center + off, r * 0.5, c.linear_multiply(0.55));
    }
    p.circle_stroke(center, r, Stroke::new(2.5, ACCENT));
}

impl eframe::App for Setup {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.stage == Stage::Working {
            let r = self.progress.lock().unwrap().result.clone();
            match r {
                Some(Ok(())) => self.stage = Stage::Done,
                Some(Err(e)) => self.stage = Stage::Failed(e),
                None => {}
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
        let t = ctx.input(|i| i.time) as f32;

        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(18.0);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(120.0, 110.0), egui::Sense::hover());
                orb(ui, rect.center(), 36.0, t);
                ui.label(RichText::new("NeedleVoice").size(28.0).strong());
                ui.label(RichText::new("Your voice assistant, powered by Needle 3").color(Color32::from_gray(160)));
                ui.add_space(14.0);
            });

            match self.stage.clone() {
                Stage::Welcome => {
                    egui::Grid::new("opts").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                        ui.label("Name your assistant");
                        ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(200.0));
                        ui.end_row();
                        ui.label("");
                        ui.label(RichText::new(format!("You'll say \"Hey {}\"", self.name.trim())).small().color(ACCENT));
                        ui.end_row();
                        ui.label("Install to");
                        ui.add(egui::TextEdit::singleline(&mut self.dir).desired_width(340.0));
                        ui.end_row();
                    });
                    ui.add_space(6.0);
                    ui.checkbox(&mut self.autostart, "Start automatically with Windows (runs quietly in the background)");
                    ui.checkbox(&mut self.desktop, "Desktop shortcut to Settings");
                    ui.checkbox(&mut self.launch, "Start listening when setup finishes");
                    ui.add_space(14.0);
                    ui.vertical_centered(|ui| {
                        let ok = !self.name.trim().is_empty() && !self.dir.trim().is_empty();
                        if ui.add_enabled(ok, big_button("Install")).clicked() {
                            self.start(&ctx);
                        }
                        ui.label(RichText::new("No admin rights needed · Everything runs offline").small().color(Color32::from_gray(130)));
                    });
                }
                Stage::Working => {
                    let g = self.progress.lock().unwrap();
                    ui.add_space(20.0);
                    ui.label(&g.step);
                    ui.add(egui::ProgressBar::new(g.fraction).show_percentage());
                }
                Stage::Done => {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("All set!").size(20.0).strong().color(ACCENT));
                        ui.add_space(6.0);
                        ui.label(format!("Try saying \"Hey {}, open Chrome\"", self.name.trim()));
                        ui.label(format!("or \"Hey {}, what is the Haber process?\"", self.name.trim()));
                        ui.add_space(6.0);
                        ui.label(RichText::new("Find it in the system tray. Change the voice, colour and more in NeedleVoice Settings.").small().color(Color32::from_gray(150)));
                        ui.add_space(10.0);
                        if ui.button("Open Settings").clicked() {
                            let exe = PathBuf::from(self.dir.trim()).join(nv_core::CONFIG_EXE);
                            let _ = win::spawn_detached(&exe, &[]);
                        }
                        ui.add_space(8.0);
                        if ui.add(big_button("Finish")).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("Tool calling by Needle 3 — Cactus Compute, Inc. (Apache-2.0)")
                                .small()
                                .color(Color32::from_gray(120)),
                        );
                        ui.label(
                            RichText::new("Speech by whisper.cpp and sherpa-onnx · voices by Piper and Kokoro")
                                .small()
                                .color(Color32::from_gray(120)),
                        );
                    });
                }
                Stage::Failed(e) => {
                    ui.label(RichText::new("Something went wrong").size(16.0).color(Color32::from_rgb(255, 90, 100)));
                    ui.label(e);
                    if ui.button("Try again").clicked() {
                        self.stage = Stage::Welcome;
                        *self.progress.lock().unwrap() = Progress::default();
                    }
                }
            }
        });
    }
}

fn big_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).size(16.0).strong().color(Color32::BLACK))
        .fill(ACCENT)
        .min_size(egui::vec2(180.0, 38.0))
        .corner_radius(8.0)
}

/// Relay an uninstall to the program that is installed to do it.
///
/// Setup used to *be* the uninstaller, which is what put a copy of this file in
/// the install folder. Now it just hands over — the ordinary division of labour,
/// and the reason this executable has no uninstall code in it.
fn relay_uninstall(args: &[String]) -> i32 {
    win::com_init();
    let dir = args
        .iter()
        .position(|a| a == "--dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .or_else(win::installed_location)
        .unwrap_or_else(nv_core::paths::default_install_dir);
    let quiet = |_: f32, _: &str| {};
    let delete_settings = args.iter().any(|a| a == "--delete-settings");
    match do_uninstall(&dir, delete_settings, &quiet) {
        Ok(()) => 0,
        Err(e) => {
            report_step(&e);
            1
        }
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    let uninstall = args.iter().any(|a| a == "--uninstall" || a == "/uninstall");

    // Headless: `--silent [--dir <path>] [--no-launch] [--no-autostart]`.
    if args.iter().any(|a| a == "--silent") || uninstall {
        if uninstall {
            std::process::exit(relay_uninstall(&args));
        }
        win::com_init();
        let dir = args
            .iter()
            .position(|a| a == "--dir")
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
            .or_else(win::installed_location)
            .unwrap_or_else(nv_core::paths::default_install_dir);
        let quiet = |_: f32, _: &str| {};
        let name = Config::load().agent_name;
        let autostart = !args.iter().any(|a| a == "--no-autostart");
        let launch = !args.iter().any(|a| a == "--no-launch");
        let r = do_install(&dir, &name, autostart, false, launch, &quiet);
        std::process::exit(if r.is_ok() { 0 } else { 1 });
    }

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("NeedleVoice Setup")
            .with_inner_size([560.0, 520.0])
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        "NeedleVoice Setup",
        opts,
        Box::new(move |cc| {
            let mut v = egui::Visuals::dark();
            v.panel_fill = Color32::from_rgb(13, 15, 20);
            v.selection.bg_fill = ACCENT.linear_multiply(0.4);
            v.selection.stroke = Stroke::new(1.0, ACCENT);
            cc.egui_ctx.set_visuals(v);
            Ok(Box::new(Setup::new()))
        }),
    )
}
