//! Settings → Listening → Microphone check: live meters for every mic, and a
//! guided calibration that measures background noise and your voice, runs the
//! real speech recogniser, and works out settings that make you heard.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use nv_core::{wake, Config};

const QUIET_SECS: f32 = 3.0;
const SPEAK_MAX_SECS: f32 = 8.0;
/// Ceiling on the clip kept for transcribing, per microphone. The speaking step
/// is at most eight seconds; this leaves room for a device clock that runs fast.
const MAX_SPEAK_SAMPLES: usize = 192_000 * 12;

/// What a mic's callback is collecting right now.
const REC_OFF: u8 = 0;
const REC_QUIET: u8 = 1;
const REC_SPEAK: u8 = 2;

pub struct MicState {
    pub name: String,
    pub is_default: bool,
    rate: u32,
    /// Smoothed level for the meter, dBFS.
    level: Mutex<f32>,
    mode: Arc<AtomicU8>,
    quiet_blocks: Mutex<Vec<f32>>,
    speak_blocks: Mutex<Vec<f32>>,
    speak_audio: Mutex<Vec<f32>>,
}

impl MicState {
    fn new(name: String, is_default: bool, rate: u32) -> Arc<MicState> {
        Arc::new(MicState {
            name,
            is_default,
            rate,
            level: Mutex::new(-100.0),
            mode: Arc::new(AtomicU8::new(REC_OFF)),
            quiet_blocks: Mutex::new(Vec::new()),
            speak_blocks: Mutex::new(Vec::new()),
            speak_audio: Mutex::new(Vec::new()),
        })
    }

    pub fn level(&self) -> f32 {
        *self.level.lock().unwrap()
    }

    // Each of these takes the locks it needs and drops them before returning.
    // Holding two guards across one expression — a struct literal field list,
    // for instance — deadlocks the moment the second lock touches a mutex the
    // first one already holds, which is exactly what froze the settings window.

    /// Background level for this mic, dBFS.
    fn floor_db(&self) -> f32 {
        let quiet = self.quiet_blocks.lock().unwrap();
        let speak = self.speak_blocks.lock().unwrap();
        noise_floor(&quiet, &speak)
    }

    /// Peak speech level, dBFS.
    fn peak_db(&self) -> f32 {
        let speak = self.speak_blocks.lock().unwrap();
        percentile(&live(&speak), 0.95)
    }

    /// True once this mic has heard speech and then gone quiet again.
    fn finished_speaking(&self) -> bool {
        let floor = self.floor_db();
        let blocks = self.speak_blocks.lock().unwrap();
        let loud = |d: &f32| *d > floor + 10.0;
        if blocks.iter().filter(|d| loud(d)).count() <= 15 {
            return false;
        }
        let tail = blocks.len().saturating_sub(120); // ~1.2 s at 10 ms blocks
        !blocks[tail..].iter().any(loud)
    }

    fn report(&self) -> MicReport {
        MicReport { name: self.name.clone(), noise_db: self.floor_db(), speech_db: self.peak_db() }
    }

    fn clear(&self) {
        self.quiet_blocks.lock().unwrap().clear();
        self.speak_blocks.lock().unwrap().clear();
        self.speak_audio.lock().unwrap().clear();
    }
}

#[derive(Clone, PartialEq)]
pub enum Phase {
    Idle,
    Quiet(Instant),
    Speak(Instant),
    Analyzing,
}

#[derive(Clone, Debug)]
pub struct MicReport {
    pub name: String,
    pub noise_db: f32,
    pub speech_db: f32,
}

impl MicReport {
    pub fn snr(&self) -> f32 {
        self.speech_db - self.noise_db
    }
}

#[derive(Clone)]
pub struct Calibration {
    pub mics: Vec<MicReport>,
    /// None when nobody could be heard on any mic.
    pub best: Option<String>,
    pub gain_db: f32,
    pub min_speech_db: f32,
    pub vad: u8,
    pub transcript: Option<String>,
    pub wake_ok: bool,
    /// Sensitivity needed for what was heard to wake the assistant.
    pub sensitivity: Option<f32>,
}

pub struct MicCheck {
    _streams: Vec<cpal::Stream>,
    pub mics: Vec<Arc<MicState>>,
    pub phase: Phase,
    pub result: Option<Calibration>,
    pending: Option<(Calibration, crate::jobs::Job, std::path::PathBuf)>,
}

fn db(p: f32) -> f32 {
    10.0 * p.max(1e-12).log10()
}

fn percentile(v: &[f32], p: f32) -> f32 {
    if v.is_empty() {
        return -100.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    s[((s.len() - 1) as f32 * p).round() as usize]
}

impl MicCheck {
    /// Open every input device for metering.
    pub fn open() -> MicCheck {
        let host = cpal::default_host();
        let default = host.default_input_device().and_then(|d| d.description().ok()).map(|d| d.name().to_string());
        let mut streams = Vec::new();
        let mut mics = Vec::new();
        if let Ok(devs) = host.input_devices() {
            for d in devs {
                let Ok(desc) = d.description() else { continue };
                let name = desc.name().to_string();
                let Ok(sup) = d.default_input_config() else { continue };
                let ch = sup.channels() as usize;
                let is_default = Some(&name) == default.as_ref();
                let state = MicState::new(name, is_default, sup.sample_rate());
                let st = state.clone();
                let on_data = move |mono: Vec<f32>| {
                    let p = mono.iter().map(|s| s * s).sum::<f32>() / mono.len().max(1) as f32;
                    let d = db(p);
                    {
                        let mut l = st.level.lock().unwrap();
                        // fast attack, slow release
                        *l = if d > *l { d } else { *l * 0.85 + d * 0.15 };
                    }
                    match st.mode.load(Ordering::Relaxed) {
                        REC_QUIET => st.quiet_blocks.lock().unwrap().push(d),
                        REC_SPEAK => {
                            st.speak_blocks.lock().unwrap().push(d);
                            // The clip kept for transcribing is capped: a long
                            // monologue at 48 kHz is megabytes per microphone,
                            // and only the first few seconds are ever used.
                            let mut audio = st.speak_audio.lock().unwrap();
                            if audio.len() < MAX_SPEAK_SAMPLES {
                                audio.extend_from_slice(&mono);
                            }
                        }
                        _ => {}
                    }
                };
                let cfg: cpal::StreamConfig = sup.clone().into();
                let stream = match sup.sample_format() {
                    cpal::SampleFormat::F32 => d.build_input_stream::<f32, _, _>(
                        cfg,
                        move |data: &[f32], _| on_data(data.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect()),
                        |_| {},
                        None,
                    ),
                    cpal::SampleFormat::I16 => d.build_input_stream::<i16, _, _>(
                        cfg,
                        move |data: &[i16], _| {
                            on_data(data.chunks(ch).map(|f| f.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / ch as f32).collect())
                        },
                        |_| {},
                        None,
                    ),
                    _ => continue,
                };
                if let Ok(s) = stream {
                    if s.play().is_ok() {
                        streams.push(s);
                        mics.push(state);
                    }
                }
            }
        }
        // Windows default first.
        mics.sort_by_key(|m| !m.is_default);
        MicCheck { _streams: streams, mics, phase: Phase::Idle, result: None, pending: None }
    }

    pub fn start(&mut self) {
        for m in &self.mics {
            m.clear();
            m.mode.store(REC_QUIET, Ordering::Relaxed);
        }
        self.result = None;
        self.phase = Phase::Quiet(Instant::now());
    }

    /// Advance the wizard; call every frame.
    pub fn tick(&mut self, ctx: &eframe::egui::Context, cfg: &Config) {
        match self.phase.clone() {
            Phase::Quiet(t) if t.elapsed().as_secs_f32() >= QUIET_SECS => {
                for m in &self.mics {
                    m.mode.store(REC_SPEAK, Ordering::Relaxed);
                }
                self.phase = Phase::Speak(Instant::now());
            }
            Phase::Speak(t) => {
                let el = t.elapsed().as_secs_f32();
                // Finish early once someone spoke and then went quiet for a bit.
                let done_talking = el > 2.5 && self.mics.iter().any(|m| m.finished_speaking());
                if el >= SPEAK_MAX_SECS || done_talking {
                    for m in &self.mics {
                        m.mode.store(REC_OFF, Ordering::Relaxed);
                    }
                    self.analyze(ctx, cfg);
                }
            }
            Phase::Analyzing => {
                if let Some((cal, job, out)) = self.pending.take() {
                    if job.done() {
                        let mut cal = cal;
                        let text = std::fs::read_to_string(&out).unwrap_or_default();
                        if !text.is_empty() && !text.starts_with("ERROR") {
                            cal.wake_ok = wake::detect(&text, cfg).is_some();
                            cal.sensitivity = wake::sensitivity_needed(&text, cfg);
                            cal.transcript = Some(text);
                        } else if !text.is_empty() {
                            cal.transcript = Some(text);
                        }
                        self.result = Some(cal);
                        self.phase = Phase::Idle;
                    } else {
                        self.pending = Some((cal, job, out));
                    }
                }
            }
            _ => {}
        }
    }

    fn analyze(&mut self, ctx: &eframe::egui::Context, _cfg: &Config) {
        let reports: Vec<MicReport> = self.mics.iter().map(|m| m.report()).collect();
        let best = reports.iter().filter(|r| r.snr() >= 8.0).max_by(|a, b| a.snr().total_cmp(&b.snr())).cloned();
        let Some(best) = best else {
            self.result = Some(Calibration {
                mics: reports,
                best: None,
                gain_db: 0.0,
                min_speech_db: -50.0,
                vad: 1,
                transcript: None,
                wake_ok: false,
                sensitivity: None,
            });
            self.phase = Phase::Idle;
            return;
        };

        // Boost so speech peaks land around -22 dBFS.
        let gain_db = (-22.0 - best.speech_db).clamp(0.0, 30.0);
        let snr = best.snr();
        let noise = best.noise_db + gain_db;
        let speech = best.speech_db + gain_db;
        let min_speech_db = (noise + (0.3 * snr).clamp(4.0, 12.0)).min(speech - 8.0).clamp(-62.0, -25.0);
        let vad = if snr >= 25.0 {
            if noise > -45.0 { 3 } else { 2 }
        } else if snr >= 15.0 {
            1
        } else {
            0
        };
        let cal = Calibration {
            mics: reports,
            best: Some(best.name.clone()),
            gain_db,
            min_speech_db,
            vad,
            transcript: None,
            wake_ok: false,
            sensitivity: None,
        };

        // Run the real recogniser (the agent exe) on what the best mic heard.
        let Some(mic) = self.mics.iter().find(|m| m.name == best.name) else {
            // The device went away between measuring it and using it.
            self.result = Some(cal);
            self.phase = Phase::Idle;
            return;
        };
        let audio = resample(&mic.speak_audio.lock().unwrap(), mic.rate, 16_000);
        let g = 10f32.powf(gain_db / 20.0);
        let wav = std::env::temp_dir().join("needlevoice-miccheck.wav");
        let out = std::env::temp_dir().join("needlevoice-miccheck.txt");
        let _ = std::fs::remove_file(&out);
        write_wav(&wav, &audio.iter().map(|s| (s * g).clamp(-1.0, 1.0)).collect::<Vec<_>>());
        let agent = nv_core::paths::sibling_exe(nv_core::AGENT_EXE);
        let (wav2, out2) = (wav.clone(), out.clone());
        let job = crate::jobs::Job::spawn(ctx, "Listening back", move |_| {
            use std::os::windows::process::CommandExt;
            std::process::Command::new(&agent)
                .args(["--transcribe", &wav2.display().to_string(), "--out", &out2.display().to_string()])
                .creation_flags(0x0800_0000)
                .status()
                .map(|_| String::new())
                .map_err(|e| format!("couldn't run speech recognition: {e}"))
        });
        self.pending = Some((cal, job, out));
        self.phase = Phase::Analyzing;
    }
}

/// Run the whole wizard without a window, for `--miccheck`: it opens every
/// microphone, waits through both steps and returns what it measured. Handy for
/// checking a machine's audio, and a smoke test that the analysis finishes.
pub fn run_to_completion(
    cfg: &Config,
    ctx: &eframe::egui::Context,
    limit: Duration,
) -> Result<Calibration, String> {
    let mut check = MicCheck::open();
    if check.mics.is_empty() {
        return Err("no microphone could be opened".into());
    }
    check.start();
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        check.tick(ctx, cfg);
        if check.phase == Phase::Idle {
            return check.result.ok_or_else(|| "the check finished without a result".to_string());
        }
        std::thread::sleep(repaint());
    }
    Err(format!("the check didn't finish within {} seconds", limit.as_secs()))
}

/// The calibration as plain text, for the `--miccheck` report file.
pub fn describe(cal: &Calibration) -> String {
    let mut out = String::new();
    for r in &cal.mics {
        out.push_str(&format!(
            "{} {}  — background {:.0} dB, voice {:.0} dB ({} dB clearer)
",
            if cal.best.as_deref() == Some(r.name.as_str()) { "✔" } else { "•" },
            r.name,
            r.noise_db,
            r.speech_db,
            r.snr().max(0.0).round()
        ));
    }
    match &cal.best {
        None => out.push_str("Couldn't hear you on any microphone.\n"),
        Some(best) => {
            out.push_str(&format!("Best: {best}\n"));
            out.push_str(&format!("Mic boost: {:.0} dB\n", cal.gain_db));
            out.push_str(&format!("Speech threshold: {:.0} dB, noise filtering {}\n", cal.min_speech_db, cal.vad));
            match &cal.transcript {
                Some(t) => out.push_str(&format!("Heard back: \"{}\"\n", t.trim())),
                None => out.push_str("Heard back: (nothing)\n"),
            }
            out.push_str(&format!(
                "Wake word recognised: {}{}\n",
                if cal.wake_ok { "yes" } else { "no" },
                match cal.sensitivity {
                    Some(s) => format!(" (sensitivity {s:.2} would be needed)"),
                    None => String::new(),
                }
            ));
        }
    }
    out
}

fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == to {
        return input.to_vec();
    }
    let step = from as f64 / to as f64;
    // Box low-pass by averaging the samples each output covers.
    let n = (input.len() as f64 / step) as usize;
    (0..n)
        .map(|i| {
            let a = (i as f64 * step) as usize;
            let b = (((i + 1) as f64 * step) as usize).min(input.len()).max(a + 1);
            input[a..b].iter().sum::<f32>() / (b - a) as f32
        })
        .collect()
}

fn write_wav(path: &std::path::Path, audio: &[f32]) {
    let n = audio.len() as u32;
    let mut w = Vec::with_capacity(44 + audio.len() * 2);
    w.extend(b"RIFF");
    w.extend((36 + n * 2).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes());
    w.extend(1u16.to_le_bytes());
    w.extend(16000u32.to_le_bytes());
    w.extend(32000u32.to_le_bytes());
    w.extend(2u16.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"data");
    w.extend((n * 2).to_le_bytes());
    for s in audio {
        w.extend(((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    let _ = std::fs::write(path, w);
}

/// How long the current step has left, for the UI.
pub fn remaining(phase: &Phase) -> Option<f32> {
    match phase {
        Phase::Quiet(t) => Some((QUIET_SECS - t.elapsed().as_secs_f32()).max(0.0)),
        Phase::Speak(t) => Some((SPEAK_MAX_SECS - t.elapsed().as_secs_f32()).max(0.0)),
        _ => None,
    }
}

pub const fn repaint() -> Duration {
    Duration::from_millis(33)
}

/// Blocks with real signal. USB mics (webcams) send pure digital zeros while
/// starting up; those aren't "background noise" and would fake a huge margin.
fn live(blocks: &[f32]) -> Vec<f32> {
    blocks.iter().copied().filter(|d| *d > -100.0).collect()
}

/// Background level: the quiet step if it has enough real signal, otherwise
/// the quietest moments of the speaking step.
fn noise_floor(quiet: &[f32], speak: &[f32]) -> f32 {
    let q = live(quiet);
    if q.len() >= 50 {
        percentile(&q, 0.5)
    } else {
        percentile(&live(speak), 0.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mic(name: &str) -> Arc<MicState> {
        MicState::new(name.to_string(), true, 48_000)
    }

    fn fill(blocks: &Mutex<Vec<f32>>, db: f32, n: usize) {
        blocks.lock().unwrap().extend(std::iter::repeat(db).take(n));
    }

    /// Regression: the report used to lock the same mutex twice inside one
    /// struct literal. Those guards overlap, so the second lock never returned
    /// and the settings window froze on the last step of the mic check.
    #[test]
    fn building_a_report_does_not_deadlock() {
        let m = mic("test mic");
        fill(&m.quiet_blocks, -60.0, 100);
        fill(&m.speak_blocks, -20.0, 200);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(m.report());
        });
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(r) => {
                assert_eq!(r.name, "test mic");
                assert!(r.speech_db > r.noise_db, "{r:?}");
            }
            Err(_) => panic!("planning the report deadlocked"),
        }
    }

    /// The same shape of bug, one level up: a report for every microphone.
    #[test]
    fn a_whole_batch_of_reports_finishes() {
        let mics: Vec<Arc<MicState>> = (0..4).map(|i| mic(&format!("mic {i}"))).collect();
        for m in &mics {
            fill(&m.quiet_blocks, -55.0, 120);
            fill(&m.speak_blocks, -18.0, 300);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let reports: Vec<MicReport> = mics.iter().map(|m| m.report()).collect();
            let _ = tx.send(reports.len());
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).expect("deadlock"), 4);
    }

    #[test]
    fn hears_speech_then_silence() {
        let m = mic("talker");
        fill(&m.quiet_blocks, -60.0, 300);
        fill(&m.speak_blocks, -60.0, 50);
        fill(&m.speak_blocks, -20.0, 60);
        fill(&m.speak_blocks, -60.0, 200);
        assert!(m.finished_speaking(), "should stop once they go quiet");

        let still = mic("still talking");
        fill(&still.quiet_blocks, -60.0, 300);
        fill(&still.speak_blocks, -20.0, 200);
        assert!(!still.finished_speaking(), "should not stop mid-sentence");
    }

    #[test]
    fn silence_reads_as_silence() {
        let m = mic("dead mic");
        fill(&m.quiet_blocks, -90.0, 300);
        fill(&m.speak_blocks, -90.0, 300);
        let r = m.report();
        assert!(r.snr() < 8.0, "silence must not look like a clear voice: {r:?}");
    }

    #[test]
    fn a_loud_room_needs_more_filtering() {
        let m = mic("noisy");
        fill(&m.quiet_blocks, -35.0, 300);
        fill(&m.speak_blocks, -20.0, 300);
        let r = m.report();
        assert!(r.snr() < 20.0 && r.snr() > 5.0, "{r:?}");
    }
}
