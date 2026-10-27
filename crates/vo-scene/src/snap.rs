//! A closed contour that snaps at one point (MO-53): the line breaks, the two ends pull
//! apart and curl outward like a snapped wire, while everything inside stays still.
//!
//! [`snapped`] returns the two open ends for a given progress `k` (0 whole, 1 fully
//! apart), so a frame drawn with it is a pure function of time like every other path.

use crate::geom::Point;
use crate::path::Contour;

/// How a contour snaps. Lengths are in layer units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snap {
    /// Arc length along the contour where it breaks.
    pub at: f32,
    /// Gap between the two ends when fully apart.
    pub gap: f32,
    /// How far each end lifts off the line, outward from the shape.
    pub curl: f32,
    /// Length of each end that bends.
    pub hook: f32,
}

impl Snap {
    pub fn new(at: f32) -> Snap {
        Snap { at, gap: 4.0, curl: 4.0, hook: 7.0 }
    }
}

/// The contour broken at `snap.at` with progress `k` (0 to 1), as one open piece that runs
/// from the break round to the break. Its two ends sit `k * gap` apart and lift `k * curl`
/// outward, so at 0 it looks whole. One piece, so a translucent stroke never blends twice.
pub fn snapped(contour: &Contour, snap: &Snap, k: f32) -> Vec<Contour> {
    let len = contour.length();
    if len <= 0.0 || contour.segs.is_empty() {
        return Vec::new();
    }
    let k = k.clamp(0.0, 1.0);
    let at = snap.at.rem_euclid(len);
    let half = 0.5 * snap.gap * k;
    // Points along the contour from just after the break, round, to just before it.
    let span = (len - 2.0 * half).max(0.0);
    let step = 1.0f32;
    let n = ((span / step).ceil() as usize).max(2);
    let pts: Vec<(Point, Point)> =
        (0..=n).filter_map(|i| contour.at((at + half + span * i as f32 / n as f32).rem_euclid(len))).collect();
    if k <= 0.0 {
        return vec![Contour::polyline(&pts.iter().map(|p| p.0).collect::<Vec<_>>(), false)];
    }
    // Outward is to the left of travel for a counter-clockwise contour on screen (y down,
    // positive signed area), to the right for a clockwise one.
    let outward = if signed_area(contour) > 0.0 { 1.0 } else { -1.0 };
    let lift = snap.curl * k;
    let hook = snap.hook.min(0.5 * span);
    let bend = |s: f32| {
        // 0 away from the ends, rising to 1 at an end, eased so the bend starts smooth.
        let d = s.min(span - s);
        if d >= hook {
            0.0
        } else {
            let u = 1.0 - d / hook;
            u * u
        }
    };
    let pts: Vec<Point> = pts
        .iter()
        .enumerate()
        .map(|(i, (p, tan))| {
            let s = span * i as f32 / n as f32;
            let (normal, lifted) = (Point::new(tan.y * outward, -tan.x * outward), lift * bend(s));
            Point::new(p.x + normal.x * lifted, p.y + normal.y * lifted)
        })
        .collect();
    vec![Contour::polyline(&pts, false)]
}

/// Twice the signed area of the polygon through the segment ends, positive when the contour
/// runs clockwise in y-up terms, which is counter-clockwise on a y-down screen.
fn signed_area(c: &Contour) -> f32 {
    let mut s = 0.0;
    for g in &c.segs {
        s += g.p0.x * g.p2.y - g.p2.x * g.p0.y;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::make;

    fn ends(c: &Contour) -> (Point, Point) {
        (c.segs.first().unwrap().p0, c.segs.last().unwrap().p2)
    }

    #[test]
    fn whole_at_zero_and_open_with_lifted_ends_at_one() {
        let frame = make::panel(0.0, 0.0, 200.0, 60.0, 8.0);
        // Break on the top edge, 100 units from the panel's start.
        let top = (0..400)
            .map(|i| i as f32 * frame.length() / 400.0)
            .find(|&s| frame.at(s).is_some_and(|(p, _)| p.y.abs() < 1e-3 && (p.x - 120.0).abs() < 1.0))
            .expect("a point on the top edge");
        let snap = Snap::new(top);
        let whole = snapped(&frame, &snap, 0.0);
        assert_eq!(whole.len(), 1);
        let (a, b) = ends(&whole[0]);
        assert!((a.x - b.x).abs() < 1.5 && (a.y - b.y).abs() < 0.5, "closed at 0: {a:?} {b:?}");
        let open = snapped(&frame, &snap, 1.0);
        let (a, b) = ends(&open[0]);
        // The ends sit about `gap` apart across, and both lift off the top edge, outward (up).
        assert!((a.x - b.x).abs() > 2.0, "{a:?} {b:?}");
        assert!(a.y < -2.0 && b.y < -2.0, "ends lift outward: {a:?} {b:?}");
        // Far from the break nothing moves: the bottom edge stays on y = 60.
        let pts = open[0].resample(200);
        assert!(pts.iter().any(|p| (p.y - 60.0).abs() < 1e-2));
        assert!(pts.iter().all(|p| p.y <= 60.0 + 1e-2 && p.y >= -snap.curl - 1.0));
    }
}
