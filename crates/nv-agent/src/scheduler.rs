//! Fires alarms, reminders and calendar events.
//!
//! One thread, one tick a second: it re-reads the list when the file changes
//! (the settings app may be editing it), announces anything that has come due,
//! and runs the function attached to it. It works whether or not the
//! microphone is paused — an alarm is not a listening feature.

use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use nv_core::apps::AppIndex;
use nv_core::brain::Action;
use nv_core::schedule::{announcement, Item, ItemKind, Schedule, Stamp};
use nv_core::{actions, personality, Config};

use crate::bubble::Mode;
use crate::overlay::Shared;
use crate::tts::Tts;

use nv_core::actions::Speaker;

pub fn spawn(cfg: Config, shared: Arc<Shared>, apps: Arc<RwLock<AppIndex>>, tts: Arc<Tts>) {
    std::thread::Builder::new()
        .name("schedule".into())
        .spawn(move || run(cfg, shared, apps, tts))
        .expect("scheduler thread");
}

fn modified() -> Option<SystemTime> {
    std::fs::metadata(Schedule::path()).and_then(|m| m.modified()).ok()
}

/// How late an item may be and still be announced as if it just happened.
const ON_TIME_MINUTES: i64 = 5;

fn run(cfg: Config, shared: Arc<Shared>, apps: Arc<RwLock<AppIndex>>, tts: Arc<Tts>) {
    let voice = crate::tts::AgentVoice::new(tts, shared.clone());
    let mut schedule = Schedule::open();
    let mut stamp = modified();
    loop {
        std::thread::sleep(Duration::from_secs(1));

        // The settings app may have added, moved or removed something.
        let now_stamp = modified();
        if now_stamp != stamp {
            stamp = now_stamp;
            schedule = Schedule::open();
        }

        let now = Stamp::now();
        let due: Vec<Item> = schedule.due(now).into_iter().cloned().collect();
        if due.is_empty() {
            continue;
        }

        let mut on_time: Vec<Item> = Vec::new();
        let mut missed: Vec<(Item, i64)> = Vec::new();
        for item in due {
            let late = item.at.map_or(0, |at| at.on_day_of(now).minutes_until(now).max(0));
            if late > ON_TIME_MINUTES {
                missed.push((item, late));
            } else {
                on_time.push(item);
            }
        }

        // Mark everything as fired before doing anything slow, so a crash or a
        // second agent can't set it off twice.
        for item in on_time.iter().chain(missed.iter().map(|(i, _)| i)) {
            schedule.mark_fired(item.id, now);
        }
        if let Err(e) = schedule.save() {
            log::warn!("couldn't save the schedule: {e}");
        }
        stamp = modified();

        if !missed.is_empty() {
            announce_missed(&missed, &voice, &shared);
        }
        for item in on_time {
            log::info!("{} due: {}", item.kind.label(), item.text);
            if item.chime || item.kind == ItemKind::Alarm {
                crate::chime::play();
            }
            if shared.mode() == Mode::Hidden {
                shared.set_mode(Mode::Success);
            }
            voice.say(&announcement(&item, now));
            run_action(&item, &cfg, &apps, &voice);
        }
    }
}

/// Things that came due while the agent wasn't running: one line, not five.
fn announce_missed(missed: &[(Item, i64)], voice: &crate::tts::AgentVoice, shared: &Shared) {
    for (item, late) in missed {
        let ago = if *late >= 120 {
            format!("{} hours ago", late / 60)
        } else {
            format!("{late} minutes ago")
        };
        log::info!("missed {} ({ago}): {}", item.kind.label(), item.text);
        if shared.mode() == Mode::Hidden {
            shared.set_mode(Mode::Success);
        }
        voice.say(&format!("While you were away ({ago}): {}", announcement(item, Stamp::now())));
    }
}

/// Run the function an item is wired to, if any.
fn run_action(item: &Item, cfg: &Config, apps: &Arc<RwLock<AppIndex>>, voice: &crate::tts::AgentVoice) {
    if item.action.trim().is_empty() {
        return;
    }
    let action = Action::Custom { name: item.action.clone(), params: Vec::new() };
    let index = apps.read().unwrap().clone();
    match actions::execute_with(&action, cfg, &index, voice) {
        Ok(msg) => log::info!("  ✓ {msg}"),
        Err(e) => {
            log::warn!("  ✗ {e}");
            voice.say(&personality::line(cfg.personality, &personality::Moment::Failed(&action, &e)));
        }
    }
}
