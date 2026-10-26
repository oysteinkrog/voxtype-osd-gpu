//! The dot-matrix design family (family 21): everything is built from round dots on one grid.
//!
//! - `field`: [`DotField`], dots keyed by id, each with springs for position, size,
//!   brightness and colour (VE05). It is a `vo_scene::dots::DotSource`, so an app shows it with
//!   `Element::dots`.
//! - `font`: [`DotFont`], 5x7 and 7x9 dot fonts as data (TY16).
//! - `text`: [`DotText`], a string as dots that assembles, dissolves and morphs (TY16).
//!
//! Nothing here touches the GPU. The renderer draws the dots with an instanced pipeline of
//! its own (`vo_render::dots`).

pub mod field;
pub mod font;
mod font_data;
pub mod text;

pub use field::{DotField, Look, Springs};
pub use font::DotFont;
pub use text::{Change, DotText, TextStyle};
pub use vo_scene::dots::{Dot, DotSource, Dots};
