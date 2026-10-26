//! Vector paths: strokes with arc length, trim and draw-on, contour morph, and filled paths.
//!
//! Two ways to draw a path:
//!
//! - **Fill** through the Slug glyph shader. Register the path once with
//!   `FontRegistry::add_path` and draw it as a `GlyphInstance` with font id [`PATH_FONT`]
//!   ([`fill_glyph`]). Static stroked icons go the same way after `kurbo::stroke`.
//! - **Stroke** through the renderer's vector pipeline. [`Outline::new`] flattens a path once
//!   into quadratic and straight [`VectorSeg`]s, each with the arc length at its ends. Trim
//!   ([`Outline::trim`], [`Outline::window`]) and morph ([`Morph::sample`]) make new segments
//!   each frame on the CPU, which costs microseconds, so every frame stays a pure function of
//!   `t`. One [`VectorInstance`] draws one contour: the fragment shader takes the nearest
//!   segment, so joins are round and a translucent stroke never blends twice.
//!
//! [`VectorKind::Blob`] is a smooth-min union of two circles, for merging dots.

use serde::Serialize;

pub use kurbo;
use kurbo::{BezPath, CubicBez, ParamCurve, ParamCurveArclen, ParamCurveDeriv, PathEl, QuadBez};

use crate::display::{DisplayList, GlyphInstance, Item, ItemKind, NO_WORD};
use crate::geom::{Affine, Color, Point};

/// `GlyphInstance::font` for a filled path from `FontRegistry::add_path`. `glyph` is the path
/// id and `size` is logical units per path unit.
pub const PATH_FONT: u32 = u32::MAX - 1;

/// Arc length accuracy, in path units.
const ACCURACY: f64 = 1e-3;

/// One stroke segment in layer space: a quadratic Bezier from `p0` through control `p1` to
/// `p2`. A straight segment has `p1` at the midpoint. `s0` and `s1` are the arc lengths along
/// the contour at its ends.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[repr(C)]
pub struct VectorSeg {
    pub p0: Point,
    pub p1: Point,
    pub p2: Point,
    pub s0: f32,
    pub s1: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[repr(u32)]
pub enum VectorKind {
    /// A round-capped stroke of the segments `segs`.
    #[default]
    Stroke,
    /// The smooth-min union of circle `a` and circle `b` (`cx, cy, r, k`; `k` from `a` is the
    /// blend radius). Filled with `c0` at `a` and `c1` at `b`. No segments.
    Blob,
}

/// One contour stroke or one blob. Colours are straight sRGB like every display colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[repr(C)]
pub struct VectorInstance {
    pub kind: VectorKind,
    /// Range into `DisplayList::vector_segs`: start and count.
    pub segs: [u32; 2],
    /// Stroke width, logical units.
    pub width: f32,
    /// Glow radius outside the stroke edge, logical units; 0 for none.
    pub glow: f32,
    /// Colour `c0` up to arc length `ramp[0]`, `c1` from `ramp[1]`, mixed between.
    pub c0: Color,
    pub c1: Color,
    pub ramp: [f32; 2],
    /// Blob circles, `cx, cy, r, k`.
    pub a: [f32; 4],
    pub b: [f32; 4],
}

/// How to stroke a contour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokeStyle {
    pub width: f32,
    pub glow: f32,
    pub c0: Color,
    pub c1: Color,
    pub ramp: [f32; 2],
}

impl StrokeStyle {
    pub fn solid(width: f32, color: Color) -> StrokeStyle {
        StrokeStyle { width, glow: 0.0, c0: color, c1: color, ramp: [0.0, 0.0] }
    }

    /// Colour `c0` at arc length `from`, `c1` at `to`.
    pub fn ramp(mut self, from: f32, c0: Color, to: f32, c1: Color) -> StrokeStyle {
        self.c0 = c0;
        self.c1 = c1;
        self.ramp = [from, to];
        self
    }

    pub fn glow(mut self, radius: f32) -> StrokeStyle {
        self.glow = radius;
        self
    }
}

/// A contour flattened to segments.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contour {
    pub segs: Vec<VectorSeg>,
    pub closed: bool,
}

impl Contour {
    /// The arc length at the end. A trimmed piece keeps its original arc lengths, so for it
    /// this is where it ends, not how long it is.
    pub fn length(&self) -> f32 {
        self.segs.last().map_or(0.0, |s| s.s1)
    }

    /// The part of the contour between arc lengths `from` and `to`, with the original arc
    /// lengths kept on the pieces. Boundary segments are cut exactly.
    pub fn trim(&self, from: f32, to: f32) -> Contour {
        let mut out = Contour { segs: Vec::new(), closed: false };
        if to <= from {
            return out;
        }
        for s in &self.segs {
            if s.s1 <= from || s.s0 >= to {
                continue;
            }
            if s.s0 >= from && s.s1 <= to {
                out.segs.push(*s);
                continue;
            }
            let q = quad(s);
            let t0 = if s.s0 < from { param_at(s, (from - s.s0) as f64) } else { 0.0 };
            let t1 = if s.s1 > to { param_at(s, (to - s.s0) as f64) } else { 1.0 };
            let sub = q.subsegment(t0..t1);
            out.segs.push(seg(sub, s.s0.max(from), s.s1.min(to)));
        }
        out
    }

    /// Moves every point by `m`. Arc lengths are scaled by `m`'s mean scale, which is exact
    /// for uniform scale and rotation.
    pub fn transform(&self, m: &Affine) -> Contour {
        let k = (m.a * m.d - m.b * m.c).abs().sqrt();
        let segs = self
            .segs
            .iter()
            .map(|s| VectorSeg { p0: m.apply(s.p0), p1: m.apply(s.p1), p2: m.apply(s.p2), s0: s.s0 * k, s1: s.s1 * k })
            .collect();
        Contour { segs, closed: self.closed }
    }

    /// A polyline through `pts` as straight segments.
    pub fn polyline(pts: &[Point], closed: bool) -> Contour {
        let mut segs = Vec::with_capacity(pts.len());
        let mut s = 0.0;
        for w in pts.windows(2) {
            let len = dist(w[0], w[1]);
            if len <= 0.0 {
                continue;
            }
            segs.push(line(w[0], w[1], s, s + len));
            s += len;
        }
        Contour { segs, closed }
    }

    /// The point and unit tangent at arc length `s`, clamped to the ends.
    pub fn at(&self, s: f32) -> Option<(Point, Point)> {
        let seg = self.segs.iter().find(|g| s <= g.s1).or(self.segs.last())?;
        let q = quad(seg);
        let t = param_at(seg, (s - seg.s0) as f64);
        let p = q.eval(t);
        let d = q.deriv().eval(t).to_vec2();
        let d = if d.hypot() > 1e-9 { d.normalize() } else { (q.p2 - q.p0).normalize() };
        Some((Point::new(p.x as f32, p.y as f32), Point::new(d.x as f32, d.y as f32)))
    }

    /// Resamples to `n` points evenly spaced by arc length, the last one at the end (for a
    /// closed contour that is the start again).
    pub fn resample(&self, n: usize) -> Vec<Point> {
        let len = self.length();
        (0..n).map(|i| self.at(len * i as f32 / (n - 1).max(1) as f32).map_or(Point::ZERO, |p| p.0)).collect()
    }
}

/// A path flattened to contours of segments with arc length. Flatten once, then trim or
/// sample every frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outline {
    pub contours: Vec<Contour>,
}

impl Outline {
    /// Flattens `path` (layer units, y down). Cubics become quadratics within `tolerance`.
    pub fn new(path: &BezPath, tolerance: f64) -> Outline {
        let mut out = Outline::default();
        let mut cur = Contour::default();
        let (mut start, mut at) = (kurbo::Point::ZERO, kurbo::Point::ZERO);
        let mut s = 0.0f32;
        let flush = |cur: &mut Contour, out: &mut Outline| {
            if !cur.segs.is_empty() {
                out.contours.push(std::mem::take(cur));
            }
            *cur = Contour::default();
        };
        for el in path.elements() {
            match *el {
                PathEl::MoveTo(p) => {
                    flush(&mut cur, &mut out);
                    s = 0.0;
                    start = p;
                    at = p;
                }
                PathEl::LineTo(p) => {
                    push_quad(&mut cur, QuadBez::new(at, at.midpoint(p), p), &mut s);
                    at = p;
                }
                PathEl::QuadTo(c, p) => {
                    push_quad(&mut cur, QuadBez::new(at, c, p), &mut s);
                    at = p;
                }
                PathEl::CurveTo(c1, c2, p) => {
                    for (_, _, q) in CubicBez::new(at, c1, c2, p).to_quads(tolerance) {
                        push_quad(&mut cur, q, &mut s);
                    }
                    at = p;
                }
                PathEl::ClosePath => {
                    if at != start {
                        push_quad(&mut cur, QuadBez::new(at, at.midpoint(start), start), &mut s);
                    }
                    cur.closed = true;
                    flush(&mut cur, &mut out);
                    s = 0.0;
                    at = start;
                }
            }
        }
        flush(&mut cur, &mut out);
        out
    }

    /// Total length of all contours.
    pub fn length(&self) -> f32 {
        self.contours.iter().map(Contour::length).sum()
    }

    /// The part between `from` and `to`, measured along all contours in order, the way a
    /// pen would draw them one after the other.
    pub fn trim(&self, from: f32, to: f32) -> Outline {
        let mut base = 0.0;
        let mut out = Outline::default();
        for c in &self.contours {
            let len = c.length();
            let piece = c.trim(from - base, to - base);
            if !piece.segs.is_empty() {
                out.contours.push(piece);
            }
            base += len;
        }
        out
    }

    /// For a closed contour: the stretch of length `len` that ends at arc length `head`, which
    /// wraps around the start. Arc lengths stay increasing across the seam, so a colour ramp
    /// from `head - len` to `head` gives a fading tail behind a light head.
    pub fn window(&self, contour: usize, head: f32, len: f32) -> Contour {
        let c = &self.contours[contour];
        let total = c.length();
        let head = head.rem_euclid(total);
        let from = head - len.min(total);
        if from >= 0.0 {
            return c.trim(from, head);
        }
        let mut out = c.trim(from + total, total);
        for s in &mut out.segs {
            s.s0 -= total;
            s.s1 -= total;
        }
        out.segs.extend(c.trim(0.0, head).segs);
        out
    }

    pub fn transform(&self, m: &Affine) -> Outline {
        Outline { contours: self.contours.iter().map(|c| c.transform(m)).collect() }
    }
}

/// A morph between two outlines by matched, resampled contours.
///
/// Contours pair up by length: the longest target contour takes the longest source contour,
/// and so on. Each pair is resampled to `n` points by arc length; a closed contour's start is
/// rotated, and an open one maybe reversed, to best match the other. A contour with no
/// partner shrinks to (or grows from) its own centroid, and its [`MorphPart::weight`] goes to
/// 0 so the caller can thin or fade it.
#[derive(Clone, Debug)]
pub struct Morph {
    pairs: Vec<(Vec<Point>, Vec<Point>, Presence)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Presence {
    Both,
    FromOnly,
    ToOnly,
}

/// One contour of a morph at some `t`.
#[derive(Clone, Debug)]
pub struct MorphPart {
    pub contour: Contour,
    /// 1 for a contour both ends have; for one that only one end has, 1 at that end and 0 at
    /// the other.
    pub weight: f32,
}

impl Morph {
    pub fn new(from: &Outline, to: &Outline, n: usize) -> Morph {
        let n = n.max(2);
        // Longest target contours choose first, each the longest source contour still free.
        let by_len = |o: &Outline| {
            let mut ix: Vec<usize> = (0..o.contours.len()).collect();
            ix.sort_by(|a, b| o.contours[*b].length().total_cmp(&o.contours[*a].length()));
            ix
        };
        let mut free = by_len(from);
        let mut pairs = Vec::new();
        for j in by_len(to) {
            let b = &to.contours[j];
            let pb = b.resample(n);
            if free.is_empty() {
                pairs.push((vec![centroid(&pb); n], pb, Presence::ToOnly));
                continue;
            }
            let a = &from.contours[free.remove(0)];
            let mut pa = a.resample(n);
            let mut pb = pb;
            if b.closed {
                pb = best_rotation(&pa, &pb);
            } else if a.closed {
                pa = best_rotation(&pb, &pa);
            }
            let reversed: Vec<Point> = pb.iter().rev().copied().collect();
            if !b.closed && cost(&pa, &reversed) < cost(&pa, &pb) {
                pb = reversed;
            }
            pairs.push((pa, pb, Presence::Both));
        }
        for i in free {
            let pa = from.contours[i].resample(n);
            let c = centroid(&pa);
            pairs.push((pa, vec![c; n], Presence::FromOnly));
        }
        Morph { pairs }
    }

    /// The contours at `t` (0 is `from`, 1 is `to`). `t` outside 0..1 extrapolates, so a
    /// spring's overshoot shows.
    pub fn sample(&self, t: f32) -> Vec<MorphPart> {
        self.pairs
            .iter()
            .map(|(a, b, presence)| {
                let pts: Vec<Point> =
                    a.iter().zip(b).map(|(p, q)| Point::new(p.x + (q.x - p.x) * t, p.y + (q.y - p.y) * t)).collect();
                let weight = match presence {
                    Presence::Both => 1.0,
                    Presence::FromOnly => (1.0 - t).clamp(0.0, 1.0),
                    Presence::ToOnly => t.clamp(0.0, 1.0),
                };
                MorphPart { contour: Contour::polyline(&pts, false), weight }
            })
            .collect()
    }
}

/// Appends a stroke of `contour` to `list`'s arenas and returns the item that draws it, for
/// the caller to put in a layer. `None` for an empty contour.
pub fn push_stroke(list: &mut DisplayList, contour: &Contour, style: &StrokeStyle) -> Option<Item> {
    if contour.segs.is_empty() || style.width <= 0.0 {
        return None;
    }
    let start = list.vector_segs.len() as u32;
    list.vector_segs.extend_from_slice(&contour.segs);
    let index = list.vectors.len() as u32;
    list.vectors.push(VectorInstance {
        kind: VectorKind::Stroke,
        segs: [start, contour.segs.len() as u32],
        width: style.width,
        glow: style.glow,
        c0: style.c0,
        c1: style.c1,
        ramp: style.ramp,
        a: [0.0; 4],
        b: [0.0; 4],
    });
    Some(Item { kind: ItemKind::Vectors, start: index, len: 1 })
}

/// Appends a blob: circles `a` and `b` (`centre, radius`) merged with blend radius `k`.
pub fn push_blob(list: &mut DisplayList, a: (Point, f32), b: (Point, f32), k: f32, c0: Color, c1: Color) -> Item {
    let index = list.vectors.len() as u32;
    list.vectors.push(VectorInstance {
        kind: VectorKind::Blob,
        c0,
        c1,
        a: [a.0.x, a.0.y, a.1, k],
        b: [b.0.x, b.0.y, b.1, 0.0],
        ..Default::default()
    });
    Item { kind: ItemKind::Vectors, start: index, len: 1 }
}

/// A filled path from `FontRegistry::add_path`, with its path origin at `pos` and `scale`
/// logical units per path unit.
pub fn fill_glyph(path: u32, pos: Point, scale: f32, color: Color) -> GlyphInstance {
    GlyphInstance { pos, size: scale, font: PATH_FONT, glyph: path, word: NO_WORD, color }
}

/// A small line icon set on a 24 x 24 grid (y down), drawn to be stroked about 2 units wide.
/// Fill one with [`icons::outline`], which strokes it to a closed outline for the glyph path.
pub mod icons {
    use kurbo::{Arc, BezPath, Cap, Join, RoundedRect, Shape, Stroke, StrokeOpts};

    /// A microphone: the capsule, the holder arc and the stem. Three contours.
    pub fn mic() -> BezPath {
        let mut p = RoundedRect::new(9.0, 2.0, 15.0, 14.0, 3.0).to_path(0.01);
        let mut holder = BezPath::new();
        holder.move_to((5.0, 9.0));
        holder.line_to((5.0, 11.0));
        Arc::new((12.0, 11.0), (7.0, 7.0), std::f64::consts::PI, -std::f64::consts::PI, 0.0)
            .to_cubic_beziers(0.01, |c1, c2, e| holder.curve_to(c1, c2, e));
        holder.line_to((19.0, 9.0));
        p.extend(holder);
        p.move_to((12.0, 18.0));
        p.line_to((12.0, 22.0));
        p
    }

    /// A check mark, one open contour drawn from the short arm.
    pub fn check() -> BezPath {
        let mut p = BezPath::new();
        p.move_to((4.5, 12.5));
        p.line_to((9.5, 17.5));
        p.line_to((19.5, 6.5));
        p
    }

    /// A stop square with round corners.
    pub fn stop() -> BezPath {
        RoundedRect::new(6.0, 6.0, 18.0, 18.0, 2.5).to_path(0.01)
    }

    /// `path` stroked `width` wide with round caps and joins, as a closed outline to fill.
    pub fn outline(path: &BezPath, width: f64) -> BezPath {
        let style = Stroke::new(width).with_caps(Cap::Round).with_join(Join::Round);
        kurbo::stroke(path.iter(), &style, &StrokeOpts::default(), 0.01)
    }
}

/// The curve parameter at arc length `len` from the segment's start.
fn param_at(s: &VectorSeg, len: f64) -> f64 {
    let total = (s.s1 - s.s0) as f64;
    let q = quad(s);
    let t = if total > 0.0 { q.inv_arclen(len.clamp(0.0, total), ACCURACY) } else { 0.0 };
    if t.is_finite() {
        t
    } else {
        (len / total).clamp(0.0, 1.0)
    }
}

fn quad(s: &VectorSeg) -> QuadBez {
    let k = |p: Point| kurbo::Point::new(p.x as f64, p.y as f64);
    QuadBez::new(k(s.p0), k(s.p1), k(s.p2))
}

fn seg(q: QuadBez, s0: f32, s1: f32) -> VectorSeg {
    let p = |p: kurbo::Point| Point::new(p.x as f32, p.y as f32);
    VectorSeg { p0: p(q.p0), p1: p(q.p1), p2: p(q.p2), s0, s1 }
}

fn line(a: Point, b: Point, s0: f32, s1: f32) -> VectorSeg {
    VectorSeg { p0: a, p1: Point::new(0.5 * (a.x + b.x), 0.5 * (a.y + b.y)), p2: b, s0, s1 }
}

fn push_quad(c: &mut Contour, q: QuadBez, s: &mut f32) {
    // kurbo's closed form gives NaN for some straight quadratics; their length is the chord.
    let curved = (q.p0.to_vec2() - 2.0 * q.p1.to_vec2() + q.p2.to_vec2()).hypot2() > 1e-12 * (q.p2 - q.p0).hypot2();
    let len = if curved { q.arclen(ACCURACY) } else { (q.p2 - q.p0).hypot() };
    let len = if len.is_finite() { len as f32 } else { (q.p2 - q.p0).hypot() as f32 };
    if len <= 1e-6 {
        return;
    }
    c.segs.push(seg(q, *s, *s + len));
    *s += len;
}

fn dist(a: Point, b: Point) -> f32 {
    (b.x - a.x).hypot(b.y - a.y)
}

fn centroid(pts: &[Point]) -> Point {
    let n = pts.len().max(1) as f32;
    let (x, y) = pts.iter().fold((0.0, 0.0), |(x, y), p| (x + p.x, y + p.y));
    Point::new(x / n, y / n)
}

/// Sum of squared distances between matching points.
fn cost(a: &[Point], b: &[Point]) -> f32 {
    a.iter().zip(b).map(|(p, q)| (p.x - q.x).powi(2) + (p.y - q.y).powi(2)).sum()
}

/// `b` (a closed loop whose last point repeats the first) rotated so its points best match
/// `a`'s, by the sum of squared distances.
fn best_rotation(a: &[Point], b: &[Point]) -> Vec<Point> {
    let m = b.len() - 1;
    let ring = &b[..m];
    let cost = |r: usize| -> f32 {
        a.iter()
            .enumerate()
            .map(|(i, p)| {
                let q = ring[(i + r) % m];
                (p.x - q.x).powi(2) + (p.y - q.y).powi(2)
            })
            .sum()
    };
    let r = (0..m).min_by(|x, y| cost(*x).total_cmp(&cost(*y))).unwrap_or(0);
    (0..b.len()).map(|i| ring[(i + r) % m]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::Shape;

    fn square() -> Outline {
        Outline::new(&kurbo::Rect::new(0.0, 0.0, 10.0, 10.0).to_path(0.1), 0.01)
    }

    #[test]
    fn square_has_four_sides_and_length_40() {
        let o = square();
        assert_eq!(o.contours.len(), 1);
        assert!(o.contours[0].closed);
        assert_eq!(o.contours[0].segs.len(), 4);
        assert!((o.length() - 40.0).abs() < 1e-4);
    }

    #[test]
    fn circle_length_is_close() {
        let o = Outline::new(&kurbo::Circle::new((0.0, 0.0), 10.0).to_path(0.01), 0.01);
        assert!((o.length() - std::f32::consts::TAU * 10.0).abs() < 0.05, "{}", o.length());
    }

    #[test]
    fn trim_cuts_exactly_and_keeps_arc_lengths() {
        let o = square().trim(5.0, 25.0);
        let c = &o.contours[0];
        assert!((c.segs[0].s0 - 5.0).abs() < 1e-4);
        assert!((c.length() - 25.0).abs() < 1e-4);
        assert!((c.segs[0].p0.x - 5.0).abs() < 1e-3 && c.segs[0].p0.y.abs() < 1e-3);
        let end = c.segs.last().unwrap().p2;
        assert!((end.x - 5.0).abs() < 1e-3 && (end.y - 10.0).abs() < 1e-3, "{end:?}");
    }

    #[test]
    fn window_wraps_the_seam_with_increasing_arc_length() {
        let w = square().window(0, 3.0, 8.0);
        assert!((w.segs[0].s0 + 5.0).abs() < 1e-4);
        assert!((w.length() - 3.0).abs() < 1e-4);
        assert!(w.segs.windows(2).all(|p| p[0].s1 <= p[1].s0 + 1e-4));
    }

    #[test]
    fn morph_ends_match_inputs() {
        let a = square();
        let b = Outline::new(&kurbo::Circle::new((5.0, 5.0), 5.0).to_path(0.01), 0.01);
        let m = Morph::new(&a, &b, 64);
        let at0 = m.sample(0.0);
        assert!((at0[0].contour.length() - 40.0).abs() < 0.5);
        let at1 = m.sample(1.0);
        assert!((at1[0].contour.length() - 31.4).abs() < 0.5, "{}", at1[0].contour.length());
    }

    #[test]
    fn diagonal_lines_have_finite_lengths() {
        let o = Outline::new(&icons::check(), 0.01);
        assert!((o.length() - (50f32.sqrt() + 221f32.sqrt())).abs() < 1e-3, "{}", o.length());
        assert!(o.contours[0].resample(33).iter().all(|p| p.x.is_finite() && p.y.is_finite()));
        let t = &o.trim(2.0, 9.0).contours[0];
        assert!((t.segs[0].s0 - 2.0).abs() < 1e-4 && (t.length() - 9.0).abs() < 1e-4);
    }

    #[test]
    fn extra_contour_fades() {
        let a = Outline { contours: [square().contours, square().contours].concat() };
        let m = Morph::new(&a, &square(), 16);
        let parts = m.sample(0.75);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].weight, 1.0);
        assert_eq!(parts[1].weight, 0.25);
    }
}
