//! NeedleVoice background agent: listens for "hey <name>", shows the neon
//! bubble, and runs commands through Needle 3.
#![windows_subsystem = "windows"]

mod audio;
mod bubble;
mod chime;
mod kws;
mod listener;
mod logger;
mod overlay;
mod scheduler;
mod stt;
mod tray;
mod tts;
#[cfg(test)]
mod pipeline_test;

use std::sync::mpsc::channel;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use nv_core::apps::AppIndex;
use nv_core::{win, Config};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, TranslateMessage, MSG};

/// Rescan installed apps in the background and swap in the result.
pub fn spawn_app_scan(cfg: Config, apps: Arc<RwLock<AppIndex>>) {
    std::thread::Builder::new()
        .name("app-scan".into())
        .spawn(move || {
            let t = std::time::Instant::now();
            let index = AppIndex::scan(&cfg);
            log::info!("found {} apps in {:?}", index.apps.len(), t.elapsed());
            index.save_cache();
            *apps.write().unwrap() = index;
        })
        .ok();
}

fn main() {
    logger::init();
    let args: Vec<String> = std::env::args().collect();
    let background = args.iter().any(|a| a == "--background");
    let restarted = args.iter().any(|a| a == "--restarted");

    // `--transcribe <in.wav> --out <out.txt>`: run Whisper on a 16 kHz mono
    // 16-bit WAV and write the text. Used by the config app's mic check.
    if let Some(i) = args.iter().position(|a| a == "--transcribe") {
        let input = args.get(i + 1).cloned().unwrap_or_default();
        let out = args.iter().position(|a| a == "--out").and_then(|j| args.get(j + 1)).cloned().unwrap_or_default();
        let cfg = Config::load();
        let text = match std::fs::read(&input) {
            Ok(bytes) => {
                let pos = bytes.windows(4).position(|w| w == b"data").map(|p| p + 8).unwrap_or(44);
                let audio: Vec<f32> = bytes[pos.min(bytes.len())..]
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                    .collect();
                let models = nv_core::paths::models_dir();
                let mut stt = stt::Stt::new(models.join(&cfg.whisper_model), cfg.threads, &cfg.agent_name);
                stt.transcribe(&audio).unwrap_or_else(|e| format!("ERROR: {e}"))
            }
            Err(e) => format!("ERROR: {e}"),
        };
        let _ = std::fs::write(out, text);
        return;
    }

    // `--bench-stt <model-file> <clip.wav>…`: transcribe clips with a given
    // Whisper model and report how long each took. This is the same code path
    // the assistant uses, so the numbers mean something.
    if args.iter().any(|a| a == "--bench-stt") {
        let rest: Vec<&String> = args.iter().skip_while(|a| *a != "--bench-stt").skip(1).collect();
        let Some((model, clips)) = rest.split_first() else {
            eprintln!("usage: --bench-stt <model-file> <clip.wav>…");
            return;
        };
        let cfg = nv_core::config::Config::load();
        let path = nv_core::paths::models_dir().join(model.as_str());
        let mut stt = stt::Stt::new(path, cfg.threads, &cfg.agent_name);
        let mut total = 0f64;
        for clip in clips {
            let Ok(bytes) = std::fs::read(clip) else {
                eprintln!("cannot read {clip}");
                continue;
            };
            let audio: Vec<f32> = bytes[44..]
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                .collect();
            let secs = audio.len() as f32 / 16000.0;
            // One warm-up run, then three timed ones.
            let _ = stt.transcribe(&audio);
            let mut best = f64::MAX;
            let mut text = String::new();
            for _ in 0..3 {
                let t = std::time::Instant::now();
                text = stt.transcribe(&audio).unwrap_or_default();
                best = best.min(t.elapsed().as_secs_f64());
            }
            total += best;
            println!("{best:5.2}s  ({secs:.1}s audio, {:.1}x realtime)  {text}", secs as f64 / best);
        }
        println!("total {total:.2}s across {} clips", clips.len());
        return;
    }

    // `--chime`: play the wake-up chime once and exit. The settings app uses
    // this so you can hear exactly what waking sounds like.
    if args.iter().any(|a| a == "--chime") {
        chime::play();
        // Give the asynchronous sound time to finish before the process goes.
        std::thread::sleep(Duration::from_millis(800));
        return;
    }

    // `--say <text> [--config <file>]`: speak once and exit. The config app
    // uses this to preview voices with unsaved settings.
    if let Some(i) = args.iter().position(|a| a == "--say") {
        let text = args.get(i + 1).cloned().unwrap_or_else(|| "Hi! This is how I sound.".into());
        let cfg = match args.iter().position(|a| a == "--config") {
            Some(j) => Config::load_from(std::path::Path::new(args.get(j + 1).map(String::as_str).unwrap_or(""))),
            None => Config::load(),
        };
        tts::speak_once(&cfg, &text);
        return;
    }

    // One listener at a time. After a settings restart, wait for the old one to exit.
    let mut guard = win::SingleInstance::acquire(nv_core::AGENT_MUTEX);
    if guard.is_none() && restarted {
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            guard = win::SingleInstance::acquire(nv_core::AGENT_MUTEX);
            if guard.is_some() {
                break;
            }
        }
    }
    let Some(guard) = guard else {
        return; // already running
    };

    let cfg = Config::load();
    if !nv_core::paths::config_file().exists() {
        let _ = cfg.save();
    }
    log::info!("NeedleVoice {} starting", env!("CARGO_PKG_VERSION"));

    // Needle's parallel kernels use rayon; cap them at the configured thread count.
    std::env::set_var("RAYON_NUM_THREADS", cfg.threads.to_string());

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    win::com_init();

    // Apps: start from the cache instantly, refresh in the background now and every 30 min.
    let apps = Arc::new(RwLock::new(AppIndex::load_cache().unwrap_or_default()));
    spawn_app_scan(cfg.clone(), apps.clone());
    {
        let (cfg, apps) = (cfg.clone(), apps.clone());
        std::thread::Builder::new()
            .name("app-rescan-timer".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(30 * 60));
                spawn_app_scan(cfg.clone(), apps.clone());
            })
            .ok();
    }

    let shared = overlay::Shared::new();
    if let Err(e) = overlay::create(shared.clone(), cfg.show_overlay, cfg.overlay_size, cfg.overlay_margin, cfg.accent_rgb()) {
        log::error!("overlay: {e}");
    }

    // One voice for the whole agent: alarms and replies must not each load
    // their own copy of the speech model.
    let tts = Arc::new(tts::Tts::start(&cfg, shared.clone()));

    let (ctl_tx, ctl_rx) = channel();
    {
        let (cfg, shared, apps, tts) = (cfg.clone(), shared.clone(), apps.clone(), tts.clone());
        std::thread::Builder::new()
            .name("listener".into())
            .spawn(move || listener::run(cfg, shared, apps, tts, ctl_rx))
            .expect("listener thread");
    }
    scheduler::spawn(cfg.clone(), shared.clone(), apps.clone(), tts.clone());

    // `--demo`: play through every bubble state once (used by the config app's
    // "Preview" button and for screenshots).
    if args.iter().any(|a| a == "--demo") {
        let (shared, cfg) = (shared.clone(), cfg.clone());
        std::thread::spawn(move || {
            use bubble::Mode;
            let sleep = |ms| std::thread::sleep(Duration::from_millis(ms));
            sleep(300);
            shared.set_mode(Mode::Listening);
            for i in 0..90 {
                shared.set_level(((i as f32 * 0.35).sin() * 0.5 + 0.5) * 0.8);
                sleep(30);
            }
            shared.set_level(0.0);
            shared.set_mode(Mode::Thinking);
            sleep(1500);
            let action = nv_core::brain::Action::OpenApp("Google Chrome".into());
            let line = nv_core::personality::line(cfg.personality, &nv_core::personality::Moment::Done(&action));
            if tts.active() {
                shared.set_speaking(true);
            }
            tts.say(&line);
            shared.set_mode(Mode::Success);
        });
    }

    if let Err(e) = tray::create(ctl_tx, cfg.clone(), !background && !restarted) {
        log::error!("tray: {e}");
    }

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    tray::remove_icon();
    if tray::wants_restart() {
        drop(guard);
        if let Ok(exe) = std::env::current_exe() {
            let _ = win::spawn_detached(&exe, &["--background", "--restarted"]);
        }
    }
    log::info!("exiting");
    std::process::exit(0);
}
