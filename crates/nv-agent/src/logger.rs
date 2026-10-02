//! Minimal file logger: `%APPDATA%\NeedleVoice\agent.log`, trimmed at startup.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

struct FileLogger {
    file: Mutex<std::fs::File>,
}

impl log::Log for FileLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info && (m.target().starts_with("nv_") || m.target().starts_with("NeedleVoice"))
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{h:02}:{m:02}:{s:02}Z {:5} {}", r.level(), r.args());
        }
    }

    fn flush(&self) {}
}

pub fn init() {
    let path = nv_core::paths::log_file();
    // Keep the log small: start fresh once it passes 1 MB.
    if std::fs::metadata(&path).map(|m| m.len() > 1_000_000).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("old.log"));
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
        let logger = Box::leak(Box::new(FileLogger { file: Mutex::new(file) }));
        let _ = log::set_logger(logger);
        log::set_max_level(log::LevelFilter::Info);
    }
}
