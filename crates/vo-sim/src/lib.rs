//! Fixed-tick simulations that replay.
//!
//! A [`Sim`] is a grid (or anything) that steps at a fixed tick and takes timed events: drops,
//! pushes, dye. [`Timeline`] drives it so that the state at time `t` depends only on `t` and
//! the event list, never on how often frames were drawn. It follows the seek pattern of the
//! GPU particles (`vo_render::particles`, `Particles::snapshot`):
//!
//! - Ticks run at a fixed rate ([`DEFAULT_TICK_HZ`], 120 Hz like the particles). Tick `n` is
//!   the state at time `n / hz`. Tick 0 is the start state from [`Sim::reset`].
//! - An event at time `te` belongs to tick `ceil(te * hz)`, the first tick at or after it. It
//!   is applied right after that tick's step, so the state at tick `n` holds every event up to
//!   and including tick `n`. Events on the same tick apply in the order they were added.
//! - [`Timeline::state_at`] goes to the last tick at or before `t`. A host that interpolates
//!   between two ticks (as the particle draw does) calls [`Timeline::goto_tick`] itself.
//! - Going back restores the nearest snapshot at or before the target, or the start state,
//!   and steps forward. Snapshots are taken every N ticks with a memory cap; see
//!   [`snapshot`] for the policy and the cost of a seek.
//!
//! The trait does not assume the CPU: a GPU sim keeps its state in buffers, `step` records a
//! compute dispatch, and its snapshot is a GPU-side buffer copy. [`reference::Diffusion`] is a
//! small CPU sim used by the tests. With the `gpu` feature (on by default), [`water`] is the GPU
//! water surface and [`fluid`] the GPU ink and smoke (stable fluids with velocity and dye). With
//! the `render` feature (on by default), [`water_render`] draws the water as a pool in a vo scene
//! and [`fluid_render`] draws the fluid's dye as ink, smoke or watercolour.

#[cfg(feature = "gpu")]
pub mod fluid;
#[cfg(feature = "render")]
pub mod fluid_render;
pub mod reference;
pub mod snapshot;
#[cfg(feature = "gpu")]
pub mod water;
#[cfg(feature = "render")]
pub mod water_render;

pub use snapshot::{SnapshotPolicy, SnapshotStore, DEFAULT_CAP_BYTES, DEFAULT_INTERVAL_TICKS};

/// The default tick rate, the same as the particles.
pub const DEFAULT_TICK_HZ: f64 = 120.0;

/// Slack for times that sit exactly on a tick but come out a hair off in floating point.
const TICK_EPS: f64 = 1e-6;

/// A simulation that steps at a fixed tick and can be snapshot and restored exactly.
pub trait Sim {
    /// A timed input: a drop, a push, a dye splat.
    type Event;
    /// A saved state. For a GPU sim this is a buffer, filled by a GPU-side copy.
    type Snapshot;

    /// Puts the sim in its start state (tick 0, before any event).
    fn reset(&mut self);
    /// Advances one tick, to tick `tick`, with time step `dt` seconds.
    fn step(&mut self, tick: u64, dt: f64);
    /// Applies one event. Called after the step of the event's tick.
    fn apply(&mut self, event: &Self::Event);
    /// Saves the current state.
    fn snapshot(&mut self) -> Self::Snapshot;
    /// Puts back a state saved by [`Sim::snapshot`], exactly.
    fn restore(&mut self, snap: &Self::Snapshot);
    /// The memory one snapshot holds, for the cap.
    fn snapshot_bytes(&self, snap: &Self::Snapshot) -> usize;
}

/// An event with the time it was given and the tick it applies at.
#[derive(Clone, Debug, PartialEq)]
pub struct Timed<E> {
    pub t: f64,
    pub tick: u64,
    pub event: E,
}

/// Drives a [`Sim`] through time: fixed ticks, timed events, snapshots and seeking.
pub struct Timeline<S: Sim> {
    sim: S,
    hz: f64,
    tick: u64,
    events: Vec<Timed<S::Event>>,
    /// Index of the first event not yet applied (tick > `self.tick`).
    cursor: usize,
    /// The current state no longer matches the event list (an event was added at or before
    /// the current tick).
    stale: bool,
    store: SnapshotStore<S::Snapshot>,
}

impl<S: Sim> Timeline<S> {
    /// A timeline at [`DEFAULT_TICK_HZ`] with the default snapshot policy.
    pub fn new(sim: S) -> Timeline<S> {
        Timeline::with(sim, DEFAULT_TICK_HZ, SnapshotPolicy::default())
    }

    pub fn with(mut sim: S, hz: f64, policy: SnapshotPolicy) -> Timeline<S> {
        assert!(hz > 0.0, "tick rate must be positive");
        sim.reset();
        Timeline { sim, hz, tick: 0, events: Vec::new(), cursor: 0, stale: false, store: SnapshotStore::new(policy) }
    }

    pub fn sim(&self) -> &S {
        &self.sim
    }

    pub fn hz(&self) -> f64 {
        self.hz
    }

    /// The tick the sim is at.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    pub fn tick_time(&self, tick: u64) -> f64 {
        tick as f64 / self.hz
    }

    /// The tick an event at `t` applies at: the first tick at or after `t`.
    pub fn event_tick(&self, t: f64) -> u64 {
        (t * self.hz - TICK_EPS).ceil().max(0.0) as u64
    }

    /// The last tick at or before `t`.
    pub fn tick_at(&self, t: f64) -> u64 {
        (t * self.hz + TICK_EPS).floor().max(0.0) as u64
    }

    pub fn events(&self) -> &[Timed<S::Event>] {
        &self.events
    }

    /// The snapshot store, for its memory and interval.
    pub fn snapshots(&self) -> &SnapshotStore<S::Snapshot> {
        &self.store
    }

    /// Adds an event at time `t`. Snapshots from its tick on are dropped. When the sim is
    /// already past that tick, the next [`Timeline::goto_tick`] replays from before it.
    pub fn push_event(&mut self, t: f64, event: S::Event) {
        let tick = self.event_tick(t);
        let at = self.events.partition_point(|e| e.tick <= tick);
        self.events.insert(at, Timed { t, tick, event });
        self.store.invalidate_from(tick);
        // An event after the current tick lands after the cursor and applies when reached.
        if tick <= self.tick {
            self.stale = true;
        }
    }

    /// Drops every event and every snapshot. The next [`Timeline::goto_tick`] replays from
    /// the start state.
    pub fn clear_events(&mut self) {
        self.events.clear();
        self.cursor = 0;
        self.store.invalidate_from(0);
        self.stale = true;
    }

    /// The state at the last tick at or before `t`.
    pub fn state_at(&mut self, t: f64) -> &S {
        let tick = self.tick_at(t);
        self.goto_tick(tick);
        &self.sim
    }

    /// Steps (or seeks back, then steps) to tick `target`. Returns the number of ticks run.
    pub fn goto_tick(&mut self, target: u64) -> u64 {
        if self.stale || target < self.tick {
            self.rewind(target);
        }
        let dt = 1.0 / self.hz;
        let mut ran = 0;
        while self.tick < target {
            self.tick += 1;
            self.sim.step(self.tick, dt);
            self.apply_due();
            ran += 1;
            if self.store.wants(self.tick) {
                let snap = self.sim.snapshot();
                let bytes = self.sim.snapshot_bytes(&snap);
                self.store.insert(self.tick, bytes, snap);
            }
        }
        ran
    }

    /// Restores the nearest snapshot at or before `target`, or the start state.
    fn rewind(&mut self, target: u64) {
        match self.store.at_or_before(target) {
            Some((tick, snap)) => {
                self.sim.restore(snap);
                self.tick = tick;
                self.cursor = self.events.partition_point(|e| e.tick <= tick);
            }
            None => {
                self.sim.reset();
                self.tick = 0;
                self.cursor = 0;
                self.apply_due();
            }
        }
        self.stale = false;
    }

    /// Applies the events of the current tick.
    fn apply_due(&mut self) {
        while let Some(e) = self.events.get(self.cursor).filter(|e| e.tick <= self.tick) {
            self.sim.apply(&e.event);
            self.cursor += 1;
        }
    }
}
