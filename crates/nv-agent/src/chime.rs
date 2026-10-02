//! A short, soft two-note "I'm listening" chime, synthesised once and played
//! asynchronously from memory.

use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

fn wav() -> &'static [u8] {
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    WAV.get_or_init(|| {
        let rate = 24_000u32;
        let mut samples: Vec<i16> = Vec::new();
        // Rising fifth: E6 then B6, each with a quick attack and soft decay.
        for (freq, ms) in [(1318.5f32, 70u32), (1975.5, 110)] {
            let n = rate * ms / 1000;
            for i in 0..n {
                let t = i as f32 / rate as f32;
                let attack = (i as f32 / (rate as f32 * 0.004)).min(1.0);
                let decay = (-(t * 1000.0 / ms as f32) * 3.0).exp();
                let s = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.7
                    + (2.0 * std::f32::consts::PI * freq * 2.0 * t).sin() * 0.12;
                samples.push((s * attack * decay * 0.22 * 32767.0) as i16);
            }
        }
        let data_len = samples.len() as u32 * 2;
        let mut w = Vec::with_capacity(44 + data_len as usize);
        w.extend(b"RIFF");
        w.extend((36 + data_len).to_le_bytes());
        w.extend(b"WAVEfmt ");
        w.extend(16u32.to_le_bytes());
        w.extend(1u16.to_le_bytes());
        w.extend(1u16.to_le_bytes());
        w.extend(rate.to_le_bytes());
        w.extend((rate * 2).to_le_bytes());
        w.extend(2u16.to_le_bytes());
        w.extend(16u16.to_le_bytes());
        w.extend(b"data");
        w.extend(data_len.to_le_bytes());
        for s in samples {
            w.extend(s.to_le_bytes());
        }
        w
    })
}

pub fn play() {
    unsafe {
        let _ = PlaySoundW(PCWSTR(wav().as_ptr().cast()), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}
