//! NeedleVoice installer / uninstaller. Per-user install, no admin needed.
#![windows_subsystem = "windows"]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, RichText, Stroke};
use nv_core::{paths, win, Config};

static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.tar.zst"));
const ACCENT: Color32 = Color32::from_rgb(182, 255, 46);

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
    uninstall: bool,
    stage: Stage,
    dir: String,
    name: String,
    autostart: bool,
    desktop: bool,
    launch: bool,
    delete_settings: bool,
    progress: Arc<Mutex<Progress>>,
}

impl Setup {
    fn new(uninstall: bool) -> Self {
        let existing = Config::load();
        let dir = win::installed_location().unwrap_or_else(paths::default_install_dir);
        Self {
            uninstall,
            stage: Stage::Welcome,
            dir: dir.display().to_string(),
            name: existing.agent_name.clone(),
            autostart: true,
            desktop: true,
            launch: true,
            delete_settings: false,
            progress: Default::default(),
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        self.stage = Stage::Working;
        let p = self.progress.clone();
        let ctx = ctx.clone();
        let dir = PathBuf::from(self.dir.trim());
        let (name, autostart, desktop, launch, uninstall, delete_settings) =
            (self.name.trim().to_string(), self.autostart, self.desktop, self.launch, self.uninstall, self.delete_settings);
        std::thread::spawn(move || {
            win::com_init();
            let report = |f: f32, s: &str| {
                let mut g = p.lock().unwrap();
                g.fraction = f;
                g.step = s.to_string();
                ctx.request_repaint();
            };
            let r = if uninstall {
                do_uninstall(&dir, delete_settings, &report)
            } else {
                do_install(&dir, &name, autostart, desktop, launch, &report)
            };
            p.lock().unwrap().result = Some(r);
            ctx.request_repaint();
        });
    }
}

fn start_menu_dir() -> PathBuf {
    use windows::Win32::UI::Shell::FOLDERID_Programs;
    win::known_folder(&FOLDERID_Programs).unwrap_or_default().join("NeedleVoice")
}

fn desktop_link() -> PathBuf {
    use windows::Win32::UI::Shell::FOLDERID_Desktop;
    win::known_folder(&FOLDERID_Desktop).unwrap_or_default().join("NeedleVoice Settings.lnk")
}

fn stop_running() {
    win::kill_by_exe(nv_core::AGENT_EXE);
    win::kill_by_exe(nv_core::CONFIG_EXE);
    std::thread::sleep(std::time::Duration::from_millis(400));
}

fn dir_size_kb(dir: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            total += if p.is_dir() { dir_size_kb(&p) * 1024 } else { e.metadata().map(|m| m.len()).unwrap_or(0) };
        }
    }
    total / 1024
}

fn do_install(dir: &Path, name: &str, autostart: bool, desktop: bool, launch: bool, report: &dyn Fn(f32, &str)) -> Result<(), String> {
    report(0.02, "Closing any running copy…");
    stop_running();
    std::fs::create_dir_all(dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;

    // Extract the payload, reporting progress by compressed bytes consumed.
    struct Counting<'a> {
        data: &'a [u8],
        pos: usize,
        report: &'a dyn Fn(f32, &str),
    }
    impl Read for Counting<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
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
    archive.set_overwrite(true);
    archive.unpack(dir).map_err(|e| format!("Couldn't copy files: {e}"))?;

    report(0.88, "Creating shortcuts…");
    let agent = dir.join(nv_core::AGENT_EXE);
    let config_exe = dir.join(nv_core::CONFIG_EXE);
    let uninst = dir.join(nv_core::UNINSTALL_EXE);
    if let Ok(me) = std::env::current_exe() {
        if me != uninst {
            std::fs::copy(&me, &uninst).map_err(|e| format!("Couldn't write uninstaller: {e}"))?;
        }
    }
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
    win::register_uninstaller(dir, env!("CARGO_PKG_VERSION"), dir_size_kb(dir) as u32)?;

    if launch {
        report(0.98, "Starting the assistant…");
        win::spawn_detached(&agent, &[]).map_err(|e| e.to_string())?;
    }
    report(1.0, "Done");
    Ok(())
}

fn do_uninstall(dir: &Path, delete_settings: bool, report: &dyn Fn(f32, &str)) -> Result<(), String> {
    report(0.1, "Stopping the assistant…");
    stop_running();
    report(0.3, "Removing shortcuts…");
    let _ = win::set_autostart(false, Path::new(""));
    let _ = std::fs::remove_dir_all(start_menu_dir());
    let _ = std::fs::remove_file(desktop_link());
    win::unregister_uninstaller();
    if delete_settings {
        let _ = std::fs::remove_dir_all(paths::data_dir());
    }
    report(0.6, "Removing files…");
    // Delete everything except ourselves (we're probably Uninstall.exe in that folder).
    let me = std::env::current_exe().ok();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if Some(&p) == me.as_ref() {
                continue;
            }
            let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        }
    }
    // Remove the folder (and this exe) once we've exited.
    use std::os::windows::process::CommandExt;
    let script = format!("ping 127.0.0.1 -n 3 > nul & rmdir /s /q \"{}\"", dir.display());
    // raw_arg: Rust's quoting (\") confuses cmd.exe.
    let _ = std::process::Command::new("cmd").raw_arg(format!("/C {script}")).creation_flags(0x0800_0000).spawn();
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
                let sub = if self.uninstall { "Uninstall" } else { "Your voice assistant, powered by Needle 3" };
                ui.label(RichText::new(sub).color(Color32::from_gray(160)));
                ui.add_space(14.0);
            });

            match self.stage.clone() {
                Stage::Welcome if self.uninstall => {
                    ui.label("This removes NeedleVoice, its shortcuts and auto-start from this PC.");
                    ui.checkbox(&mut self.delete_settings, "Also delete my settings");
                    ui.add_space(16.0);
                    ui.vertical_centered(|ui| {
                        if ui.add(big_button("Uninstall")).clicked() {
                            self.start(&ctx);
                        }
                    });
                }
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
                        if self.uninstall {
                            ui.label(RichText::new("NeedleVoice has been removed.").size(16.0));
                        } else {
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
                        }
                        ui.add_space(8.0);
                        if ui.add(big_button("Finish")).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if !self.uninstall {
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
                        }
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

fn main() -> eframe::Result {
    let uninstall = std::env::args().any(|a| a == "--uninstall")
        || std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case(nv_core::UNINSTALL_EXE)))
            .unwrap_or(false);
    // Headless: `--silent [--dir <path>] [--no-launch] [--no-autostart]`.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--silent") {
        win::com_init();
        let dir = args
            .iter()
            .position(|a| a == "--dir")
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
            .or_else(win::installed_location)
            .unwrap_or_else(paths::default_install_dir);
        let quiet = |_: f32, _: &str| {};
        let r = if uninstall {
            do_uninstall(&dir, args.iter().any(|a| a == "--delete-settings"), &quiet)
        } else {
            let name = Config::load().agent_name;
            let autostart = !args.iter().any(|a| a == "--no-autostart");
            let launch = !args.iter().any(|a| a == "--no-launch");
            do_install(&dir, &name, autostart, false, launch, &quiet)
        };
        std::process::exit(if r.is_ok() { 0 } else { 1 });
    }

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(if uninstall { "Uninstall NeedleVoice" } else { "NeedleVoice Setup" })
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
            Ok(Box::new(Setup::new(uninstall)))
        }),
    )
}
