//! GPU renderer for the voxtype overlay: draws `vo_scene` display lists with wgpu.
//!
//! - `renderer`: [`Renderer`] and [`Target`], the display list to texture path.
//! - `shapes`: the CPU side of the SDF shape uber-shader.
//! - `dots`: the CPU side of the dot pipeline.
//! - `text`: the Slug glyph path.
//! - `vector`: the CPU side of the vector stroke and blob pipeline.
//! - `particles`: GPU particles, free and word-forming; `glyph_points` samples glyphs for them.
//! - `glass`: glass panels, their animated rim and the compositor blur region.
//! - `material`: material blocks for shapes, glyphs and strokes, and app material WGSL (FX15).
//! - `displace`: a layer's backdrop drawn back with each pixel moved (EG-03).
//! - `blur`: layer blur and frost, a two-pass Gaussian (EG-04).
//! - `gpu`: painters for GPU elements, app GPU code drawn between two passes.
//! - `offscreen`: readback to CPU memory for goldens and export.
//! - `hot`: WGSL read from disk when `VO_SHADER_DIR` is set, for live shader reload.
//! - `color`: sRGB, linear and OKLab conversions.
//! - `cpu` (feature `cpu`): the same display list drawn on the CPU, for the first frame.

pub mod blur;
pub mod color;
#[cfg(feature = "cpu")]
pub mod cpu;
pub mod displace;
pub mod dots;
pub mod glass;
pub mod glyph_points;
pub mod gpu;
pub mod hot;
pub mod material;
pub mod offscreen;
pub mod particle_layer;
pub mod particle_text;
pub mod particles;
pub mod profile;
pub mod renderer;
pub mod shapes;
pub mod text;
pub mod vector;

pub use gpu::{BackdropView, GpuPainter, PaintCx, StagePx};
pub use offscreen::Offscreen;
pub use renderer::{Quality, Renderer, Target};
