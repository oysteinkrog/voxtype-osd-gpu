//! Elements whose drawing depends on the frame time itself, not only on channels: a scrolling
//! waveform and a progress bar with an indeterminate sweep.
//!
//! Both stay pure functions of `t`, like every other part of `sample`. A waveform scrolls
//! between two messages without a new view, and a 144 Hz session and a 60 fps export draw the
//! same frames at the same times.

use std::sync::Arc;

use crate::display::{Paint, ShapeInstance};
use crate::geom::{Color, Point, Rect};

/// A filled waveform mirrored about its centre line, scrolling right to left
/// (`Element::wave`).
///
/// Levels are evenly spaced in time. The newest reaches the right edge at `newest_at`; until
/// then the wave scrolls at the frame time, and after it the wave stays still. So a wave whose
/// levels stop coming freezes with its history in place, and the app only sends new levels
/// and a new `newest_at`.
#[derive(Clone, Debug, PartialEq)]
pub struct Wave {
    /// Half heights from 0 to 1, as a fraction of half the box height, oldest first.
    pub levels: Arc<[f32]>,
    /// Scene time at which the newest level reaches the right edge.
    pub newest_at: f64,
    /// Levels per second.
    pub rate: f32,
    /// Seconds of levels across the box width.
    pub window: f32,
    /// Colour stops across the box width as `(position 0..1, colour)`, in order. Between stops
    /// the colour is mixed linearly; outside them it is the nearest stop's.
    pub stops: Vec<(f32, Color)>,
    /// Smallest half height in logical units, so silence still draws a thin line.
    pub min_half: f32,
    /// A one-pixel centre line under the wave, across the whole box.
    pub baseline: Option<Color>,
}

impl Default for Wave {
    fn default() -> Wave {
        Wave {
            levels: Arc::from([]),
            newest_at: 0.0,
            rate: 100.0,
            window: 3.0,
            stops: vec![(0.0, Color::WHITE)],
            min_half: 0.5,
            baseline: None,
        }
    }
}

impl Wave {
    /// Whether the wave still scrolls at `t`.
    pub fn moving(&self, t: f64) -> bool {
        self.levels.len() > 1 && t < self.newest_at
    }

    /// The colour at `f` (0 to 1 across the box).
    pub fn color_at(&self, f: f32) -> Color {
        let Some(&(p0, c0)) = self.stops.first() else { return Color::TRANSPARENT };
        if f <= p0 {
            return c0;
        }
        for w in self.stops.windows(2) {
            let ((pa, a), (pb, b)) = (w[0], w[1]);
            if f <= pb {
                let k = if pb > pa { (f - pa) / (pb - pa) } else { 1.0 };
                let mix = |x: f32, y: f32| x + (y - x) * k;
                return Color::rgba(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b), mix(a.a, b.a));
            }
        }
        self.stops.last().map_or(c0, |s| s.1)
    }

    /// The bands that draw the wave in `rect` at `t`, with `alpha` multiplied in.
    pub fn shapes(&self, rect: Rect, t: f64, alpha: f32, out: &mut Vec<ShapeInstance>) {
        let mid = rect.y + 0.5 * rect.h;
        if let Some(c) = self.baseline {
            let line = Paint::solid(c.fade(alpha));
            out.push(
                ShapeInstance::line(Point::new(rect.x, mid), Point::new(rect.right(), mid), 0.0, line)
                    .with_hairline(line),
            );
        }
        let n = self.levels.len();
        if n < 2 || self.rate <= 0.0 || self.window <= 0.0 || rect.w <= 0.0 {
            return;
        }
        let edge = t.min(self.newest_at);
        let px_per_s = rect.w as f64 / self.window as f64;
        let step = 1.0 / self.rate as f64;
        let max_half = (0.5 * rect.h - 0.5).max(0.0);
        let x_of = |i: usize| rect.right() as f64 - (edge - (self.newest_at - (n - 1 - i) as f64 * step)) * px_per_s;
        let half_of = |i: usize| (self.levels[i].clamp(0.0, 1.0) * max_half).max(self.min_half);
        // Only levels that reach into the box: the oldest one on screen is one before the left
        // edge.
        let first = {
            let behind = (self.newest_at - edge) / step + self.window as f64 * self.rate as f64;
            (n as f64 - 1.0 - behind.ceil() - 1.0).max(0.0) as usize
        };
        for i in first..n - 1 {
            let (xa, xb) = (x_of(i) as f32, x_of(i + 1) as f32);
            if xb <= rect.x || xa >= rect.right() {
                continue;
            }
            let (ha, hb) = (half_of(i), half_of(i + 1));
            let at = |x: f32| ha + (hb - ha) * (x - xa) / (xb - xa);
            let (x0, x1) = (xa.max(rect.x), xb.min(rect.right()));
            if x1 <= x0 {
                continue;
            }
            let f = |x: f32| (x - rect.x) / rect.w;
            let paint = Paint::linear(
                Point::new(x0, mid),
                self.color_at(f(x0)).fade(alpha),
                Point::new(x1, mid),
                self.color_at(f(x1)).fade(alpha),
            );
            out.push(ShapeInstance::band(x0, x1, mid, at(x0), at(x1), paint));
        }
    }
}

/// A thin progress bar (`Element::progress`): a track across the box and a bar over it.
#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    /// Done, 0 to 1. `None` draws the indeterminate sweep.
    pub fraction: Option<f32>,
    pub track: Color,
    pub bar: Color,
    /// The sweep: a bar a quarter of the box wide crosses it once every `period` seconds,
    /// counted from `since`.
    pub since: f64,
    pub period: f32,
}

impl Default for Progress {
    fn default() -> Progress {
        Progress { fraction: None, track: Color::TRANSPARENT, bar: Color::WHITE, since: 0.0, period: 1.6 }
    }
}

impl Progress {
    /// Whether the bar moves at `t`: only the sweep does.
    pub fn moving(&self) -> bool {
        self.fraction.is_none() && self.period > 0.0
    }

    /// The shapes for `rect` at `t`.
    pub fn shapes(&self, rect: Rect, t: f64, alpha: f32, out: &mut Vec<ShapeInstance>) {
        let rrect =
            |r: Rect, c: Color| ShapeInstance::rrect(crate::geom::RRect::new(r, 0.0), Paint::solid(c.fade(alpha)));
        if self.track.a > 0.0 {
            out.push(rrect(rect, self.track));
        }
        let (x0, x1) = match self.fraction {
            Some(f) => (rect.x, rect.x + rect.w * f.clamp(0.0, 1.0)),
            None => {
                let seg = rect.w * 0.25;
                let phase = if self.period > 0.0 {
                    ((t - self.since).rem_euclid(self.period as f64) / self.period as f64) as f32
                } else {
                    0.0
                };
                let x = rect.x + phase * (rect.w + seg) - seg;
                (x.max(rect.x), (x + seg).min(rect.right()))
            }
        };
        if x1 > x0 {
            out.push(rrect(Rect::new(x0, rect.y, x1 - x0, rect.h), self.bar));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wave(levels: &[f32], newest_at: f64) -> Wave {
        Wave { levels: Arc::from(levels), newest_at, rate: 10.0, window: 1.0, ..Wave::default() }
    }

    #[test]
    fn wave_scrolls_until_the_newest_level_and_then_stays() {
        let w = wave(&[0.5; 20], 2.0);
        let r = Rect::new(0.0, 0.0, 100.0, 20.0);
        let at = |t: f64| {
            let mut out = Vec::new();
            w.shapes(r, t, 1.0, &mut out);
            out
        };
        assert!(w.moving(1.9) && !w.moving(2.0));
        // At 1.95 s the newest level is half a step right of the edge, so the last band is cut
        // there.
        let last = *at(1.95).last().unwrap();
        assert!((last.geom[0] - 95.0).abs() < 1e-3 && last.geom[2] == 100.0, "{:?}", last.geom);
        assert_eq!(at(2.0), at(5.0), "frozen after the newest level");
        // One second across the box at ten levels a second: ten bands, the oldest cut at the
        // left edge.
        let bands = at(2.0);
        assert_eq!(bands.len(), 10);
        assert_eq!(bands[0].geom[0], 0.0);
        assert_eq!(bands.last().unwrap().geom[2], 100.0);
        for p in bands.windows(2) {
            assert_eq!(p[0].geom[2], p[1].geom[0], "bands share their ends");
        }
    }

    #[test]
    fn wave_colour_follows_the_stops() {
        let w = Wave { stops: vec![(0.0, Color::rgba(0.0, 0.0, 0.0, 0.0)), (0.5, Color::WHITE)], ..Wave::default() };
        assert_eq!(w.color_at(-1.0).a, 0.0);
        assert_eq!(w.color_at(0.25), Color::rgba(0.5, 0.5, 0.5, 0.5));
        assert_eq!(w.color_at(0.9), Color::WHITE);
    }

    #[test]
    fn sweep_crosses_once_per_period() {
        let p = Progress { track: Color::BLACK, period: 2.0, ..Progress::default() };
        let r = Rect::new(0.0, 0.0, 100.0, 2.0);
        let bar = |t: f64| {
            let mut out = Vec::new();
            p.shapes(r, t, 1.0, &mut out);
            out.get(1).map(|s| (s.geom[0], s.geom[2]))
        };
        assert_eq!(bar(0.0), None, "starts just left of the box");
        assert_eq!(bar(0.5), bar(2.5));
        assert!(bar(0.5).is_some());
        let done = Progress { fraction: Some(0.25), ..p.clone() };
        let mut out = Vec::new();
        done.shapes(r, 0.0, 1.0, &mut out);
        assert_eq!(out[1].geom[2], 25.0);
        assert!(!done.moving() && p.moving());
    }
}

#[cfg(test)]
mod scene_tests {
    use super::*;
    use crate::element::Element;
    use crate::scene::Scene;

    #[test]
    fn a_scrolling_wave_keeps_the_scene_awake_until_it_freezes() {
        let mut scene: Scene = Scene::new();
        let wave = Wave { levels: Arc::from([0.2f32; 30].as_slice()), newest_at: 3.0, ..Wave::default() };
        scene.apply(Element::column("root").child(Element::wave("w", wave).size(100.0, 20.0)), 1.0);
        assert_eq!(scene.next_wake(2.0), Some(2.0));
        assert_eq!(scene.next_wake(3.0), None);
        assert_ne!(scene.sample(2.9), scene.sample(2.95));
        assert!(!scene.sample(2.9).shapes.is_empty());
        assert_eq!(scene.sample(3.0), scene.sample(4.0));
    }
}
