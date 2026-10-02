//! Streaming wake-word spotting with a tiny (3.3 M parameter) zipformer
//! keyword model. Fires the instant "hey <name>" is heard — no waiting for the
//! end of the sentence, no Whisper involved, and it costs about 1% of one core.
//!
//! The keyword lines and the tuning live in [`nv_core::wake_model`], so the
//! settings app can show exactly what the spotter is listening for.

use std::path::PathBuf;

use nv_core::bpe::Bpe;
use nv_core::Config;
use sherpa_onnx::{KeywordSpotter, KeywordSpotterConfig, OnlineStream};

pub use nv_core::wake_model::Tuning;

pub struct Kws {
    spotter: KeywordSpotter,
    stream: OnlineStream,
    /// Samples fed since the last reset.
    fed: usize,
    /// The keyword lines, for the log.
    pub keywords: String,
    pub tuning: Tuning,
}

pub fn dir() -> PathBuf {
    nv_core::wake_model::dir()
}

/// True when the keyword model is on disk and usable.
pub fn installed() -> bool {
    nv_core::wake_model::is_installed()
}

/// File name in the pack, as a string the config wants.
fn file(rel: &str) -> Option<String> {
    let p = dir().join(rel);
    p.exists().then(|| p.to_string_lossy().to_string())
}

impl Kws {
    /// Load the model and build the keyword list. The error says what is
    /// missing, so both the agent log and the settings app can report it.
    pub fn new(cfg: &Config) -> Result<Kws, String> {
        let d = dir();
        if !installed() {
            return Err(format!("wake-word model not installed ({} is missing)", d.display()));
        }
        let bpe = Bpe::load(&d.join("bpe.model")).ok_or("could not read bpe.model")?;
        let keywords = nv_core::wake_model::keyword_lines(cfg, &bpe);
        log::info!("wake keywords:\n{keywords}");
        let mut c = KeywordSpotterConfig::default();
        c.model_config.transducer.encoder = file("encoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx");
        c.model_config.transducer.decoder = file("decoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx");
        c.model_config.transducer.joiner = file("joiner-epoch-12-avg-2-chunk-16-left-64.int8.onnx");
        c.model_config.tokens = file("tokens.txt");
        c.model_config.num_threads = 1;
        c.max_active_paths = 4;
        c.num_trailing_blanks = 1;
        c.keywords_buf = Some(keywords.clone());
        let spotter = KeywordSpotter::create(&c).ok_or("could not create the keyword spotter")?;
        let stream = spotter.create_stream();
        let tuning = Tuning::for_config(cfg);
        Ok(Kws { spotter, stream, fed: 0, keywords, tuning })
    }

    /// Feed 16 kHz audio; returns the keyword the moment it is recognised.
    pub fn feed(&mut self, samples: &[f32]) -> Option<String> {
        self.stream.accept_waveform(16_000, samples);
        self.fed += samples.len();
        let mut hit = None;
        while self.spotter.is_ready(&self.stream) {
            self.spotter.decode(&self.stream);
            if let Some(r) = self.spotter.get_result(&self.stream) {
                if !r.keyword.is_empty() {
                    hit = Some(r.keyword.replace('_', " "));
                    self.spotter.reset(&self.stream);
                    self.fed = 0;
                }
            }
        }
        hit
    }

    /// Start fresh. Only ever called during silence, so a phrase can never be
    /// cut in half by it.
    pub fn reset(&mut self) {
        if self.fed > 0 {
            self.spotter.reset(&self.stream);
            self.fed = 0;
        }
    }
}
