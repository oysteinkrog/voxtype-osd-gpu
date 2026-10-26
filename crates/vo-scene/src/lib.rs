//! Scene: what the app describes, what the framework keeps, and what the renderer draws.
//!
//! - `element`: the keyed builder a view function returns. Every number on it is a target.
//! - `scene`: the retained node arena. `apply(view, t)` reconciles by key path and retargets
//!   channels at exactly `t`; `sample(t)` is pure; `next_wake(t)` is `None` when settled.
//! - `layout`: taffy (flexbox and block) on the node arena through its low-level traits.
//! - `font`: the font table that text layout and the renderer share.
//! - `text`: text measurement behind a trait, with a width estimate for now.
//! - `input`: hit testing at the event's time and pointer messages.
//! - `inspect`: read-only node and channel views for tools (preview overlays, state dumps).
//! - `display`: the flat display list that `vo-render` consumes. No tree, no GPU types.
//! - `timed`: elements drawn from the frame time: a scrolling waveform, a progress sweep.
//! - `geom`: points, sizes, rects, colours and 2D transforms in logical units (`f32`).

pub mod display;
pub mod element;
pub mod font;
pub mod geom;
pub mod input;
pub mod inspect;
mod layout;
pub mod scene;
pub mod text;
pub mod timed;

/// The layout engine, for `Element::layout` and the alignment types the builder takes.
pub use taffy;

pub use display::DisplayList;
pub use element::{Element, Key, Motion, Prop, TextStyle, Transition};
pub use font::{FontId, FontRegistry};
pub use scene::{NodeId, Presence, Scene};
