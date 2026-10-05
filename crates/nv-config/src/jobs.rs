//! Long-running work (downloads, app scans, Needle test runs) on background
//! threads, polled by the UI each frame.

use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct JobState {
    pub label: String,
    /// 0–1, or negative when indeterminate.
    pub progress: f32,
    pub result: Option<Result<String, String>>,
}

#[derive(Clone)]
pub struct Job(pub Arc<Mutex<JobState>>);

impl Job {
    pub fn spawn(
        ctx: &eframe::egui::Context,
        label: impl Into<String>,
        work: impl FnOnce(&dyn Fn(f32)) -> Result<String, String> + Send + 'static,
    ) -> Job {
        let job = Job(Arc::new(Mutex::new(JobState { label: label.into(), progress: -1.0, result: None })));
        let j = job.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            nv_core::win::com_init();
            let set = |p: f32| {
                j.0.lock().unwrap().progress = p;
                ctx.request_repaint();
            };
            let r = work(&set);
            j.0.lock().unwrap().result = Some(r);
            ctx.request_repaint();
        });
        job
    }

    pub fn done(&self) -> bool {
        self.0.lock().unwrap().result.is_some()
    }
}

/// Download any file over HTTPS with progress (used for Whisper models).
///
/// The bytes land in a `.partial` beside the destination and are only moved into
/// place once the whole file has arrived. A failed download used to leave that
/// partial file behind, and the next attempt over the same file resumed from a
/// stale half — or, worse, a truncated model was treated as installed.
pub fn download_file(url: &str, dest: &std::path::Path, progress: &dyn Fn(f32)) -> Result<(), String> {
    use std::io::{Read, Write};
    // The suffix is appended rather than replacing the extension: `with_extension`
    // turned "ggml-base.en-q5_1.bin" into "ggml-base.en-q5_1.partial", losing
    // which model it was for.
    let mut name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".partial");
    let tmp = dest.with_file_name(name);
    if let Some(dir) = tmp.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
    }
    let _ = std::fs::remove_file(&tmp);

    let attempt = (|| -> Result<(), String> {
        let (mut reader, total) = ureq_get(url)?;
        let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 1 << 16];
        let mut done = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            done += n as u64;
            if total > 0 {
                progress(done as f32 / total as f32);
            }
        }
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        // A short reply is a truncated file, not a small one.
        if total > 0 && done != total {
            return Err(format!("the download stopped early ({done} of {total} bytes)"));
        }
        std::fs::rename(&tmp, dest).map_err(|e| e.to_string())
    })();
    if attempt.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    attempt
}

fn ureq_get(url: &str) -> Result<(Box<dyn std::io::Read + Send>, u64), String> {
    nv_core::voices::http_get(url)
}
