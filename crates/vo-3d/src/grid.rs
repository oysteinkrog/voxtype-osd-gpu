//! A perspective grid floor (FX-152, the synthwave floor): lines that run to a vanishing point
//! and fade toward the horizon, scrolling toward the viewer, with a glow along the horizon, a
//! pool of light on the floor and a bright streak under it. `Element::gpu(key,
//! vo_3d::grid_floor(source))` reserves the box; the floor fills it below the horizon.
//!
//! The focal length is the stage camera's (`vo_scene::stage`), so the floor's perspective
//! matches tilted layers and 3D items. Each pixel finds where its ray meets the floor in
//! closed form: one quad over the box, no marching and no textures.
//!
//! The streak stands in for a reflection; the floor cannot read what is drawn above it. A
//! theme that wants its waveform mirrored draws a flipped, faint copy as a 2D layer.

use std::any::Any;
use std::fmt::Debug;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use vo_render::color::linear_premul;
use vo_render::{GpuPainter, PaintCx};
use vo_scene::geom::{Color, Size};
use vo_scene::gpu::GpuSource;

use crate::quad::{clip_to_scissor, QuadPass};

const GRID_WGSL: &str = include_str!("wgsl/grid_floor.wgsl");

pub fn grid_shader_source() -> &'static str {
    GRID_WGSL
}

/// A pool of light lying on the floor. Positions in logical pixels: `x` across from the
/// vanishing point, `depth` away from the eye.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridPool {
    pub x: f32,
    pub depth: f32,
    pub radius: f32,
    /// 0 for none; 1 triples the lines' brightness at its middle.
    pub strength: f32,
}

/// The floor at one moment. Lengths are logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridFrame {
    /// Where the horizon sits, as a fraction of the box height from its top.
    pub horizon: f32,
    /// Where the lines meet, as a fraction of the box width from its left.
    pub vanish_x: f32,
    /// How high the eye is above the floor. Lower looks flatter.
    pub eye_height: f32,
    /// Size of one grid square on the floor.
    pub cell: f32,
    /// How far the floor has moved toward the viewer: speed times time for a scroll.
    pub scroll: f32,
    /// Line width on screen.
    pub line_width: f32,
    /// The depth at which the lines have faded out toward the horizon.
    pub far: f32,
    pub pool: GridPool,
    /// Height of the glow along the horizon, and its strength (0 for none).
    pub horizon_glow: (f32, f32),
    /// Width of the bright streak under the pool, and its strength (0 for none).
    pub streak: (f32, f32),
    /// Line colours at the left and the right of the box.
    pub left: Color,
    pub right: Color,
    /// Colour of the horizon glow, the pool and the streak.
    pub haze: Color,
    /// The floor between the lines; its alpha is its opacity (0 leaves what is below).
    pub fill: Color,
}

impl Default for GridFrame {
    fn default() -> GridFrame {
        GridFrame {
            horizon: 0.35,
            vanish_x: 0.5,
            eye_height: 60.0,
            cell: 48.0,
            scroll: 0.0,
            line_width: 1.4,
            far: 2600.0,
            pool: GridPool { x: 0.0, depth: 600.0, radius: 260.0, strength: 0.8 },
            horizon_glow: (14.0, 0.7),
            streak: (40.0, 0.35),
            left: Color::hex(0xff3ad7, 1.0),
            right: Color::hex(0x35c8ff, 1.0),
            haze: Color::hex(0x9a5cff, 1.0),
            fill: Color::hex(0x07041a, 0.0),
        }
    }
}

/// What a grid floor element shows. `frame` must be pure in `t`.
pub trait GridSource: Debug + Send + Sync {
    fn frame(&self, t: f64, size: Size) -> GridFrame;

    /// The scene keeps waking for this element until `until`; a scrolling floor never rests.
    fn until(&self) -> f64 {
        f64::INFINITY
    }
}

/// A [`GridSource`] as the source of a GPU element.
#[derive(Debug)]
pub struct GridItem(Box<dyn GridSource>);

impl GpuSource for GridItem {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn until(&self) -> f64 {
        self.0.until()
    }
}

/// `source` as the source of `Element::gpu`. Draw it with [`GridPainter`] (added by
/// [`crate::install`]).
pub fn grid_floor(source: impl GridSource + 'static) -> Arc<dyn GpuSource> {
    Arc::new(GridItem(Box::new(source)))
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GridUniform {
    rect: [f32; 4],
    viewport: [f32; 4],
    view: [f32; 4],
    cells: [f32; 4],
    pool: [f32; 4],
    glow: [f32; 4],
    c: [[f32; 4]; 4],
}

/// Draws [`GridItem`] elements.
pub struct GridPainter {
    pass: QuadPass,
}

impl Default for GridPainter {
    fn default() -> GridPainter {
        GridPainter { pass: QuadPass::new("vo3d-grid", GRID_WGSL, "vs_grid", "fs_grid") }
    }
}

impl GpuPainter for GridPainter {
    fn paint(&mut self, cx: &mut PaintCx<'_>) -> bool {
        let Some(item) = cx.source.as_any().downcast_ref::<GridItem>() else { return false };
        if cx.opacity <= 0.0 {
            return true;
        }
        let f = item.0.frame(cx.t, Size::new(cx.rect.w, cx.rect.h));
        let [x0, y0, x1, y1] = cx.rect_px();
        let Some(q) = clip_to_scissor(cx, [x0, y0, x1, y1]) else { return true };
        let k = if cx.rect.w > 0.0 { (x1 - x0) / cx.rect.w } else { 1.0 };
        let lin = |c: Color| {
            let p = linear_premul(c, 1.0);
            [p[0], p[1], p[2], c.a]
        };
        let vp = [x0 + f.vanish_x * (x1 - x0), y0 + f.horizon * (y1 - y0)];
        let u = GridUniform {
            rect: [x0, y0, x1 - x0, y1 - y0],
            viewport: [cx.size[0] as f32, cx.size[1] as f32, cx.opacity, cx.encode_srgb as u32 as f32],
            view: [vp[0], vp[1], cx.stage.perspective, f.eye_height * k],
            cells: [f.cell * k, f.scroll * k, f.line_width * k, f.far * k],
            pool: [f.pool.x * k, f.pool.depth * k, f.pool.radius * k, f.pool.strength],
            glow: [f.horizon_glow.0 * k, f.horizon_glow.1, f.streak.0 * k, f.streak.1],
            c: [lin(f.left), lin(f.right), lin(f.haze), lin(f.fill)],
        };
        self.pass.draw(cx, q, bytemuck::bytes_of(&u));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_shader_validates_in_naga() {
        let src = grid_shader_source();
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }

    #[test]
    fn uniform_matches_the_shader() {
        assert_eq!(std::mem::size_of::<GridUniform>(), 10 * 16);
    }
}
