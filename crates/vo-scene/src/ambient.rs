//! Ambient loops on element properties (FX-203, FX-047) and ripples (FX-101).
//!
//! `Element::ambient(LoopProp::Rotation, Loop::spin(2.0))` runs a loop on a property while the
//! view declares it. When a later view leaves it out, or the node leaves, the loop settles to
//! its rest (see `vo_core::ambient`), and the scene reaches `next_wake == None` again. Reduced
//! motion holds every loop at its rest and draws no ripples.
//!
//! `Element::ripples` draws thin rings that grow from the centre of the box and fade, a new
//! one every `every` seconds while `active` is set. When `active` goes off, no new ring
//! starts and the ones on screen finish.

pub use vo_core::ambient::{Loop, Settle, Wave};
use vo_core::ambient::{LoopRun, Toggle, AMBIENT_FPS};
use vo_core::channel::Rest;
use vo_core::track::CubicBezier;

use crate::display::{Paint, ShapeInstance};
use crate::element::Slot;
use crate::geom::{Color, Point, Rect};

/// A property an ambient loop can drive. The loop's offset is added to the property's value
/// in the property's own units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LoopProp {
    Opacity,
    OffsetX,
    OffsetY,
    Scale,
    /// Radians.
    Rotation,
    StrokeWidth,
    GlowRadius,
    GlowStrength,
    BloomStrength,
    /// The rim's angle in turns (`Element::rim`).
    RimAngle,
    /// A wavy line's phase in waves.
    WavePhase,
    TrimStart,
    TrimEnd,
    Morph,
    TextBlur,
}

impl LoopProp {
    pub(crate) fn slot(self) -> u8 {
        (match self {
            LoopProp::Opacity => Slot::Opacity,
            LoopProp::OffsetX => Slot::OffsetX,
            LoopProp::OffsetY => Slot::OffsetY,
            LoopProp::Scale => Slot::Scale,
            LoopProp::Rotation => Slot::Rotation,
            LoopProp::StrokeWidth => Slot::StrokeWidth,
            LoopProp::GlowRadius => Slot::GlowRadius,
            LoopProp::GlowStrength => Slot::GlowStrength,
            LoopProp::BloomStrength => Slot::BloomStrength,
            LoopProp::RimAngle => Slot::RimAngle,
            LoopProp::WavePhase => Slot::WavePhase,
            LoopProp::TrimStart => Slot::TrimStart,
            LoopProp::TrimEnd => Slot::TrimEnd,
            LoopProp::Morph => Slot::Morph,
            LoopProp::TextBlur => Slot::TextBlur,
        }) as u8
    }
}

/// Rings that grow from the centre of the box and fade (`Element::ripples`).
#[derive(Clone, Debug, PartialEq)]
pub struct Ripples {
    pub color: Color,
    /// Stroke width at birth in logical units; a ring thins to half as it grows.
    pub width: f32,
    /// Radius at birth.
    pub from: f32,
    /// Radius at the end of its life; 0 reaches the edge of the box.
    pub to: f32,
    /// Seconds between rings while active.
    pub every: f64,
    /// Seconds one ring lives.
    pub life: f64,
    /// Rings start while this is set. The first one starts when it turns on.
    pub active: bool,
    /// Steps per second (FX08 caps ambient motion at 30).
    pub fps: f64,
    /// Extra rings at these times, for example one per detected word, active or not.
    pub pulses: Vec<f64>,
}

impl Ripples {
    pub fn new(color: Color) -> Ripples {
        Ripples {
            color,
            width: 1.5,
            from: 0.0,
            to: 0.0,
            every: 0.9,
            life: 1.8,
            active: true,
            fps: AMBIENT_FPS,
            pulses: Vec::new(),
        }
    }

    pub fn radii(self, from: f32, to: f32) -> Ripples {
        Ripples { from, to, ..self }
    }

    pub fn width(self, width: f32) -> Ripples {
        Ripples { width, ..self }
    }

    /// A ring every `every` seconds, each living `life` seconds.
    pub fn timing(self, every: f64, life: f64) -> Ripples {
        Ripples { every: every.max(1e-3), life: life.max(1e-3), ..self }
    }

    pub fn active(self, active: bool) -> Ripples {
        Ripples { active, ..self }
    }

    pub fn pulses(self, at: impl IntoIterator<Item = f64>) -> Ripples {
        Ripples { pulses: at.into_iter().collect(), ..self }
    }

    /// `t` held to the start of its step.
    fn quantized(&self, t: f64) -> f64 {
        if self.fps > 0.0 {
            (t * self.fps + 1e-6).floor() / self.fps
        } else {
            t
        }
    }

    /// Births between `lo` and `hi`, both included, without walking an open run from the start.
    fn births_in(&self, runs: &[(f64, f64)], lo: f64, hi: f64) -> Vec<f64> {
        let mut out = Vec::new();
        for &(on, off) in runs {
            let first = ((lo - on) / self.every).ceil().max(0.0) as u64;
            let mut k = first;
            loop {
                let b = on + k as f64 * self.every;
                if b > hi || b >= off {
                    break;
                }
                out.push(b);
                k += 1;
            }
        }
        out.extend(self.pulses.iter().copied().filter(|&p| p >= lo && p <= hi));
        out
    }

    /// The rings at `t`: each ring's age as a fraction of its life, 0 to 1.
    pub fn rings(&self, runs: &[(f64, f64)], t: f64) -> Vec<f32> {
        let tq = self.quantized(t);
        let mut ages: Vec<f32> = self
            .births_in(runs, tq - self.life, tq)
            .into_iter()
            .map(|b| ((tq - b) / self.life) as f32)
            .filter(|p| (0.0..1.0).contains(p))
            .collect();
        // Oldest first, so younger rings draw on top.
        ages.sort_by(|a, b| b.total_cmp(a));
        ages
    }

    /// The rings at `t` in `rect`.
    pub fn shapes(&self, runs: &[(f64, f64)], rect: Rect, t: f64, alpha: f32, out: &mut Vec<ShapeInstance>) {
        let centre = Point::new(rect.x + 0.5 * rect.w, rect.y + 0.5 * rect.h);
        let to = if self.to > 0.0 { self.to } else { 0.5 * rect.w.min(rect.h) - self.width };
        for p in self.rings(runs, t) {
            let r = self.from + (to - self.from) * CubicBezier::OUT.ease(p as f64) as f32;
            let k = (1.0 - p) * (1.0 - p);
            let mut ring = ShapeInstance::circle(centre, r.max(0.0), Paint::NONE);
            ring.stroke = Paint::solid(self.color.fade(alpha * k));
            ring.stroke_width = self.width * (1.0 - 0.5 * p);
            out.push(ring);
        }
    }

    /// When the ripples are at rest for good: infinite while active.
    pub fn rest_at(&self, runs: &[(f64, f64)]) -> f64 {
        if runs.last().is_some_and(|r| r.1.is_infinite()) {
            return f64::INFINITY;
        }
        let last_births = runs.iter().filter_map(|&(on, off)| {
            let n = ((off - on) / self.every - 1e-9).ceil();
            (n >= 1.0).then_some(on + (n - 1.0) * self.every)
        });
        last_births.chain(self.pulses.iter().copied()).map(|b| b + self.life).fold(f64::NEG_INFINITY, f64::max)
    }

    /// The next time after `t` at which the rings change, or `None` once none are left and
    /// none are due.
    pub fn wake(&self, runs: &[(f64, f64)], t: f64) -> Option<f64> {
        let open = runs.iter().any(|r| r.0 <= t && t < r.1);
        if open || !self.rings(runs, t).is_empty() {
            let next = if self.fps > 0.0 { ((t * self.fps + 1e-6).floor() + 1.0) / self.fps } else { t };
            return Some(next);
        }
        // Nothing on screen: sleep until the next ring is born, if one is due.
        let run = runs.iter().map(|r| r.0).filter(|&on| on > t);
        let due = run.chain(self.pulses.iter().copied().filter(|&p| p > t)).fold(f64::INFINITY, f64::min);
        due.is_finite().then(|| if self.fps > 0.0 { (due * self.fps - 1e-6).ceil() / self.fps } else { due })
    }

    pub fn fps(self, fps: f64) -> Ripples {
        Ripples { fps: fps.max(0.0), ..self }
    }
}

/// A node's ambient state: its loops by slot and the on and off runs of its ripples.
#[derive(Clone, Debug, Default)]
pub(crate) struct NodeAmbient {
    loops: Vec<(u8, LoopRun)>,
    ripples: Option<Toggle>,
}

impl NodeAmbient {
    /// Matches the loops to the view's at `t`, and the ripples to `ripples_on`. `fresh` is set
    /// for a node created at `t`, whose loops start at their own value; a loop added to a node
    /// already on screen takes over from offset 0. Returns whether any motion changed.
    pub fn sync(&mut self, want: &[(u8, Loop)], ripples_on: Option<bool>, t: f64, fresh: bool, retention: f64) -> bool {
        let mut changed = false;
        for &(slot, l) in want {
            match self.loops.iter_mut().find(|(s, _)| *s == slot) {
                Some((_, run)) => changed |= run.set(t, Some(l)),
                None => {
                    let rest = if Slot::is_unit(slot) { Rest::UNIT } else { Rest::PIXELS };
                    let from = (!fresh).then_some((0.0, 0.0));
                    self.loops.push((slot, LoopRun::new(t, l, from, rest, retention)));
                    changed = true;
                }
            }
        }
        for (slot, run) in &mut self.loops {
            if !want.iter().any(|(s, _)| s == slot) {
                changed |= run.set(t, None);
            }
        }
        if retention.is_finite() {
            self.loops.retain(|(_, run)| run.rest_at() >= t - retention);
        }
        match (&mut self.ripples, ripples_on) {
            (Some(tg), Some(on)) => changed |= tg.set(t, on),
            (None, Some(on)) => {
                self.ripples = Some(Toggle::new(t, on));
                changed |= on;
            }
            (Some(tg), None) => changed |= tg.set(t, false),
            (None, None) => {}
        }
        changed
    }

    /// Lets every loop settle and stops the ripples, for a node that leaves.
    pub fn stop(&mut self, t: f64) {
        for (_, run) in &mut self.loops {
            run.set(t, None);
        }
        if let Some(tg) = &mut self.ripples {
            tg.set(t, false);
        }
    }

    /// The loop offset on `slot` at `t`.
    pub fn offset(&self, slot: u8, t: f64, reduced: bool) -> f64 {
        match self.loops.iter().find(|(s, _)| *s == slot) {
            None => 0.0,
            Some((_, run)) if reduced => run.spec().rest,
            Some((_, run)) => run.value(t),
        }
    }

    pub fn velocity(&self, slot: u8, t: f64, reduced: bool) -> f64 {
        match self.loops.iter().find(|(s, _)| *s == slot) {
            Some((_, run)) if !reduced => run.velocity(t),
            _ => 0.0,
        }
    }

    pub fn ripple_runs(&self) -> &[(f64, f64)] {
        self.ripples.as_ref().map_or(&[], |tg| tg.runs())
    }

    /// When anything ambient next changes after `t`.
    pub fn wake(&self, t: f64, reduced: bool, ripples: Option<&Ripples>) -> Option<f64> {
        if reduced {
            return None;
        }
        let rings = ripples.and_then(|r| r.wake(self.ripple_runs(), t));
        self.loops.iter().filter_map(|(_, run)| run.wake(t)).chain(rings).min_by(f64::total_cmp)
    }

    /// When everything ambient is at rest for good.
    pub fn rest_at(&self, reduced: bool, ripples: Option<&Ripples>) -> f64 {
        if reduced {
            return f64::NEG_INFINITY;
        }
        let rings = ripples.map_or(f64::NEG_INFINITY, |r| r.rest_at(self.ripple_runs()));
        self.loops.iter().map(|(_, run)| run.rest_at()).fold(rings, f64::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rings_start_each_period_and_finish_after_off() {
        let r = Ripples::new(Color::WHITE).timing(0.5, 1.2).fps(0.0);
        let runs = [(1.0, 2.1)];
        // Births at 1.0, 1.5, 2.0; at 2.05 all three are alive.
        let ages = r.rings(&runs, 2.05);
        assert_eq!(ages.len(), 3);
        assert!((ages[0] - (1.05 / 1.2) as f32).abs() < 1e-6);
        assert_eq!(r.rest_at(&runs), 3.2);
        assert!(r.rings(&runs, 3.2).is_empty());
        assert_eq!(r.wake(&runs, 3.2), None);
        assert_eq!(r.wake(&runs, 3.1), Some(3.1));
        assert_eq!(r.rest_at(&[(1.0, f64::INFINITY)]), f64::INFINITY);
    }

    #[test]
    fn rings_step_at_their_frame_rate() {
        let r = Ripples::new(Color::WHITE);
        let runs = [(0.0, f64::INFINITY)];
        assert_eq!(r.rings(&runs, 0.34), r.rings(&runs, 0.36));
        assert_ne!(r.rings(&runs, 0.36), r.rings(&runs, 0.37));
        let w = r.wake(&runs, 0.34).unwrap();
        assert!((w - 11.0 / 30.0).abs() < 1e-12);
        assert!(r.wake(&runs, w).unwrap() > w);
    }

    #[test]
    fn pulses_wake_when_they_come() {
        let r = Ripples::new(Color::WHITE).active(false).pulses([5.0]).fps(30.0);
        assert_eq!(r.wake(&[], 1.0), Some(5.0));
        assert_eq!(r.rings(&[], 5.0).len(), 1);
        assert!(r.wake(&[], 5.0).is_some());
        assert_eq!(r.wake(&[], 6.81), None);
    }

    #[test]
    fn shapes_grow_and_fade() {
        let r = Ripples::new(Color::WHITE).radii(10.0, 50.0).timing(10.0, 1.0).fps(0.0);
        let runs = [(0.0, f64::INFINITY)];
        let mut out = Vec::new();
        r.shapes(&runs, Rect::new(0.0, 0.0, 200.0, 200.0), 0.0, 1.0, &mut out);
        assert_eq!(out[0].geom, [100.0, 100.0, 10.0, 0.0]);
        assert_eq!(out[0].stroke.c0.a, 1.0);
        out.clear();
        r.shapes(&runs, Rect::new(0.0, 0.0, 200.0, 200.0), 0.5, 1.0, &mut out);
        assert!(out[0].geom[2] > 30.0 && out[0].stroke.c0.a < 0.3, "{:?}", out[0]);
    }
}
