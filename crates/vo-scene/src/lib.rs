//! Scene: what the app describes, what the framework keeps, and what the renderer draws.
//!
//! - `element`: the keyed builder a view function returns. Every number on it is a target.
//! - `scene`: the retained node arena. `apply(view, t)` reconciles by key path and retargets
//!   channels at exactly `t`; `sample(t)` is pure; `next_wake(t)` is `None` when settled.
//! - `layout`: taffy (flexbox and block) on the node arena through its low-level traits.
//! - `font`: the font table that text layout and the renderer share, with fallback chains.
//! - `assets`: fonts and other small files built into the binary, by name.
//! - `text`: text measurement behind a trait, with a width estimate for now.
//! - `flow`: live text motion: words spring to each new layout, revisions, caret.
//! - `input`: hit testing at the event's time and pointer messages.
//! - `inspect`: read-only node and channel views for tools (preview overlays, state dumps).
//! - `bars`: a bar-strip waveform over the level history, with settle and a playhead.
//! - `levels`: the level history and colour stops the waveform elements share.
//! - `trace`: a line trace over the level, with ghost copies and a fading horizon.
//! - `ribbon`: glowing ribbon strands that wind round each other, swinging with the level.
//! - `segments`: segment meters in a row, a column or an arc, with peak hold and clip.
//! - `canvas`: elements drawn by app code from the frame time.
//! - `path`: vector paths: strokes with arc length, trim, morph, filled paths.
//! - `curve`: glyphs on curves: text along a path, a glyph flying on an S-curve, curved arrows.
//! - `reach`: how far effects draw outside an element's box, to size a surface's margin.
//! - `repeat`: children and ticks around a ring or along a path, sliding on springs.
//! - `display`: the flat display list that `vo-render` consumes. No tree, no GPU types.
//! - `timed`: elements drawn from the frame time: a scrolling waveform, a progress sweep.
//! - `bind`: props driven by the signal bus (audio level, pitch, bands), composed on springs.
//! - `dots`: dot grids whose dots come from a source drawn from the frame time.
//! - `emitter`: particle emitters and particle words, simulated by the renderer.
//! - `emitter_signals`: emitter inputs driven by the signal bus (level, pitch, onsets).
//! - `gpu`: elements drawn by app GPU code, such as a 3D scene.
//! - `glass`: the glass material and its display list item (FX06 to FX08).
//! - `roll`: digit roll, numbers whose changed digits roll like an odometer (FX04).
//! - `status_icon`: the record, stop, spinner, check and X icon as one continuous shape.
//! - `inline`: inline objects in live text: pause dots, gap waveforms, braces, chips.
//! - `lines`: live text broken into lines whose look follows their age (teleprompter).
//! - `swap`: word swap, a label that changes word by word, shared words sliding in place.
//! - `story`: labelled sequences whose phases hand on their state and velocities.
//! - `stage`: 2D layers as planes in one perspective camera: rotate_x, rotate_y,
//!   translate_z.
//! - `geom`: points, sizes, rects, colours and 2D transforms in logical units (`f32`).

pub mod ambient;
pub mod assets;
pub mod bars;
pub mod bind;
pub mod canvas;
pub mod curve;
pub mod display;
pub mod dots;
pub mod element;
pub mod emitter;
pub mod emitter_signals;
pub mod flow;
pub mod font;
pub mod geom;
pub mod glass;
pub mod gpu;
pub mod inline;
pub mod input;
pub mod inspect;
mod layout;
pub mod levels;
pub mod lines;
pub mod path;
pub mod reach;
pub mod repeat;
pub mod ribbon;
pub mod roll;
pub mod scene;
pub mod segments;
pub mod stage;
pub mod status_icon;
pub mod story;
pub mod swap;
pub mod text;
pub mod timed;
pub mod trace;

/// The layout engine, for `Element::layout` and the alignment types the builder takes.
pub use taffy;

pub use ambient::{Loop, LoopProp, Ripples};
pub use bind::Param;
pub use display::DisplayList;
pub use element::{Element, Key, Motion, Prop, TextStyle, Transition};
pub use font::{FontId, FontRegistry};
pub use reach::{surface_for, Insets};
pub use repeat::{Orient, Ticks};
pub use scene::{NodeId, Presence, Scene};
