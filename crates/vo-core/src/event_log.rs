//! The event log: one format for live input, the replay corpus and authored scenarios.
//!
//! A log is a seed plus timestamped events in time order. Replaying it and sampling at any
//! `t` gives the same frame as the live run did at `t`. The payload type belongs to the app;
//! this module only adds the envelope. Stored as JSON with a `version` field, so later
//! versions can read old logs and refuse ones they do not understand.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

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
            EventLogError::Json(e) => write!(f, "event log is not valid JSON for this app: {e}"),
        }
    }
}

impl std::error::Error for EventLogError {}

impl<E> EventLog<E> {
    pub fn new(seed: u32) -> EventLog<E> {
        EventLog { version: EVENT_LOG_VERSION, seed, events: Vec::new() }
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
        Ok(log)
    }
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
}
