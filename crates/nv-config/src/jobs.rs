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
pub fn download_file(url: &str, dest: &std::path::Path, progress: &dyn Fn(f32)) -> Result<(), String> {
    use std::io::{Read, Write};
    let resp = ureq_get(url)?;
    let total = resp.1;
    let mut reader = resp.0;
    let tmp = dest.with_extension("partial");
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
    drop(file);
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())
}

fn ureq_get(url: &str) -> Result<(Box<dyn std::io::Read + Send>, u64), String> {
    nv_core::voices::http_get(url)
}
