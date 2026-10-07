//! Streaming recognition: partial text while an utterance is still being spoken.
//!
//! Streaming models are the ones built to emit words as they arrive, rather than
//! transcribing a finished sentence. The offline engines (Moonshine, SenseVoice,
//! Whisper) cannot do this at all, which is why the caption needed something
//! else. Several streaming packs are supported here so they can be measured:
//!
//! * NeMo FastConformer CTC, 80 ms — one model, low latency.
//! * Streaming zipformer (kroko and the 2023-06-26 English one) — encoder,
//!   decoder and joiner.
//!
//! See `--bench-live`, which prints the partial text every half second so the
//! caption's quality can be judged rather than assumed.

use nv_core::Config;
use sherpa_onnx::{
    OnlineNemoCtcModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineTransducerModelConfig,
};

/// Which streaming pack to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveModel {
    /// NeMo FastConformer CTC, English, 80 ms lookahead.
    NemoCtc80,
    /// Streaming zipformer, English, 2023-06-26.
    ZipformerEn,
    /// Streaming zipformer "kroko", English, 2025.
    Kroko,
    /// The small 20 M zipformer first tried for captions.
    Tiny20M,
}

impl LiveModel {
    pub fn dir(self) -> &'static str {
        match self {
            LiveModel::NemoCtc80 => "sherpa-onnx-nemo-streaming-fast-conformer-ctc-en-80ms-int8",
            LiveModel::ZipformerEn => "sherpa-onnx-streaming-zipformer-en-2023-06-26",
            LiveModel::Kroko => "sherpa-onnx-streaming-zipformer-en-kroko-2025-08-06",
            LiveModel::Tiny20M => nv_core::STREAMING_PACK,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LiveModel::NemoCtc80 => "nemo-ctc-80ms",
            LiveModel::ZipformerEn => "zipformer-en-2023",
            LiveModel::Kroko => "kroko",
            LiveModel::Tiny20M => "zipformer-20m",
        }
    }

    pub fn parse(s: &str) -> Option<LiveModel> {
        match s.to_lowercase().replace('_', "-").as_str() {
            "nemo" | "nemo-ctc" | "nemo-80ms" => Some(LiveModel::NemoCtc80),
            "zipformer" | "zipformer-en" | "en" => Some(LiveModel::ZipformerEn),
            "kroko" => Some(LiveModel::Kroko),
            "20m" | "tiny" | "zipformer-20m" => Some(LiveModel::Tiny20M),
            _ => None,
        }
    }
}

pub struct Live {
    recognizer: OnlineRecognizer,
    stream: sherpa_onnx::OnlineStream,
    shown: String,
    model: LiveModel,
}

impl Live {
    pub fn new(cfg: &Config) -> Result<Live, String> {
        Live::with(LiveModel::NemoCtc80, cfg.threads)
    }

    pub fn with(model: LiveModel, threads: u32) -> Result<Live, String> {
        let dir = nv_core::paths::models_dir().join("streaming").join(model.dir());
        let file = |name: &str| Some(dir.join(name).to_string_lossy().to_string());
        let mut config = OnlineRecognizerConfig::default();
        config.model_config.num_threads = threads as i32;
        config.model_config.provider = Some("cpu".to_string());
        config.decoding_method = Some("greedy_search".to_string());
        // The segmenter decides when an utterance ends; endpoint detection here
        // would only cut the caption short.
        config.enable_endpoint = false;
        match model {
            LiveModel::NemoCtc80 => {
                config.model_config.nemo_ctc = OnlineNemoCtcModelConfig { model: file("model.int8.onnx") };
                config.model_config.model_type = Some("nemo_ctc".to_string());
                // NeMo's tokens are BPE, so it needs to be told that and given the
                // vocabulary to detokenise with — without this it decodes nothing.
                config.model_config.modeling_unit = Some("bpe".to_string());
                config.model_config.bpe_vocab = file("tokens.txt");
            }
            LiveModel::ZipformerEn | LiveModel::Tiny20M => {
                let (e, d, j) = if model == LiveModel::ZipformerEn {
                    ("encoder-epoch-99-avg-1-chunk-16-left-128.onnx",
                     "decoder-epoch-99-avg-1-chunk-16-left-128.onnx",
                     "joiner-epoch-99-avg-1-chunk-16-left-128.onnx")
                } else {
                    ("encoder-epoch-99-avg-1.int8.onnx", "decoder-epoch-99-avg-1.int8.onnx", "joiner-epoch-99-avg-1.int8.onnx")
                };
                config.model_config.transducer = OnlineTransducerModelConfig {
                    encoder: file(e),
                    decoder: file(d),
                    joiner: file(j),
                };
                config.model_config.model_type = Some("zipformer".to_string());
            }
            LiveModel::Kroko => {
                config.model_config.transducer = OnlineTransducerModelConfig {
                    encoder: file("encoder.onnx"),
                    decoder: file("decoder.onnx"),
                    joiner: file("joiner.onnx"),
                };
                config.model_config.model_type = Some("zipformer".to_string());
            }
        }
        config.model_config.tokens = file("tokens.txt");
        let recognizer = OnlineRecognizer::create(&config)
            .ok_or_else(|| format!("could not load the streaming model from {}", dir.display()))?;
        let stream = recognizer.create_stream();
        Ok(Live { recognizer, stream, shown: String::new(), model })
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

    pub fn model(&self) -> LiveModel {
        self.model
    }

    /// Start a fresh utterance.
    pub fn reset(&mut self) {
        self.recognizer.reset(&self.stream);
        self.shown.clear();
    }
}
