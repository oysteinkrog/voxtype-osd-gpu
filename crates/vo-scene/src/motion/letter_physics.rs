//! Letter physics (MO-83): letters as small rigid bodies that drop into the line.
//!
//! Each new letter of a live word falls from `drop` above its place, lands, bounces a few
//! times with `restitution` and stops. When a letter lands it nudges its neighbours: each
//! gets a short damped sideways push away from it. In flight a letter also drifts a little
//! sideways along its own trajectory and arrives exactly on its place. After `lock` seconds
//! from its start every offset is exactly zero, so the line ends in perfect alignment.
//!
//! The simulation is closed form: the bounces are the exact ballistic arcs of a body under
//! constant gravity with a restitution coefficient, and the nudges are damped sines. So a
//! letter's offset is a pure function of `t`, like every other part of `sample`, and a seek,
//! a replay or a render at another frame rate gives the same frames.
//!
//! Rule 1 of the motion language, the reading-line envelope: on the newest line the drop is
//! at most [`READING_DROP`] pixels, letters never go below the baseline, and the sideways
//! offsets stay within [`READING_SIDE`] ems (inside the 5 % tracking change). Above the
//! newest line the physics is freer: a word that arrives on an older line, or a specimen
//! word set with [`LetterPhysics::specimen`], falls the full `drop`. Under reduced motion
//! nothing moves: letters arrive in place.

use vo_core::hash::rand_f32;

use crate::geom::Color;

/// The largest drop on the newest line, in pixels (rule 1: baseline shift at most 3 px).
pub const READING_DROP: f32 = 3.0;
/// The largest sideways offset on the newest line, in ems.
pub const READING_SIDE: f32 = 0.04;
/// Bounces after the first touch that are worked out; later ones are below a hundredth of a
/// pixel for any drop the popup uses.
const MAX_BOUNCES: u32 = 8;
/// Frequency and damping of a neighbour's nudge.
const NUDGE_HZ: f64 = 9.0;
const NUDGE_ZETA: f64 = 0.35;

/// How letters of new words fall into the line (`Element::letter_physics`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LetterPhysics {
    /// Height a letter falls from, in pixels. Clamped to [`READING_DROP`] on the newest line
    /// unless `free` is set.
    pub drop: f32,
    /// Seconds from a letter's start to its first touch of the baseline. Sets gravity:
    /// `g = 2 drop / land²`, so a bigger drop falls just as fast.
    pub land: f64,
    /// Share of its speed a letter keeps at each bounce.
    pub restitution: f32,
    /// Delay between letters of one word, in reading order.
    pub stagger: f64,
    /// How far a landing letter pushes each neighbour, in ems.
    pub nudge: f32,
    /// How far a letter drifts sideways in flight, at most, in ems.
    pub drift: f32,
    /// Seconds from a letter's start after which it is locked exactly in place.
    pub lock: f64,
    /// Faint motion vectors on letters in flight, in this colour (the mock-up's accent).
    pub vectors: Option<Color>,
    /// No reading-line clamp: for a display word that is not the transcript being read.
    pub free: bool,
}

impl Default for LetterPhysics {
    /// The reading-line form: a 3 px drop landing in 80 ms, two visible micro bounces,
    /// letters 16 ms apart, locked after 420 ms.
    fn default() -> LetterPhysics {
        LetterPhysics {
            drop: READING_DROP,
            land: 0.08,
            restitution: 0.45,
            stagger: 0.016,
            nudge: 0.03,
            drift: 0.025,
            lock: 0.42,
            vectors: None,
            free: false,
        }
    }
}

/// One letter's offset and speed at a time, down and right positive, in pixels and pixels
/// per second.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LetterState {
    pub dx: f32,
    pub dy: f32,
    pub vx: f32,
    pub vy: f32,
}

impl LetterPhysics {
    /// A display word that falls from `drop` pixels with the mock-up's timing: drop in
    /// 100 ms, bounces to about 220 ms, nudges to about 320 ms, locked at 500 ms. Not
    /// clamped by the reading-line envelope, so not for the transcript being read.
    pub fn specimen(drop: f32) -> LetterPhysics {
        LetterPhysics {
            drop,
            land: 0.1,
            restitution: 0.35,
            stagger: 0.035,
            nudge: 0.06,
            drift: 0.12,
            lock: 0.5,
            vectors: None,
            free: true,
        }
    }

    /// The same physics with motion vectors in `color`.
    pub fn vectors(self, color: Color) -> LetterPhysics {
        LetterPhysics { vectors: Some(color), ..self }
    }

    /// The drop and sideways limit (in ems) on a line: clamped on the newest line.
    fn limits(&self, newest: bool) -> (f32, f32) {
        if newest && !self.free {
            (self.drop.clamp(0.0, READING_DROP), READING_SIDE)
        } else {
            (self.drop.max(0.0), f32::INFINITY)
        }
    }

    /// Height above the baseline and upward speed of a body dropped from `h` at `tau`
    /// seconds after its start: the exact ballistic arcs with restitution.
    fn fall(&self, h: f32, tau: f64) -> (f64, f64) {
        if tau <= 0.0 || h <= 0.0 {
            return (h.max(0.0) as f64, 0.0);
        }
        let land = self.land.max(1e-3);
        let g = 2.0 * h as f64 / (land * land);
        if tau < land {
            return (h as f64 - 0.5 * g * tau * tau, -g * tau);
        }
        let e = self.restitution.clamp(0.0, 0.95) as f64;
        let (mut at, mut v) = (land, g * land);
        for _ in 0..MAX_BOUNCES {
            v *= e;
            let span = 2.0 * v / g;
            if span <= 0.0 {
                break;
            }
            if tau < at + span {
                let s = tau - at;
                return (v * s - 0.5 * g * s * s, v - g * s);
            }
            at += span;
        }
        (0.0, 0.0)
    }

    /// When a letter that starts at `start` first touches the baseline.
    fn touch(&self, start: f64) -> f64 {
        start + self.land.max(1e-3)
    }

    /// The state of letter `k` of a word whose letters started at `starts` (one per letter,
    /// in reading order) at time `t`. `size` is the em size, `seed` makes the drift
    /// differ between words, and `newest` says whether the word is on the newest line.
    pub fn letter(&self, starts: &[f64], k: usize, t: f64, size: f32, seed: u32, newest: bool) -> LetterState {
        let Some(&start) = starts.get(k) else { return LetterState::default() };
        let (h, side) = self.limits(newest);
        let side = side * size;
        let tau = t - start;
        if tau >= self.lock {
            return LetterState::default();
        }
        // The lock: a short ease over the last 60 ms brings every offset to exactly zero.
        let lock = lock_weight(tau, self.lock);
        let (y, vy) = self.fall(h, tau);
        // Drift: the letter starts a little to one side and its path reaches its place at
        // the first touch.
        let land = self.land.max(1e-3);
        let drift = (rand_f32(seed, k as u32, 0x5eed, 0) * 2.0 - 1.0) * self.drift * size;
        let (mut dx, mut vx) = if tau < land {
            let u = 1.0 - tau / land;
            (drift as f64 * u * u, -2.0 * drift as f64 * u / land)
        } else {
            (0.0, 0.0)
        };
        // Nudges from the neighbours that landed: away from them.
        for (j, &other) in starts.iter().enumerate() {
            if j == k || j + 1 < k || j > k + 1 {
                continue;
            }
            let s = t - self.touch(other);
            if s <= 0.0 {
                continue;
            }
            let dir = if j < k { 1.0 } else { -1.0 };
            let (p, v) = damped(s);
            let amp = dir * (self.nudge * size) as f64;
            dx += amp * p;
            vx += amp * v;
        }
        let dx = (dx as f32).clamp(-side, side) * lock;
        let dy = (-y as f32).clamp(-h, 0.0) * lock;
        LetterState { dx, dy, vx: vx as f32 * lock, vy: -vy as f32 * lock }
    }

    /// When the last of letters starting at `starts` is locked in place.
    pub fn settled_at(&self, starts: impl IntoIterator<Item = f64>) -> f64 {
        starts.into_iter().fold(f64::NEG_INFINITY, f64::max) + self.lock
    }

    /// Start times for the letters of a word, from the time each letter was born: letters
    /// born together fall one after another in reading order.
    pub fn starts(&self, born: impl IntoIterator<Item = f64>) -> Vec<f64> {
        let mut out: Vec<f64> = Vec::new();
        let mut rank = 0u32;
        let mut last = None::<f64>;
        for b in born {
            if last.is_none_or(|l| (b - l).abs() > 1e-9) {
                rank = 0;
                last = Some(b);
            }
            out.push(b + self.stagger * rank as f64);
            rank += 1;
        }
        out
    }
}

/// A unit nudge `s` seconds after the impact: a damped sine with its speed, peak about 1.
fn damped(s: f64) -> (f64, f64) {
    let w = std::f64::consts::TAU * NUDGE_HZ;
    let wd = w * (1.0 - NUDGE_ZETA * NUDGE_ZETA).sqrt();
    let decay = (-NUDGE_ZETA * w * s).exp();
    // Normalised so the first peak is about 1.
    let norm = 1.6;
    let p = norm * decay * (wd * s).sin();
    let v = norm * decay * (wd * (wd * s).cos() - NUDGE_ZETA * w * (wd * s).sin());
    (p, v)
}

/// 1 until 60 ms before `lock`, easing to exactly 0 at it.
fn lock_weight(tau: f64, lock: f64) -> f32 {
    let ease = 0.06f64.min(lock);
    let u = ((lock - tau) / ease).clamp(0.0, 1.0);
    (u * u * (3.0 - 2.0 * u)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(p: &LetterPhysics, n: usize) -> Vec<f64> {
        p.starts(std::iter::repeat_n(1.0, n))
    }

    #[test]
    fn letters_fall_in_reading_order() {
        let p = LetterPhysics::default();
        let s = word(&p, 4);
        assert_eq!(s[0], 1.0);
        assert!((s[3] - (1.0 + 3.0 * p.stagger)).abs() < 1e-12);
        // A letter born later starts its own run.
        let s = p.starts([1.0, 1.0, 2.0]);
        assert_eq!(s[2], 2.0);
    }

    #[test]
    fn a_letter_starts_up_lands_bounces_and_locks() {
        let p = LetterPhysics::default();
        let s = word(&p, 1);
        let at = |t: f64| p.letter(&s, 0, t, 20.0, 7, true);
        assert!((at(1.0).dy + READING_DROP).abs() < 1e-4, "starts a full drop up");
        assert!(at(1.0 + p.land).dy.abs() < 1e-3, "touches the baseline at land");
        // It bounces up again after the touch.
        let peak = (1..60).map(|i| at(1.0 + p.land + i as f64 * 0.002).dy).fold(0.0f32, f32::min);
        assert!(peak < -0.2, "a visible micro bounce, got {peak}");
        assert!(peak > -READING_DROP * 0.5);
        assert_eq!(at(1.0 + p.lock), LetterState::default(), "locked exactly");
        assert_eq!(at(5.0), LetterState::default());
    }

    #[test]
    fn the_reading_line_envelope_holds_every_frame() {
        let p = LetterPhysics { drop: 40.0, nudge: 0.5, drift: 0.5, ..LetterPhysics::default() };
        let s = word(&p, 6);
        let size = 18.0;
        for f in 0..2000 {
            let t = 0.9 + f as f64 * 0.0005;
            for k in 0..6 {
                let m = p.letter(&s, k, t, size, 3, true);
                assert!(m.dy >= -READING_DROP - 1e-4 && m.dy <= 0.0, "baseline {}", m.dy);
                assert!(m.dx.abs() <= READING_SIDE * size + 1e-4, "side {}", m.dx);
            }
        }
    }

    #[test]
    fn above_the_newest_line_and_in_a_specimen_it_is_freer() {
        let p = LetterPhysics::specimen(40.0);
        let s = word(&p, 3);
        assert!((p.letter(&s, 0, 1.0, 40.0, 1, true).dy + 40.0).abs() < 1e-3);
        let q = LetterPhysics { drop: 12.0, ..LetterPhysics::default() };
        assert!((q.letter(&s, 0, 1.0, 20.0, 1, false).dy + 12.0).abs() < 1e-3);
        assert!((q.letter(&s, 0, 1.0, 20.0, 1, true).dy + READING_DROP).abs() < 1e-3);
    }

    #[test]
    fn a_landing_letter_pushes_its_neighbours_away() {
        let p = LetterPhysics { drift: 0.0, ..LetterPhysics::default() };
        // The middle letter lands first; its neighbours have not started yet.
        let s = [1.2, 1.0, 1.2];
        let t = 1.0 + p.land + 0.02;
        assert!(p.letter(&s, 0, t, 20.0, 0, true).dx < 0.0);
        assert!(p.letter(&s, 2, t, 20.0, 0, true).dx > 0.0);
    }

    #[test]
    fn pure_in_t() {
        let p = LetterPhysics::specimen(30.0);
        let s = word(&p, 5);
        let a: Vec<_> = (0..5).map(|k| p.letter(&s, k, 1.13, 30.0, 9, false)).collect();
        let _ = p.letter(&s, 2, 1.4, 30.0, 9, false);
        let b: Vec<_> = (0..5).map(|k| p.letter(&s, k, 1.13, 30.0, 9, false)).collect();
        assert_eq!(a, b);
        assert_eq!(p.settled_at(s.iter().copied()), s[4] + p.lock);
    }
}
