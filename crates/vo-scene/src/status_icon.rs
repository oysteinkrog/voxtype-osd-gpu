//! The dictation status icon as one continuous shape: a record dot becomes a stop square,
//! the square rounds into a ring that opens into a spinner, the spinner closes back into a
//! ring and a check draws on inside it. Cancel turns the dot into an X.
//!
//! Built on [`Story`]: each event adds a labelled step that starts from the shape on screen,
//! with the velocities it had. When "done" arrives mid-spin, the spinner's tail and head and
//! their speeds become the start of the two springs that close the ring, so the arc never
//! jumps or runs backwards.
//!
//! ```ignore
//! let mut icon = StatusIcon::new(IconStyle::default());
//! icon.record(0.0);
//! icon.stop(2.0);      // finishing: the dot becomes a stop square
//! icon.finish(2.3);    // the square becomes a spinner
//! icon.done(3.9);      // the ring closes, then the check draws on
//! icon.paint(t, centre, 48.0, painter);
//! ```

use std::f64::consts::{FRAC_PI_2, TAU};
use std::sync::Arc;

use vo_core::spring::Spring;

use crate::canvas::Painter;
use crate::display::{Paint, ShapeInstance};
use crate::geom::{Affine, Color, Point, RRect, Rect};
use crate::path::{icons, make, Outline, StrokeStyle};
use crate::story::{Ch, Phase, Story};

/// Sizes are fractions of the icon's box side.
const DOT: f64 = 0.30;
const SQUARE: f64 = 0.22;
const SQUARE_CORNER: f64 = 0.25;
const RING: f64 = 0.36;

/// Colours and stroke width of a [`StatusIcon`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconStyle {
    /// The record dot.
    pub record: Color,
    /// The stop square and the spinner.
    pub work: Color,
    /// The closed ring and the check.
    pub done: Color,
    /// The X.
    pub cancel: Color,
    /// Ring, check and X stroke width as a fraction of the box side.
    pub width: f32,
    /// Seconds from `done` until the check is fully drawn. `None` uses the natural timing
    /// (about 0.6 s, depending on where the spinner was); a value scales the ring springs and
    /// the check draw to fit, for a popup that closes soon after it is done.
    pub done_secs: Option<f32>,
}

impl Default for IconStyle {
    fn default() -> IconStyle {
        IconStyle {
            record: Color::hex(0xef4444, 1.0),
            work: Color::hex(0xe5e7eb, 1.0),
            done: Color::hex(0x4ade80, 1.0),
            cancel: Color::hex(0x9ca3af, 1.0),
            width: 0.08,
            done_secs: None,
        }
    }
}

/// Everything that moves in the icon. Angles are in turns, clockwise from 12 o'clock and never
/// wrapped.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct IconState {
    /// Half the side of the solid shape.
    pub half: Ch,
    /// Corner radius as a fraction of `half`: 1 is a circle.
    pub corner: Ch,
    /// Opacity of the solid shape.
    pub solid: Ch,
    /// The ring's tail and head.
    pub tail: Ch,
    pub head: Ch,
    /// Opacity of the ring.
    pub ring: Ch,
    /// How much of the check is drawn, 0 to 1.
    pub check: f32,
    /// How much of the X is drawn, 0 to 1.
    pub cross: f32,
    /// RGBA.
    pub color: [Ch; 4],
}

impl IconState {
    fn colour(&self) -> Color {
        let c = self.color.map(|c| c.x.clamp(0.0, 1.0) as f32);
        Color::rgba(c[0], c[1], c[2], c[3])
    }

    fn tint(mut self, to: Color, spring: Spring, dt: f64) -> IconState {
        let to = [to.r, to.g, to.b, to.a];
        for (c, t) in self.color.iter_mut().zip(to) {
            *c = c.to(t as f64, spring, dt);
        }
        self
    }
}

/// The status icon story. Events go in with [`StatusIcon::record`], [`StatusIcon::stop`] and
/// so on; [`StatusIcon::paint`] draws it at any time.
#[derive(Clone, Debug)]
pub struct StatusIcon {
    story: Story<IconState>,
    style: IconStyle,
    check: Arc<Outline>,
    cross: Arc<Outline>,
    /// When the last step stops moving: `INFINITY` while recording or spinning.
    until: f64,
}

impl StatusIcon {
    pub fn new(style: IconStyle) -> StatusIcon {
        let c = style.record;
        let initial = IconState {
            corner: Ch::at(1.0),
            tail: Ch::at(0.0),
            head: Ch::at(1.0),
            color: [c.r, c.g, c.b, c.a].map(|v| Ch::at(v as f64)),
            ..IconState::default()
        };
        StatusIcon {
            story: Story::new(initial),
            style,
            check: Arc::new(Outline::new(&icons::check(), 0.01)),
            cross: Arc::new(Outline::new(&icons::x(), 0.01)),
            until: 0.0,
        }
    }

    pub fn style(&self) -> &IconStyle {
        &self.style
    }

    /// The labelled steps so far, as `(label, start)`.
    pub fn story(&self) -> &Story<IconState> {
        &self.story
    }

    /// The state at `t`.
    pub fn state(&self, t: f64) -> Phase<'_, IconState> {
        self.story.state(t)
    }

    /// When the icon stops moving (`INFINITY` while it pulses or spins).
    pub fn until(&self) -> f64 {
        self.until
    }

    /// Recording: the dot pops in and breathes.
    pub fn record(&mut self, t: f64) {
        let c = self.style.record;
        self.story.then("record", t, move |e: &IconState, dt: f64| {
            let mut s = *e;
            s.half = e.half.to(DOT, Spring::POP, dt);
            s.corner = e.corner.to(1.0, Spring::SNAP, dt);
            s.solid = e.solid.to(1.0, Spring::SNAP, dt);
            s.ring = e.ring.to(0.0, Spring::SNAP, dt);
            // A slow breath that starts at rest, so the pop hands over without a kick.
            let w = TAU / 1.6;
            s.half.x -= 0.025 * (1.0 - (w * dt).cos());
            s.half.v -= 0.025 * w * (w * dt).sin();
            s.tint(c, Spring::SNAP, dt)
        });
        self.until = f64::INFINITY;
    }

    /// Recording stopped: the dot becomes a stop square.
    pub fn stop(&mut self, t: f64) {
        let c = self.style.work;
        let at = self.story.then("stop", t, move |e: &IconState, dt: f64| {
            let mut s = *e;
            s.half = e.half.to(SQUARE, Spring::POP, dt);
            s.corner = e.corner.to(SQUARE_CORNER, Spring::SOFT, dt);
            s.solid = e.solid.to(1.0, Spring::SNAP, dt);
            s.tint(c, Spring::SOFT, dt)
        });
        self.until = at + 0.6;
    }

    /// Transcribing: the square rounds into a ring that opens into a spinner. The spinner's
    /// head starts where the ring's head is; the gap to the spinner's path closes on a spring.
    pub fn finish(&mut self, t: f64) {
        let c = self.style.work;
        self.story.then("finish", t, move |e: &IconState, dt: f64| {
            let mut s = *e;
            s.half = e.half.to(RING, Spring::SOFT, dt);
            s.corner = e.corner.to(1.0, Spring::SOFT, dt);
            s.solid = e.solid.to(0.0, Spring::SNAP, dt);
            s.ring = e.ring.to(1.0, Spring::SNAP, dt);
            let [tail0, head0] = spinner(0.0);
            let [tail, head] = spinner(dt);
            let off = e.head.x - head0.x;
            let gap = Spring::critical_to_98(0.5);
            let follow = |ch: Ch, from: Ch, to: Ch| {
                let (y, v) = gap.eval(ch.x - (from.x + off), ch.v - from.v, dt);
                Ch { x: to.x + off + y, v: to.v + v }
            };
            s.tail = follow(e.tail, tail0, tail);
            s.head = follow(e.head, head0, head);
            s.tint(c, Spring::SNAP, dt)
        });
        self.until = f64::INFINITY;
    }

    /// Done: the arc closes into a ring at 12 o'clock, then the check draws on. A short
    /// labelled sequence: "close", then "check" once the ring has closed.
    pub fn done(&mut self, t: f64) {
        let c = self.style.done;
        let at = self.story.last_start().map_or(t, |s| t.max(s));
        let e = self.story.state(at).state;
        // Natural timing, scaled to `done_secs` when it is set.
        let k = self.style.done_secs.map_or(1.0, |d| self.done_scale(&e, d as f64));
        let (tail_to, springs, lag, draw, closed) = done_plan(&e, k);
        let (tail_s, head_s) = springs;
        let head_to = tail_to + 1.0;
        let close = move |e: &IconState, dt: f64| {
            let mut s = *e;
            s.half = e.half.to(RING, Spring::SOFT, dt);
            s.corner = e.corner.to(1.0, Spring::SOFT, dt);
            s.solid = e.solid.to(0.0, Spring::SOFT, dt);
            s.ring = e.ring.to(1.0, Spring::SNAP, dt);
            s.tail = e.tail.to(tail_to, tail_s, dt);
            s.head = e.head.to(head_to, head_s, dt);
            s.tint(c, Spring::SOFT, dt)
        };
        let check = move |_: &IconState, dt: f64| {
            let mut s = close(&e, dt + lag);
            let u = (dt / draw).clamp(0.0, 1.0);
            s.check = (1.0 - (1.0 - u).powi(3)) as f32;
            s
        };
        type Boxed = Box<dyn crate::story::Step<IconState>>;
        let steps: [(&'static str, f64, Boxed); 2] = [("close", 0.0, Box::new(close)), ("check", lag, Box::new(check))];
        self.story.sequence(at, steps);
        self.until = at + (lag + draw).max(closed);
    }

    /// The factor that makes a done sequence entered from `e` take `secs` until the check is
    /// drawn. The handed-over velocity makes the time not quite proportional to the factor,
    /// so it is refined a few times.
    fn done_scale(&self, e: &IconState, secs: f64) -> f64 {
        let mut k = 1.0;
        for _ in 0..4 {
            let (_, _, lag, draw, _) = done_plan(e, k);
            k = (k * secs / (lag + draw)).clamp(0.1, 4.0);
        }
        k
    }

    /// Cancelled: whatever is showing shrinks away while an X draws on.
    pub fn cancel(&mut self, t: f64) {
        let c = self.style.cancel;
        let at = self.story.then("cancel", t, move |e: &IconState, dt: f64| {
            let mut s = *e;
            s.half = e.half.to(0.0, Spring::SOFT, dt);
            s.solid = e.solid.to(0.0, Spring::SLOW, dt);
            s.ring = e.ring.to(0.0, Spring::SNAP, dt);
            let u = ((dt - 0.06) / 0.30).clamp(0.0, 1.0);
            s.cross = (1.0 - (1.0 - u).powi(3)) as f32;
            s.tint(c, Spring::SNAP, dt)
        });
        self.until = at + 0.6;
    }

    /// Draws the icon at `t` in a `side` x `side` box centred on `centre`.
    pub fn paint(&self, t: f64, centre: Point, side: f32, p: &mut Painter) {
        let s = self.state(t).state;
        let col = s.colour();
        let d = side as f64;
        let solid = s.solid.x.clamp(0.0, 1.0) as f32;
        let half = (s.half.x.max(0.0) * d) as f32;
        if solid > 0.002 && half > 0.05 {
            let r = half * s.corner.x.clamp(0.0, 1.0) as f32;
            let rect = Rect::new(centre.x - half, centre.y - half, 2.0 * half, 2.0 * half);
            p.shape(ShapeInstance::rrect(RRect::new(rect, r), Paint::solid(col.fade(solid))));
        }
        let width = self.style.width * side;
        let ring = s.ring.x.clamp(0.0, 1.0) as f32;
        let sweep = (s.head.x - s.tail.x).clamp(0.0, 1.0);
        if ring > 0.002 && sweep > 1e-4 {
            let style = StrokeStyle::solid(width, col.fade(ring));
            let r = (RING * d) as f32;
            let start = s.tail.x.rem_euclid(1.0) * TAU - FRAC_PI_2;
            let arc = if sweep >= 0.999 {
                let mut c = make::arc(centre, r, start as f32, std::f32::consts::TAU);
                c.closed = true;
                c
            } else {
                make::arc(centre, r, start as f32, (sweep * TAU) as f32)
            };
            p.stroke(&arc, &style);
        }
        // The check and the X live on a 24 unit grid. The check fits just inside the ring;
        // the X, which has no ring, is drawn larger.
        let mark = |o: &Outline, f: f32, fit: f32, p: &mut Painter| {
            if f <= 0.0 {
                return;
            }
            let k = side * fit / 24.0;
            let place = Affine::scale(k).then(&Affine::translate(centre.x - 12.0 * k, centre.y - 12.0 * k));
            let o = o.transform(&place);
            let n = o.contours.len().max(1) as f32;
            // Strokes draw one after another, overlapping by a third.
            for (i, c) in o.contours.iter().enumerate() {
                let u = ((f * (n + 0.5) - i as f32 * 1.0) / 1.5).clamp(0.0, 1.0);
                if u > 0.0 {
                    p.stroke(&c.trim(0.0, u * c.length()), &StrokeStyle::solid(width, col));
                }
            }
        };
        mark(&self.check, s.check, 0.46, p);
        mark(&self.cross, s.cross, 0.7, p);
    }
}

/// The done sequence entered from `e` with durations scaled by `k`: the tail's target turn,
/// the tail and head springs, when the check starts, how long it draws, and when the ring has
/// closed (seconds after done).
fn done_plan(e: &IconState, k: f64) -> (f64, (Spring, Spring), f64, f64, f64) {
    let (tail_s, head_s) = (Spring::critical_to_98(0.45 * k), Spring::critical_to_98(0.40 * k));
    // A critically damped spring does not overshoot when the target is at least v / omega
    // ahead, so the targets are the next whole turn past that point.
    let lead = |ch: Ch, s: Spring| ch.x + ch.v.max(0.0) / s.omega() + 0.02;
    let mut tail_to = lead(e.tail, tail_s).ceil();
    if lead(e.head, head_s) > tail_to + 1.0 {
        tail_to += 1.0;
    }
    let closed = e.tail.settle(tail_to, tail_s, 0.004).max(e.head.settle(tail_to + 1.0, head_s, 0.004));
    let lag = (closed - 0.08 * k).max(0.12 * k);
    (tail_to, (tail_s, head_s), lag, 0.26 * k, closed)
}

/// The spinner's tail and head in turns from 12 o'clock, with velocities in turns a second.
fn spinner(t: f64) -> [Ch; 2] {
    make::spinner_ends(t).map(|(a, v)| Ch { x: (a + FRAC_PI_2) / TAU, v: v / TAU })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn story() -> StatusIcon {
        let mut icon = StatusIcon::new(IconStyle::default());
        icon.record(0.0);
        icon.stop(1.0);
        icon.finish(1.3);
        icon.done(2.77);
        icon
    }

    fn close(a: Ch, b: Ch, tol: f64) -> bool {
        (a.x - b.x).abs() < tol && (a.v - b.v).abs() < tol * 100.0
    }

    #[test]
    fn every_handoff_is_continuous() {
        let icon = story();
        let starts: Vec<(String, f64)> = icon.story().steps().map(|(l, s)| (l.to_string(), s)).collect();
        for (label, at) in &starts {
            let a = icon.state(at - 1e-7).state;
            let b = icon.state(*at).state;
            for (x, y) in [
                (a.half, b.half),
                (a.corner, b.corner),
                (a.solid, b.solid),
                (a.tail, b.tail),
                (a.head, b.head),
                (a.ring, b.ring),
            ] {
                assert!(close(x, y, 1e-4), "{label} at {at}: {x:?} vs {y:?}");
            }
        }
        let labels: Vec<_> = starts.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["record", "stop", "finish", "close", "check"]);
    }

    #[test]
    fn the_ring_closes_forwards_at_twelve_and_the_check_draws_on() {
        // Done at many points of the spinner's cycle.
        for k in 0..40 {
            let mut icon = StatusIcon::new(IconStyle::default());
            icon.record(0.0);
            icon.stop(1.0);
            icon.finish(1.3);
            let done = 2.0 + k as f64 * 0.037;
            icon.done(done);
            let mut last = icon.state(done).state;
            for i in 1..200 {
                let s = icon.state(done + i as f64 * 0.005).state;
                assert!(
                    s.tail.x >= last.tail.x - 1e-9 && s.head.x >= last.head.x - 1e-9,
                    "runs backwards at k={k} i={i}"
                );
                last = s;
            }
            let end = icon.state(icon.until() + 0.5).state;
            assert!((end.tail.x - end.tail.x.round()).abs() < 1e-3, "tail at 12 o'clock: {}", end.tail.x);
            assert!((end.head.x - end.tail.x - 1.0).abs() < 1e-3, "a whole ring");
            assert_eq!(end.check, 1.0);
            assert!(icon.until().is_finite());
        }
    }

    #[test]
    fn the_spinner_reports_its_own_velocity() {
        for i in 0..100 {
            let t = 0.031 * i as f64;
            let h = 1e-6;
            let [a0, b0] = spinner(t);
            let [a1, b1] = spinner(t + h);
            assert!(((a1.x - a0.x) / h - a0.v).abs() < 1e-3 && ((b1.x - b0.x) / h - b0.v).abs() < 1e-3, "t={t}");
        }
    }

    #[test]
    fn done_secs_sets_when_the_check_is_drawn() {
        for secs in [0.3f32, 0.45, 1.2] {
            let mut icon = StatusIcon::new(IconStyle { done_secs: Some(secs), ..IconStyle::default() });
            icon.record(0.0);
            icon.stop(1.0);
            icon.finish(1.3);
            icon.done(2.6);
            let at = |t: f64| icon.state(2.6 + t).state.check;
            assert_eq!(at(secs as f64 + 0.02), 1.0, "drawn by {secs}");
            assert!(at(secs as f64 * 0.8) < 1.0, "not long before {secs}");
            assert!(icon.until() <= 2.6 + secs as f64 + 0.05);
        }
    }

    #[test]
    fn cancel_draws_the_x_and_hides_the_rest() {
        let mut icon = StatusIcon::new(IconStyle::default());
        icon.record(0.0);
        icon.cancel(0.8);
        let s = icon.state(icon.until()).state;
        assert_eq!(s.cross, 1.0);
        assert!(s.half.x.abs() < 0.01 && s.solid.x < 0.05);
        assert_eq!(icon.state(1.0).label, "cancel");
    }
}
