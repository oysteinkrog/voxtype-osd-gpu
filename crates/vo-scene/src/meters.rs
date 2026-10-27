//! Progress that only moves forwards, and a dismiss countdown ring.
//!
//! [`Hairline`] is a one-pixel progress line fed with reported fractions at the times they
//! arrived. It shows the largest fraction so far, approached on a first-order curve: it never
//! overshoots and never moves backwards, even when a later report is lower (a recogniser that
//! re-estimates its progress).
//!
//! [`Countdown`] is a ring whose arc trims from whole to nothing over its duration, for a
//! popup that dismisses itself. Both are pure functions of `t` and draw through a
//! [`Painter`], so they go in a view as `Element::canvas`.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::sync::Arc;

use crate::canvas::Painter;
use crate::display::{Paint, ShapeInstance};
use crate::geom::{Color, Point, Rect};
use crate::path::{make, StrokeStyle};

/// A progress hairline (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct Hairline {
    /// Reported fractions 0 to 1 with the time each arrived, in time order.
    pub reports: Arc<[(f64, f32)]>,
    /// Time constant of the approach in seconds: the line gets within 2% of a new value
    /// in about four of these (200 ms at the default).
    pub tau: f32,
    /// Line thickness in logical units; 0 draws one physical pixel.
    pub width: f32,
    pub color: Color,
    /// The track under the line.
    pub track: Color,
}

impl Default for Hairline {
    fn default() -> Hairline {
        Hairline {
            reports: Arc::from([]),
            tau: 0.05,
            width: 0.0,
            color: Color::WHITE,
            track: Color::hex(0xffffff, 0.12),
        }
    }
}

impl Hairline {
    /// The fraction shown at `t`.
    pub fn value(&self, t: f64) -> f32 {
        let tau = self.tau.max(1e-3) as f64;
        let (mut x, mut target, mut since) = (0.0f64, 0.0f64, f64::NEG_INFINITY);
        for &(at, f) in self.reports.iter() {
            if at > t {
                break;
            }
            // Where the line is when this report arrives, then the new target: the most so far.
            x = approach(x, target, at - since, tau);
            target = target.max(f.clamp(0.0, 1.0) as f64);
            since = at;
        }
        approach(x, target, t - since, tau) as f32
    }

    /// Whether the line still moves at `t`.
    pub fn moving(&self, t: f64) -> bool {
        let shown = self.value(t);
        let target = self.reports.iter().filter(|r| r.0 <= t).fold(0.0f32, |m, r| m.max(r.1.clamp(0.0, 1.0)));
        target - shown > 1e-4 || self.reports.iter().any(|r| r.0 > t)
    }

    /// Draws the track and the line across `rect`, centred on its middle.
    pub fn paint(&self, t: f64, rect: Rect, p: &mut Painter) {
        let y = rect.y + 0.5 * rect.h;
        let line = |x1: f32, c: Color| {
            let paint = Paint::solid(c);
            let s = ShapeInstance::line(Point::new(rect.x, y), Point::new(x1, y), self.width, paint);
            if self.width <= 0.0 {
                s.with_hairline(paint)
            } else {
                s
            }
        };
        if self.track.a > 0.0 {
            p.shape(line(rect.right(), self.track));
        }
        let x1 = rect.x + rect.w * self.value(t);
        if x1 > rect.x + 0.01 {
            p.shape(line(x1, self.color));
        }
    }
}

fn approach(x: f64, target: f64, dt: f64, tau: f64) -> f64 {
    // Before the first report there is no start time; the line has been at its target forever.
    if !dt.is_finite() {
        return target;
    }
    target - (target - x) * (-dt.max(0.0) / tau).exp()
}

/// A dismiss countdown ring (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Countdown {
    /// When the countdown starts and how long it runs.
    pub start: f64,
    pub secs: f32,
    /// Stroke width as a fraction of the box side.
    pub width: f32,
    pub color: Color,
    /// The faint whole ring under the arc.
    pub track: Color,
}

impl Default for Countdown {
    fn default() -> Countdown {
        Countdown { start: 0.0, secs: 4.0, width: 0.09, color: Color::WHITE, track: Color::hex(0xffffff, 0.14) }
    }
}

impl Countdown {
    /// The fraction of the ring left at `t`: 1 at the start, 0 at the end.
    pub fn left(&self, t: f64) -> f32 {
        let u = ((t - self.start) / self.secs.max(1e-3) as f64).clamp(0.0, 1.0);
        1.0 - u as f32
    }

    /// When the ring is empty.
    pub fn end(&self) -> f64 {
        self.start + self.secs as f64
    }

    /// Draws the ring in the square that fits `rect`, centred. The arc starts at 12 o'clock
    /// and runs clockwise; its end moves back towards 12 as time runs out.
    pub fn paint(&self, t: f64, rect: Rect, p: &mut Painter) {
        let side = rect.w.min(rect.h);
        let w = self.width * side;
        let r = 0.5 * side - 0.5 * w - 0.5;
        let c = Point::new(rect.x + 0.5 * rect.w, rect.y + 0.5 * rect.h);
        if self.track.a > 0.0 {
            p.stroke(&make::ring(c, r), &StrokeStyle::solid(w, self.track));
        }
        let left = self.left(t);
        if left > 1e-4 {
            p.stroke(&make::arc(c, r, -FRAC_PI_2, left * TAU), &StrokeStyle::solid(w, self.color));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hairline_never_moves_backwards() {
        let reports: Vec<(f64, f32)> =
            vec![(0.0, 0.1), (0.3, 0.4), (0.35, 0.2), (0.6, 0.38), (0.9, 0.7), (1.0, 0.65), (1.4, 1.0)];
        let h = Hairline { reports: Arc::from(reports.as_slice()), ..Hairline::default() };
        let mut last = 0.0f32;
        for i in 0..400 {
            let t = i as f64 * 0.005;
            let v = h.value(t);
            assert!(v >= last - 1e-6, "back at {t}: {v} < {last}");
            assert!(v <= 1.0);
            last = v;
        }
        // The lower report at 0.35 does not pull it back; it reaches the most so far.
        assert!((h.value(0.89) - 0.4).abs() < 0.01, "{}", h.value(0.89));
        assert!((h.value(3.0) - 1.0).abs() < 1e-4);
        assert!(h.moving(1.5) && !h.moving(3.0));
        assert_eq!(h.value(0.0), 0.0, "starts from nothing");
    }

    #[test]
    fn the_countdown_runs_from_whole_to_nothing() {
        let c = Countdown { start: 1.0, secs: 2.0, ..Countdown::default() };
        assert_eq!(c.left(0.5), 1.0);
        assert_eq!(c.left(2.0), 0.5);
        assert_eq!(c.left(3.5), 0.0);
        assert_eq!(c.end(), 3.0);
    }
}
