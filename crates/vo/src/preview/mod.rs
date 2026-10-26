//! The preview and scrub tool (feature `preview`).
//!
//! - [`Player`]: the scene at any time, in any order, exactly as an export renders it.

mod player;

pub use player::{Player, SeekStats};
