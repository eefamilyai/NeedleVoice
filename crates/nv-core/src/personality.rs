//! What the assistant says back. Each persona has a handful of lines per
//! situation; one is picked at random so it doesn't sound like a robot.

use serde::{Deserialize, Serialize};

use crate::brain::{Action, MediaKey, VolumeChange};
use crate::config::Config;
use crate::tools;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Persona {
    Cheerful,
    Chill,
    Sarcastic,
    Butler,
}

impl Persona {
    pub const ALL: [Persona; 4] = [Persona::Cheerful, Persona::Chill, Persona::Sarcastic, Persona::Butler];

    pub fn label(self) -> &'static str {
        match self {
            Persona::Cheerful => "Cheerful — upbeat and eager",
            Persona::Chill => "Chill — laid back, few words",
            Persona::Sarcastic => "Sarcastic — helpful, but with attitude",
            Persona::Butler => "Butler — impeccably polite",
        }
    }
}

/// What happened to one action, and what the action runner reported.
pub enum Outcome {
    Done(Action, String),
    Failed(Action, String),
}

/// The sentence to say after running a batch of actions.
pub fn reply(cfg: &Config, outcomes: &[Outcome]) -> String {
    // One failure is more worth mentioning than any number of successes.
    if let Some(Outcome::Failed(action, why)) = outcomes.iter().find(|o| matches!(o, Outcome::Failed(..))) {
        return line(cfg.personality, &Moment::Failed(action, why));
    }
    match outcomes {
        [Outcome::Done(action, message)] => single(cfg, action, message),
        many => line(cfg.personality, &Moment::Multi(many.len())),
    }
}

/// A lone success: the user's own wording wins, then answers, then a line.
fn single(cfg: &Config, action: &Action, message: &str) -> String {
    if let Action::Custom { name, params } = action {
        if let Some(tool) = cfg.custom_tools.iter().find(|t| &t.name == name) {
            if !tool.reply.is_empty() {
                return capitalize(&tools::substitute(&tool.reply, params, false));
            }
        }
        // A script that prints an answer should say it, not "done".
        let prints_answers = cfg.custom_tools.iter().find(|t| &t.name == name).is_some_and(|t| t.kind.has_output());
        if !message.is_empty() && prints_answers {
            return capitalize(message);
        }
        // A function that only speaks still needs something to say.
        if message.is_empty() {
            return capitalize(&format!("{}.", name.replace('_', " ")));
        }
    }
    if action.informational() {
        return capitalize(message);
    }
    line(cfg.personality, &Moment::Done(action))
}

/// Something worth reacting to.
pub enum Moment<'a> {
    /// Heard the wake phrase with no command yet.
    Wake,
    /// The very short "I heard you" said the instant the name is recognised.
    /// Deliberately one word: it plays over the start of the command.
    WakeAck,
    /// An action succeeded.
    Done(&'a Action),
    /// An action failed; carries the reason.
    Failed(&'a Action, &'a str),
    /// Woken but nothing was said.
    Timeout,
    /// Several actions done at once.
    Multi(usize),
}

// Cheap xorshift so we don't pull in a rand crate for picking lines.
thread_local! {
    /// When set, `pick` always takes the first line (stable UI examples).
    static FIRST_LINE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Like [`line`], but always the same wording — for showing an example in a
/// UI that redraws many times a second.
pub fn example(persona: Persona, moment: &Moment) -> String {
    FIRST_LINE.with(|f| f.set(true));
    let l = line(persona, moment);
    FIRST_LINE.with(|f| f.set(false));
    l
}

fn pick<'a>(lines: &[&'a str]) -> &'a str {
    if FIRST_LINE.with(|f| f.get()) {
        return lines[0];
    }
    use std::sync::atomic::{AtomicU64, Ordering};
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut x = STATE.load(Ordering::Relaxed);
    if x == 0 {
        x = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15)
            | 1;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    STATE.store(x, Ordering::Relaxed);
    lines[(x % lines.len() as u64) as usize]
}

/// Shorten long search queries so the reply stays snappy.
fn short(s: &str) -> String {
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.len() <= 6 {
        s.trim().trim_end_matches(['?', '.', '!']).to_string()
    } else {
        format!("{}…", words[..6].join(" "))
    }
}

/// The "{x}" wording for an action. Volume wants the number itself
/// ("Volume at 30%"), everything else reads the way it would be said aloud.
fn reply_subject(a: &Action) -> String {
    match a {
        Action::OpenApp(n) | Action::CloseApp(n) | Action::OpenWebsite(n) => speakable(n),
        Action::WebSearch(q) | Action::YoutubeSearch(q) => short(q),
        Action::Volume(VolumeChange::Set(p)) => format!("{p}%"),
        other => other.subject(),
    }
}

/// Turn "Google Chrome" / "reddit.com" into something nice to say.
fn speakable(s: &str) -> String {
    s.trim().trim_start_matches("https://").trim_start_matches("www.").trim_end_matches('/').to_string()
}

pub fn line(persona: Persona, moment: &Moment) -> String {
    use Persona::*;
    let template: &str = match moment {
        Moment::Wake => pick(match persona {
            Cheerful => &["Hey! What's up?", "I'm all ears!", "Yes? What can I do?", "Hi! Go ahead."],
            Chill => &["Yeah?", "Mm-hm?", "Sup.", "Go for it."],
            Sarcastic => &["What now?", "You rang?", "I'm listening. Unfortunately.", "Oh, it's you."],
            Butler => &["At your service.", "You called?", "How may I assist?", "I'm listening."],
        }),
        Moment::WakeAck => pick(match persona {
            Cheerful => &["Yes?", "Mm-hm?", "Yep?"],
            Chill => &["Mm?", "Yeah?"],
            Sarcastic => &["Hm?", "What?"],
            Butler => &["Sir?", "Madam?"],
        }),
        Moment::Timeout => pick(match persona {
            Cheerful => &["No worries, I'll be here!", "Okay, call me anytime!"],
            Chill => &["All good.", "Later."],
            Sarcastic => &["Riveting conversation.", "Okay, never mind then."],
            Butler => &["Very well. I shall wait.", "Do call if you need me."],
        }),
        Moment::Multi(_) => pick(match persona {
            Cheerful => &["All done! Opened {n} things for you.", "Boom, {n} for {n}!"],
            Chill => &["Done. All {n}.", "{n} things, handled."],
            Sarcastic => &["{n} things at once. Look at you, multitasking.", "Done. All {n}. You're welcome."],
            Butler => &["All {n} tasks completed.", "Done, all {n} of them."],
        }),
        Moment::Done(action) => match action {
            Action::OpenApp(_) => pick(match persona {
                Cheerful => &["Opening {x}!", "{x}, coming right up!", "Here's {x}!", "Launching {x}, let's go!"],
                Chill => &["{x}.", "Opening {x}.", "Here's {x}."],
                Sarcastic => &["Fine, opening {x}.", "{x}. Because clicking was too hard.", "Opening {x}. Try not to get distracted."],
                Butler => &["Opening {x} for you.", "{x}, as requested.", "Right away. {x} is opening."],
            }),
            Action::CloseApp(_) => pick(match persona {
                Cheerful => &["Closed {x}!", "Bye bye, {x}!", "{x} is gone!"],
                Chill => &["Closed {x}.", "{x}'s gone."],
                Sarcastic => &["{x} has left the building.", "Closed {x}. It had a good run."],
                Butler => &["{x} has been closed.", "I've closed {x} for you."],
            }),
            Action::WebSearch(_) => pick(match persona {
                Cheerful => &["Ooh, {x}! Pulling that up.", "Let's find out about {x}!", "Searching {x} for you!"],
                Chill => &["Looking up {x}.", "Here's {x}."],
                Sarcastic => &["Googling {x}. Groundbreaking.", "Let me search that for you. {x}.", "{x}? Sure, I'll look it up."],
                Butler => &["Searching for {x}.", "Here are the results for {x}."],
            }),
            Action::YoutubeSearch(_) => pick(match persona {
                Cheerful => &["YouTube time! {x}.", "Finding {x} on YouTube!"],
                Chill => &["{x}, on YouTube.", "Putting on {x}."],
                Sarcastic => &["{x} on YouTube. Productive.", "Another YouTube session. {x}."],
                Butler => &["Searching YouTube for {x}.", "{x}, on YouTube, as requested."],
            }),
            Action::OpenWebsite(_) => pick(match persona {
                Cheerful => &["Heading to {x}!", "Opening {x}!"],
                Chill => &["{x}.", "Opening {x}."],
                Sarcastic => &["{x}. Again.", "Off to {x}, I guess."],
                Butler => &["Opening {x} for you.", "Navigating to {x}."],
            }),
            Action::Media(MediaKey::Pause) => pick(match persona {
                Cheerful => &["Paused!", "Music's on hold.", "Quiet time!"],
                Chill => &["Paused.", "Held."],
                Sarcastic => &["Paused. Enjoy the silence.", "Fine, it's paused."],
                Butler => &["Paused for you.", "I've paused it."],
            }),
            Action::Media(MediaKey::Resume) => pick(match persona {
                Cheerful => &["Back on!", "Playing again!", "Here we go!"],
                Chill => &["Playing.", "Resumed."],
                Sarcastic => &["Resuming. You're welcome.", "Back to it."],
                Butler => &["Resuming playback.", "Playing it again."],
            }),
            Action::Media(MediaKey::Next) => pick(match persona {
                Cheerful => &["Next one!", "Skipped!"],
                Chill => &["Next.", "Skipped."],
                Sarcastic => &["Skipped. That one was terrible anyway.", "Next. Sure."],
                Butler => &["Skipping ahead.", "The next track, then."],
            }),
            Action::Media(MediaKey::Previous) => pick(match persona {
                Cheerful => &["Back one!", "Previous track!"],
                Chill => &["Previous.", "Went back."],
                Sarcastic => &["Rewinding. Nostalgic today?", "Back you go."],
                Butler => &["Going back a track.", "The previous one, of course."],
            }),
            Action::Media(MediaKey::Stop) => pick(match persona {
                Cheerful => &["Stopped!", "All quiet!"],
                Chill => &["Stopped.", "Done."],
                Sarcastic => &["Stopped. Silence, finally.", "It's off."],
                Butler => &["Stopped.", "Playback has ceased."],
            }),
            Action::Volume(VolumeChange::Set(_)) => pick(match persona {
                Cheerful => &["Volume at {x}!", "Sound is at {x}!"],
                Chill => &["Volume {x}.", "Set to {x}."],
                Sarcastic => &["{x}. Try not to blow your ears out.", "Volume {x}. Fine."],
                Butler => &["The volume is now {x}.", "Set to {x}, as you asked."],
            }),
            Action::Volume(VolumeChange::Up) => pick(match persona {
                Cheerful => &["Louder it is!", "Turned it up!"],
                Chill => &["Louder.", "Up."],
                Sarcastic => &["Louder. Your neighbours will love that.", "Up it goes."],
                Butler => &["Turning it up.", "Louder, certainly."],
            }),
            Action::Volume(VolumeChange::Down) => pick(match persona {
                Cheerful => &["Turned it down!", "Softer now!"],
                Chill => &["Quieter.", "Down."],
                Sarcastic => &["Quieter. How considerate.", "Down it goes."],
                Butler => &["Turning it down.", "Quieter, as you wish."],
            }),
            Action::Mute(true) => pick(match persona {
                Cheerful => &["Muted!", "Not a peep!"],
                Chill => &["Muted.", "Silence."],
                Sarcastic => &["Muted. Bliss.", "Silenced."],
                Butler => &["Muted.", "The sound is off."],
            }),
            Action::Mute(false) => pick(match persona {
                Cheerful => &["Sound's back!", "Unmuted!"],
                Chill => &["Unmuted.", "Sound's on."],
                Sarcastic => &["Unmuted. Brace yourself.", "Sound on again."],
                Butler => &["Sound restored.", "Unmuted for you."],
            }),
            // Short: they just told us to go away.
            Action::Disengage => pick(match persona {
                Cheerful => &["Okay!", "No problem!", "Standing by!"],
                Chill => &["Okay.", "Fair enough.", "Standing by."],
                Sarcastic => &["Fine. I'll be here.", "Okay, forget I asked.", "Right. Nothing, then."],
                Butler => &["Very good.", "As you wish.", "Standing by, of course."],
            }),
            Action::LockPc => pick(match persona {
                Cheerful => &["Locking up. See you soon!", "Locked! Stay safe."],
                Chill => &["Locked.", "Locking up."],
                Sarcastic => &["Locked. Going somewhere nice?", "Locked. Don't forget your password."],
                Butler => &["Locking the computer.", "The computer is locked."],
            }),
            Action::ShowDesktop => pick(match persona {
                Cheerful => &["Ta-da! Desktop!", "All clear!"],
                Chill => &["Desktop.", "Cleared."],
                Sarcastic => &["Your desktop. Riveting.", "Hidden. You're welcome."],
                Butler => &["Showing the desktop.", "Windows minimized."],
            }),
            Action::Screenshot => pick(match persona {
                Cheerful => &["Got it! Screenshot saved.", "Snapped!"],
                Chill => &["Saved.", "Screenshot saved."],
                Sarcastic => &["Screenshot saved. That's going in the folder.", "Saved. Say cheese next time."],
                Butler => &["Screenshot saved.", "Captured and saved."],
            }),
            Action::CloseWindow => pick(match persona {
                Cheerful => &["Closed it!", "Gone!"],
                Chill => &["Closed.", "Done."],
                Sarcastic => &["Closed. You're welcome.", "It's gone."],
                Butler => &["Closed.", "The window has been closed."],
            }),
            Action::OpenSettings => pick(match persona {
                Cheerful => &["Opening Settings!", "Here are your settings!"],
                Chill => &["Settings.", "Opening Settings."],
                Sarcastic => &["Settings. Have fun in there.", "Opening Settings. Good luck."],
                Butler => &["Opening Windows Settings.", "Settings, at once."],
            }),
            Action::Custom { .. } => pick(match persona {
                Cheerful => &["Done!", "All sorted!"],
                Chill => &["Done.", "Sorted."],
                Sarcastic => &["Done. Anything else?", "Consider it handled."],
                Butler => &["Done.", "Very good. It's done."],
            }),
            // The schedule actions report the whole answer themselves.
            Action::Alarm { .. } | Action::Reminder { .. } | Action::CalendarEvent { .. } => "{x}.",
            Action::Todo { .. } => "{x}.",
            Action::ShowSchedule { .. } => "{x}.",
            Action::CancelSchedule { .. } => "{x}.",
            Action::CompleteTodo { .. } => "{x}.",
            Action::TellTime | Action::TellDate => "{x}.",
        },
        Moment::Failed(_, _) => pick(match persona {
            Cheerful => &["Hmm, I couldn't do that. {why}.", "Oops! {why}."],
            Chill => &["Nope. {why}.", "Didn't work. {why}."],
            Sarcastic => &["Yeah, that didn't work. {why}.", "Bold of you to assume. {why}."],
            Butler => &["My apologies. {why}.", "I'm afraid I couldn't. {why}."],
        }),
    };

    let x = match moment {
        Moment::Done(a) | Moment::Failed(a, _) => reply_subject(a),
        _ => String::new(),
    };
    let _ = &x;
    let why = match moment {
        Moment::Failed(_, why) => capitalize(why),
        _ => String::new(),
    };
    let n = match moment {
        Moment::Multi(n) => n.to_string(),
        _ => String::new(),
    };
    let out = template.replace("{x}", &x).replace("{why}", &why).replace("{n}", &n);
    // Templates that start with {x} need a capital letter.
    capitalize(&out).replace("..", ".")
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_templates() {
        for p in Persona::ALL {
            let l = line(p, &Moment::Done(&Action::OpenApp("Google Chrome".into())));
            assert!(l.contains("Google Chrome"), "{l}");
            let l = line(p, &Moment::Failed(&Action::CloseApp("x".into()), "x isn't running"));
            assert!(l.contains("X isn't running"), "{l}");
            assert!(!line(p, &Moment::Wake).is_empty());
        }
    }

    #[test]
    fn every_action_has_a_line() {
        let cfg = Config::default();
        let actions = [
            Action::OpenApp("Spotify".into()),
            Action::CloseApp("Spotify".into()),
            Action::WebSearch("the haber process".into()),
            Action::OpenWebsite("reddit.com".into()),
            Action::YoutubeSearch("lofi".into()),
            Action::Media(MediaKey::Pause),
            Action::Media(MediaKey::Resume),
            Action::Media(MediaKey::Next),
            Action::Media(MediaKey::Previous),
            Action::Media(MediaKey::Stop),
            Action::Volume(VolumeChange::Set(30)),
            Action::Volume(VolumeChange::Up),
            Action::Volume(VolumeChange::Down),
            Action::Mute(true),
            Action::Mute(false),
            Action::TellTime,
            Action::TellDate,
            Action::LockPc,
            Action::Disengage,
            Action::ShowDesktop,
            Action::Screenshot,
            Action::CloseWindow,
            Action::OpenSettings,
            Action::Custom { name: "spotify_search".into(), params: vec![] },
        ];
        for a in &actions {
            for p in Persona::ALL {
                let l = line(p, &Moment::Done(a));
                assert!(!l.is_empty() && !l.contains('{'), "{p:?} {a}: {l:?}");
            }
            assert!(!reply(&cfg, &[Outcome::Done(a.clone(), "something".into())]).is_empty(), "{a}");
        }
    }

    #[test]
    fn answers_and_user_wording_win() {
        let cfg = Config::default();
        assert_eq!(reply(&cfg, &[Outcome::Done(Action::TellTime, "it's 3:42 pm".into())]), "It's 3:42 pm");
        let mut cfg = Config::default();
        cfg.custom_tools = crate::tools::sanitize_tools(crate::tools::templates(), &[]);
        let action = Action::Custom { name: "spotify_search".into(), params: vec![("query".into(), "daft punk".into())] };
        assert_eq!(reply(&cfg, &[Outcome::Done(action.clone(), "ran spotify search".into())]), "Searching Spotify for daft punk.");
        // A "just say the reply" function speaks its own text, and a function
        // with no reply of its own still gets a sensible line.
        let joke = Action::Custom { name: "shutdown_pc".into(), params: vec![] };
        assert_eq!(
            reply(&cfg, &[Outcome::Done(joke, "said the reply for shutdown pc".into())]),
            "Shutting down in 30 seconds. Say cancel shutdown to stop it."
        );
        let mute = Action::Custom { name: "no_reply_here".into(), params: vec![] };
        assert!(!reply(&cfg, &[Outcome::Done(mute, String::new())]).is_empty());
        let failed = reply(&cfg, &[Outcome::Done(Action::Screenshot, "saved".into()), Outcome::Failed(action, "no internet".into())]);
        assert!(failed.contains("No internet"), "{failed}");
    }
}
