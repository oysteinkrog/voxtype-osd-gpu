//! Signed distance fields of single glyphs, built on the CPU from the font outline, for
//! shader effects that need to know how far a point is from the letter's edge: ink that
//! bleeds out of it, letters that blend into each other, light that a word stencils
//! (`crate::glyph_fx`).
//!
//! The outline comes from skrifa, unhinted, flattened to polylines in em units (y up). Each
//! texel holds the distance in em from its centre to the nearest edge, negative inside by the
//! nonzero rule. The grid is [`TEXELS_PER_EM`] across and reaches [`PAD_EM`] past the ink box,
//! so an effect can read the field a little outside the letter. A field depends only on the
//! font and the glyph, so one build serves every size.

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{GlyphId, MetadataProvider};
use vo_scene::font::Face;

/// Texels per em. At 64 the field is finer than the screen up to a 64 px em, and bilinear
/// reads keep edges smooth past that.
pub const TEXELS_PER_EM: f32 = 64.0;
/// How far the grid reaches past the ink box on each side, in em.
pub const PAD_EM: f32 = 0.35;

/// One glyph's field.
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphSdf {
    pub w: u32,
    pub h: u32,
    /// The em position of the grid's outer corners: `x0, y0, x1, y1`, y up. Texel `(i, j)`
    /// (row 0 at the top) has its centre at `x0 + (i + 0.5) / TEXELS_PER_EM`,
    /// `y1 - (j + 0.5) / TEXELS_PER_EM`.
    pub em: [f32; 4],
    /// Distances in em, row by row from the top.
    pub dist: Vec<f32>,
}

impl GlyphSdf {
    /// The distance stored for the texel nearest `p` (em, y up), or `None` off the grid.
    pub fn at(&self, p: [f32; 2]) -> Option<f32> {
        let i = ((p[0] - self.em[0]) * TEXELS_PER_EM).floor();
        let j = ((self.em[3] - p[1]) * TEXELS_PER_EM).floor();
        if i < 0.0 || j < 0.0 || i >= self.w as f32 || j >= self.h as f32 {
            return None;
        }
        Some(self.dist[j as usize * self.w as usize + i as usize])
    }
}

#[derive(Default)]
struct Flatten {
    k: f32,
    cur: Vec<[f32; 2]>,
    contours: Vec<Vec<[f32; 2]>>,
}

impl Flatten {
    fn p(&self, x: f32, y: f32) -> [f32; 2] {
        [x * self.k, y * self.k]
    }

    fn end(&mut self) {
        if self.cur.len() > 2 {
            self.contours.push(std::mem::take(&mut self.cur));
        }
        self.cur.clear();
    }

    /// Segments for a curve whose control polygon is `len` em long: about one per
    /// 1/128 em, so the flattening error stays far under a texel.
    fn segments(len: f32) -> usize {
        ((len * 128.0).sqrt().ceil() as usize * 2).clamp(2, 32)
    }
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

impl OutlinePen for Flatten {
    fn move_to(&mut self, x: f32, y: f32) {
        self.end();
        let p = self.p(x, y);
        self.cur.push(p);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.cur.push(p);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let p0 = *self.cur.last().unwrap_or(&[0.0, 0.0]);
        let (c, p) = (self.p(cx, cy), self.p(x, y));
        let n = Flatten::segments(dist(p0, c) + dist(c, p));
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            self.cur.push([
                u * u * p0[0] + 2.0 * u * t * c[0] + t * t * p[0],
                u * u * p0[1] + 2.0 * u * t * c[1] + t * t * p[1],
            ]);
        }
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let p0 = *self.cur.last().unwrap_or(&[0.0, 0.0]);
        let (c0, c1, p) = (self.p(cx0, cy0), self.p(cx1, cy1), self.p(x, y));
        let n = Flatten::segments(dist(p0, c0) + dist(c0, c1) + dist(c1, p));
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.cur.push([a * p0[0] + b * c0[0] + c * c1[0] + d * p[0], a * p0[1] + b * c0[1] + c * c1[1] + d * p[1]]);
        }
    }

    fn close(&mut self) {
        self.end();
    }
}

/// The outline of `glyph` as closed polylines in em units, y up. `None` when the font has no
/// outline for it; empty for a glyph with no ink, such as a space.
pub fn outline_em(face: &Face, glyph: u32) -> Option<Vec<Vec<[f32; 2]>>> {
    let font = face.skrifa();
    let outline = font.outline_glyphs().get(GlyphId::new(glyph))?;
    let mut pen = Flatten { k: 1.0 / face.units_per_em() as f32, ..Default::default() };
    outline.draw(DrawSettings::unhinted(Size::unscaled(), LocationRef::default()), &mut pen).ok()?;
    pen.end();
    let mut contours = pen.contours;
    for c in &mut contours {
        if c.len() > 1 && dist(c[0], c[c.len() - 1]) < 1e-6 {
            c.pop();
        }
    }
    contours.retain(|c| c.len() > 2);
    Some(contours)
}

/// The field of `glyph` of `face`. `None` for a glyph with no outline or no ink.
pub fn glyph_sdf(face: &Face, glyph: u32) -> Option<GlyphSdf> {
    let contours = outline_em(face, glyph)?;
    sdf_of(&contours)
}

/// The field of closed polylines (em units, y up), nonzero rule. `None` when there is no ink.
pub fn sdf_of(contours: &[Vec<[f32; 2]>]) -> Option<GlyphSdf> {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for p in contours.iter().flatten() {
        b = [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])];
    }
    if b[0] > b[2] {
        return None;
    }
    let w = (((b[2] - b[0]) + 2.0 * PAD_EM) * TEXELS_PER_EM).ceil() as u32;
    let h = (((b[3] - b[1]) + 2.0 * PAD_EM) * TEXELS_PER_EM).ceil() as u32;
    let em = [
        b[0] - PAD_EM,
        b[3] + PAD_EM - h as f32 / TEXELS_PER_EM,
        b[0] - PAD_EM + w as f32 / TEXELS_PER_EM,
        b[3] + PAD_EM,
    ];
    let segs: Vec<([f32; 2], [f32; 2])> =
        contours.iter().flat_map(|c| (0..c.len()).map(move |i| (c[i], c[(i + 1) % c.len()]))).collect();
    let mut out = vec![0.0f32; (w * h) as usize];
    let step = 1.0 / TEXELS_PER_EM;
    for j in 0..h {
        let y = em[3] - (j as f32 + 0.5) * step;
        // Segments that cross this row, for the winding count.
        let crossing: Vec<&([f32; 2], [f32; 2])> = segs.iter().filter(|(a, b)| (a[1] <= y) != (b[1] <= y)).collect();
        for i in 0..w {
            let x = em[0] + (i as f32 + 0.5) * step;
            let mut d2 = f32::MAX;
            for (a, b) in &segs {
                d2 = d2.min(seg_dist2([x, y], *a, *b));
            }
            let mut wind = 0;
            for (a, b) in &crossing {
                let cross = (b[0] - a[0]) * (y - a[1]) - (x - a[0]) * (b[1] - a[1]);
                if a[1] <= y {
                    if cross > 0.0 {
                        wind += 1;
                    }
                } else if cross < 0.0 {
                    wind -= 1;
                }
            }
            let d = d2.sqrt();
            out[(j * w + i) as usize] = if wind != 0 { -d } else { d };
        }
    }
    Some(GlyphSdf { w, h, em, dist: out })
}

fn seg_dist2(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (vx, vy) = (b[0] - a[0], b[1] - a[1]);
    let (wx, wy) = (p[0] - a[0], p[1] - a[1]);
    let l2 = vx * vx + vy * vy;
    let t = if l2 > 0.0 { ((wx * vx + wy * vy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    let (dx, dy) = (wx - t * vx, wy - t * vy);
    dx * dx + dy * dy
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: f32, y0: f32, s: f32) -> Vec<[f32; 2]> {
        vec![[x0, y0], [x0 + s, y0], [x0 + s, y0 + s], [x0, y0 + s]]
    }

    #[test]
    fn square_field_is_signed_and_metric() {
        let f = sdf_of(&[square(0.0, 0.0, 0.5)]).unwrap();
        // Centre: a quarter em inside. Just outside the right edge: positive.
        let c = f.at([0.25, 0.25]).unwrap();
        assert!((c + 0.25).abs() < 2.0 / TEXELS_PER_EM, "{c}");
        let o = f.at([0.6, 0.25]).unwrap();
        assert!((o - 0.1).abs() < 2.0 / TEXELS_PER_EM, "{o}");
        // The grid reaches PAD_EM past the ink on every side.
        assert!(f.em[0] <= -PAD_EM + 1e-4 && f.em[2] >= 0.5 + PAD_EM - 1e-4);
        assert!(f.at([-PAD_EM + 0.01, 0.25]).is_some());
    }

    #[test]
    fn a_hole_is_outside() {
        // An outer square with an inner one wound the other way: a ring.
        let mut inner = square(0.2, 0.2, 0.2);
        inner.reverse();
        let f = sdf_of(&[square(0.0, 0.0, 0.6), inner]).unwrap();
        assert!(f.at([0.3, 0.3]).unwrap() > 0.0, "the hole counts as ink");
        assert!(f.at([0.1, 0.3]).unwrap() < 0.0, "the ring is not ink");
    }

    #[test]
    fn real_glyph_has_ink_and_counter() {
        let reg = vo_scene::font::FontRegistry::new();
        let id = reg.add_file(&vo_scene::font::test_font_path()).unwrap();
        let face = reg.get(id).unwrap();
        // "o": ink on its ring, none in its counter.
        let gid = face.skrifa().charmap().map('o').unwrap().to_u32();
        let f = glyph_sdf(&face, gid).unwrap();
        let mid_y = 0.5 * (f.em[1] + f.em[3]);
        let mid_x = 0.5 * (f.em[0] + f.em[2]);
        assert!(f.at([mid_x, mid_y]).unwrap() > 0.0, "the counter of o is ink");
        let inside = f.dist.iter().filter(|d| **d < 0.0).count();
        assert!(inside > 100, "o has too little ink: {inside}");
        // A space has no field.
        let space = face.skrifa().charmap().map(' ').unwrap().to_u32();
        assert!(glyph_sdf(&face, space).is_none());
    }
}
