//! Text-to-speech through Windows' built-in SAPI voices (offline, no extra
//! downloads). Both the classic voices and the newer OneCore voices are listed.

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Media::Speech::{
    IEnumSpObjectTokens, ISpObjectToken, ISpObjectTokenCategory, ISpVoice, SpObjectTokenCategory, SpVoice,
    SPF_IS_NOT_XML, SPF_PURGEBEFORESPEAK,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};

const CATEGORIES: [&str; 2] = [
    r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech_OneCore\Voices",
    r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech\Voices",
];

fn tokens() -> Vec<(String, ISpObjectToken)> {
    let mut out: Vec<(String, ISpObjectToken)> = Vec::new();
    unsafe {
        for cat_id in CATEGORIES {
            let Ok(cat) = CoCreateInstance::<_, ISpObjectTokenCategory>(&SpObjectTokenCategory, None, CLSCTX_ALL) else {
                continue;
            };
            if cat.SetId(&HSTRING::from(cat_id), false).is_err() {
                continue;
            }
            let Ok(list): Result<IEnumSpObjectTokens, _> = cat.EnumTokens(PCWSTR::null(), PCWSTR::null()) else {
                continue;
            };
            let mut n = 0u32;
            let _ = list.GetCount(&mut n);
            for i in 0..n {
                let Ok(tok) = list.Item(i) else { continue };
                let Ok(p) = tok.GetStringValue(PCWSTR::null()) else { continue };
                let name = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                if !name.is_empty() && !out.iter().any(|(n, _)| n == &name) {
                    out.push((name, tok));
                }
            }
        }
    }
    out
}

/// Names of installed voices, e.g. "Microsoft Aria - English (United States)".
/// COM must be initialised on this thread.
pub fn list_voices() -> Vec<String> {
    tokens().into_iter().map(|(n, _)| n).collect()
}

pub struct Speaker {
    voice: ISpVoice,
}

impl Speaker {
    /// COM must be initialised on this thread.
    pub fn new(voice_name: &str, rate: i32, volume: u8) -> Result<Self, String> {
        unsafe {
            let voice: ISpVoice = CoCreateInstance(&SpVoice, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
            if !voice_name.is_empty() {
                if let Some((_, tok)) = tokens().into_iter().find(|(n, _)| n == voice_name) {
                    let _ = voice.SetVoice(&tok);
                }
            }
            let _ = voice.SetRate(rate.clamp(-10, 10));
            let _ = voice.SetVolume(volume.min(100) as u16);
            Ok(Self { voice })
        }
    }

    /// Speak and block until finished.
    pub fn say(&self, text: &str) {
        unsafe {
            let flags = (SPF_PURGEBEFORESPEAK.0 | SPF_IS_NOT_XML.0) as u32;
            if let Err(e) = self.voice.Speak(&HSTRING::from(text), flags, None) {
                log::warn!("speech failed: {e}");
            }
        }
    }
}
