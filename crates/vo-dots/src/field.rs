//! A field of dots, each with its own springs for position, size, brightness and colour
//! (VE05).
//!
//! The field is plain data: every channel is a closed-form spring segment, so
//! [`DotField::sample`] is a pure function of `t`, like the rest of the scene. An app keeps
//! the field in an `Arc`, changes it in `update` at the message's time with
//! `Arc::make_mut`, and hands the `Arc` to `Element::dots`. A change makes a new snapshot
//! (about 1 ms per 50 000 dots to copy); a frame only reads.
//!
//! Every retarget can start later than `t` (a stagger delay). Until then the dot keeps
//! following the segment it was on, so a dot never jumps when a new target arrives during
//! its delay.

use std::collections::HashMap;

use vo_core::spring::Spring;
use vo_scene::dots::{Dot, DotSource};
use vo_scene::geom::Color;

/// What a dot moves toward.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// Grid cells; fractional cells are fine.
    pub x: f32,
    pub y: f32,
    /// Diameter as a fraction of the pitch.
    pub size: f32,
    /// Multiplies the colour's alpha. 0 hides the dot.
    pub bright: f32,
    /// Straight alpha sRGB.
    pub color: Color,
}

impl Look {
    pub fn new(x: f32, y: f32, size: f32, color: Color) -> Look {
        Look { x, y, size, bright: 1.0, color }
    }

    /// The same look at another cell.
    pub fn at(self, x: f32, y: f32) -> Look {
        Look { x, y, ..self }
    }

    /// The same look, hidden: brightness and size 0.
    pub fn hidden(self) -> Look {
        Look { bright: 0.0, size: 0.0, ..self }
    }
}

/// Springs for each kind of channel. Every dot in a field shares them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Springs {
    pub pos: Spring,
    pub size: Spring,
    pub bright: Spring,
    pub color: Spring,
}

impl Default for Springs {
    fn default() -> Springs {
        Springs { pos: Spring::SOFT, size: Spring::SNAP, bright: Spring::SNAP, color: Spring::SOFT }
    }
}

/// A channel is at rest when it is this close to its target, in its own unit (cells, pitch
/// fractions, brightness, colour mix), and moves slower than ten times that per second.
const REST: f64 = 1e-3;

/// One spring segment. `t0` is seconds after the field's epoch, so the segment stays small.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Seg {
    t0: f32,
    x0: f32,
    v0: f32,
    target: f32,
}

impl Seg {
    fn at_rest(t0: f32, x: f32) -> Seg {
        Seg { t0, x0: x, v0: 0.0, target: x }
    }

    fn eval(&self, spring: &Spring, t: f32) -> (f32, f32) {
        let dt = (t - self.t0).max(0.0) as f64;
        if self.x0 == self.target && self.v0 == 0.0 {
            return (self.target, 0.0);
        }
        let (y, v) = spring.eval((self.x0 - self.target) as f64, self.v0 as f64, dt);
        (self.target + y as f32, v as f32)
    }

    /// Seconds after `t0` when the segment is at rest for good.
    fn settle(&self, spring: &Spring) -> f32 {
        spring.settle_time((self.x0 - self.target) as f64, self.v0 as f64, REST, 10.0 * REST) as f32
    }
}

/// A channel: the segment it is on, and the one it was on before `cur.t0`, for a retarget
/// that starts after a delay.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Chan {
    pre: Seg,
    cur: Seg,
}

impl Chan {
    fn new(t0: f32, x: f32) -> Chan {
        let s = Seg::at_rest(t0, x);
        Chan { pre: s, cur: s }
    }

    fn eval(&self, spring: &Spring, t: f32) -> (f32, f32) {
        if t < self.cur.t0 {
            self.pre.eval(spring, t)
        } else {
            self.cur.eval(spring, t)
        }
    }

    /// Retargets at `now`, with the motion starting at `start >= now`. Returns when the new
    /// segment comes to rest.
    fn retarget(&mut self, spring: &Spring, now: f32, start: f32, target: f32) -> f32 {
        // A segment that has not started yet is dropped: until `start` the channel stays on
        // the one it is on at `now`.
        if self.cur.t0 > now {
            self.cur = self.pre;
        }
        if self.cur.target == target && start <= now {
            return self.cur.t0 + self.cur.settle(spring);
        }
        let (x, v) = self.cur.eval(spring, start);
        self.pre = self.cur;
        self.cur = Seg { t0: start, x0: x, v0: v, target };
        start + self.cur.settle(spring)
    }
}

const X: usize = 0;
const Y: usize = 1;
const SIZE: usize = 2;
const BRIGHT: usize = 3;
/// Colour mix from `from` (0) to `to` (1).
const MIX: usize = 4;

#[derive(Clone, Debug, PartialEq)]
struct State {
    id: u32,
    ch: [Chan; 5],
    from: Color,
    to: Color,
    /// Seconds after the epoch when every channel is at rest.
    rest: f32,
    /// Position channels at rest, so the dot may be snapped to pixels.
    pos_rest: f32,
}

/// A field of dots keyed by `u32` ids. See the module notes.
#[derive(Clone, Debug)]
pub struct DotField {
    springs: Springs,
    epoch: f64,
    dots: Vec<State>,
    index: HashMap<u32, usize>,
    /// The latest `rest` of any dot.
    rest: f32,
}

impl DotField {
    /// An empty field. `epoch` is any scene time near where the field is used; times are kept
    /// relative to it in `f32`.
    pub fn new(epoch: f64, springs: Springs) -> DotField {
        DotField { springs, epoch, dots: Vec::new(), index: HashMap::new(), rest: 0.0 }
    }

    pub fn len(&self) -> usize {
        self.dots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dots.is_empty()
    }

    pub fn contains(&self, id: u32) -> bool {
        self.index.contains_key(&id)
    }

    pub fn springs(&self) -> Springs {
        self.springs
    }

    fn rel(&self, t: f64) -> f32 {
        (t - self.epoch) as f32
    }

    /// Adds dot `id` at rest with look `look` at `t`. An existing dot jumps there.
    pub fn place(&mut self, t: f64, id: u32, look: Look) {
        let t0 = self.rel(t);
        let ch = [
            Chan::new(t0, look.x),
            Chan::new(t0, look.y),
            Chan::new(t0, look.size),
            Chan::new(t0, look.bright),
            Chan::new(t0, 1.0),
        ];
        let s = State { id, ch, from: look.color, to: look.color, rest: t0, pos_rest: t0 };
        match self.index.get(&id) {
            Some(&i) => self.dots[i] = s,
            None => {
                self.index.insert(id, self.dots.len());
                self.dots.push(s);
            }
        }
    }

    /// Moves dot `id` toward `look`, starting `delay` seconds after `t`. A dot that does not
    /// exist yet starts at `from` (at rest) at `t`.
    pub fn retarget(&mut self, t: f64, id: u32, look: Look, delay: f64, from: Option<Look>) {
        if !self.index.contains_key(&id) {
            self.place(t, id, from.unwrap_or(look));
        }
        let (now, start) = (self.rel(t), self.rel(t + delay.max(0.0)));
        let sp = self.springs;
        let i = self.index[&id];
        let s = &mut self.dots[i];
        let mut rest = now;
        let px = s.ch[X].retarget(&sp.pos, now, start, look.x);
        let py = s.ch[Y].retarget(&sp.pos, now, start, look.y);
        s.pos_rest = px.max(py);
        rest = rest.max(s.pos_rest);
        rest = rest.max(s.ch[SIZE].retarget(&sp.size, now, start, look.size));
        rest = rest.max(s.ch[BRIGHT].retarget(&sp.bright, now, start, look.bright));
        if look.color != s.to {
            // Restart the mix from the colour the dot shows when the change starts.
            let (k, _) = s.ch[MIX].eval(&sp.color, start);
            s.from = mix(s.from, s.to, k);
            s.to = look.color;
            s.ch[MIX] = Chan { pre: s.ch[MIX].cur, cur: Seg::at_rest(start, 0.0) };
            rest = rest.max(s.ch[MIX].retarget(&sp.color, start, start, 1.0));
        } else {
            rest = rest.max(s.ch[MIX].cur.t0 + s.ch[MIX].cur.settle(&sp.color));
        }
        s.rest = rest;
        self.rest = self.rest.max(rest);
    }

    /// The look dot `id` moves toward, or `None` when there is no such dot.
    pub fn target(&self, id: u32) -> Option<Look> {
        let s = &self.dots[*self.index.get(&id)?];
        Some(Look {
            x: s.ch[X].cur.target,
            y: s.ch[Y].cur.target,
            size: s.ch[SIZE].cur.target,
            bright: s.ch[BRIGHT].cur.target,
            color: s.to,
        })
    }

    /// The look of dot `id` at `t`.
    pub fn look(&self, t: f64, id: u32) -> Option<Look> {
        let s = &self.dots[*self.index.get(&id)?];
        Some(self.state_look(s, self.rel(t)).0)
    }

    /// Ids in the order they were added.
    pub fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.dots.iter().map(|s| s.id)
    }

    /// Drops every dot that is hidden (target brightness or size 0) and at rest at `t`.
    pub fn prune(&mut self, t: f64) {
        let now = self.rel(t);
        let before = self.dots.len();
        self.dots.retain(|s| !(s.rest <= now && (s.ch[BRIGHT].cur.target <= 0.0 || s.ch[SIZE].cur.target <= 0.0)));
        if self.dots.len() != before {
            self.reindex();
        }
    }

    /// Drops dot `id` at once.
    pub fn remove(&mut self, id: u32) {
        if self.index.remove(&id).is_some() {
            self.dots.retain(|s| s.id != id);
            self.reindex();
        }
    }

    fn reindex(&mut self) {
        self.index.clear();
        for (i, s) in self.dots.iter().enumerate() {
            self.index.insert(s.id, i);
        }
    }

    /// The time from which nothing in the field moves any more.
    pub fn rest_at(&self) -> f64 {
        self.epoch + self.rest as f64
    }

    fn state_look(&self, s: &State, t: f32) -> (Look, f32) {
        let sp = &self.springs;
        if t >= s.rest {
            let look = Look {
                x: s.ch[X].cur.target,
                y: s.ch[Y].cur.target,
                size: s.ch[SIZE].cur.target,
                bright: s.ch[BRIGHT].cur.target,
                color: s.to,
            };
            return (look, 1.0);
        }
        let (x, vx) = s.ch[X].eval(&sp.pos, t);
        let (y, vy) = s.ch[Y].eval(&sp.pos, t);
        let (size, _) = s.ch[SIZE].eval(&sp.size, t);
        let (bright, _) = s.ch[BRIGHT].eval(&sp.bright, t);
        let (k, _) = s.ch[MIX].eval(&sp.color, t);
        // Snapping fades in as the dot slows down near its cell, so it never pops.
        let snap = if t >= s.pos_rest { 1.0 } else { (1.0 - 0.5 * vx.hypot(vy)).clamp(0.0, 1.0) };
        let look = Look { x, y, size: size.max(0.0), bright: bright.clamp(0.0, 1.0), color: mix(s.from, s.to, k) };
        (look, snap)
    }

    /// Appends the dots at `t` in grid cells.
    pub fn sample(&self, t: f64, out: &mut Vec<Dot>) {
        let now = self.rel(t);
        out.reserve(self.dots.len());
        for s in &self.dots {
            let (l, snap) = self.state_look(s, now);
            let a = l.color.a * l.bright;
            if a <= 0.0 || l.size <= 0.0 {
                continue;
            }
            out.push(Dot { x: l.x, y: l.y, size: l.size, color: Color { a, ..l.color }, snap });
        }
    }
}

impl DotSource for DotField {
    fn dots(&self, t: f64, out: &mut Vec<Dot>) {
        self.sample(t, out);
    }

    fn moving(&self, t: f64) -> bool {
        self.rel(t) < self.rest
    }
}

/// Straight-alpha mix of two colours, `k` 0 to 1 (clamped so an overshoot keeps the hue).
pub fn mix(a: Color, b: Color, k: f32) -> Color {
    let k = k.clamp(0.0, 1.0);
    let m = |x: f32, y: f32| x + (y - x) * k;
    Color::rgba(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b), m(a.a, b.a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white(x: f32, y: f32) -> Look {
        Look::new(x, y, 0.6, Color::WHITE)
    }

    fn sample(f: &DotField, t: f64) -> Vec<Dot> {
        let mut v = Vec::new();
        f.sample(t, &mut v);
        v
    }

    #[test]
    fn a_dot_springs_to_its_target_and_rests() {
        let mut f = DotField::new(0.0, Springs::default());
        f.place(0.0, 1, white(0.0, 0.0));
        assert!(!f.moving(0.0));
        f.retarget(1.0, 1, white(10.0, 2.0), 0.0, None);
        assert!(f.moving(1.1));
        let mid = sample(&f, 1.1)[0];
        assert!(mid.x > 0.0 && mid.x < 10.0 && mid.snap < 1.0, "{mid:?}");
        let end = f.rest_at();
        assert!(end > 1.1 && end < 2.0, "{end}");
        let rest = sample(&f, end)[0];
        assert!((rest.x - 10.0).abs() < 1e-3 && rest.snap == 1.0, "{rest:?}");
        assert!(!f.moving(end));
    }

    #[test]
    fn a_delayed_retarget_holds_the_current_motion_until_it_starts() {
        let mut f = DotField::new(0.0, Springs::default());
        f.place(0.0, 1, white(0.0, 0.0));
        f.retarget(0.0, 1, white(10.0, 0.0), 0.0, None);
        let before = f.clone();
        // A new target at 0.05 s that starts 0.1 s later.
        f.retarget(0.05, 1, white(-5.0, 0.0), 0.1, None);
        for t in [0.05, 0.1, 0.149] {
            assert_eq!(sample(&f, t), sample(&before, t), "at {t}");
        }
        // Continuous at the start of the new segment.
        let (a, b) = (sample(&f, 0.149_999)[0].x, sample(&f, 0.150_001)[0].x);
        assert!((a - b).abs() < 1e-3, "{a} {b}");
        // A second retarget during the delay replaces the pending one without a jump.
        f.retarget(0.12, 1, white(3.0, 0.0), 0.0, None);
        assert!((sample(&f, 0.12)[0].x - sample(&before, 0.12)[0].x).abs() < 1e-4);
    }

    #[test]
    fn colour_changes_blend_from_what_is_shown() {
        let mut f = DotField::new(0.0, Springs::default());
        let red = Color::rgba(1.0, 0.0, 0.0, 1.0);
        f.place(0.0, 1, white(0.0, 0.0));
        f.retarget(0.0, 1, Look { color: red, ..white(0.0, 0.0) }, 0.0, None);
        let mid = sample(&f, 0.05)[0].color;
        assert!(mid.g < 1.0 && mid.g > 0.0);
        f.retarget(0.05, 1, white(0.0, 0.0), 0.0, None);
        let just_after = sample(&f, 0.050_001)[0].color;
        assert!((just_after.g - mid.g).abs() < 1e-3, "{mid:?} {just_after:?}");
        assert_eq!(sample(&f, f.rest_at())[0].color, Color::WHITE);
    }

    #[test]
    fn hidden_dots_are_pruned_once_at_rest() {
        let mut f = DotField::new(0.0, Springs::default());
        f.place(0.0, 1, white(0.0, 0.0));
        f.place(0.0, 2, white(1.0, 0.0));
        f.retarget(0.0, 1, white(0.0, 0.0).hidden(), 0.0, None);
        f.prune(0.05);
        assert_eq!(f.len(), 2);
        f.prune(f.rest_at());
        assert_eq!(f.ids().collect::<Vec<_>>(), vec![2]);
        assert_eq!(f.target(2).unwrap().x, 1.0);
    }

    #[test]
    fn sampling_is_pure() {
        let mut f = DotField::new(5.0, Springs::default());
        for i in 0..100 {
            f.retarget(5.0, i, white(i as f32, 0.0), i as f64 * 0.01, Some(white(0.0, 3.0)));
        }
        assert_eq!(sample(&f, 5.3), sample(&f, 5.3));
        assert_eq!(sample(&f, 5.3), sample(&f.clone(), 5.3));
    }
}
