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
use nv_core::schedule::Stamp;
use nv_core::Config;
use sherpa_onnx::{
    OfflineDolphinModelConfig, OfflineMoonshineModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineSenseVoiceModelConfig,
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

/// Which recogniser to run. The config offers three user-facing engines; the
/// rest are here so they can be measured against each other before one is
/// chosen — see `--bench-stt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// whisper.cpp: multilingual, accurate, slow on a CPU.
    Whisper,
    /// sherpa-onnx Moonshine v1 (four ONNX models).
    Moonshine,
    /// Moonshine v2: an encoder and a merged decoder, in `.ort` form.
    Moonshine2,
    /// SenseVoice: non-autoregressive, so it does not decode token by token.
    SenseVoice,
    SenseVoiceNano,
    /// Dolphin: a small multilingual CTC model.
    Dolphin,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Whisper => "whisper",
            Backend::Moonshine => "moonshine",
            Backend::Moonshine2 => "moonshine-v2",
            Backend::SenseVoice => "sensevoice",
            Backend::SenseVoiceNano => "sensevoice-nano",
            Backend::Dolphin => "dolphin",
        }
    }

    /// Parse a name, for `--bench-stt`.
    pub fn parse(s: &str) -> Option<Backend> {
        match s.to_lowercase().replace('_', "-").as_str() {
            "whisper" => Some(Backend::Whisper),
            "moonshine" | "moonshine-v1" => Some(Backend::Moonshine),
            "moonshine-v2" | "moonshine2" => Some(Backend::Moonshine2),
            "sensevoice" | "sense-voice" => Some(Backend::SenseVoice),
            "sensevoice-nano" | "sense-voice-nano" | "nano" => Some(Backend::SenseVoiceNano),
            "dolphin" => Some(Backend::Dolphin),
            _ => None,
        }
    }
}

/// Whisper is primed with the words this app acts on, so it spells them the way
/// we expect — "disengage" used to come back as "this engage". Deliberately never
/// contains "hey <name>": on silence Whisper may echo its prompt, and that must
/// not wake us.
fn command_prompt(agent_name: &str) -> String {
    format!(
        "{agent_name}. Commands: open Chrome, close Spotify, play music, pause, next track, \
         volume up, take a screenshot, set an alarm, remind me, never mind, disengage, \
         cancel that, what time is it, search the web."
    )
}

pub struct Stt {
    backend: Backend,
    threads: i32,
    /// Whisper: the model file, and the prompt that primes its spelling.
    whisper_path: PathBuf,
    prompt: String,
    whisper: Option<(WhisperContext, WhisperState)>,
    /// Every sherpa-onnx engine loads into the same recogniser type.
    sherpa: Option<OfflineRecognizer>,
    /// Save each clip for later inspection.
    save_clips: bool,
    /// Set when the model would not load, so it is not retried for every caption
    /// pass (which is how one failure became seven error lines a second).
    failed: bool,
    last_used: Instant,
}

impl Stt {
    /// Build a recogniser for this config, honouring the engine choice and
    /// falling back to whatever will actually run.
    pub fn new(cfg: &Config) -> Self {
        let whisper_path = nv_core::whisper_model_path(&cfg.whisper_model);
        let mut backend = match cfg.stt_engine {
            SttEngine::Moonshine => {
                if !nv_core::moonshine_installed() {
                    log::warn!(
                        "Moonshine files are not in {} — using Whisper until they are downloaded",
                        nv_core::moonshine_dir().display()
                    );
                    Backend::Whisper
                } else if nv_core::moonshine_v2_installed() {
                    Backend::Moonshine2
                } else {
                    Backend::Moonshine
                }
            }
            SttEngine::Whisper => Backend::Whisper,
            SttEngine::SenseVoice => Backend::SenseVoice,
        };
        // A chosen engine whose files were never downloaded must not leave the
        // assistant unable to hear anything — which is what "I changed the model
        // and now it just disengages" was: SenseVoice was selected, its model was
        // not installed, and every command came back empty.
        if !Stt::installed(backend) {
            let fallback = [Backend::Moonshine2, Backend::Moonshine, Backend::Whisper]
                .into_iter()
                .find(|b| Stt::installed(*b));
            match fallback {
                Some(b) => {
                    log::warn!(
                        "the {} model is not installed — using {} until it is",
                        backend.name(),
                        b.name()
                    );
                    backend = b;
                }
                None => log::error!("no speech model is installed: download one in Settings > Listening"),
            }
        }
        Stt {
            backend,
            threads: cfg.threads as i32,
            whisper_path,
            prompt: command_prompt(&cfg.agent_name),
            whisper: None,
            sherpa: None,
            save_clips: cfg.save_clips,
            failed: false,
            last_used: Instant::now(),
        }
    }

    /// A specific engine, for the benchmark and the pipeline tests.
    pub fn with(backend: Backend, threads: u32, agent_name: &str, whisper_model: &str) -> Self {
        Stt {
            backend,
            threads: threads as i32,
            whisper_path: nv_core::whisper_model_path(whisper_model),
            prompt: command_prompt(agent_name),
            whisper: None,
            sherpa: None,
            save_clips: false,
            failed: false,
            last_used: Instant::now(),
        }
    }

    /// A Whisper recogniser for a specific model file, whatever the config says.
    /// The benchmark command and the pipeline tests need this.
    pub fn whisper(path: PathBuf, threads: u32, agent_name: &str) -> Self {
        let mut stt = Stt::with(Backend::Whisper, threads, agent_name, "");
        stt.whisper_path = path;
        stt
    }

    /// Are this engine's model files on disk?
    pub fn installed(backend: Backend) -> bool {
        let models = nv_core::paths::models_dir();
        match backend {
            Backend::Whisper => nv_core::whisper_model_path("").exists() || models.join("ggml-tiny.en-q5_1.bin").exists(),
            Backend::Moonshine => nv_core::moonshine_pack_dir(nv_core::MOONSHINE_PACKS[0]).exists(),
            Backend::Moonshine2 => nv_core::moonshine_v2_installed(),
            Backend::SenseVoice => models.join("sensevoice").join(nv_core::SENSEVOICE_PACK).join("model.int8.onnx").exists(),
            Backend::SenseVoiceNano => {
                models.join("sensevoice").join(nv_core::SENSEVOICE_NANO_PACK).join("model.int8.onnx").exists()
            }
            Backend::Dolphin => models.join("dolphin").join(nv_core::DOLPHIN_PACK).join("model.int8.onnx").exists(),
        }
    }

    /// Which engine this instance actually ended up with.
    pub fn engine(&self) -> &'static str {
        self.backend.name()
    }

    /// Is the model in memory already?
    pub fn loaded(&self) -> bool {
        match self.backend {
            Backend::Whisper => self.whisper.is_some(),
            _ => self.sherpa.is_some(),
        }
    }

    /// Load the model now. Called when the wake word is heard, so the load
    /// overlaps the sentence being spoken rather than delaying the answer.
    pub fn prepare(&mut self) {
        if let Err(e) = self.ensure() {
            log::error!("{e}");
        }
    }

    /// Load whatever is needed, if it is not loaded already.
    fn ensure(&mut self) -> Result<(), String> {
        if self.backend == Backend::Whisper {
            if self.whisper.is_none() {
                let t = Instant::now();
                // `--features cuda` (or vulkan) builds the GPU backend in;
                // without it this is a CPU build and asking for the GPU would
                // just fail.
                let mut params = WhisperContextParameters::default();
                params.use_gpu(cfg!(feature = "gpu"));
                // Measured on a Ryzen 5 2600: flash attention is 1.5× *slower* on
                // this CPU (0.97 s vs 0.65 s per clip with base.en).
                params.flash_attn(false);
                let ctx = WhisperContext::new_with_params(&self.whisper_path, params)
                    .map_err(|e| format!("could not load Whisper model {}: {e}", self.whisper_path.display()))?;
                let state = ctx.create_state().map_err(|e| e.to_string())?;
                log::info!("Whisper loaded in {:?}", t.elapsed());
                self.whisper = Some((ctx, state));
            }
            return Ok(());
        }
        if self.sherpa.is_none() {
            if self.failed {
                return Err(format!("the {} model would not load", self.backend.name()));
            }
            let t = Instant::now();
            let config = self.sherpa_config()?;
            let recognizer = match OfflineRecognizer::create(&config) {
                Some(r) => r,
                None => {
                    self.failed = true;
                    return Err(format!("could not load the {} model", self.backend.name()));
                }
            };
            log::info!("{} loaded in {:?}", self.backend.name(), t.elapsed());
            self.sherpa = Some(recognizer);
        }
        Ok(())
    }

    /// The model paths for whichever sherpa-onnx engine is selected.
    fn sherpa_config(&self) -> Result<OfflineRecognizerConfig, String> {
        let mut config = OfflineRecognizerConfig::default();
        config.model_config.num_threads = self.threads;
        config.model_config.provider = Some("cpu".to_string());
        config.decoding_method = Some("greedy_search".to_string());
        let models = nv_core::paths::models_dir();
        let path = |p: std::path::PathBuf| Some(p.to_string_lossy().to_string());
        match self.backend {
            Backend::Moonshine => {
                let dir = models.join("moonshine").join(nv_core::MOONSHINE_PACKS[0]);
                config.model_config.moonshine = OfflineMoonshineModelConfig {
                    preprocessor: path(dir.join("preprocess.onnx")),
                    encoder: path(dir.join("encode.int8.onnx")),
                    uncached_decoder: path(dir.join("uncached_decode.int8.onnx")),
                    cached_decoder: path(dir.join("cached_decode.int8.onnx")),
                    merged_decoder: None,
                };
                config.model_config.tokens = path(dir.join("tokens.txt"));
            }
            Backend::Moonshine2 => {
                // v2: an encoder and a merged decoder, in ONNX Runtime format.
                let dir = nv_core::moonshine_v2_dir();
                config.model_config.moonshine = OfflineMoonshineModelConfig {
                    preprocessor: None,
                    encoder: path(dir.join("encoder_model.ort")),
                    uncached_decoder: None,
                    cached_decoder: None,
                    merged_decoder: path(dir.join("decoder_model_merged.ort")),
                };
                config.model_config.tokens = path(dir.join("tokens.txt"));
            }
            Backend::SenseVoice | Backend::SenseVoiceNano => {
                let pack = if self.backend == Backend::SenseVoice {
                    nv_core::SENSEVOICE_PACK
                } else {
                    nv_core::SENSEVOICE_NANO_PACK
                };
                let dir = models.join("sensevoice").join(pack);
                config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
                    model: path(dir.join("model.int8.onnx")),
                    language: Some("en".to_string()),
                    use_itn: true,
                };
                config.model_config.tokens = path(dir.join("tokens.txt"));
            }
            Backend::Dolphin => {
                let dir = models.join("dolphin").join(nv_core::DOLPHIN_PACK);
                config.model_config.dolphin = OfflineDolphinModelConfig {
                    model: path(dir.join("model.int8.onnx")),
                };
                config.model_config.tokens = path(dir.join("tokens.txt"));
            }
            Backend::Whisper => return Err("Whisper does not use the sherpa path".into()),
        }
        Ok(config)
    }

    pub fn unload_if_idle(&mut self, idle: std::time::Duration) {
        if self.last_used.elapsed() < idle {
            return;
        }
        if self.backend == Backend::Whisper {
            if self.whisper.take().is_some() {
                log::info!("Whisper unloaded (idle)");
            }
        } else if self.sherpa.take().is_some() {
            log::info!("{} unloaded (idle)", self.backend.name());
        }
    }

    /// Transcribe 16 kHz mono audio.
    pub fn transcribe(&mut self, audio: &[f32]) -> Result<String, String> {
        self.last_used = Instant::now();
        if self.failed {
            return Err("no model loaded".into());
        }
        // Loading is worth calling out separately: it is the difference between
        // "instant" and "why is this slow", and it is invisible from the outside.
        let load_started = Instant::now();
        let cold = !self.loaded();
        self.ensure()?;
        let load = load_started.elapsed();
        let infer_started = Instant::now();
        let text = match self.backend {
            Backend::Whisper => self.run_whisper(audio)?,
            _ => self.run_sherpa(audio)?,
        };
        let infer = infer_started.elapsed();
        self.dump_if_wanted(audio);
        if cold {
            log::info!(
                "{}: {:.0} ms infer + {:.0} ms load (cold)",
                self.backend.name(),
                infer.as_secs_f32() * 1000.0,
                load.as_secs_f32() * 1000.0
            );
        } else {
            log::info!("{}: {:.0} ms infer", self.backend.name(), infer.as_secs_f32() * 1000.0);
        }
        self.last_used = Instant::now();
        Ok(clean(&text))
    }

    /// `NV_DUMP_AUDIO=<dir>`, or the "Save what it hears" setting, writes the
    /// clip the recogniser was given. Guessing at a mis-heard word is hopeless;
    /// having the clip is not.
    fn dump_if_wanted(&self, audio: &[f32]) {
        let dir = std::env::var("NV_DUMP_AUDIO")
            .ok()
            .map(std::path::PathBuf::from)
            .or_else(|| self.save_clips.then(|| nv_core::paths::data_dir().join("clips")));
        let Some(dir) = dir else { return };
        let peak = audio.iter().fold(0f32, |m, s| m.max(s.abs()));
        let rms = (audio.iter().map(|s| s * s).sum::<f32>() / audio.len().max(1) as f32).sqrt();
        let stamp = Stamp::now().clock().replace(':', "");
        let ms = audio.len() as f32 / crate::audio::RATE as f32 * 1000.0;
        let path = dir.join(format!("cmd-{stamp}-{ms:.0}ms.wav"));
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = write_wav(&path, audio);
            log::info!(
                "audio dumped to {} (peak {:.0} dBFS, rms {:.0} dBFS)",
                path.display(),
                20.0 * peak.max(1e-9).log10(),
                20.0 * rms.max(1e-9).log10()
            );
        }
    }

    /// Any sherpa-onnx engine: hand it the clip, take the text.
    fn run_sherpa(&mut self, audio: &[f32]) -> Result<String, String> {
        let recognizer = self.sherpa.as_ref().ok_or("the model is not loaded")?;
        let stream = recognizer.create_stream();
        // Levelling and both pads in one allocation, the same treatment Whisper
        // gets: a soft first syllable on a quiet microphone is the one most
        // likely to go missing.
        let data = normalize_padded(audio, crate::audio::RATE as usize / 16, crate::audio::RATE as usize / 2);
        stream.accept_waveform(crate::audio::RATE as i32, &data);
        recognizer.decode(&stream);
        let result = stream.get_result().ok_or("the model returned nothing")?;
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
        let data = normalize_padded(audio, 0, crate::audio::RATE as usize + 1600);
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

/// Write 16 kHz mono f32 samples as a 16-bit WAV. Used by the dump above.
fn write_wav(path: &std::path::Path, samples: &[f32]) -> std::io::Result<()> {
    write_wav_at(path, samples, crate::audio::RATE)
}

/// Write mono f32 samples as a 16-bit WAV at any rate.
pub fn write_wav_at(path: &std::path::Path, samples: &[f32], rate: u32) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let data_len = (samples.len() * 2) as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    // One conversion and one write: a per-sample `write_all` was tens of
    // thousands of tiny calls for a dumped command clip.
    let pcm: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
        .collect();
    f.write_all(&pcm)?;
    f.flush()
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
    // SenseVoice returns capitals and, now and then, a character from another
    // script in the middle of a word; a command is English words either way.
    let ascii: String = out
        .chars()
        .map(|c| if c.is_ascii() || c.is_whitespace() { c } else { ' ' })
        .collect();
    let sentence = ascii.split_whitespace().collect::<Vec<_>>().join(" ");
    dedupe_words(&dedupe_sentences(&sentence)).to_lowercase()
}

/// Collapse words repeated back to back. Whisper does this on a partial clip —
/// the live caption showed "Reminder reminder reminder reminder rem" before the
/// sentence had been said — and the same guard is harmless on a finished one.
fn dedupe_words(s: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for word in s.split_whitespace() {
        let repeat = out.last().is_some_and(|last| {
            last.eq_ignore_ascii_case(word.trim_matches(|c: char| !c.is_alphanumeric()))
        });
        if !repeat {
            out.push(word);
        }
    }
    out.join(" ")
}

/// With a shortened audio context Whisper sometimes loops ("Open Chrome.
/// Open Chrome. Open Chrome.").
///
/// A sentence that repeats the one before it is a stutter and is dropped; a
/// sentence that repeats an *earlier* one is not, and neither is a trailing
/// fragment that merely starts with an earlier sentence's words — cutting the
/// output off at the first repeat is how a real command ("Open Chrome. Open
/// Chrome. And search for trains.") lost everything after the stutter.
fn dedupe_sentences(s: &str) -> String {
    let mut out = String::new();
    let mut previous: Option<String> = None;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        let end = i + c.len_utf8();
        if !matches!(c, '.' | '?' | '!') && end != s.len() {
            continue;
        }
        let sentence = &s[start..end];
        start = end;
        let key = nv_core::fuzzy::normalize(sentence);
        if key.is_empty() {
            out.push_str(sentence);
            continue;
        }
        if previous.as_deref() == Some(key.as_str()) {
            continue; // a stutter, not a second request
        }
        // A trailing fragment can come back as the beginning of the sentence
        // that preceded it; that is the same stutter, heard mid-loop.
        if end == s.len() && previous.as_deref().is_some_and(|p| p.starts_with(&key)) {
            continue;
        }
        previous = Some(key);
        out.push_str(sentence);
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn dedupes_loops() {
        assert_eq!(super::clean("Hey Nova, open Chrome. Hey Nova, open Chrome. Hey"), "hey nova, open chrome.");
        assert_eq!(super::clean("[BLANK_AUDIO]"), "");
        assert_eq!(super::clean("What is it? Tell me."), "what is it? tell me.");
        assert_eq!(super::clean("(music) open chrome"), "open chrome");
    }

    /// A stutter in the middle of a sentence must not throw away the rest of it.
    #[test]
    fn a_looping_transcript_keeps_what_follows() {
        assert_eq!(
            super::clean("Open Chrome. Open Chrome. And search for trains."),
            "open chrome. and search for trains."
        );
        // A genuine second sentence with the same opening is not a stutter.
        assert_eq!(
            super::clean("Open Chrome. Open Spotify."),
            "open chrome. open spotify."
        );
        // Repeating something from earlier is a repeat, not a loop to be cut.
        assert_eq!(super::clean("Set a timer. Get coffee. Set a timer."), "set a timer. get coffee. set a timer.");
    }

    #[test]
    fn padding_and_levelling_happen_together() {
        let quiet = super::normalize_padded(&[0.05, -0.05], 100, 50);
        assert_eq!(quiet.len(), 102);
        assert!(quiet[..100].iter().all(|s| *s == 0.0));
        // Levelled to the 0.5 peak target, not beyond it.
        assert!((quiet[100] - 0.5).abs() < 1e-6, "{}", quiet[100]);
        assert!((quiet[101] + 0.5).abs() < 1e-6, "{}", quiet[101]);
        let short = super::normalize_padded(&[0.5], 0, 400);
        assert_eq!(short.len(), 400);
        assert!(short[1..].iter().all(|s| *s == 0.0));
        // Silence stays silent: there is nothing to level up.
        assert!(super::normalize_padded(&[0.0; 8], 0, 0).iter().all(|s| *s == 0.0));
    }
}

/// Boost quiet speech so its peak sits around -6 dBFS (at most +26 dB, so
/// pure hiss isn't blown up into "words"), with `lead` silent samples in front
/// and the result padded out to at least `least` samples — all in one
/// allocation, since every recogniser wants both.
pub fn normalize_padded(audio: &[f32], lead: usize, least: usize) -> Vec<f32> {
    let peak = audio.iter().fold(0f32, |m, s| m.max(s.abs()));
    let gain = if peak > 1e-6 { (0.5 / peak).clamp(1.0, 20.0) } else { 1.0 };
    let mut out = Vec::with_capacity((lead + audio.len()).max(least));
    out.resize(lead, 0.0);
    out.extend(audio.iter().map(|s| (s * gain).clamp(-1.0, 1.0)));
    out.resize(out.len().max(least), 0.0);
    out
}

#[cfg(test)]
mod caption_tests {
    use super::*;

    #[test]
    fn a_repeated_word_is_said_once() {
        // Whisper's partial-clip loop, seen in the live caption.
        assert_eq!(clean("Reminder reminder reminder reminder rem"), "reminder rem");
        assert_eq!(clean("reminder reminder"), "reminder");
        // Real repetition is not a loop and is left alone once.
        assert_eq!(clean("very very good"), "very good");
        assert_eq!(clean("no no no"), "no");
    }

    #[test]
    fn sense_voice_shouting_and_stray_scripts_are_tidied() {
        // SenseVoice returns capitals, and occasionally a character from another
        // script in the middle of a word.
        assert_eq!(clean("HEY REGGIE OPEN CHROME"), "hey reggie open chrome");
        let mangled = format!("SEVENHRT{}Y IN THE MORNING", char::from_u32(0x5341).unwrap());
        assert!(clean(&mangled).chars().all(|c| c.is_ascii()), "{:?}", clean(&mangled));
    }
}
