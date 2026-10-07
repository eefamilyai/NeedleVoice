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
/// SenseVoice: one model file, non-autoregressive, fast.
pub const SENSEVOICE_PACK: &str = "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09";
pub const SENSEVOICE_NANO_PACK: &str = "sherpa-onnx-sense-voice-funasr-nano-int8-2025-12-17";
/// Dolphin: a small multilingual CTC model.
pub const DOLPHIN_PACK: &str = "sherpa-onnx-dolphin-base-ctc-multi-lang-int8-2025-04-02";
/// The streaming model behind the live transcript: 20 M parameters, English.
pub const STREAMING_PACK: &str = "sherpa-onnx-nemo-streaming-fast-conformer-ctc-en-80ms-int8";
/// Moonshine v2, which supersedes v1 when it is present.
pub const MOONSHINE_V2_PACK: &str = "sherpa-onnx-moonshine-base-en-quantized-2026-02-27";
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

/// Where the v2 Moonshine pack lives.
pub fn moonshine_v2_dir() -> std::path::PathBuf {
    paths::models_dir().join("moonshine").join(MOONSHINE_V2_PACK)
}

/// Is Moonshine v2 on disk? It needs an encoder and a merged decoder.
pub fn moonshine_v2_installed() -> bool {
    let d = moonshine_v2_dir();
    d.join("encoder_model.ort").exists() && d.join("decoder_model_merged.ort").exists() && d.join("tokens.txt").exists()
}

/// Where the streaming model lives.
pub fn streaming_dir() -> std::path::PathBuf {
    paths::models_dir().join("streaming").join(STREAMING_PACK)
}

/// Is the streaming model on disk? The one that is installed is a NeMo
/// FastConformer, so it is a single model file — this used to look for the
/// zipformer's three, which meant the caption window was never created.
pub fn streaming_installed() -> bool {
    let d = streaming_dir();
    d.join("model.int8.onnx").exists() && d.join("tokens.txt").exists()
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
