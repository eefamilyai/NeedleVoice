//! Whisper speech-to-text, loaded on demand and dropped when idle.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

pub struct Stt {
    path: PathBuf,
    threads: i32,
    prompt: String,
    loaded: Option<(WhisperContext, WhisperState)>,
    last_used: Instant,
}

impl Stt {
    pub fn new(path: PathBuf, threads: u32, agent_name: &str) -> Self {
        // Priming Whisper with the name and some vocabulary makes it spell them
        // consistently. The prompt deliberately never contains "hey <name>":
        // on silence Whisper may echo its prompt, which must not wake us.
        let prompt = format!("{agent_name}. Open Chrome. Search for the weather. Close Spotify.");
        Self { path, threads: threads as i32, prompt, loaded: None, last_used: Instant::now() }
    }

    fn ensure(&mut self) -> Result<&mut WhisperState, String> {
        if self.loaded.is_none() {
            let t = Instant::now();
            let mut params = WhisperContextParameters::default();
            params.use_gpu(false);
            let ctx = WhisperContext::new_with_params(&self.path, params)
                .map_err(|e| format!("could not load Whisper model {}: {e}", self.path.display()))?;
            let state = ctx.create_state().map_err(|e| e.to_string())?;
            log::info!("Whisper loaded in {:?}", t.elapsed());
            self.loaded = Some((ctx, state));
        }
        Ok(&mut self.loaded.as_mut().unwrap().1)
    }

    pub fn unload_if_idle(&mut self, idle: Duration) {
        if self.loaded.is_some() && self.last_used.elapsed() >= idle {
            self.loaded = None;
            log::info!("Whisper unloaded (idle)");
        }
    }

    /// Transcribe 16 kHz mono audio.
    pub fn transcribe(&mut self, audio: &[f32]) -> Result<String, String> {
        self.last_used = Instant::now();
        let threads = self.threads;
        let prompt = self.prompt.clone();
        let state = self.ensure()?;

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
        let data = &data[..];
        state.full(p, data).map_err(|e| e.to_string())?;
        let mut text = String::new();
        for seg in state.as_iter() {
            if let Ok(s) = seg.to_str_lossy() {
                text.push_str(&s);
            }
        }
        self.last_used = Instant::now();
        Ok(clean(&text))
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
