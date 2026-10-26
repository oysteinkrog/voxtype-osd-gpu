//! CPU side of the vector pipeline (`wgsl/vectors.wgsl`): turns `DisplayList::vectors` into
//! GPU instances and their segments into a storage buffer, both in physical pixels.

use bytemuck::{Pod, Zeroable};
use vo_scene::display::DisplayList;
use vo_scene::geom::{Affine, Point};
use vo_scene::path::{VectorInstance, VectorKind};

use crate::color::linear_premul;

const COMMON_WGSL: &str = include_str!("wgsl/common.wgsl");
const VECTORS_WGSL: &str = include_str!("wgsl/vectors.wgsl");

/// The vector shader: `common.wgsl` and `vectors.wgsl`.
pub fn vector_shader_source() -> String {
    [crate::hot::wgsl("common.wgsl", COMMON_WGSL), crate::hot::wgsl("vectors.wgsl", VECTORS_WGSL)].join("\n")
}

/// One contour or blob as the shader reads it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct GpuVector {
    /// Kind, first segment, segment count, unused.
    pub head: [u32; 4],
    /// Bounding box in physical pixels.
    pub bbox: [f32; 4],
    /// Half width and glow radius in pixels, colour ramp start and end in arc length.
    pub params: [f32; 4],
    pub c0: [f32; 4],
    pub c1: [f32; 4],
    pub a: [f32; 4],
    pub b: [f32; 4],
}

impl GpuVector {
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
            0 => Uint32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4,
            4 => Float32x4, 5 => Float32x4, 6 => Float32x4,
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GpuVector>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        }
    }

    /// Converts `v` through `to_px` at `ppu` pixels per layer unit, appending its segments to
    /// `segs` (two texels each). `None` when nothing would be drawn.
    pub fn new(
        list: &DisplayList,
        v: &VectorInstance,
        to_px: &Affine,
        ppu: f32,
        opacity: f32,
        segs: &mut Vec<[f32; 4]>,
    ) -> Option<GpuVector> {
        let c0 = linear_premul(v.c0, opacity);
        let c1 = linear_premul(v.c1, opacity);
        if c0[3] <= 0.0 && c1[3] <= 0.0 {
            return None;
        }
        let glow = v.glow.max(0.0) * ppu;
        // Room for the stroke, the glow (out to where it is under 1/255) and the AA ramp.
        let pad_glow = if glow > 0.0 { 2.5 * glow } else { 0.0 };
        let mut g = GpuVector { c0, c1, ..Default::default() };
        match v.kind {
            VectorKind::Blob => {
                let map = |c: [f32; 4]| {
                    let p = to_px.apply(Point::new(c[0], c[1]));
                    [p.x, p.y, c[2] * ppu, c[3] * ppu]
                };
                g.a = map(v.a);
                g.b = map(v.b);
                g.head = [1, 0, 0, 0];
                let pad = g.a[3] + pad_glow + 1.0;
                g.bbox = [
                    (g.a[0] - g.a[2]).min(g.b[0] - g.b[2]) - pad,
                    (g.a[1] - g.a[2]).min(g.b[1] - g.b[2]) - pad,
                    (g.a[0] + g.a[2]).max(g.b[0] + g.b[2]) + pad,
                    (g.a[1] + g.a[2]).max(g.b[1] + g.b[2]) + pad,
                ];
                g.params = [0.0, glow, 0.0, 0.0];
            }
            VectorKind::Stroke => {
                let range = v.segs[0] as usize..(v.segs[0] + v.segs[1]) as usize;
                let src = list.vector_segs.get(range)?;
                if src.is_empty() || v.width <= 0.0 {
                    return None;
                }
                let first = (segs.len() / 2) as u32;
                let mut b = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
                for s in src {
                    let [p0, p1, p2] = [s.p0, s.p1, s.p2].map(|p| to_px.apply(p));
                    // The control polygon holds the curve.
                    for p in [p0, p1, p2] {
                        b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
                    }
                    segs.push([p0.x, p0.y, p1.x, p1.y]);
                    segs.push([p2.x, p2.y, s.s0, s.s1]);
                }
                let hw = 0.5 * v.width * ppu;
                let pad = hw + pad_glow + 1.0;
                g.bbox = [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad];
                g.head = [0, first, src.len() as u32, 0];
                g.params = [hw, glow, v.ramp[0], v.ramp[1]];
            }
        }
        Some(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_validates_in_naga() {
        let src = vector_shader_source();
        let module = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }
}
