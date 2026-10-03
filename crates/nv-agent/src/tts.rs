//! Talking back. Neural voices (Piper / Kokoro via sherpa-onnx) or the
//! built-in Windows voices, on a background thread so listening never blocks.
//!
//! The model is loaded when the wake word is heard (while the user is still
//! speaking the command) and dropped again after a minute of quiet.

use std::collections::VecDeque;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use nv_core::voices::{self, Engine};
use nv_core::Config;
use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig};

use crate::overlay::Shared;

enum Msg {
    Prepare,
    Say(String),
}

pub struct Tts {
    tx: Option<Sender<Msg>>,
    shared: Arc<Shared>,
}

impl Tts {
    pub fn start(cfg: &Config, shared: Arc<Shared>) -> Tts {
        if !cfg.voice_enabled {
            return Tts { tx: None, shared };
        }
        let (tx, rx) = channel::<Msg>();
        let cfg = cfg.clone();
        let shared2 = shared.clone();
        let spawned = std::thread::Builder::new().name("tts".into()).spawn(move || {
            let shared = shared2;
            nv_core::win::com_init();
            let idle = Duration::from_secs(cfg.unload_after_secs.max(5));
            let mut voice: Option<Voice> = None;
            // Kept open across replies so the output device stays awake.
            let mut player: Option<Player> = None;
            let mut last_used = Instant::now();
            loop {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(Msg::Prepare) => {
                        if voice.is_none() {
                            voice = Voice::load(&cfg);
                        }
                        // Wake the output device now, while the command is still
                        // being spoken, so the reply starts the moment it is ready.
                        if let Some(p) = ensure_player(&voice, &cfg, &shared, &mut player) {
                            p.prime_hard();
                        }
                        last_used = Instant::now();
                    }
                    Ok(Msg::Say(first)) => {
                        // If several lines queued up, only the newest is worth saying.
                        let mut text = first;
                        for m in rx.try_iter() {
                            if let Msg::Say(t) = m {
                                text = t;
                            }
                        }
                        if voice.is_none() {
                            voice = Voice::load(&cfg);
                        }
                        shared.set_speaking(true);
                        match &voice {
                            Some(v) => v.speak(&text, &cfg, &shared, &mut player),
                            None => log::warn!("no voice available"),
                        }
                        shared.set_level(0.0);
                        shared.set_speaking(false);
                        last_used = Instant::now();
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if voice.is_some() && last_used.elapsed() > idle {
                            voice = None;
                            player = None;
                            log::info!("voice unloaded (idle)");
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
        Tts { tx: spawned.ok().map(|_| tx), shared }
    }

    pub fn active(&self) -> bool {
        self.tx.is_some()
    }

    /// Start loading the voice now so the reply isn't delayed later.
    pub fn prepare(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Prepare);
        }
    }

    pub fn say(&self, text: &str) {
        if let Some(tx) = &self.tx {
            log::info!("says: {text:?}");
            if tx.send(Msg::Say(text.to_string())).is_err() {
                self.shared.set_speaking(false);
            }
        }
    }
}

/// The agent's voice behind the trait `nv_core::actions` uses, so a function
/// can speak mid-sequence instead of only at the end.
pub struct AgentVoice {
    tts: Arc<Tts>,
    shared: Arc<Shared>,
}

impl AgentVoice {
    pub fn new(tts: Arc<Tts>, shared: Arc<Shared>) -> Arc<AgentVoice> {
        Arc::new(AgentVoice { tts, shared })
    }
}

impl nv_core::actions::Speaker for AgentVoice {
    fn say(&self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        log::info!("says: {text:?}");
        // Set speaking before the voice starts so the bubble stays up and the
        // microphone stays closed while it talks.
        self.shared.set_speaking(true);
        self.tts.say(text);
    }
}

/// Synthesise `text` with the configured voice and write it as a WAV, without
/// playing anything. Separating the two is the only way to tell a voice that
/// clips a word from a player that does.
pub fn synthesize_to_wav(cfg: &Config, text: &str, out: &std::path::Path) -> Result<(), String> {
    nv_core::win::com_init();
    let Some(Voice::Neural { tts, sid }) = Voice::load(cfg) else {
        return Err("the configured voice is not a neural one".into());
    };
    let gen = GenerationConfig { sid, speed: cfg.voice_speed, ..Default::default() };
    let audio = tts
        .generate_with_config(text, &gen, None::<fn(&[f32], f32) -> bool>)
        .ok_or("synthesis failed")?;
    let samples = audio.samples();
    let rate = tts.sample_rate() as u32;
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    let data_len = (samples.len() * 2) as u32;
    bytes.extend(b"RIFF");
    bytes.extend((36 + data_len).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * 2).to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(data_len.to_le_bytes());
    for s in samples {
        bytes.extend(((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(out, bytes).map_err(|e| e.to_string())
}

/// Speak once and return (used by the config app's "Preview voice").
pub fn speak_once(cfg: &Config, text: &str) {
    nv_core::win::com_init();
    let shared = Shared::new();
    match Voice::load(cfg) {
        Some(v) => {
            let mut player = None;
            v.speak(text, cfg, &shared, &mut player)
        }
        None => log::warn!("preview: voice unavailable"),
    }
}

enum Voice {
    Neural { tts: OfflineTts, sid: i32 },
    System(nv_core::voice::Speaker),
}

/// [`ensure_player`] for a voice that is already in hand.
fn ensure_player_from<'a>(
    voice: &Voice,
    cfg: &Config,
    shared: &Arc<Shared>,
    player: &'a mut Option<Player>,
) -> Option<&'a mut Player> {
    let Voice::Neural { tts, .. } = voice else { return None };
    let rate = tts.sample_rate() as u32;
    let stale = player.as_ref().is_none_or(|p| p.rate != rate || p.volume != cfg.voice_volume);
    if stale {
        match Player::start(rate, cfg.voice_volume, shared.clone()) {
            Ok(p) => *player = Some(p),
            Err(e) => {
                log::error!("audio output: {e}");
                return None;
            }
        }
    }
    player.as_mut()
}

/// Open the speakers for a neural voice, or hand back the one already open.
///
/// Created on the wake word rather than on the reply: the device — a Bluetooth
/// headset here — takes a moment to wake, and doing that while the user is still
/// speaking keeps the delay off the reply.
fn ensure_player<'a>(
    voice: &Option<Voice>,
    cfg: &Config,
    shared: &Arc<Shared>,
    player: &'a mut Option<Player>,
) -> Option<&'a mut Player> {
    let Voice::Neural { tts, .. } = voice.as_ref()? else { return None };
    let rate = tts.sample_rate() as u32;
    let stale = player.as_ref().is_none_or(|p| p.rate != rate || p.volume != cfg.voice_volume);
    if stale {
        match Player::start(rate, cfg.voice_volume, shared.clone()) {
            Ok(p) => *player = Some(p),
            Err(e) => {
                log::error!("audio output: {e}");
                return None;
            }
        }
    }
    player.as_mut()
}

impl Voice {
    fn load(cfg: &Config) -> Option<Voice> {
        let t = Instant::now();
        let v = match cfg.voice_engine {
            Engine::System => {
                let rate = ((cfg.voice_speed - 1.0) * 10.0).round() as i32;
                nv_core::voice::Speaker::new(&cfg.system_voice, rate, cfg.voice_volume).ok().map(Voice::System)
            }
            Engine::Piper => load_piper(&cfg.piper_voice, cfg.threads)
                .or_else(|| {
                    log::warn!("Piper voice {} not installed, using default", cfg.piper_voice);
                    load_piper(voices::DEFAULT_PIPER, cfg.threads)
                })
                .map(|tts| Voice::Neural { tts, sid: 0 }),
            Engine::Kokoro => load_kokoro(&cfg.kokoro_voice, cfg.threads)
                .or_else(|| {
                    log::warn!("Kokoro isn't installed, falling back to Piper");
                    load_piper(&cfg.piper_voice, cfg.threads).map(|tts| (tts, 0))
                })
                .map(|(tts, sid)| Voice::Neural { tts, sid }),
        };
        // Last resort: the Windows voice is always there.
        let v = v.or_else(|| nv_core::voice::Speaker::new("", 0, cfg.voice_volume).ok().map(Voice::System));
        log::info!("voice loaded in {:?}", t.elapsed());
        v
    }

    fn speak(&self, text: &str, cfg: &Config, shared: &Arc<Shared>, player: &mut Option<Player>) {
        match self {
            Voice::System(s) => s.say(text),
            Voice::Neural { tts, sid } => {
                let _ = tts;
                // Kept open between replies: closing and reopening the stream is
                // what lets the device fall asleep in the first place. The wake
                // word normally opens it first, in time to wake a Bluetooth link.
                let Some(player) = ensure_player_from(self, cfg, shared, player) else { return };
                player.prime();
                // Each sentence arrives through the callback as soon as it's
                // synthesised, so playback starts before the whole reply is done.
                let queue = player.feeder();
                let sink = queue.clone();
                let pushed = Arc::new(Mutex::new(false));
                let pushed2 = pushed.clone();
                let gen = GenerationConfig { sid: *sid, speed: cfg.voice_speed, ..Default::default() };
                let audio = tts.generate_with_config(
                    text,
                    &gen,
                    Some(move |chunk: &[f32], _progress: f32| {
                        sink.push(chunk);
                        *pushed2.lock().unwrap() = true;
                        true
                    }),
                );
                if !*pushed.lock().unwrap() {
                    if let Some(a) = &audio {
                        queue.push(a.samples());
                    }
                }
                player.finish();
            }
        }
    }
}

fn s(p: &std::path::Path) -> Option<String> {
    Some(p.to_string_lossy().to_string())
}

fn load_piper(id: &str, threads: u32) -> Option<OfflineTts> {
    let dir = voices::pack_dir(id);
    let model = voices::pack_model(&dir)?;
    let mut c = OfflineTtsConfig::default();
    c.model.vits.model = s(&model);
    c.model.vits.tokens = s(&dir.join("tokens.txt"));
    c.model.vits.data_dir = s(&dir.join("espeak-ng-data"));
    c.model.num_threads = threads.min(8) as i32;
    c.max_num_sentences = 1;
    OfflineTts::create(&c)
}

fn load_kokoro(name: &str, threads: u32) -> Option<(OfflineTts, i32)> {
    let dir = voices::pack_dir(voices::KOKORO_PACK);
    if !voices::is_installed(voices::KOKORO_PACK) {
        return None;
    }
    let v = voices::kokoro_voice(name);
    let british = v.name.starts_with('b');
    let mut c = OfflineTtsConfig::default();
    c.model.kokoro.model = s(&dir.join("model.onnx"));
    c.model.kokoro.voices = s(&dir.join("voices.bin"));
    c.model.kokoro.tokens = s(&dir.join("tokens.txt"));
    c.model.kokoro.data_dir = s(&dir.join("espeak-ng-data"));
    c.model.kokoro.lexicon = s(&dir.join(if british { "lexicon-gb-en.txt" } else { "lexicon-us-en.txt" }));
    c.model.kokoro.lang = Some(if british { "en-gb" } else { "en-us" }.into());
    c.model.num_threads = threads.min(8) as i32;
    c.max_num_sentences = 1;
    OfflineTts::create(&c).map(|t| (t, v.sid))
}

/// A signal that keeps an audio link awake without being audible.
///
/// Two things matter. Level: at -78 dBFS it is 18 dB quieter than the first
/// attempt, which was audible as a hiss in a quiet room. Spectrum: white noise
/// is the most noticeable thing you can play at a given level, so this changes
/// value only every few hundred samples — a slow rumble below about 150 Hz,
/// which is far harder to hear on any speaker or headset.
fn dither(n: usize) -> Vec<f32> {
    const SLOW: usize = 320; // ~150 Hz at 48 kHz
    let mut seed: u32 = 0x9E37_79B9;
    let mut value = 0.0f32;
    (0..n)
        .map(|i| {
            if i % SLOW == 0 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                value = ((seed >> 8) as f32 / 8_388_608.0 - 1.0) * 0.00013;
            }
            value
        })
        .collect()
}

fn device_name(d: &cpal::Device) -> String {
    d.description().map(|d| d.name().to_string()).unwrap_or_default()
}

/// Streams mono samples to the default speakers, resampling to the device
/// rate, and feeds the bubble the playback loudness.
struct Player {
    _stream: cpal::Stream,
    feeder: Feeder,
    /// Sample rate the speech arrives at.
    rate: u32,
    volume: u8,
    /// When it last had something to say, for deciding how much lead-in to give.
    last_used: Instant,
    /// Until when the output keeps a whisper of signal going, so a link that
    /// suspends on silence is already awake when the reply starts.
    keep_alive: Arc<Mutex<Instant>>,
}

#[derive(Clone)]
struct Feeder {
    queue: Arc<Mutex<VecDeque<f32>>>,
    step: f64,
    state: Arc<Mutex<(f64, f32)>>, // (fractional position, previous sample)
    /// When set, everything queued for the device is also kept, so what we sent
    /// can be compared with what was heard.
    dump: Option<Arc<Mutex<Vec<f32>>>>,
    rate: u32,
}

impl Feeder {
    /// Resample (linear) and enqueue a chunk.
    fn push(&self, chunk: &[f32]) {
        let mut st = self.state.lock().unwrap();
        let (mut pos, mut prev) = *st;
        let mut out = Vec::with_capacity((chunk.len() as f64 / self.step) as usize + 2);
        for &x in chunk {
            while pos <= 1.0 {
                out.push(prev + (x - prev) * pos as f32);
                pos += self.step;
            }
            pos -= 1.0;
            prev = x;
        }
        *st = (pos, prev);
        if let Some(dump) = &self.dump {
            dump.lock().unwrap().extend_from_slice(&out);
        }
        self.queue.lock().unwrap().extend(out);
    }
}

impl Player {
    fn start(src_rate: u32, volume: u8, shared: Arc<Shared>) -> Result<Player, String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no speakers found")?;
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        let channels = supported.channels() as usize;
        let rate = supported.sample_rate();
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let queue = Arc::new(Mutex::new(VecDeque::<f32>::new()));
        let gain = volume.min(100) as f32 / 100.0;

        let keep_alive = Arc::new(Mutex::new(Instant::now()));
        let dump = std::env::var("NV_DUMP_TTS").ok().map(|_| Arc::new(Mutex::new(Vec::<f32>::new())));
        let dump_in_callback = dump.clone();
        let q = queue.clone();
        let awake_until = keep_alive.clone();
        let mut env = 0.0f32;
        // Only while a reply is on its way or just past: the rest of the time the
        // output is genuinely silent, because a constant whisper is worse than
        // the clipped syllable it was meant to prevent.
        let idle = dither(64 * 1024);
        let mut idle_at = idle.len();
        let mut fill = move |out: &mut dyn FnMut(usize, f32), frames: usize| {
            let mut q = q.lock().unwrap();
            let whisper = Instant::now() < *awake_until.lock().unwrap();
            let mut sum = 0.0;
            for i in 0..frames {
                let s = match q.pop_front() {
                    Some(v) => v * gain,
                    None if whisper => {
                        if idle_at >= idle.len() {
                            idle_at = 0;
                        }
                        idle_at += 1;
                        let v = idle[idle_at - 1];
                        if let Some(dump) = &dump_in_callback {
                            dump.lock().unwrap().push(v);
                        }
                        v
                    }
                    None => 0.0,
                };
                sum += s * s;
                for c in 0..channels {
                    out(i * channels + c, s);
                }
            }
            let rms = (sum / frames.max(1) as f32).sqrt();
            env = env * 0.7 + (rms * 4.0).min(1.0) * 0.3;
            shared.set_level(env);
        };
        let err = |e: cpal::Error| {
            if e.kind() != cpal::ErrorKind::Xrun {
                log::error!("playback error: {e}");
            }
        };
        let stream = match format {
            cpal::SampleFormat::F32 => device.build_output_stream::<f32, _, _>(
                config.clone(),
                move |data: &mut [f32], _| {
                    let frames = data.len() / channels;
                    fill(&mut |i, s| data[i] = s, frames);
                },
                err,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream::<i16, _, _>(
                config.clone(),
                move |data: &mut [i16], _| {
                    let frames = data.len() / channels;
                    fill(&mut |i, s| data[i] = (s.clamp(-1.0, 1.0) * 32767.0) as i16, frames);
                },
                err,
                None,
            ),
            other => return Err(format!("unsupported output format {other:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        let feeder = Feeder {
            queue,
            step: src_rate as f64 / rate as f64,
            state: Arc::new(Mutex::new((0.0, 0.0))),
            dump,
            rate,
        };
        log::info!("speakers: {} ({rate} Hz, {channels} ch, {format:?})", device_name(&device));
        Ok(Player { _stream: stream, feeder, rate: src_rate, volume, last_used: Instant::now(), keep_alive })
    }

    fn feeder(&self) -> Feeder {
        self.feeder.clone()
    }

    /// Silence in front of the speech.
    ///
    /// Monitor speakers, Bluetooth headsets and codecs that power down between
    /// sounds all take a moment to wake, and the first thing they are asked to
    /// play is the first phoneme of the reply — which is why "Reminder" arrives
    /// as "minder". A little silence costs nothing and gives them that moment.
    /// Wake the device properly: used when the wake word is heard, where the
    /// leading sound happens while the user is still talking.
    fn prime_hard(&mut self) {
        self.prime_for(400);
        self.wake_for(Duration::from_secs(20));
        self.last_used = Instant::now();
    }

    /// Keep the output awake for this long: from the wake word until well after
    /// the reply, and no longer.
    fn wake_for(&self, how_long: Duration) {
        *self.keep_alive.lock().unwrap() = Instant::now() + how_long;
    }

    fn prime(&mut self) {
        let cold = self.last_used.elapsed() > Duration::from_secs(2);
        self.prime_for(if cold { 300 } else { 40 });
    }

    /// Lead-in before speech, at about -60 dBFS.
    ///
    /// Deliberately *not* digital silence: a Bluetooth link suspends when the
    /// stream is silent, and a suspended link loses the first samples it is
    /// given — which is why "Reminder" arrives as "minder". A noise floor too
    /// quiet to hear keeps the link up and the word whole.
    fn prime_for(&mut self, ms: u32) {
        let n = (self.rate as u64 * ms as u64 / 1000) as usize;
        self.feeder.push(&dither(n));
    }

    /// Write what was queued for the device, if `NV_DUMP_TTS` names a folder.
    fn dump_sent(&self) {
        let (Some(dump), Ok(dir)) = (&self.feeder.dump, std::env::var("NV_DUMP_TTS")) else { return };
        let samples = dump.lock().unwrap();
        let stamp = nv_core::schedule::Stamp::now().clock().replace(':', "");
        let path = std::path::Path::new(&dir).join(format!("sent-{stamp}.wav"));
        let _ = std::fs::create_dir_all(&dir);
        let _ = crate::stt::write_wav_at(&path, &samples, self.feeder.rate);
        log::info!("sent {} samples ({} ms) to the device -> {}", samples.len(), samples.len() as f32 * 1000.0 / self.feeder.rate as f32, path.display());
    }

    /// Block until everything queued has played, without closing the device.
    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !self.feeder.queue.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        // Let the device drain its own buffer.
        std::thread::sleep(Duration::from_millis(150));
        self.dump_sent();
        // Two seconds of grace, then silence: the link stays up across a reply
        // and its tail, not for the rest of the session.
        self.wake_for(Duration::from_secs(2));
        self.last_used = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keep_alive_signal_is_inaudible_and_slow() {
        let d = dither(48_000);
        let peak = d.iter().fold(0f32, |m, s| m.max(s.abs()));
        // -78 dBFS peak: a fifth of a percent of full scale.
        assert!(peak < 0.0003, "peak {peak} is loud enough to hear");
        assert!(peak > 0.0, "it has to be a signal, not silence");
        // Slow: it should change value only every few hundred samples, which is
        // what stops it sounding like a hiss.
        let changes = d.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(changes < d.len() / 100, "{changes} changes in {} samples is a hiss", d.len());
    }
}
