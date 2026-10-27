//! Cut panel frames for angular HUD looks ([`Element::frame`]): chamfered, rounded or
//! notched corners, notches cut into the edges, cut-outs, a solid or striped fill, a stroke
//! with an optional glow, and a hatched band inside the edge.
//!
//! The outline is a polygon built from the box size every frame, so a frame whose box
//! springs reshapes with it. Fills go through the vector fill (nonzero winding); a cut-out is
//! a hole wound the other way, clipped to the frame's convex outline first, so cut-outs may
//! cross the edge but must not overlap each other. Stripes and hatching are a paint on the
//! fill (`VectorPaint::Stripes`), not geometry, so they cost nothing per stripe.
//!
//! The node's trim (`.trim`, `Prop::Trim`) draws the outline stroke on.

use std::f32::consts::FRAC_PI_2;

use crate::display::{DisplayList, Item, ItemKind};
use crate::element::{Element, Key, Kind, Slot};
use crate::geom::{Color, Point, Size};
use crate::path::{push_fill, push_stroke, Contour, StrokeStyle, VectorPaint};
use crate::scene::Node;

/// A corner of a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Corner {
    #[default]
    Square,
    /// Cut at 45 degrees, `size` along each edge.
    Chamfer(f32),
    /// Rounded with radius `size`.
    Round(f32),
    /// A square bite `size` deep out of the corner.
    Notch(f32),
}

/// An edge of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Right,
    Bottom,
    Left,
}

/// A notch cut into an edge: centred at `at` (0 to 1 along the edge, clockwise), `width`
/// wide at its floor, `depth` deep (negative makes a tab that sticks out), and `slant` wider
/// on each side at the edge, for a trapezoid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Notch {
    pub edge: Edge,
    pub at: f32,
    pub width: f32,
    pub depth: f32,
    pub slant: f32,
}

impl Notch {
    pub fn new(edge: Edge, at: f32, width: f32, depth: f32) -> Notch {
        Notch { edge, at, width, depth, slant: 0.0 }
    }

    /// Sides leaning out by `slant` at the edge.
    pub fn slant(self, slant: f32) -> Notch {
        Notch { slant, ..self }
    }
}

/// Parallel stripes: lines running at `angle` (radians, clockwise on screen, 0 is
/// horizontal), one every `period`, each `duty` of the period wide, in `color`. They march
/// across at `speed` logical units per second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stripes {
    pub angle: f32,
    pub period: f32,
    pub duty: f32,
    pub color: Color,
    pub speed: f32,
}

impl Stripes {
    pub fn new(angle: f32, period: f32, duty: f32, color: Color) -> Stripes {
        Stripes { angle, period, duty: duty.clamp(0.01, 0.99), color, speed: 0.0 }
    }

    /// Thin lines: hatching `width` wide every `period`.
    pub fn hatch(angle: f32, period: f32, width: f32, color: Color) -> Stripes {
        Stripes::new(angle, period, width / period.max(1e-3), color)
    }

    /// Marching at `speed` logical units per second.
    pub fn speed(self, speed: f32) -> Stripes {
        Stripes { speed, ..self }
    }

    fn paint(&self, t: f64) -> VectorPaint {
        let (s, c) = self.angle.sin_cos();
        let period = self.period.max(0.5);
        let phase = ((self.speed as f64 * t) / period as f64).rem_euclid(1.0) as f32;
        VectorPaint::Stripes { normal: Point::new(-s, c), period, duty: self.duty, phase }
    }

    fn style(&self, t: f64, alpha: f32) -> StrokeStyle {
        let c = self.color.fade(alpha);
        StrokeStyle { paint: self.paint(t), ..StrokeStyle::solid(0.0, c).ramp(0.0, c.fade(0.0), 1.0, c) }
    }
}

/// What [`Element::frame`] draws (`Kind::Frame`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    /// Top left, top right, bottom right, bottom left.
    pub corners: [Corner; 4],
    pub notches: Vec<Notch>,
    /// Holes, as polygons in the box's space.
    pub cutouts: Vec<Vec<Point>>,
    pub fill: Option<Color>,
    /// Stripes over the fill, inside the outline.
    pub stripes: Option<Stripes>,
    /// Stroke width and colour, centred on the outline and the cut-outs.
    pub stroke: Option<(f32, Color)>,
    /// Glow radius of the stroke, or of the fill when there is no stroke.
    pub glow: f32,
    /// A band this wide inside the outline, filled with these stripes.
    pub hatch: Option<(f32, Stripes)>,
}

impl Frame {
    /// A frame with square corners and nothing drawn yet.
    pub fn new() -> Frame {
        Frame::default()
    }

    /// All four corners cut at 45 degrees.
    pub fn chamfer(size: f32) -> Frame {
        Frame { corners: [Corner::Chamfer(size); 4], ..Frame::default() }
    }

    pub fn corners(mut self, corners: [Corner; 4]) -> Frame {
        self.corners = corners;
        self
    }

    pub fn notch(mut self, notch: Notch) -> Frame {
        self.notches.push(notch);
        self
    }

    /// A hole of these points (box space).
    pub fn cut(mut self, points: Vec<Point>) -> Frame {
        self.cutouts.push(points);
        self
    }

    /// A rectangular hole, its corners cut by `chamfer`.
    pub fn cut_rect(self, x: f32, y: f32, w: f32, h: f32, chamfer: f32) -> Frame {
        let pts = polygon(Size::new(w, h), &[Corner::Chamfer(chamfer); 4], &[], 0.0);
        self.cut(pts.into_iter().map(|p| Point::new(p.x + x, p.y + y)).collect())
    }

    pub fn fill(mut self, color: Color) -> Frame {
        self.fill = Some(color);
        self
    }

    pub fn stripes(mut self, stripes: Stripes) -> Frame {
        self.stripes = Some(stripes);
        self
    }

    pub fn stroke(mut self, width: f32, color: Color) -> Frame {
        self.stroke = Some((width, color));
        self
    }

    pub fn glow(mut self, radius: f32) -> Frame {
        self.glow = radius;
        self
    }

    /// A band `width` wide inside the outline, filled with `stripes`.
    pub fn hatch(mut self, width: f32, stripes: Stripes) -> Frame {
        self.hatch = Some((width, stripes));
        self
    }

    /// The outline in a box of `size`, clockwise on screen from the top left corner.
    pub fn outline(&self, size: Size) -> Vec<Point> {
        polygon(size, &self.corners, &self.notches, 0.0)
    }

    /// The outline moved `d` inward.
    pub fn inset(&self, size: Size, d: f32) -> Vec<Point> {
        polygon(size, &self.corners, &self.notches, d)
    }

    /// The cut-outs in a box of `size`, clipped to the convex outline (no notches) and wound
    /// against the outline.
    pub fn holes(&self, size: Size) -> Vec<Vec<Point>> {
        let base: [Corner; 4] = self.corners.map(|c| if let Corner::Notch(_) = c { Corner::Square } else { c });
        let base = polygon(size, &base, &[], 0.0);
        self.cutouts
            .iter()
            .map(|c| {
                let mut h = clip_convex(c, &base);
                if area(&h) > 0.0 {
                    h.reverse();
                }
                h
            })
            .filter(|h| h.len() >= 3)
            .collect()
    }

    /// Whether the stripes march.
    pub(crate) fn moving(&self) -> bool {
        self.stripes.is_some_and(|s| s.speed != 0.0) || self.hatch.is_some_and(|(_, s)| s.speed != 0.0)
    }
}

impl<M> Element<M> {
    /// A cut panel frame filling the box: see [`Frame`].
    pub fn frame(key: impl Into<Key>, frame: Frame) -> Element<M> {
        Element::new(key, Kind::Frame(frame), taffy::Style::default())
    }
}

/// The frame polygon in a box of `size`, moved `d` inward.
fn polygon(size: Size, corners: &[Corner; 4], notches: &[Notch], d: f32) -> Vec<Point> {
    let (w, h) = (size.w - 2.0 * d, size.h - 2.0 * d);
    if w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let max = 0.5 * w.min(h);
    // The corner points clockwise, the direction of the edge after each, and its length.
    let at = [Point::new(d, d), Point::new(d + w, d), Point::new(d + w, d + h), Point::new(d, d + h)];
    let dirs = [Point::new(1.0, 0.0), Point::new(0.0, 1.0), Point::new(-1.0, 0.0), Point::new(0.0, -1.0)];
    let lens = [w, h, w, h];
    let edges = [Edge::Top, Edge::Right, Edge::Bottom, Edge::Left];
    let mut pts = Vec::new();
    for i in 0..4 {
        let p = at[i];
        let (din, dout) = (dirs[(i + 3) % 4], dirs[i]);
        let off = |a: Point, u: Point, k: f32| Point::new(a.x + u.x * k, a.y + u.y * k);
        match corners[i] {
            Corner::Square => pts.push(p),
            Corner::Chamfer(c) => {
                // Moving the 45 degree cut in by d shortens it along each edge by d (2 - sqrt 2).
                let c = (c - d * (2.0 - std::f32::consts::SQRT_2)).clamp(0.0, max);
                pts.push(off(p, din, -c));
                pts.push(off(p, dout, c));
            }
            Corner::Round(r) => {
                let r = (r - d).clamp(0.0, max);
                let centre = off(off(p, din, -r), dout, r);
                let steps = 8;
                for k in 0..=steps {
                    let a = FRAC_PI_2 * k as f32 / steps as f32;
                    pts.push(off(off(centre, dout, -r * a.cos()), din, r * a.sin()));
                }
            }
            Corner::Notch(c) => {
                let c = c.clamp(0.0, max);
                pts.push(off(p, din, -c));
                pts.push(off(off(p, din, -c), dout, c));
                pts.push(off(p, dout, c));
            }
        }
        // Notches along the edge after this corner, in order.
        let mut on: Vec<&Notch> = notches.iter().filter(|n| n.edge == edges[i]).collect();
        on.sort_by(|a, b| a.at.total_cmp(&b.at));
        let inward = Point::new(-dout.y, dout.x);
        let full = lens[i] + 2.0 * d;
        for n in on {
            // Positions along the edge of the full box, moved in with it.
            let x = n.at * full - d;
            let half = 0.5 * n.width + d * n.depth.signum();
            let mouth = half + n.slant;
            let floor = off(p, inward, n.depth);
            pts.push(off(p, dout, x - mouth));
            pts.push(off(floor, dout, x - half));
            pts.push(off(floor, dout, x + half));
            pts.push(off(p, dout, x + mouth));
        }
    }
    pts
}

/// Twice the signed area: positive when clockwise on screen (y down).
fn area(pts: &[Point]) -> f32 {
    let n = pts.len();
    (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum()
}

/// `subject` clipped to the convex polygon `clip` (Sutherland and Hodgman).
fn clip_convex(subject: &[Point], clip: &[Point]) -> Vec<Point> {
    let sign = area(clip).signum();
    let mut out = subject.to_vec();
    let n = clip.len();
    for i in 0..n {
        let (a, b) = (clip[i], clip[(i + 1) % n]);
        let side = |p: Point| sign * ((b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x));
        let input = std::mem::take(&mut out);
        for j in 0..input.len() {
            let (p, q) = (input[j], input[(j + 1) % input.len()]);
            let (sp, sq) = (side(p), side(q));
            if sp >= 0.0 {
                out.push(p);
            }
            if (sp >= 0.0) != (sq >= 0.0) {
                let k = sp / (sp - sq);
                out.push(Point::new(p.x + (q.x - p.x) * k, p.y + (q.y - p.y) * k));
            }
        }
        if out.is_empty() {
            break;
        }
    }
    out
}

/// `top` laid over `bottom`, blended in linear light like the renderer.
fn over(top: Color, bottom: Color) -> Color {
    let lin = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    let srgb = |c: f32| if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    let a = top.a + bottom.a * (1.0 - top.a);
    if a <= 0.0 {
        return Color::TRANSPARENT;
    }
    let mix = |t: f32, b: f32| srgb((lin(t) * top.a + lin(b) * bottom.a * (1.0 - top.a)) / a);
    Color::rgba(mix(top.r, bottom.r), mix(top.g, bottom.g), mix(top.b, bottom.b), a)
}

/// A closed contour through `pts`, moved by `origin`.
fn closed(pts: &[Point], origin: Point) -> Contour {
    let mut p: Vec<Point> = pts.iter().map(|p| Point::new(p.x + origin.x, p.y + origin.y)).collect();
    if let Some(&first) = p.first() {
        p.push(first);
    }
    Contour::polyline(&p, true)
}

/// The frame at `t`, at `origin` in the current layer.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw<M>(
    f: &Frame,
    n: &Node<M>,
    t: f64,
    origin: Point,
    size: Size,
    alpha: f32,
    items: &mut Vec<Item>,
    out: &mut DisplayList,
) {
    let outline = f.outline(size);
    if outline.len() < 3 {
        return;
    }
    let shape = closed(&outline, origin);
    let holes: Vec<Contour> = f.holes(size).iter().map(|h| closed(h, origin)).collect();
    let mut push = |item: Option<Item>| {
        if let Some(item) = item {
            match items.last_mut() {
                Some(last) if last.kind == ItemKind::Vectors && last.start + last.len == item.start => last.len += 1,
                _ => items.push(item),
            }
        }
    };
    let glow_fill = if f.stroke.is_none() { f.glow } else { 0.0 };
    let filled = || std::iter::once(&shape).chain(&holes);
    match (f.fill, f.stripes) {
        // Stripes over a fill are one pass: the fill colour between the stripes and the
        // stripe colour laid over it on them.
        (Some(c), Some(s)) => {
            let (c0, c1) = (c.fade(alpha), over(s.color, c).fade(alpha));
            let style = StrokeStyle { paint: s.paint(t), ..StrokeStyle::solid(0.0, c0).ramp(0.0, c0, 1.0, c1) };
            push(push_fill(out, filled(), &style.glow(glow_fill)));
        }
        (Some(c), None) => push(push_fill(out, filled(), &StrokeStyle::solid(0.0, c.fade(alpha)).glow(glow_fill))),
        (None, Some(s)) => push(push_fill(out, filled(), &s.style(t, alpha))),
        (None, None) => {}
    }
    if let Some((width, s)) = f.hatch {
        // One quad per edge between the outline and its inset, so each covers only its own
        // strip of pixels instead of the whole box.
        let inner = f.inset(size, width);
        if inner.len() == outline.len() {
            let style = s.style(t, alpha);
            let k = outline.len();
            for i in 0..k {
                let j = (i + 1) % k;
                let quad = [outline[i], outline[j], inner[j], inner[i]];
                push(push_fill(out, &[closed(&quad, origin)], &style));
            }
        }
    }
    if let Some((width, color)) = f.stroke {
        let style = StrokeStyle::solid(width, color.fade(alpha)).glow(f.glow);
        let start = n.value(Slot::TrimStart as u8, t).clamp(0.0, 1.0) as f32;
        let end = if n.has(Slot::TrimEnd as u8) { n.value(Slot::TrimEnd as u8, t).clamp(0.0, 1.0) as f32 } else { 1.0 };
        let len = shape.length();
        let drawn = if start <= 0.0 && end >= 1.0 { shape.clone() } else { shape.trim(start * len, end * len) };
        push(push_stroke(out, &drawn, &style));
        if start <= 0.0 && end >= 1.0 {
            for h in &holes {
                push(push_stroke(out, h, &style));
            }
        }
    }
}

/// How far a frame draws outside its box.
pub(crate) fn reach(f: &Frame) -> f32 {
    let tabs = f.notches.iter().map(|n| (-n.depth).max(0.0)).fold(0.0, f32::max);
    tabs + crate::reach::stroke_glow_reach(f.stroke.map_or(0.0, |s| s.0), f.glow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Scene;

    #[test]
    fn a_chamfered_box_has_eight_points_clockwise() {
        let f = Frame::chamfer(10.0);
        let p = f.outline(Size::new(100.0, 50.0));
        assert_eq!(p.len(), 8);
        assert_eq!((p[0], p[1]), (Point::new(0.0, 10.0), Point::new(10.0, 0.0)));
        assert!(area(&p) > 0.0);
        // Area of the box less four corner triangles of 50 each.
        assert!((0.5 * area(&p) - (5000.0 - 200.0)).abs() < 1e-2);
    }

    #[test]
    fn notches_cut_into_the_edge_and_tabs_stick_out() {
        let f =
            Frame::new().notch(Notch::new(Edge::Top, 0.5, 20.0, 6.0)).notch(Notch::new(Edge::Bottom, 0.5, 20.0, -6.0));
        let p = f.outline(Size::new(100.0, 50.0));
        assert_eq!(p.len(), 12);
        assert_eq!(p[2], Point::new(40.0, 6.0));
        assert_eq!(p[3], Point::new(60.0, 6.0));
        // The bottom edge runs right to left: the tab's floor sits below the box.
        assert!(p.iter().any(|q| (q.y - 56.0).abs() < 1e-4));
        // Area: 5000 less the notch, plus the tab.
        assert!((0.5 * area(&p) - 5000.0).abs() < 1e-2);
        assert_eq!(reach(&f), 6.0);
    }

    #[test]
    fn a_cut_out_across_the_edge_is_clipped_and_wound_the_other_way() {
        let f = Frame::new().cut_rect(-10.0, 10.0, 30.0, 20.0, 0.0);
        let holes = f.holes(Size::new(100.0, 50.0));
        assert_eq!(holes.len(), 1);
        assert!(area(&holes[0]) < 0.0);
        // Only the part inside the box: 20 x 20.
        assert!((0.5 * area(&holes[0]).abs() - 400.0).abs() < 1e-2);
        assert!(holes[0].iter().all(|p| p.x >= -1e-4));
    }

    #[test]
    fn the_inset_outline_runs_parallel() {
        let f = Frame::chamfer(12.0);
        let size = Size::new(100.0, 60.0);
        let inner = f.inset(size, 4.0);
        // The top edge moves down by 4 and its chamfer ends 4 (2 - sqrt 2) sooner.
        let c = 12.0 - 4.0 * (2.0 - std::f32::consts::SQRT_2);
        assert!((inner[1].y - 4.0).abs() < 1e-4 && (inner[1].x - (4.0 + c)).abs() < 1e-4, "{:?}", inner[1]);
        // The distance between the outer and inner chamfer lines is 4.
        let n = Point::new(std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2);
        let line = |p: Point| p.x * n.x + p.y * n.y;
        assert!((line(inner[1]) - line(f.outline(size)[1]) - 4.0).abs() < 1e-3);
    }

    #[test]
    fn a_frame_draws_fill_stripes_hatch_and_stroke() {
        let f = Frame::chamfer(8.0)
            .fill(Color::hex(0x101820, 1.0))
            .stripes(Stripes::new(-1.0, 8.0, 0.4, Color::WHITE).speed(20.0))
            .hatch(6.0, Stripes::hatch(1.0, 4.0, 1.0, Color::WHITE))
            .stroke(2.0, Color::WHITE)
            .cut_rect(20.0, 20.0, 10.0, 10.0, 0.0);
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(120.0, 60.0), 1.0);
        scene.apply(Element::frame("f", f).size(120.0, 60.0), 0.0);
        let list = scene.sample(0.0);
        // Fill with stripes, the hatch band as one quad per edge of the eight, the outline
        // and one hole outline.
        assert_eq!(list.vectors.len(), 1 + 8 + 2);
        assert!(matches!(list.vectors[0].paint, VectorPaint::Stripes { .. }));
        // Marching stripes keep the scene awake.
        assert_eq!(scene.next_wake(5.0), Some(5.0));
    }
}
