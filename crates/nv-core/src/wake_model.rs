//! Installing the wake-word keyword model.
//!
//! The spotter needs four small files (about 5 MB). They come from the
//! sherpa-onnx release on GitHub, with the Hugging Face mirror as a fallback —
//! the same two-source strategy the voice packs use.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::bpe::Bpe;
use crate::config::Config;

/// Folder under `models/`, and the release asset it comes from.
pub const PACK: &str = "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01";
const RELEASE: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/kws-models";
const MIRROR: &str = "https://huggingface.co/csukuangfj";

/// Only the int8 models are worth the disk space and the speed they cost.
const KEEP: [&str; 8] = [
    "encoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx",
    "decoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx",
    "joiner-epoch-12-avg-2-chunk-16-left-64.int8.onnx",
    "tokens.txt",
    "bpe.model",
    "README.md",
    // The model's own example keywords: a few hundred bytes, and they are what
    // the tokeniser test checks against.
    "keywords.txt",
    "keywords_raw.txt",
];

pub fn dir() -> PathBuf {
    crate::paths::models_dir().join("kws").join(PACK)
}

pub fn is_installed() -> bool {
    let d = dir();
    d.join("tokens.txt").exists() && d.join("bpe.model").exists() && KEEP[0..3].iter().all(|f| d.join(f).exists())
}

pub fn remove() -> std::io::Result<()> {
    std::fs::remove_dir_all(dir())
}

/// Download and install the keyword model. Blocking; run on a worker thread.
pub fn download(mut progress: impl FnMut(u64, u64)) -> Result<(), String> {
    let tmp = crate::paths::models_dir().join("kws").join(format!(".{PACK}.partial"));
    let mut errors = Vec::new();
    for attempt in 0..3 {
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        // One `.tar.bz2` from GitHub is a single 17 MB request and the asset is
        // known to exist; the per-file mirror is the backstop.
        let r = if attempt < 2 { from_release(&tmp, &mut progress) } else { from_mirror(&tmp, &mut progress) };
        match r {
            Ok(()) => {
                let final_dir = dir();
                let _ = std::fs::remove_dir_all(&final_dir);
                std::fs::rename(&tmp, &final_dir).map_err(|e| e.to_string())?;
                if !is_installed() {
                    return Err("the download didn't contain the keyword model files".into());
                }
                return Ok(());
            }
            Err(e) => {
                log::warn!("wake model download attempt {} failed: {e}", attempt + 1);
                errors.push(e);
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Err(format!(
        "Couldn't download the wake-word model. Check your internet connection and try again. ({})",
        errors.join("; ")
    ))
}

/// One file at a time from Hugging Face.
fn from_mirror(dest: &Path, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    // Sizes are known from the release; missing ones just report 0.
    let total = 5_400_000u64;
    let mut done = 0u64;
    for name in KEEP {
        let (mut body, _) = crate::voices::http_get(&format!("{MIRROR}/{PACK}/resolve/main/{name}"))?;
        let mut out = std::fs::File::create(dest.join(name)).map_err(|e| e.to_string())?;
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
    Ok(())
}

/// The whole pack as one `.tar.bz2` from the GitHub release.
fn from_release(dest: &Path, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    let (body, total) = crate::voices::http_get(&format!("{RELEASE}/{PACK}.tar.bz2"))?;

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
        // The archive wraps everything in `<PACK>/`.
        let rel: PathBuf = path.components().skip(1).collect();
        let name = rel.to_string_lossy().replace('\\', "/");
        if !KEEP.contains(&name.as_str()) {
            continue;
        }
        let to = dest.join(&rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        if entry.header().entry_type().is_file() {
            entry.unpack(&to).map_err(|e| format!("connection dropped: {e}"))?;
        }
    }
    Ok(())
}

// ── Tuning and the keyword lines ─────────────────────────────────────────

/// How the spotter is tuned. `boost` rewards the keyword's path through the
/// beam search; `threshold` is the posterior it must reach. Raising the boost
/// or lowering the threshold makes waking easier (and false alarms likelier).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tuning {
    pub boost: f32,
    pub threshold: f32,
}

impl Tuning {
    /// What the sensitivity slider means, ignoring any hand-set values.
    pub fn automatic(cfg: &Config) -> Tuning {
        let s = cfg.wake_sensitivity.clamp(0.0, 1.0);
        Tuning { boost: 1.0 + 2.5 * s, threshold: 0.32 - 0.20 * s }
    }

    /// True when the values come from the slider rather than from the file.
    pub fn is_automatic(cfg: &Config) -> bool {
        cfg.wake_score <= 0.0 && cfg.wake_threshold <= 0.0
    }

    pub fn for_config(cfg: &Config) -> Tuning {
        let mut t = Tuning::automatic(cfg);
        if cfg.wake_score > 0.0 {
            t.boost = cfg.wake_score;
        }
        if cfg.wake_threshold > 0.0 {
            t.threshold = cfg.wake_threshold;
        }
        t
    }
}

/// Every phrase the spotter listens for: each prefix with each name, plus the
/// bare name when that's allowed.
pub fn wake_phrases(cfg: &Config) -> Vec<String> {
    let mut phrases: Vec<String> = Vec::new();
    for name in cfg.wake_names() {
        for prefix in &cfg.wake_prefixes {
            phrases.push(format!("{prefix} {name}"));
        }
        if cfg.allow_name_only {
            phrases.push(name.clone());
        }
    }
    if phrases.is_empty() {
        phrases.push(cfg.agent_name.clone());
    }
    phrases.retain(|p| !p.trim().is_empty());
    let mut seen = std::collections::HashSet::new();
    phrases.retain(|p| seen.insert(p.clone()));
    phrases
}

/// The keyword lines the model is handed, e.g.
/// `▁HE Y ▁NO VA :2.25 #0.220 @hey_nova`.
pub fn keyword_lines(cfg: &Config, bpe: &Bpe) -> String {
    let t = Tuning::for_config(cfg);
    wake_phrases(cfg)
        .iter()
        .map(|p| {
            let toks = bpe.encode(p).join(" ");
            let tag = p.to_lowercase().replace(' ', "_");
            format!("{toks} :{:.2} #{:.3} @{tag}", t.boost, t.threshold)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pieces the model has no pronunciation for — usually a name with digits or
/// non-English letters in it. Each one is a reason waking will be unreliable.
pub fn unpronounceable(cfg: &Config, bpe: &Bpe) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for phrase in wake_phrases(cfg) {
        for piece in bpe.encode(&phrase) {
            if !bpe.has(&piece) {
                let ch: String = piece.trim_start_matches('▁').to_string();
                if !out.contains(&ch) {
                    out.push(ch);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Downloads and installs the real pack. Ignored by default because it is
    /// a 17 MB download over the network.
    #[test]
    #[ignore]
    fn downloads_and_installs() {
        if is_installed() {
            println!("already installed at {}", dir().display());
            return;
        }
        download(|d, t| {
            if t > 0 && d % (4 << 20) < (1 << 16) {
                println!("  {}/{} MB", d >> 20, t >> 20);
            }
        })
        .expect("download failed");
        assert!(is_installed());
        assert!(dir().join("bpe.model").exists());
        assert!(dir().join("tokens.txt").exists());
        println!("installed {}", dir().display());
    }

    fn bpe() -> Option<Bpe> {
        Bpe::load(&dir().join("bpe.model"))
    }

    #[test]
    fn phrases_and_lines() {
        let Some(bpe) = bpe() else {
            eprintln!("wake model not present; skipping");
            return;
        };
        let mut cfg = Config::default();
        cfg.agent_name = "Nova".into();
        cfg.wake_prefixes = vec!["hey".into()];
        cfg.wake_extra_names = vec!["no va".into()];
        assert_eq!(wake_phrases(&cfg), vec!["hey Nova".to_string(), "hey no va".to_string()]);
        let lines = keyword_lines(&cfg, &bpe);
        assert!(lines.contains("@hey_nova"), "{lines}");
        assert!(lines.contains("@hey_no_va"), "{lines}");
        assert!(lines.lines().all(|l| l.contains('#') && l.contains(':')), "{lines}");

        // The automatic tuning follows the slider; hand-set values win.
        assert!(Tuning::is_automatic(&cfg));
        cfg.wake_sensitivity = 1.0;
        let t = Tuning::for_config(&cfg);
        assert!((t.boost - 3.5).abs() < 1e-6 && (t.threshold - 0.12).abs() < 1e-6, "{t:?}");
        cfg.wake_score = 3.0;
        cfg.wake_threshold = 0.1;
        assert!(!Tuning::is_automatic(&cfg));
        assert!(keyword_lines(&cfg, &bpe).contains(":3.00 #0.100"));

        // "Nova" is spellable; a name with a digit is not.
        assert!(unpronounceable(&cfg, &bpe).is_empty(), "{:?}", unpronounceable(&cfg, &bpe));
        let mut odd = cfg.clone();
        odd.agent_name = "Nova2".into();
        odd.wake_extra_names.clear();
        assert!(!unpronounceable(&odd, &bpe).is_empty());
    }
}
