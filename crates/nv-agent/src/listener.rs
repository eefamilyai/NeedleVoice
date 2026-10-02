//! The always-on loop: mic → voice-activity detection → Whisper (only when
//! someone is talking) → wake phrase → command → Needle 3 → actions.
//!
//! Idle cost is just the audio callback plus a WebRTC VAD check every 30 ms;
//! Whisper only runs on short bursts of speech, and both models are dropped
//! from memory after a minute of quiet.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use nv_core::apps::AppIndex;
use nv_core::brain::{Action, Brain};
use nv_core::{actions, wake, Config};
use webrtc_vad::{SampleRate, Vad, VadMode};

use crate::audio::{Mic, RATE};
use crate::bubble::Mode;
use crate::tts::Tts;
use nv_core::personality::{self, Moment, Outcome};
use crate::overlay::Shared;
use crate::stt::Stt;

const FRAME: usize = 480; // 30 ms at 16 kHz
const FRAME_MS: u32 = 30;
const PREROLL_FRAMES: usize = 12; // keep ~360 ms before speech starts
const WAKE_END_SILENCE_MS: u32 = 450;
const EARLY_CHECK_SECS: f32 = 2.2;
const MAX_IDLE_SEGMENT_SECS: f32 = 8.0;
const MAX_COMMAND_SECS: f32 = 15.0;
const MIN_SPEECH_FRAMES: u32 = 7; // ~200 ms of actual speech
/// Silence before the wake-word spotter is wound back to a clean state.
/// Only ever reset then, so a phrase can't be split in half.
const KWS_RESET_AFTER_MS: u32 = 1200;
/// How long the microphone is ignored after waking, so the chime (180 ms) and
/// the last syllable of the name can't be mistaken for the start of a command.
/// Kept under the preroll window below, which means anything spoken during it
/// is still prepended to the command when speech is finally detected.
const WAKE_SETTLE_MS: u64 = 240;
/// The settle window must fit inside the preroll, or the first words of the
/// command after the name would be lost. Checked at compile time.
const _: () = assert!(WAKE_SETTLE_MS <= PREROLL_FRAMES as u64 * FRAME_MS as u64);

pub enum Ctl {
    SetPaused(bool),
    Rescan,
    Quit,
}

enum Phase {
    /// Waiting for "hey <name>".
    Idle,
    /// Woken; waiting for the command to start.
    Await { deadline: Instant },
    /// Recording the command. `has_wake` = the audio still begins with the wake phrase.
    Command { has_wake: bool },
}

struct Segment {
    audio: Vec<f32>,
    speech_frames: u32,
    silence_frames: u32,
    early_checked: bool,
    rejected: bool,
}

impl Segment {
    fn secs(&self) -> f32 {
        self.audio.len() as f32 / RATE as f32
    }
}

pub fn run(cfg: Config, shared: Arc<Shared>, apps: Arc<RwLock<AppIndex>>, tts: Arc<Tts>, ctl: Receiver<Ctl>) {
    let models = nv_core::paths::models_dir();
    let mut stt = Stt::new(models.join(&cfg.whisper_model), cfg.threads, &cfg.agent_name);
    let mut brain = Brain::new(models.join(nv_core::NEEDLE_MODEL), cfg.needle_depth);
    if !brain.model_exists() {
        log::error!("Needle model missing at {}", models.display());
    }
    let idle = Duration::from_secs(cfg.unload_after_secs.max(5));
    let mut paused = cfg.paused;
    let mut mic: Option<Mic> = None;
    let mut last_audio = Instant::now();
    let mut last_open_try = Instant::now() - Duration::from_secs(60);

    let mode = match cfg.vad_aggressiveness {
        0 => VadMode::Quality,
        1 => VadMode::LowBitrate,
        2 => VadMode::Aggressive,
        _ => VadMode::VeryAggressive,
    };
    let mut vad = Vad::new_with_rate_and_mode(SampleRate::Rate16kHz, mode);

    let mut pending: Vec<f32> = Vec::with_capacity(FRAME * 4);
    let mut preroll: VecDeque<[f32; FRAME]> = VecDeque::with_capacity(PREROLL_FRAMES + 1);
    let mut seg: Option<Segment> = None;
    let mut phase = Phase::Idle;
    let mut level_smooth = 0.0f32;
    // Lets a multi-step function speak as it goes, not only at the end.
    let speaker = crate::tts::AgentVoice::new(tts.clone(), shared.clone());
    let mut was_speaking = false;
    // Software boost from calibration, for quiet microphones.
    let gain = 10f32.powf(cfg.mic_gain_db / 20.0);
    // Streaming wake-word spotter: it hears the name the moment it is spoken,
    // instead of waiting for Whisper to transcribe a finished sentence. If the
    // model isn't installed we fall back to the old Whisper-based detection.
    let mut kws = match crate::kws::Kws::new(&cfg) {
        Ok(k) => Some(k),
        Err(e) => {
            log::warn!("wake-word spotter unavailable ({e}) — using Whisper to find the name");
            None
        }
    };
    if let Some(k) = &kws {
        log::info!(
            "wake spotter ready: boost {:.2}, threshold {:.3}, {} keyword(s)",
            k.tuning.boost,
            k.tuning.threshold,
            k.keywords.lines().count()
        );
    }
    // Frames of quiet fed to the spotter since it was last reset.
    let mut spotter_quiet_ms = 0u32;
    // Ignore the mic briefly after waking so the chime isn't taken as speech.
    let mut deaf_until = Instant::now();

    log::info!("listening for \"{}\"", cfg.wake_phrase());

    loop {
        // ── control messages ───────────────────────────────────────────
        while let Ok(msg) = ctl.try_recv() {
            match msg {
                Ctl::Quit => return,
                Ctl::SetPaused(p) => {
                    paused = p;
                    if p {
                        mic = None; // release the microphone entirely
                        seg = None;
                        phase = Phase::Idle;
                        shared.set_mode(Mode::Hidden);
                        log::info!("paused");
                    } else {
                        log::info!("resumed");
                    }
                }
                Ctl::Rescan => crate::spawn_app_scan(cfg.clone(), apps.clone()),
            }
        }

        stt.unload_if_idle(idle);
        brain.unload_if_idle(idle);

        if paused {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }

        // ── (re)open the microphone ────────────────────────────────────
        if mic.is_none() || last_audio.elapsed() > Duration::from_secs(4) {
            if last_open_try.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            last_open_try = Instant::now();
            mic = None;
            match Mic::open(&cfg.microphone) {
                Ok(m) => {
                    mic = Some(m);
                    last_audio = Instant::now();
                }
                Err(e) => {
                    log::warn!("microphone unavailable: {e}");
                    continue;
                }
            }
        }

        // Wake periodically even without audio so timeouts fire.
        let chunk = match mic.as_ref().unwrap().rx.recv_timeout(Duration::from_millis(100)) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => {
                check_timeout(&mut phase, &shared);
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                mic = None;
                continue;
            }
        };
        last_audio = Instant::now();
        pending.extend_from_slice(&chunk);

        let mut consumed = 0;
        while pending.len() - consumed >= FRAME {
            let mut frame = [0f32; FRAME];
            frame.copy_from_slice(&pending[consumed..consumed + FRAME]);
            consumed += FRAME;
            if gain != 1.0 {
                for s in frame.iter_mut() {
                    *s = (*s * gain).clamp(-1.0, 1.0);
                }
            }

            // Don't listen to ourselves talk: dropping frames while the voice
            // is playing is what stops the reply (or the wake chime) from being
            // recorded as the next command. A command already in flight keeps
            // recording, so a long sentence isn't cut off mid-way.
            let speaking = shared.speaking();
            if speaking || was_speaking {
                was_speaking = speaking;
                if !matches!(phase, Phase::Command { .. }) {
                    seg = None;
                    preroll.clear();
                    continue;
                }
            }

            let rms = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
            let db = 20.0 * rms.max(1e-9).log10();
            let pcm: Vec<i16> = frame.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect();
            let speech = db > cfg.min_speech_db && vad.is_voice_segment(&pcm).unwrap_or(false);

            // Mic level for the orb: -60 dB → 0, -15 dB → 1.
            let lvl = ((db + 60.0) / 45.0).clamp(0.0, 1.0);
            level_smooth = level_smooth * 0.6 + lvl * 0.4;
            if !matches!(phase, Phase::Idle) {
                shared.set_level(if speech { level_smooth } else { level_smooth * 0.5 });
            }

            // ── wake word (keyword spotter) ──
            if let (Some(k), Phase::Idle) = (kws.as_mut(), &phase) {
                // Fed every frame, not just during speech: a 3.3 M parameter
                // model costs ~1% of one core, and never gating the audio means
                // the name can't be missed because it started quietly.
                if let Some(word) = k.feed(&frame) {
                    let t = Instant::now();
                    log::info!("wake word: \"{word}\" — listening now");
                    woke(&cfg, &shared, &tts, &mut phase, &mut deaf_until);
                    log::debug!("wake handled in {:?}", t.elapsed());
                    k.reset();
                    spotter_quiet_ms = 0;
                    seg = None;
                    preroll.clear();
                    continue;
                }
                spotter_quiet_ms = if speech { 0 } else { spotter_quiet_ms + FRAME_MS };
                if spotter_quiet_ms >= KWS_RESET_AFTER_MS {
                    k.reset();
                    spotter_quiet_ms = 0;
                }
                continue;
            }
            let speech = speech && Instant::now() >= deaf_until;

            let outcome = step(&mut seg, &mut preroll, &frame, speech, &phase, &cfg);
            match outcome {
                Step::Nothing => {}
                Step::EarlyCheck => {
                    let s = seg.as_mut().unwrap();
                    s.early_checked = true;
                    let text = transcribe(&mut stt, &s.audio);
                    if wake::detect(&text, &cfg).is_some() {
                        log::info!("wake (early): {text:?}");
                        shared.set_mode(Mode::Listening);
                        tts.prepare();
                        phase = Phase::Command { has_wake: true };
                    } else {
                        s.rejected = true;
                    }
                }
                Step::SegmentEnd => {
                    let s = seg.take().unwrap();
                    match phase {
                        Phase::Idle => {
                            if s.rejected || s.early_checked || s.speech_frames < MIN_SPEECH_FRAMES {
                                continue;
                            }
                            let text = transcribe(&mut stt, &s.audio);
                            if let Some(hit) = wake::detect(&text, &cfg) {
                                log::info!("wake: {text:?}");
                                if has_words(&hit.command) {
                                    shared.set_mode(Mode::Listening);
                                    tts.prepare();
                                    phase = handle_command(&hit.command, &cfg, &mut brain, &apps, &shared, &tts, speaker.as_ref());
                                    drain(mic.as_ref());
                                } else {
                                    shared.set_mode(Mode::Listening);
                                    tts.prepare();
                                    phase = Phase::Await { deadline: Instant::now() + secs(cfg.command_timeout_secs) };
                                }
                            } else if wake::name_score(&text, &cfg) > 0.7 {
                                // Helps tune sensitivity: the name was close but not accepted.
                                log::info!("almost woke (try raising sensitivity): heard {text:?}");
                            } else {
                                log::info!("speech without wake word ({:.1}s)", s.secs());
                            }
                        }
                        Phase::Command { has_wake } => {
                            let text = transcribe(&mut stt, &s.audio);
                            let hit = wake::detect(&text, &cfg);
                            // Saying the name again with nothing after it just
                            // re-arms the clock instead of dropping back to sleep.
                            if hit.as_ref().is_some_and(|h| !has_words(&h.command)) {
                                log::info!("wake again: {text:?}");
                                shared.set_mode(Mode::Listening);
                                phase = Phase::Await { deadline: Instant::now() + secs(cfg.command_timeout_secs) };
                                continue;
                            }
                            let command = match hit {
                                Some(hit) => hit.command,
                                None if has_wake => strip_leading_name(&text, &cfg),
                                None => text.clone(),
                            };
                            log::info!("heard: {text:?} -> command {command:?}");
                            if has_words(&command) {
                                phase = handle_command(&command, &cfg, &mut brain, &apps, &shared, &tts, speaker.as_ref());
                                drain(mic.as_ref());
                            } else if has_wake {
                                phase = Phase::Await { deadline: Instant::now() + secs(cfg.command_timeout_secs) };
                            } else {
                                shared.set_mode(Mode::Hidden);
                                phase = Phase::Idle;
                            }
                        }
                        Phase::Await { .. } => {}
                    }
                    if matches!(phase, Phase::Idle) {
                        shared.set_level(0.0);
                    }
                }
                Step::SpeechStarted => {
                    if let Phase::Await { .. } = phase {
                        phase = Phase::Command { has_wake: false };
                    }
                }
            }
        }
        pending.drain(..consumed);
        check_timeout(&mut phase, &shared);
    }
}

/// Everything that has to happen the instant the name is heard: the bubble
/// appears, the chime plays, and the clock starts on the command.
fn woke(cfg: &Config, shared: &Shared, tts: &Tts, phase: &mut Phase, deaf_until: &mut Instant) {
    let now = Instant::now();
    if cfg.wake_sound {
        crate::chime::play();
    }
    *deaf_until = now + Duration::from_millis(WAKE_SETTLE_MS);
    // Start loading the voice now, so the reply isn't delayed later.
    tts.prepare();
    if cfg.wake_reply && tts.active() {
        // A word back is the clearest "I heard you", but it talks over the
        // first moment of the command, so it's off unless asked for.
        shared.set_speaking(true);
        tts.say(&personality::line(cfg.personality, &Moment::WakeAck));
    }
    shared.set_mode(Mode::Listening);
    *phase = Phase::Await { deadline: now + secs(cfg.command_timeout_secs) };
}

enum Step {
    Nothing,
    SpeechStarted,
    EarlyCheck,
    SegmentEnd,
}

/// Advance the segmenter by one frame.
fn step(
    seg: &mut Option<Segment>,
    preroll: &mut VecDeque<[f32; FRAME]>,
    frame: &[f32; FRAME],
    speech: bool,
    phase: &Phase,
    cfg: &Config,
) -> Step {
    match seg {
        None => {
            preroll.push_back(*frame);
            if preroll.len() > PREROLL_FRAMES {
                preroll.pop_front();
            }
            if speech {
                let mut audio = Vec::with_capacity(RATE as usize * 4);
                for f in preroll.drain(..) {
                    audio.extend_from_slice(&f);
                }
                *seg = Some(Segment { audio, speech_frames: 1, silence_frames: 0, early_checked: false, rejected: false });
                return Step::SpeechStarted;
            }
            Step::Nothing
        }
        Some(s) => {
            s.audio.extend_from_slice(frame);
            if speech {
                s.speech_frames += 1;
                s.silence_frames = 0;
            } else {
                s.silence_frames += 1;
            }
            let (end_ms, max_secs) = match phase {
                Phase::Idle => (WAKE_END_SILENCE_MS, MAX_IDLE_SEGMENT_SECS),
                _ => (cfg.end_silence_ms, MAX_COMMAND_SECS),
            };
            if s.silence_frames * FRAME_MS >= end_ms || s.secs() >= max_secs {
                return Step::SegmentEnd;
            }
            // Long utterance in idle: check its start for the wake phrase now,
            // so the orb appears while the user is still talking.
            if matches!(phase, Phase::Idle)
                && !s.early_checked
                && !s.rejected
                && s.secs() >= EARLY_CHECK_SECS
                && s.speech_frames >= MIN_SPEECH_FRAMES
            {
                return Step::EarlyCheck;
            }
            Step::Nothing
        }
    }
}

fn check_timeout(phase: &mut Phase, shared: &Shared) {
    if let Phase::Await { deadline } = phase {
        if Instant::now() > *deadline {
            log::info!("no command heard, going back to sleep");
            shared.set_mode(Mode::Hidden);
            *phase = Phase::Idle;
        }
    }
}

fn transcribe(stt: &mut Stt, audio: &[f32]) -> String {
    let t = Instant::now();
    match stt.transcribe(audio) {
        Ok(text) => {
            log::debug!("stt {:.1}s audio in {:?}: {text:?}", audio.len() as f32 / RATE as f32, t.elapsed());
            text
        }
        Err(e) => {
            log::error!("{e}");
            String::new()
        }
    }
}

fn handle_command(
    command: &str,
    cfg: &Config,
    brain: &mut Brain,
    apps: &Arc<RwLock<AppIndex>>,
    shared: &Shared,
    tts: &Tts,
    speaker: &crate::tts::AgentVoice,
) -> Phase {
    shared.set_mode(Mode::Thinking);
    let index = apps.read().unwrap().clone();
    let decision = brain.decide(command, cfg, &index);
    log::info!(
        "command {command:?} -> {:?} via {:?} in {} ms",
        decision.actions,
        decision.via,
        decision.millis
    );
    // Talk back, using the app's real name ("chrome" -> "Google Chrome").
    let real = |n: &str| index.find(n, cfg).map(|(e, _)| e.name.clone()).unwrap_or_else(|| n.to_string());
    let pretty = |a: &Action| match a {
        Action::OpenApp(n) => Action::OpenApp(real(n)),
        Action::CloseApp(n) => Action::CloseApp(real(n)),
        other => other.clone(),
    };
    let mut outcomes: Vec<Outcome> = Vec::new();
    for action in &decision.actions {
        let action = pretty(action);
        match actions::execute_with(&action, cfg, &index, speaker) {
            Ok(msg) => {
                log::info!("  ✓ {msg}");
                outcomes.push(Outcome::Done(action, msg));
            }
            Err(e) => {
                log::warn!("  ✗ {e}");
                outcomes.push(Outcome::Failed(action, e));
            }
        }
    }
    shared.set_level(0.0);

    let reply = personality::reply(cfg, &outcomes);
    log::info!("reply: {reply:?}");
    let failed = outcomes.iter().any(|o| matches!(o, Outcome::Failed(..)));
    // Set speaking before the mode flips so the bubble stays up for the reply.
    if tts.active() {
        shared.set_speaking(true);
    }
    tts.say(&reply);
    shared.set_mode(if failed { Mode::Error } else { Mode::Success });
    Phase::Idle
}

/// Throw away audio captured while we were busy thinking.
fn drain(mic: Option<&Mic>) {
    if let Some(m) = mic {
        while m.rx.try_recv().is_ok() {}
    }
}

fn has_words(s: &str) -> bool {
    s.chars().filter(|c| c.is_alphanumeric()).count() >= 2
}

/// Fallback when the full transcript no longer matches the wake phrase:
/// drop everything up to and including the word closest to the name.
fn strip_leading_name(text: &str, cfg: &Config) -> String {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let words: Vec<String> = tokens.iter().map(|t| nv_core::fuzzy::squash(t)).collect();
    match nv_core::fuzzy::find_name(&words, &cfg.agent_name, 3) {
        Some((_, _, end)) => tokens.get(end..).unwrap_or_default().join(" "),
        None => text.to_string(),
    }
}

fn secs(s: f32) -> Duration {
    Duration::from_secs_f32(s)
}
