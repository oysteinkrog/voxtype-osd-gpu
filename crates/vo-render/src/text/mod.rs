//! Text rendering: Slug-style analytic glyph coverage taken from HarfBuzz
//! `hb-gpu` (encoder ported to Rust, WGSL copied), see `docs/p0/slug-design.md`.

pub mod slug_encode;
pub mod slug_gpu;
