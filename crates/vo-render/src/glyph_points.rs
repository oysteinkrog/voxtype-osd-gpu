//! Glyph point sampling (CAPABILITIES TY18): points along a glyph's outline and inside its
//! fill, for particles that form text (PA03).
//!
//! The outline comes from skrifa, unhinted, flattened to polylines. Contour points are
//! resampled by arc length; interior points come from a jittered grid kept where the nonzero
//! winding rule says "inside". The jitter is a hash of the grid cell, so the same glyph at the
//! same size always gives the same points. [`GlyphPointCache`] keeps one set per font, glyph
//! and size.
//!
//! Order matters to the particle assignment: contour points run contour by contour, the
//! largest contour first, each by arc length; interior points run along a Hilbert curve. A
//! word's targets keep that order, so particle `i` of one revision and target `i` of the next
//! sit close together.

use std::collections::HashMap;
use std::sync::Arc;

use skrifa::instance::Size;
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{GlyphId, MetadataProvider};
use vo_core::hash::rand_f32;
use vo_scene::font::Face;

/// Points of one glyph at one size, in logical units from the pen position on the baseline,
/// y down (layer space).
#[derive(Clone, Debug, Default)]
pub struct GlyphPoints {
    /// Along the outline, about [`CONTOUR_SPACING`] apart, in contour order.
    pub contour: Vec<[f32; 2]>,
    /// Inside the fill, about [`INTERIOR_SPACING`] apart, in Hilbert order.
    pub interior: Vec<[f32; 2]>,
    /// Total outline length.
    pub perimeter: f32,
    /// Filled area.
    pub area: f32,
    /// Ink box: x0, y0, x1, y1.
    pub bounds: [f32; 4],
}

/// Spacing of contour points in pixels.
pub const CONTOUR_SPACING: f32 = 0.5;
/// Spacing of the interior grid in pixels.
pub const INTERIOR_SPACING: f32 = 0.75;

#[derive(Default)]
struct Flatten {
    contours: Vec<Vec<[f32; 2]>>,
    cur: Vec<[f32; 2]>,
    /// Font units to output units; y is flipped.
    k: f32,
    /// Output units per pixel, so curves get about one segment per half pixel.
    unit_px: f32,
}

impl Flatten {
    fn p(&self, x: f32, y: f32) -> [f32; 2] {
        [x * self.k, -y * self.k]
    }

    fn segments(&self, len: f32) -> usize {
        ((len / (0.5 * self.unit_px)).ceil() as usize).clamp(1, 64)
    }

    fn end(&mut self) {
        if self.cur.len() > 2 {
            self.contours.push(std::mem::take(&mut self.cur));
        }
        self.cur.clear();
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
        let n = self.segments(dist(p0, c) + dist(c, p));
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
        let n = self.segments(dist(p0, c0) + dist(c0, c1) + dist(c1, p));
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

fn signed_area(c: &[[f32; 2]]) -> f32 {
    let mut a = 0.0;
    for i in 0..c.len() {
        let (p, q) = (c[i], c[(i + 1) % c.len()]);
        a += p[0] * q[1] - q[0] * p[1];
    }
    0.5 * a
}

/// Nonzero winding number of `p` against closed polylines.
fn winding(contours: &[Vec<[f32; 2]>], p: [f32; 2]) -> i32 {
    let mut w = 0;
    for c in contours {
        for i in 0..c.len() {
            let (a, b) = (c[i], c[(i + 1) % c.len()]);
            let cross = (b[0] - a[0]) * (p[1] - a[1]) - (p[0] - a[0]) * (b[1] - a[1]);
            if a[1] <= p[1] {
                if b[1] > p[1] && cross > 0.0 {
                    w += 1;
                }
            } else if b[1] <= p[1] && cross < 0.0 {
                w -= 1;
            }
        }
    }
    w
}

/// Index of cell `(x, y)` along a Hilbert curve over a `n` x `n` grid, `n` a power of two.
pub fn hilbert(n: u32, mut x: u32, mut y: u32) -> u64 {
    let mut d = 0u64;
    let mut s = n / 2;
    while s > 0 {
        let rx = ((x & s) > 0) as u32;
        let ry = ((y & s) > 0) as u32;
        d += s as u64 * s as u64 * ((3 * rx) ^ ry) as u64;
        if ry == 0 {
            if rx == 1 {
                x = s.wrapping_sub(1).wrapping_sub(x) & (n - 1);
                y = s.wrapping_sub(1).wrapping_sub(y) & (n - 1);
            }
            std::mem::swap(&mut x, &mut y);
        }
        s /= 2;
    }
    d
}

/// Samples `glyph` of `face` at `size` (em size in logical units), with `px_per_unit` pixels
/// per logical unit setting the point spacing. `None` when the font has no outline for it.
pub fn sample_glyph(face: &Face, glyph: u32, size: f32, px_per_unit: f32) -> Option<GlyphPoints> {
    let font = face.skrifa();
    let outline = font.outline_glyphs().get(GlyphId::new(glyph))?;
    let unit_px = 1.0 / px_per_unit.max(1e-3);
    let mut pen = Flatten { k: size / face.units_per_em() as f32, unit_px, ..Default::default() };
    outline.draw(DrawSettings::unhinted(Size::unscaled(), face.location()), &mut pen).ok()?;
    pen.end();
    let mut contours = pen.contours;
    // Drop a closing point that repeats the first.
    for c in &mut contours {
        if c.len() > 1 && dist(c[0], c[c.len() - 1]) < 1e-5 {
            c.pop();
        }
    }
    contours.retain(|c| c.len() > 2);
    let mut out = GlyphPoints { bounds: [f32::MAX, f32::MAX, f32::MIN, f32::MIN], ..Default::default() };
    if contours.is_empty() {
        out.bounds = [0.0; 4];
        return Some(out);
    }
    for c in &contours {
        for p in c {
            let b = &mut out.bounds;
            *b = [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])];
        }
    }
    out.area = contours.iter().map(|c| signed_area(c)).sum::<f32>().abs();

    // Contours: largest first, each resampled by arc length.
    let mut order: Vec<usize> = (0..contours.len()).collect();
    order.sort_by(|&a, &b| signed_area(&contours[b]).abs().total_cmp(&signed_area(&contours[a]).abs()));
    let spacing = CONTOUR_SPACING * unit_px;
    for &ci in &order {
        let c = &contours[ci];
        let seg: Vec<f32> = (0..c.len()).map(|i| dist(c[i], c[(i + 1) % c.len()])).collect();
        let len: f32 = seg.iter().sum();
        out.perimeter += len;
        let n = ((len / spacing).round() as usize).max(3);
        let (mut i, mut acc) = (0usize, 0.0f32);
        for j in 0..n {
            let s = (j as f32 + 0.5) * len / n as f32;
            while i + 1 < c.len() && acc + seg[i] < s {
                acc += seg[i];
                i += 1;
            }
            let f = if seg[i] > 0.0 { ((s - acc) / seg[i]).clamp(0.0, 1.0) } else { 0.0 };
            let (a, b) = (c[i], c[(i + 1) % c.len()]);
            out.contour.push([a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]);
        }
    }

    // Interior: a jittered grid, nonzero rule, Hilbert order.
    let step = INTERIOR_SPACING * unit_px;
    let [x0, y0, x1, y1] = out.bounds;
    let (nx, ny) = (((x1 - x0) / step).ceil() as u32 + 1, ((y1 - y0) / step).ceil() as u32 + 1);
    let n = nx.max(ny).next_power_of_two();
    let mut inside: Vec<(u64, [f32; 2])> = Vec::new();
    for gy in 0..ny {
        for gx in 0..nx {
            let jx = rand_f32(glyph, gx, gy, 1) - 0.5;
            let jy = rand_f32(glyph, gx, gy, 2) - 0.5;
            let p = [x0 + (gx as f32 + 0.5 * jx) * step, y0 + (gy as f32 + 0.5 * jy) * step];
            if winding(&contours, p) != 0 {
                inside.push((hilbert(n, gx, gy), p));
            }
        }
    }
    inside.sort_by_key(|&(d, _)| d);
    out.interior = inside.into_iter().map(|(_, p)| p).collect();
    Some(out)
}

/// Sampled glyphs, kept per font, glyph and size (rounded to a quarter unit) and pixel scale.
#[derive(Default)]
pub struct GlyphPointCache {
    map: HashMap<(u32, u32, u32, u32), Arc<GlyphPoints>>,
}

impl GlyphPointCache {
    pub fn new() -> GlyphPointCache {
        GlyphPointCache::default()
    }

    pub fn get(&mut self, font: u32, face: &Face, glyph: u32, size: f32, px_per_unit: f32) -> Arc<GlyphPoints> {
        let key = (font, glyph, (size * 4.0).round() as u32, (px_per_unit * 16.0).round() as u32);
        self.map
            .entry(key)
            .or_insert_with(|| Arc::new(sample_glyph(face, glyph, size, px_per_unit).unwrap_or_default()))
            .clone()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vo_scene::font::{test_font_path, FontRegistry};

    fn face() -> Arc<Face> {
        let fonts = FontRegistry::new();
        fonts.add_file(&test_font_path()).unwrap();
        fonts.get(0).unwrap()
    }

    fn gid(face: &Face, c: char) -> u32 {
        face.skrifa().charmap().map(c).unwrap().to_u32()
    }

    #[test]
    fn letter_o_has_two_contours_and_a_hole() {
        let face = face();
        let o = sample_glyph(&face, gid(&face, 'o'), 40.0, 1.0).unwrap();
        // Outline of an 'o' at 40 px: two rings, roughly 2 * pi * 10 px and 2 * pi * 7 px.
        assert!((70.0..140.0).contains(&o.perimeter), "{}", o.perimeter);
        let n = o.contour.len() as f32;
        assert!((n - o.perimeter / CONTOUR_SPACING).abs() < 4.0, "{n}");
        // No interior point in the hole: the centre of the bounds is empty.
        let c = [(o.bounds[0] + o.bounds[2]) / 2.0, (o.bounds[1] + o.bounds[3]) / 2.0];
        let near = o.interior.iter().filter(|p| dist(**p, c) < 3.0).count();
        assert_eq!(near, 0);
        assert!(o.interior.len() as f32 > 0.6 * o.area / (INTERIOR_SPACING * INTERIOR_SPACING));
        // Above the baseline: y is negative.
        assert!(o.bounds[3] <= 1.0 && o.bounds[1] < -15.0, "{:?}", o.bounds);
    }

    #[test]
    fn sampling_is_fixed_and_cached() {
        let face = face();
        let g = gid(&face, 'g');
        let a = sample_glyph(&face, g, 24.0, 1.0).unwrap();
        let b = sample_glyph(&face, g, 24.0, 1.0).unwrap();
        assert_eq!(a.contour, b.contour);
        assert_eq!(a.interior, b.interior);
        let mut cache = GlyphPointCache::new();
        let c1 = cache.get(0, &face, g, 24.0, 1.0);
        let c2 = cache.get(0, &face, g, 24.0, 1.0);
        assert!(Arc::ptr_eq(&c1, &c2));
        cache.get(0, &face, g, 24.0, 2.0);
        assert_eq!(cache.len(), 2);
        // Twice the pixel scale gives about twice the contour points.
        let hi = sample_glyph(&face, g, 24.0, 2.0).unwrap();
        let ratio = hi.contour.len() as f32 / a.contour.len() as f32;
        assert!((1.8..2.2).contains(&ratio), "{ratio}");
    }

    #[test]
    fn space_has_no_points() {
        let face = face();
        let s = sample_glyph(&face, gid(&face, ' '), 24.0, 1.0).unwrap();
        assert!(s.contour.is_empty() && s.interior.is_empty());
    }

    #[test]
    fn hilbert_visits_every_cell_once_in_unit_steps() {
        let n = 8;
        let mut cells: Vec<(u64, u32, u32)> =
            (0..n).flat_map(|y| (0..n).map(move |x| (hilbert(n, x, y), x, y))).collect();
        cells.sort();
        for (i, w) in cells.windows(2).enumerate() {
            assert_eq!(w[0].0, i as u64);
            let step = w[0].1.abs_diff(w[1].1) + w[0].2.abs_diff(w[1].2);
            assert_eq!(step, 1, "{w:?}");
        }
    }
}
