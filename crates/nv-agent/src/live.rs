//! Streaming recognition, kept for when a better streaming model is available.
//!
//! The caption does not use this: the 20 M streaming zipformer's partial text was
//! poor enough to mislead ("OR" for "reminder to call my mum"), so the caption is
//! built by re-running the accurate engine on the audio so far instead. This
//! wrapper works and is wired for `--bench-stt`-style experiments.
#![allow(dead_code)]
//!
//! The command engines (Moonshine, SenseVoice, Whisper) are offline: they need
//! the whole utterance before they can produce a word, which is no use for a
//! caption. This is a small streaming zipformer — 20 M parameters — fed the same
//! audio as it arrives, so text appears as you speak.

use nv_core::Config;
use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, OnlineTransducerModelConfig};

pub struct Live {
    recognizer: OnlineRecognizer,
    stream: sherpa_onnx::OnlineStream,
    /// What was last shown, so the caller only updates on a change.
    shown: String,
}

impl Live {
    pub fn new(cfg: &Config) -> Result<Live, String> {
        let dir = nv_core::streaming_dir();
        let file = |name: &str| Some(dir.join(name).to_string_lossy().to_string());
        let mut config = OnlineRecognizerConfig::default();
        config.model_config.transducer = OnlineTransducerModelConfig {
            encoder: file("encoder-epoch-99-avg-1.int8.onnx"),
            decoder: file("decoder-epoch-99-avg-1.int8.onnx"),
            joiner: file("joiner-epoch-99-avg-1.int8.onnx"),
        };
        config.model_config.tokens = file("tokens.txt");
        config.model_config.num_threads = cfg.threads as i32;
        config.model_config.provider = Some("cpu".to_string());
        // Greedy is plenty for a caption, and it is the cheapest.
        config.decoding_method = Some("greedy_search".to_string());
        // The segmenter already decides when an utterance ends; endpoint
        // detection here would only cut the caption short.
        config.enable_endpoint = false;
        let recognizer = OnlineRecognizer::create(&config).ok_or_else(|| {
            format!("could not load the streaming model from {}", dir.display())
        })?;
        let stream = recognizer.create_stream();
        Ok(Live { recognizer, stream, shown: String::new() })
    }

    /// Feed audio; returns the caption when it has changed.
    pub fn feed(&mut self, samples: &[f32]) -> Option<String> {
        self.stream.accept_waveform(crate::audio::RATE as i32, samples);
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
        let text = self
            .recognizer
            .get_result(&self.stream)
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default();
        if text != self.shown {
            self.shown = text.clone();
            Some(text)
        } else {
            None
        }
    }

    /// Start a fresh utterance.
    pub fn reset(&mut self) {
        self.recognizer.reset(&self.stream);
        self.shown.clear();
    }
}
