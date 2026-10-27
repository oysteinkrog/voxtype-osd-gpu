//! The dot-matrix design family (family 21): everything is built from round dots on one grid.
//!
//! - `field`: [`DotField`], dots keyed by id, each with springs for position, size,
//!   brightness and colour (VE05). It is a `vo_scene::dots::DotSource`, so an app shows it with
//!   `Element::dots`.
//! - `font`: [`DotFont`], 5x7 and 7x9 dot fonts as data (TY16).
//! - `text`: [`DotText`], a string as dots that assembles, dissolves and morphs (TY16).
//! - `particles`: dots that peel off and fly away, and dots that assemble into a shape such
//!   as the check (PA09); [`DotStack`] to draw several sources on one grid.
//! - `signals`: [`MelField`], a mel spectrogram as dots, and [`pitch_contour`], a pitch
//!   stroke, both read from the signal bus (`vo_core::signals`).
//! - `wave`: [`DotWave`], [`DotMeter`] and [`DotTicker`]: level visuals and a ticker as dots.
//!
//! Nothing here touches the GPU. The renderer draws the dots with an instanced pipeline of
//! its own (`vo_render::dots`).

pub mod field;
pub mod font;
mod font_data;
pub mod particles;
pub mod signals;
pub mod text;
pub mod wave;

pub use field::{DotField, Look, Springs};
pub use font::DotFont;
pub use particles::{assemble, check_cells, peel_off, DotStack, Peel};
pub use signals::{pitch_contour, sample_speech, ContourLook, MelField};
pub use text::{Change, DotText, TextStyle};
pub use vo_scene::dots::{Dot, DotSource, Dots};
pub use wave::{DotMeter, DotTicker, DotWave};
