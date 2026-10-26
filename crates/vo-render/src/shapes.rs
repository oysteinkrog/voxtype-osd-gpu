//! CPU side of the SDF shape uber-shader (`wgsl/shapes.wgsl`): turns display-list shapes
//! into GPU instances.
//!
//! Work done here so the shader stays simple:
//! - Paint stops become OKLab with straight alpha, with the layer's opacity multiplied in.
//! - `SHAPE_HAIRLINE` becomes a stroke one physical pixel wide. When the layer maps axes to
//!   axes, the outline is moved by under half a pixel so the hairline's centre sits on a pixel
//!   centre, which gives one row of full coverage with no partial neighbours.

use bytemuck::{Pod, Zeroable};
use vo_scene::display::{Paint, ShapeInstance, ShapeKind, SHAPE_HAIRLINE};
use vo_scene::geom::Affine;

use crate::color::oklab_alpha;

/// One shape as the shader reads it. Field order matches [`GpuShape::layout`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct GpuShape {
    /// Shape kind in bits 0..8, fill paint kind in 8..16, stroke paint kind in 16..24.
    pub kinds: u32,
    /// Layer units. Already one physical pixel for a hairline.
    pub stroke_width: f32,
    pub geom: [f32; 4],
    pub radii: [f32; 4],
    pub fill: GpuPaint,
    pub stroke: GpuPaint,
}

/// Paint stops as OKLab plus straight alpha, and the gradient ends `p0.xy, p1.xy`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct GpuPaint {
    pub c0: [f32; 4],
    pub c1: [f32; 4],
    pub ends: [f32; 4],
}

impl GpuPaint {
    fn new(p: &Paint, opacity: f32) -> GpuPaint {
        GpuPaint {
            c0: oklab_alpha(p.c0, opacity),
            c1: oklab_alpha(p.c1, opacity),
            ends: [p.p0.x, p.p0.y, p.p1.x, p.p1.y],
        }
    }
}

impl GpuShape {
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 10] = wgpu::vertex_attr_array![
            0 => Uint32, 1 => Float32, 2 => Float32x4, 3 => Float32x4,
            4 => Float32x4, 5 => Float32x4, 6 => Float32x4,
            7 => Float32x4, 8 => Float32x4, 9 => Float32x4,
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GpuShape>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        }
    }

    /// Converts `s`, drawn through `to_px` (layer space to physical pixels) at `ppu` physical
    /// pixels per layer unit, with `opacity` multiplied into both paints. `None` when nothing
    /// would be drawn.
    pub fn new(s: &ShapeInstance, to_px: &Affine, ppu: f32, opacity: f32) -> Option<GpuShape> {
        let hairline = s.flags & SHAPE_HAIRLINE != 0;
        let fill = if s.kind == ShapeKind::Line { Paint::NONE } else { s.fill };
        let stroke_width = if hairline { 1.0 / ppu } else { s.stroke_width.max(0.0) };
        let stroke = if stroke_width > 0.0 { s.stroke } else { Paint::NONE };
        if opacity <= 0.0 || !(fill.is_visible() || stroke.is_visible()) {
            return None;
        }
        let mut geom = s.geom;
        if hairline && stroke.is_visible() && to_px.is_axis_aligned() {
            snap_hairline(s.kind, &mut geom, to_px);
        }
        let kinds = s.kind as u32 | (fill.kind as u32) << 8 | (stroke.kind as u32) << 16;
        Some(GpuShape {
            kinds,
            stroke_width,
            geom,
            radii: s.radii,
            fill: GpuPaint::new(&fill, opacity),
            stroke: GpuPaint::new(&stroke, opacity),
        })
    }
}

/// `x` in physical pixels moved to the nearest pixel centre.
fn to_centre(x: f32) -> f32 {
    x.floor() + 0.5
}

/// Moves the outline so a one-pixel stroke centred on it covers whole pixels. `to_px` has no
/// rotation or skew. A rect keeps its size rounded to whole pixels; a circle moves its centre
/// and keeps its radius; a horizontal or vertical line moves across its length only.
fn snap_hairline(kind: ShapeKind, geom: &mut [f32; 4], to_px: &Affine) {
    let (sx, sy) = (to_px.a, to_px.d);
    if sx == 0.0 || sy == 0.0 {
        return;
    }
    let px_x = |x: f32| sx * x + to_px.e;
    let px_y = |y: f32| sy * y + to_px.f;
    let back_x = |x: f32| (x - to_px.e) / sx;
    let back_y = |y: f32| (y - to_px.f) / sy;
    match kind {
        ShapeKind::RRect | ShapeKind::Capsule => {
            let (x0, x1) = (px_x(geom[0]), px_x(geom[0] + geom[2]));
            let (y0, y1) = (px_y(geom[1]), px_y(geom[1] + geom[3]));
            // Snap the near edge, then keep the size a whole number of pixels so both edges
            // land on centres.
            let snap_span = |a: f32, b: f32| {
                let lo = to_centre(a.min(b));
                let len = (a - b).abs().round();
                (lo, lo + len)
            };
            let (nx0, nx1) = snap_span(x0, x1);
            let (ny0, ny1) = snap_span(y0, y1);
            let (lx0, lx1) = (back_x(nx0), back_x(nx1));
            let (ly0, ly1) = (back_y(ny0), back_y(ny1));
            *geom = [lx0.min(lx1), ly0.min(ly1), (lx1 - lx0).abs(), (ly1 - ly0).abs()];
        }
        ShapeKind::Circle => {
            geom[0] = back_x(to_centre(px_x(geom[0])));
            geom[1] = back_y(to_centre(px_y(geom[1])));
        }
        // A band has no stroke worth snapping.
        ShapeKind::Band => {}
        ShapeKind::Line => {
            if geom[1] == geom[3] {
                let y = back_y(to_centre(px_y(geom[1])));
                geom[1] = y;
                geom[3] = y;
            } else if geom[0] == geom[2] {
                let x = back_x(to_centre(px_x(geom[0])));
                geom[0] = x;
                geom[2] = x;
            }
        }
    }
}

/// Physical pixels per layer unit for a layer-to-pixel transform: the square root of the
/// area scale, exact for rotation with uniform scale.
pub fn pixels_per_unit(to_px: &Affine) -> f32 {
    (to_px.a * to_px.d - to_px.b * to_px.c).abs().sqrt().max(1e-6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vo_scene::display::PaintKind;
    use vo_scene::geom::{Color, Point, RRect, Rect};

    #[test]
    fn hairline_rect_edges_land_on_pixel_centres() {
        let s = ShapeInstance::rrect(RRect::new(Rect::new(10.3, 4.0, 50.0, 20.2), 3.0), Paint::NONE)
            .with_hairline(Paint::solid(Color::WHITE));
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let m = Affine::scale(scale);
            let g = GpuShape::new(&s, &m, scale, 1.0).unwrap();
            for edge in [g.geom[0], g.geom[0] + g.geom[2]] {
                let px = edge * scale;
                assert!((px - px.floor() - 0.5).abs() < 1e-3, "scale {scale}: x edge at {px}");
            }
            for edge in [g.geom[1], g.geom[1] + g.geom[3]] {
                let px = edge * scale;
                assert!((px - px.floor() - 0.5).abs() < 1e-3, "scale {scale}: y edge at {px}");
            }
            assert!((g.stroke_width * scale - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn invisible_shapes_are_dropped() {
        let s = ShapeInstance::circle(Point::new(5.0, 5.0), 3.0, Paint::NONE);
        assert_eq!(GpuShape::new(&s, &Affine::IDENTITY, 1.0, 1.0), None);
        let s = ShapeInstance::circle(Point::new(5.0, 5.0), 3.0, Paint::solid(Color::WHITE));
        assert_eq!(GpuShape::new(&s, &Affine::IDENTITY, 1.0, 0.0), None);
        // A stroke paint with zero width and no hairline flag draws nothing.
        let s =
            ShapeInstance::circle(Point::new(5.0, 5.0), 3.0, Paint::NONE).with_stroke(Paint::solid(Color::WHITE), 0.0);
        assert_eq!(GpuShape::new(&s, &Affine::IDENTITY, 1.0, 1.0), None);
    }

    #[test]
    fn opacity_goes_into_alpha_and_kinds_are_packed() {
        let s =
            ShapeInstance::line(Point::ZERO, Point::new(10.0, 0.0), 2.0, Paint::solid(Color::rgba(1.0, 0.0, 0.0, 0.8)));
        let g = GpuShape::new(&s, &Affine::IDENTITY, 1.0, 0.5).unwrap();
        assert!((g.stroke.c0[3] - 0.4).abs() < 1e-6);
        assert_eq!(g.kinds, ShapeKind::Line as u32 | (PaintKind::Solid as u32) << 16);
    }
}
