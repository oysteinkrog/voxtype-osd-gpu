//! Ambient loops with a rest state (FX-203): a ring that spins, a glow that breathes, dots
//! that light one at a time, a stripe that scrolls.
//!
//! A loop adds a periodic offset to one property while the view declares it. When the view
//! stops declaring it, the loop settles to a declared rest, so the scene still reaches
//! `next_wake == None` and an idle overlay draws zero frames. Two ways to settle:
//!
//! - [`Settle::Spring`]: the offset springs from where it was drawn, with its velocity, to
//!   `rest`. A wrapping loop (spin, marquee) instead coasts to a stop on a whole repeat
//!   ahead, at constant deceleration, so it never turns back.
//! - [`Settle::Finish`]: the loop plays to the end of its current cycle and holds the value it
//!   has there. "Listening..." dots end with all dots lit.
//!
//! Everything is a function of the start and stop times, which come from `apply(view, t)`,
//! so a loop samples the same at any frame rate and for a seek. While it runs, a loop moves
//! in steps of `1 / fps` (30 fps by default, the FX08 cap for ambient motion), so it asks for
//! at most `fps` frames a second.

use crate::channel::{Channel, Rest};
use crate::spring::Spring;

/// Frame rate of ambient loops unless a loop sets its own (FX08).
pub const AMBIENT_FPS: f64 = 30.0;

/// Added to a step count before `floor`, so a sample at a step's own time lands in that step
/// even when `(t - origin) * fps` rounds a hair below the whole number.
const STEP_EPS: f64 = 1e-6;

/// The shape of one cycle. `u` counts cycles; every shape repeats each whole `u`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wave {
    /// `sin(2 pi u)`, from -1 to 1, 0 at the start of a cycle. Bob, sway.
    Sine,
    /// `(1 - cos(2 pi u)) / 2`, from 0 up to 1 at half a cycle and back. Pulse, breathe.
    Swell,
    /// 0 to 1 at half a cycle, straight lines, back to 0.
    Triangle,
    /// 0 to 1 over the cycle, then wraps to 0. Spin and marquee: only for properties where
    /// adding `amp` changes nothing on screen (a full turn, one stripe tile).
    Ramp,
    /// 1 while the cycle position is in `[from, to)`, otherwise 0. Blink, steps.
    Gate { from: f64, to: f64 },
    /// Smooth simplex noise, about -1 to 1, one feature per cycle. Drift, wander.
    Noise { seed: u32 },
}

impl Wave {
    /// The value at `u` cycles.
    pub fn at(&self, u: f64) -> f64 {
        let f = u - u.floor();
        match *self {
            Wave::Sine => (std::f64::consts::TAU * u).sin(),
            Wave::Swell => 0.5 - 0.5 * (std::f64::consts::TAU * u).cos(),
            Wave::Triangle => 1.0 - (2.0 * f - 1.0).abs(),
            Wave::Ramp => f,
            Wave::Gate { from, to } => {
                if f >= from && f < to {
                    1.0
                } else {
                    0.0
                }
            }
            Wave::Noise { seed } => crate::noise::simplex2([u as f32, 0.5], seed, 0.0) as f64,
        }
    }

    /// The slope `d value / d u` at `u`. Steps have none.
    pub fn slope(&self, u: f64) -> f64 {
        let f = u - u.floor();
        match *self {
            Wave::Sine => std::f64::consts::TAU * (std::f64::consts::TAU * u).cos(),
            Wave::Swell => std::f64::consts::PI * (std::f64::consts::TAU * u).sin(),
            Wave::Triangle => {
                if f < 0.5 {
                    2.0
                } else {
                    -2.0
                }
            }
            Wave::Ramp => 1.0,
            Wave::Gate { .. } => 0.0,
            Wave::Noise { .. } => {
                let h = 1e-3;
                (self.at(u + h) - self.at(u - h)) / (2.0 * h)
            }
        }
    }
}

/// How a loop comes to rest when the view stops declaring it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Settle {
    /// Spring from the drawn value and velocity to `rest`.
    Spring(Spring),
    /// Play to the end of the current cycle and hold the value there.
    Finish,
}

/// One ambient loop on one property: `amp * wave(cycles + phase)` added to its value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Loop {
    pub wave: Wave,
    /// In the property's own units: radians for a rotation, logical units for an offset.
    pub amp: f64,
    /// Seconds per cycle.
    pub period: f64,
    /// Cycles added to the clock, for a stagger between siblings.
    pub phase: f64,
    /// Steps per second while running; 0 moves every frame.
    pub fps: f64,
    /// The offset at rest, for [`Settle::Spring`] and for reduced motion.
    pub rest: f64,
    pub settle: Settle,
}

impl Loop {
    pub fn new(wave: Wave, amp: f64, period: f64) -> Loop {
        Loop {
            wave,
            amp,
            period: period.max(1e-3),
            phase: 0.0,
            fps: AMBIENT_FPS,
            rest: 0.0,
            settle: Settle::Spring(Spring::SOFT),
        }
    }

    /// One full turn (2 pi) every `period` seconds, for a rotation. Comes to rest on a whole
    /// turn with the slow spring, so it decelerates.
    pub fn spin(period: f64) -> Loop {
        Loop::new(Wave::Ramp, std::f64::consts::TAU, period).settle(Spring::SLOW)
    }

    /// One turn every `period` seconds for a property counted in turns, such as a rim angle.
    pub fn turns(period: f64) -> Loop {
        Loop::new(Wave::Ramp, 1.0, period).settle(Spring::SLOW)
    }

    /// Up to `amp` and back once a cycle, smooth: a pulse on scale, a breathing glow.
    pub fn pulse(amp: f64, period: f64) -> Loop {
        Loop::new(Wave::Swell, amp, period)
    }

    /// Up and down by `amp` around the value: an idle bob on an offset.
    pub fn bob(amp: f64, period: f64) -> Loop {
        Loop::new(Wave::Sine, amp, period)
    }

    /// Scrolls left by one `tile` every `period` seconds, for a stripe pattern that repeats
    /// every `tile` logical units.
    pub fn marquee(tile: f64, period: f64) -> Loop {
        Loop::new(Wave::Ramp, -tile, period)
    }

    /// Wanders up to about `amp` either way, one change of direction about every `period`
    /// seconds.
    pub fn drift(amp: f64, period: f64, seed: u32) -> Loop {
        Loop::new(Wave::Noise { seed }, amp, period)
    }

    /// 1 while the cycle position is in `[from, to)`: a blink.
    pub fn gate(from: f64, to: f64, period: f64) -> Loop {
        Loop::new(Wave::Gate { from, to }, 1.0, period)
    }

    /// Dot `i` of `n` in a "..." loop on opacity: the cycle has `n + 1` steps, no dot lit in
    /// the first, then one more each step. Finishes its cycle on stop, so all dots end lit,
    /// which is also its reduced-motion look. Put it on an element whose opacity is 0.
    pub fn step_on(i: usize, n: usize, period: f64) -> Loop {
        let from = (i + 1) as f64 / (n + 1) as f64;
        Loop { rest: 1.0, ..Loop::gate(from, 1.0, period).finish() }
    }

    pub fn phase(self, cycles: f64) -> Loop {
        Loop { phase: cycles, ..self }
    }

    pub fn fps(self, fps: f64) -> Loop {
        Loop { fps: fps.max(0.0), ..self }
    }

    pub fn rest(self, rest: f64) -> Loop {
        Loop { rest, ..self }
    }

    /// Settle with this spring.
    pub fn settle(self, spring: Spring) -> Loop {
        Loop { settle: Settle::Spring(spring), ..self }
    }

    /// Settle by playing to the end of the cycle.
    pub fn finish(self) -> Loop {
        Loop { settle: Settle::Finish, ..self }
    }

    /// Whether `amp` is a whole repeat of the property, so the value may wrap.
    pub fn wraps(&self) -> bool {
        self.wave == Wave::Ramp
    }

    /// The step `t` is in, counted from `origin`.
    fn step(&self, origin: f64, t: f64) -> f64 {
        ((t - origin).max(0.0) * self.fps + STEP_EPS).floor()
    }

    /// Cycles at `t` for a loop started at `origin`, held per step.
    fn cycles(&self, origin: f64, t: f64) -> f64 {
        let dt = if self.fps > 0.0 { self.step(origin, t) / self.fps } else { (t - origin).max(0.0) };
        dt / self.period + self.phase
    }

    /// The offset at `t` for a loop started at `origin`.
    pub fn value(&self, origin: f64, t: f64) -> f64 {
        self.amp * self.wave.at(self.cycles(origin, t))
    }

    /// The offset's rate of change at `t`, per second, from the smooth curve (not the steps).
    pub fn velocity(&self, origin: f64, t: f64) -> f64 {
        let u = (t - origin).max(0.0) / self.period + self.phase;
        self.amp * self.wave.slope(u) / self.period
    }

    /// The next time after `t` at which the drawn value can change.
    pub fn next_step(&self, origin: f64, t: f64) -> f64 {
        if self.fps <= 0.0 {
            return t;
        }
        let next = origin + (self.step(origin, t) + 1.0) / self.fps;
        if next > t {
            next
        } else {
            origin + (self.step(origin, t) + 2.0) / self.fps
        }
    }

    /// End of the cycle `t` is in, counted from `origin` (the phase does not move it, so
    /// staggered siblings finish together).
    fn cycle_end(&self, origin: f64, t: f64) -> f64 {
        let k = ((t - origin).max(0.0) / self.period - 1e-9).ceil().max(0.0);
        origin + k * self.period
    }

    /// The value a finishing loop holds: the one just before its cycle ends at `end`.
    fn hold(&self, origin: f64, end: f64) -> f64 {
        let k = ((end - origin) / self.period).round();
        self.amp * self.wave.at(k + self.phase - 1e-9)
    }

    fn spring(&self) -> Spring {
        match self.settle {
            Settle::Spring(s) => s,
            Settle::Finish => Spring::SOFT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Run,
    /// Stopped at the epoch's time; a finishing loop plays on until `finish_at`.
    Stop {
        finish_at: f64,
    },
    /// A wrapping loop stopped at the epoch's time from `x0` with velocity `v0`, slowing at
    /// a constant rate to rest `dist` further on at `until`.
    Coast {
        x0: f64,
        v0: f64,
        dist: f64,
        until: f64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Epoch {
    t: f64,
    spec: Loop,
    origin: f64,
    phase: Phase,
}

/// One loop's history on one property: when it ran, when it stopped, and a residual channel
/// that carries the value across each hand-off so nothing jumps.
///
/// The drawn offset is the loop's own value plus the residual while it runs, and the
/// residual alone once a spring stop has taken over.
#[derive(Clone, Debug)]
pub struct LoopRun {
    epochs: Vec<Epoch>,
    resid: Channel,
}

impl LoopRun {
    /// Starts `spec` at `t`. With `from`, the loop takes over from that value and velocity
    /// (a property already on screen); without, it starts at its own value at `phase`.
    pub fn new(t: f64, spec: Loop, from: Option<(f64, f64)>, rest: Rest, retention: f64) -> LoopRun {
        let mut run = LoopRun {
            epochs: vec![Epoch { t, spec, origin: t, phase: Phase::Run }],
            resid: Channel::new(t, 0.0, spec.spring()).with_rest(rest).with_retention(retention),
        };
        if let Some((x, v)) = from {
            let (r, rv) = run.residual(&spec, t, t, x, v);
            run.resid.restart(t, r, rv, 0.0);
        }
        run
    }

    /// The residual that makes `spec` started at `origin` draw `x` with velocity `v` at `t`.
    fn residual(&self, spec: &Loop, origin: f64, t: f64, x: f64, v: f64) -> (f64, f64) {
        let mut r = x - spec.value(origin, t);
        if spec.wraps() {
            r -= spec.amp * (r / spec.amp).round();
        }
        (r, v - spec.velocity(origin, t))
    }

    fn epoch(&self, t: f64) -> &Epoch {
        &self.epochs[self.epochs.partition_point(|e| e.t <= t).saturating_sub(1)]
    }

    fn last(&self) -> &Epoch {
        self.epochs.last().expect("never empty")
    }

    /// Whether the loop runs from the latest event on.
    pub fn running(&self) -> bool {
        self.last().phase == Phase::Run
    }

    /// The loop the view declared last, running or settling.
    pub fn spec(&self) -> Loop {
        self.last().spec
    }

    /// Declares the loop at `t`: `Some` runs it, `None` lets it settle. Returns whether the
    /// motion changed.
    pub fn set(&mut self, t: f64, spec: Option<Loop>) -> bool {
        let last = *self.last();
        match (last.phase, spec) {
            (Phase::Run, Some(s)) if s == last.spec => false,
            (Phase::Stop { .. } | Phase::Coast { .. }, None) => false,
            (Phase::Stop { finish_at }, Some(s)) if s == last.spec && t < finish_at => {
                // Still finishing its cycle: carry on as if it had never stopped.
                self.push(Epoch { t, spec: s, origin: last.origin, phase: Phase::Run });
                true
            }
            (_, Some(s)) => {
                let (x, v) = (self.value(t), self.velocity(t));
                self.resid.set_spring(s.spring());
                let (r, rv) = self.residual(&s, t, t, x, v);
                self.resid.restart(t, r, rv, 0.0);
                self.push(Epoch { t, spec: s, origin: t, phase: Phase::Run });
                true
            }
            (Phase::Run, None) => {
                let s = last.spec;
                let phase = match s.settle {
                    Settle::Finish => Phase::Stop { finish_at: s.cycle_end(last.origin, t) },
                    Settle::Spring(spring) => {
                        let (x, v) = (self.value(t), self.velocity(t));
                        if s.wraps() && v * s.amp > 0.0 {
                            // Coast at least as far as the spring would carry it, on to the
                            // next whole repeat past that. Constant deceleration over `dist`
                            // takes `2 dist / v`.
                            let past = x + v / spring.omega() - s.rest;
                            let target = s.rest + s.amp * (past / s.amp).ceil();
                            let dist = target - x;
                            Phase::Coast { x0: x, v0: v, dist, until: t + 2.0 * dist / v }
                        } else {
                            self.resid.set_spring(spring);
                            self.resid.restart(t, x, v, s.rest);
                            Phase::Stop { finish_at: t }
                        }
                    }
                };
                self.push(Epoch { t, spec: s, origin: last.origin, phase });
                true
            }
        }
    }

    /// Appends an epoch, replacing any at or after `t`, and drops epochs the residual no
    /// longer needs.
    fn push(&mut self, e: Epoch) {
        let keep = self.epochs.partition_point(|x| x.t < e.t);
        self.epochs.truncate(keep);
        self.epochs.push(e);
        let earliest = self.resid.earliest();
        let drop = self.epochs.partition_point(|x| x.t <= earliest).saturating_sub(1);
        self.epochs.drain(..drop);
    }

    /// The offset at `t`. Pure.
    pub fn value(&self, t: f64) -> f64 {
        let e = self.epoch(t);
        let resid = self.resid.value(t);
        match e.phase {
            Phase::Run => e.spec.value(e.origin, t) + resid,
            Phase::Stop { finish_at } => match e.spec.settle {
                Settle::Spring(_) => resid,
                Settle::Finish if t < finish_at => e.spec.value(e.origin, t) + resid,
                Settle::Finish => e.spec.hold(e.origin, finish_at) + resid,
            },
            Phase::Coast { x0, v0, dist, until } => {
                if t >= until {
                    x0 + dist
                } else {
                    let tau = (t - e.t).max(0.0);
                    x0 + v0 * tau - 0.25 * v0 * v0 * tau * tau / dist
                }
            }
        }
    }

    /// The offset's rate of change at `t`.
    pub fn velocity(&self, t: f64) -> f64 {
        let e = self.epoch(t);
        let resid = self.resid.sample(t).1;
        match e.phase {
            Phase::Run => e.spec.velocity(e.origin, t) + resid,
            Phase::Stop { finish_at } if e.spec.settle == Settle::Finish && t < finish_at => {
                e.spec.velocity(e.origin, t) + resid
            }
            Phase::Stop { .. } => resid,
            Phase::Coast { v0, until, .. } => {
                if t >= until {
                    0.0
                } else {
                    v0 * (1.0 - (t - e.t) / (until - e.t))
                }
            }
        }
    }

    /// When the drawn offset next changes after `t`: `Some(t)` while the residual moves,
    /// the next step while the loop runs, `None` once it rests.
    pub fn wake(&self, t: f64) -> Option<f64> {
        let e = self.epoch(t);
        if let Phase::Coast { until, .. } = e.phase {
            return (t < until).then_some(t);
        }
        if !self.resid.is_settled(t) {
            return Some(t);
        }
        match e.phase {
            Phase::Run => Some(e.spec.next_step(e.origin, t)),
            Phase::Stop { finish_at } if t < finish_at => Some(e.spec.next_step(e.origin, t).min(finish_at)),
            Phase::Stop { .. } | Phase::Coast { .. } => None,
        }
    }

    /// When the loop is at rest for good, unless declared again. Infinite while it runs.
    pub fn rest_at(&self) -> f64 {
        match self.last().phase {
            Phase::Run => f64::INFINITY,
            Phase::Stop { finish_at } => finish_at.max(self.resid.settled_at()),
            Phase::Coast { until, .. } => until,
        }
    }
}

/// An on and off history from view events, for things that run while a state lasts, such as
/// ripples. Each run is `[on, off)`; the last may be open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Toggle {
    runs: Vec<(f64, f64)>,
}

impl Toggle {
    pub fn new(t: f64, on: bool) -> Toggle {
        let mut tg = Toggle::default();
        tg.set(t, on);
        tg
    }

    /// Turns on or off at `t`. Returns whether it changed.
    pub fn set(&mut self, t: f64, on: bool) -> bool {
        let is_on = self.runs.last().is_some_and(|r| r.1.is_infinite());
        match (is_on, on) {
            (false, true) => {
                self.runs.push((t, f64::INFINITY));
                true
            }
            (true, false) => {
                let last = self.runs.last_mut().expect("on");
                if last.0 >= t {
                    self.runs.pop();
                } else {
                    last.1 = t;
                }
                true
            }
            _ => false,
        }
    }

    pub fn is_on(&self) -> bool {
        self.runs.last().is_some_and(|r| r.1.is_infinite())
    }

    pub fn runs(&self) -> &[(f64, f64)] {
        &self.runs
    }

    /// Drops runs that ended before `t`.
    pub fn prune(&mut self, t: f64) {
        self.runs.retain(|r| r.1 >= t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(spec: Loop) -> LoopRun {
        LoopRun::new(0.0, spec, None, Rest::PIXELS, f64::INFINITY)
    }

    #[test]
    fn waves_start_at_their_rest_and_repeat() {
        for w in [Wave::Sine, Wave::Swell, Wave::Triangle, Wave::Ramp] {
            assert!(w.at(0.0).abs() < 1e-12, "{w:?}");
            for u in [0.13, 0.5, 0.77] {
                assert!((w.at(u) - w.at(u + 3.0)).abs() < 1e-9, "{w:?}");
            }
        }
        assert_eq!(Wave::Swell.at(0.5), 1.0);
        assert_eq!(Wave::Triangle.at(0.25), 0.5);
        let g = Wave::Gate { from: 0.5, to: 1.0 };
        assert_eq!((g.at(0.49), g.at(0.5), g.at(0.99), g.at(1.0)), (0.0, 1.0, 1.0, 0.0));
        let n = Wave::Noise { seed: 7 };
        assert!((n.at(0.3) - n.at(0.3)).abs() == 0.0 && n.at(0.3) != n.at(0.8));
    }

    #[test]
    fn slopes_match_finite_differences() {
        for w in [Wave::Sine, Wave::Swell, Wave::Triangle, Wave::Ramp, Wave::Noise { seed: 3 }] {
            for u in [0.11, 0.37, 0.62, 0.9] {
                // Noise is f32 inside, so a tiny step only measures rounding.
                let h = 1e-3;
                let fd = (w.at(u + h) - w.at(u - h)) / (2.0 * h);
                assert!((w.slope(u) - fd).abs() < 1e-2 * (1.0 + fd.abs()), "{w:?} at {u}: {} vs {fd}", w.slope(u));
            }
        }
    }

    #[test]
    fn running_loop_steps_at_its_frame_rate() {
        let r = run(Loop::pulse(10.0, 2.0));
        // 30 fps: the value holds for a step, then moves.
        assert_eq!(r.value(0.01), r.value(0.033));
        assert_ne!(r.value(0.033), r.value(0.034));
        assert_eq!(r.wake(0.01), Some(1.0 / 30.0));
        // A wake time lands in its own step, and the next wake is the step after.
        let w = r.wake(0.01).unwrap();
        assert!(r.wake(w).unwrap() > w);
        assert!((r.wake(w).unwrap() - 2.0 / 30.0).abs() < 1e-12);
        assert!((r.value(1.0) - 10.0).abs() < 1e-9, "top of the swell at half a cycle");
        assert_eq!(r.rest_at(), f64::INFINITY);
    }

    #[test]
    fn spring_stop_has_no_jump_and_settles_to_rest() {
        for spec in [Loop::bob(6.0, 1.3), Loop::pulse(0.2, 0.9), Loop::drift(4.0, 0.7, 11)] {
            let mut r = run(spec.fps(0.0));
            let ts = 0.83;
            let (x, v) = (r.value(ts), r.velocity(ts));
            assert!(r.set(ts, None));
            assert!(!r.running());
            assert!((r.value(ts) - x).abs() < 1e-12 && (r.velocity(ts) - v).abs() < 1e-9, "{spec:?}");
            let rest = r.rest_at();
            assert!(rest.is_finite() && rest > ts && rest < ts + 1.0, "{rest}");
            assert_eq!(r.value(rest), spec.rest);
            assert_eq!(r.wake(rest), None);
            // History is kept: before the stop it still samples as the running loop.
            assert_eq!(r.value(0.5), spec.fps(0.0).value(0.0, 0.5));
        }
    }

    #[test]
    fn spin_stops_on_a_whole_turn_ahead() {
        let spec = Loop::spin(1.0).fps(0.0);
        let mut r = run(spec);
        let ts = 2.3;
        let x = r.value(ts);
        r.set(ts, None);
        let end = r.value(r.rest_at());
        let turns = end / std::f64::consts::TAU;
        assert!((turns - turns.round()).abs() < 1e-9, "{turns}");
        assert!(end > x, "keeps turning forward: {x} to {end}");
        // Never turns back.
        let mut last = x;
        for i in 1..400 {
            let v = r.value(ts + i as f64 * 0.005);
            assert!(v >= last - 1e-9, "turned back at step {i}: {last} to {v}");
            last = v;
        }
    }

    #[test]
    fn finish_plays_the_cycle_out_and_holds() {
        let spec = Loop::step_on(2, 3, 1.2);
        let mut r = run(spec);
        // In the first step of the cycle no dot is lit; dot 2 lights in the last step.
        assert_eq!(r.value(0.1), 0.0);
        assert_eq!(r.value(1.0), 1.0);
        assert!(r.set(1.5, None));
        // Plays on until the cycle ends at 2.4 s.
        assert_eq!(r.value(1.6), 0.0);
        assert_eq!(r.value(2.35), 1.0);
        assert_eq!(r.rest_at(), 2.4);
        assert_eq!(r.value(2.4), 1.0);
        assert_eq!(r.value(9.0), 1.0);
        assert_eq!(r.wake(2.4), None);
        assert!(r.wake(2.35).unwrap() <= 2.4);
    }

    #[test]
    fn finishing_loop_declared_again_carries_on() {
        let spec = Loop::step_on(0, 3, 1.0);
        let mut a = run(spec);
        a.set(0.4, None);
        a.set(0.7, Some(spec));
        let b = run(spec);
        for i in 0..300 {
            let t = i as f64 * 0.01;
            assert_eq!(a.value(t), b.value(t), "t {t}");
        }
        assert!(a.running());
    }

    #[test]
    fn restart_after_rest_hands_off_without_a_jump() {
        let spec = Loop::bob(5.0, 1.0).phase(0.3).fps(0.0);
        let mut r = run(spec);
        r.set(0.4, None);
        let t = 0.55;
        let (x, v) = (r.value(t), r.velocity(t));
        r.set(t, Some(spec));
        assert!((r.value(t) - x).abs() < 1e-9 && (r.velocity(t) - v).abs() < 1e-9);
        // Once the residual has died out, it is the plain loop started at `t`.
        let late = t + 3.0;
        assert!((r.value(late) - spec.value(t, late)).abs() < 1e-3);
    }

    #[test]
    fn handoff_from_a_value_on_screen() {
        let spec = Loop::pulse(0.1, 1.0).phase(0.25).fps(0.0);
        let r = LoopRun::new(1.0, spec, Some((0.0, 0.0)), Rest::UNIT, f64::INFINITY);
        assert!(r.value(1.0).abs() < 1e-12, "starts where the property was, not at the phase");
        assert!((r.value(4.0) - spec.value(1.0, 4.0)).abs() < 1e-4);
    }

    #[test]
    fn same_events_same_values_at_any_frame_rate() {
        let spec = Loop::spin(0.8);
        let events = [(0.0, true), (1.37, false), (2.05, true), (3.3, false)];
        let build = |hz: f64| {
            let mut r: Option<LoopRun> = None;
            let mut next = 0;
            for f in 0..=(5.0 * hz) as u64 {
                let t = f as f64 / hz;
                while next < events.len() && events[next].0 <= t {
                    let (te, on) = events[next];
                    match r.as_mut() {
                        None => r = Some(LoopRun::new(te, spec, None, Rest::UNIT, f64::INFINITY)),
                        Some(r) => {
                            r.set(te, on.then_some(spec));
                        }
                    }
                    next += 1;
                }
            }
            r.unwrap()
        };
        let (a, b, c) = (build(144.0), build(60.0), build(30.0));
        for i in 0..5000 {
            let t = i as f64 * 1e-3;
            assert_eq!(a.value(t), b.value(t));
            assert_eq!(a.value(t), c.value(t));
        }
        assert_eq!(a.rest_at(), c.rest_at());
    }

    #[test]
    fn toggle_runs() {
        let mut tg = Toggle::new(1.0, true);
        assert!(!tg.set(1.5, true));
        assert!(tg.set(2.0, false));
        assert!(tg.set(3.0, true));
        assert_eq!(tg.runs(), &[(1.0, 2.0), (3.0, f64::INFINITY)]);
        assert!(tg.is_on());
        // Off at the same time it came on: the run never happened.
        assert!(tg.set(3.0, false));
        assert_eq!(tg.runs(), &[(1.0, 2.0)]);
        tg.prune(2.5);
        assert!(tg.runs().is_empty());
    }
}
