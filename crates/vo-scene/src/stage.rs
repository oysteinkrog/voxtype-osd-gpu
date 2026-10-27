//! The stage: 2D layers as flat planes in one perspective camera, the CSS 3D transforms model.
//!
//! A layer element can turn about its own x and y axes and move along z
//! (`Element::rotate_x`, `Element::rotate_y`, `Element::translate_z`), each a spring channel.
//! The scene projects the layer's plane through the stage camera, a perspective `perspective`
//! logical pixels in front of the surface, looking at its centre. With no layer turned or
//! moved in z, nothing changes: the list is the same as without a stage.
//!
//! What reaches the renderer is a [`Warp`] per layer: a plane-to-plane perspective map
//! (homography) from surface coordinates as laid out to surface coordinates as seen. A layer
//! inside a turned layer is flattened onto it, as in CSS without `preserve-3d`.
//!
//! GPU elements (`crate::gpu`) can draw in the same camera: [`Stage::view_proj`] is the
//! camera's 4x4 for a 3D scene laid out in surface units.

use serde::Serialize;

use crate::geom::{Point, Rect, Size};

/// The stage camera. Positive z points from the surface toward the viewer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Stage {
    /// Distance from the eye to the surface plane, in logical pixels. CSS `perspective`.
    pub perspective: f32,
    /// The point on the surface the eye looks at, in logical pixels. `None` for the centre.
    pub origin: Option<Point>,
}

impl Default for Stage {
    fn default() -> Stage {
        Stage { perspective: 1000.0, origin: None }
    }
}

/// Nearest a point may come to the eye, as a share of the perspective distance. A layer with
/// any corner nearer is not warped (it would wrap through infinity).
const NEAR: f32 = 0.05;

impl Stage {
    /// The look-at point for a surface of `size`.
    pub fn origin_in(&self, size: Size) -> Point {
        self.origin.unwrap_or(Point::new(0.5 * size.w, 0.5 * size.h))
    }

    /// The warp of a layer whose box `rect` (surface units, as laid out) turns by `rx` about
    /// its horizontal centre line (positive: the top edge goes away), by `ry` about its
    /// vertical centre line (positive: the right edge goes away), then moves `tz` toward the
    /// viewer. `None` when nothing turns or moves, or when a corner would reach the eye.
    pub fn tilt(&self, size: Size, rect: Rect, rx: f32, ry: f32, tz: f32) -> Option<Warp> {
        if rx == 0.0 && ry == 0.0 && tz == 0.0 {
            return None;
        }
        let d = self.perspective.max(1.0);
        let o = self.origin_in(size);
        let (cx, cy) = (rect.x + 0.5 * rect.w, rect.y + 0.5 * rect.h);
        let (sin_y, cos_y) = ry.sin_cos();
        let (sin_x, cos_x) = rx.sin_cos();
        // A point (u, v, 0) relative to the centre turns about y, then about x:
        //   about y: (u cos ry, v, -u sin ry), so the right edge goes away for ry > 0;
        //   about x: y' = y cos rx - z sin rx, z' = y sin rx + z cos rx, so the top edge
        //   (v < 0) goes away for rx > 0.
        // As linear functions of (u, v):
        let (xu, xv) = (cos_y, 0.0);
        let (yu, yv) = (sin_y * sin_x, cos_x);
        let (zu, zv) = (-sin_y * cos_x, sin_x);
        // World point: (cx + X, cy + Y, tz + Z) with u = x - cx, v = y - cy.
        // Seen: x' = o.x + (Xw - o.x) * d / (d - Zw). Homogeneous with w = (d - Zw) / d:
        //   x' w = o.x w + Xw - o.x, y' w = o.y w + Yw - o.y.
        // Each of Xw, Yw, Zw is a + b u + c v, and u, v are affine in (x, y).
        let lin = |a: f32, bu: f32, bv: f32| [bu, bv, a - bu * cx - bv * cy];
        let xw = lin(cx, xu, xv);
        let yw = lin(cy, yu, yv);
        let zw = lin(tz, zu, zv);
        let w = [-zw[0] / d, -zw[1] / d, 1.0 - zw[2] / d];
        let row = |c: [f32; 3], o: f32| [o * w[0] + c[0], o * w[1] + c[1], o * w[2] + c[2] - o];
        let warp = Warp([row(xw, o.x), row(yw, o.y), w]);
        let corners = [
            (rect.x, rect.y),
            (rect.x + rect.w, rect.y),
            (rect.x, rect.y + rect.h),
            (rect.x + rect.w, rect.y + rect.h),
        ];
        if corners.iter().any(|&(x, y)| warp.w_at(Point::new(x, y)) < NEAR) {
            return None;
        }
        Some(warp)
    }

    /// Column-major 4x4 from a 3D point in surface units (x right, y down, z toward the
    /// viewer) to clip space (wgpu: x and y -1 to 1 with y up, z 0 to 1) for a surface of
    /// `size`. Points at z = 0 land where the 2D layout puts them; `near` and `far` are view
    /// distances in surface units.
    pub fn view_proj(&self, size: Size, near: f32, far: f32) -> [[f32; 4]; 4] {
        let d = self.perspective.max(1.0);
        let o = self.origin_in(size);
        // View distance of (x, y, z): d - z. Seen position: o + (p - o) d / (d - z).
        // NDC x = (seen.x / w * 2 - 1), NDC y = 1 - seen.y / h * 2.
        // clip.w = (d - z) / d keeps x and y linear:
        //   clip.x = (2 / W) (o.x clip.w + x - o.x) - clip.w
        let (sw, sh) = (2.0 / size.w.max(1.0), 2.0 / size.h.max(1.0));
        // clip.w = 1 - z / d
        let cw = [0.0, 0.0, -1.0 / d, 1.0];
        let cx = [sw, 0.0, sw * o.x * cw[2] - cw[2], sw * o.x * cw[3] - sw * o.x - cw[3]];
        let cy = [0.0, -sh, -sh * o.y * cw[2] + cw[2], -sh * o.y * cw[3] + sh * o.y + cw[3]];
        // Depth: view distance v = d - z mapped to 0 at `near` and 1 at `far`, as z/w with
        // w = v / d: clip.z = a v + b with z/w = (a v + b) d / v.
        let (n, f) = (near.max(1e-3), far.max(near + 1e-3));
        let a = f / (d * (f - n));
        let b = -n * f / (d * (f - n));
        // v = d - z, so clip.z = a d - a z + b.
        let cz = [0.0, 0.0, -a, a * d + b];
        // Rows cx, cy, cz, cw over (x, y, z, 1) to columns.
        let rows = [cx, cy, cz, cw];
        let mut cols = [[0.0; 4]; 4];
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                cols[c][r] = *v;
            }
        }
        cols
    }
}

/// A perspective map of the plane (homography), rows of a 3x3 over (x, y, 1). The seen point
/// is `(r0 . p, r1 . p) / (r2 . p)`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Warp(pub [[f32; 3]; 3]);

impl Warp {
    pub const IDENTITY: Warp = Warp([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);

    pub fn apply(&self, p: Point) -> Point {
        let [x, y, w] = self.homogeneous(p);
        Point::new(x / w, y / w)
    }

    /// `(x w, y w, w)` before the divide.
    pub fn homogeneous(&self, p: Point) -> [f32; 3] {
        self.0.map(|r| r[0] * p.x + r[1] * p.y + r[2])
    }

    /// The divisor at `p`: 1 on the surface plane, smaller nearer the eye.
    pub fn w_at(&self, p: Point) -> f32 {
        let r = self.0[2];
        r[0] * p.x + r[1] * p.y + r[2]
    }

    /// `self` followed by `next`.
    pub fn then(&self, next: &Warp) -> Warp {
        let (a, b) = (&next.0, &self.0);
        let mut m = [[0.0; 3]; 3];
        for (i, row) in m.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
            }
        }
        Warp(m)
    }

    /// The inverse, or `None` when the map collapses the plane.
    pub fn invert(&self) -> Option<Warp> {
        let m = &self.0;
        let c = |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
        let adj = [
            [c(1, 2, 1, 2), -c(0, 2, 1, 2), c(0, 1, 1, 2)],
            [-c(1, 2, 0, 2), c(0, 2, 0, 2), -c(0, 1, 0, 2)],
            [c(1, 2, 0, 1), -c(0, 2, 0, 1), c(0, 1, 0, 1)],
        ];
        let det = m[0][0] * adj[0][0] + m[0][1] * adj[1][0] + m[0][2] * adj[2][0];
        if det.abs() < 1e-12 {
            return None;
        }
        Some(Warp(adj.map(|r| r.map(|v| v / det))))
    }

    /// The same map for coordinates scaled by `k` (logical to physical pixels).
    pub fn scaled(&self, k: f32) -> Warp {
        let m = &self.0;
        Warp([[m[0][0], m[0][1], m[0][2] * k], [m[1][0], m[1][1], m[1][2] * k], [m[2][0] / k, m[2][1] / k, m[2][2]]])
    }

    /// The bounding box of `rect` once warped, `x0, y0, x1, y1`.
    pub fn bounds(&self, r: [f32; 4]) -> [f32; 4] {
        let pts = [(r[0], r[1]), (r[2], r[1]), (r[0], r[3]), (r[2], r[3])].map(|(x, y)| self.apply(Point::new(x, y)));
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in pts {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
        }
        [x0, y0, x1, y1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Point, b: Point) -> bool {
        (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3
    }

    const SIZE: Size = Size { w: 600.0, h: 300.0 };
    const RECT: Rect = Rect { x: 100.0, y: 50.0, w: 400.0, h: 200.0 };

    #[test]
    fn no_turn_no_warp() {
        assert_eq!(Stage::default().tilt(SIZE, RECT, 0.0, 0.0, 0.0), None);
    }

    #[test]
    fn top_edge_goes_away_for_positive_rotate_x() {
        let w = Stage::default().tilt(SIZE, RECT, 0.5, 0.0, 0.0).unwrap();
        let (tl, tr) = (w.apply(Point::new(100.0, 50.0)), w.apply(Point::new(500.0, 50.0)));
        let (bl, br) = (w.apply(Point::new(100.0, 250.0)), w.apply(Point::new(500.0, 250.0)));
        // Away from the viewer: smaller, so the top edge is shorter than the bottom edge.
        assert!(tr.x - tl.x < br.x - bl.x, "top {tl:?}..{tr:?} bottom {bl:?}..{br:?}");
        // The centre line stays where it was (the turn is about it, at z = 0).
        assert!(close(w.apply(Point::new(300.0, 150.0)), Point::new(300.0, 150.0)));
        // Foreshortened vertically.
        assert!(bl.y - tl.y < 200.0);
    }

    #[test]
    fn right_edge_goes_away_for_positive_rotate_y() {
        let w = Stage::default().tilt(SIZE, RECT, 0.0, 0.5, 0.0).unwrap();
        let left = w.apply(Point::new(100.0, 250.0)).y - w.apply(Point::new(100.0, 50.0)).y;
        let right = w.apply(Point::new(500.0, 250.0)).y - w.apply(Point::new(500.0, 50.0)).y;
        assert!(right < left, "left edge {left}, right edge {right}");
    }

    #[test]
    fn pushing_back_shrinks_toward_the_origin() {
        let s = Stage::default();
        let w = s.tilt(SIZE, RECT, 0.0, 0.0, -1000.0).unwrap();
        // Twice as far: half size about the stage origin.
        assert!(close(w.apply(Point::new(100.0, 50.0)), Point::new(200.0, 100.0)));
        assert!(close(w.apply(Point::new(300.0, 150.0)), Point::new(300.0, 150.0)));
        // Toward the viewer grows.
        let near = s.tilt(SIZE, RECT, 0.0, 0.0, 500.0).unwrap();
        assert!(close(near.apply(Point::new(100.0, 50.0)), Point::new(-100.0, -50.0)));
        // At the eye: no warp.
        assert_eq!(s.tilt(SIZE, RECT, 0.0, 0.0, 999.0), None);
    }

    #[test]
    fn invert_and_scale_round_trip() {
        let w = Stage::default().tilt(SIZE, RECT, 0.4, -0.3, -120.0).unwrap();
        let inv = w.invert().unwrap();
        for p in [Point::new(100.0, 50.0), Point::new(321.0, 99.0), Point::new(500.0, 250.0)] {
            assert!(close(inv.apply(w.apply(p)), p));
            assert!(close(w.then(&inv).apply(p), p));
        }
        let k = w.scaled(2.0);
        let p = Point::new(321.0, 99.0);
        let q = w.apply(p);
        assert!(close(k.apply(Point::new(2.0 * p.x, 2.0 * p.y)), Point::new(2.0 * q.x, 2.0 * q.y)));
    }

    #[test]
    fn view_proj_matches_the_warp_on_the_plane() {
        let s = Stage::default();
        let m = s.view_proj(SIZE, 10.0, 5000.0);
        let w = s.tilt(SIZE, RECT, 0.0, 0.0, -300.0).unwrap();
        let p = Point::new(150.0, 80.0);
        // The same point pushed back 300 through the 4x4.
        let v = [p.x, p.y, -300.0, 1.0];
        let clip: [f32; 4] = std::array::from_fn(|r| (0..4).map(|c| m[c][r] * v[c]).sum());
        let ndc = [clip[0] / clip[3], clip[1] / clip[3], clip[2] / clip[3]];
        let seen = Point::new((ndc[0] + 1.0) * 0.5 * SIZE.w, (1.0 - ndc[1]) * 0.5 * SIZE.h);
        // The warp of a box moved back by 300 maps p to the same place.
        assert!(close(seen, w.apply(p)), "{seen:?} vs {:?}", w.apply(p));
        assert!((0.0..=1.0).contains(&ndc[2]));
        // On the surface plane, the identity.
        let v = [p.x, p.y, 0.0, 1.0];
        let clip: [f32; 4] = std::array::from_fn(|r| (0..4).map(|c| m[c][r] * v[c]).sum());
        let seen = Point::new((clip[0] / clip[3] + 1.0) * 0.5 * SIZE.w, (1.0 - clip[1] / clip[3]) * 0.5 * SIZE.h);
        assert!(close(seen, p));
    }
}
