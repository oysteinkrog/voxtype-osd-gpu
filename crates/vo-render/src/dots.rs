//! CPU side of the dot pipeline (`wgsl/dots.wgsl`): turns display-list dots into 16-byte GPU
//! instances.
//!
//! A dot is a filled circle with one colour, so it needs far less than a shape instance
//! (152 bytes): centre, radius and a packed colour. Work done here:
//! - Layer opacity goes into the colour's alpha, and the colour is packed as straight-alpha
//!   sRGB bytes. The shader decodes and premultiplies.
//! - Pixel snapping. When the layer maps axes to axes, the centre moves `snap` of the way to
//!   the nearest spot where the dot covers whole pixels symmetrically: a pixel centre for an
//!   odd diameter in pixels, a pixel corner for an even one. Dots at rest (`snap` 1) are crisp
//!   and all alike; dots in flight (`snap` 0) move smoothly.

use bytemuck::{Pod, Zeroable};
use vo_scene::display::DotInstance;
use vo_scene::geom::{Affine, Point};

/// One dot as the shader reads it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct GpuDot {
    /// Layer units.
    pub centre: [f32; 2],
    pub radius: f32,
    /// Straight-alpha sRGB, red in the low byte.
    pub color: u32,
    /// Ring width in layer units; 0 is a filled dot.
    pub ring: f32,
}

impl GpuDot {
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 4] =
            wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32, 2 => Uint32, 3 => Float32];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GpuDot>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        }
    }

    /// Converts `d`, drawn through `to_px` at `ppu` physical pixels per layer unit, with
    /// `opacity` multiplied into its alpha. `None` when nothing would be drawn.
    pub fn new(d: &DotInstance, to_px: &Affine, ppu: f32, opacity: f32) -> Option<GpuDot> {
        let a = (d.color.a * opacity).clamp(0.0, 1.0);
        if a <= 0.0 || d.radius <= 0.0 {
            return None;
        }
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        let color = byte(d.color.r) | byte(d.color.g) << 8 | byte(d.color.b) << 16 | byte(a) << 24;
        let centre = snap(d.centre, d.radius, d.snap, to_px, ppu);
        let ring = if d.ring < d.radius { d.ring } else { 0.0 };
        Some(GpuDot { centre: [centre.x, centre.y], radius: d.radius, color, ring })
    }
}

/// `centre` moved `amount` of the way to its snapped spot. Unchanged when the layer rotates
/// or skews, or `amount` is 0.
pub fn snap(centre: Point, radius: f32, amount: f32, to_px: &Affine, ppu: f32) -> Point {
    if amount <= 0.0 || !to_px.is_axis_aligned() || to_px.a == 0.0 || to_px.d == 0.0 {
        return centre;
    }
    let diameter = (2.0 * radius * ppu).round() as i64;
    let even = diameter > 0 && diameter % 2 == 0;
    let to_grid = |v: f32| if even { v.round() } else { v.floor() + 0.5 };
    let px = to_px.apply(centre);
    let (sx, sy) = (to_grid(px.x), to_grid(px.y));
    let k = amount.min(1.0);
    let x = px.x + (sx - px.x) * k;
    let y = px.y + (sy - px.y) * k;
    Point::new((x - to_px.e) / to_px.a, (y - to_px.f) / to_px.d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vo_scene::geom::Color;

    fn dot(x: f32, y: f32, r: f32, snap: f32) -> DotInstance {
        DotInstance { centre: Point::new(x, y), radius: r, snap, color: Color::WHITE, ring: 0.0 }
    }

    #[test]
    fn resting_dots_land_on_pixel_centres_or_corners() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let m = Affine::scale(scale);
            // An odd diameter in pixels sits on a pixel centre.
            let odd = 1.5 / scale;
            let g = GpuDot::new(&dot(10.37, 4.81, odd, 1.0), &m, scale, 1.0).unwrap();
            for v in g.centre {
                let px = v * scale;
                assert!((px - px.floor() - 0.5).abs() < 1e-3, "scale {scale}: {px}");
            }
            // An even one on a pixel corner.
            let even = 1.0 / scale;
            let g = GpuDot::new(&dot(10.37, 4.81, even, 1.0), &m, scale, 1.0).unwrap();
            for v in g.centre {
                let px = v * scale;
                assert!((px - px.round()).abs() < 1e-3, "scale {scale}: {px}");
            }
        }
    }

    #[test]
    fn moving_dots_are_not_snapped() {
        let g = GpuDot::new(&dot(10.37, 4.81, 1.5, 0.0), &Affine::scale(1.5), 1.5, 1.0).unwrap();
        assert_eq!(g.centre, [10.37, 4.81]);
        let half = GpuDot::new(&dot(10.2, 4.0, 0.5, 0.5), &Affine::IDENTITY, 1.0, 1.0).unwrap();
        assert!((half.centre[0] - 10.35).abs() < 1e-5, "{:?}", half.centre);
    }

    #[test]
    fn a_ring_as_wide_as_the_radius_is_a_filled_dot() {
        let ring = |w: f32| {
            let d = DotInstance { ring: w, ..dot(0.0, 0.0, 2.0, 0.0) };
            GpuDot::new(&d, &Affine::IDENTITY, 1.0, 1.0).unwrap().ring
        };
        assert_eq!(ring(0.5), 0.5);
        assert_eq!(ring(2.0), 0.0);
        assert_eq!(ring(0.0), 0.0);
    }

    #[test]
    fn colour_is_packed_with_opacity() {
        let d = DotInstance { color: Color::rgba(1.0, 0.0, 0.5, 0.8), ..dot(0.0, 0.0, 1.0, 0.0) };
        let g = GpuDot::new(&d, &Affine::IDENTITY, 1.0, 0.5).unwrap();
        assert_eq!(g.color & 0xff, 255);
        assert_eq!(g.color >> 8 & 0xff, 0);
        assert_eq!(g.color >> 16 & 0xff, 128);
        assert_eq!(g.color >> 24, 102);
        assert_eq!(GpuDot::new(&d, &Affine::IDENTITY, 1.0, 0.0), None);
        assert_eq!(std::mem::size_of::<GpuDot>(), 20);
    }
}
