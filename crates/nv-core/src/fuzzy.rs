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
    if q.is_empty() || c.is_empty() {
        return 0.0;
    }
    let (qs, cs) = (q.replace(' ', ""), c.replace(' ', ""));
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
    if cs.starts_with(&qs) && qs.len() >= 4 {
        best = best.max(0.8 + 0.15 * (qs.len() as f64 / cs.len() as f64));
    }

    // Whole-string similarity handles mis-hearings: "spotty fi" -> "spotify".
    best = best.max(strsim::jaro_winkler(&qs, &cs) * 0.97);

    // Best similarity against any single word of the name, for long names
    // where the speaker used one distinctive word ("photoshop").
    if q_words.len() == 1 && qs.len() >= 4 {
        for w in &c_words {
            if w.len() >= 4 {
                best = best.max(strsim::jaro_winkler(&qs, w) * 0.9);
            }
        }
    }
    best
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
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.trim().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
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
    }
}
