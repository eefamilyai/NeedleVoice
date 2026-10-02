//! Spot "hey <name>" at the start of a transcript and split off the command.

use crate::config::Config;
use crate::fuzzy;

#[derive(Debug, Clone, PartialEq)]
pub struct WakeHit {
    /// Similarity of the heard name to the configured one (0–1).
    pub score: f64,
    /// Whatever followed the wake phrase, with original casing/punctuation.
    pub command: String,
}

/// Similarity needed for the name, from the 0–1 sensitivity slider.
fn threshold(cfg: &Config) -> f64 {
    0.93 - 0.14 * cfg.wake_sensitivity as f64
}

/// Words Whisper commonly writes for "hey".
fn prefix_matches(word: &str, cfg: &Config) -> bool {
    let w = fuzzy::squash(word);
    cfg.wake_prefixes.iter().any(|p| {
        let p = fuzzy::squash(p);
        w == p || (p == "hey" && matches!(w.as_str(), "hay" | "hei" | "heh" | "a" | "eh" | "hey"))
            || (p == "ok" && matches!(w.as_str(), "okay" | "o" | "k"))
    })
}

/// Check a transcript for the wake phrase, trying every configured spelling of
/// the name and keeping the best match.
pub fn detect(transcript: &str, cfg: &Config) -> Option<WakeHit> {
    let split = split_words(transcript);
    cfg.wake_names()
        .iter()
        .filter_map(|name| detect_name(&split, cfg, name))
        .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))
}

/// The transcript as normalised words, plus which original token each came from.
struct Split<'a> {
    tokens: Vec<&'a str>,
    words: Vec<String>,
    word_token: Vec<usize>,
}

fn split_words(transcript: &str) -> Split<'_> {
    let tokens: Vec<&str> = transcript.split_whitespace().collect();
    let mut words = Vec::new();
    let mut word_token = Vec::new();
    for (ti, tok) in tokens.iter().enumerate() {
        for w in fuzzy::normalize(tok).split(' ').filter(|w| !w.is_empty()) {
            words.push(w.to_string());
            word_token.push(ti);
        }
    }
    Split { tokens, words, word_token }
}

fn detect_name(text: &Split<'_>, cfg: &Config, name: &str) -> Option<WakeHit> {
    let Split { tokens, words, word_token } = text;
    if words.is_empty() {
        return None;
    }
    let target = fuzzy::squash(name);
    let (score, start, end) = fuzzy::find_name(words, name, 4)?;
    let mut hit = score >= threshold(cfg);

    // "Hey Nova" sometimes arrives glued together as "Heynova".
    if !hit {
        for (i, w) in words.iter().take(3).enumerate() {
            for p in &cfg.wake_prefixes {
                let p = fuzzy::squash(p);
                if let Some(rest) = w.strip_prefix(p.as_str()) {
                    if !rest.is_empty() && strsim::jaro_winkler(rest, &target) >= threshold(cfg) {
                        return Some(WakeHit { score: 1.0, command: rest_of(tokens, word_token, i + 1) });
                    }
                }
            }
        }
        return None;
    }

    // Require "hey"/"ok" directly before the name unless the bare name is allowed.
    let prefixed = start > 0 && prefix_matches(&words[start - 1], cfg);
    if !cfg.allow_name_only && !prefixed {
        hit = false;
    }
    // The name must be near the start: "hey nova ..." not "... I told nova".
    let lead = if prefixed { start - 1 } else { start };
    if lead > 1 {
        hit = false;
    }
    hit.then(|| WakeHit { score, command: rest_of(tokens, word_token, end) })
}

/// How close anything near the start of the transcript is to the name (0–1),
/// ignoring the "hey" requirement. Used for diagnostics and calibration.
pub fn name_score(transcript: &str, cfg: &Config) -> f64 {
    let words: Vec<String> = fuzzy::normalize(transcript).split(' ').filter(|w| !w.is_empty()).map(String::from).collect();
    cfg.wake_names().iter().filter_map(|n| fuzzy::find_name(&words, n, 4)).map(|(s, _, _)| s).fold(0.0, f64::max)
}

/// Lowest sensitivity (0–1) at which this transcript would wake, if any.
pub fn sensitivity_needed(transcript: &str, cfg: &Config) -> Option<f32> {
    (0..=20).map(|i| i as f32 / 20.0).find(|&s| {
        let mut c = cfg.clone();
        c.wake_sensitivity = s;
        detect(transcript, &c).is_some()
    })
}

/// Original text from the token holding word `from` onwards.
fn rest_of(tokens: &[&str], word_token: &[usize], from: usize) -> String {
    let Some(&ti) = word_token.get(from) else { return String::new() };
    // If the name ended mid-token ("nova," / "nova.") skip that token.
    let ti = if from > 0 && word_token[from - 1] == ti { ti + 1 } else { ti };
    tokens
        .get(ti..)
        .unwrap_or_default()
        .join(" ")
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects() {
        let cfg = Config::default();
        let cmd = |t: &str| detect(t, &cfg).map(|h| h.command);
        assert_eq!(cmd("Hey Nova, open Chrome."), Some("open Chrome.".into()));
        assert_eq!(cmd("Hey, Nova. What is the Haber process?"), Some("What is the Haber process?".into()));
        assert_eq!(cmd("hey nova"), Some("".into()));
        assert_eq!(cmd("Hey Noah, go to reddit.com"), Some("go to reddit.com".into()));
        assert_eq!(cmd("Hey, no va. Open Steam."), Some("Open Steam.".into()));
        assert_eq!(cmd("Okay Nova open discord"), Some("open discord".into()));
        assert_eq!(cmd("Heynova open notepad"), Some("open notepad".into()));
        assert_eq!(cmd("I was telling Nova about it"), None);
        assert_eq!(cmd("Nova open chrome"), None);
        assert_eq!(cmd("Thank you."), None);
        assert_eq!(cmd("Hey Steve, how are you"), None);
    }

    #[test]
    fn extra_names_wake_too() {
        let mut cfg = Config::default();
        cfg.wake_extra_names = vec!["no va".into(), "novaa".into()];
        assert!(detect("Hey no va, open chrome", &cfg).is_some());
        assert!(detect("hey novaa open steam", &cfg).is_some());
        assert!(detect("Hey Nova open chrome", &cfg).is_some());
        // Still not fooled by the name being mentioned later in a sentence.
        assert!(detect("I was telling nova about it", &cfg).is_none());
    }

    #[test]
    fn bare_name_only_when_allowed() {
        let mut cfg = Config::default();
        assert!(detect("Nova open chrome", &cfg).is_none());
        cfg.allow_name_only = true;
        assert!(detect("Nova open chrome", &cfg).is_some());
    }
}
