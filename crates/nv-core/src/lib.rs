//! Shared logic for the NeedleVoice agent, config app and installer:
//! settings, app discovery, the Needle 3 "brain" and the actions it can take.

pub mod actions;
pub mod apps;
pub mod bpe;
pub mod brain;
pub mod config;
pub mod fuzzy;
pub mod paths;
pub mod schedule;
pub mod schedule_parse;
pub mod voice;
pub mod voices;
pub mod personality;
pub mod tools;
pub mod wake;
pub mod wake_model;
pub mod win;

pub use config::Config;

/// Product name, used for folders, registry keys and window titles.
pub const PRODUCT: &str = "NeedleVoice";
pub const AGENT_EXE: &str = "NeedleVoice.exe";
pub const CONFIG_EXE: &str = "NeedleVoiceConfig.exe";
pub const UNINSTALL_EXE: &str = "Uninstall.exe";
pub const NEEDLE_MODEL: &str = "needle3.cact";
pub const DEFAULT_WHISPER_MODEL: &str = "ggml-medium.en-q5_0.bin";
/// The Moonshine packs sherpa-onnx publishes, best first. Both run at the same
/// speed; base is the more accurate of the two.
pub const MOONSHINE_PACKS: [&str; 2] =
    ["sherpa-onnx-moonshine-base-en-int8", "sherpa-onnx-moonshine-tiny-en-int8"];
/// The pack the settings app downloads when none is installed.
pub const MOONSHINE_PACK: &str = MOONSHINE_PACKS[0];
/// Tarball the settings app downloads when it is missing.
pub const MOONSHINE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-moonshine-base-en-int8.tar.bz2";

/// Where a Moonshine pack lives.
pub fn moonshine_pack_dir(pack: &str) -> std::path::PathBuf {
    paths::models_dir().join("moonshine").join(pack)
}

/// The pack to use: the best one that is actually on disk, or where base would
/// go if none is, so callers get a sensible path either way.
pub fn moonshine_dir() -> std::path::PathBuf {
    MOONSHINE_PACKS
        .iter()
        .map(|p| moonshine_pack_dir(p))
        .find(|d| pack_complete(d))
        .unwrap_or_else(|| moonshine_pack_dir(MOONSHINE_PACK))
}

/// Are all five files of a pack on disk?
pub fn pack_complete(dir: &std::path::Path) -> bool {
    ["preprocess.onnx", "encode.int8.onnx", "uncached_decode.int8.onnx", "cached_decode.int8.onnx", "tokens.txt"]
        .iter()
        .all(|f| dir.join(f).exists())
}

/// Is any Moonshine pack usable?
pub fn moonshine_installed() -> bool {
    MOONSHINE_PACKS.iter().any(|p| pack_complete(&moonshine_pack_dir(p)))
}

/// The Whisper model to load. If the configured file is not there — a fresh
/// install only carries the small one — fall back to any model in the folder
/// rather than failing every command.
pub fn whisper_model_path(requested: &str) -> std::path::PathBuf {
    let dir = paths::models_dir();
    let want = dir.join(requested);
    if want.exists() {
        return want;
    }
    let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("ggml-") && n.ends_with(".bin"))
        })
        .collect();
    found.sort();
    match found.first() {
        Some(p) => {
            log::warn!("Whisper model {requested} is not in {} — using {}", dir.display(), p.display());
            p.clone()
        }
        None => want,
    }
}

/// Named mutex the agent holds while running, so only one copy listens.
pub const AGENT_MUTEX: &str = "Local\\NeedleVoiceAgentSingleton";
