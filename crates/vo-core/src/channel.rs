//! An animated value as a list of closed-form spring segments.
//!
//! Every retarget starts a new segment at the event's own timestamp, from the exact position
//! and velocity the previous segment has at that moment. Sampling is a binary search plus one
//! closed-form evaluation, so `sample(t)` is pure: it gives the same answer at any frame rate,
//! in any order, and for a seek straight to `t`.

use crate::spring::Spring;

/// One closed-form piece: from `t0`, start at `x0` with velocity `v0` and move toward `target`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub t0: f64,
    pub x0: f64,
    pub v0: f64,
    pub target: f64,
    pub spring: Spring,
    /// Seconds after `t0` from which the segment counts as settled and samples as exactly
    /// `(target, 0)`.
    pub settle: f64,
}

impl Segment {
    fn sample(&self, t: f64) -> (f64, f64) {
        let dt = t - self.t0;
        if dt <= 0.0 {
            return (self.x0, self.v0);
        }
        // Same expression as `Channel::settled_at`, so sampling at that time is at rest.
        if t >= self.t0 + self.settle {
            return (self.target, 0.0);
        }
        let (y, v) = self.spring.eval(self.x0 - self.target, self.v0, dt);
        (self.target + y, v)
    }
}

/// When a channel counts as at rest: distance to the target and speed both below these.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rest {
    pub position: f64,
    pub velocity: f64,
}

impl Rest {
    /// Suits pixels: a thousandth of a pixel, and a hundredth of a pixel per second.
    pub const PIXELS: Rest = Rest { position: 1e-3, velocity: 1e-2 };
    /// Suits values from 0 to 1, such as opacity and progress.
    pub const UNIT: Rest = Rest { position: 1e-5, velocity: 1e-4 };
}

/// One animated scalar property.
#[derive(Clone, Debug)]
pub struct Channel {
    /// Sorted by `t0`, never empty.
    segments: Vec<Segment>,
    spring: Spring,
    rest: Rest,
    retention: f64,
}

impl Channel {
    /// A channel at rest at `value` from time `t`.
    pub fn new(t: f64, value: f64, spring: Spring) -> Channel {
        let first = Segment { t0: t, x0: value, v0: 0.0, target: value, spring, settle: 0.0 };
        Channel { segments: vec![first], spring, rest: Rest::PIXELS, retention: f64::INFINITY }
    }

    /// Sets the rest thresholds for segments started from now on.
    pub fn with_rest(mut self, rest: Rest) -> Channel {
        self.rest = rest;
        self
    }

    /// Keep only the segments needed to sample `secs` back from the latest retarget. The
    /// default keeps every segment, which an export or a scrubbing tool wants.
    pub fn with_retention(mut self, secs: f64) -> Channel {
        self.retention = secs;
        self
    }

    /// The spring for segments started from now on. Running segments keep theirs, so a token
    /// reload never changes what an earlier `t` samples to.
    pub fn set_spring(&mut self, spring: Spring) {
        self.spring = spring;
    }

    pub fn spring(&self) -> Spring {
        self.spring
    }

    /// Moves toward `target` from time `t`, keeping position and velocity at `t`.
    ///
    /// Segments that start after `t` are dropped, so a retarget earlier than the last one
    /// rewrites the future from `t` on. Hosts feed events in time order, so this only happens
    /// when a replay tool edits history on purpose.
    pub fn retarget(&mut self, t: f64, target: f64) {
        let last = self.segments.last().expect("never empty");
        if t >= last.t0 && target == last.target {
            return;
        }
        let (x, v) = self.sample(t);
        self.start_segment(t, x, v, target);
    }

    /// Jumps to `value` at `t` and rests there. For state that must not animate, such as the
    /// first layout or a reduced-motion change.
    pub fn snap(&mut self, t: f64, value: f64) {
        self.start_segment(t, value, 0.0, value);
    }

    fn start_segment(&mut self, t: f64, x0: f64, v0: f64, target: f64) {
        let i = self.index(t);
        let keep = if t < self.segments[0].t0 || self.segments[i].t0 == t { i } else { i + 1 };
        self.segments.truncate(keep);
        let settle = self.spring.settle_time(x0 - target, v0, self.rest.position, self.rest.velocity);
        self.segments.push(Segment { t0: t, x0, v0, target, spring: self.spring, settle });
        if self.retention.is_finite() {
            self.prune(t - self.retention);
        }
    }

    /// Drops segments that no `t >= before` needs.
    pub fn prune(&mut self, before: f64) {
        let i = self.index(before);
        self.segments.drain(..i);
    }

    /// Index of the segment active at `t`: the last one with `t0 <= t`, or the first one.
    fn index(&self, t: f64) -> usize {
        self.segments.partition_point(|s| s.t0 <= t).saturating_sub(1)
    }

    /// Value and velocity at `t`. Pure. Before the first kept segment it gives that
    /// segment's start state.
    pub fn sample(&self, t: f64) -> (f64, f64) {
        self.segments[self.index(t)].sample(t)
    }

    pub fn value(&self, t: f64) -> f64 {
        self.sample(t).0
    }

    /// The target of the latest segment.
    pub fn target(&self) -> f64 {
        self.segments.last().expect("never empty").target
    }

    /// The time from which the channel rests at its target, unless retargeted again.
    /// Infinite for a spring with no damping.
    pub fn settled_at(&self) -> f64 {
        let last = self.segments.last().expect("never empty");
        last.t0 + last.settle
    }

    pub fn is_settled(&self, t: f64) -> bool {
        t >= self.settled_at()
    }

    /// The earliest `t` that samples exactly; earlier segments were pruned.
    pub fn earliest(&self) -> f64 {
        self.segments[0].t0
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::rand_f64;

    /// Random retarget events from a fixed seed: `(t, target)` in time order.
    fn events(seed: u32, n: u32, span: f64) -> Vec<(f64, f64)> {
        let mut ev: Vec<(f64, f64)> =
            (0..n).map(|i| (rand_f64(seed, i, 0, 0) * span, rand_f64(seed, i, 1, 0) * 400.0 - 200.0)).collect();
        ev.sort_by(|a, b| a.0.total_cmp(&b.0));
        ev
    }

    /// A drawn frame: its time and the sampled value and velocity.
    type Frame = (f64, (f64, f64));

    /// A host loop at `hz`: before each frame, apply every event stamped up to the frame time
    /// at its own timestamp, then sample the frame. Returns the channel and the samples.
    fn run(spring: Spring, ev: &[(f64, f64)], hz: f64, span: f64) -> (Channel, Vec<Frame>) {
        let mut ch = Channel::new(0.0, 0.0, spring);
        let mut next = 0;
        let mut out = Vec::new();
        let frames = (span * hz).ceil() as u64 + 1;
        for frame in 0..frames {
            let t = frame as f64 / hz;
            while next < ev.len() && ev[next].0 <= t {
                ch.retarget(ev[next].0, ev[next].1);
                next += 1;
            }
            out.push((t, ch.sample(t)));
        }
        (ch, out)
    }

    #[test]
    fn same_events_same_samples_at_any_frame_rate() {
        let span = 3.0;
        for seed in 0..40 {
            for (_, spring) in Spring::PRESETS.iter().chain([("over", Spring::from_frequency(15.0, 2.5))].iter()) {
                let ev = events(seed, 1 + seed % 12, span);
                let runs: Vec<_> = [144.0, 60.0, 30.0].iter().map(|&hz| run(*spring, &ev, hz, span)).collect();
                let reference = &runs[0].0;
                for (ch, frames) in &runs {
                    // The segment lists are identical, bit for bit.
                    assert_eq!(ch.segments(), reference.segments(), "seed {seed}");
                    // Every frame a run drew is what every other run gives at that time.
                    for (t, s) in frames {
                        for (other, _) in &runs {
                            assert_eq!(other.sample(*t), *s, "seed {seed} t {t}");
                        }
                    }
                }
                // Times all three rates share (every 1/6 s) were drawn as the same pixels.
                let at = |frames: &Vec<Frame>, k: u64, hz: f64| frames[(k as f64 * hz / 6.0) as usize].1;
                for k in 0..=(span * 6.0) as u64 {
                    let a = at(&runs[0].1, k, 144.0);
                    assert_eq!(a, at(&runs[1].1, k, 60.0));
                    assert_eq!(a, at(&runs[2].1, k, 30.0));
                }
            }
        }
    }

    #[test]
    fn velocity_is_continuous_across_retarget() {
        for (name, spring) in Spring::PRESETS {
            let mut ch = Channel::new(0.0, 0.0, spring);
            ch.retarget(0.0, 100.0);
            for (i, (t, target)) in [(0.05, -50.0), (0.0731, 300.0), (0.2, 10.0), (0.21, 10.5)].into_iter().enumerate()
            {
                let before = ch.sample(t);
                ch.retarget(t, target);
                let after = ch.sample(t);
                assert_eq!(before, after, "{name} retarget {i}: state at the split point");
                // Velocity has no jump on either side: it changes by at most what the
                // acceleration allows in 0.1 microseconds.
                let h = 1e-7;
                let accel =
                    |x: f64, v: f64, target: f64| spring.stiffness * (x - target).abs() + spring.damping * v.abs();
                let (_, v_left) = ch.sample(t - h);
                let (_, v_right) = ch.sample(t + h);
                let bound = 1.01
                    * h
                    * accel(before.0, before.1, target).max(accel(
                        before.0,
                        before.1,
                        ch.segments()[ch.segments().len() - 2].target,
                    ))
                    + 1e-6;
                assert!((v_left - before.1).abs() < bound, "{name} retarget {i}: left {v_left} vs {}", before.1);
                assert!((v_right - before.1).abs() < bound, "{name} retarget {i}: right {v_right} vs {}", before.1);
            }
        }
    }

    #[test]
    fn seek_equals_in_order_sampling() {
        let ev = events(7, 20, 4.0);
        let mut ch = Channel::new(0.0, 0.0, Spring::POP);
        for (t, x) in &ev {
            ch.retarget(*t, *x);
        }
        let ordered: Vec<_> = (0..4000).map(|i| ch.sample(i as f64 * 1e-3)).collect();
        // Seek in a scrambled order.
        for k in 0..4000u32 {
            let i = (k.wrapping_mul(2_654_435_761) % 4000) as usize;
            assert_eq!(ch.sample(i as f64 * 1e-3), ordered[i]);
        }
    }

    #[test]
    fn settles_and_snaps_to_target() {
        let mut ch = Channel::new(0.0, 0.0, Spring::SOFT);
        assert!(ch.is_settled(0.0));
        ch.retarget(1.0, 50.0);
        assert!(!ch.is_settled(1.1));
        let ts = ch.settled_at();
        assert!(ts > 1.22 && ts < 1.6, "{ts}");
        assert_eq!(ch.sample(ts), (50.0, 0.0));
        let (x, v) = ch.sample(ts - 1e-4);
        assert!((x - 50.0).abs() < 2e-3 && v.abs() < 0.1, "{x} {v}");
        // Retargeting to the current target adds nothing.
        ch.retarget(2.0, 50.0);
        assert_eq!(ch.segments().len(), 2);
    }

    #[test]
    fn retarget_in_the_past_rewrites_the_future() {
        let mut a = Channel::new(0.0, 0.0, Spring::SNAP);
        a.retarget(0.1, 10.0);
        a.retarget(0.3, 20.0);
        a.retarget(0.2, 30.0);
        let mut b = Channel::new(0.0, 0.0, Spring::SNAP);
        b.retarget(0.1, 10.0);
        b.retarget(0.2, 30.0);
        assert_eq!(a.segments(), b.segments());
        // Two retargets at the same instant: the later one wins.
        a.retarget(0.2, 40.0);
        b = Channel::new(0.0, 0.0, Spring::SNAP);
        b.retarget(0.1, 10.0);
        b.retarget(0.2, 40.0);
        assert_eq!(a.segments(), b.segments());
    }

    #[test]
    fn pruning_keeps_the_window_exact() {
        let ev = events(3, 60, 10.0);
        let mut full = Channel::new(0.0, 0.0, Spring::POP);
        let mut kept = Channel::new(0.0, 0.0, Spring::POP).with_retention(1.0);
        for (t, x) in &ev {
            full.retarget(*t, *x);
            kept.retarget(*t, *x);
        }
        let last = ev.last().unwrap().0;
        assert!(kept.segments().len() < full.segments().len() / 3, "{}", kept.segments().len());
        assert!(kept.earliest() <= last - 1.0);
        for i in 0..3000 {
            let t = last - 1.0 + i as f64 * 1e-3;
            assert_eq!(kept.sample(t), full.sample(t));
        }
    }

    #[test]
    fn spring_change_keeps_earlier_samples() {
        let mut ch = Channel::new(0.0, 0.0, Spring::SLOW);
        ch.retarget(0.0, 100.0);
        let before = ch.sample(0.15);
        ch.set_spring(Spring::SNAP);
        ch.retarget(0.2, 0.0);
        assert_eq!(ch.sample(0.15), before);
        // The retarget at t = 0 replaced the resting segment that also started at 0.
        assert_eq!(ch.segments()[0].spring, Spring::SLOW);
        assert_eq!(ch.segments()[1].spring, Spring::SNAP);
    }

    #[test]
    fn snap_jumps_and_rests() {
        let mut ch = Channel::new(0.0, 0.0, Spring::SOFT);
        ch.retarget(0.0, 100.0);
        ch.snap(0.1, 7.0);
        assert_eq!(ch.sample(0.1), (7.0, 0.0));
        assert_eq!(ch.sample(5.0), (7.0, 0.0));
        assert_eq!(ch.settled_at(), 0.1);
    }
}
