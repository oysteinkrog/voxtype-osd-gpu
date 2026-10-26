//! GPU renderer for the voxtype overlay: draws `vo_scene` display lists with wgpu.
//!
//! - `renderer`: [`Renderer`] and [`Target`], the display list to texture path.
//! - `shapes`: the CPU side of the SDF shape uber-shader.
//! - `text`: the Slug glyph path.
//! - `glyph_points`: points sampled from glyph outlines, for text made of particles.
//! - `offscreen`: readback to CPU memory for goldens and export.
//! - `color`: sRGB, linear and OKLab conversions.
//! - `cpu` (feature `cpu`): the same display list drawn on the CPU, for the first frame.

pub mod color;
#[cfg(feature = "cpu")]
pub mod cpu;
pub mod glyph_points;
pub mod offscreen;
pub mod renderer;
pub mod shapes;
pub mod text;

pub use offscreen::Offscreen;
pub use renderer::{Renderer, Target};
