//! Glyphs on curves: text along a path (FX-032), a glyph flying along an S-curve with a
//! leader line (FX-039), and a curved arrow annotation (FX-040).
//!
//! All three draw from a canvas element (`Element::canvas`), pure in the frame time. Rotated
//! glyphs go through `Painter::layer`, one direct child layer per glyph, so they keep the
//! Slug glyph quality at any angle. Positions are in the canvas's layer space, y down.
//!
//! - **Text on a path.** `place_on_path` puts the glyphs of one shaped line along a contour
//!   by arc length: each glyph's middle sits on the curve and the glyph turns to the tangent
//!   there. Relay it each frame from the latest curve (a pitch trace that updates) and the
//!   glyphs ride the new shape.
//! - **Flight.** A glyph leaves `from`, follows an S-shaped cubic and lands at `to`, with an
//!   ease in and out. A leader line draws on ahead of it, then retracts into the landing
//!   spot once it is there.
//! - **Arrow.** A hooked arc from `from` to `to` that draws on, then grows its head.

use kurbo::BezPath;

use crate::canvas::Painter;
use crate::display::{GlyphInstance, NO_WORD};
use crate::geom::{Affine, Color, Point};
use crate::path::{Contour, Outline, StrokeStyle};
use crate::text::{Align, ShapedText};

/// Flatness of curves built here, in layer units.
const TOLERANCE: f64 = 0.05;

/// One glyph placed on a path: its pen position, the angle it turns by (radians, clockwise
/// on screen) about the pen, and the shaped glyph's id, face and cluster.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OnPath {
    pub glyph: u32,
    pub font: u32,
    pub cluster: u32,
    pub pen: Point,
    pub angle: f32,
}

/// Where the text starts along a path of `length` for a text `width` wide, by `align`.
pub fn path_start(align: Align, length: f32, width: f32) -> f32 {
    align.factor() * (length - width)
}

/// The glyphs of `shaped`, laid out as one line (no width), along `path`, starting `start` units along it.
/// Each glyph's middle goes to the point at its arc length and the glyph turns to the
/// tangent there. Its shaping offset from the baseline goes along the normal. Glyphs past
/// either end follow the end's tangent.
pub fn place_on_path(shaped: &ShapedText, path: &Contour, start: f32) -> Vec<OnPath> {
    let Some(first) = shaped.glyphs.first() else { return Vec::new() };
    let baseline = first.pos.y;
    let length = path.length();
    let mut out = Vec::with_capacity(shaped.glyphs.len());
    for g in &shaped.glyphs {
        let half = 0.5 * g.advance;
        let s = start + g.pos.x + half;
        let Some((p, tan)) = path.at(s.clamp(0.0, length)) else { continue };
        // Past an end, run on along the end's tangent.
        let over = if s < 0.0 { s } else { (s - length).max(0.0) };
        let mid = Point::new(p.x + tan.x * over, p.y + tan.y * over);
        let normal = Point::new(-tan.y, tan.x);
        let dy = g.pos.y - baseline;
        let pen = Point::new(mid.x - tan.x * half + normal.x * dy, mid.y - tan.y * half + normal.y * dy);
        out.push(OnPath { glyph: g.glyph, font: g.font, cluster: g.cluster, pen, angle: tan.y.atan2(tan.x) });
    }
    out
}

/// Draws glyphs placed by `place_on_path` at em size `size`.
pub fn draw_on_path(p: &mut Painter, glyphs: &[OnPath], size: f32, color: Color) {
    for g in glyphs {
        draw_turned(p, g.glyph, g.font, g.pen, g.angle, size, 1.0, color);
    }
}

/// One glyph with its pen at `pen`, turned by `angle` and scaled by `scale` about the pen.
#[allow(clippy::too_many_arguments)]
pub fn draw_turned(
    p: &mut Painter,
    glyph: u32,
    font: u32,
    pen: Point,
    angle: f32,
    size: f32,
    scale: f32,
    color: Color,
) {
    if color.a <= 0.0 || scale <= 0.0 {
        return;
    }
    let instance = GlyphInstance {
        pos: Point::ZERO,
        size: size * scale,
        font,
        glyph,
        word: NO_WORD,
        color,
        xform: crate::display::GlyphXform::IDENTITY,
    };
    if angle.abs() < 1e-4 {
        p.glyph(GlyphInstance { pos: pen, ..instance });
        return;
    }
    let m = Affine::rotate(angle).then(&Affine::translate(pen.x, pen.y));
    p.layer(m, 1.0, |p| p.glyph(instance));
}

/// Ease in and out (cubic), 0 to 1.
fn ease(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    if u < 0.5 {
        4.0 * u * u * u
    } else {
        1.0 - (-2.0 * u + 2.0).powi(3) / 2.0
    }
}

/// A glyph's flight from a rail into its slot along an S-shaped cubic (FX-039).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flight {
    pub from: Point,
    pub to: Point,
    /// When it leaves, and how long the flight takes, in seconds.
    pub start: f64,
    pub duration: f64,
    /// How far the S bows out each way, as a share of the distance.
    pub bend: f32,
    /// How long the leader takes to retract after landing.
    pub retract: f64,
}

impl Flight {
    pub fn new(from: Point, to: Point, start: f64, duration: f64) -> Flight {
        Flight { from, to, start, duration, bend: 0.35, retract: 0.25 }
    }

    /// The S-curve: the first control point bows to one side of the straight line and the
    /// second to the other.
    pub fn path(&self) -> Contour {
        let (a, b) = (self.from, self.to);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        // A perpendicular as long as the distance.
        let n = Point::new(-dy, dx);
        let k = self.bend;
        let c1 = Point::new(a.x + dx / 3.0 + n.x * k, a.y + dy / 3.0 + n.y * k);
        let c2 = Point::new(a.x + 2.0 * dx / 3.0 - n.x * k, a.y + 2.0 * dy / 3.0 - n.y * k);
        let mut bez = BezPath::new();
        let pt = |p: Point| kurbo::Point::new(p.x as f64, p.y as f64);
        bez.move_to(pt(a));
        bez.curve_to(pt(c1), pt(c2), pt(b));
        Outline::new(&bez, TOLERANCE).contours.into_iter().next().unwrap_or_default()
    }

    /// Share of the way along the curve at `t`, eased.
    pub fn progress(&self, t: f64) -> f32 {
        if self.duration <= 0.0 {
            return if t >= self.start { 1.0 } else { 0.0 };
        }
        ease(((t - self.start) / self.duration) as f32)
    }

    /// Where the glyph is at `t` and the angle of its path there. Before the start it waits
    /// at `from`; after landing it rests at `to`, upright.
    pub fn at(&self, t: f64) -> (Point, f32) {
        let u = self.progress(t);
        if u >= 1.0 {
            return (self.to, 0.0);
        }
        if u <= 0.0 {
            return (self.from, 0.0);
        }
        let path = self.path();
        let (p, tan) = path.at(u * path.length()).unwrap_or((self.from, Point::new(1.0, 0.0)));
        // The glyph turns with the path in flight and comes upright as it lands.
        let upright = 1.0 - ((u - 0.8) / 0.2).clamp(0.0, 1.0);
        (p, upright * tan.y.atan2(tan.x))
    }

    /// The leader at `t` as arc lengths `(from, to)` along `path()`: it draws on just ahead
    /// of the glyph and, after landing, retracts from the rail end into the slot. `None`
    /// before the start and once it is gone.
    pub fn leader_span(&self, t: f64) -> Option<(f32, f32)> {
        let len = self.path().length();
        if t < self.start || len <= 0.0 {
            return None;
        }
        let land = self.start + self.duration;
        if t < land {
            // Leads the glyph by an eighth of the way.
            let head = (self.progress(t) + 0.125).min(1.0);
            return Some((0.0, head * len));
        }
        let r = if self.retract > 0.0 { ease(((t - land) / self.retract) as f32) } else { 1.0 };
        (r < 1.0).then_some((r * len, len))
    }

    /// When the flight and its leader are over.
    pub fn done_at(&self) -> f64 {
        self.start + self.duration + self.retract
    }

    /// Draws the leader and the glyph at `t`: the glyph's pen goes along the curve, with its
    /// middle on it.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        p: &mut Painter,
        t: f64,
        glyph: u32,
        font: u32,
        advance: f32,
        size: f32,
        color: Color,
        leader: &StrokeStyle,
    ) {
        if let Some((a, b)) = self.leader_span(t) {
            p.stroke(&self.path().trim(a, b), leader);
        }
        let (mid, angle) = self.at(t);
        let (s, c) = angle.sin_cos();
        let half = 0.5 * advance;
        draw_turned(p, glyph, font, Point::new(mid.x - c * half, mid.y - s * half), angle, size, 1.0, color);
    }
}

/// A curved arrow from `from` to `to` that hooks over to one side (FX-040), drawn on by
/// `progress`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arrow {
    pub from: Point,
    pub to: Point,
    /// How far the arc bows out, as a share of the distance; negative bows the other way.
    pub bend: f32,
    /// Arrowhead arm length in layer units.
    pub head: f32,
}

impl Arrow {
    pub fn new(from: Point, to: Point) -> Arrow {
        Arrow { from, to, bend: 0.45, head: 6.0 }
    }

    /// The whole arc.
    pub fn path(&self) -> Contour {
        let (a, b) = (self.from, self.to);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let n = Point::new(dy, -dx);
        let k = self.bend;
        let c1 = Point::new(a.x + 0.15 * dx + n.x * k, a.y + 0.15 * dy + n.y * k);
        let c2 = Point::new(a.x + 0.85 * dx + n.x * k, a.y + 0.85 * dy + n.y * k);
        let mut bez = BezPath::new();
        let pt = |p: Point| kurbo::Point::new(p.x as f64, p.y as f64);
        bez.move_to(pt(a));
        bez.curve_to(pt(c1), pt(c2), pt(b));
        Outline::new(&bez, TOLERANCE).contours.into_iter().next().unwrap_or_default()
    }

    /// The contours at `progress` (0 to 1): the arc drawn on from the tail, and from 70% on,
    /// the head's two arms growing at the tip, which follows the drawn end.
    pub fn contours(&self, progress: f32) -> Vec<Contour> {
        let u = progress.clamp(0.0, 1.0);
        let path = self.path();
        let len = path.length();
        if u <= 0.0 || len <= 0.0 {
            return Vec::new();
        }
        let end = u * len;
        let mut out = vec![path.trim(0.0, end)];
        let grow = ((u - 0.7) / 0.3).clamp(0.0, 1.0);
        if grow > 0.0 {
            let (tip, tan) = path.at(end).unwrap_or((self.to, Point::new(1.0, 0.0)));
            let arm = self.head * grow;
            for side in [-1.0f32, 1.0] {
                // Arms at 28 degrees either side of straight back.
                let a = tan.y.atan2(tan.x) + std::f32::consts::PI + side * 0.49;
                let p = Point::new(tip.x + arm * a.cos(), tip.y + arm * a.sin());
                out.push(Contour::polyline(&[tip, p], false));
            }
        }
        out
    }

    pub fn draw(&self, p: &mut Painter, progress: f32, style: &StrokeStyle) {
        for c in self.contours(progress) {
            p.stroke(&c, style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::make;
    use crate::text::{EstimateText, TextLayout};

    fn close(a: Point, b: Point, eps: f32) -> bool {
        (a.x - b.x).abs() < eps && (a.y - b.y).abs() < eps
    }

    fn shaped(text: &str) -> ShapedText {
        EstimateText::default().layout(text, &crate::element::TextStyle::new(0, 10.0, 12.0), None)
    }

    #[test]
    fn on_a_straight_line_glyphs_sit_as_laid_out() {
        let s = shaped("abc");
        let line = Contour::polyline(&[Point::new(10.0, 50.0), Point::new(200.0, 50.0)], false);
        let placed = place_on_path(&s, &line, 0.0);
        assert_eq!(placed.len(), 3);
        for (p, g) in placed.iter().zip(&s.glyphs) {
            assert!(close(p.pen, Point::new(10.0 + g.pos.x, 50.0), 1e-3), "{p:?}");
            assert!(p.angle.abs() < 1e-5);
        }
    }

    #[test]
    fn on_a_circle_glyphs_turn_to_the_tangent() {
        let s = shaped("abcdef");
        let centre = Point::new(100.0, 100.0);
        // Clockwise from the top: the tangent at arc length s points along the circle.
        let ring = make::arc(centre, 50.0, -std::f32::consts::FRAC_PI_2, std::f32::consts::PI);
        let placed = place_on_path(&s, &ring, 0.0);
        for (p, g) in placed.iter().zip(&s.glyphs) {
            let mid = Point::new(p.pen.x + p.angle.cos() * 0.5 * g.advance, p.pen.y + p.angle.sin() * 0.5 * g.advance);
            let r = ((mid.x - centre.x).powi(2) + (mid.y - centre.y).powi(2)).sqrt();
            assert!((r - 50.0).abs() < 0.2, "glyph middle off the circle: {r}");
            // The tangent of a circle is at right angles to the radius.
            let (rx, ry) = (mid.x - centre.x, mid.y - centre.y);
            let dot = rx * p.angle.cos() + ry * p.angle.sin();
            assert!(dot.abs() < 0.5, "{dot}");
        }
        // Angles grow clockwise along a clockwise arc.
        assert!(placed.windows(2).all(|w| w[1].angle > w[0].angle));
    }

    #[test]
    fn path_start_by_alignment() {
        assert_eq!(path_start(Align::Start, 100.0, 40.0), 0.0);
        assert_eq!(path_start(Align::Center, 100.0, 40.0), 30.0);
        assert_eq!(path_start(Align::End, 100.0, 40.0), 60.0);
    }

    #[test]
    fn flight_goes_from_rail_to_slot_with_an_s_bend() {
        let f = Flight::new(Point::new(0.0, 0.0), Point::new(100.0, 0.0), 1.0, 0.4);
        assert_eq!(f.at(0.5).0, f.from);
        assert_eq!(f.at(2.0), (f.to, 0.0));
        // An S: the curve crosses the straight line, one bow each side.
        let pts = f.path().resample(21);
        assert!(pts.iter().any(|p| p.y > 5.0) && pts.iter().any(|p| p.y < -5.0));
        let mid = f.at(1.2).0;
        assert!(mid.x > 10.0 && mid.x < 90.0, "{mid:?}");
        // The leader leads the glyph, then retracts toward the slot and is gone.
        let (a, b) = f.leader_span(1.1).unwrap();
        assert_eq!(a, 0.0);
        let len = f.path().length();
        assert!(b > f.progress(1.1) * len);
        let (a, b) = f.leader_span(1.5).unwrap();
        assert!(a > 0.0 && (b - len).abs() < 1e-3);
        assert!(f.leader_span(f.done_at() + 0.01).is_none());
        assert!(f.leader_span(0.9).is_none());
    }

    #[test]
    fn arrow_draws_on_then_grows_its_head() {
        let a = Arrow::new(Point::new(0.0, 40.0), Point::new(60.0, 40.0));
        assert!(a.contours(0.0).is_empty());
        assert_eq!(a.contours(0.5).len(), 1);
        let full = a.contours(1.0);
        assert_eq!(full.len(), 3);
        // The arc bows up (y down) for a positive bend from left to right.
        assert!(a.path().resample(11).iter().any(|p| p.y < 30.0));
        // Head arms start at the tip and are `head` long.
        for arm in &full[1..] {
            let pts = arm.resample(2);
            assert!(close(pts[0], a.to, 0.2), "{pts:?}");
            let d = ((pts[1].x - pts[0].x).powi(2) + (pts[1].y - pts[0].y).powi(2)).sqrt();
            assert!((d - a.head).abs() < 0.1);
        }
    }
}
