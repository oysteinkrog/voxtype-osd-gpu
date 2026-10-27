//! Magnetic caret (MO-89): the caret as a small magnet that pulls new words into place.
//!
//! - **Field lines.** Around the caret bar, the field lines of a vertical dipole (in polar
//!   form `r = k sin²θ` about the bar's centre) on both sides, fainter over the text on the
//!   left. A soft light runs along each line toward the poles, brighter as the voice gets
//!   louder.
//! - **Strength follows the voice.** The field's strength is the voice level from the signal
//!   bus (fast attack, 250 ms release). A loud voice tightens and brightens the field; each
//!   new word adds a short pulse, so the field also lives without voice features.
//! - **Silence relaxes it.** About a second after the voice and the words stop, each line
//!   eases into a concentric ring around the caret, then the picture is still.
//! - **The snap.** A new word arrives a few pixels to the right, toward the caret, and is
//!   pulled into place with a short accelerating snap and a small click back. The whole word
//!   moves as one, so its letter spacing never changes; letters a word gets in a revision
//!   snap the same way.
//!
//! Rule 1 holds: the snap moves a word along its line, never off its baseline, and the
//! field is drawn behind nothing and over the text only at low opacity. Under reduced
//! motion words land in place and the field is drawn still, as its rings.

use vo_core::signals::{Sig, Signals};

use crate::display::{Paint, ShapeInstance};
use crate::geom::{Color, Point, Rect};

/// Most pixels a word starts from its place.
pub const MAX_SNAP: f32 = 8.0;
/// A new word keeps the field awake this long, while it pulses and relaxes.
pub const AWAKE_SECS: f64 = 1.6;

/// The magnetic caret's look (`Element::magnetic_caret`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Magnet {
    /// Colour of the field lines.
    pub color: Color,
    /// Field lines on each side of the caret.
    pub lines: u32,
    /// Reach of the outermost line from the caret, in ems.
    pub reach: f32,
    /// How far right a new word starts, in pixels (at most [`MAX_SNAP`]).
    pub snap: f32,
    /// Seconds a new word takes to snap into place.
    pub snap_secs: f64,
    /// Line width in pixels.
    pub width: f32,
    /// Share of the field's opacity drawn on the text side (left of the caret).
    pub text_side: f32,
}

impl Default for Magnet {
    fn default() -> Magnet {
        Magnet {
            color: Color::hex(0x4da3ff, 1.0),
            lines: 6,
            reach: 1.9,
            snap: 5.0,
            snap_secs: 0.11,
            width: 1.0,
            text_side: 0.45,
        }
    }
}

/// What drives the field at one time: strength 0 to 1, the share relaxed into rings, the
/// pulse of the newest word 0 to 1, and how far the light along the lines has run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldState {
    pub strength: f32,
    pub relax: f32,
    pub pulse: f32,
    pub phase: f32,
}

impl FieldState {
    /// The still form: rings, no light running.
    pub const RINGS: FieldState = FieldState { strength: 0.0, relax: 1.0, pulse: 0.0, phase: 0.0 };
}

impl Magnet {
    /// The same magnet in `color`.
    pub fn color(self, color: Color) -> Magnet {
        Magnet { color, ..self }
    }

    /// A word's offset along its line `tau` seconds after it arrived: from `snap` pixels
    /// right, pulled in faster and faster, past its place by a fifth and back.
    pub fn snap_offset(&self, tau: f64) -> f32 {
        let s = self.snap.clamp(0.0, MAX_SNAP);
        let d = self.snap_secs.max(1e-3);
        if tau <= 0.0 {
            return s;
        }
        if tau < d {
            let u = (tau / d) as f32;
            return s * (1.0 - u * u * u);
        }
        let click = 0.06;
        let k = (tau - d) / click;
        if k >= 1.0 - 1e-9 {
            return 0.0;
        }
        -0.2 * s * (std::f64::consts::PI * k).sin() as f32
    }

    /// When a word that arrived at `at` is in place.
    pub fn snapped_at(&self, at: f64) -> f64 {
        at + self.snap_secs.max(1e-3) + 0.06
    }

    /// The field at `t`, from the voice on `signals` (if any) and the time the last word
    /// arrived.
    pub fn state(&self, signals: Option<&Signals>, arrived: f64, t: f64) -> FieldState {
        let (level, lasting) = match signals {
            Some(s) if !s.is_empty() => (s.envelope(Sig::Level, 0.02, 0.25, t), s.envelope(Sig::Level, 0.02, 0.9, t)),
            _ => (0.0, 0.0),
        };
        let since = (t - arrived).max(0.0);
        let pulse = if arrived.is_finite() { (-since / 0.25).exp() as f32 } else { 0.0 };
        // Words hold the field open for a while even with no voice features.
        let held = if arrived.is_finite() { (1.0 - since / AWAKE_SECS).clamp(0.0, 1.0) as f32 } else { 0.0 };
        let strength = (level * 1.4).max(0.35 * held + 0.5 * pulse).min(1.0);
        let awake = (lasting * 5.0).max(held * 1.6).min(1.0);
        let relax = 1.0 - smooth(awake);
        // The light runs at a steady 0.9 cycles a second; the strength sets how bright it is.
        let phase = (0.9 * t).fract() as f32;
        FieldState { strength, relax, pulse, phase }
    }

    /// Line segments that draw the field about the caret bar `bar` for an em of `size`:
    /// each one's ends and its opacity factor (0 to 1, before the colour's own alpha).
    pub fn segments(&self, bar: Rect, size: f32, f: FieldState) -> Vec<(Point, Point, f32)> {
        const STEPS: usize = 18;
        let n = self.lines.max(1);
        let centre = Point::new(bar.x + 0.5 * bar.w, bar.y + 0.5 * bar.h);
        let half = 0.5 * bar.h;
        let reach = self.reach * size * (1.12 - 0.3 * f.strength);
        let mut out = Vec::with_capacity(2 * n as usize * STEPS);
        for i in 0..n {
            let share = if n == 1 { 1.0 } else { i as f32 / (n - 1) as f32 };
            let k = reach * (0.38 + 0.62 * share);
            let ring = half * 1.25 + (reach - half * 1.25) * (0.15 + 0.85 * share);
            // The line leaves the bar at its tip: k sin²θ = half.
            let start = (half / k).clamp(0.0, 1.0).sqrt().asin() * (1.0 - f.relax);
            let span = std::f32::consts::PI - 2.0 * start;
            let base = 0.16 + 0.45 * f.strength + 0.3 * f.pulse;
            let base = base * (1.0 - 0.45 * share) * (1.0 - f.relax) + f.relax * 0.2 * (1.0 - 0.6 * share);
            for side in [1.0f32, -1.0] {
                let side_k = if side > 0.0 { 1.0 } else { self.text_side };
                let at = |j: usize| {
                    let th = start + span * j as f32 / STEPS as f32;
                    let s = th.sin();
                    let r = (1.0 - f.relax) * k * s * s + f.relax * ring;
                    Point::new(centre.x + side * r * s, centre.y - r * th.cos())
                };
                for j in 0..STEPS {
                    let u = (j as f32 + 0.5) / STEPS as f32;
                    // 0 at the equator, 1 at the poles; the light runs toward the poles.
                    let toward = (2.0 * u - 1.0).abs();
                    let light = 0.6 + 0.4 * (std::f32::consts::TAU * (2.0 * toward + f.phase)).cos();
                    let light = 1.0 + (light - 1.0) * (1.0 - f.relax);
                    // The outer ends of a line taper where it meets the bar.
                    let taper = 1.0 - (1.0 - f.relax) * toward.powi(6) * 0.7;
                    let a = base * side_k * light * taper;
                    if a > 0.004 {
                        out.push((at(j), at(j + 1), a.min(1.0)));
                    }
                }
            }
        }
        out
    }

    /// The field as shapes, in the space `bar` is in, scaled by `z` about `origin`.
    pub fn shapes(&self, bar: Rect, size: f32, f: FieldState, alpha: f32, origin: Point, z: f32) -> Vec<ShapeInstance> {
        let map = |p: Point| Point::new(origin.x + p.x * z, origin.y + p.y * z);
        self.segments(bar, size, f)
            .into_iter()
            .map(|(a, b, k)| {
                ShapeInstance::line(map(a), map(b), self.width * z, Paint::solid(self.color.fade(alpha * k)))
            })
            .collect()
    }
}

impl super::Flow {
    /// The magnetic caret's field at `t`: the magnet, the caret bar in the text box's space,
    /// and the field's state. Still rings under reduced motion.
    pub(crate) fn magnet_field(&self, signals: Option<&Signals>, t: f64) -> Option<(Magnet, Rect, FieldState)> {
        let m = self.magnet?;
        let (bar, ..) = self.caret(t)?;
        let f = if self.magnet_still { FieldState::RINGS } else { m.state(signals, self.arrived, t) };
        Some((m, bar, f))
    }

    /// Whether the field still moves at `t`.
    pub(crate) fn magnet_moving(&self, signals: Option<&Signals>, t: f64) -> bool {
        if self.magnet.is_none() || self.magnet_still || self.caret.is_none() {
            return false;
        }
        let voice = signals.and_then(|s| s.active_until(4.5)).is_some_and(|until| t < until);
        voice || t < self.arrived + AWAKE_SECS + 0.2
    }

    /// The snap of word `w` along its line at `t`, in pixels.
    pub(super) fn magnet_snap(&self, w: &super::FlowWord, t: f64) -> f32 {
        match self.magnet {
            Some(m) if !self.magnet_still => {
                let born = w.glyphs.iter().map(|g| g.born).fold(f64::INFINITY, f64::min);
                if born.is_finite() {
                    m.snap_offset(t - born)
                } else {
                    0.0
                }
            }
            _ => 0.0,
        }
    }

    /// When the last word's snap ends.
    pub(crate) fn magnet_settled_at(&self) -> f64 {
        match self.magnet {
            Some(m) if !self.magnet_still => self
                .words
                .iter()
                .map(|w| w.glyphs.iter().map(|g| g.born).fold(f64::INFINITY, f64::min))
                .filter(|b| b.is_finite())
                .map(|b| m.snapped_at(b))
                .fold(f64::NEG_INFINITY, f64::max),
            _ => f64::NEG_INFINITY,
        }
    }
}

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vo_core::signals::SignalFrame;

    fn voice(db: f32, from: f64, to: f64) -> Signals {
        let mut s = Signals::new();
        let mut c = from;
        while c < to {
            s.push(SignalFrame { rms_dbfs: db, ..SignalFrame::silent(c) }, c);
            c += 0.01;
        }
        s
    }

    #[test]
    fn a_word_snaps_from_the_right_and_clicks_into_place() {
        let m = Magnet::default();
        assert_eq!(m.snap_offset(0.0), m.snap);
        assert!(m.snap_offset(0.5 * m.snap_secs) > 0.8 * m.snap, "slow start");
        assert!(m.snap_offset(m.snap_secs + 0.03) < 0.0, "a click past its place");
        assert_eq!(m.snap_offset(m.snapped_at(0.0)), 0.0);
        let big = Magnet { snap: 50.0, ..m };
        assert_eq!(big.snap_offset(0.0), MAX_SNAP);
    }

    #[test]
    fn strength_follows_the_voice_and_silence_relaxes_into_rings() {
        let m = Magnet::default();
        let loud = voice(-8.0, 0.0, 2.0);
        let soft = voice(-40.0, 0.0, 2.0);
        let a = m.state(Some(&loud), f64::NEG_INFINITY, 1.5);
        let b = m.state(Some(&soft), f64::NEG_INFINITY, 1.5);
        assert!(a.strength > b.strength + 0.3, "{a:?} {b:?}");
        assert!(a.relax < 0.05);
        // Long after the voice stopped: rings.
        let quiet = m.state(Some(&loud), f64::NEG_INFINITY, 6.0);
        assert!(quiet.relax > 0.99, "{quiet:?}");
        // No voice features: a word holds the field open, then it relaxes.
        let w = m.state(None, 1.0, 1.1);
        assert!(w.relax < 0.1 && w.strength > 0.3, "{w:?}");
        assert!(m.state(None, 1.0, 1.0 + AWAKE_SECS + 0.1).relax > 0.99);
    }

    #[test]
    fn relaxed_lines_are_concentric_rings() {
        let m = Magnet::default();
        let bar = Rect::new(100.0, 40.0, 2.0, 20.0);
        let c = Point::new(101.0, 50.0);
        let segs = m.segments(bar, 20.0, FieldState::RINGS);
        assert!(!segs.is_empty());
        let mut radii: Vec<f32> = Vec::new();
        for (a, _, _) in &segs {
            let r = (a.x - c.x).hypot(a.y - c.y);
            if !radii.iter().any(|q| (q - r).abs() < 0.01) {
                radii.push(r);
            }
        }
        assert_eq!(radii.len(), m.lines as usize, "one radius per line: {radii:?}");
    }

    #[test]
    fn active_lines_are_dipole_loops_that_leave_the_bar_tips() {
        let m = Magnet::default();
        let bar = Rect::new(100.0, 40.0, 2.0, 20.0);
        let f = FieldState { strength: 0.8, relax: 0.0, pulse: 0.0, phase: 0.0 };
        let segs = m.segments(bar, 20.0, f);
        // Every point lies on r = k sin²θ for its line, so the loop passes the bar's tips.
        let first = segs[0].0;
        let (dx, dy) = (first.x - 101.0, first.y - 50.0);
        assert!((dx.hypot(dy) - 10.0).abs() < 0.5, "starts at the tip, r = {}", dx.hypot(dy));
        // The right side is brighter than the text side.
        let right: f32 = segs.iter().filter(|s| s.0.x > 101.5).map(|s| s.2).sum();
        let left: f32 = segs.iter().filter(|s| s.0.x < 100.5).map(|s| s.2).sum();
        assert!(right > 1.5 * left);
    }
}
