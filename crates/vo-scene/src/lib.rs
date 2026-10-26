//! Scene: what the app describes, what the framework keeps, and what the renderer draws.
//!
//! - `geom`: points, sizes, rects, colours and 2D transforms in logical units (`f32`).
//! - `display`: the flat display list that `vo-render` consumes. No tree, no GPU types.

pub mod display;
pub mod geom;
