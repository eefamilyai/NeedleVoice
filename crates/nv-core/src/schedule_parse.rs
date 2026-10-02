//! Turning what someone actually said into a time: "in 20 minutes",
//! "tomorrow at 9", "every weekday at 7:30", "next monday at noon".
//!
//! Whisper writes digits for times ("7:30", "7.30", "19:30"), which normalise
//! to "7 30" or "19 30", so this works on normalised words and accepts both
//! spellings. Word numbers ("twenty five") are accepted too, because other
//! sources of text — Windows Voice Typing, for instance — produce them.

use crate::fuzzy;
use crate::schedule::{Repeat, Stamp, WEEKDAYS};

/// The time of day used when only a date was given ("tomorrow").
const DEFAULT_HOUR: u8 = 9;

/// Parse a spoken time. Returns the first occurrence and how it repeats.
pub fn parse(text: &str, now: Stamp) -> Result<(Stamp, Repeat), String> {
    let w = words(text);
    let repeat = parse_repeat(&w);

    // "in 20 minutes" is unambiguous and wins over everything else.
    if let Some(minutes) = parse_relative(&w) {
        let repeat = if matches!(repeat, Repeat::Every(_)) { repeat } else { Repeat::Once };
        return Ok((now.add_minutes(minutes), repeat));
    }
    // "every 30 minutes" with no time of day: anchored on now.
    if let Repeat::Every(n) = repeat {
        return Ok((now.add_minutes(n.max(1) as i64), repeat));
    }

    let day = parse_day(&w, now).or_else(|| repeat_anchor_day(&w, now));
    let clock = parse_clock(&w);
    if clock.is_none() && day.is_none() && repeat == Repeat::Once {
        return Err("I didn't catch a time".into());
    }
    let (hour, minute, pm) = clock.unwrap_or((default_hour_for(&w), 0, None));
    let mut at = resolve(hour, minute, pm, day, now);
    if at <= now {
        if repeat.is_repeating() {
            // "every day at 7:30" said at 8am means tomorrow, not right now.
            at = advance_one_period(at, repeat, now);
        } else if weekday_named(&w).is_some() {
            // "friday at 6am" on a Friday means next Friday.
            at = at.add_days(7);
        } else if w.has("today") || w.has("tonight") {
            return Err(format!("{} has already passed", at.speak_time()));
        } else {
            // "at 7:30 am" said at 8am means tomorrow morning.
            at = at.add_days(1);
        }
    }
    Ok((at, repeat))
}

/// Just the time of day, for matching "cancel my 7am alarm".
pub fn parse_time_only(text: &str) -> Option<(u8, u8)> {
    let w = words(text);
    let (hour, minute, pm) = parse_clock(&w)?;
    match pm {
        Some(true) if hour < 12 => Some((hour + 12, minute)),
        Some(false) if hour == 12 => Some((0, minute)),
        _ => Some((hour, minute)),
    }
}

/// Tokens that belong to a time expression rather than to the item's text.
pub fn is_time_word(word: &str) -> bool {
    const WORDS: [&str; 46] = [
        "at", "in", "on", "for", "past", "half", "quarter", "oclock", "am", "pm", "a", "p", "m", "o", "clock",
        "tomorrow", "today", "tonight", "morning", "afternoon", "evening", "night", "noon", "midnight", "next", "every",
        "day", "daily", "weekday", "weekdays", "weekly", "month", "monthly", "minute", "minutes", "min", "mins", "hour",
        "hours", "hr", "hrs", "second", "seconds", "sec", "and", "the",
    ];
    WORDS.contains(&word)
        || !word.is_empty() && word.chars().all(|c| c.is_ascii_digit())
        || WEEKDAYS.iter().any(|d| d.to_lowercase() == word)
        || number_word(word).is_some()
}

// ── tokens ───────────────────────────────────────────────────────────────

struct Words {
    w: Vec<String>,
}

impl Words {
    fn has(&self, word: &str) -> bool {
        self.w.iter().any(|t| t == word)
    }
    fn find(&self, word: &str) -> Option<usize> {
        self.w.iter().position(|t| t == word)
    }
}

fn words(text: &str) -> Words {
    // "7am" and "7:30pm" arrive glued together, and "a.m." arrives as "a m".
    let mut split: Vec<String> = Vec::new();
    for token in fuzzy::normalize(text).split(' ').filter(|s| !s.is_empty()) {
        let mut current = String::new();
        let mut digits: Option<bool> = None;
        for ch in token.chars() {
            let is_digit = ch.is_ascii_digit();
            match digits {
                Some(d) if d != is_digit => {
                    split.push(std::mem::take(&mut current));
                    digits = Some(is_digit);
                }
                _ => digits = Some(is_digit),
            }
            current.push(ch);
        }
        if !current.is_empty() {
            split.push(current);
        }
    }
    let mut out: Vec<String> = Vec::with_capacity(split.len());
    let mut i = 0;
    while i < split.len() {
        if (split[i] == "a" || split[i] == "p") && split.get(i + 1).map(String::as_str) == Some("m") {
            out.push(format!("{}m", split[i]));
            i += 2;
            continue;
        }
        out.push(split[i].clone());
        i += 1;
    }
    Words { w: out }
}

fn number_word(word: &str) -> Option<u32> {
    Some(match word {
        "zero" | "oh" => 0,
        "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        "thirteen" => 13,
        "fourteen" => 14,
        "fifteen" => 15,
        "sixteen" => 16,
        "seventeen" => 17,
        "eighteen" => 18,
        "nineteen" => 19,
        "twenty" => 20,
        "thirty" => 30,
        "forty" => 40,
        "fifty" => 50,
        _ => return None,
    })
}

/// A number at `i`: digits or spoken, possibly two words ("twenty five").
/// Returns the value and how many tokens it used.
fn number_at(w: &[String], i: usize) -> Option<(u32, usize)> {
    let token = w.get(i)?;
    if let Ok(n) = token.parse::<u32>() {
        return Some((n, 1));
    }
    let first = number_word(token)?;
    if first >= 20 && first % 10 == 0 {
        if let Some(second) = w.get(i + 1).and_then(|t| number_word(t)) {
            if second < 10 && second > 0 {
                return Some((first + second, 2));
            }
        }
    }
    Some((first, 1))
}

// ── pieces ───────────────────────────────────────────────────────────────

/// Minutes from now for "in 20 minutes", "in half an hour", "for 2 hours".
fn parse_relative(w: &Words) -> Option<i64> {
    for i in 0..w.w.len() {
        if !["in", "for", "after"].contains(&w.w[i].as_str()) {
            continue;
        }
        if w.w.get(i + 1).map(String::as_str) == Some("half") {
            let unit = w.w.get(i + 3).or_else(|| w.w.get(i + 2))?;
            return unit_minutes(unit).map(|m| m / 2);
        }
        if matches!(w.w.get(i + 1).map(String::as_str), Some("a" | "an")) {
            if let Some(minutes) = w.w.get(i + 2).and_then(|u| unit_minutes(u)) {
                return Some(minutes);
            }
        }
        let Some((n, used)) = number_at(&w.w, i + 1) else { continue };
        let Some(minutes) = w.w.get(i + 1 + used).and_then(|u| unit_minutes(u)) else { continue };
        let mut total = minutes * n as i64;
        // "in 2 hours 30 minutes"
        if let Some((n2, used2)) = number_at(&w.w, i + 2 + used) {
            if let Some(m2) = w.w.get(i + 2 + used + used2).and_then(|u| unit_minutes(u)) {
                if m2 < total || minutes < 60 {
                    total += m2 * n2 as i64;
                }
            }
        }
        return Some(total);
    }
    // A bare duration, as captured from "wake me up in {when}" — the pattern
    // eats the "in", leaving just "20 minutes".
    let (n, used) = number_at(&w.w, 0)?;
    let minutes = w.w.get(used).and_then(|u| unit_minutes(u))?;
    Some(minutes * n as i64)
}

fn unit_minutes(unit: &str) -> Option<i64> {
    Some(match unit {
        // Sub-minute waits are rounded up: the scheduler ticks on the minute.
        "second" | "seconds" | "sec" | "secs" | "minute" | "minutes" | "min" | "mins" => 1,
        "hour" | "hours" | "hr" | "hrs" => 60,
        _ => return None,
    })
}

/// "every day", "weekdays", "every monday", "every 30 minutes", "monthly".
fn parse_repeat(w: &Words) -> Repeat {
    let every = w.find("every");
    if every.is_none() && !w.has("daily") && !w.has("weekly") && !w.has("monthly") && !w.has("weekdays") {
        return Repeat::Once;
    }
    if let Some(i) = every {
        if let Some((n, used)) = number_at(&w.w, i + 1) {
            if let Some(unit) = w.w.get(i + 1 + used).and_then(|u| unit_minutes(u)) {
                return Repeat::Every((unit * n as i64).clamp(1, 1440) as u32);
            }
        }
        match w.w.get(i + 1).map(String::as_str) {
            Some("hour" | "hours") => return Repeat::Every(60),
            Some("half") => return Repeat::Every(30),
            _ => {}
        }
    }
    if w.has("weekdays") || w.has("weekday") {
        return Repeat::Weekdays;
    }
    if weekday_named(w).is_some() {
        return Repeat::Weekly;
    }
    if w.has("monthly") || w.has("month") {
        return Repeat::Monthly;
    }
    if w.has("weekly") || w.has("week") {
        return Repeat::Weekly;
    }
    Repeat::Daily
}

/// The weekday named in the text, if any: (index 0=Sunday, forced to next week).
fn weekday_named(w: &Words) -> Option<(u8, bool)> {
    for (i, token) in w.w.iter().enumerate() {
        for (d, name) in WEEKDAYS.iter().enumerate() {
            if *token == name.to_lowercase() {
                return Some((d as u8, i > 0 && w.w[i - 1] == "next"));
            }
        }
    }
    None
}

/// The day the text points at, resolving "tomorrow"/"monday" against `now`.
fn parse_day(w: &Words, now: Stamp) -> Option<Stamp> {
    if w.has("tomorrow") {
        return Some(day_of(now.add_days(1)));
    }
    if w.has("today") || w.has("tonight") {
        return Some(day_of(now));
    }
    let (dow, forced_next) = weekday_named(w)?;
    let mut ahead = (dow as i64 - now.weekday() as i64).rem_euclid(7);
    if ahead == 0 && forced_next {
        ahead = 7;
    }
    Some(day_of(now.add_days(ahead)))
}

/// For a weekly repeat, the day to anchor on (its weekday matters).
fn repeat_anchor_day(w: &Words, now: Stamp) -> Option<Stamp> {
    if parse_repeat(w) != Repeat::Weekly {
        return None;
    }
    let (dow, _) = weekday_named(w)?;
    let ahead = (dow as i64 - now.weekday() as i64).rem_euclid(7);
    Some(day_of(now.add_days(ahead)))
}

fn day_of(s: Stamp) -> Stamp {
    s.with_time(0, 0)
}

/// The clock time in the text: "7:30 am", "7 30 pm", "19:30", "noon", "at 7".
/// Returns (hour, minute, pm) where `pm` is None when the speaker didn't say.
fn parse_clock(w: &Words) -> Option<(u8, u8, Option<bool>)> {
    let mer = meridiem(w);
    if w.has("noon") {
        return Some((12, 0, Some(true)));
    }
    if w.has("midnight") {
        return Some((0, 0, Some(false)));
    }
    // "half past seven", "quarter to eight"
    if let Some(i) = w.w.iter().position(|t| t == "half" || t == "quarter") {
        let to = w.w.get(i + 1).map(String::as_str) == Some("to");
        if let Some((n, _)) = number_at(&w.w, i + 2) {
            let half = w.w[i] == "half";
            let raw = n as i32 + if to { -1 } else { 0 };
            let hour = if raw <= 0 { raw + 12 } else { raw } as u8;
            let minute = match (half, to) {
                (true, _) => 30,
                (false, true) => 45,
                (false, false) => 15,
            };
            return Some((hour, minute, mer));
        }
    }
    // "7 30" — two numbers in a row are hours and minutes.
    for i in 0..w.w.len() {
        if i > 0 && matches!(w.w[i - 1].as_str(), "in" | "for" | "every") {
            continue; // part of a duration, not a clock time
        }
        let Some((n, used)) = number_at(&w.w, i) else { continue };
        if used == 1 && (1..=24).contains(&n) {
            if let Some((n2, _)) = number_at(&w.w, i + 1) {
                if n2 < 60 {
                    return Some((n as u8, n2 as u8, mer));
                }
            }
        }
    }
    // A lone hour: "at 7", "7 am".
    for i in 0..w.w.len() {
        if i > 0 && matches!(w.w[i - 1].as_str(), "in" | "for" | "every") {
            continue;
        }
        let Some((n, _)) = number_at(&w.w, i) else { continue };
        if (1..=24).contains(&n) {
            return Some((n as u8, 0, mer));
        }
    }
    // A time of day with no number at all: "in the morning", "tonight".
    match () {
        _ if w.has("morning") => Some((8, 0, Some(false))),
        _ if w.has("afternoon") => Some((14, 0, Some(true))),
        _ if w.has("evening") => Some((18, 0, Some(true))),
        _ if w.has("night") || w.has("tonight") => Some((20, 0, Some(true))),
        _ => None,
    }
}

/// Did the speaker say which half of the day it is?
fn meridiem(w: &Words) -> Option<bool> {
    if w.has("am") {
        return Some(false);
    }
    if w.has("pm") {
        return Some(true);
    }
    if w.has("morning") {
        return Some(false);
    }
    if w.w.iter().any(|t| matches!(t.as_str(), "afternoon" | "evening" | "night" | "tonight")) {
        return Some(true);
    }
    None
}

/// Turn a clock reading into a stamp, using the day if one was named.
fn resolve(hour: u8, minute: u8, pm: Option<bool>, day: Option<Stamp>, now: Stamp) -> Stamp {
    let base = day.unwrap_or(now);
    match pm {
        Some(true) => base.with_time(if hour < 12 { hour + 12 } else { hour }, minute),
        Some(false) => base.with_time(if hour == 12 { 0 } else { hour }, minute),
        // 24-hour reading, or a named day (where morning is the safe reading).
        None if hour >= 13 || day.is_some() => base.with_time(hour, minute),
        // No day and no am/pm: the next time the clock reads that way.
        None => {
            let am = now.with_time(if hour == 12 { 0 } else { hour }, minute);
            let pm_time = now.with_time(if hour == 12 { 12 } else { hour + 12 }, minute);
            if am > now {
                am
            } else if pm_time > now {
                pm_time
            } else {
                am.add_days(1)
            }
        }
    }
}

/// Move a repeating anchor forward by one period so it doesn't fire at once.
fn advance_one_period(at: Stamp, repeat: Repeat, now: Stamp) -> Stamp {
    match repeat {
        Repeat::Daily | Repeat::Weekdays => at.add_days(1),
        Repeat::Weekly => at.add_days(7),
        Repeat::Monthly => {
            let (year, month) = if at.month == 12 { (at.year + 1, 1) } else { (at.year, at.month + 1) };
            let candidate = Stamp::new(year, month, at.day, at.hour, at.minute);
            if candidate.valid() {
                candidate
            } else {
                at.add_days(31)
            }
        }
        Repeat::Every(n) => now.add_minutes(n.max(1) as i64),
        Repeat::Once => at,
    }
}

/// "tonight" and "morning" decide the hour when no clock was given.
fn default_hour_for(w: &Words) -> u8 {
    if w.has("tonight") || w.has("night") {
        20
    } else if w.has("noon") {
        12
    } else if w.has("midnight") {
        0
    } else if w.has("afternoon") {
        14
    } else if w.has("evening") {
        18
    } else if w.has("morning") {
        8
    } else {
        DEFAULT_HOUR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Friday 2 October 2026, 08:00.
    fn now() -> Stamp {
        Stamp::new(2026, 10, 2, 8, 0)
    }

    fn at(text: &str) -> (Stamp, Repeat) {
        parse(text, now()).unwrap_or_else(|e| panic!("{text:?}: {e}"))
    }

    #[test]
    fn relative_times() {
        assert_eq!(at("in 20 minutes").0, now().add_minutes(20));
        assert_eq!(at("in 2 hours").0, now().add_minutes(120));
        assert_eq!(at("in an hour").0, now().add_minutes(60));
        assert_eq!(at("in half an hour").0, now().add_minutes(30));
        assert_eq!(at("in 2 hours 30 minutes").0, now().add_minutes(150));
        assert_eq!(at("in twenty five minutes").0, now().add_minutes(25));
        assert_eq!(at("in 5 minutes").1, Repeat::Once);
        assert_eq!(at("in 90 minutes").0.clock(), "09:30");
        // What a captured "{when}" looks like after "wake me up in {when}".
        assert_eq!(at("20 minutes").0, now().add_minutes(20));
        assert_eq!(at("2 hours").0, now().add_minutes(120));
        assert_eq!(at("1 hour").0, now().add_minutes(60));
    }

    #[test]
    fn clock_times() {
        assert_eq!(at("at 7:30 am").0.clock(), "07:30");
        assert_eq!(at("at 7 30 pm").0.clock(), "19:30");
        assert_eq!(at("at 19:30").0.clock(), "19:30");
        // It is 08:00, so 7:30am has gone: that means tomorrow morning.
        assert_eq!(at("at 7:30 am").0.date(), "2026-10-03");
        assert_eq!(at("at 9 am").0.date(), "2026-10-02");
        assert_eq!(at("at 7 30 a m").0.clock(), "07:30");
        assert_eq!(at("at noon").0.clock(), "12:00");
        assert_eq!(at("at midnight").0.clock(), "00:00");
        // No am/pm: the next seven o'clock — 7am has gone, so 7pm.
        assert_eq!(at("at 7").0.clock(), "19:00");
        let late = parse("at 7", Stamp::new(2026, 10, 2, 22, 0)).unwrap().0;
        assert_eq!((late.date().as_str(), late.clock().as_str()), ("2026-10-03", "07:00"));
        assert_eq!(at("at 6 in the morning").0.clock(), "06:00");
        assert_eq!(at("at 6 in the evening").0.clock(), "18:00");
        assert_eq!(at("half past seven").0.clock(), "19:30");
        assert_eq!(at("quarter to eight am").0.clock(), "07:45");
    }

    #[test]
    fn days_ahead() {
        assert_eq!(at("tomorrow").0.date(), "2026-10-03");
        assert_eq!(at("tomorrow").0.clock(), "09:00");
        assert_eq!(at("tomorrow at 9").0.clock(), "09:00");
        assert_eq!(at("tomorrow at 6 pm").0.clock(), "18:00");
        assert_eq!(at("today at 6 pm").0.date(), "2026-10-02");
        assert_eq!(at("tonight").0.clock(), "20:00");
        assert_eq!(at("monday at 9").0.date(), "2026-10-05");
        assert_eq!(at("next monday at 9").0.date(), "2026-10-05");
        // Today is Friday: "friday at 7 am" has gone, so it means next week.
        assert_eq!(at("friday at 6 am").0.date(), "2026-10-09");
        assert_eq!(at("sunday at 5 pm").0.date(), "2026-10-04");
        assert_eq!(at("tomorrow morning").0.clock(), "08:00");
    }

    #[test]
    fn repeats() {
        assert_eq!(at("every day at 7:30 am").1, Repeat::Daily);
        assert_eq!(at("every day at 7:30 am").0.clock(), "07:30");
        // Said at 08:00, so the first one is tomorrow.
        assert_eq!(at("every day at 7:30 am").0.date(), "2026-10-03");
        assert_eq!(at("daily at 9").1, Repeat::Daily);
        assert_eq!(at("every weekday at 8 am").1, Repeat::Weekdays);
        assert_eq!(at("weekdays at 8 am").1, Repeat::Weekdays);
        assert_eq!(at("every monday at 9").1, Repeat::Weekly);
        assert_eq!(at("every monday at 9").0.date(), "2026-10-05");
        assert_eq!(at("every month on the 15").1, Repeat::Monthly);
        assert_eq!(at("every 30 minutes").1, Repeat::Every(30));
        assert_eq!(at("every hour").1, Repeat::Every(60));
        assert_eq!(at("every half hour").1, Repeat::Every(30));
        assert_eq!(at("every 30 minutes").0, now().add_minutes(30));
        // A weekly alarm for a day that is already past this week moves on.
        assert_eq!(at("every friday at 6 am").0.date(), "2026-10-09");
    }

    #[test]
    fn junk_is_refused() {
        assert!(parse("banana", now()).is_err());
        assert!(parse("", now()).is_err());
        assert!(parse("dentist", now()).is_err());
        // A time that has already gone today means tomorrow.
        assert_eq!(at("at 6 am").0.date(), "2026-10-03");
        assert!(parse("today at 6 am", now()).is_err(), "an explicit past time is a mistake");
    }

    #[test]
    fn a_bare_hour_is_the_next_one() {
        // "reminder to call mum at 4" captured "4" and it became 4 am tomorrow.
        let at = |s: &str| parse(s, Stamp::new(2026, 10, 2, 13, 38)).unwrap().0;
        assert_eq!(at("4").clock(), "16:00", "mid-afternoon, 4 means this afternoon");
        assert_eq!(at("4").date(), "2026-10-02");
        assert_eq!(at("9").clock(), "21:00", "9 am has gone, so 9 tonight");
        // Late at night the only sensible four o'clock is tomorrow morning.
        let late = parse("4", Stamp::new(2026, 10, 2, 22, 30)).unwrap().0;
        assert_eq!((late.date().as_str(), late.clock().as_str()), ("2026-10-03", "04:00"));
    }

    #[test]
    fn time_only_matching() {
        assert_eq!(parse_time_only("cancel my 7am alarm"), Some((7, 0)));
        assert_eq!(parse_time_only("the 7 30 pm one"), Some((19, 30)));
        assert_eq!(parse_time_only("dentist"), None);
        assert!(is_time_word("tomorrow"));
        assert!(is_time_word("7"));
        assert!(!is_time_word("dentist"));
    }
}
