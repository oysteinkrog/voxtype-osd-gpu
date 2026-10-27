//! Labelled sequences with handoff: a state that moves through named phases as events arrive,
//! each phase starting from exactly where the one before it was.
//!
//! A [`Story`] holds an initial state and a list of steps. Each step has a label, a start time
//! and a [`Step`]: a pure function from the state the step was entered with and the time since
//! it started to the state now. The entry state of a step is the previous step evaluated at
//! the new step's start, computed once when the step is added. So a spinner that is cut short
//! by "done" hands its current angles and their velocities to the springs that close the ring,
//! and nothing snaps.
//!
//! Everything stays a pure function of `t`: [`Story::state`] at any time, in any order, gives
//! the same answer. Steps must arrive in time order. A step whose time is earlier than the
//! last one (an event that arrived late) is moved to the last step's time: it happens now
//! instead, which keeps what is already on screen.

use std::sync::Arc;

use vo_core::spring::Spring;

/// One phase of a [`Story`]: the state `dt` seconds after the phase started, given the state
/// it started from.
pub trait Step<S>: Send + Sync {
    fn eval(&self, entry: &S, dt: f64) -> S;
}

impl<S, F: Fn(&S, f64) -> S + Send + Sync> Step<S> for F {
    fn eval(&self, entry: &S, dt: f64) -> S {
        self(entry, dt)
    }
}

/// Boxed steps, for sequences whose steps are different closures.
impl<S> Step<S> for Box<dyn Step<S>> {
    fn eval(&self, entry: &S, dt: f64) -> S {
        (**self).eval(entry, dt)
    }
}

/// A value and its velocity per second. States that hand off between phases carry these, so
/// a spring that takes over starts with the speed the value already had.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ch {
    pub x: f64,
    pub v: f64,
}

impl Ch {
    /// At rest at `x`.
    pub const fn at(x: f64) -> Ch {
        Ch { x, v: 0.0 }
    }

    /// `dt` seconds after this value started moving towards `target` on `spring`.
    pub fn to(self, target: f64, spring: Spring, dt: f64) -> Ch {
        let (y, v) = spring.eval(self.x - target, self.v, dt.max(0.0));
        Ch { x: target + y, v }
    }

    /// Seconds until a move from here to `target` on `spring` stays within `eps` of it.
    pub fn settle(self, target: f64, spring: Spring, eps: f64) -> f64 {
        spring.settle_time(self.x - target, self.v, eps, eps * 10.0)
    }
}

/// The state of a [`Story`] at some time.
#[derive(Clone, Debug, PartialEq)]
pub struct Phase<'a, S> {
    /// The label of the active step, or `""` before the first one.
    pub label: &'a str,
    /// The active step's index, `None` before the first step.
    pub index: Option<usize>,
    /// Seconds since the active step started (since `-inf` before the first: 0 then).
    pub dt: f64,
    pub state: S,
}

struct Entry<S> {
    label: String,
    start: f64,
    entry: S,
    step: Arc<dyn Step<S>>,
}

impl<S: Clone> Clone for Entry<S> {
    fn clone(&self) -> Entry<S> {
        Entry { label: self.label.clone(), start: self.start, entry: self.entry.clone(), step: self.step.clone() }
    }
}

/// A state moving through labelled steps (see the module docs).
pub struct Story<S> {
    initial: S,
    steps: Vec<Entry<S>>,
}

impl<S: Clone> Clone for Story<S> {
    fn clone(&self) -> Story<S> {
        Story { initial: self.initial.clone(), steps: self.steps.clone() }
    }
}

impl<S: std::fmt::Debug> std::fmt::Debug for Story<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let steps: Vec<_> = self.steps.iter().map(|e| (e.label.as_str(), e.start)).collect();
        f.debug_struct("Story").field("initial", &self.initial).field("steps", &steps).finish()
    }
}

impl<S: Clone> Story<S> {
    /// A story that holds `initial` until its first step.
    pub fn new(initial: S) -> Story<S> {
        Story { initial, steps: Vec::new() }
    }

    /// Adds a step labelled `label` starting at `at`, entered from the state the story has
    /// then. Returns the start actually used: `at`, or the last step's start if `at` is
    /// earlier.
    pub fn then(&mut self, label: impl Into<String>, at: f64, step: impl Step<S> + 'static) -> f64 {
        let start = self.steps.last().map_or(at, |e| at.max(e.start));
        let entry = self.state(start).state;
        self.steps.push(Entry { label: label.into(), start, entry, step: Arc::new(step) });
        start
    }

    /// Adds several steps at once, each at `at` plus its offset: a short labelled sequence
    /// such as "close the ring, then draw the check". Offsets must not go down.
    pub fn sequence<I, T>(&mut self, at: f64, steps: I) -> f64
    where
        I: IntoIterator<Item = (&'static str, f64, T)>,
        T: Step<S> + 'static,
    {
        let mut start = at;
        for (label, offset, step) in steps {
            start = self.then(label, at + offset, step);
        }
        start
    }

    /// The state at `t`.
    pub fn state(&self, t: f64) -> Phase<'_, S> {
        let i = self.steps.partition_point(|e| e.start <= t);
        match i.checked_sub(1) {
            None => Phase { label: "", index: None, dt: 0.0, state: self.initial.clone() },
            Some(i) => {
                let e = &self.steps[i];
                let dt = t - e.start;
                Phase { label: &e.label, index: Some(i), dt, state: e.step.eval(&e.entry, dt) }
            }
        }
    }

    /// The label active at `t` (`""` before the first step).
    pub fn label_at(&self, t: f64) -> &str {
        let i = self.steps.partition_point(|e| e.start <= t);
        i.checked_sub(1).map_or("", |i| &self.steps[i].label)
    }

    /// The start of the last step labelled `label`.
    pub fn start_of(&self, label: &str) -> Option<f64> {
        self.steps.iter().rev().find(|e| e.label == label).map(|e| e.start)
    }

    /// The state a step was entered with.
    pub fn entry_of(&self, label: &str) -> Option<&S> {
        self.steps.iter().rev().find(|e| e.label == label).map(|e| &e.entry)
    }

    /// The steps as `(label, start)`, in order.
    pub fn steps(&self) -> impl Iterator<Item = (&str, f64)> {
        self.steps.iter().map(|e| (e.label.as_str(), e.start))
    }

    /// The start of the last step, or `None` with no steps.
    pub fn last_start(&self) -> Option<f64> {
        self.steps.last().map(|e| e.start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moving_story() -> Story<Ch> {
        let mut s = Story::new(Ch::at(0.0));
        // Moves at 2 units a second, then a spring takes it to 10.
        s.then("ramp", 1.0, |e: &Ch, dt: f64| Ch { x: e.x + 2.0 * dt, v: 2.0 });
        s.then("settle", 2.0, |e: &Ch, dt: f64| e.to(10.0, Spring::SOFT, dt));
        s
    }

    #[test]
    fn handoff_keeps_value_and_velocity() {
        let s = moving_story();
        let before = s.state(2.0 - 1e-9).state;
        let after = s.state(2.0).state;
        assert!((before.x - after.x).abs() < 1e-6 && (before.v - after.v).abs() < 1e-6, "{before:?} {after:?}");
        assert_eq!(s.state(2.0).label, "settle");
        assert_eq!(s.state(1.5).label, "ramp");
        assert_eq!(s.state(0.5).label, "");
        assert!((s.state(5.0).state.x - 10.0).abs() < 1e-3);
    }

    #[test]
    fn seeking_in_any_order_gives_the_same_states() {
        let s = moving_story();
        let times: Vec<f64> = (0..300).map(|i| i as f64 * 0.01).collect();
        let forward: Vec<Ch> = times.iter().map(|&t| s.state(t).state).collect();
        let backward: Vec<Ch> = times.iter().rev().map(|&t| s.state(t).state).collect();
        assert!(forward.iter().eq(backward.iter().rev()));
    }

    #[test]
    fn a_late_step_happens_at_the_last_start() {
        let mut s = moving_story();
        let at = s.then("late", 1.5, |e: &Ch, _dt: f64| *e);
        assert_eq!(at, 2.0);
        assert_eq!(s.last_start(), Some(2.0));
        // The step entered from what was on screen at 2.0, so nothing jumps.
        assert_eq!(s.state(2.0).state, *s.entry_of("late").unwrap());
    }

    #[test]
    fn a_sequence_adds_labelled_steps_at_offsets() {
        let mut s = Story::new(Ch::at(0.0));
        s.sequence(
            1.0,
            [
                ("a", 0.0, Box::new(|e: &Ch, dt: f64| Ch::at(e.x + dt)) as Box<dyn Step<Ch>>),
                ("b", 0.5, Box::new(|e: &Ch, _| *e)),
            ],
        );
        let steps: Vec<_> = s.steps().collect();
        assert_eq!(steps, [("a", 1.0), ("b", 1.5)]);
        assert_eq!(s.state(3.0).state.x, 0.5);
    }
}
