//! The dot-matrix design family (family 21): everything is built from round dots on one grid.
//!
//! - `field`: [`DotField`], dots keyed by id, each with springs for position, size,
//!   brightness and colour (VE05). It is a `vo_scene::dots::DotSource`, so an app shows it with
//!   `Element::dots`.
//!
//! Nothing here touches the GPU. The renderer draws the dots with an instanced pipeline of
//! its own (`vo_render::dots`).

pub mod field;

pub use field::{DotField, Look, Springs};
pub use vo_scene::dots::{Dot, DotSource, Dots};
