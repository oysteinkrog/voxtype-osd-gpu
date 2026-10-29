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
//! - `svg`: a small SVG importer for icons and logos.
//! - `canvas`: elements drawn by app code from the frame time.
//! - `path`: vector paths: strokes with arc length, trim, morph, filled paths.
//! - `curve`: glyphs on curves: text along a path, a glyph flying on an S-curve, curved arrows.
//! - `reach`: how far effects draw outside an element's box, to size a surface's margin.
//! - `repeat`: children and ticks around a ring or along a path, sliding on springs.
//! - `frame`: cut panel frames: chamfers, notches, cut-outs, stripes, hatching and grids.
//! - `graticule`: a plot area with minor and major grids, ticks and number labels.
//! - `spectrum`: a radial spectrum, bars round a ring from the signal bus's mel bands.
//! - `container`: a dot, pill or card container whose size and radius spring, content clipped.
//! - `meters`: a progress hairline that never moves backwards, and a dismiss countdown ring.
//! - `reveal`: a frame that draws on from its corners, hairlines and brackets, then content.
//! - `erase`: cancel erase, a strike through the text and the words leaving last first.
//! - `displace`: a layer redraws what is under it with each pixel moved: ripples, noise,
//!   curl, a page curl or a texture of offsets (EG-03).
//! - `display`: the flat display list that `vo-render` consumes. No tree, no GPU types.
//! - `timed`: elements drawn from the frame time: a scrolling waveform, a progress sweep.
//! - `bind`: props driven by the signal bus (audio level, pitch, bands), composed on springs.
//! - `dots`: dot grids whose dots come from a source drawn from the frame time.
//! - `emitter`: particle emitters and particle words, simulated by the renderer.
//! - `emitter_signals`: emitter inputs driven by the signal bus (level, pitch, onsets).
//! - `final_pass`: a reading pass across live text, placed by the finishing fraction.
//! - `gpu`: elements drawn by app GPU code, such as a 3D scene.
//! - `glass`: the glass material and its display list item (FX06 to FX08).
//! - `material`: materials for shapes, glyphs and strokes: neon, hologram, chrome, gradient
//!   (FX15).
//! - `roll`: digit roll, numbers whose changed digits roll like an odometer (FX04).
//! - `status_icon`: the record, stop, spinner, check and X icon as one continuous shape.
//! - `inline`: inline objects in live text: pause dots, gap waveforms, braces, chips.
//! - `hearing`: the popup hears before it reads: pre-echo blobs per onset, pause ticks that
//!   gather into a hollow comma, and the exhale in long quiet (MO-11, MO-12, MO-30).
//! - `lines`: live text broken into lines whose look follows their age (teleprompter).
//! - `typo`: per-word typographic channels on live text: ink, weight, tracking, baseline and
//!   fill on springs, clamped on the reading line (MO-03).
//! - `word_voice`: what the voice did while each word was said, from the signal bus, and
//!   how it maps to the typographic channels (MO-04).
//! - `swap`: word swap, a label that changes word by word, shared words sliding in place.
//! - `story`: labelled sequences whose phases hand on their state and velocities.
//! - `stage`: 2D layers as planes in one perspective camera: rotate_x, rotate_y,
//!   translate_z.
//! - `geom`: points, sizes, rects, colours and 2D transforms in logical units (`f32`).

pub mod ambient;
pub mod arrival;
pub mod assets;
pub mod bars;
pub mod bind;
pub mod canvas;
pub mod container;
pub mod curve;
pub mod depth_stack;
pub mod displace;
pub mod display;
pub mod dots;
pub mod element;
pub mod emitter;
pub mod emitter_signals;
pub mod erase;
pub mod final_pass;
pub mod flow;
pub mod fluid;
pub mod font;
pub mod frame;
pub mod geom;
pub mod glass;
pub mod glyph_morph;
pub mod gpu;
pub mod graticule;
pub mod hearing;
pub mod inline;
pub mod input;
pub mod inspect;
pub mod lattice;
mod layout;
pub mod levels;
pub mod lines;
pub mod material;
pub mod meters;
pub mod path;
pub mod reach;
pub mod repeat;
pub mod reveal;
pub mod ribbon;
pub mod roll;
pub mod scene;
pub mod segments;
pub mod snap;
pub mod spectrum;
pub mod stage;
pub mod status_icon;
pub mod story;
pub mod stroke_write;
pub mod svg;
pub mod swap;
pub mod text;
pub mod timed;
pub mod trace;
pub mod typo;
pub mod water;
pub mod word_voice;

/// The layout engine, for `Element::layout` and the alignment types the builder takes.
pub use taffy;

pub use ambient::{Loop, LoopProp, Ripples};
pub use bind::Param;
pub use display::DisplayList;
pub use element::{Element, Key, MarkShape, Motion, Prop, TextStyle, Transition, WordMark};
pub use font::{FontId, FontRegistry};
pub use frame::{Corner, Edge, Frame, Grid, Notch, Stripes};
pub use graticule::{Axis, Graticule};
pub use reach::{surface_for, Insets};
pub use repeat::{Orient, Ticks};
pub use scene::{NodeId, Presence, Scene};
pub use spectrum::{Bands, Spectrum};
