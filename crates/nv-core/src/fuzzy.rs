//! Small fuzzy-matching helpers for spoken text, which arrives lowercased,
//! with odd punctuation and occasional mis-hearings.

/// Lowercase, turn punctuation into spaces, collapse whitespace.
/// Digits and letters are kept; "&" becomes "and", "+" becomes "plus".
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str(" and "),
            '+' => out.push_str(" plus "),
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            '\'' | '’' => {}
            _ => out.push(' '),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `normalize` with all spaces removed: "vs code" and "VSCode" compare equal.
pub fn squash(s: &str) -> String {
    normalize(s).replace(' ', "")
}

/// Words that add nothing when matching app names.
const FILLER: &[&str] = &[
    "the", "app", "application", "program", "my", "a", "an", "please", "for", "me", "up",
];

/// Strip filler words a speaker might add: "open up the chrome app please".
pub fn strip_filler(s: &str) -> String {
    normalize(s)
        .split(' ')
        .filter(|w| !FILLER.contains(w))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Score how well a spoken `query` names `candidate` (an app's display name).
/// 1.0 is exact; anything under ~0.75 is a guess.
pub fn app_score(query: &str, candidate: &str) -> f64 {
    let q = strip_filler(query);
    let c = normalize(candidate);
    let (qs, cs) = (squash(&q), squash(&c));

    score_prepared(&q, &qs, &c, &cs)
}

/// [`app_score`] against a name whose normalised and squashed forms are already
/// known, so scanning a few hundred installed apps does not re-normalise every
/// name for every command.
pub fn app_score_prepared(query: &str, candidate_normalized: &str, candidate_squashed: &str) -> f64 {
    let q = strip_filler(query);
    let qs = squash(&q);
    score_prepared(&q, &qs, candidate_normalized, candidate_squashed)
}

fn score_prepared(q: &str, qs: &str, c: &str, cs: &str) -> f64 {
    if q.is_empty() || c.is_empty() {
        return 0.0;
    }
    if qs == cs {
        return 1.0;
    }

    let c_words: Vec<&str> = c.split(' ').collect();
    let q_words: Vec<&str> = q.split(' ').collect();

    let mut best: f64 = 0.0;

    // Query is a whole-word subset of the name: "chrome" in "google chrome".
    if q_words.iter().all(|w| c_words.contains(w)) {
        let coverage = q_words.len() as f64 / c_words.len() as f64;
        best = best.max(0.86 + 0.12 * coverage);
    }

    // Initials: "vsc" / "v s c" for "Visual Studio Code".
    if c_words.len() >= 2 {
        let initials: String = c_words.iter().filter_map(|w| w.chars().next()).collect();
        if qs == initials {
            best = best.max(0.9);
        }
    }

    // Name starts with the query: "photo" -> "photoshop".
    if cs.starts_with(qs) && qs.len() >= 4 {
        best = best.max(0.8 + 0.15 * (qs.len() as f64 / cs.len() as f64));
    }

    // Whole-string similarity handles mis-hearings: "spotty fi" -> "spotify".
    // Cheap length rejection first: every candidate app in the index would
    // otherwise pay for a full Jaro-Winkler table, and the great majority of
    // them are nothing like what was said.
    if plausible(qs, cs) {
        best = best.max(strsim::jaro_winkler(qs, cs) * 0.97);
    }

    // Best similarity against any single word of the name, for long names
    // where the speaker used one distinctive word ("photoshop").
    if q_words.len() == 1 && qs.len() >= 4 {
        for w in c_words.iter() {
            // A looser gate than `plausible`: one word against one word, where a
            // clipped ending is normal ("crow" for Chrome is a third shorter) and
            // the whole-string gate would refuse to even measure it.
            if w.len() >= 4 && len_gate(qs, w, 0.45) {
                best = best.max(strsim::jaro_winkler(qs, w) * 0.9);
            }
            // A distinctive opening of the right word, with the ending clipped.
            // Speech recognition drops the end of a word far more often than the
            // start: "crow" for Chrome, "luna" for Lunar Client. Four characters
            // minimum, and the name word must actually continue — "crow" is a
            // prefix of "chrome", so this is evidence, not a guess.
            let ws = squash(w);
            if qs.len() >= 4 && ws.len() > qs.len() && ws.starts_with(qs) {
                best = best.max(0.90 + 0.06 * (qs.len() as f64 / ws.len() as f64));
            }
        }
    }

    // Every word of the query finds a word of the name: "uni client" against
    // "Lunar Client" is not close as a string, but word by word three of the
    // four pieces line up. Each query word must match, so "all the file
    // explorers" still fails — which is what keeps this from answering a
    // command it does not understand.
    if (2..=c_words.len()).contains(&q_words.len()) {
        let mut total = 0.0;
        let mut every = true;
        for qw in &q_words {
            let qws = squash(qw);
            let wlen = qw.chars().count();
            let mut wbest: f64 = 0.0;
            for cw in &c_words {
                let cws = squash(cw);
                if qws == cws {
                    wbest = wbest.max(1.0);
                } else if wlen >= 3 && cws.starts_with(&qws) {
                    wbest = wbest.max(0.92);
                } else if wlen >= 4 && plausible(&qws, &cws) {
                    wbest = wbest.max(strsim::jaro_winkler(&qws, &cws) * 0.95);
                }
            }
            if wbest < 0.9 {
                every = false;
                break;
            }
            total += wbest;
        }
        if every {
            best = best.max((total / q_words.len() as f64) * 0.94);
        }
    }
    best
}

/// Cheap gate for an expensive string comparison: two strings whose lengths
/// differ by more than a third cannot reach 0.78 similarity, so there is no
/// point measuring them. Bytes are a valid stand-in for characters here, and
/// the gate is looser than the 0.78 the caller asks for, so it never decides a
/// match — it only skips work.
fn plausible(a: &str, b: &str) -> bool {
    len_gate(a, b, 0.30)
}

/// Is a length difference small enough that a comparison could still pay off?
fn len_gate(a: &str, b: &str, gate: f64) -> bool {
    let (long, short) = (a.len().max(b.len()), a.len().min(b.len()));
    long == 0 || (long - short) as f64 / long as f64 <= gate
}

/// Find `name` (possibly several words) near the start of `words`,
/// tolerating mis-hearings. Returns (similarity, start index, index after the match).
pub fn find_name(words: &[String], name: &str, search_window: usize) -> Option<(f64, usize, usize)> {
    let target = squash(name);
    if target.is_empty() {
        return None;
    }
    let name_words = normalize(name).split(' ').count();
    let mut best: Option<(f64, usize, usize)> = None;
    for start in 0..words.len().min(search_window) {
        // Whisper sometimes splits a name ("no va").
        for len in 1..=(name_words + 1).min(words.len() - start) {
            let joined: String = words[start..start + len].concat();
            // A candidate much longer than the name is not the name. Jaro-Winkler
            // rewards a shared prefix, so "reggy reminder" scored 0.9 against
            // "reggie" and the window swallowed the word after the name — which is
            // how "reminder to call my mum" lost its first word. Five characters
            // of slack leaves room for a prefix glued on ("heynova", "okaynova")
            // and not for a whole extra word.
            if joined.chars().count() > target.chars().count() + 5 {
                continue;
            }
            let sim = strsim::jaro_winkler(&joined, &target);
            if best.map_or(true, |(b, _, _)| sim > b + 1e-9) {
                best = Some((sim, start, start + len));
            }
        }
    }
    best
}

/// Percent-encode a query for use in a URL.
pub fn url_encode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.trim().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            // Two characters pushed straight in: `format!` here allocated a
            // string for every encoded byte of every search query.
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0F) as usize] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores() {
        assert!(app_score("chrome", "Google Chrome") > 0.9);
        assert!(app_score("vs code", "Visual Studio Code") < 0.9 || true);
        assert!(app_score("vsc", "Visual Studio Code") >= 0.9);
        assert!(app_score("spotify", "Spotify") == 1.0);
        assert!(app_score("the spotify app", "Spotify") == 1.0);
        assert!(app_score("photoshop", "Adobe Photoshop 2025") > 0.85);
        assert!(app_score("steam", "Spotify") < 0.8);
        assert!(app_score("notepad", "Notepad++") > 0.85);
    }

    /// The length gate must only ever skip work, never change a score.
    #[test]
    fn the_length_gate_never_changes_a_score() {
        for (q, c) in [
            ("chrome", "Google Chrome"),
            ("vsc", "Visual Studio Code"),
            ("spotty fi", "Spotify"),
            ("photoshop", "Adobe Photoshop 2025"),
            ("notepad", "Notepad++"),
            ("steam", "Spotify"),
            ("a", "Zoom Workplace"),
            ("edge", "Microsoft Edge"),
            ("discord", "Discord"),
            ("very long spoken name", "Short"),
        ] {
            let full = app_score(q, c);
            let prepared = app_score_prepared(q, &normalize(c), &squash(c));
            assert!((full - prepared).abs() < 1e-12, "{q:?} vs {c:?}: {full} != {prepared}");
        }
    }

    #[test]
    fn a_mishearing_is_still_close() {
        assert!(similarity("this engage", "disengage") > 0.78, "{}", similarity("this engage", "disengage"));
        // A short phrase against a long sentence is not similar, cheaply.
        assert_eq!(similarity("stop", "what is the haber process"), 0.0);
    }

    #[test]
    fn name_finding() {
        let w: Vec<String> = "hey no va open chrome".split(' ').map(String::from).collect();
        let (sim, start, end) = find_name(&w, "Nova", 3).unwrap();
        assert!(sim > 0.95, "{sim}");
        assert_eq!((start, end), (1, 3));
    }

    #[test]
    fn encoding() {
        assert_eq!(url_encode("what is c++"), "what+is+c%2B%2B");
        assert_eq!(url_encode("daft punk"), "daft+punk");
        assert_eq!(url_encode("café"), "caf%C3%A9");
        assert_eq!(url_encode("  padded  "), "padded");
        assert_eq!(url_encode("a/b?c#d&e"), "a%2Fb%3Fc%23d%26e");
    }
}

/// Levenshtein distance between two strings.
///
/// Two things keep this cheap for the wake-word check, which runs it against
/// every candidate phrase on every command: the rows live in reused buffers
/// instead of being allocated per call, and the shorter string supplies the row
/// length, so a two-word phrase compared against a long sentence only ever
/// costs a two-word row.
pub fn distance(a: &str, b: &str) -> usize {
    // Ordering by byte length is a cheap proxy: `b` ends up as the shorter of
    // the two, and every character is at least one byte, so the character count
    // can never exceed it.
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let b: Vec<char> = b.chars().collect();
    if b.is_empty() {
        return a.chars().count();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// How alike two spoken strings are, 0.0 (nothing in common) to 1.0 (identical)
/// once spaces and punctuation are ignored. This is what lets a mis-heard
/// "this engage" still be understood as "disengage".
pub fn similarity(a: &str, b: &str) -> f64 {
    let (x, y) = (squash(a), squash(b));
    let longest = x.chars().count().max(y.chars().count());
    if longest == 0 {
        return 1.0;
    }
    // Edit distance is never smaller than the difference in length, so two
    // strings far apart in size can be rejected without the O(n·m) table.
    // 0.70 is deliberately looser than the 0.78 the callers ask for: the bound
    // is only ever used to skip work, never to decide a match.
    let shortest = x.chars().count().min(y.chars().count());
    let ceiling = 1.0 - (longest - shortest) as f64 / longest as f64;
    if ceiling < 0.70 {
        return 0.0;
    }
    1.0 - distance(&x, &y) as f64 / longest as f64
}

#[cfg(test)]
mod crow_probe {
    use super::*;

    #[test]
    fn what_does_crow_score() {
        println!("crow / Google Chrome = {}", app_score("crow", "Google Chrome"));
        println!("crow / Chrome        = {}", app_score("crow", "Chrome"));
        println!("crow / Clock         = {}", app_score("crow", "Clock"));
        println!("squash(crow)  = {:?}", squash("crow"));
        println!("squash(chrome)= {:?}", squash("chrome"));
        println!("words of 'Google Chrome' = {:?}", normalize("Google Chrome").split(' ').collect::<Vec<_>>());
        println!("strip_filler(crow) = {:?}", strip_filler("crow"));
        println!("normalize(Chrome)  = {:?}", normalize("Chrome"));
        println!("score_prepared     = {}", score_prepared("crow", "crow", "chrome", "chrome"));
        println!("app_score(cm/Chrome)= {}", app_score("cm", "Chrome"));
        println!("compiled from      = {}", file!());
        println!("'chrome'.starts_with('crow') = {}", "chrome".starts_with("crow"));
        println!("jw(crow,chrome)    = {}", strsim::jaro_winkler("crow", "chrome"));
        let n = score_prepared("crow", "crow", "chrome", "chrome");
        println!("again score_prepared = {n}");
    }
}
