//! CPU side of the vector pipeline (`wgsl/vectors.wgsl`): turns `DisplayList::vectors` into
//! GPU instances and their segments into a storage buffer, both in physical pixels.

use bytemuck::{Pod, Zeroable};
use vo_scene::display::{DisplayList, MATERIAL_SHIFT};
use vo_scene::geom::{Affine, Point};
use vo_scene::path::{VectorInstance, VectorKind, VectorPaint, VECTOR_HAIRLINE};

use crate::color::oklab_alpha;

const COMMON_WGSL: &str = include_str!("wgsl/common.wgsl");
const VECTORS_WGSL: &str = include_str!("wgsl/vectors.wgsl");

/// The vector shader: `common.wgsl` and `vectors.wgsl`.
pub fn vector_shader_source() -> String {
    vector_shader_source_with(&[])
}

/// [`vector_shader_source`] with app material functions (FX15).
pub fn vector_shader_source_with(materials: &[crate::material::MaterialFn]) -> String {
    [
        crate::hot::wgsl("common.wgsl", COMMON_WGSL).into_owned(),
        crate::hot::wgsl("vectors.wgsl", VECTORS_WGSL).into_owned(),
        crate::material::materials_source(materials),
    ]
    .join("\n")
}

/// Texels per segment in the storage buffer: `p0 p1`, `p2 s0 s1`, `w0 w1`.
pub const SEG_TEXELS: usize = 3;

/// One contour, fill or blob as the shader reads it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct GpuVector {
    /// Kind, first segment, segment count, paint kind (0 along, 1 linear, 2 conic, 3 conic
    /// mirrored).
    pub head: [u32; 4],
    /// Bounding box in target pixels.
    pub bbox: [f32; 4],
    /// Half width and glow radius in pixels, colour ramp start and end.
    pub params: [f32; 4],
    /// OKLab and straight alpha, opacity multiplied in.
    pub c0: [f32; 4],
    pub c1: [f32; 4],
    /// Blob circles in pixels, `cx, cy, r, k`; for a stroke or fill, `a` is the paint geometry
    /// in pixels (linear: `p0, p1`; conic: centre, start angle).
    pub a: [f32; 4],
    pub b: [f32; 4],
    /// Material id (FX15), 0 for none; the rest is unused.
    pub material: [u32; 4],
}

impl GpuVector {
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 8] = wgpu::vertex_attr_array![
            0 => Uint32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4,
            4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Uint32x4,
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GpuVector>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        }
    }

    /// Converts `v` through `to_px` at `ppu` pixels per layer unit, appending its segments to
    /// `segs` ([`SEG_TEXELS`] texels each). `None` when nothing would be drawn.
    pub fn new(
        list: &DisplayList,
        v: &VectorInstance,
        to_px: &Affine,
        ppu: f32,
        opacity: f32,
        segs: &mut Vec<[f32; 4]>,
    ) -> Option<GpuVector> {
        if v.c0.a * opacity <= 0.0 && v.c1.a * opacity <= 0.0 {
            return None;
        }
        let material = v.flags >> MATERIAL_SHIFT;
        let mat = list.material(material);
        // A material draws its own light: the plain glow is off under one.
        let glow = if mat.is_some() { 0.0 } else { v.glow.max(0.0) * ppu };
        // Room for the glow out to where it is under 1/255 or the material's reach, and the
        // AA ramp.
        let pad_glow = if glow > 0.0 { 2.5 * glow } else { 0.0 } + mat.map_or(0.0, |m| m.reach.max(0.0) * ppu);
        let mut g = GpuVector {
            c0: oklab_alpha(v.c0, opacity),
            c1: oklab_alpha(v.c1, opacity),
            material: [if mat.is_some() { material } else { 0 }, 0, 0, 0],
            ..Default::default()
        };
        let px = |p: Point| to_px.apply(p);
        let paint = match v.paint {
            VectorPaint::Along => 0,
            VectorPaint::Linear { p0, p1 } => {
                let (a, b) = (px(p0), px(p1));
                g.a = [a.x, a.y, b.x, b.y];
                1
            }
            VectorPaint::Conic { centre, start, mirror } => {
                let c = px(centre);
                // The layer's own rotation turns the gradient with it.
                g.a = [c.x, c.y, start + to_px.b.atan2(to_px.a), 0.0];
                if mirror {
                    3
                } else {
                    2
                }
            }
            VectorPaint::Stripes { normal, period, duty, phase } => {
                // Stripe position u = dot(p, n) / period in layer units is dot(q, n') - o in
                // pixels, with n' = M^-T n / period and o = dot(T, n') for q = M p + T.
                let m = to_px;
                let det = m.a * m.d - m.b * m.c;
                if det.abs() < 1e-12 || period <= 0.0 {
                    return None;
                }
                let (nx, ny) = (normal.x / period, normal.y / period);
                let n = [(m.d * nx - m.b * ny) / det, (m.a * ny - m.c * nx) / det];
                g.a = [n[0], n[1], m.e * n[0] + m.f * n[1] + phase, duty.clamp(0.0, 1.0)];
                4
            }
        };
        match v.kind {
            VectorKind::Blob => {
                let map = |c: [f32; 4]| {
                    let p = px(Point::new(c[0], c[1]));
                    [p.x, p.y, c[2] * ppu, c[3] * ppu]
                };
                g.a = map(v.a);
                g.b = map(v.b);
                // Blobs mix c0 to c1 between their circles; `paint` does not apply.
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
            VectorKind::Stroke | VectorKind::Fill => {
                let range = v.segs[0] as usize..(v.segs[0] + v.segs[1]) as usize;
                let src = list.vector_segs.get(range)?;
                let fill = v.kind == VectorKind::Fill;
                let hairline = v.flags & VECTOR_HAIRLINE != 0;
                if src.is_empty() || (!fill && !hairline && v.width <= 0.0) {
                    return None;
                }
                let first = (segs.len() / SEG_TEXELS) as u32;
                let mut b = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
                let mut wmax = 0.0f32;
                for s in src {
                    let [p0, p1, p2] = [s.p0, s.p1, s.p2].map(px);
                    // The control polygon holds the curve.
                    for p in [p0, p1, p2] {
                        b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
                    }
                    wmax = wmax.max(s.w[0]).max(s.w[1]);
                    segs.push([p0.x, p0.y, p1.x, p1.y]);
                    segs.push([p2.x, p2.y, s.s0, s.s1]);
                    segs.push([s.w[0], s.w[1], 0.0, 0.0]);
                }
                let hw = if fill {
                    0.0
                } else if hairline {
                    0.5
                } else {
                    0.5 * v.width * ppu
                };
                let pad = hw * wmax.max(1.0) + pad_glow + 1.0;
                g.bbox = [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad];
                g.head = [if fill { 2 } else { 0 }, first, src.len() as u32, paint];
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
