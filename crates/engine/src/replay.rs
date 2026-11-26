//! Replay scripts: JSON input scripts (ticks and keys, never game data) that drive the
//! headless host.
//!
//! ```json
//! {
//!   "seed": 7,
//!   "quit_at": 40000,
//!   "events": [{ "tick": 10, "tap": "SPACE" }, { "tick": 12, "down": "SHIFT" }],
//!   "rules": [
//!     { "screen": "PlayerCount", "entry": 1, "delay": 30, "key": "1" },
//!     { "screen": "AnswerPhase", "entry": "every", "delay": 3, "key": "correct" },
//!     { "screen": "PickVictim", "entry": "every", "delay": 6, "key": ["1", "2"],
//!       "repeat": 4 }
//!   ],
//!   "punch": ["RIGHT", "LEFT", "UP", "DOWN"]
//! }
//! ```
//!
//! - `events`: fixed key events delivered by the present with that index (see
//!   [`crate::headless`]): `tap`, `down` or `up` with a key name.
//! - `rules`: tap a key `delay` presents after the `entry`-th entry (1-based, or
//!   `"every"` entry) into a screen loop named by [`DebugState::screen`]. `key` is one
//!   key or a list; with `repeat` the keys are tapped in turn every `repeat` presents
//!   while the screen lasts (at most `times` taps, default unlimited), otherwise one key
//!   once. A list rotates with the entry: the n-th entry starts at key n - 1. The key `"correct"` is the letter of the correct answer slot
//!   (the engine debug hook [`DebugState::correct_slot`], since answer order depends on
//!   the seed), `"wrong"` the next slot.
//! - `punch`: while a beat-up round is fighting and the fist is idle, tap these arrows in
//!   turn (a different arrow each time avoids the 3-same-punch block).
//! - `quit_at`: the window is closed at this present index (bounds every run).
//! - `stop` (optional): close the window `delay` presents after the first entry into
//!   `screen`, e.g. `{ "screen": "PlayAgain", "delay": 20 }`.

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::audio::SoundSource;
use crate::ctx::{Abort, Ctx, DebugState};
use crate::headless::{HeadlessHost, ScriptEvent};
use crate::input::Vk;
use crate::state::Game;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Vk(Vk),
    /// The letter of the correct answer slot.
    Correct,
    /// The letter after the correct slot (a wrong answer).
    Wrong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// The n-th entry (1-based).
    Nth(usize),
    Every,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub screen: String,
    pub entry: Entry,
    pub delay: u64,
    pub keys: Vec<Key>,
    /// Tap period; `None` taps once.
    pub repeat: Option<u64>,
    pub times: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub seed: u32,
    pub quit_at: u64,
    pub events: Vec<(u64, ScriptEvent)>,
    pub rules: Vec<Rule>,
    pub punch: Vec<Vk>,
    /// (screen, delay): quit this many presents after first entering the screen.
    pub stop: Option<(String, u64)>,
}

/// Named keys accepted in scripts besides single letters and digits.
const NAMED: [(&str, Vk); 9] = [
    ("SPACE", Vk::SPACE),
    ("RETURN", Vk::RETURN),
    ("ESCAPE", Vk::ESCAPE),
    ("SHIFT", Vk::SHIFT),
    ("LEFT", Vk::LEFT),
    ("UP", Vk::UP),
    ("RIGHT", Vk::RIGHT),
    ("DOWN", Vk::DOWN),
    ("F12", Vk::F12),
];

pub fn parse_vk(name: &str) -> Result<Vk, String> {
    if let Some(&(_, vk)) = NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        return Ok(vk);
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Vk::from_char(c).ok_or_else(|| format!("unknown key {name:?}")),
        _ => Err(format!("unknown key {name:?}")),
    }
}

fn parse_key(name: &str) -> Result<Key, String> {
    match name {
        "correct" => Ok(Key::Correct),
        "wrong" => Ok(Key::Wrong),
        _ => parse_vk(name).map(Key::Vk),
    }
}

fn u64_field(v: &Value, name: &str) -> Result<Option<u64>, String> {
    match v.get(name) {
        None => Ok(None),
        Some(x) => x
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{name}: expected a non-negative integer")),
    }
}

fn str_list(v: &Value, what: &str) -> Result<Vec<String>, String> {
    match v {
        Value::String(s) => Ok(vec![s.clone()]),
        Value::Array(a) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{what}: expected strings"))
            })
            .collect(),
        _ => Err(format!("{what}: expected a string or a list of strings")),
    }
}

impl Script {
    pub fn parse(json: &str) -> Result<Script, String> {
        let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let seed = u64_field(&v, "seed")?.unwrap_or(1);
        let seed = u32::try_from(seed).map_err(|_| "seed: out of range".to_string())?;
        let quit_at = u64_field(&v, "quit_at")?.ok_or("quit_at: missing")?;
        let mut events = Vec::new();
        for e in v
            .get("events")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let tick = u64_field(e, "tick")?.ok_or("event: missing tick")?;
            let ev = if let Some(k) = e.get("tap").and_then(Value::as_str) {
                ScriptEvent::Tap(parse_vk(k)?)
            } else if let Some(k) = e.get("down").and_then(Value::as_str) {
                ScriptEvent::KeyDown(parse_vk(k)?)
            } else if let Some(k) = e.get("up").and_then(Value::as_str) {
                ScriptEvent::KeyUp(parse_vk(k)?)
            } else {
                return Err(format!("event at tick {tick}: expected tap, down or up"));
            };
            events.push((tick, ev));
        }
        let mut rules = Vec::new();
        for r in v
            .get("rules")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let screen = r
                .get("screen")
                .and_then(Value::as_str)
                .ok_or("rule: missing screen")?
                .to_string();
            let entry = match r.get("entry") {
                None => Entry::Nth(1),
                Some(Value::String(s)) if s == "every" => Entry::Every,
                Some(x) => match x.as_u64() {
                    Some(n) if n >= 1 => Entry::Nth(n as usize),
                    _ => return Err(format!("rule {screen}: entry must be >= 1 or \"every\"")),
                },
            };
            let keys = str_list(r.get("key").ok_or("rule: missing key")?, "key")?
                .iter()
                .map(|k| parse_key(k))
                .collect::<Result<Vec<_>, _>>()?;
            if keys.is_empty() {
                return Err(format!("rule {screen}: no keys"));
            }
            let repeat = u64_field(r, "repeat")?.filter(|&p| p > 0);
            let times = u64_field(r, "times")?.map(|t| t as usize);
            rules.push(Rule {
                screen,
                entry,
                delay: u64_field(r, "delay")?.unwrap_or(0),
                keys,
                repeat,
                times,
            });
        }
        let punch = match v.get("punch") {
            None => Vec::new(),
            Some(p) => str_list(p, "punch")?
                .iter()
                .map(|k| parse_vk(k))
                .collect::<Result<Vec<_>, _>>()?,
        };
        let stop = match v.get("stop") {
            None => None,
            Some(s) => Some((
                s.get("screen")
                    .and_then(Value::as_str)
                    .ok_or("stop: missing screen")?
                    .to_string(),
                u64_field(s, "delay")?.unwrap_or(0),
            )),
        };
        Ok(Script {
            seed,
            quit_at,
            events,
            rules,
            punch,
            stop,
        })
    }
}

/// Per-rule progress within the current screen entry.
#[derive(Debug, Clone, Copy, Default)]
struct Progress {
    /// (screen entry serial, taps in that entry).
    entry: u64,
    taps: usize,
}

/// The responder that turns the rules into key taps.
#[derive(Debug, Default)]
pub struct Driver {
    rules: Vec<(Rule, Progress)>,
    punch: Vec<Vk>,
    stop: Option<(String, u64)>,
    stop_seen: Option<u64>,
    punches: usize,
    last_punch: Option<u64>,
    screen: &'static str,
    entered_at: u64,
    /// Increments on every screen change (identifies an entry).
    serial: u64,
    entries: Vec<(&'static str, usize)>,
    /// Every key the driver tapped: (present index, key).
    pub sent: Vec<(u64, Vk)>,
}

impl Driver {
    pub fn new(script: &Script) -> Self {
        Driver {
            rules: script
                .rules
                .iter()
                .map(|r| (r.clone(), Progress::default()))
                .collect(),
            punch: script.punch.clone(),
            stop: script.stop.clone(),
            ..Driver::default()
        }
    }

    pub fn step(&mut self, tick: u64, d: &DebugState) -> Vec<ScriptEvent> {
        if d.screen != self.screen || self.serial == 0 {
            self.screen = d.screen;
            self.entered_at = tick;
            self.serial += 1;
            match self.entries.iter_mut().find(|(s, _)| *s == d.screen) {
                Some((_, n)) => *n += 1,
                None => self.entries.push((d.screen, 1)),
            }
        }
        let entry = self
            .entries
            .iter()
            .find(|(s, _)| *s == d.screen)
            .map_or(0, |e| e.1);
        if let Some((screen, delay)) = &self.stop {
            if self.stop_seen.is_none() && d.screen == screen {
                self.stop_seen = Some(tick);
            }
            if self.stop_seen.is_some_and(|t| tick >= t + delay) {
                return vec![ScriptEvent::Quit];
            }
        }
        let mut out = Vec::new();
        let since = tick - self.entered_at;
        for (r, p) in &mut self.rules {
            if r.screen != d.screen {
                continue;
            }
            if let Entry::Nth(n) = r.entry
                && n != entry
            {
                continue;
            }
            if p.entry != self.serial {
                *p = Progress {
                    entry: self.serial,
                    taps: 0,
                };
            }
            let limit = match r.repeat {
                None => 1,
                Some(_) => r.times.unwrap_or(usize::MAX),
            };
            if p.taps >= limit || since < r.delay {
                continue;
            }
            if let Some(period) = r.repeat
                && !(since - r.delay).is_multiple_of(period)
            {
                continue;
            }
            let vk = match r.keys[(entry - 1 + p.taps) % r.keys.len()] {
                Key::Vk(vk) => Some(vk),
                Key::Correct => d.correct_slot.map(|s| Vk(b'A' + s as u8)),
                Key::Wrong => d.correct_slot.map(|s| Vk(b'A' + ((s as u8 + 1) % 4))),
            };
            if let Some(vk) = vk {
                p.taps += 1;
                out.push(ScriptEvent::Tap(vk));
                self.sent.push((tick, vk));
            }
        }
        // The arrow tapped now is polled after this present and read by the fist on the
        // next tick; skip one tick so the same punch is not sent twice.
        if !self.punch.is_empty()
            && d.beat_up_state == Some(1)
            && d.fist_idle
            && self.last_punch.is_none_or(|t| tick >= t + 2)
        {
            let vk = self.punch[self.punches % self.punch.len()];
            self.punches += 1;
            self.last_punch = Some(tick);
            out.push(ScriptEvent::Tap(vk));
            self.sent.push((tick, vk));
        }
        out
    }
}

/// A headless host running `script`: fixed events, the rule driver as responder and a
/// quit at `quit_at`.
pub fn host(
    script: &Script,
    source: Box<dyn SoundSource + Send>,
) -> (HeadlessHost, Arc<Mutex<Driver>>) {
    host_observed(script, source, |_, _| {})
}

/// [`host`] that also hands the debug state before every present to `observe`.
pub fn host_observed(
    script: &Script,
    source: Box<dyn SoundSource + Send>,
    mut observe: impl FnMut(u64, &DebugState) + Send + 'static,
) -> (HeadlessHost, Arc<Mutex<Driver>>) {
    let mut events = script.events.clone();
    events.push((script.quit_at, ScriptEvent::Quit));
    let mut host = HeadlessHost::new(events, source);
    let driver = Arc::new(Mutex::new(Driver::new(script)));
    let d2 = driver.clone();
    host.set_responder(Box::new(move |t, s| {
        observe(t, s);
        d2.lock().unwrap_or_else(|e| e.into_inner()).step(t, s)
    }));
    (host, driver)
}

/// Run the whole program (`crate::run`) on `host` with the script's seed.
pub fn run(host: &mut HeadlessHost, game: &mut Game, seed: u32) -> (Result<(), Abort>, DebugState) {
    let mut ctx = Ctx::new(host, seed);
    let r = crate::screens::run(&mut ctx, game);
    (r, ctx.debug.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_rules_and_punch() {
        let s = Script::parse(
            r#"{"seed": 3, "quit_at": 100,
                "events": [{"tick": 2, "tap": "space"}, {"tick": 4, "down": "SHIFT"}],
                "rules": [{"screen": "A", "delay": 5, "key": "1"},
                          {"screen": "B", "entry": "every", "key": ["correct", "q"],
                           "repeat": 3, "times": 2}],
                "punch": ["RIGHT", "LEFT"]}"#,
        )
        .unwrap();
        assert_eq!((s.seed, s.quit_at), (3, 100));
        assert_eq!(
            s.events,
            vec![
                (2, ScriptEvent::Tap(Vk::SPACE)),
                (4, ScriptEvent::KeyDown(Vk::SHIFT))
            ]
        );
        assert_eq!(s.rules[0].entry, Entry::Nth(1));
        assert_eq!(s.rules[1].keys, vec![Key::Correct, Key::Vk(Vk::Q)]);
        assert_eq!(s.punch, vec![Vk::RIGHT, Vk::LEFT]);
        assert!(Script::parse(r#"{"seed": 1}"#).is_err());
        assert!(
            Script::parse(r#"{"quit_at": 1, "rules": [{"screen": "A", "key": "XY"}]}"#).is_err()
        );
    }

    fn state(screen: &'static str) -> DebugState {
        DebugState {
            screen,
            ..DebugState::default()
        }
    }

    #[test]
    fn driver_taps_per_entry_and_repeats() {
        let s = Script::parse(
            r#"{"quit_at": 100, "rules": [
                {"screen": "A", "entry": 2, "delay": 1, "key": "1"},
                {"screen": "B", "entry": "every", "delay": 0, "key": ["correct", "X"],
                 "repeat": 2, "times": 2}]}"#,
        )
        .unwrap();
        let mut d = Driver::new(&s);
        let a = state("A");
        let mut b = state("B");
        b.correct_slot = Some(2);
        let mut taps = Vec::new();
        let seq = [&a, &a, &b, &a, &a, &a, &b, &b, &b, &b, &b];
        for (t, st) in seq.iter().enumerate() {
            for e in d.step(t as u64, st) {
                taps.push((t, e));
            }
        }
        assert_eq!(
            taps,
            vec![
                (2, ScriptEvent::Tap(Vk::C)),
                (4, ScriptEvent::Tap(Vk::D1)),
                // The second entry starts at the second key.
                (6, ScriptEvent::Tap(Vk::X)),
                (8, ScriptEvent::Tap(Vk::C)),
            ]
        );
    }

    #[test]
    fn driver_punches_when_the_fist_is_idle() {
        let s = Script::parse(r#"{"quit_at": 1, "punch": ["RIGHT", "LEFT"]}"#).unwrap();
        let mut d = Driver::new(&s);
        let mut st = state("BeatUpLoop");
        st.beat_up_state = Some(1);
        st.fist_idle = true;
        let mut taps = Vec::new();
        for t in 0..5 {
            taps.extend(d.step(t, &st));
        }
        st.fist_idle = false;
        taps.extend(d.step(5, &st));
        assert_eq!(
            taps,
            vec![
                ScriptEvent::Tap(Vk::RIGHT),
                ScriptEvent::Tap(Vk::LEFT),
                ScriptEvent::Tap(Vk::RIGHT)
            ]
        );
    }
}
