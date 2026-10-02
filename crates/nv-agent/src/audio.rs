//! Microphone capture → 16 kHz mono f32, the format Whisper and the VAD want.

use std::sync::mpsc::{sync_channel, Receiver, SyncSender};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub const RATE: u32 = 16_000;

pub struct Mic {
    _streams: Vec<cpal::Stream>,
    pub rx: Receiver<Vec<f32>>,
}

fn device_name(d: &cpal::Device) -> String {
    d.description().map(|d| d.name().to_string()).unwrap_or_default()
}

impl Mic {
    /// Open `name`, or the Windows default microphone when empty / not found.
    pub fn open(name: &str) -> Result<Mic, String> {
        // Dev/testing: NV_FAKE_MIC=<16 kHz mono wav> plays a file in real time, then silence.
        if let Some(path) = std::env::var_os("NV_FAKE_MIC") {
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let pos = bytes.windows(4).position(|w| w == b"data").map(|p| p + 8).unwrap_or(44);
            let audio: Vec<f32> =
                bytes[pos..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
            let (tx, rx) = sync_channel::<Vec<f32>>(256);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                let silence = vec![0.0001f32; 160];
                let mut it = audio.chunks(160);
                loop {
                    let c = it.next().map(|c| c.to_vec()).unwrap_or_else(|| silence.clone());
                    if tx.send(c).is_err() {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            });
            log::info!("microphone: fake ({})", std::path::Path::new(&path).display());
            return Ok(Mic { _streams: Vec::new(), rx });
        }
        let host = cpal::default_host();
        if !name.is_empty() {
            if let Some(d) = host.input_devices().ok().and_then(|mut it| it.find(|d| device_name(d) == name)) {
                return Self::open_device(d);
            }
            log::warn!("microphone \"{name}\" not found, using the Windows default");
        }
        Self::open_device(host.default_input_device().ok_or("no microphone found")?)
    }

    fn open_device(device: cpal::Device) -> Result<Mic, String> {
        let device_name = device_name(&device);
        let supported = device.default_input_config().map_err(|e| e.to_string())?;
        let channels = supported.channels() as usize;
        let in_rate = supported.sample_rate();
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        log::info!("microphone: {device_name} ({in_rate} Hz, {channels} ch, {format:?})");

        // Bounded so a stalled consumer can't grow memory; chunks are dropped instead.
        let (tx, rx) = sync_channel::<Vec<f32>>(256);
        let mut rs = Resampler::new(in_rate, RATE);

        // Over/underruns are harmless glitches (e.g. while a game hogs the CPU).
        let err = |e: cpal::Error| {
            if e.kind() != cpal::ErrorKind::Xrun {
                log::error!("audio stream error: {e}");
            }
        };
        macro_rules! build {
            ($t:ty, $conv:expr) => {{
                let tx: SyncSender<Vec<f32>> = tx.clone();
                device.build_input_stream::<$t, _, _>(
                    config.clone(),
                    move |data: &[$t], _| {
                        let mono: Vec<f32> = data
                            .chunks(channels)
                            .map(|f| f.iter().map(|&s| $conv(s)).sum::<f32>() / channels as f32)
                            .collect();
                        let out = rs.process(&mono);
                        if !out.is_empty() {
                            let _ = tx.try_send(out);
                        }
                    },
                    err,
                    None,
                )
            }};
        }
        let stream = match format {
            cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
            cpal::SampleFormat::I16 => build!(i16, |s: i16| s as f32 / 32768.0),
            cpal::SampleFormat::I32 => build!(i32, |s: i32| s as f32 / 2147483648.0),
            cpal::SampleFormat::U16 => build!(u16, |s: u16| (s as f32 - 32768.0) / 32768.0),
            other => return Err(format!("unsupported sample format {other:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Mic { _streams: vec![stream], rx })
    }
}

/// Streaming resampler: one-pole low-pass, then linear interpolation.
/// Plenty for speech recognition and nearly free.
struct Resampler {
    step: f64,
    pos: f64,
    prev: f32,
    lp: f32,
    alpha: f32,
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        // Cut-off a little under the target Nyquist.
        let fc = to as f32 * 0.45;
        let dt = 1.0 / from as f32;
        let rc = 1.0 / (2.0 * std::f32::consts::PI * fc);
        Self { step: from as f64 / to as f64, pos: 0.0, prev: 0.0, lp: 0.0, alpha: dt / (rc + dt) }
    }

    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let mut out = Vec::with_capacity((input.len() as f64 / self.step) as usize + 2);
        for &x in input {
            let filtered = if self.step > 1.0 {
                self.lp += self.alpha * (x - self.lp);
                self.lp
            } else {
                x
            };
            // Emit every output sample that falls between prev and this input.
            while self.pos <= 1.0 {
                let t = self.pos as f32;
                out.push(self.prev + (filtered - self.prev) * t);
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev = filtered;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn resample_length() {
        let mut r = super::Resampler::new(48_000, 16_000);
        let n: usize = (0..100).map(|_| r.process(&[0.0; 480]).len()).sum();
        assert!((15_990..=16_010).contains(&n), "{n}");
    }
}
