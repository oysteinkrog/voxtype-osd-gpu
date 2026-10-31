//! CPU side of the vector pipeline (`wgsl/vectors.wgsl`): turns `DisplayList::vectors` into
//! GPU instances and their segments into a storage buffer, both in physical pixels.

use bytemuck::{Pod, Zeroable};
use vo_scene::display::{DisplayList, MATERIAL_SHIFT};
use vo_scene::geom::{Affine, Point};
use vo_scene::path::{VectorInstance, VectorKind, VectorPaint, VECTOR_HAIRLINE, VECTOR_TAPER};

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
    /// Material id (FX15), 0 for none; 1 when the stroke has a width table
    /// (`VECTOR_TAPER`), else 0; the rest is unused.
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
            material: [if mat.is_some() { material } else { 0 }, (v.flags & VECTOR_TAPER != 0) as u32, 0, 0],
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
            VectorPaint::Grid { origin, cell, width, dots } => {
                // Two stripe axes as above: a = the two normals over the cell in pixels,
                // b = their offsets and the line width over the cell (negative: axis off).
                let m = to_px;
                let det = m.a * m.d - m.b * m.c;
                if det.abs() < 1e-12 {
                    return None;
                }
                let axis = |nx: f32, ny: f32, o: f32| {
                    let n = [(m.d * nx - m.b * ny) / det, (m.a * ny - m.c * nx) / det];
                    (n, m.e * n[0] + m.f * n[1] + o)
                };
                let (n1, o1) = if cell.x > 0.0 { axis(1.0 / cell.x, 0.0, origin.x / cell.x) } else { ([0.0; 2], 0.0) };
                let (n2, o2) = if cell.y > 0.0 { axis(0.0, 1.0 / cell.y, origin.y / cell.y) } else { ([0.0; 2], 0.0) };
                let duty = |c: f32| if c > 0.0 { (width / c).clamp(0.0, 1.0) } else { -1.0 };
                g.a = [n1[0], n1[1], n2[0], n2[1]];
                g.b = [o1, o2, duty(cell.x), duty(cell.y)];
                if dots {
                    6
                } else {
                    5
                }
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

/// Side of the tiles a large stroke is cut into, in pixels.
const STROKE_TILE: f32 = 16.0;
/// Side of the tiles a large fill is cut into, in pixels.
const FILL_TILE: f32 = 64.0;
/// A stroke or fill with a smaller box than this many square pixels is drawn whole.
const TILED_AREA: f32 = 128.0 * 128.0;
/// A fill's tile this far inside its edge, past the antialiasing ramp, is drawn solid.
const SOLID_GAP: f32 = 16.0;
/// `head[0]` of a fill tile that is all inside: the shader skips the segments.
pub const KIND_SOLID: u32 = 3;

/// What a tile of a large stroke or fill needs.
#[derive(Clone, Copy, PartialEq)]
enum Tile {
    /// Nothing draws there.
    Empty,
    /// Near an edge: the full shader.
    Edge,
    /// Inside a fill, away from its edges: solid paint.
    Solid,
}

/// Pushes `g` to `out`, a large stroke or fill as several instances over the tiles of its
/// bounding box: the tiles near its segments with the full shader, a fill's tiles well
/// inside it as solid paint, and none where nothing draws. Each run of like tiles along a
/// row is one instance, and a stroke's run reads only the segments that reach it (copied to
/// the end of `segs`). The shader gives each pixel the same value either way: a pixel takes
/// its distance from the nearest segment, and a segment that does not reach a tile is never
/// the nearest there. This only skips work, such as the inside of a panel's border or the
/// segment loop inside a big panel. The tiles share their edges exactly, so no pixel is
/// drawn twice. `segs` is the segment buffer `GpuVector::new` appended to.
pub fn push_tiled(g: GpuVector, segs: &mut Vec<[f32; 4]>, out: &mut Vec<GpuVector>) {
    let [x0, y0, x1, y1] = g.bbox;
    let (first, count) = (g.head[1] as usize, g.head[2] as usize);
    let fill = g.head[0] == 2;
    let size = if fill { FILL_TILE } else { STROKE_TILE };
    let (cols, rows) = (((x1 - x0) / size).ceil(), ((y1 - y0) / size).ceil());
    // Blobs are small; a material's seed is per instance; a glowing fill draws outside its
    // edge and paint along a fill's edge needs the segments everywhere.
    let skip = !(g.head[0] == 0 || fill)
        || g.material[0] != 0
        || (fill && (g.params[1] > 0.0 || g.head[3] == 0))
        || (x1 - x0) * (y1 - y0) < TILED_AREA
        || cols * rows * count as f32 > 400_000.0;
    let src = segs.get(first * SEG_TEXELS..(first + count) * SEG_TEXELS).map(<[_]>::to_vec);
    let Some(src) = src.filter(|_| !skip) else {
        out.push(g);
        return;
    };
    // Each segment's control polygon holds it; the bounding box is their union grown by the
    // stroke's reach.
    let curves: Vec<[[f32; 2]; 3]> = src
        .as_chunks::<SEG_TEXELS>()
        .0
        .iter()
        .map(|s| [[s[0][0], s[0][1]], [s[0][2], s[0][3]], [s[1][0], s[1][1]]])
        .collect();
    let hulls: Vec<[f32; 4]> = curves
        .iter()
        .map(|c| {
            let (xs, ys) = (c.map(|p| p[0]), c.map(|p| p[1]));
            let min = |v: [f32; 3]| v[0].min(v[1]).min(v[2]);
            let max = |v: [f32; 3]| v[0].max(v[1]).max(v[2]);
            [min(xs), min(ys), max(xs), max(ys)]
        })
        .collect();
    let inner = hulls.iter().fold([f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY], |b, h| {
        [b[0].min(h[0]), b[1].min(h[1]), b[2].max(h[2]), b[3].max(h[3])]
    });
    let pad = (inner[0] - x0).max(inner[1] - y0).max(x1 - inner[2]).max(y1 - inner[3]).max(0.0);
    let near = if fill { pad + SOLID_GAP } else { pad };
    let reaches = |h: &[f32; 4], r: [f32; 4], by: f32| {
        h[0] - by < r[2] && h[2] + by > r[0] && h[1] - by < r[3] && h[3] + by > r[1]
    };
    let (cols, rows) = (cols as usize, rows as usize);
    let edge = |i: usize, lo: f32, hi: f32| (lo + i as f32 * size).min(hi);
    let tile = |c: usize, r: usize| {
        let rect = [edge(c, x0, x1), edge(r, y0, y1), edge(c + 1, x0, x1), edge(r + 1, y0, y1)];
        if hulls.iter().any(|h| reaches(h, rect, near)) {
            Tile::Edge
        } else if fill && winding(&curves, [(rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0]) != 0 {
            Tile::Solid
        } else {
            Tile::Empty
        }
    };
    // A stroke's tile also lists the segments that reach it, in order: a run keeps to one list.
    // Each segment is tried only on the tiles around its hull, not every tile on every
    // segment, so a long trace costs its length rather than its length times its tiles. A
    // stroke's tile is an edge tile exactly when its list has a segment.
    let mut lists: Vec<Vec<usize>> = Vec::new();
    if !fill {
        lists.resize(cols * rows, Vec::new());
        // The tiles from `lo` to `hi` along one axis, one more each side: the exact test decides.
        let span = |lo: f32, hi: f32, origin: f32, n: usize| {
            let a = (((lo - origin) / size).floor() as i64).saturating_sub(1).max(0);
            let b = (((hi - origin) / size).floor() as i64).saturating_add(1).min(n as i64 - 1);
            a..=b
        };
        for (i, h) in hulls.iter().enumerate() {
            for r in span(h[1] - pad, h[3] + pad, y0, rows) {
                for c in span(h[0] - pad, h[2] + pad, x0, cols) {
                    let (c, r) = (c as usize, r as usize);
                    let rect = [edge(c, x0, x1), edge(r, y0, y1), edge(c + 1, x0, x1), edge(r + 1, y0, y1)];
                    if reaches(h, rect, pad) {
                        lists[r * cols + c].push(i);
                    }
                }
            }
        }
    }
    for r in 0..rows {
        let (ty0, ty1) = (edge(r, y0, y1), edge(r + 1, y0, y1));
        let mut run: Option<(usize, Tile, Vec<usize>)> = None;
        for c in 0..=cols {
            let t = match (c < cols, fill) {
                (false, _) => Tile::Empty,
                (true, true) => tile(c, r),
                (true, false) if lists[r * cols + c].is_empty() => Tile::Empty,
                (true, false) => Tile::Edge,
            };
            let list = if t == Tile::Edge && !fill { std::mem::take(&mut lists[r * cols + c]) } else { Vec::new() };
            if run.as_ref().is_some_and(|(_, k, l)| *k == t && *l == list) {
                continue;
            }
            if let Some((from, k, mine)) = run.take() {
                let mut piece = GpuVector { bbox: [edge(from, x0, x1), ty0, edge(c, x0, x1), ty1], ..g };
                if k == Tile::Solid {
                    piece.head[0] = KIND_SOLID;
                } else if !fill && mine.len() < count {
                    // A fill's winding needs all its segments; a stroke's run only those
                    // that reach it.
                    piece.head[1] = (segs.len() / SEG_TEXELS) as u32;
                    piece.head[2] = mine.len() as u32;
                    for i in mine {
                        segs.extend_from_slice(&src[i * SEG_TEXELS..(i + 1) * SEG_TEXELS]);
                    }
                }
                out.push(piece);
            }
            if t != Tile::Empty {
                run = Some((c, t, list));
            }
        }
    }
}

/// Nonzero winding number of quadratic curves around `p`, each flattened to 16 lines. Only
/// asked for points well away from the curves, where flattening cannot change it.
fn winding(curves: &[[[f32; 2]; 3]], p: [f32; 2]) -> i32 {
    let at = |c: &[[f32; 2]; 3], t: f32| {
        let u = 1.0 - t;
        [0, 1].map(|i| u * u * c[0][i] + 2.0 * u * t * c[1][i] + t * t * c[2][i])
    };
    let mut w = 0;
    for c in curves {
        let mut a = c[0];
        for k in 1..=16 {
            let b = at(c, k as f32 / 16.0);
            if (a[1] <= p[1]) != (b[1] <= p[1]) {
                let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                if x > p[0] {
                    w += if b[1] > a[1] { 1 } else { -1 };
                }
            }
            a = b;
        }
    }
    w
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

    /// A square from (100, 100) to (500, 500) as four straight segments, and an instance
    /// for it with `kind`, the given paint and glow, its box grown by `pad`.
    fn square(kind: u32, paint: u32, glow: f32, pad: f32) -> (GpuVector, Vec<[f32; 4]>) {
        let corners = [[100.0, 100.0], [500.0, 100.0], [500.0, 500.0], [100.0, 500.0], [100.0, 100.0]];
        let mut segs = Vec::new();
        for w in corners.windows(2) {
            let (a, c): ([f32; 2], [f32; 2]) = (w[0], w[1]);
            let b = [(a[0] + c[0]) / 2.0, (a[1] + c[1]) / 2.0];
            segs.push([a[0], a[1], b[0], b[1]]);
            segs.push([c[0], c[1], 0.0, 1.0]);
            segs.push([1.0, 1.0, 0.0, 0.0]);
        }
        let g = GpuVector {
            head: [kind, 0, 4, paint],
            bbox: [100.0 - pad, 100.0 - pad, 500.0 + pad, 500.0 + pad],
            params: [1.5, glow, 0.0, 1.0],
            ..Default::default()
        };
        (g, segs)
    }

    fn area(b: [f32; 4]) -> f32 {
        (b[2] - b[0]) * (b[3] - b[1])
    }

    /// The tiles must not overlap and must cover every pixel within `reach` of the square's
    /// outline (and, for `solid_inside`, all of its inside).
    fn check_tiles(tiles: &[GpuVector], reach: f32, solid_inside: bool) {
        for (i, a) in tiles.iter().enumerate() {
            for b in &tiles[i + 1..] {
                let (a, b) = (a.bbox, b.bbox);
                let overlap = a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3];
                assert!(!overlap, "{a:?} overlaps {b:?}");
            }
        }
        let covered =
            |x: f32, y: f32| tiles.iter().any(|t| t.bbox[0] <= x && x < t.bbox[2] && t.bbox[1] <= y && y < t.bbox[3]);
        for y in (80..520).map(|v| v as f32 + 0.5) {
            for x in (80..520).map(|v| v as f32 + 0.5) {
                let inside = (100.0..500.0).contains(&x) && (100.0..500.0).contains(&y);
                let dist = if inside {
                    (x - 100.0).min(500.0 - x).min(y - 100.0).min(500.0 - y)
                } else {
                    let dx = (100.0 - x).max(x - 500.0).max(0.0);
                    let dy = (100.0 - y).max(y - 500.0).max(0.0);
                    dx.hypot(dy)
                };
                if dist <= reach || (solid_inside && inside) {
                    assert!(covered(x, y), "({x}, {y}) is not drawn");
                }
            }
        }
    }

    #[test]
    fn a_large_stroke_skips_its_inside() {
        let (g, mut segs) = square(0, 1, 4.0, 13.0);
        let mut out = Vec::new();
        push_tiled(g, &mut segs, &mut out);
        assert!(out.len() > 1);
        assert!(out.iter().all(|t| t.head[0] == g.head[0] && t.head[3] == g.head[3] && t.params == g.params));
        // A tile on the top edge reads only the top segment; its copy is the same texels.
        let top = out.iter().find(|t| t.bbox[1] < 100.0 && t.bbox[0] <= 300.0 && t.bbox[2] > 300.0).unwrap();
        assert_eq!(top.head[2], 1);
        let at = top.head[1] as usize * SEG_TEXELS;
        assert_eq!(segs[at..at + SEG_TEXELS], segs[0..SEG_TEXELS]);
        let drawn: f32 = out.iter().map(|t| area(t.bbox)).sum();
        assert!(drawn < 0.6 * area(g.bbox), "{drawn} of {}", area(g.bbox));
        check_tiles(&out, 13.0, false);
    }

    #[test]
    fn a_large_fill_is_solid_inside() {
        let (g, mut segs) = square(2, 1, 0.0, 1.0);
        let mut out = Vec::new();
        push_tiled(g, &mut segs, &mut out);
        assert!(out.iter().any(|t| t.head[0] == KIND_SOLID));
        // Solid tiles keep well away from the edge.
        for t in out.iter().filter(|t| t.head[0] == KIND_SOLID) {
            let b = t.bbox;
            assert!(b[0] >= 100.0 + SOLID_GAP && b[2] <= 500.0 - SOLID_GAP, "{b:?}");
            assert!(b[1] >= 100.0 + SOLID_GAP && b[3] <= 500.0 - SOLID_GAP, "{b:?}");
        }
        check_tiles(&out, 1.0, true);
    }

    #[test]
    fn small_glowing_or_material_vectors_stay_whole() {
        let mut out = Vec::new();
        // Paint along the edge needs the segments everywhere.
        let (g, mut segs) = square(2, 0, 0.0, 1.0);
        push_tiled(g, &mut segs, &mut out);
        // A glowing fill draws outside its edge.
        let (g, mut segs) = square(2, 1, 3.0, 9.0);
        push_tiled(g, &mut segs, &mut out);
        // A material's seed is per instance.
        let (mut g, mut segs) = square(0, 1, 0.0, 2.0);
        g.material[0] = 1;
        push_tiled(g, &mut segs, &mut out);
        // Small.
        let (mut g, mut segs) = square(0, 1, 0.0, 2.0);
        g.bbox = [0.0, 0.0, 100.0, 100.0];
        push_tiled(g, &mut segs, &mut out);
        assert_eq!(out.len(), 4);
    }

    /// A stroke through `pts` as straight segments (each control point halfway), and an
    /// instance for it with half width `hw` and glow `glow`, its box grown by `pad`.
    fn polyline(pts: &[[f32; 2]], hw: f32, glow: f32, pad: f32) -> (GpuVector, Vec<[f32; 4]>) {
        let mut segs = Vec::new();
        let mut b = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
        for w in pts.windows(2) {
            let (a, c) = (w[0], w[1]);
            segs.push([a[0], a[1], (a[0] + c[0]) / 2.0, (a[1] + c[1]) / 2.0]);
            segs.push([c[0], c[1], 0.0, 1.0]);
            segs.push([1.0, 1.0, 0.0, 0.0]);
            b = [
                b[0].min(a[0]).min(c[0]),
                b[1].min(a[1]).min(c[1]),
                b[2].max(a[0]).max(c[0]),
                b[3].max(a[1]).max(c[1]),
            ];
        }
        let g = GpuVector {
            head: [0, 0, (pts.len() - 1) as u32, 1],
            bbox: [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad],
            params: [hw, glow, 0.0, 1.0],
            ..Default::default()
        };
        (g, segs)
    }

    /// The stroke tiling as it was before segments were binned to tiles: every tile tries
    /// every segment, once for whether it draws and once for its list.
    fn tiled_trying_every_segment(g: GpuVector, segs: &mut Vec<[f32; 4]>, out: &mut Vec<GpuVector>) {
        let [x0, y0, x1, y1] = g.bbox;
        let (first, count) = (g.head[1] as usize, g.head[2] as usize);
        let (cols, rows) = (((x1 - x0) / STROKE_TILE).ceil(), ((y1 - y0) / STROKE_TILE).ceil());
        assert!(g.head[0] == 0 && (x1 - x0) * (y1 - y0) >= TILED_AREA && cols * rows * (count as f32) <= 400_000.0);
        let src = segs[first * SEG_TEXELS..(first + count) * SEG_TEXELS].to_vec();
        let hulls: Vec<[f32; 4]> = src
            .as_chunks::<SEG_TEXELS>()
            .0
            .iter()
            .map(|s| {
                let (xs, ys) = ([s[0][0], s[0][2], s[1][0]], [s[0][1], s[0][3], s[1][1]]);
                let min = |v: [f32; 3]| v[0].min(v[1]).min(v[2]);
                let max = |v: [f32; 3]| v[0].max(v[1]).max(v[2]);
                [min(xs), min(ys), max(xs), max(ys)]
            })
            .collect();
        let inner = hulls.iter().fold([f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY], |b, h| {
            [b[0].min(h[0]), b[1].min(h[1]), b[2].max(h[2]), b[3].max(h[3])]
        });
        let pad = (inner[0] - x0).max(inner[1] - y0).max(x1 - inner[2]).max(y1 - inner[3]).max(0.0);
        let reaches = |h: &[f32; 4], r: [f32; 4]| {
            h[0] - pad < r[2] && h[2] + pad > r[0] && h[1] - pad < r[3] && h[3] + pad > r[1]
        };
        let (cols, rows) = (cols as usize, rows as usize);
        let edge = |i: usize, lo: f32, hi: f32| (lo + i as f32 * STROKE_TILE).min(hi);
        for r in 0..rows {
            let (ty0, ty1) = (edge(r, y0, y1), edge(r + 1, y0, y1));
            let mut run: Option<(usize, Vec<usize>)> = None;
            for c in 0..=cols {
                let list: Vec<usize> = if c < cols {
                    let rect = [edge(c, x0, x1), ty0, edge(c + 1, x0, x1), ty1];
                    (0..count).filter(|&i| reaches(&hulls[i], rect)).collect()
                } else {
                    Vec::new()
                };
                if run.as_ref().is_some_and(|(_, l)| *l == list) {
                    continue;
                }
                if let Some((from, mine)) = run.take() {
                    let mut piece = GpuVector { bbox: [edge(from, x0, x1), ty0, edge(c, x0, x1), ty1], ..g };
                    if mine.len() < count {
                        piece.head[1] = (segs.len() / SEG_TEXELS) as u32;
                        piece.head[2] = mine.len() as u32;
                        for i in mine {
                            segs.extend_from_slice(&src[i * SEG_TEXELS..(i + 1) * SEG_TEXELS]);
                        }
                    }
                    out.push(piece);
                }
                if !list.is_empty() {
                    run = Some((c, list));
                }
            }
        }
    }

    #[test]
    fn binned_stroke_tiles_match_trying_every_segment() {
        // An oscilloscope trace: 480 jagged segments across 700 px.
        let noise = |k: usize| ((k as f32 * 12.9898).sin() * 43758.547).fract().abs();
        let trace: Vec<[f32; 2]> =
            (0..481).map(|i| [20.0 + i as f32 * 1.45, 140.0 + (noise(i) - 0.5) * 160.0 * noise(i + 7)]).collect();
        // A spiral, whose segments cross many rows, and a diagonal off a tile grid's lines.
        let spiral: Vec<[f32; 2]> = (0..200)
            .map(|i| {
                let a = i as f32 * 0.21;
                [300.0 + a.cos() * (20.0 + 1.2 * i as f32), 300.0 + a.sin() * (20.0 + 1.2 * i as f32)]
            })
            .collect();
        let diagonal: Vec<[f32; 2]> = (0..40).map(|i| [3.3 + i as f32 * 13.7, 7.1 + i as f32 * 9.9]).collect();
        for (name, pts, hw, glow, pad) in [
            ("trace glow", &trace, 1.25, 4.0, 9.0),
            ("trace", &trace, 0.55, 0.0, 1.6),
            ("spiral", &spiral, 2.0, 0.0, 3.0),
            ("diagonal", &diagonal, 6.0, 2.0, 15.0),
        ] {
            let (g, segs) = polyline(pts, hw, glow, pad);
            let (mut a, mut b) = (segs.clone(), segs);
            let (mut out_a, mut out_b) = (Vec::new(), Vec::new());
            push_tiled(g, &mut a, &mut out_a);
            tiled_trying_every_segment(g, &mut b, &mut out_b);
            assert!(out_a.len() > 1, "{name}: tiled");
            assert_eq!(out_a, out_b, "{name}: tiles");
            assert_eq!(a, b, "{name}: segments");
        }
    }

    #[test]
    fn winding_counts_the_square() {
        let (_, segs) = square(2, 1, 0.0, 1.0);
        let curves: Vec<[[f32; 2]; 3]> = segs
            .as_chunks::<SEG_TEXELS>()
            .0
            .iter()
            .map(|s| [[s[0][0], s[0][1]], [s[0][2], s[0][3]], [s[1][0], s[1][1]]])
            .collect();
        assert_ne!(winding(&curves, [300.0, 300.0]), 0);
        assert_eq!(winding(&curves, [50.0, 300.0]), 0);
        assert_eq!(winding(&curves, [600.0, 300.0]), 0);
    }
}
