//! A reference sim: value that diffuses on a small grid. Runs on the CPU so its tests run in
//! `just test`. The water and fluid sims implement the same trait on the GPU.

use crate::Sim;

/// A drop of value at one cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drop {
    pub x: u32,
    pub y: u32,
    pub amount: f32,
}

/// Explicit diffusion on a `width x height` grid with closed edges, so the total is kept.
pub struct Diffusion {
    width: u32,
    height: u32,
    /// Diffusion per tick, stable below 0.25.
    rate: f32,
    field: Vec<f32>,
    scratch: Vec<f32>,
}

impl Diffusion {
    pub fn new(width: u32, height: u32, rate: f32) -> Diffusion {
        assert!(width > 0 && height > 0);
        assert!((0.0..0.25).contains(&rate), "explicit diffusion is stable below 0.25");
        let n = (width * height) as usize;
        Diffusion { width, height, rate, field: vec![0.0; n], scratch: vec![0.0; n] }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn field(&self) -> &[f32] {
        &self.field
    }

    pub fn get(&self, x: u32, y: u32) -> f32 {
        self.field[(y * self.width + x) as usize]
    }

    /// The field as little-endian bytes, for byte-for-byte comparison.
    pub fn bytes(&self) -> Vec<u8> {
        self.field.iter().flat_map(|v| v.to_le_bytes()).collect()
    }
}

impl Sim for Diffusion {
    type Event = Drop;
    type Snapshot = Vec<f32>;

    fn reset(&mut self) {
        self.field.fill(0.0);
    }

    fn step(&mut self, _tick: u64, _dt: f64) {
        let (w, h) = (self.width as usize, self.height as usize);
        let f = &self.field;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let c = f[i];
                let l = if x > 0 { f[i - 1] } else { c };
                let r = if x + 1 < w { f[i + 1] } else { c };
                let u = if y > 0 { f[i - w] } else { c };
                let d = if y + 1 < h { f[i + w] } else { c };
                self.scratch[i] = c + self.rate * (l + r + u + d - 4.0 * c);
            }
        }
        std::mem::swap(&mut self.field, &mut self.scratch);
    }

    fn apply(&mut self, e: &Drop) {
        if e.x < self.width && e.y < self.height {
            self.field[(e.y * self.width + e.x) as usize] += e.amount;
        }
    }

    fn snapshot(&mut self) -> Vec<f32> {
        self.field.clone()
    }

    fn restore(&mut self, snap: &Vec<f32>) {
        self.field.copy_from_slice(snap);
    }

    fn snapshot_bytes(&self, snap: &Vec<f32>) -> usize {
        std::mem::size_of_val(snap.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SnapshotPolicy, Timeline, DEFAULT_CAP_BYTES, DEFAULT_TICK_HZ};
    use std::time::Instant;

    const W: u32 = 64;
    const H: u32 = 32;

    fn timeline(policy: SnapshotPolicy) -> Timeline<Diffusion> {
        let mut tl = Timeline::with(Diffusion::new(W, H, 0.2), DEFAULT_TICK_HZ, policy);
        tl.push_event(0.5, Drop { x: 10, y: 10, amount: 100.0 });
        tl.push_event(1.004, Drop { x: 50, y: 20, amount: 40.0 });
        tl.push_event(2.2, Drop { x: 32, y: 5, amount: 70.0 });
        tl
    }

    /// Draws frames every `dt` seconds up to `end`, then samples `end` itself.
    fn run_frames(dt: f64, end: f64) -> Vec<u8> {
        let mut tl = timeline(SnapshotPolicy::default());
        let mut frame = 0u64;
        loop {
            let t = frame as f64 * dt;
            if t >= end {
                break;
            }
            tl.state_at(t);
            frame += 1;
        }
        tl.state_at(end).bytes()
    }

    #[test]
    fn frame_rate_does_not_change_the_state() {
        for end in [0.7, 1.5, 3.0] {
            let a = run_frames(1.0 / 144.0, end);
            let b = run_frames(1.0 / 60.0, end);
            assert_eq!(a, b, "states differ at t = {end}");
        }
        assert_ne!(run_frames(1.0 / 60.0, 3.0), timeline(SnapshotPolicy::default()).state_at(0.0).bytes());
    }

    #[test]
    fn seek_back_equals_a_fresh_run() {
        let mut tl = timeline(SnapshotPolicy::default());
        tl.state_at(5.0);
        let seeked = tl.state_at(2.3).bytes();
        assert_eq!(tl.tick(), 276);
        let fresh = timeline(SnapshotPolicy::default()).state_at(2.3).bytes();
        assert_eq!(seeked, fresh);
        // And forward again after the seek.
        let again = tl.state_at(5.0).bytes();
        assert_eq!(again, timeline(SnapshotPolicy::default()).state_at(5.0).bytes());
    }

    #[test]
    fn late_event_replays_from_before_it() {
        let mut tl = timeline(SnapshotPolicy::default());
        tl.state_at(4.0);
        tl.push_event(1.5, Drop { x: 1, y: 1, amount: 9.0 });
        let late = tl.state_at(4.0).bytes();
        let mut fresh = timeline(SnapshotPolicy::default());
        fresh.push_event(1.5, Drop { x: 1, y: 1, amount: 9.0 });
        assert_eq!(late, fresh.state_at(4.0).bytes());
    }

    #[test]
    fn event_applies_at_the_first_tick_at_or_after_it() {
        for dt in [1.0 / 144.0, 1.0 / 60.0] {
            let mut tl = Timeline::new(Diffusion::new(W, H, 0.2));
            tl.push_event(1.004, Drop { x: 20, y: 16, amount: 1.0 });
            assert_eq!(tl.events()[0].tick, 121);
            let mut saw_after = false;
            for frame in 0..200 {
                let tick = {
                    tl.state_at(frame as f64 * dt);
                    tl.tick()
                };
                let total: f32 = tl.sim().field().iter().sum();
                if tick < 121 {
                    assert_eq!(total, 0.0, "event early at tick {tick}");
                } else {
                    assert!(total > 0.0, "event missing at tick {tick}");
                    saw_after = true;
                }
            }
            assert!(saw_after);
            tl.goto_tick(120);
            assert_eq!(tl.sim().field().iter().sum::<f32>(), 0.0);
            tl.goto_tick(121);
            assert_eq!(tl.sim().get(20, 16), 1.0, "applied after tick 121's step, not diffused yet");
        }
    }

    #[test]
    fn snapshot_memory_stays_under_the_cap() {
        let one = (W * H) as usize * 4;
        // A tight cap: 20 snapshots, so 60 s at one per second must thin.
        let tight = SnapshotPolicy { interval: 120, cap_bytes: one * 20 };
        let mut tl = timeline(tight);
        for frame in 0..=3600 {
            tl.state_at(frame as f64 / 60.0);
            assert!(tl.snapshots().bytes() <= tight.cap_bytes);
        }
        assert!(tl.snapshots().interval() > 120, "thinning kicked in");
        let fresh = timeline(SnapshotPolicy::default()).state_at(37.3).bytes();
        assert_eq!(tl.state_at(37.3).bytes(), fresh, "seek after thinning is still exact");

        let mut tl = timeline(SnapshotPolicy::default());
        tl.state_at(60.0);
        assert_eq!(tl.snapshots().len(), 60);
        assert!(tl.snapshots().bytes() <= DEFAULT_CAP_BYTES);
    }

    #[test]
    fn seek_time_over_60_s() {
        let mut tl = timeline(SnapshotPolicy::default());
        let start = Instant::now();
        let ran = tl.goto_tick(tl.tick_at(60.0));
        let forward = start.elapsed();
        let start = Instant::now();
        let back = tl.goto_tick(tl.tick_at(30.5));
        let seek_back = start.elapsed();
        let start = Instant::now();
        tl.goto_tick(0);
        let to_zero = start.elapsed();
        println!(
            "vo-sim seek, {W}x{H} diffusion: 0 -> 60 s ran {ran} ticks in {forward:?}; \
             60 -> 30.5 s ran {back} ticks in {seek_back:?}; back to 0 in {to_zero:?}; \
             snapshots {} ({} bytes)",
            tl.snapshots().len(),
            tl.snapshots().bytes()
        );
        assert_eq!(ran, 7200);
        assert!(back < 120);
    }
}
