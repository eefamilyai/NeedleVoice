//! Alarms, reminders, to-dos and calendar events.
//!
//! Times are *local wall-clock* stamps (year, month, day, hour, minute), not
//! UTC. That is deliberate: "7:30 every weekday" has to mean 7:30 on the
//! clock, whatever the timezone does in between, and comparing against the
//! current time then needs no conversion at all.
//!
//! The store is a small JSON file under `%APPDATA%\NeedleVoice`, so the agent,
//! the settings app and a text editor all see the same list.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fuzzy;

/// A local date and time, ordered chronologically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Stamp {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
}

pub const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
pub const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October",
    "November", "December",
];

impl Stamp {
    pub fn new(year: i32, month: u8, day: u8, hour: u8, minute: u8) -> Stamp {
        Stamp { year, month, day, hour, minute }
    }

    /// The current local time.
    pub fn now() -> Stamp {
        let t = crate::win::local_time();
        Stamp { year: t.year as i32, month: t.month as u8, day: t.day as u8, hour: t.hour as u8, minute: t.minute as u8 }
    }

    /// Days since 1970-01-01 (Howard Hinnant's civil calendar algorithm).
    fn days(self) -> i64 {
        let y = if self.month <= 2 { self.year as i64 - 1 } else { self.year as i64 };
        let m = self.month as i64;
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + self.day as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146097 + doe - 719468
    }

    fn from_days(z: i64) -> (i32, u8, u8) {
        let z = z + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
        ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
    }

    fn minutes(self) -> i64 {
        self.days() * 1440 + self.hour as i64 * 60 + self.minute as i64
    }

    fn from_minutes(m: i64) -> Stamp {
        let days = m.div_euclid(1440);
        let mins = m.rem_euclid(1440);
        let (year, month, day) = Stamp::from_days(days);
        Stamp { year, month, day, hour: (mins / 60) as u8, minute: (mins % 60) as u8 }
    }

    pub fn add_minutes(self, n: i64) -> Stamp {
        Stamp::from_minutes(self.minutes() + n)
    }

    pub fn add_days(self, n: i64) -> Stamp {
        Stamp::from_minutes(self.minutes() + n * 1440)
    }

    pub fn with_time(self, hour: u8, minute: u8) -> Stamp {
        Stamp { hour, minute, ..self }
    }

    /// The same clock time on `other`'s calendar day.
    pub fn on_day_of(self, other: Stamp) -> Stamp {
        Stamp { year: other.year, month: other.month, day: other.day, ..self }
    }

    /// False for dates that don't exist, e.g. 31 February.
    pub fn valid(self) -> bool {
        Stamp::from_days(self.days()) == (self.year, self.month, self.day)
    }

    /// 0 = Sunday, matching Windows' `SYSTEMTIME.wDayOfWeek`.
    pub fn weekday(self) -> u8 {
        ((self.days() + 4).rem_euclid(7)) as u8
    }

    pub fn is_weekend(self) -> bool {
        matches!(self.weekday(), 0 | 6)
    }

    /// True when both fall on the same calendar day.
    pub fn same_day(self, other: Stamp) -> bool {
        (self.year, self.month, self.day) == (other.year, other.month, other.day)
    }

    /// Whole minutes from `self` to `other` (negative when `other` is earlier).
    pub fn minutes_until(self, other: Stamp) -> i64 {
        other.minutes() - self.minutes()
    }

    /// Minutes since midnight.
    pub fn minute_of_day(self) -> i64 {
        self.hour as i64 * 60 + self.minute as i64
    }

    /// Parse the `2026-10-03 07:30` form the settings app shows. A bare date
    /// means midnight.
    pub fn parse_iso(text: &str) -> Option<Stamp> {
        let numbers: Vec<i32> = text
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse::<i32>().ok())
            .collect();
        let (year, month, day, hour, minute) = match numbers.len() {
            3 => (numbers[0], numbers[1], numbers[2], 0, 0),
            4 => (numbers[0], numbers[1], numbers[2], numbers[3], 0),
            5 => (numbers[0], numbers[1], numbers[2], numbers[3], numbers[4]),
            _ => return None,
        };
        let stamp = Stamp::new(year, month as u8, day as u8, hour as u8, minute as u8);
        (stamp.valid() && (1..=12).contains(&stamp.month) && (1..=31).contains(&stamp.day)
            && stamp.hour < 24 && stamp.minute < 60)
            .then_some(stamp)
    }

    /// "7:30 am" — the way it would be said.
    pub fn speak_time(self) -> String {
        let (hour, suffix) = match self.hour {
            0 => (12, "am"),
            1..=11 => (self.hour, "am"),
            12 => (12, "pm"),
            h => (h - 12, "pm"),
        };
        if self.minute == 0 {
            format!("{hour} {suffix}")
        } else {
            format!("{hour}:{:02} {suffix}", self.minute)
        }
    }

    /// "19:30" — for the settings app.
    pub fn clock(self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    /// "2026-10-02".
    pub fn date(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// "today" / "tomorrow" / "Monday, 6 October".
    pub fn speak_date(self, now: Stamp) -> String {
        if self.same_day(now) {
            return "today".into();
        }
        if self.same_day(now.add_days(1)) {
            return "tomorrow".into();
        }
        if self.same_day(now.add_days(-1)) {
            return "yesterday".into();
        }
        let wd = WEEKDAYS[self.weekday() as usize];
        let month = MONTHS[(self.month as usize).saturating_sub(1).min(11)];
        format!("{wd}, {} {month}", self.day)
    }
}

/// "7:30 am tomorrow", "every day at 7:30 am", "every 30 minutes" — the way a
/// person would repeat it back.
pub fn describe_when(at: Stamp, repeat: Repeat, now: Stamp) -> String {
    match repeat {
        Repeat::Every(n) => format!("every {n} minutes"),
        Repeat::Daily => format!("every day at {}", at.speak_time()),
        Repeat::Weekdays => format!("weekdays at {}", at.speak_time()),
        Repeat::Weekly => format!("every {} at {}", WEEKDAYS[at.weekday() as usize], at.speak_time()),
        Repeat::Monthly => format!("the {} of every month at {}", at.day, at.speak_time()),
        Repeat::Once if at.same_day(now) => at.speak_time(),
        Repeat::Once => format!("{} {}", at.speak_time(), at.speak_date(now)),
    }
}

/// What to say when something goes off.
pub fn announcement(item: &Item, now: Stamp) -> String {
    let time = item.at.map(|at| at.speak_time()).unwrap_or_default();
    let text = item.text.trim().trim_end_matches('.').to_string();
    match item.kind {
        ItemKind::Alarm => {
            if text.is_empty() || text.eq_ignore_ascii_case("alarm") {
                format!("Alarm! It's {time}.")
            } else {
                format!("Alarm! It's {time}. {text}.")
            }
        }
        ItemKind::Reminder => {
            if text.is_empty() {
                "You asked me to remind you of something, but the note is empty.".into()
            } else {
                format!("Reminder: {text}.")
            }
        }
        ItemKind::Event => {
            let _ = now;
            if text.is_empty() {
                format!("Something on your calendar at {time}.")
            } else {
                format!("Coming up at {time}: {text}.")
            }
        }
        ItemKind::Todo => format!("To do: {text}."),
    }
}

/// How often something repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    Once,
    Daily,
    /// Monday to Friday.
    Weekdays,
    /// The same weekday as the first occurrence.
    Weekly,
    /// The same day of the month as the first occurrence.
    Monthly,
    /// Every n minutes, counted from the first (or last) occurrence.
    Every(u32),
}

impl Repeat {
    pub const ALL: [Repeat; 5] = [Repeat::Once, Repeat::Daily, Repeat::Weekdays, Repeat::Weekly, Repeat::Monthly];

    pub fn label(self) -> &'static str {
        match self {
            Repeat::Once => "Once",
            Repeat::Daily => "Every day",
            Repeat::Weekdays => "Weekdays (Mon–Fri)",
            Repeat::Weekly => "Every week",
            Repeat::Monthly => "Every month",
            Repeat::Every(_) => "Every n minutes",
        }
    }

    pub fn is_repeating(self) -> bool {
        !matches!(self, Repeat::Once)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Alarm,
    Reminder,
    Todo,
    /// Something on the calendar: it shows up in the agenda but stays quiet.
    Event,
}

impl ItemKind {
    pub const ALL: [ItemKind; 4] = [ItemKind::Alarm, ItemKind::Reminder, ItemKind::Todo, ItemKind::Event];

    pub fn label(self) -> &'static str {
        match self {
            ItemKind::Alarm => "Alarm",
            ItemKind::Reminder => "Reminder",
            ItemKind::Todo => "To-do",
            ItemKind::Event => "Event",
        }
    }

    /// Whether this kind is expected to have a time.
    pub fn timed(self) -> bool {
        !matches!(self, ItemKind::Todo)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Item {
    pub id: u32,
    pub kind: ItemKind,
    /// What to say: "take the pizza out".
    pub text: String,
    /// When it goes off (local). `None` for a to-do with no date.
    pub at: Option<Stamp>,
    pub repeat: Repeat,
    /// Finished to-dos stay in the list, ticked.
    pub done: bool,
    /// The occurrence this last went off for.
    pub fired: Option<Stamp>,
    /// A function of yours to run when it goes off.
    pub action: String,
    /// Say it out loud when it goes off.
    pub speak: bool,
    /// Play the chime first.
    pub chime: bool,
    pub created: Stamp,
}

impl Default for Item {
    fn default() -> Self {
        Self {
            id: 0,
            kind: ItemKind::Reminder,
            text: String::new(),
            at: None,
            repeat: Repeat::Once,
            done: false,
            fired: None,
            action: String::new(),
            speak: true,
            chime: false,
            created: Stamp::now(),
        }
    }
}

impl Item {
    /// A one-line description, e.g. "Alarm today at 7:30 am (every day) — Wake up".
    pub fn describe(&self, now: Stamp) -> String {
        let when = match self.at {
            Some(at) => {
                let repeat = self.repeat_label();
                let repeat = if repeat.is_empty() { repeat } else { format!(" {repeat}") };
                format!("{} at {}{repeat}", at.speak_date(now), at.speak_time())
            }
            None => String::new(),
        };
        let action = if self.action.is_empty() { String::new() } else { format!(" → {}", self.action) };
        let text = if self.text.is_empty() { String::new() } else { format!(": {}", self.text) };
        format!("{} {when}{text}{action}", self.kind.label()).trim_end().to_string()
    }

    /// Should this go off now?
    pub fn due(&self, now: Stamp) -> bool {
        if self.done {
            return false;
        }
        let Some(at) = self.at else { return false };
        let fired_today = self.fired.is_some_and(|f| f.same_day(now));
        let time_reached = now.minute_of_day() >= at.minute_of_day();
        match self.repeat {
            Repeat::Once => self.fired.is_none() && now >= at,
            Repeat::Daily => !fired_today && time_reached,
            Repeat::Weekdays => !fired_today && (1..=5).contains(&now.weekday()) && time_reached,
            Repeat::Weekly => !fired_today && now.weekday() == at.weekday() && time_reached,
            Repeat::Monthly => !fired_today && now.day == at.day && time_reached,
            Repeat::Every(n) => match self.fired {
                None => now >= at,
                Some(last) => last.minutes_until(now) >= n.max(1) as i64,
            },
        }
    }

    /// The next time this will go off, for showing in the list.
    pub fn next_after(&self, now: Stamp) -> Option<Stamp> {
        if self.done {
            return None;
        }
        let at = self.at?;
        match self.repeat {
            Repeat::Once => (self.fired.is_none() && at > now).then_some(at),
            Repeat::Daily => {
                let today = at.on_day_of(now);
                Some(if today > now { today } else { today.add_days(1) })
            }
            Repeat::Weekdays | Repeat::Weekly => {
                // A weekly item repeats on the weekday of its *anchor*, which is
                // not always the weekday of the day currently being tried: a
                // reminder set for Monday must not come back as Tuesday just
                // because Tuesday happens to follow today.
                let want = (self.repeat == Repeat::Weekly).then(|| at.weekday());
                let mut day = now;
                for _ in 0..400 {
                    let right_day = want.map_or(!day.is_weekend(), |w| day.weekday() == w);
                    let candidate = at.on_day_of(day);
                    if right_day && candidate > now {
                        return Some(candidate);
                    }
                    day = day.add_days(1);
                }
                None
            }
            Repeat::Monthly => {
                let mut month = now.month as i32;
                let mut year = now.year;
                for _ in 0..24 {
                    let candidate = Stamp::new(year, month as u8, at.day, at.hour, at.minute);
                    if candidate.valid() && candidate > now {
                        return Some(candidate);
                    }
                    month += 1;
                    if month > 12 {
                        month = 1;
                        year += 1;
                    }
                }
                None
            }
            // The next one is always one interval after the last; when that
            // moment has passed (the agent was off), it is due right now.
            Repeat::Every(n) => Some(match self.fired {
                None => at.max(now),
                Some(last) => last.add_minutes(n.max(1) as i64).max(now),
            }),
        }
    }

    /// How the repeat reads in a list, alongside the kind and the wording.
    pub fn repeat_label(&self) -> String {
        match self.repeat {
            Repeat::Once => String::new(),
            Repeat::Daily => "(every day)".into(),
            Repeat::Weekdays => "(weekdays)".into(),
            Repeat::Weekly => match self.at {
                Some(at) => format!("(every {})", WEEKDAYS[at.weekday() as usize]),
                None => "(every week)".into(),
            },
            Repeat::Monthly => "(monthly)".into(),
            Repeat::Every(n) => format!("(every {n} min)"),
        }
    }

    /// "7:30 am tomorrow" — the way the time would be said, or nothing at all
    /// for a to-do with no date. Derived from the item rather than the resolved
    /// next occurrence, so a repeating item does not claim to be "today" when
    /// it is not.
    pub fn time_phrase(&self, now: Stamp) -> String {
        match self.at {
            Some(at) if self.kind == ItemKind::Todo => format!(" by {}", at.speak_date(now)),
            Some(at) => format!(" {} at {}", at.speak_date(now), at.speak_time()),
            None => String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub next_id: u32,
    pub items: Vec<Item>,
}

impl Schedule {
    /// Where the list lives. `NV_SCHEDULE_FILE` overrides it, which tests and
    /// a portable install both use.
    pub fn path() -> PathBuf {
        if let Some(p) = std::env::var_os("NV_SCHEDULE_FILE") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
        crate::paths::data_dir().join("schedule.json")
    }

    pub fn open() -> Schedule {
        Schedule::load_from(&Schedule::path())
    }

    pub fn load_from(path: &Path) -> Schedule {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_else(|e| {
                log::warn!("couldn't read {}: {e}", path.display());
                Schedule::default()
            }),
            Err(_) => Schedule::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&Schedule::path())
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, path)
    }

    /// Add an item and return its id (the caller saves).
    pub fn add(&mut self, item: Item) -> u32 {
        self.next_id += 1;
        let id = self.next_id;
        self.items.push(Item { id, ..item });
        id
    }

    pub fn get(&self, id: u32) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn remove(&mut self, id: u32) -> Option<Item> {
        let at = self.items.iter().position(|i| i.id == id)?;
        Some(self.items.remove(at))
    }

    /// Throw away everything of one kind, or all of it. Returns what went, so
    /// the reply can say what was removed.
    pub fn clear(&mut self, kind: Option<ItemKind>) -> Vec<Item> {
        let (gone, keep): (Vec<Item>, Vec<Item>) = self.items.drain(..).partition(|i| kind.is_none_or(|k| i.kind == k));
        self.items = keep;
        gone
    }

    pub fn set_done(&mut self, id: u32, done: bool) -> Option<&Item> {
        let now = Stamp::now();
        let item = self.items.iter_mut().find(|i| i.id == id)?;
        item.done = done;
        item.fired = done.then_some(now);
        Some(item)
    }

    pub fn mark_fired(&mut self, id: u32, at: Stamp) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.fired = Some(at);
        }
    }

    /// Everything worth showing: unfinished, and not a spent one-off.
    pub fn pending(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(|i| !i.done && (i.repeat.is_repeating() || i.fired.is_none()))
    }

    pub fn todos(&self, include_done: bool) -> Vec<&Item> {
        self.items.iter().filter(|i| i.kind == ItemKind::Todo && (include_done || !i.done)).collect()
    }

    /// The soonest thing coming up.
    pub fn next_up(&self, now: Stamp) -> Option<(Stamp, &Item)> {
        self.pending().filter_map(|i| i.next_after(now).map(|t| (t, i))).min_by_key(|(t, _)| *t)
    }

    /// The next `n` things coming up, soonest first.
    pub fn soonest(&self, now: Stamp, n: usize) -> Vec<&Item> {
        let mut timed: Vec<(Stamp, &Item)> =
            self.pending().filter_map(|i| i.next_after(now).map(|t| (t, i))).collect();
        timed.sort_by_key(|(t, _)| *t);
        timed.into_iter().take(n).map(|(_, i)| i).collect()
    }

    /// Items that should go off now, oldest first.
    pub fn due(&self, now: Stamp) -> Vec<&Item> {
        let mut out: Vec<&Item> = self.pending().filter(|i| i.due(now)).collect();
        out.sort_by_key(|i| i.at);
        out
    }

    /// Today's agenda, plus anything overdue.
    pub fn agenda(&self, now: Stamp) -> Vec<&Item> {
        let mut out: Vec<&Item> =
            self.pending().filter(|i| i.at.is_some_and(|at| at.same_day(now) || at <= now)).collect();
        out.sort_by_key(|i| i.at);
        out
    }

    /// Best match for "cancel the dentist reminder". A time mentioned in the
    /// query wins over a weak text match.
    pub fn find(&self, query: &str) -> Option<&Item> {
        let q = fuzzy::normalize(query);
        if q.is_empty() {
            return None;
        }
        let q_time = crate::schedule_parse::parse_time_only(&q);
        let q_words: Vec<&str> = q
            .split(' ')
            .filter(|w| !crate::schedule_parse::is_time_word(w) && !["alarm", "reminder", "todo", "event", "my", "the", "a"].contains(w))
            .collect();
        let mut best: Option<(f64, &Item)> = None;
        for item in self.pending() {
            let text = fuzzy::normalize(&item.text);
            let mut score = if q_words.is_empty() {
                0.5
            } else {
                let needle = q_words.join(" ");
                if text.contains(&needle) || fuzzy::squash(&text).contains(&fuzzy::squash(&needle)) {
                    1.0
                } else {
                    // One distinctive word is enough ("dentist"), but a long
                    // query shouldn't match because one word got close.
                    let best = q_words
                        .iter()
                        .flat_map(|w| text.split(' ').map(move |t| strsim::jaro_winkler(w, t)))
                        .fold(0.0, f64::max);
                    best - 0.1 * (q_words.len().saturating_sub(1) as f64).min(3.0)
                }
            };
            // A time in the query only has to agree on the clock face: "the 7am
            // alarm" should also match one stored as 07:00.
            if let (Some((want_h, want_m)), Some(at)) = (q_time, item.at) {
                if want_h % 12 == at.hour % 12 && want_m == at.minute {
                    score += 0.5;
                }
            }
            if score > best.map_or(0.0, |(b, _)| b) {
                best = Some((score, item));
            }
        }
        best.filter(|(s, _)| *s >= 0.8).map(|(_, i)| i)
    }

    /// The whole list as iCalendar text, for importing into a phone, Outlook
    /// or Google Calendar. Times are written as floating local times, which is
    /// what a calendar app expects from a desktop reminder.
    pub fn to_ics(&self, now: Stamp) -> String {
        let mut out = String::from(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//NeedleVoice//NeedleVoice//EN\r\nCALSCALE:GREGORIAN\r\n",
        );
        for item in self.items.iter().filter(|i| !i.done && i.at.is_some()) {
            let at = item.at.unwrap();
            let summary = if item.text.trim().is_empty() { item.kind.label().to_string() } else { item.text.clone() };
            out.push_str("BEGIN:VEVENT\r\n");
            out.push_str(&format!("UID:nv-{}@needlevoice\r\n", item.id));
            out.push_str(&format!("DTSTAMP:{}\r\n", ics_time(now)));
            out.push_str(&format!("DTSTART:{}\r\n", ics_time(at)));
            out.push_str(&format!("SUMMARY:{}\r\n", ics_escape(&summary)));
            out.push_str(&format!("DESCRIPTION:{}\r\n", ics_escape(item.kind.label())));
            if let Some(rule) = ics_rule(item) {
                out.push_str(&format!("RRULE:{rule}\r\n"));
            }
            out.push_str("END:VEVENT\r\n");
        }
        out.push_str("END:VCALENDAR\r\n");
        out
    }

    /// The sentence for "what's on my schedule".
    pub fn spoken_summary(&self, now: Stamp, kind: Option<ItemKind>) -> String {
        let items: Vec<&Item> = match kind {
            Some(ItemKind::Todo) => self.todos(false),
            Some(k) => self.pending().filter(|i| i.kind == k).collect(),
            None => {
                let mut v: Vec<&Item> = self.agenda(now);
                v.extend(self.todos(false));
                // Asked late at night, everything timed has already rolled into
                // tomorrow and today's agenda is empty — which used to be
                // reported as "your schedule is clear" with a reminder sitting
                // there due at 4 pm. Fill the list out with what is coming.
                if v.len() < 6 {
                    for item in self.soonest(now, 6) {
                        if !v.iter().any(|seen| std::ptr::eq(*seen, item)) {
                            v.push(item);
                        }
                    }
                }
                v
            }
        };
        if items.is_empty() {
            return match kind {
                Some(k) => format!("You have no {}s.", k.label().to_lowercase()),
                None => "Your schedule is clear.".into(),
            };
        }
        let lines: Vec<String> =
            items.iter().take(6).map(|item| format!("{}{}: {}", item.kind.label(), item.time_phrase(now), item.text)).collect();
        let more = items.len().saturating_sub(6);
        let tail = if more > 0 { format!(" And {more} more.") } else { String::new() };
        format!("{}.{tail}", lines.join(". "))
    }
}

/// `20261003T073000` — floating local time.
fn ics_time(s: Stamp) -> String {
    format!("{:04}{:02}{:02}T{:02}{:02}00", s.year, s.month, s.day, s.hour, s.minute)
}

fn ics_rule(item: &Item) -> Option<String> {
    let at = item.at?;
    Some(match item.repeat {
        Repeat::Once => return None,
        Repeat::Daily => "FREQ=DAILY".into(),
        Repeat::Weekdays => "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".into(),
        Repeat::Weekly => {
            let days = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"];
            format!("FREQ=WEEKLY;BYDAY={}", days[at.weekday() as usize])
        }
        Repeat::Monthly => format!("FREQ=MONTHLY;BYMONTHDAY={}", at.day),
        Repeat::Every(n) => format!("FREQ=MINUTELY;INTERVAL={}", n.max(1)),
    })
}

fn ics_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(y: i32, mo: u8, d: u8, h: u8, mi: u8) -> Stamp {
        Stamp::new(y, mo, d, h, mi)
    }

    fn item(kind: ItemKind, text: &str, at: Stamp, repeat: Repeat) -> Item {
        Item { kind, text: text.into(), at: Some(at), repeat, ..Default::default() }
    }

    #[test]
    fn a_byte_order_mark_is_ignored() {
        let dir = std::env::temp_dir().join("nv-bom-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("bom-{}.json", std::process::id()));
        std::fs::write(&path, "\u{feff}{\"next_id\":1,\"items\":[]}").unwrap();
        assert_eq!(Schedule::load_from(&path).next_id, 1);
        let _ = std::fs::remove_file(&path);

        let cfg_path = dir.join(format!("bom-{}.toml", std::process::id()));
        std::fs::write(&cfg_path, "\u{feff}agent_name = \"Nova\"\n").unwrap();
        assert_eq!(crate::Config::load_from(&cfg_path).agent_name, "Nova");
        let _ = std::fs::remove_file(&cfg_path);
    }

    #[test]
    fn exports_a_calendar() {
        let now = stamp(2026, 10, 2, 8, 0);
        let mut s = Schedule::default();
        s.add(item(ItemKind::Alarm, "wake up", stamp(2026, 10, 3, 7, 30), Repeat::Weekdays));
        s.add(item(ItemKind::Reminder, "rent, and\nother things", stamp(2026, 10, 15, 8, 0), Repeat::Monthly));
        s.add(Item { kind: ItemKind::Todo, text: "buy milk".into(), ..Default::default() });
        let ics = s.to_ics(now);
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(ics.contains("DTSTART:20261003T073000\r\n"));
        assert!(ics.contains("RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR\r\n"));
        assert!(ics.contains("RRULE:FREQ=MONTHLY;BYMONTHDAY=15\r\n"));
        assert!(ics.contains("SUMMARY:rent\\, and\\nother things\r\n"), "{ics}");
        // A to-do with no date is not a calendar entry.
        assert!(!ics.contains("buy milk"));
        assert_eq!(ics.matches("BEGIN:VEVENT").count(), 2);
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
    }

    #[test]
    fn parses_the_editor_format() {
        assert_eq!(Stamp::parse_iso("2026-10-03 07:30"), Some(stamp(2026, 10, 3, 7, 30)));
        assert_eq!(Stamp::parse_iso("2026-10-03"), Some(stamp(2026, 10, 3, 0, 0)));
        assert_eq!(Stamp::parse_iso("2026-10-03 7"), Some(stamp(2026, 10, 3, 7, 0)));
        assert_eq!(Stamp::parse_iso("banana"), None);
        assert_eq!(Stamp::parse_iso("2026-02-31 10:00"), None);
        assert_eq!(Stamp::parse_iso("2026-10-03 25:00"), None);
    }

    #[test]
    fn calendar_arithmetic() {
        // 2026-10-02 is a Friday.
        let d = stamp(2026, 10, 2, 8, 0);
        assert_eq!(d.weekday(), 5);
        assert_eq!(d.add_days(1).date(), "2026-10-03");
        assert_eq!(d.add_days(-1).date(), "2026-10-01");
        assert_eq!(d.add_minutes(90).clock(), "09:30");
        assert_eq!(stamp(2026, 12, 31, 23, 30).add_minutes(45).date(), "2027-01-01");
        assert_eq!(stamp(2026, 2, 28, 0, 0).add_days(1).date(), "2026-03-01"); // 2026 is not a leap year
        assert_eq!(stamp(2028, 2, 28, 0, 0).add_days(1).date(), "2028-02-29");
        assert_eq!(stamp(2026, 10, 2, 23, 59).add_minutes(1).day, 3);
        assert!(stamp(2026, 2, 31, 0, 0).valid() == false);
        assert!(stamp(2026, 2, 28, 0, 0).valid());
    }

    #[test]
    fn speaks_times_and_dates() {
        let now = stamp(2026, 10, 2, 8, 0);
        assert_eq!(stamp(2026, 10, 2, 7, 30).speak_time(), "7:30 am");
        assert_eq!(stamp(2026, 10, 2, 19, 5).speak_time(), "7:05 pm");
        assert_eq!(stamp(2026, 10, 2, 0, 0).speak_time(), "12 am");
        assert_eq!(stamp(2026, 10, 2, 12, 0).speak_time(), "12 pm");
        assert_eq!(stamp(2026, 10, 2, 7, 30).speak_date(now), "today");
        assert_eq!(stamp(2026, 10, 3, 7, 30).speak_date(now), "tomorrow");
        assert_eq!(stamp(2026, 10, 5, 7, 30).speak_date(now), "Monday, 5 October");
    }

    #[test]
    fn one_off_fires_once() {
        let now = stamp(2026, 10, 2, 8, 0);
        let mut it = item(ItemKind::Alarm, "wake up", stamp(2026, 10, 2, 7, 30), Repeat::Once);
        assert!(it.due(now));
        it.fired = Some(stamp(2026, 10, 2, 7, 30));
        assert!(!it.due(now));
        assert!(it.next_after(now).is_none());
        // Not due before its time.
        let later = item(ItemKind::Alarm, "wake up", stamp(2026, 10, 2, 9, 0), Repeat::Once);
        assert!(!later.due(now));
        assert_eq!(later.next_after(now), Some(stamp(2026, 10, 2, 9, 0)));
    }

    #[test]
    fn daily_fires_once_a_day() {
        let now = stamp(2026, 10, 2, 8, 0);
        let mut it = item(ItemKind::Alarm, "wake up", stamp(2026, 10, 2, 7, 30), Repeat::Daily);
        assert!(it.due(now));
        it.fired = Some(now);
        assert!(!it.due(now), "must not fire twice in one day");
        // Tomorrow at 7:30 is after today's 8:00, so it's due again tomorrow.
        let tomorrow = stamp(2026, 10, 3, 7, 31);
        assert!(it.due(tomorrow));
        it.fired = Some(tomorrow);
        let next = it.next_after(tomorrow).unwrap();
        assert_eq!(next.date(), "2026-10-04");
        assert_eq!(next.clock(), "07:30");
    }

    #[test]
    fn weekday_alarms_skip_the_weekend() {
        let friday = stamp(2026, 10, 2, 9, 0);
        let mut it = item(ItemKind::Alarm, "standup", stamp(2026, 10, 1, 9, 0), Repeat::Weekdays);
        assert!(it.due(friday));
        it.fired = Some(friday);
        assert!(!it.due(friday), "already fired today");
        let saturday = stamp(2026, 10, 3, 9, 0);
        assert!(!it.due(saturday), "not a weekday");
        let monday = stamp(2026, 10, 5, 9, 0);
        assert!(it.due(monday));
        assert_eq!(it.next_after(saturday).unwrap().date(), "2026-10-05");
    }

    #[test]
    fn weekly_and_monthly() {
        let now = stamp(2026, 10, 2, 12, 0);
        // Anchored on a Friday 9:00.
        let weekly = item(ItemKind::Reminder, "payday", stamp(2026, 9, 25, 9, 0), Repeat::Weekly);
        assert_eq!(weekly.next_after(now).unwrap().date(), "2026-10-09");
        let monthly = item(ItemKind::Reminder, "rent", stamp(2026, 9, 15, 8, 0), Repeat::Monthly);
        assert_eq!(monthly.next_after(now).unwrap().date(), "2026-10-15");
        // 31st: February and April are skipped.
        let thirty_first = item(ItemKind::Reminder, "invoice", stamp(2026, 1, 31, 9, 0), Repeat::Monthly);
        assert_eq!(thirty_first.next_after(stamp(2026, 2, 1, 0, 0)).unwrap().date(), "2026-03-31");
    }

    /// A weekly item repeats on the weekday it was anchored to, whichever day
    /// the question is asked on. Asking on a Tuesday about a Monday reminder
    /// used to answer "today at 09:00", which is both wrong and one day early.
    #[test]
    fn a_weekly_item_keeps_its_own_weekday() {
        // Monday 2026-09-28 at 09:00, asked about on each later day that week.
        let monday = item(ItemKind::Reminder, "standup", stamp(2026, 9, 28, 9, 0), Repeat::Weekly);
        // Tuesday 29 September through Sunday 4 October, all of which must
        // answer with the following Monday.
        for (month, day) in [(9u8, 29u8), (9, 30), (10, 1), (10, 2), (10, 3), (10, 4)] {
            let now = stamp(2026, month, day, 12, 0);
            assert_eq!(monday.next_after(now).unwrap().date(), "2026-10-05", "asked on 2026-{month:02}-{day:02}");
        }
        // Late on the anchor day itself, the next one is a week away.
        assert_eq!(monday.next_after(stamp(2026, 9, 28, 10, 0)).unwrap().date(), "2026-10-05");
        // Before the time on the anchor day, it is still today.
        assert_eq!(monday.next_after(stamp(2026, 9, 28, 8, 0)).unwrap().date(), "2026-09-28");
        // And it is never "due" on the wrong weekday.
        assert!(monday.due(stamp(2026, 9, 29, 9, 0)) == false, "a Monday reminder must not fire on Tuesday");
    }

    #[test]
    fn every_n_minutes() {
        let start = stamp(2026, 10, 2, 8, 0);
        // The parser anchors "every 30 minutes" on now + 30.
        let mut it = item(ItemKind::Reminder, "stretch", start.add_minutes(30), Repeat::Every(30));
        assert!(!it.due(start.add_minutes(29)));
        assert!(it.due(start.add_minutes(30)));
        it.fired = Some(start.add_minutes(30));
        assert!(!it.due(start.add_minutes(45)));
        assert!(it.due(start.add_minutes(60)));
        assert_eq!(it.next_after(start.add_minutes(45)).unwrap(), start.add_minutes(60));
    }

    #[test]
    fn announcements() {
        let now = stamp(2026, 10, 2, 7, 30);
        let alarm = item(ItemKind::Alarm, "wake up", now, Repeat::Once);
        assert_eq!(announcement(&alarm, now), "Alarm! It's 7:30 am. wake up.");
        let bare = item(ItemKind::Alarm, "alarm", now, Repeat::Once);
        assert_eq!(announcement(&bare, now), "Alarm! It's 7:30 am.");
        let reminder = item(ItemKind::Reminder, "take the pizza out.", now, Repeat::Once);
        assert_eq!(announcement(&reminder, now), "Reminder: take the pizza out.");
        let event = item(ItemKind::Event, "team lunch", stamp(2026, 10, 2, 12, 30), Repeat::Once);
        assert_eq!(announcement(&event, now), "Coming up at 12:30 pm: team lunch.");
    }

    #[test]
    fn clearing_one_kind_leaves_the_rest() {
        let mut s = Schedule::default();
        s.add(Item { kind: ItemKind::Alarm, text: "wake up".into(), ..Default::default() });
        s.add(Item { kind: ItemKind::Alarm, text: "meds".into(), ..Default::default() });
        s.add(Item { kind: ItemKind::Todo, text: "buy milk".into(), ..Default::default() });
        assert_eq!(s.clear(Some(ItemKind::Alarm)).len(), 2);
        assert_eq!(s.items.len(), 1);
        assert_eq!(s.items[0].kind, ItemKind::Todo);
        // "Everything" means everything.
        assert_eq!(s.clear(None).len(), 1);
        assert!(s.items.is_empty());
    }

    #[test]
    fn store_roundtrip_and_queries() {
        let dir = std::env::temp_dir().join("nv-schedule-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("s-{}.json", std::process::id()));
        let now = stamp(2026, 10, 2, 8, 0);

        let mut s = Schedule::default();
        let a = s.add(item(ItemKind::Alarm, "wake up", stamp(2026, 10, 2, 7, 30), Repeat::Daily));
        let r = s.add(item(ItemKind::Reminder, "call the dentist", stamp(2026, 10, 2, 16, 0), Repeat::Once));
        let t = s.add(Item { kind: ItemKind::Todo, text: "buy milk".into(), at: None, ..Default::default() });
        s.add(item(ItemKind::Event, "team lunch", stamp(2026, 10, 2, 12, 30), Repeat::Once));
        s.save_to(&path).unwrap();

        let s2 = Schedule::load_from(&path);
        assert_eq!(s2.items.len(), 4);
        assert_eq!(s2.next_id, 4);
        assert_eq!(s2.next_up(now).unwrap().1.text, "team lunch");
        assert_eq!(s2.due(now).iter().map(|i| i.text.as_str()).collect::<Vec<_>>(), vec!["wake up"]);
        assert_eq!(s2.find("dentist").unwrap().id, r);
        assert_eq!(s2.find("milk").unwrap().id, t);
        assert_eq!(s2.find("alarm at 7:30").unwrap().id, a);
        assert!(s2.find("nothing like this").is_none());
        // The alarm (already due), the event and the reminder are all today.
        assert_eq!(s2.agenda(now).len(), 3);
        assert!(s2.spoken_summary(now, Some(ItemKind::Todo)).contains("buy milk"));

        // Asked late at night, "today" is empty of everything timed and the
        // answer used to be "your schedule is clear" with four things on it.
        let late = stamp(2026, 10, 2, 23, 30);
        let all = s2.spoken_summary(late, None);
        assert!(all.contains("wake up"), "{all}");
        assert!(all.contains("call the dentist"), "{all}");
        assert!(all.contains("team lunch"), "{all}");
        assert!(all.contains("buy milk"), "{all}");
        assert!(!all.contains("clear"), "{all}");
        // A quiet list really is clear, though.
        assert_eq!(Schedule::default().spoken_summary(late, None), "Your schedule is clear.");
        // Marking a to-do done takes it out of the list.
        let mut s3 = s2.clone();
        s3.set_done(t, true);
        assert!(s3.todos(false).is_empty());
        assert_eq!(s3.todos(true).len(), 1);
        // Removing works on the id.
        assert!(s3.remove(a).is_some());
        assert_eq!(s3.items.len(), 3);
        let _ = std::fs::remove_file(&path);
    }
}
