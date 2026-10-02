//! Speech recognition, loaded on demand and dropped when idle.
//!
//! Two engines, because they are good at different things:
//!
//! * **Moonshine** (via sherpa-onnx, the crate the voices already use) is built
//!   for short utterances. It transcribes a command in a fraction of a second on
//!   a CPU, where Whisper's encoder needs seconds — Whisper always encodes a
//!   whole 30-second window whatever the clip length.
//! * **Whisper** is the fallback, and the one to pick when Moonshine's files are
//!   missing or when a different model was chosen in Settings.

use std::path::PathBuf;
use std::time::Instant;

use nv_core::config::SttEngine;
use nv_core::Config;
use sherpa_onnx::{OfflineMoonshineModelConfig, OfflineRecognizer, OfflineRecognizerConfig};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

pub struct Stt {
    engine: SttEngine,
    threads: i32,
    /// Whisper: the model file, and the prompt that primes its spelling.
    whisper_path: PathBuf,
    prompt: String,
    whisper: Option<(WhisperContext, WhisperState)>,
    /// Moonshine: sherpa's recogniser, which holds the whole model.
    moonshine: Option<OfflineRecognizer>,
    last_used: Instant,
}

impl Stt {
    /// Build a recogniser for this config. Moonshine is used when it was asked
    /// for and its files are on disk; otherwise this falls back to Whisper with
    /// a line in the log saying why.
    pub fn new(cfg: &Config) -> Self {
        let mut engine = cfg.stt_engine;
        let whisper_path = nv_core::whisper_model_path(&cfg.whisper_model);
        if engine == SttEngine::Moonshine && !nv_core::moonshine_installed() {
            log::warn!(
                "Moonshine files are not in {} — using Whisper until they are downloaded",
                nv_core::moonshine_dir().display()
            );
            engine = SttEngine::Whisper;
        }
        // And the other way round: a chosen Whisper model that was never
        // downloaded shouldn't leave the assistant unable to hear anything.
        if engine == SttEngine::Whisper && !whisper_path.exists() && nv_core::moonshine_installed() {
            log::warn!("Whisper model {} is missing — using Moonshine", whisper_path.display());
            engine = SttEngine::Moonshine;
        }
        // Priming Whisper with the name and the words this app actually acts on
        // makes it spell them consistently — "disengage" used to come back as
        // "this engage". The prompt deliberately never contains "hey <name>": on
        // silence Whisper may echo its prompt, and that must not wake us.
        let prompt = format!(
            "{}. Commands: open Chrome, close Spotify, play music, pause, next track, \
             volume up, take a screenshot, set an alarm, remind me, never mind, disengage, \
             cancel that, what time is it, search the web.",
            cfg.agent_name
        );
        Stt {
            engine,
            threads: cfg.threads as i32,
            whisper_path,
            prompt,
            whisper: None,
            moonshine: None,
            last_used: Instant::now(),
        }
    }

    /// A Whisper recogniser for a specific model file, whatever the config says.
    /// The benchmark command and the pipeline tests need this.
    pub fn whisper(path: PathBuf, threads: u32, agent_name: &str) -> Self {
        let mut stt = Stt {
            engine: SttEngine::Whisper,
            threads: threads as i32,
            whisper_path: path,
            prompt: String::new(),
            whisper: None,
            moonshine: None,
            last_used: Instant::now(),
        };
        stt.prompt = format!(
            "{agent_name}. Commands: open Chrome, close Spotify, play music, pause, next track, \
             volume up, take a screenshot, set an alarm, remind me, never mind, disengage, \
             cancel that, what time is it, search the web."
        );
        stt
    }

    /// A Moonshine recogniser, whatever the config says.
    pub fn moonshine(threads: u32) -> Self {
        Stt {
            engine: SttEngine::Moonshine,
            threads: threads as i32,
            whisper_path: PathBuf::new(),
            prompt: String::new(),
            whisper: None,
            moonshine: None,
            last_used: Instant::now(),
        }
    }

    /// Which engine this instance actually ended up with.
    pub fn engine(&self) -> SttEngine {
        self.engine
    }

    /// Load whatever is needed, if it is not loaded already.
    fn ensure(&mut self) -> Result<(), String> {
        match self.engine {
            SttEngine::Moonshine => {
                if self.moonshine.is_none() {
                    let t = Instant::now();
                    let dir = nv_core::moonshine_dir();
                    let file = |name: &str| Some(dir.join(name).to_string_lossy().to_string());
                    let mut config = OfflineRecognizerConfig::default();
                    config.model_config.moonshine = OfflineMoonshineModelConfig {
                        preprocessor: file("preprocess.onnx"),
                        encoder: file("encode.int8.onnx"),
                        uncached_decoder: file("uncached_decode.int8.onnx"),
                        cached_decoder: file("cached_decode.int8.onnx"),
                        merged_decoder: None,
                    };
                    config.model_config.tokens = file("tokens.txt");
                    config.model_config.num_threads = self.threads;
                    config.model_config.provider = Some("cpu".to_string());
                    config.decoding_method = Some("greedy_search".to_string());
                    let recognizer = OfflineRecognizer::create(&config)
                        .ok_or_else(|| format!("could not load Moonshine from {}", dir.display()))?;
                    log::info!("Moonshine loaded in {:?}", t.elapsed());
                    self.moonshine = Some(recognizer);
                }
            }
            SttEngine::Whisper => {
                if self.whisper.is_none() {
                    let t = Instant::now();
                    // `--features cuda` (or vulkan) builds the GPU backend in;
                    // without it this is a CPU build and asking for the GPU would
                    // just fail.
                    let mut params = WhisperContextParameters::default();
                    params.use_gpu(cfg!(feature = "gpu"));
                    // Measured on a Ryzen 5 2600: flash attention is 1.5× *slower*
                    // on this CPU (0.97 s vs 0.65 s per clip with base.en).
                    params.flash_attn(false);
                    let ctx = WhisperContext::new_with_params(&self.whisper_path, params)
                        .map_err(|e| format!("could not load Whisper model {}: {e}", self.whisper_path.display()))?;
                    let state = ctx.create_state().map_err(|e| e.to_string())?;
                    log::info!("Whisper loaded in {:?}", t.elapsed());
                    self.whisper = Some((ctx, state));
                }
            }
        }
        Ok(())
    }

    pub fn unload_if_idle(&mut self, idle: std::time::Duration) {
        if self.last_used.elapsed() < idle {
            return;
        }
        match self.engine {
            SttEngine::Whisper if self.whisper.is_some() => {
                self.whisper = None;
                log::info!("Whisper unloaded (idle)");
            }
            SttEngine::Moonshine if self.moonshine.is_some() => {
                self.moonshine = None;
                log::info!("Moonshine unloaded (idle)");
            }
            _ => {}
        }
    }

    /// Transcribe 16 kHz mono audio.
    pub fn transcribe(&mut self, audio: &[f32]) -> Result<String, String> {
        self.last_used = Instant::now();
        self.ensure()?;
        let text = match self.engine {
            SttEngine::Moonshine => self.run_moonshine(audio)?,
            SttEngine::Whisper => self.run_whisper(audio)?,
        };
        self.last_used = Instant::now();
        Ok(clean(&text))
    }

    /// Moonshine takes the whole clip at once, which is exactly what a command is.
    fn run_moonshine(&mut self, audio: &[f32]) -> Result<String, String> {
        let recognizer = self.moonshine.as_ref().ok_or("Moonshine is not loaded")?;
        let stream = recognizer.create_stream();
        // It needs a little more than half a second of audio to say anything.
        let mut data = audio.to_vec();
        if data.len() < crate::audio::RATE as usize / 2 {
            data.resize(crate::audio::RATE as usize / 2, 0.0);
        }
        stream.accept_waveform(crate::audio::RATE as i32, &data);
        recognizer.decode(&stream);
        let result = stream.get_result().ok_or("Moonshine returned nothing")?;
        Ok(result.text)
    }

    fn run_whisper(&mut self, audio: &[f32]) -> Result<String, String> {
        let threads = self.threads;
        let prompt = self.prompt.clone();
        let state = &mut self.whisper.as_mut().ok_or("Whisper is not loaded")?.1;

        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        p.set_language(Some("en"));
        p.set_n_threads(threads);
        p.set_no_context(true);
        p.set_single_segment(true);
        p.set_no_timestamps(true);
        p.set_suppress_blank(true);
        p.set_suppress_nst(true);
        p.set_temperature(0.0);
        // No temperature fallback: retries make bad clips take 10× longer.
        p.set_temperature_inc(0.0);
        p.set_max_tokens(48);
        p.set_print_special(false);
        p.set_print_progress(false);
        p.set_print_realtime(false);
        p.set_print_timestamps(false);
        p.set_initial_prompt(&prompt);
        // Whisper always encodes 30 s windows; shrinking the audio context to
        // the clip length (plus headroom) makes short clips several times faster.
        let secs = audio.len() as f32 / crate::audio::RATE as f32;
        let ctx = ((secs / 30.0 * 1500.0) as i32 + 128).clamp(256, 1500);
        p.set_audio_ctx(ctx);

        // Level the clip (quiet mics) and pad to the ~1 s Whisper needs.
        let mut data = normalize(audio);
        if data.len() < crate::audio::RATE as usize + 1600 {
            data.resize(crate::audio::RATE as usize + 1600, 0.0);
        }
        state.full(p, &data).map_err(|e| e.to_string())?;
        let mut text = String::new();
        for seg in state.as_iter() {
            if let Ok(s) = seg.to_str_lossy() {
                text.push_str(&s);
            }
        }
        Ok(text)
    }
}

/// Drop Whisper's non-speech annotations like "[BLANK_AUDIO]" or "(music)".
fn clean(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    dedupe_sentences(&out)
}

/// With a shortened audio context Whisper sometimes lo

/// With a shortened audio context Whisper sometimes loops ("Open Chrome.
/// Open Chrome. Open Chrome."). Keep sentences until one repeats.
fn dedupe_sentences(s: &str) -> String {
    let mut seen: Vec<String> = Vec::new();
    let mut out = String::new();
    let mut start = 0;
    for (i, c) in s.char_indices() {
        let end = i + c.len_utf8();
        if matches!(c, '.' | '?' | '!') || end == s.len() {
            let sentence = &s[start..end];
            let key = nv_core::fuzzy::normalize(sentence);
            if !key.is_empty() {
                if seen.iter().any(|k| k == &key || (end == s.len() && k.starts_with(&key))) {
                    break;
                }
                seen.push(key);
            }
            out.push_str(sentence);
            start = end;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn dedupes_loops() {
        assert_eq!(super::clean("Hey Nova, open Chrome. Hey Nova, open Chrome. Hey"), "Hey Nova, open Chrome.");
        assert_eq!(super::clean("[BLANK_AUDIO]"), "");
        assert_eq!(super::clean("What is it? Tell me."), "What is it? Tell me.");
    }
}

/// Boost quiet speech so its peak sits around -6 dBFS (at most +26 dB, so
/// pure hiss isn't blown up into "words").
pub fn normalize(audio: &[f32]) -> Vec<f32> {
    let peak = audio.iter().fold(0f32, |m, s| m.max(s.abs()));
    let gain = if peak > 1e-6 { (0.5 / peak).clamp(1.0, 20.0) } else { 1.0 };
    audio.iter().map(|s| (s * gain).clamp(-1.0, 1.0)).collect()
}
