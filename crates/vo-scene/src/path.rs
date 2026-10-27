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

use crate::display::{DisplayList, GlyphInstance, Item, ItemKind, MATERIAL_SHIFT, NO_WORD};
use crate::geom::{Affine, Color, Point};

/// `GlyphInstance::font` for a filled path from `FontRegistry::add_path`. `glyph` is the path
/// id and `size` is logical units per path unit.
pub const PATH_FONT: u32 = u32::MAX - 1;

/// Arc length accuracy, in path units.
const ACCURACY: f64 = 1e-3;

/// One stroke segment in layer space: a quadratic Bezier from `p0` through control `p1` to
/// `p2`. A straight segment has `p1` at the midpoint. `s0` and `s1` are the arc lengths along
/// the contour at its ends; `w` multiplies the stroke width at each end, mixed between, for
/// ribbons whose width varies.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[repr(C)]
pub struct VectorSeg {
    pub p0: Point,
    pub p1: Point,
    pub p2: Point,
    pub s0: f32,
    pub s1: f32,
    pub w: [f32; 2],
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
    /// The inside of all the segments' contours by the nonzero winding rule, with holes where
    /// contours wind the other way. For fills that change every frame (morphs); a fixed fill
    /// is cheaper through the glyph path (`fill_glyph`).
    Fill,
}

/// What the colour ramp of a vector instance runs along.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub enum VectorPaint {
    /// Arc length along the contour, in logical units.
    #[default]
    Along,
    /// 0 at `p0` to 1 at `p1`, measured along the line between them (layer space).
    Linear { p0: Point, p1: Point },
    /// The angle around `centre` as a fraction of a turn, 0 at `start` radians (clockwise on
    /// screen). With `mirror` it goes 0, 1, 0 around the turn, so there is no seam.
    Conic { centre: Point, start: f32, mirror: bool },
    /// Stripes across `normal` (a unit vector), one every `period` logical units, each
    /// `duty` of the period wide, shifted by `phase` periods: 1 on a stripe, 0 between,
    /// with an antialiased edge. Use a ramp from 0 to 1.
    Stripes { normal: Point, period: f32, duty: f32, phase: f32 },
    /// A grid: lines `width` wide every `cell.x` across and `cell.y` down, with one through
    /// `origin`, or dots `width` across at the crossings. A cell of 0 leaves that axis out.
    /// 1 on a line or dot, 0 between. Use a ramp from 0 to 1.
    Grid { origin: Point, cell: Point, width: f32, dots: bool },
}

/// `VectorInstance::flags` bit: the stroke is one physical pixel wide at any scale.
pub const VECTOR_HAIRLINE: u32 = 1;

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
    /// Colour `c0` up to `ramp[0]`, `c1` from `ramp[1]`, mixed between in OKLab. The ramp
    /// runs along `paint`.
    pub c0: Color,
    pub c1: Color,
    pub ramp: [f32; 2],
    pub paint: VectorPaint,
    /// `VECTOR_HAIRLINE`, and the material id (FX15) from bit `display::MATERIAL_SHIFT` up.
    pub flags: u32,
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
    pub paint: VectorPaint,
    pub hairline: bool,
    /// Material id (FX15) from `DisplayList::add_material`, 0 for none.
    pub material: u32,
}

impl StrokeStyle {
    pub fn solid(width: f32, color: Color) -> StrokeStyle {
        StrokeStyle {
            width,
            glow: 0.0,
            c0: color,
            c1: color,
            ramp: [0.0, 0.0],
            paint: VectorPaint::Along,
            hairline: false,
            material: 0,
        }
    }

    /// One physical pixel wide at any scale.
    pub fn hairline(color: Color) -> StrokeStyle {
        StrokeStyle { hairline: true, ..StrokeStyle::solid(1.0, color) }
    }

    /// Colour `c0` at `from`, `c1` at `to`, along the paint (arc length unless set).
    pub fn ramp(mut self, from: f32, c0: Color, to: f32, c1: Color) -> StrokeStyle {
        self.c0 = c0;
        self.c1 = c1;
        self.ramp = [from, to];
        self
    }

    /// A linear gradient from `c0` at `p0` to `c1` at `p1`.
    pub fn linear(self, p0: Point, c0: Color, p1: Point, c1: Color) -> StrokeStyle {
        StrokeStyle { paint: VectorPaint::Linear { p0, p1 }, ..self.ramp(0.0, c0, 1.0, c1) }
    }

    /// A conic gradient around `centre`: `c0` at angle `start`, `c1` half a turn on, and back.
    pub fn conic(self, centre: Point, start: f32, c0: Color, c1: Color) -> StrokeStyle {
        StrokeStyle { paint: VectorPaint::Conic { centre, start, mirror: true }, ..self.ramp(0.0, c0, 1.0, c1) }
    }

    pub fn glow(mut self, radius: f32) -> StrokeStyle {
        self.glow = radius;
        self
    }

    /// Draws with material `id` from `DisplayList::add_material`. The material reads the
    /// stroke's colour and its distance to the centre line.
    pub fn material(mut self, id: u32) -> StrokeStyle {
        self.material = id;
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
            let w = |t: f64| s.w[0] + (s.w[1] - s.w[0]) * t as f32;
            out.segs.push(VectorSeg { w: [w(t0), w(t1)], ..seg(sub, s.s0.max(from), s.s1.min(to)) });
        }
        out
    }

    /// Dashes: `pattern` alternates dash and gap lengths, starting with a dash, shifted back by
    /// `offset` (a growing offset makes the dashes march). Each dash is its own piece, with
    /// round caps when stroked.
    pub fn dash(&self, pattern: &[f32], offset: f32) -> Vec<Contour> {
        let period: f32 = pattern.iter().sum();
        let (Some(first), true) = (self.segs.first(), period > 0.0) else { return Vec::new() };
        let (start, end) = (first.s0, self.length());
        let mut out = Vec::new();
        let mut at = start - offset.rem_euclid(period);
        let mut i = 0;
        while at < end {
            let len = pattern[i % pattern.len()];
            if i % 2 == 0 {
                let piece = self.trim(at.max(start), (at + len).min(end));
                if !piece.segs.is_empty() {
                    out.push(piece);
                }
            }
            at += len;
            i += 1;
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
            .map(|s| VectorSeg {
                p0: m.apply(s.p0),
                p1: m.apply(s.p1),
                p2: m.apply(s.p2),
                s0: s.s0 * k,
                s1: s.s1 * k,
                ..*s
            })
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
                // Try both directions: a clockwise contour paired with a counter-clockwise
                // one would turn inside out on the way.
                let rev: Vec<Point> = pb.iter().rev().copied().collect();
                let (fwd, back) = (best_rotation(&pa, &pb), best_rotation(&pa, &rev));
                pb = if cost(&pa, &back) < cost(&pa, &fwd) { back } else { fwd };
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
        paint: style.paint,
        flags: if style.hairline { VECTOR_HAIRLINE } else { 0 } | style.material << MATERIAL_SHIFT,
        a: [0.0; 4],
        b: [0.0; 4],
    });
    Some(Item { kind: ItemKind::Vectors, start: index, len: 1 })
}

/// Appends the inside of `contours` (nonzero winding, open contours closed by a straight
/// line) and returns the item that draws it. `style.width` is not used; glow and paint are.
pub fn push_fill<'a>(
    list: &mut DisplayList,
    contours: impl IntoIterator<Item = &'a Contour>,
    style: &StrokeStyle,
) -> Option<Item> {
    let start = list.vector_segs.len() as u32;
    for c in contours {
        let (Some(first), Some(last)) = (c.segs.first(), c.segs.last()) else { continue };
        let (a, b) = (first.p0, last.p2);
        list.vector_segs.extend_from_slice(&c.segs);
        if dist(a, b) > 1e-4 {
            list.vector_segs.push(line(b, a, last.s1, last.s1 + dist(a, b)));
        }
    }
    let count = list.vector_segs.len() as u32 - start;
    if count == 0 {
        return None;
    }
    let index = list.vectors.len() as u32;
    list.vectors.push(VectorInstance {
        kind: VectorKind::Fill,
        segs: [start, count],
        width: 0.0,
        glow: style.glow,
        c0: style.c0,
        c1: style.c1,
        ramp: style.ramp,
        paint: style.paint,
        flags: style.material << MATERIAL_SHIFT,
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

/// Contours made from numbers: arcs, waves, sparklines, ribbons, and the closed-form motion of
/// spinners and progress arcs. All in layer units, y down; angles in radians, clockwise on
/// screen, 0 pointing right.
pub mod make {
    use super::{Contour, Outline, VectorSeg};
    use crate::geom::Point;
    use kurbo::{Arc, BezPath};

    /// An arc of a circle, from `start` sweeping `sweep` (negative goes the other way).
    pub fn arc(centre: Point, r: f32, start: f32, sweep: f32) -> Contour {
        let mut p = BezPath::new();
        let arc = Arc::new((centre.x as f64, centre.y as f64), (r as f64, r as f64), start as f64, sweep as f64, 0.0);
        p.move_to(arc.center + kurbo::Vec2::from_angle(start as f64) * r as f64);
        arc.to_cubic_beziers(0.01, |a, b, c| p.curve_to(a, b, c));
        Outline::new(&p, 0.01).contours.into_iter().next().unwrap_or_default()
    }

    /// A whole circle starting at the top and running clockwise, so trimming it from 0 fills
    /// it like a clock hand.
    pub fn ring(centre: Point, r: f32) -> Contour {
        let mut c = arc(centre, r, -std::f32::consts::FRAC_PI_2, std::f32::consts::TAU);
        c.closed = true;
        c
    }

    /// A determinate progress arc at `fraction` (0 to 1) of the ring.
    pub fn progress(centre: Point, r: f32, fraction: f32) -> Contour {
        let ring = ring(centre, r);
        let len = ring.length();
        ring.trim(0.0, fraction.clamp(0.0, 1.0) * len)
    }

    /// An indeterminate spinner at time `t`: an arc whose head and tail chase each other with
    /// eased speeds, growing to 270 degrees and shrinking to 20, while the whole thing turns.
    /// Closed form in `t`, one cycle every 1.4 s.
    pub fn spinner(centre: Point, r: f32, t: f64) -> Contour {
        let [(start, _), (end, _)] = spinner_ends(t);
        arc(centre, r, start as f32, (end - start) as f32)
    }

    /// The spinner's tail and head angles at `t`, in radians clockwise from +x and never
    /// wrapped, each with its velocity in radians per second. For handing a spinner over to
    /// something else without a jump.
    pub fn spinner_ends(t: f64) -> [(f64, f64); 2] {
        const PERIOD: f64 = 1.4;
        const TURN_RATE: f64 = 2.0;
        // Smoothstep and its slope, flat outside 0..1.
        let ease = |x: f64| {
            let x = x.clamp(0.0, 1.0);
            (x * x * (3.0 - 2.0 * x), 6.0 * x * (1.0 - x))
        };
        let cycle = (t / PERIOD).floor();
        let u = t / PERIOD - cycle;
        let (min, max) = (20f64.to_radians(), 270f64.to_radians());
        // Each cycle the head leads by (max - min); the tail catches up in the second half.
        let (head, dhead) = ease(u * 2.0);
        let (tail, dtail) = ease(u * 2.0 - 1.0);
        let k = (max - min) * 2.0 / PERIOD;
        let base = cycle * (max - min) + t * TURN_RATE;
        [(base + tail * (max - min), TURN_RATE + dtail * k), (base + head * (max - min) + min, TURN_RATE + dhead * k)]
    }

    /// A sine wave across `x0..x1` around the line `y`: amplitude `amp`, `wavelength`, moved
    /// along by `phase` radians. For wavy underlines and ribbons.
    pub fn wave(x0: f32, x1: f32, y: f32, amp: f32, wavelength: f32, phase: f32) -> Contour {
        let steps = (((x1 - x0) / wavelength) * 16.0).ceil().max(2.0) as usize;
        let pts: Vec<Point> = (0..=steps)
            .map(|i| {
                let x = x0 + (x1 - x0) * i as f32 / steps as f32;
                Point::new(x, y + amp * (std::f32::consts::TAU * (x - x0) / wavelength + phase).sin())
            })
            .collect();
        Contour::polyline(&pts, false)
    }

    /// A straight underline from `x0` to `x1` at `y`.
    pub fn underline(x0: f32, x1: f32, y: f32) -> Contour {
        Contour::polyline(&[Point::new(x0, y), Point::new(x1, y)], false)
    }

    /// A sparkline: `values` (0 at the bottom, 1 at the top) spread across `rect` (x, y, w, h).
    pub fn sparkline(values: &[f32], x: f32, y: f32, w: f32, h: f32) -> Contour {
        let n = values.len().max(2) - 1;
        let pts: Vec<Point> = values
            .iter()
            .enumerate()
            .map(|(i, v)| Point::new(x + w * i as f32 / n as f32, y + h * (1.0 - v.clamp(0.0, 1.0))))
            .collect();
        Contour::polyline(&pts, false)
    }

    /// A ribbon: a polyline through `pts` whose width factor is `widths[i]` at point `i`
    /// (multiplied by the stroke width).
    pub fn ribbon(pts: &[Point], widths: &[f32]) -> Contour {
        let mut c = Contour::polyline(pts, false);
        // `polyline` skips zero-length steps, so match widths by segment end points.
        let mut j = 0;
        for s in &mut c.segs {
            while j + 1 < pts.len() && pts[j] != s.p0 {
                j += 1;
            }
            let w0 = widths.get(j).copied().unwrap_or(1.0);
            let w1 = widths.get(j + 1).copied().unwrap_or(w0);
            *s = VectorSeg { w: [w0, w1], ..*s };
        }
        c
    }

    /// Four corner brackets around `x, y, w, h`, each arm `arm` long.
    pub fn brackets(x: f32, y: f32, w: f32, h: f32, arm: f32) -> Vec<Contour> {
        let (r, b) = (x + w, y + h);
        [
            [Point::new(x, y + arm), Point::new(x, y), Point::new(x + arm, y)],
            [Point::new(r - arm, y), Point::new(r, y), Point::new(r, y + arm)],
            [Point::new(r, b - arm), Point::new(r, b), Point::new(r - arm, b)],
            [Point::new(x + arm, b), Point::new(x, b), Point::new(x, b - arm)],
        ]
        .iter()
        .map(|p| Contour::polyline(p, false))
        .collect()
    }

    /// A rounded rect outline, starting at the middle of the top edge and running clockwise,
    /// for a border that draws on from the top.
    pub fn panel(x: f32, y: f32, w: f32, h: f32, r: f32) -> Contour {
        use std::f64::consts::FRAC_PI_2;
        let (x0, y0, x1, y1) = (x as f64, y as f64, (x + w) as f64, (y + h) as f64);
        let r = (r.max(0.0) as f64).min(0.5 * (w.min(h)) as f64);
        let mut p = BezPath::new();
        p.move_to((0.5 * (x0 + x1), y0));
        let corners = [
            ((x1 - r, y0), (x1 - r, y0 + r), -FRAC_PI_2),
            ((x1, y1 - r), (x1 - r, y1 - r), 0.0),
            ((x0 + r, y1), (x0 + r, y1 - r), FRAC_PI_2),
            ((x0, y0 + r), (x0 + r, y0 + r), 2.0 * FRAC_PI_2),
        ];
        for (line_end, centre, start) in corners {
            p.line_to(line_end);
            if r > 0.0 {
                Arc::new(centre, (r, r), start, FRAC_PI_2, 0.0).to_cubic_beziers(0.01, |a, b, c| p.curve_to(a, b, c));
            }
        }
        p.close_path();
        Outline::new(&p, 0.02).contours.into_iter().next().unwrap_or_default()
    }
}

/// A glyph's outline as a path in layer units at `size`, pen origin at 0, y down. For glyph
/// morphs (TY21) and text drawn as strokes.
pub fn glyph_path(face: &crate::font::Face, glyph: u32, size: f32) -> BezPath {
    use skrifa::instance::{LocationRef, Size};
    use skrifa::outline::{DrawSettings, OutlinePen};
    use skrifa::MetadataProvider;
    struct Pen(BezPath);
    impl OutlinePen for Pen {
        fn move_to(&mut self, x: f32, y: f32) {
            self.0.move_to((x as f64, -y as f64));
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.0.line_to((x as f64, -y as f64));
        }
        fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
            self.0.quad_to((cx as f64, -cy as f64), (x as f64, -y as f64));
        }
        fn curve_to(&mut self, ax: f32, ay: f32, bx: f32, by: f32, x: f32, y: f32) {
            self.0.curve_to((ax as f64, -ay as f64), (bx as f64, -by as f64), (x as f64, -y as f64));
        }
        fn close(&mut self) {
            self.0.close_path();
        }
    }
    let font = face.skrifa();
    let mut pen = Pen(BezPath::new());
    if let Some(g) = font.outline_glyphs().get(skrifa::GlyphId::new(glyph)) {
        let _ = g.draw(DrawSettings::unhinted(Size::new(size), LocationRef::default()), &mut pen);
    }
    pen.0
}

/// A small line icon set on a 24 x 24 grid (y down), drawn to be stroked about 2 units wide.
/// Fill one with [`icons::outline`], which strokes it to a closed outline for the glyph path.
pub mod icons {
    pub use super::make::brackets;
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

    /// An X, two strokes.
    pub fn x() -> BezPath {
        let mut p = BezPath::new();
        p.move_to((6.0, 6.0));
        p.line_to((18.0, 18.0));
        p.move_to((18.0, 6.0));
        p.line_to((6.0, 18.0));
        p
    }

    /// A timer: a dial, a hand and the button on top.
    pub fn timer() -> BezPath {
        let mut p = kurbo::Circle::new((12.0, 13.5), 8.0).to_path(0.01);
        p.move_to((12.0, 13.5));
        p.line_to((12.0, 9.0));
        p.move_to((10.0, 2.5));
        p.line_to((14.0, 2.5));
        p
    }

    /// A chip: a rounded square with three pins on each side.
    pub fn chip() -> BezPath {
        let mut p = RoundedRect::new(6.0, 6.0, 18.0, 18.0, 2.0).to_path(0.01);
        for k in [9.0, 12.0, 15.0] {
            for (a, b) in [((k, 2.5), (k, 6.0)), ((k, 18.0), (k, 21.5)), ((2.5, k), (6.0, k)), ((18.0, k), (21.5, k))] {
                p.move_to(a);
                p.line_to(b);
            }
        }
        p
    }

    /// A gear with eight teeth and a hub.
    pub fn gear() -> BezPath {
        let mut p = BezPath::new();
        let (c, teeth) = (kurbo::Point::new(12.0, 12.0), 8);
        for i in 0..teeth * 4 {
            let a = std::f64::consts::TAU * (i as f64 - 0.5) / (teeth * 4) as f64;
            let r = if (i / 2) % 2 == 0 { 9.5 } else { 7.0 };
            let q = c + kurbo::Vec2::from_angle(a) * r;
            if i == 0 {
                p.move_to(q);
            } else {
                p.line_to(q);
            }
        }
        p.close_path();
        p.extend(kurbo::Circle::new(c, 3.0).to_path(0.01));
        p
    }

    /// A stop square with round corners.
    pub fn stop() -> BezPath {
        RoundedRect::new(6.0, 6.0, 18.0, 18.0, 2.5).to_path(0.01)
    }

    /// A keyboard: a rounded body, two rows of keys as dots and a space bar.
    pub fn keyboard() -> BezPath {
        let mut p = RoundedRect::new(2.5, 6.0, 21.5, 18.0, 2.0).to_path(0.01);
        for (y, xs) in [(9.5, [6.0, 9.5, 13.0, 16.5, 20.0]), (12.5, [7.5, 11.0, 14.5, 18.0, f64::NAN])] {
            for x in xs.into_iter().filter(|x| x.is_finite()) {
                // A key is a zero-length stroke: a round dot once stroked.
                p.move_to((x - 0.01, y));
                p.line_to((x + 0.01, y));
            }
        }
        p.move_to((8.0, 15.0));
        p.line_to((16.0, 15.0));
        p
    }

    /// A chevron pointing right. Turn it with [`rotated`] for the other directions.
    pub fn chevron() -> BezPath {
        let mut p = BezPath::new();
        p.move_to((9.0, 5.5));
        p.line_to((15.5, 12.0));
        p.line_to((9.0, 18.5));
        p
    }

    /// A clock: a dial and two hands at ten past ten.
    pub fn clock() -> BezPath {
        let mut p = kurbo::Circle::new((12.0, 12.0), 9.0).to_path(0.01);
        p.move_to((12.0, 7.0));
        p.line_to((12.0, 12.0));
        p.line_to((15.5, 14.0));
        p
    }

    /// A bar glyph: five vertical bars of different heights round the middle line.
    pub fn bars() -> BezPath {
        let mut p = BezPath::new();
        for (x, h) in [(4.0, 3.0), (8.0, 7.0), (12.0, 10.0), (16.0, 6.0), (20.0, 2.5)] {
            p.move_to((x, 12.0 - h));
            p.line_to((x, 12.0 + h));
        }
        p
    }

    /// A lightning bolt, one closed contour.
    pub fn lightning() -> BezPath {
        let mut p = BezPath::new();
        p.move_to((13.5, 2.5));
        p.line_to((5.0, 13.5));
        p.line_to((11.5, 13.5));
        p.line_to((10.5, 21.5));
        p.line_to((19.0, 10.5));
        p.line_to((12.5, 10.5));
        p.close_path();
        p
    }

    /// An inbox tray: a box with a dip in the top edge of its lower part.
    pub fn tray() -> BezPath {
        let mut p = BezPath::new();
        p.move_to((3.0, 13.0));
        p.line_to((8.0, 13.0));
        p.line_to((9.5, 16.0));
        p.line_to((14.5, 16.0));
        p.line_to((16.0, 13.0));
        p.line_to((21.0, 13.0));
        p.extend(RoundedRect::new(3.0, 5.0, 21.0, 19.5, 2.0).to_path(0.01));
        p
    }

    /// A comma, for a punctuation receipt: a dot with a tail.
    pub fn comma() -> BezPath {
        let mut p = BezPath::new();
        p.move_to((12.0, 13.0));
        p.line_to((12.02, 13.0));
        p.move_to((12.5, 13.5));
        p.quad_to((12.8, 17.0), (10.0, 19.5));
        p
    }

    /// Swap: two arrows, right along the top and left along the bottom.
    pub fn swap() -> BezPath {
        let mut p = BezPath::new();
        p.move_to((4.0, 8.0));
        p.line_to((19.0, 8.0));
        p.move_to((15.5, 4.5));
        p.line_to((19.0, 8.0));
        p.line_to((15.5, 11.5));
        p.move_to((20.0, 16.0));
        p.line_to((5.0, 16.0));
        p.move_to((8.5, 12.5));
        p.line_to((5.0, 16.0));
        p.line_to((8.5, 19.5));
        p
    }

    /// Letter case ("Aa"): a capital A and a small a, for a casing receipt.
    pub fn letter_case() -> BezPath {
        let mut p = BezPath::new();
        p.move_to((2.5, 19.0));
        p.line_to((7.5, 5.0));
        p.line_to((12.5, 19.0));
        p.move_to((4.3, 14.0));
        p.line_to((10.7, 14.0));
        p.extend(kurbo::Ellipse::new((17.5, 15.5), (3.2, 3.5), 0.0).to_path(0.01));
        p.move_to((20.7, 12.0));
        p.line_to((20.7, 19.0));
        p
    }

    /// `path` turned by `turns` (clockwise) about the middle of the 24 x 24 grid.
    pub fn rotated(path: &BezPath, turns: f64) -> BezPath {
        let m = kurbo::Affine::rotate_about(turns * std::f64::consts::TAU, kurbo::Point::new(12.0, 12.0));
        m * path.clone()
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
    VectorSeg { p0: p(q.p0), p1: p(q.p1), p2: p(q.p2), s0, s1, w: [1.0; 2] }
}

fn line(a: Point, b: Point, s0: f32, s1: f32) -> VectorSeg {
    VectorSeg { p0: a, p1: Point::new(0.5 * (a.x + b.x), 0.5 * (a.y + b.y)), p2: b, s0, s1, w: [1.0; 2] }
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
    fn dashes_cover_their_share() {
        let c = &square().contours[0];
        let dashes = c.dash(&[3.0, 2.0], 0.0);
        assert_eq!(dashes.len(), 8);
        let inked: f32 = dashes.iter().map(|d| d.length() - d.segs[0].s0).sum();
        assert!((inked - 24.0).abs() < 1e-3, "{inked}");
    }

    #[test]
    fn panel_starts_at_the_top_middle_and_closes() {
        let c = make::panel(0.0, 0.0, 100.0, 40.0, 10.0);
        let (p, _) = c.at(0.0).unwrap();
        assert!((p.x - 50.0).abs() < 1e-3 && p.y.abs() < 1e-3);
        let perimeter = 2.0 * (80.0 + 20.0) + std::f32::consts::TAU * 10.0;
        assert!((c.length() - perimeter).abs() < 0.05, "{}", c.length());
    }

    #[test]
    fn spinner_stays_between_20_and_270_degrees() {
        for i in 0..200 {
            let c = make::spinner(Point::ZERO, 10.0, i as f64 * 0.013);
            let deg = c.length() / 10.0 * 180.0 / std::f32::consts::PI;
            assert!((19.0..=271.0).contains(&deg), "{deg}");
        }
    }

    #[test]
    fn ribbon_widths_follow_points() {
        let pts = [Point::new(0.0, 0.0), Point::new(10.0, 0.0), Point::new(20.0, 0.0)];
        let r = make::ribbon(&pts, &[0.0, 1.0, 0.5]);
        assert_eq!(r.segs[0].w, [0.0, 1.0]);
        assert_eq!(r.segs[1].w, [1.0, 0.5]);
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
