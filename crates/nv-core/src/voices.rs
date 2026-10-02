//! Catalogue of neural voices and the downloader that installs them.
//!
//! Voices come from sherpa-onnx's model releases (Piper and Kokoro, both run
//! offline). Each pack is extracted to `models\voices\<pack id>\`.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// Natural neural voices, fast. Default.
    Piper,
    /// Most human-sounding; heavier and slower to start talking.
    Kokoro,
    /// The built-in Windows voices (robotic, but zero download).
    System,
}

impl Engine {
    pub const ALL: [Engine; 3] = [Engine::Piper, Engine::Kokoro, Engine::System];

    /// Short label for a segmented control.
    pub fn label_short(self) -> &'static str {
        match self {
            Engine::Piper => "Natural",
            Engine::Kokoro => "Ultra-realistic",
            Engine::System => "Windows built-in",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Engine::Piper => "Natural (Piper) — fast",
            Engine::Kokoro => "Ultra-realistic (Kokoro) — slower to start",
            Engine::System => "Windows built-in — robotic",
        }
    }
}

pub struct PiperVoice {
    /// Pack id = archive name without `.tar.bz2`.
    pub id: &'static str,
    pub label: &'static str,
    pub size_mb: u32,
}

const fn p(id: &'static str, label: &'static str, size_mb: u32) -> PiperVoice {
    PiperVoice { id, label, size_mb }
}

/// Piper voice that ships with the installer.
pub const DEFAULT_PIPER: &str = "vits-piper-en_US-lessac-medium";

pub const PIPER_VOICES: &[PiperVoice] = &[
    p("vits-piper-en_US-lessac-medium", "Lessac — US female, clear (default)", 64),
    p("vits-piper-en_US-amy-medium", "Amy — US female, bright", 64),
    p("vits-piper-en_US-hfc_female-medium", "Hannah — US female, warm", 64),
    p("vits-piper-en_US-kristin-medium", "Kristin — US female, calm", 64),
    p("vits-piper-en_US-ljspeech-medium", "Linda — US female, audiobook", 64),
    p("vits-piper-en_US-ryan-medium", "Ryan — US male, friendly", 64),
    p("vits-piper-en_US-hfc_male-medium", "Henry — US male, warm", 64),
    p("vits-piper-en_US-joe-medium", "Joe — US male, casual", 64),
    p("vits-piper-en_US-john-medium", "John — US male, newsreader", 64),
    p("vits-piper-en_US-bryce-medium", "Bryce — US male, young", 64),
    p("vits-piper-en_US-norman-medium", "Norman — US male, deep", 64),
    p("vits-piper-en_US-kusal-medium", "Kusal — US male, soft", 64),
    p("vits-piper-en_US-sam-medium", "Sam — US, neutral", 64),
    p("vits-piper-en_GB-jenny_dioco-medium", "Jenny — UK female", 64),
    p("vits-piper-en_GB-cori-medium", "Cori — UK female, crisp", 64),
    p("vits-piper-en_GB-alba-medium", "Alba — Scottish female", 64),
    p("vits-piper-en_GB-southern_english_female-medium", "Sophie — UK female, southern", 77),
    p("vits-piper-en_GB-alan-medium", "Alan — UK male", 64),
    p("vits-piper-en_GB-northern_english_male-medium", "Noah — UK male, northern", 64),
    p("vits-piper-en_GB-southern_english_male-medium", "Oliver — UK male, southern", 77),
    p("vits-piper-en_US-ryan-high", "Ryan HQ — US male (higher quality, slower)", 110),
    p("vits-piper-en_US-lessac-high", "Lessac HQ — US female (higher quality, slower)", 110),
    p("vits-piper-en_US-glados", "GLaDOS — sarcastic robot (just for fun)", 64),
];

/// The Kokoro pack (one download, many speakers).
pub const KOKORO_PACK: &str = "kokoro-multi-lang-v1_0";
pub const KOKORO_SIZE_MB: u32 = 334;

pub struct KokoroVoice {
    pub name: &'static str,
    /// Speaker id inside the pack.
    pub sid: i32,
    pub label: &'static str,
}

const fn k(name: &'static str, sid: i32, label: &'static str) -> KokoroVoice {
    KokoroVoice { name, sid, label }
}

pub const DEFAULT_KOKORO: &str = "af_heart";

/// English speakers of kokoro v1.0 (ids from the model's metadata).
pub const KOKORO_VOICES: &[KokoroVoice] = &[
    k("af_heart", 3, "Heart — US female, warm ★ most natural"),
    k("af_bella", 2, "Bella — US female, expressive ★"),
    k("af_nicole", 6, "Nicole — US female, soft & breathy"),
    k("af_sarah", 9, "Sarah — US female, friendly"),
    k("af_sky", 10, "Sky — US female, light"),
    k("af_nova", 7, "Nova — US female, confident"),
    k("af_jessica", 4, "Jessica — US female"),
    k("af_river", 8, "River — US female, relaxed"),
    k("af_alloy", 0, "Alloy — US female, neutral"),
    k("af_aoede", 1, "Aoede — US female"),
    k("af_kore", 5, "Kore — US female"),
    k("am_michael", 16, "Michael — US male, natural ★"),
    k("am_fenrir", 14, "Fenrir — US male, energetic"),
    k("am_puck", 18, "Puck — US male, playful"),
    k("am_adam", 11, "Adam — US male"),
    k("am_eric", 13, "Eric — US male, steady"),
    k("am_liam", 15, "Liam — US male, young"),
    k("am_onyx", 17, "Onyx — US male, deep"),
    k("am_echo", 12, "Echo — US male"),
    k("am_santa", 19, "Santa — US male, jolly"),
    k("bf_emma", 21, "Emma — UK female, polished ★"),
    k("bf_isabella", 22, "Isabella — UK female"),
    k("bf_alice", 20, "Alice — UK female"),
    k("bf_lily", 23, "Lily — UK female, gentle"),
    k("bm_george", 26, "George — UK male, distinguished ★"),
    k("bm_fable", 25, "Fable — UK male, storyteller"),
    k("bm_lewis", 27, "Lewis — UK male"),
    k("bm_daniel", 24, "Daniel — UK male"),
];

pub fn kokoro_voice(name: &str) -> &'static KokoroVoice {
    KOKORO_VOICES.iter().find(|v| v.name == name).unwrap_or(&KOKORO_VOICES[0])
}

pub fn voices_dir() -> PathBuf {
    crate::paths::models_dir().join("voices")
}

pub fn pack_dir(id: &str) -> PathBuf {
    voices_dir().join(id)
}

/// First `.onnx` file in a pack folder.
pub fn pack_model(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "onnx"))
}

pub fn is_installed(id: &str) -> bool {
    let dir = pack_dir(id);
    pack_model(&dir).is_some() && dir.join("tokens.txt").exists()
}

pub fn remove(id: &str) -> std::io::Result<()> {
    std::fs::remove_dir_all(pack_dir(id))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(15)))
        .timeout_recv_response(Some(std::time::Duration::from_secs(45)))
        .build()
        .into()
}

/// GET a URL, returning the body reader and its length (0 if unknown).
pub fn http_get(url: &str) -> Result<(Box<dyn Read + Send>, u64), String> {
    let resp = agent().get(url).call().map_err(|e| format!("download failed: {e}"))?;
    let total = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0u64);
    Ok((Box::new(resp.into_body().into_reader()), total))
}

const RELEASE: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models";
/// Hugging Face mirror of the same packs (one repo per pack). Much more
/// reliable than GitHub's release CDN, which often times out on big files.
const HF: &str = "https://huggingface.co/csukuangfj";

/// Files we keep from the (large, multi-language) Kokoro archive.
fn keep_entry(pack: &str, rel: &str) -> bool {
    if pack != KOKORO_PACK {
        return true;
    }
    rel.starts_with("espeak-ng-data/")
        || matches!(rel, "model.onnx" | "voices.bin" | "tokens.txt" | "lexicon-us-en.txt" | "lexicon-gb-en.txt" | "LICENSE")
}

/// Download and install a voice pack. `progress(downloaded, total)` is
/// called as bytes arrive. Blocking; run on a worker thread.
///
/// Tries Hugging Face first, then GitHub (twice). The final folder is only
/// swapped in once everything arrived, so failures never leave a broken voice.
pub fn download(id: &str, mut progress: impl FnMut(u64, u64)) -> Result<(), String> {
    let tmp = voices_dir().join(format!(".{id}.partial"));
    let mut errors = Vec::new();
    for attempt in 0..3 {
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let r = if attempt == 0 { download_hf(id, &tmp, &mut progress) } else { download_github(id, &tmp, &mut progress) };
        match r {
            Ok(()) => {
                let final_dir = pack_dir(id);
                let _ = std::fs::remove_dir_all(&final_dir);
                std::fs::rename(&tmp, &final_dir).map_err(|e| e.to_string())?;
                if !is_installed(id) {
                    return Err("the download didn't contain a voice model".into());
                }
                return Ok(());
            }
            Err(e) => {
                log::warn!("voice download attempt {} failed: {e}", attempt + 1);
                errors.push(e);
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Err(format!("Couldn't download the voice. Check your internet connection and try again. ({})", errors.join("; ")))
}

/// Per-file download from the Hugging Face mirror. The espeak-ng
/// pronunciation data is identical in every pack, so it's copied from a voice
/// that's already installed instead of downloading hundreds of small files.
fn download_hf(id: &str, dest: &Path, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    let espeak_src = std::fs::read_dir(voices_dir())
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path().join("espeak-ng-data"))
        .find(|p| p.join("phontab").exists())
        .ok_or("no local espeak-ng-data to reuse")?;

    let (mut list, _) = http_get(&format!("https://huggingface.co/api/models/csukuangfj/{id}/tree/main"))?;
    let mut json = String::new();
    list.read_to_string(&mut json).map_err(|e| e.to_string())?;
    let entries: Vec<serde_json::Value> = serde_json::from_str(&json).map_err(|e| format!("bad file list: {e}"))?;
    let files: Vec<(String, u64)> = entries
        .iter()
        .filter(|e| e["type"] == "file")
        .filter_map(|e| {
            let path = e["path"].as_str()?.to_string();
            let size = e["lfs"]["size"].as_u64().or(e["size"].as_u64()).unwrap_or(0);
            Some((path, size))
        })
        .filter(|(p, _)| !p.starts_with('.') && ![".md", ".fst", ".py", ".sh"].iter().any(|x| p.ends_with(x)) && keep_entry(id, p))
        .collect();
    if !files.iter().any(|(p, _)| p.ends_with(".onnx")) {
        return Err("mirror has no model file".into());
    }
    let total: u64 = files.iter().map(|(_, s)| s).sum();
    let mut done = 0u64;
    for (path, _) in &files {
        let (mut body, _) = http_get(&format!("{HF}/{id}/resolve/main/{path}"))?;
        let mut out = std::fs::File::create(dest.join(path)).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = body.read(&mut buf).map_err(|e| format!("connection dropped: {e}"))?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut out, &buf[..n]).map_err(|e| e.to_string())?;
            done += n as u64;
            progress(done, total);
        }
    }
    copy_dir(&espeak_src, &dest.join("espeak-ng-data")).map_err(|e| format!("copying espeak data: {e}"))
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)?.flatten() {
        let to = dst.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), to)?;
        }
    }
    Ok(())
}

/// Whole-pack `.tar.bz2` from GitHub releases.
fn download_github(id: &str, dest: &Path, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    let (body, total) = http_get(&format!("{RELEASE}/{id}.tar.bz2"))?;

    struct Counting<'a, R> {
        inner: R,
        done: u64,
        total: u64,
        cb: &'a mut dyn FnMut(u64, u64),
    }
    impl<R: Read> Read for Counting<'_, R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.done += n as u64;
            (self.cb)(self.done, self.total);
            Ok(n)
        }
    }

    let reader = Counting { inner: body, done: 0, total, cb: progress };
    let mut archive = tar::Archive::new(bzip2::read::BzDecoder::new(reader));
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| format!("connection dropped: {e}"))?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        // Archives contain a top-level `<id>/` folder; strip it.
        let rel: PathBuf = path.components().skip(1).collect();
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if rel_str.is_empty() || rel_str.contains("..") || !keep_entry(id, &rel_str) {
            continue;
        }
        let to = dest.join(&rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        if entry.header().entry_type().is_file() {
            entry.unpack(&to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
