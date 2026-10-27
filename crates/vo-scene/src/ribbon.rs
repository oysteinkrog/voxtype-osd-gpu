//! Ribbon strands (`Element::ribbon`): several translucent glowing strands that wind around
//! each other across the box, pinched to a point at both ends, their swing following the
//! level (FX-112, concept 24).
//!
//! Each strand is a sum of two sine waves with its own phase, so neighbours cross and part
//! again. Its width swells in the middle and tapers to nothing at the ends, which also fades
//! it out. Strand `i` of `n` takes its colours from `left.at(i / (n - 1))` at the left end to
//! `right.at(i / (n - 1))` at the right end. An optional bright core line runs on the first
//! strand.
//!
//! The strands move with time until the newest level (or the end of `settle`) and then hold
//! still. Every frame is a pure function of `t` and the fields. Each strand is cut into runs
//! like `Trace`, for the same reason.

use crate::canvas::Painter;
use crate::element::Element;
use crate::geom::{Color, Point, Rect};
use crate::levels::{smoothstep, Levels, Stops};
use crate::path::{make, Contour, StrokeStyle};

/// See the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct Ribbon {
    pub levels: Levels,
    pub strands: u32,
    /// Waves across the box and their speed in cycles a second.
    pub cycles: f32,
    pub speed: f32,
    /// Phase spread over all strands, in turns.
    pub spread: f32,
    /// Swing at level 1 and at silence, as fractions of half the box height.
    pub gain: f32,
    pub floor: f32,
    /// Where the ribbon is widest, 0 to 1 across the box.
    pub belly: f32,
    /// Strand width at the widest point, and its alpha.
    pub width: f32,
    pub alpha: f32,
    pub glow: f32,
    pub left: Stops,
    pub right: Stops,
    /// A thin bright line on the first strand.
    pub core: Option<Color>,
    pub settle: Option<f64>,
    pub settle_secs: f32,
    /// Logical units between points, and segments per stroke run.
    pub step: f32,
    pub run: usize,
}

impl Default for Ribbon {
    fn default() -> Ribbon {
        let (cyan, blue, violet) = (Color::hex(0x22d3ee, 1.0), Color::hex(0x3b82f6, 1.0), Color::hex(0xa855f7, 1.0));
        Ribbon {
            levels: Levels::default(),
            strands: 5,
            cycles: 2.2,
            speed: 0.35,
            spread: 0.55,
            gain: 0.85,
            floor: 0.12,
            belly: 0.55,
            width: 3.0,
            alpha: 0.7,
            glow: 8.0,
            left: Stops::new(&[(0.0, cyan), (1.0, blue)]),
            right: Stops::new(&[(0.0, violet), (0.5, blue), (1.0, cyan)]),
            core: Some(Color::hex(0xf0f9ff, 0.95)),
            settle: None,
            settle_secs: 0.5,
            step: 3.0,
            run: 48,
        }
    }
}

impl Ribbon {
    pub fn until(&self) -> f64 {
        let end = self.settle.map_or(f64::NEG_INFINITY, |s| s + self.settle_secs as f64);
        self.levels.newest_at.max(end)
    }

    pub fn moving(&self, t: f64) -> bool {
        t < self.until()
    }

    /// The swing envelope across the box: 0 at both ends, 1 at `belly`.
    pub fn shape(&self, u: f32) -> f32 {
        let b = self.belly.clamp(0.05, 0.95);
        let v = if u < b { u / b } else { (1.0 - u) / (1.0 - b) };
        smoothstep(0.0, 1.0, v.clamp(0.0, 1.0))
    }

    /// Strand `i`'s points and width factors in `rect` at `t`.
    pub fn strand(&self, rect: Rect, t: f64, i: u32) -> (Vec<Point>, Vec<f32>) {
        let tp = t.min(self.until());
        let level = self.levels.at(tp.min(self.levels.newest_at));
        let settle = self
            .settle
            .map_or(1.0, |s| 1.0 - smoothstep(0.0, 1.0, ((tp - s) / self.settle_secs.max(1e-3) as f64) as f32));
        let half = 0.5 * rect.h;
        let mid = rect.y + half;
        let n = self.strands.max(1);
        let f = if n > 1 { i as f32 / (n - 1) as f32 } else { 0.0 };
        let swing = (self.floor + (self.gain - self.floor) * level) * settle * half * (1.0 - 0.3 * f);
        let phase = self.spread as f64 * f as f64;
        let tau = std::f64::consts::TAU;
        let steps = (rect.w / self.step.max(0.5)).ceil().max(2.0) as usize;
        let mut pts = Vec::with_capacity(steps + 1);
        let mut widths = Vec::with_capacity(steps + 1);
        for k in 0..=steps {
            let u = k as f32 / steps as f32;
            let env = self.shape(u);
            let x = u as f64 * self.cycles as f64;
            let a = (tau * (x - self.speed as f64 * tp + phase)).sin();
            let b = (tau * (0.53 * x + 0.71 * self.speed as f64 * tp - 1.7 * phase)).sin();
            let y = (0.7 * a + 0.3 * b) as f32;
            pts.push(Point::new(rect.x + u * rect.w, mid - swing * env * y));
            widths.push(env.max(0.02));
        }
        (pts, widths)
    }

    pub fn draw(&self, t: f64, rect: Rect, p: &mut Painter) {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let n = self.strands.max(1);
        let (x0, x1) = (Point::new(rect.x, 0.0), Point::new(rect.right(), 0.0));
        for i in (0..n).rev() {
            let f = if n > 1 { i as f32 / (n - 1) as f32 } else { 0.0 };
            let (pts, widths) = self.strand(rect, t, i);
            let c = make::ribbon(&pts, &widths);
            let (a, b) = (self.left.at(f).fade(self.alpha), self.right.at(f).fade(self.alpha));
            let style = StrokeStyle::solid(self.width, a).linear(x0, a, x1, b).glow(self.glow);
            self.stroke(&c, &style, p);
            if i == 0 {
                if let Some(core) = self.core {
                    self.stroke(&c, &StrokeStyle::solid(self.width * 0.35, core), p);
                }
            }
        }
    }

    fn stroke(&self, c: &Contour, style: &StrokeStyle, p: &mut Painter) {
        for run in c.segs.chunks(self.run.max(1)) {
            p.stroke(&Contour { segs: run.to_vec(), closed: false }, style);
        }
    }
}

impl<M> Element<M> {
    /// Ribbon strands filling the box (`crate::ribbon`). Give it a size.
    pub fn ribbon(key: impl Into<crate::element::Key>, ribbon: Ribbon) -> Element<M> {
        let until = ribbon.until();
        Element::canvas(key, until, move |t, rect, p| ribbon.draw(t, rect, p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Scene;

    const R: Rect = Rect { x: 0.0, y: 0.0, w: 300.0, h: 60.0 };

    fn ribbon(level: f32) -> Ribbon {
        Ribbon { levels: Levels::new(vec![level; 300], 3.0), ..Ribbon::default() }
    }

    #[test]
    fn strands_meet_at_both_ends_and_part_in_the_middle() {
        let r = ribbon(0.8);
        let all: Vec<_> = (0..r.strands).map(|i| r.strand(R, 2.5, i)).collect();
        for (pts, w) in &all {
            assert_eq!(pts[0].y, 30.0);
            assert_eq!(pts.last().unwrap().y, 30.0);
            assert!(w[0] < 0.05 && *w.last().unwrap() < 0.05);
            assert!(w.iter().cloned().fold(0.0, f32::max) > 0.99);
        }
        let mid = all[0].0.len() / 2;
        let ys: Vec<f32> = all.iter().map(|(p, _)| p[mid].y).collect();
        let spread = ys.iter().cloned().fold(f32::MIN, f32::max) - ys.iter().cloned().fold(f32::MAX, f32::min);
        assert!(spread > 5.0, "{ys:?}");
    }

    #[test]
    fn the_level_widens_the_swing() {
        let swing = |l: f32| {
            let r = ribbon(l);
            r.strand(R, 2.5, 0).0.iter().map(|q| (q.y - 30.0).abs()).fold(0.0, f32::max)
        };
        assert!(swing(1.0) > 2.5 * swing(0.0));
    }

    #[test]
    fn settle_lays_the_strands_flat_and_the_scene_sleeps() {
        let r = Ribbon { settle: Some(3.0), floor: 0.0, ..ribbon(0.8) };
        assert!(r.strand(R, 3.6, 2).0.iter().all(|q| (q.y - 30.0).abs() < 1e-4));
        let mut scene: Scene = Scene::new();
        scene.apply(Element::column("root").child(Element::ribbon("r", r).size(300.0, 60.0)), 0.0);
        assert_eq!(scene.next_wake(3.2), Some(3.2));
        assert_eq!(scene.next_wake(3.6), None);
        let list = scene.sample(2.0);
        // Five strands and a core, each in runs.
        assert!(list.vectors.len() >= 6);
        assert!(list.vectors.iter().all(|v| v.glow == 8.0 || v.glow == 0.0));
    }
}
