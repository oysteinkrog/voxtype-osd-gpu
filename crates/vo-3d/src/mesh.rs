//! Triangle meshes and the builders the layer ships with: torus, box and extruded 2D paths
//! (glyph outlines, icons). Meshes are built once on the CPU and uploaded once
//! ([`crate::Renderer3d::add_mesh`]); motion comes from the per-draw model matrix.

use bytemuck::{Pod, Zeroable};
use lyon_tessellation::path::iterator::PathIterator;
use lyon_tessellation::path::{Path, PathEvent};
use lyon_tessellation::{BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex, VertexBuffers};

use crate::math::Vec3;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
}

impl Vertex {
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRS,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl Mesh {
    fn push(&mut self, pos: Vec3, normal: Vec3) -> u32 {
        self.vertices.push(Vertex { pos: pos.to_array(), normal: normal.to_array() });
        self.vertices.len() as u32 - 1
    }

    /// Appends `other`, offsetting its indices.
    pub fn append(&mut self, other: &Mesh) {
        let base = self.vertices.len() as u32;
        self.vertices.extend_from_slice(&other.vertices);
        self.indices.extend(other.indices.iter().map(|i| i + base));
    }

    /// Axis-aligned bounds `(min, max)`, or zeros for an empty mesh.
    pub fn bounds(&self) -> (Vec3, Vec3) {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for v in &self.vertices {
            for k in 0..3 {
                lo[k] = lo[k].min(v.pos[k]);
                hi[k] = hi[k].max(v.pos[k]);
            }
        }
        if self.vertices.is_empty() {
            return (Vec3::ZERO, Vec3::ZERO);
        }
        (Vec3::new(lo[0], lo[1], lo[2]), Vec3::new(hi[0], hi[1], hi[2]))
    }

    /// A torus in the xz plane around the y axis: `major` is the ring radius, `minor` the
    /// tube radius.
    pub fn torus(major: f32, minor: f32, ring_segments: u32, tube_segments: u32) -> Mesh {
        use std::f32::consts::TAU;
        let mut m = Mesh::default();
        for i in 0..=ring_segments {
            let u = i as f32 / ring_segments as f32 * TAU;
            let (su, cu) = u.sin_cos();
            for j in 0..=tube_segments {
                let v = j as f32 / tube_segments as f32 * TAU;
                let (sv, cv) = v.sin_cos();
                let n = Vec3::new(cu * cv, sv, su * cv);
                let p = Vec3::new(cu * major, 0.0, su * major) + n * minor;
                m.push(p, n);
            }
        }
        let row = tube_segments + 1;
        for i in 0..ring_segments {
            for j in 0..tube_segments {
                let (a, b) = (i * row + j, (i + 1) * row + j);
                m.indices.extend([a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        m
    }

    /// A box centred on the origin with the given full size.
    pub fn cuboid(size: Vec3) -> Mesh {
        let h = size * 0.5;
        let mut m = Mesh::default();
        for (n, u, v) in [
            (Vec3::X, Vec3::Y, Vec3::Z),
            (-Vec3::X, Vec3::Z, Vec3::Y),
            (Vec3::Y, Vec3::Z, Vec3::X),
            (-Vec3::Y, Vec3::X, Vec3::Z),
            (Vec3::Z, Vec3::X, Vec3::Y),
            (-Vec3::Z, Vec3::Y, Vec3::X),
        ] {
            let c = Vec3::new(n.x * h.x, n.y * h.y, n.z * h.z);
            let (du, dv) = (Vec3::new(u.x * h.x, u.y * h.y, u.z * h.z), Vec3::new(v.x * h.x, v.y * h.y, v.z * h.z));
            let base = m.vertices.len() as u32;
            for (a, b) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                m.push(c + du * a + dv * b, n);
            }
            m.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        m
    }

    /// Extrudes a filled 2D path (y up, any units) along z from `-depth / 2` to `depth / 2`:
    /// front and back caps from the nonzero fill, and side walls. `tolerance` is the curve
    /// flattening tolerance in path units. Side normals are smooth where neighbouring edges
    /// turn by less than 35 degrees and hard at corners.
    pub fn extrude(path: &Path, depth: f32, tolerance: f32) -> Mesh {
        let mut m = Mesh::default();
        let hz = 0.5 * depth;

        // Caps.
        let mut fill: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
        let opts = FillOptions::tolerance(tolerance).with_fill_rule(FillRule::NonZero);
        let built = FillTessellator::new().tessellate_path(
            path,
            &opts,
            &mut BuffersBuilder::new(&mut fill, |v: FillVertex| v.position().to_array()),
        );
        if built.is_err() {
            return m;
        }
        for (z, n, flip) in [(hz, Vec3::Z, false), (-hz, -Vec3::Z, true)] {
            let base = m.vertices.len() as u32;
            for p in &fill.vertices {
                m.push(Vec3::new(p[0], p[1], z), n);
            }
            for t in fill.indices.as_chunks::<3>().0 {
                // Wind every cap triangle to face its normal, whatever order lyon gave.
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| fill.vertices[i as usize]);
                let ccw = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]) > 0.0;
                let tri = if ccw != flip { [t[0], t[1], t[2]] } else { [t[0], t[2], t[1]] };
                m.indices.extend(tri.map(|i| base + i));
            }
        }

        // Walls.
        let contours = flatten(path, tolerance);
        for c in &contours {
            if c.len() < 3 {
                continue;
            }
            let outward = outward_sign(c, &contours);
            let n = c.len();
            let normal = |i: usize| {
                let (a, b) = (c[i], c[(i + 1) % n]);
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                Vec3::new(-dy * outward, dx * outward, 0.0).normalize()
            };
            let edge_normals: Vec<Vec3> = (0..n).map(normal).collect();
            let smooth = 35f32.to_radians().cos();
            for i in 0..n {
                let j = (i + 1) % n;
                let e = edge_normals[i];
                // The normal at each end of the edge: shared with the neighbour when the turn
                // is gentle, the edge's own otherwise.
                let at = |other: Vec3| if e.dot(other) > smooth { (e + other).normalize() } else { e };
                let n0 = at(edge_normals[(i + n - 1) % n]);
                let n1 = at(edge_normals[j]);
                let (a, b) = (c[i], c[j]);
                let base = m.vertices.len() as u32;
                m.push(Vec3::new(a[0], a[1], hz), n0);
                m.push(Vec3::new(b[0], b[1], hz), n1);
                m.push(Vec3::new(b[0], b[1], -hz), n1);
                m.push(Vec3::new(a[0], a[1], -hz), n0);
                // (top a, bottom b, top b) faces the edge's right; flip it when outward is left.
                if outward > 0.0 {
                    m.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
                } else {
                    m.indices.extend([base, base + 2, base + 1, base, base + 3, base + 2]);
                }
            }
        }
        m
    }
}

/// The path as closed polylines.
fn flatten(path: &Path, tolerance: f32) -> Vec<Vec<[f32; 2]>> {
    let mut out: Vec<Vec<[f32; 2]>> = Vec::new();
    for ev in path.iter().flattened(tolerance) {
        match ev {
            PathEvent::Begin { at } => out.push(vec![at.to_array()]),
            PathEvent::Line { to, .. } => out.last_mut().unwrap().push(to.to_array()),
            PathEvent::End { .. } => {
                // Drop a closing point that repeats the first.
                if let Some(c) = out.last_mut() {
                    if c.len() > 1 && c.first() == c.last() {
                        c.pop();
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// +1 when the left side of `c`'s edges is outside the filled shape, -1 when it is inside.
/// Tested on the longest edge with the nonzero winding number of every contour, so it holds
/// for TrueType and CFF winding alike.
fn outward_sign(c: &[[f32; 2]], all: &[Vec<[f32; 2]>]) -> f32 {
    let n = c.len();
    let i = (0..n)
        .max_by(|&a, &b| {
            let len = |k: usize| {
                let (p, q) = (c[k], c[(k + 1) % n]);
                (q[0] - p[0]).hypot(q[1] - p[1])
            };
            len(a).total_cmp(&len(b))
        })
        .unwrap();
    let (a, b) = (c[i], c[(i + 1) % n]);
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = dx.hypot(dy).max(1e-6);
    let eps = (len * 1e-3).max(1e-4);
    let probe = [0.5 * (a[0] + b[0]) - dy / len * eps, 0.5 * (a[1] + b[1]) + dx / len * eps];
    if winding(probe, all) != 0 {
        -1.0
    } else {
        1.0
    }
}

fn winding(p: [f32; 2], contours: &[Vec<[f32; 2]>]) -> i32 {
    let mut w = 0;
    for c in contours {
        for i in 0..c.len() {
            let (a, b) = (c[i], c[(i + 1) % c.len()]);
            let side = (b[0] - a[0]) * (p[1] - a[1]) - (p[0] - a[0]) * (b[1] - a[1]);
            if a[1] <= p[1] {
                if b[1] > p[1] && side > 0.0 {
                    w += 1;
                }
            } else if b[1] <= p[1] && side < 0.0 {
                w -= 1;
            }
        }
    }
    w
}

impl From<[f32; 3]> for Vec3 {
    fn from(a: [f32; 3]) -> Vec3 {
        Vec3::new(a[0], a[1], a[2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lyon_tessellation::math::point;

    fn square(path: &mut lyon_tessellation::path::path::Builder, x: f32, y: f32, s: f32, ccw: bool) {
        let mut pts = [point(x, y), point(x + s, y), point(x + s, y + s), point(x, y + s)];
        if !ccw {
            pts.reverse();
        }
        path.begin(pts[0]);
        for p in &pts[1..] {
            path.line_to(*p);
        }
        path.end(true);
    }

    #[test]
    fn torus_counts() {
        let t = Mesh::torus(1.0, 0.25, 48, 16);
        assert_eq!(t.vertices.len(), 49 * 17);
        assert_eq!(t.indices.len(), 48 * 16 * 6);
        let (lo, hi) = t.bounds();
        assert!((hi.x - 1.25).abs() < 1e-5 && (lo.y + 0.25).abs() < 1e-5);
    }

    #[test]
    fn extruded_square_with_hole_has_outward_walls() {
        let mut b = Path::builder();
        square(&mut b, 0.0, 0.0, 4.0, true);
        square(&mut b, 1.0, 1.0, 2.0, false);
        let m = Mesh::extrude(&b.build(), 1.0, 0.01);
        // Every wall normal points away from the filled ring: outer walls away from the
        // centre, hole walls toward it.
        for v in m.vertices.iter().filter(|v| v.normal[2] == 0.0) {
            let (px, py) = (v.pos[0] - 2.0, v.pos[1] - 2.0);
            let radial = px * v.normal[0] + py * v.normal[1];
            let on_outer = px.abs().max(py.abs()) > 1.5;
            assert_eq!(radial > 0.0, on_outer, "{v:?}");
        }
        // Caps: the ring's area 16 - 4 = 12, twice.
        let area: f32 = m
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| m.vertices[i as usize]);
                if a.normal[2] == 0.0 {
                    return 0.0;
                }
                let (ux, uy) = (b.pos[0] - a.pos[0], b.pos[1] - a.pos[1]);
                let (vx, vy) = (c.pos[0] - a.pos[0], c.pos[1] - a.pos[1]);
                0.5 * (ux * vy - uy * vx).abs()
            })
            .sum();
        assert!((area - 24.0).abs() < 1e-3, "{area}");
    }

    #[test]
    fn cap_triangles_face_their_normals() {
        let mut b = Path::builder();
        square(&mut b, 0.0, 0.0, 1.0, true);
        let m = Mesh::extrude(&b.build(), 1.0, 0.01);
        for t in m.indices.as_chunks::<3>().0.iter() {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| m.vertices[i as usize]);
            let e1 = Vec3::from(b.pos) - Vec3::from(a.pos);
            let e2 = Vec3::from(c.pos) - Vec3::from(a.pos);
            let face = e1.cross(e2);
            assert!(face.dot(Vec3::from(a.normal)) > 0.0, "triangle faces away from its normal");
        }
    }
}
