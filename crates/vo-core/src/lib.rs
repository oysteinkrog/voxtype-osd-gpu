//! Overlay core: pure CPU logic shared by the scene, the renderer and their tests.
//!
//! Time model (M1): `time`, `spring`, `channel`, `track`, `hash`, `event_log`.
//! Timelines (M2): `timeline`. Nothing here
//! reads a clock; `clippy.toml` in this crate bans `Instant::now` and `SystemTime::now`.
//! Word model (phase 0): `words`. Noise (FX10): `noise`, the Rust side of the shaders' noise.
//! Signal bus (SIG01): `signals`, audio features as a timestamped input.
//! Keyboard (edit F1): `key`, the key event types the host, driver and edit model share.
//! Edit model (edit F3): `edit`, one editable text with selection, moves and edits;
//! `edit_keymap`, which key does what to it.

pub mod ambient;
pub mod channel;
pub mod edit;
pub mod edit_keymap;
pub mod event_log;
pub mod hash;
pub mod key;
pub mod noise;
pub mod signals;
pub mod spring;
pub mod throttle;
pub mod time;
pub mod timeline;
pub mod track;
pub mod words;
