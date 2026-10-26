//! GPU renderer for the voxtype overlay: draws `vo_scene` display lists with wgpu.
//!
//! - `renderer`: [`Renderer`] and [`Target`], the display list to texture path.
//! - `shapes`: the CPU side of the SDF shape uber-shader.
//! - `dots`: the CPU side of the dot pipeline.
//! - `text`: the Slug glyph path.
//! - `vector`: the CPU side of the vector stroke and blob pipeline.
//! - `particles`: GPU particles, free and word-forming; `glyph_points` samples glyphs for them.
//! - `offscreen`: readback to CPU memory for goldens and export.
//! - `hot`: WGSL read from disk when `VO_SHADER_DIR` is set, for live shader reload.
//! - `color`: sRGB, linear and OKLab conversions.
//! - `cpu` (feature `cpu`): the same display list drawn on the CPU, for the first frame.

pub mod color;
#[cfg(feature = "cpu")]
pub mod cpu;
pub mod dots;
pub mod glyph_points;
pub mod hot;
pub mod offscreen;
pub mod particles;
pub mod renderer;
pub mod shapes;
pub mod text;
pub mod vector;

pub use offscreen::Offscreen;
pub use renderer::{Renderer, Target};
