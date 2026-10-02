//! End-to-end check on WAV files in target/wav (generated with Windows TTS):
//! Whisper → wake phrase → Needle decision. Nothing is executed.

use std::time::Instant;

use nv_core::apps::AppIndex;
use nv_core::brain::Brain;
use nv_core::{wake, Config};

fn read_wav(path: &std::path::Path) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap();
    let pos = bytes.windows(4).position(|w| w == b"data").unwrap() + 8;
    bytes[pos..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect()
}

#[test]
#[ignore]
fn pipeline_on_wavs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cfg = Config::default();
    let models = root.join("models");
    let mut stt = crate::stt::Stt::new(models.join(&cfg.whisper_model), cfg.threads, &cfg.agent_name);
    let mut brain = Brain::new(models.join(nv_core::NEEDLE_MODEL), cfg.needle_depth);
    let apps = AppIndex::load_cache().unwrap_or_default();
    let mut entries: Vec<_> = std::fs::read_dir(root.join("target/wav")).unwrap().flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let audio = read_wav(&e.path());
        let t = Instant::now();
        let text = stt.transcribe(&audio).unwrap();
        let stt_ms = t.elapsed().as_millis();
        let hit = wake::detect(&text, &cfg);
        let decision = hit.as_ref().filter(|h| !h.command.is_empty()).map(|h| brain.decide(&h.command, &cfg, &apps));
        println!(
            "{:<18} stt {:>4}ms {:?}\n{:<18} wake={:?}\n{:<18} -> {:?}",
            e.file_name().to_string_lossy(),
            stt_ms,
            text,
            "",
            hit.map(|h| h.command),
            "",
            decision.map(|d| (d.actions, d.via, d.millis))
        );
    }
}

#[test]
#[ignore]
fn whisper_timing() {
    use whisper_rs::*;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let audio = read_wav(&root.join("target/wav/open_chrome.wav"));
    let ctx = WhisperContext::new_with_params(root.join("models/ggml-tiny.en-q5_1.bin").to_str().unwrap(), WhisperContextParameters::default()).unwrap();
    let mut st = ctx.create_state().unwrap();
    for (threads, actx, prompt) in [(4, 0, false), (4, 256, false), (8, 256, false), (4, 256, true), (1, 256, false), (4, 512, false)] {
        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        p.set_language(Some("en")); p.set_n_threads(threads); p.set_single_segment(true); p.set_no_timestamps(true);
        p.set_print_progress(false); p.set_print_realtime(false); p.set_temperature_inc(0.0);
        if actx > 0 { p.set_audio_ctx(actx); }
        if prompt { p.set_initial_prompt("Hey Nova, open Chrome."); }
        let t = Instant::now();
        st.full(p, &audio).unwrap();
        let txt: String = st.as_iter().map(|s| s.to_str_lossy().unwrap().to_string()).collect();
        println!("threads {threads} actx {actx} prompt {prompt}: {:?} {txt:?}", t.elapsed());
    }
}

#[test]
#[ignore]
fn neural_tts_timing() {
    use sherpa_onnx::*;
    let v = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/voices");
    let s = |p: &std::path::Path| Some(p.to_string_lossy().to_string());
    let text = "Ooh, the Haber process! Pulling that up for you.";
    let mut cases: Vec<(&str, OfflineTtsConfig, i32)> = Vec::new();
    for (name, onnx) in [("vits-piper-en_US-lessac-medium", "en_US-lessac-medium.onnx"), ("vits-piper-en_US-lessac-high-fp16", "en_US-lessac-high.onnx")] {
        let d = v.join(name);
        let mut c = OfflineTtsConfig::default();
        c.model.vits.model = s(&d.join(onnx));
        c.model.vits.tokens = s(&d.join("tokens.txt"));
        c.model.vits.data_dir = s(&d.join("espeak-ng-data"));
        c.model.num_threads = 6;
        cases.push((name, c, 0));
    }
    let d = v.join("kokoro-multi-lang-v1_0");
    let mut c = OfflineTtsConfig::default();
    c.model.kokoro.model = s(&d.join("model.onnx"));
    c.model.kokoro.voices = s(&d.join("voices.bin"));
    c.model.kokoro.tokens = s(&d.join("tokens.txt"));
    c.model.kokoro.data_dir = s(&d.join("espeak-ng-data"));
    c.model.kokoro.lexicon = s(&d.join("lexicon-us-en.txt"));
    c.model.kokoro.lang = Some("en-us".into());
    c.model.num_threads = 6;
    cases.push(("kokoro af_heart", c, 3));
    for (name, cfg, sid) in cases {
        let t = Instant::now();
        let tts = OfflineTts::create(&cfg).expect(name);
        let load = t.elapsed();
        let t = Instant::now();
        let _ = tts.generate_with_config("Hi.", &GenerationConfig { sid, ..Default::default() }, None::<fn(&[f32], f32) -> bool>);
        println!("  warm {:?}", t.elapsed());
        let t = Instant::now();
        let audio = tts.generate_with_config(text, &GenerationConfig { sid, ..Default::default() }, None::<fn(&[f32], f32) -> bool>).unwrap();
        let secs = audio.samples().len() as f32 / audio.sample_rate() as f32;
        println!("{name:40} load {load:?} gen {:?} for {secs:.1}s audio @{}Hz", t.elapsed(), audio.sample_rate());
        audio.save(&format!("{}/../../target/tts-{}.wav", env!("CARGO_MANIFEST_DIR"), name.replace(' ', "_")));
    }
}

/// Records the default mic for a few seconds and prints per-channel levels.
#[test]
#[ignore]
fn mic_probe() {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::{Arc, Mutex};
    let host = cpal::default_host();
    let dev = host.default_input_device().unwrap();
    let cfg = dev.default_input_config().unwrap();
    println!("device {:?} {:?}", dev.description().map(|d| d.name().to_string()), cfg);
    let ch = cfg.channels() as usize;
    let stats = Arc::new(Mutex::new((vec![0f64; ch], vec![0f32; ch], 0usize, 0f64)));
    let s2 = stats.clone();
    let stream = dev
        .build_input_stream::<f32, _, _>(
            cfg.into(),
            move |data: &[f32], _| {
                let mut s = s2.lock().unwrap();
                for f in data.chunks(ch) {
                    let mut mix = 0.0;
                    for (i, &x) in f.iter().enumerate() {
                        s.0[i] += (x * x) as f64;
                        s.1[i] = s.1[i].max(x.abs());
                        mix += x;
                    }
                    let m = mix / ch as f32;
                    s.3 += (m * m) as f64;
                    s.2 += 1;
                }
            },
            |e| println!("err {e}"),
            None,
        )
        .unwrap();
    stream.play().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(4));
    drop(stream);
    let s = stats.lock().unwrap();
    for i in 0..ch {
        let rms = (s.0[i] / s.2 as f64).sqrt();
        println!("ch{i}: rms {:.1} dB, peak {:.1} dB", 20.0 * rms.max(1e-9).log10(), 20.0 * (s.1[i].max(1e-9) as f64).log10());
    }
    println!("mono mix rms {:.1} dB over {} frames", 10.0 * (s.3 / s.2 as f64).max(1e-18).log10(), s.2);
}

/// Records 10 s through the real Mic pipeline, saves target/live.wav and
/// reports what the VAD and Whisper make of it.
#[test]
#[ignore]
fn live_wake() {
    let mic = crate::audio::Mic::open(&std::env::var("NV_MIC").unwrap_or_default()).unwrap();
    let start = Instant::now();
    let mut audio: Vec<f32> = Vec::new();
    let secs: u64 = std::env::var("NV_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
    while start.elapsed().as_secs() < secs {
        if let Ok(c) = mic.rx.recv_timeout(std::time::Duration::from_millis(200)) {
            audio.extend(c);
        }
    }
    drop(mic);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // save wav
    let mut w = Vec::new();
    let n = audio.len() as u32;
    w.extend(b"RIFF"); w.extend((36 + n * 2).to_le_bytes()); w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes()); w.extend(1u16.to_le_bytes()); w.extend(1u16.to_le_bytes());
    w.extend(16000u32.to_le_bytes()); w.extend(32000u32.to_le_bytes()); w.extend(2u16.to_le_bytes()); w.extend(16u16.to_le_bytes());
    w.extend(b"data"); w.extend((n * 2).to_le_bytes());
    for s in &audio { w.extend(((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()); }
    std::fs::write(root.join("target/live.wav"), w).unwrap();

    let mut vad = webrtc_vad::Vad::new_with_rate_and_mode(webrtc_vad::SampleRate::Rate16kHz, webrtc_vad::VadMode::Aggressive);
    let mut line = String::new();
    let (mut loud, mut voiced, mut both) = (0, 0, 0);
    let mut max_db = -120f32;
    for f in audio.chunks_exact(480) {
        let rms = (f.iter().map(|s| s * s).sum::<f32>() / 480.0).sqrt();
        let db = 20.0 * rms.max(1e-9).log10();
        max_db = max_db.max(db);
        let pcm: Vec<i16> = f.iter().map(|s| (s * 32767.0) as i16).collect();
        let v = vad.is_voice_segment(&pcm).unwrap_or(false);
        let l = db > -45.0;
        loud += l as u32; voiced += v as u32; both += (l && v) as u32;
        line.push(match (l, v) { (true, true) => '#', (false, true) => 'v', (true, false) => 'l', _ => '.' });
    }
    println!("{} s audio, max frame {max_db:.1} dB; frames loud={loud} vad={voiced} both={both}", audio.len() / 16000);
    println!("{line}");
    let cfg = Config::default();
    let mut stt = crate::stt::Stt::new(root.join("models").join(&cfg.whisper_model), 4, "Nova");
    let text = stt.transcribe(&audio).unwrap();
    println!("whisper: {text:?}\nwake: {:?}", wake::detect(&text, &cfg));
}

#[test]
#[ignore]
fn list_mics() {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    println!("default: {:?}", host.default_input_device().and_then(|d| d.description().ok()).map(|d| d.name().to_string()));
    for d in host.input_devices().unwrap() {
        println!("input: {:?} {:?}", d.description().map(|d| d.name().to_string()), d.default_input_config().map(|c| (c.channels(), c.sample_rate())));
    }
}

#[test]
#[ignore]
fn auto_mic() {
    struct L;
    impl log::Log for L {
        fn enabled(&self, _: &log::Metadata) -> bool { true }
        fn log(&self, r: &log::Record) { println!("{}", r.args()); }
        fn flush(&self) {}
    }
    let _ = log::set_logger(&L);
    log::set_max_level(log::LevelFilter::Info);
    let m = crate::audio::Mic::open("").unwrap();
    let t = Instant::now();
    while t.elapsed().as_secs() < 8 { let _ = m.rx.recv_timeout(std::time::Duration::from_millis(100)); }
}

#[test]
#[ignore]
fn realtek_timeline() {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::{Arc, Mutex};
    let dev = cpal::default_host().default_input_device().unwrap();
    let cfg = dev.default_input_config().unwrap();
    let ch = cfg.channels() as usize;
    let buf = Arc::new(Mutex::new(Vec::<f32>::new()));
    let b2 = buf.clone();
    let s = dev.build_input_stream::<f32, _, _>(cfg.into(), move |d: &[f32], _| {
        b2.lock().unwrap().extend(d.chunks(ch).map(|f| f[0]));
    }, |_| {}, None).unwrap();
    s.play().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(20));
    drop(s);
    let a = buf.lock().unwrap();
    let mut line = String::new();
    let mut peak = -120f32;
    for w in a.chunks(4800) { // 100 ms
        let db = 10.0 * (w.iter().map(|x| x * x).sum::<f32>() / w.len() as f32).max(1e-12).log10();
        peak = peak.max(db);
        line.push(match db as i32 { d if d > -30 => '#', d if d > -40 => '=', d if d > -50 => '-', d if d > -58 => '.', _ => ' ' });
    }
    println!("peak 100ms window: {peak:.1} dB\n[{line}]");
    // save 16k-ish copy (every 3rd sample) for whisper check
    let mono: Vec<f32> = a.iter().step_by(3).copied().collect();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let maxabs = mono.iter().fold(0f32, |m, x| m.max(x.abs())).max(1e-6);
    let norm: Vec<f32> = mono.iter().map(|x| x / maxabs * 0.7).collect();
    let mut stt = crate::stt::Stt::new(root.join("models/ggml-tiny.en-q5_1.bin"), 4, "Nova");
    println!("raw whisper: {:?}", stt.transcribe(&mono).unwrap());
    println!("normalized whisper: {:?} (gain {:.0} dB)", stt.transcribe(&norm).unwrap(), 20.0 * (0.7 / maxabs).log10());
}

#[test]
#[ignore]
fn mic_volumes() {
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap();
        let col = en.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE).unwrap();
        for i in 0..col.GetCount().unwrap() {
            let d = col.Item(i).unwrap();
            let store = d.OpenPropertyStore(STGM_READ).unwrap();
            let name = store.GetValue(&PKEY_Device_FriendlyName).map(|v| v.to_string()).unwrap_or_default();
            let vol: IAudioEndpointVolume = d.Activate(CLSCTX_ALL, None).unwrap();
            let scalar = vol.GetMasterVolumeLevelScalar().unwrap();
            let db = vol.GetMasterVolumeLevel().unwrap();
            let mute = vol.GetMute().unwrap().as_bool();
            println!("{name}: level {:.0}% ({db:.1} dB) muted={mute}", scalar * 100.0);
        }
    }
}

/// Proves the per-keyword boost/threshold in the keyword lines really reach
/// sherpa: an impossible threshold must stop the name being recognised at all.
#[test]
#[ignore]
fn kws_tuning_is_live() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let clip = read_wav(&root.join("target/wav/open_chrome.wav"));
    let fires = |boost: f32, threshold: f32| {
        let mut cfg = Config::default();
        cfg.wake_score = boost;
        cfg.wake_threshold = threshold;
        let mut k = crate::kws::Kws::new(&cfg).expect("kws model");
        clip.chunks(480).any(|c| k.feed(c).is_some())
    };
    assert!(fires(2.0, 0.25), "the shipped defaults must hear \"hey nova\"");
    assert!(!fires(0.1, 0.99), "a near-impossible threshold must silence it");
    println!("defaults fire, impossible threshold does not — the tuning is live");
}

/// Sweeps the keyword spotter's boost and threshold over every recording we
/// have, so the shipped defaults are the ones that actually work. Positives
/// live in target/wav, hard negatives (names that sound like "Nova") in
/// target/wav_neg.
#[test]
#[ignore]
fn kws_sweep() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let wavs = |dir: &str| -> Vec<(String, Vec<f32>)> {
        let mut v: Vec<_> = std::fs::read_dir(root.join(dir))
            .map(|d| d.flatten().collect::<Vec<_>>())
            .unwrap_or_default();
        v.sort_by_key(|e| e.file_name());
        v.into_iter().map(|e| (e.file_name().to_string_lossy().to_string(), read_wav(&e.path()))).collect()
    };
    let pos = wavs("target/wav");
    let neg = wavs("target/wav_neg");
    println!("{} positive, {} negative clips", pos.len(), neg.len());
    let grid: &[(f32, f32)] = &[
        (1.0, 0.250),
        (1.5, 0.250),
        (2.0, 0.250),
        (2.5, 0.250),
        (3.0, 0.250),
        (2.0, 0.200),
        (2.0, 0.150),
        (3.0, 0.200),
        (3.0, 0.150),
        (3.0, 0.100),
        (4.0, 0.100),
        (1.0, 0.100),
        (2.0, 0.050),
    ];
    for &(score, threshold) in grid {
        let mut grid_cfg = Config::default();
        grid_cfg.wake_score = score;
        grid_cfg.wake_threshold = threshold;
        let mut missed = Vec::new();
        let mut latencies = Vec::new();
        for (name, audio) in &pos {
            let mut k = crate::kws::Kws::new(&grid_cfg).expect("kws model");
            let mut hit = None;
            for (i, c) in audio.chunks(480).enumerate() {
                if let Some(h) = k.feed(c) {
                    hit = Some((h, i as f32 * 0.03));
                    break;
                }
            }
            match hit {
                Some((_, at)) => latencies.push(at),
                None => missed.push(name.clone()),
            }
        }
        let mut false_alarms = Vec::new();
        for (name, audio) in &neg {
            let mut k = crate::kws::Kws::new(&grid_cfg).expect("kws model");
            let mut hit = None;
            for (i, c) in audio.chunks(480).enumerate() {
                if let Some(h) = k.feed(c) {
                    hit = Some((h, i as f32 * 0.03));
                    break;
                }
            }
            if let Some((h, at)) = hit {
                false_alarms.push(format!("{name}@{at:.2}s->{h}"));
            }
        }
        let avg = if latencies.is_empty() {
            0.0
        } else {
            latencies.iter().sum::<f32>() / latencies.len() as f32
        };
        println!(
            "boost {score:>4.1} thr {threshold:.3} | caught {}/{}  avg {:>4.2}s  max {:>4.2}s | false {:>2}/{} {} | missed {}",
            pos.len() - missed.len(),
            pos.len(),
            avg,
            latencies.iter().cloned().fold(0.0f32, f32::max),
            false_alarms.len(),
            neg.len(),
            if false_alarms.is_empty() { String::new() } else { false_alarms.join(", ") },
            if missed.is_empty() { String::new() } else { missed.join(", ") }
        );
    }
}

#[test]
#[ignore]
fn kws_on_wavs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cfg = Config::default();
    let mut k = crate::kws::Kws::new(&cfg).expect("kws model");
    let mut entries: Vec<_> = std::fs::read_dir(root.join(std::env::var("NV_WAVS").unwrap_or("target/wav".into()))).unwrap().flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let audio = read_wav(&e.path());
        k.reset();
        let t = Instant::now();
        let mut hit = None;
        for (i, c) in audio.chunks(480).enumerate() {
            if let Some(h) = k.feed(c) {
                hit = Some((h, i as f32 * 0.03));
                break;
            }
        }
        let tail = vec![0f32; 16000];
        if hit.is_none() { if let Some(h) = k.feed(&tail) { hit = Some((h, audio.len() as f32 / 16000.0)); } }
        println!("{:<20} {:?}  ({:?} cpu for {:.1}s audio)", e.file_name().to_string_lossy(), hit, t.elapsed(), audio.len() as f32 / 16000.0);
    }
}
