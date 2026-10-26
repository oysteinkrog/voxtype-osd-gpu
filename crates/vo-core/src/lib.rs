//! Overlay core: pure CPU logic shared by the scene, the renderer and their tests.
//!
//! Time model (M1): `time`, `spring`, `channel`, `track`, `hash`, `event_log`.
//! Timelines (M2): `timeline`. Nothing here
//! reads a clock; `clippy.toml` in this crate bans `Instant::now` and `SystemTime::now`.
//! Word model (phase 0): `words`.

pub mod channel;
pub mod event_log;
pub mod hash;
pub mod spring;
pub mod time;
pub mod timeline;
pub mod track;
pub mod words;
