//! The event log: one format for live input, the replay corpus and authored scenarios.
//!
//! A log is a seed plus timestamped events in time order. Replaying it and sampling at any
//! `t` gives the same frame as the live run did at `t`. The payload type belongs to the app;
//! this module only adds the envelope. Stored as JSON with a `version` field, so later
//! versions can read old logs and refuse ones they do not understand.
//!
//! Keyboard input (keys, focus, modifiers, keymap and repeat info) goes in its own `input`
//! list of [`InputRecord`]s beside `events`, still version 1: a version 1 reader ignores
//! fields it does not know, so it skips them, and a log with no input writes exactly as
//! before. Repeat info always comes before the first key, so a replay repeats keys as the
//! live run did.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::key::{KeyState, LogicalKey, Mods, RepeatInfo};

/// The format version this build writes. Raise it on any change that an older reader would
/// misread, and keep reading the old versions.
pub const EVENT_LOG_VERSION: u32 = 1;

/// One event at time `t` (seconds from the log's start).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event<E> {
    pub t: f64,
    pub event: E,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventLog<E> {
    pub version: u32,
    /// Seed for every `hash::rand` call the replay makes.
    pub seed: u32,
    events: Vec<Event<E>>,
    /// Keyboard input, in time order (`push_input`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    input: Vec<Event<InputRecord>>,
}

/// One keyboard input record. The surface is left out: the app has one keyboard focus.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputRecord {
    /// A key as the host sent it. Repeats are not logged: the replay makes them again from
    /// the repeat info.
    Key {
        state: KeyState,
        key: LogicalKey,
        code: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        mods: Mods,
    },
    KeyboardFocus {
        gained: bool,
        requested: bool,
    },
    Modifiers {
        mods: Mods,
    },
    /// The compositor's keymap changed; `name` is its layout, when known.
    Keymap {
        name: String,
    },
    RepeatInfo {
        info: RepeatInfo,
    },
}

#[derive(Debug)]
pub enum EventLogError {
    /// An event earlier than the one before it, or a time that is not a finite number.
    BadTime {
        previous: f64,
        t: f64,
    },
    /// Written by a newer build.
    UnsupportedVersion(u32),
    /// A key record before any repeat info: a replay would not know how the key repeats.
    KeyBeforeRepeatInfo {
        t: f64,
    },
    Json(serde_json::Error),
}

impl std::fmt::Display for EventLogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventLogError::BadTime { previous, t } => {
                write!(f, "event at t = {t} s comes after one at {previous} s; events must be finite and in time order")
            }
            EventLogError::UnsupportedVersion(v) => {
                write!(f, "event log version {v} is newer than this build reads ({EVENT_LOG_VERSION}); update the tool")
            }
            EventLogError::KeyBeforeRepeatInfo { t } => {
                write!(f, "key at t = {t} s comes before any repeat info; log the repeat info first")
            }
            EventLogError::Json(e) => write!(f, "event log is not valid JSON for this app: {e}"),
        }
    }
}

impl std::error::Error for EventLogError {}

impl<E> EventLog<E> {
    pub fn new(seed: u32) -> EventLog<E> {
        EventLog { version: EVENT_LOG_VERSION, seed, events: Vec::new(), input: Vec::new() }
    }

    /// Appends a keyboard input record. Times must be finite and never go backwards, and a
    /// key needs repeat info logged before it.
    pub fn push_input(&mut self, t: f64, record: InputRecord) -> Result<(), EventLogError> {
        check_input(&self.input, t, &record)?;
        self.input.push(Event { t, event: record });
        Ok(())
    }

    pub fn input(&self) -> &[Event<InputRecord>] {
        &self.input
    }

    /// Appends an event. Times must be finite and never go backwards; equal times keep
    /// their order.
    pub fn push(&mut self, t: f64, event: E) -> Result<(), EventLogError> {
        let previous = self.events.last().map_or(f64::NEG_INFINITY, |e| e.t);
        if !t.is_finite() || t < previous {
            return Err(EventLogError::BadTime { previous, t });
        }
        self.events.push(Event { t, event });
        Ok(())
    }

    pub fn events(&self) -> &[Event<E>] {
        &self.events
    }

    /// Events with `after < t <= until`: what a host applies before sampling the frame at
    /// `until` when the previous frame was at `after`.
    pub fn between(&self, after: f64, until: f64) -> &[Event<E>] {
        let lo = self.events.partition_point(|e| e.t <= after);
        let hi = self.events.partition_point(|e| e.t <= until);
        &self.events[lo..hi.max(lo)]
    }

    /// Time of the last event, or 0 for an empty log.
    pub fn end(&self) -> f64 {
        self.events.last().map_or(0.0, |e| e.t)
    }
}

impl<E: Serialize> EventLog<E> {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("an event log always serializes")
    }
}

impl<E: DeserializeOwned> EventLog<E> {
    /// Reads a log, checking the version and the time order.
    pub fn from_json(text: &str) -> Result<EventLog<E>, EventLogError> {
        #[derive(Deserialize)]
        struct Header {
            version: u32,
        }
        // Check the version before the payloads, so a newer log reports its version rather
        // than a payload error.
        let header: Header = serde_json::from_str(text).map_err(EventLogError::Json)?;
        if header.version > EVENT_LOG_VERSION {
            return Err(EventLogError::UnsupportedVersion(header.version));
        }
        let log: EventLog<E> = serde_json::from_str(text).map_err(EventLogError::Json)?;
        let mut previous = f64::NEG_INFINITY;
        for e in &log.events {
            if !e.t.is_finite() || e.t < previous {
                return Err(EventLogError::BadTime { previous, t: e.t });
            }
            previous = e.t;
        }
        for (i, e) in log.input.iter().enumerate() {
            check_input(&log.input[..i], e.t, &e.event)?;
        }
        Ok(log)
    }
}

/// Whether `record` at `t` may follow `before`.
fn check_input(before: &[Event<InputRecord>], t: f64, record: &InputRecord) -> Result<(), EventLogError> {
    let previous = before.last().map_or(f64::NEG_INFINITY, |e| e.t);
    if !t.is_finite() || t < previous {
        return Err(EventLogError::BadTime { previous, t });
    }
    let has_repeat = || before.iter().any(|e| matches!(e.event, InputRecord::RepeatInfo { .. }));
    if matches!(record, InputRecord::Key { .. }) && !has_repeat() {
        return Err(EventLogError::KeyBeforeRepeatInfo { t });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    enum Ev {
        Start,
        Preview { text: String },
        Level(f32),
        Stop,
    }

    fn sample() -> EventLog<Ev> {
        let mut log = EventLog::new(42);
        log.push(0.0, Ev::Start).unwrap();
        log.push(0.31, Ev::Level(0.4)).unwrap();
        log.push(0.52, Ev::Preview { text: "hello there".into() }).unwrap();
        log.push(0.52, Ev::Level(0.6)).unwrap();
        log.push(1.75, Ev::Stop).unwrap();
        log
    }

    #[test]
    fn json_round_trip() {
        let log = sample();
        let text = log.to_json();
        assert!(text.contains("\"version\": 1"), "{text}");
        let back: EventLog<Ev> = EventLog::from_json(&text).unwrap();
        assert_eq!(back, log);
    }

    #[test]
    fn rejects_bad_order_and_newer_versions() {
        let mut log = sample();
        assert!(matches!(log.push(1.0, Ev::Stop), Err(EventLogError::BadTime { .. })));
        assert!(matches!(log.push(f64::NAN, Ev::Stop), Err(EventLogError::BadTime { .. })));
        let newer = log.to_json().replace("\"version\": 1", "\"version\": 2");
        let err = EventLog::<Ev>::from_json(&newer).unwrap_err();
        assert!(matches!(err, EventLogError::UnsupportedVersion(2)), "{err}");
        let shuffled = r#"{"version":1,"seed":0,"events":[{"t":1.0,"event":"Start"},{"t":0.5,"event":"Stop"}]}"#;
        assert!(matches!(EventLog::<Ev>::from_json(shuffled), Err(EventLogError::BadTime { .. })));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let text = r#"{"version":1,"seed":3,"note":"from a later minor change","events":[{"t":0.0,"event":"Start"}]}"#;
        let log = EventLog::<Ev>::from_json(text).unwrap();
        assert_eq!(log.seed, 3);
        assert_eq!(log.events().len(), 1);
    }

    #[test]
    fn between_gives_each_event_to_exactly_one_frame() {
        let log = sample();
        for hz in [30.0, 60.0, 144.0] {
            let mut seen = 0;
            let mut prev = f64::NEG_INFINITY;
            for frame in 0..=(2.0 * hz) as u64 {
                let t = frame as f64 / hz;
                seen += log.between(prev, t).len();
                prev = t;
            }
            assert_eq!(seen, log.events().len(), "{hz} Hz");
        }
        assert_eq!(log.between(0.3, 0.52).len(), 3);
        assert_eq!(log.end(), 1.75);
    }

    fn key(state: KeyState, c: char) -> InputRecord {
        let text = (state == KeyState::Down).then(|| c.to_string());
        InputRecord::Key { state, key: LogicalKey::Char(c), code: 18, text, mods: Mods::NONE }
    }

    fn with_keys() -> EventLog<Ev> {
        let mut log = sample();
        log.push_input(0.0, InputRecord::Keymap { name: "us(altgr-intl)".into() }).unwrap();
        log.push_input(0.0, InputRecord::RepeatInfo { info: RepeatInfo { rate_hz: 25.0, delay_s: 0.6 } }).unwrap();
        log.push_input(0.6, InputRecord::KeyboardFocus { gained: true, requested: false }).unwrap();
        log.push_input(0.7, key(KeyState::Down, 'é')).unwrap();
        log.push_input(0.75, InputRecord::Modifiers { mods: Mods::SHIFT }).unwrap();
        log.push_input(0.8, key(KeyState::Up, 'é')).unwrap();
        log.push_input(1.2, InputRecord::KeyboardFocus { gained: false, requested: true }).unwrap();
        log
    }

    #[test]
    fn key_records_round_trip() {
        let log = with_keys();
        let back: EventLog<Ev> = EventLog::from_json(&log.to_json()).unwrap();
        assert_eq!(back, log);
        assert_eq!(back.input().len(), 7);
        // A log without input writes exactly as before: no `input` field.
        assert!(!sample().to_json().contains("input"));
    }

    #[test]
    fn version_1_readers_skip_key_records() {
        /// The reader as it was before key records.
        #[derive(Deserialize)]
        struct V1<E> {
            version: u32,
            seed: u32,
            events: Vec<Event<E>>,
        }
        let log = with_keys();
        let old: V1<Ev> = serde_json::from_str(&log.to_json()).unwrap();
        assert_eq!((old.version, old.seed), (1, 42));
        assert_eq!(old.events, log.events());
    }

    #[test]
    fn repeat_info_comes_before_the_first_key() {
        let mut log = EventLog::<Ev>::new(1);
        log.push_input(0.1, InputRecord::KeyboardFocus { gained: true, requested: false }).unwrap();
        let err = log.push_input(0.2, key(KeyState::Down, 'a')).unwrap_err();
        assert!(matches!(err, EventLogError::KeyBeforeRepeatInfo { .. }), "{err}");
        // A file that breaks the rule is refused too.
        let mut bad = EventLog::<Ev>::new(1);
        bad.input.push(Event { t: 0.2, event: key(KeyState::Down, 'a') });
        let err = EventLog::<Ev>::from_json(&bad.to_json()).unwrap_err();
        assert!(matches!(err, EventLogError::KeyBeforeRepeatInfo { .. }), "{err}");
    }
}
