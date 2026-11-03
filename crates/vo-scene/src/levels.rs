//! The level history and colour stops that the waveform elements share (`bars`, `trace`,
//! `ribbon`, `segments`).
//!
//! [`Levels`] is a run of levels from 0 to 1, oldest first, evenly spaced in time, with an
//! attack and release follower so a picture rises fast and falls slowly. Every read is a pure
//! function of `t` and the fields: the follower runs over the whole history from the oldest
//! level, so keep about a second of history beyond what the element shows. The followed
//! history is kept per history and follower (a few at a time, per thread), so the many reads
//! of one frame run the follower once.
//!
//! [`Stops`] is a colour ramp: colours at positions from 0 to 1, mixed between in sRGB.

use std::cell::RefCell;
use std::sync::Arc;

use crate::geom::Color;

/// See the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct Levels {
    /// Levels from 0 to 1, oldest first, evenly spaced in time.
    pub levels: Arc<[f32]>,
    /// Scene time of the newest level. The picture moves until then and stays still after.
    pub newest_at: f64,
    /// Levels per second.
    pub rate: f32,
    /// Follower time constants in seconds: rising and falling.
    pub attack: f32,
    pub release: f32,
}

impl Default for Levels {
    fn default() -> Levels {
        Levels { levels: Arc::from([]), newest_at: 0.0, rate: 100.0, attack: 0.060, release: 0.200 }
    }
}

impl Levels {
    pub fn new(levels: impl Into<Arc<[f32]>>, newest_at: f64) -> Levels {
        Levels { levels: levels.into(), newest_at, ..Levels::default() }
    }

    /// The last `n` levels from the signal bus at `t`, 100 a second on a fixed grid of
    /// absolute time, ending at the newest frame that has arrived (or at `t`, when that is
    /// sooner). Rebuild it as frames arrive, as with any other `Levels`.
    pub fn from_signals(cell: &vo_core::signals::SignalsCell, t: f64, n: usize) -> Levels {
        let signals = cell.get();
        let t = signals.newest_at().map_or(t, |at| t.min(at + signals.playout()));
        let (levels, newest_at) = signals.history(vo_core::signals::Sig::Level, t, 100.0, n);
        Levels { levels: levels.into(), newest_at, rate: 100.0, ..Levels::default() }
    }

    /// Whether the history still scrolls at `t`.
    pub fn moving(&self, t: f64) -> bool {
        self.levels.len() > 1 && t < self.newest_at
    }

    /// Scene time of the oldest level.
    pub fn first_at(&self) -> f64 {
        self.newest_at - self.levels.len().saturating_sub(1) as f64 / self.rate.max(1e-3) as f64
    }

    /// The followed level at every input level.
    pub fn followed(&self) -> Vec<f32> {
        follow(&self.levels, self.rate, self.attack, self.release)
    }

    /// A reader of the followed level at any scene time: 0 before the oldest level, the
    /// newest level after the newest, linear between. Runs the follower at most once for a
    /// history and follower.
    pub fn reader(&self) -> Reader {
        Reader { followed: self.followed_shared(), first_at: self.first_at(), rate: self.rate.max(1e-3) as f64 }
    }

    /// [`Levels::followed`], from the cache when this history was followed with the same rate
    /// and time constants. The cache holds the history itself, so its address cannot be
    /// reused by another history while the entry lives.
    fn followed_shared(&self) -> Arc<[f32]> {
        const KEEP: usize = 8;
        let key = [self.rate.to_bits(), self.attack.to_bits(), self.release.to_bits()];
        FOLLOWED.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some(i) = cache.iter().position(|(l, k, _)| *k == key && Arc::ptr_eq(l, &self.levels)) {
                let hit = cache.remove(i);
                let followed = hit.2.clone();
                cache.insert(0, hit);
                return followed;
            }
            let followed: Arc<[f32]> = self.followed().into();
            cache.insert(0, (self.levels.clone(), key, followed.clone()));
            cache.truncate(KEEP);
            followed
        })
    }

    /// The followed level at `t`, clamped to the newest level.
    pub fn at(&self, t: f64) -> f32 {
        self.reader().at(t)
    }
}

type FollowedCache = Vec<(Arc<[f32]>, [u32; 3], Arc<[f32]>)>;

thread_local! {
    /// Followed histories, the last used first: `(history, follower key, followed)`.
    static FOLLOWED: RefCell<FollowedCache> = const { RefCell::new(Vec::new()) };
}

/// The followed levels of a [`Levels`], read at any time.
#[derive(Clone, Debug)]
pub struct Reader {
    followed: Arc<[f32]>,
    first_at: f64,
    rate: f64,
}

impl Reader {
    pub fn at(&self, at: f64) -> f32 {
        let n = self.followed.len();
        if n == 0 || at < self.first_at - 1e-9 {
            return 0.0;
        }
        let f = ((at - self.first_at) * self.rate).min((n - 1) as f64);
        let i = f.floor() as usize;
        let k = (f - i as f64) as f32;
        let a = self.followed[i];
        let b = self.followed[(i + 1).min(n - 1)];
        a + (b - a) * k
    }
}

/// An attack and release follower over `levels` (clamped to 0..1) at `rate` levels a second,
/// starting from the first level.
pub fn follow(levels: &[f32], rate: f32, attack: f32, release: f32) -> Vec<f32> {
    let dt = 1.0 / rate.max(1e-3);
    let up = 1.0 - (-dt / attack.max(1e-4)).exp();
    let down = 1.0 - (-dt / release.max(1e-4)).exp();
    let mut y = levels.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
    levels
        .iter()
        .map(|&x| {
            let x = x.clamp(0.0, 1.0);
            y += (x - y) * if x > y { up } else { down };
            y
        })
        .collect()
}

/// Colours at positions from 0 to 1, in rising order. Below the first stop the first colour,
/// above the last the last colour.
#[derive(Clone, Debug, PartialEq)]
pub struct Stops(pub Arc<[(f32, Color)]>);

impl Stops {
    pub fn new(stops: &[(f32, Color)]) -> Stops {
        Stops(Arc::from(stops))
    }

    /// Two colours, `a` at 0 and `b` at 1.
    pub fn two(a: Color, b: Color) -> Stops {
        Stops::new(&[(0.0, a), (1.0, b)])
    }

    pub fn at(&self, x: f32) -> Color {
        let s = &self.0;
        let Some(&(p0, c0)) = s.first() else { return Color::TRANSPARENT };
        if x <= p0 {
            return c0;
        }
        for w in s.windows(2) {
            let ((pa, ca), (pb, cb)) = (w[0], w[1]);
            if x <= pb {
                return mix(ca, cb, if pb > pa { (x - pa) / (pb - pa) } else { 1.0 });
            }
        }
        s[s.len() - 1].1
    }
}

/// `a` to `b` by `k`, straight alpha.
pub fn mix(a: Color, b: Color, k: f32) -> Color {
    let k = k.clamp(0.0, 1.0);
    Color::rgba(a.r + (b.r - a.r) * k, a.g + (b.g - a.g) * k, a.b + (b.b - a.b) * k, a.a + (b.a - a.a) * k)
}

/// 0 at `e0`, 1 at `e1`, smooth between.
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let u = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

#[cfg(test)]
mod tests {
    #[test]
    fn from_signals_ends_at_the_newest_frame() {
        use vo_core::signals::{SignalFrame, Signals, SignalsCell, FRAME_SECS};
        let frames = (0..50).map(|i| SignalFrame { rms_dbfs: -30.0, ..SignalFrame::silent(i as f64 * FRAME_SECS) });
        let cell = SignalsCell::new(Signals::recorded(frames, 0.0).with_playout(0.0));
        let l = Levels::from_signals(&cell, 3.0, 20);
        assert_eq!(l.levels.len(), 20);
        assert!((l.newest_at - 0.49).abs() < 1e-9, "{}", l.newest_at);
        assert!(l.levels.iter().all(|v| (v - 0.5).abs() < 1e-5));
    }

    use super::*;

    #[test]
    fn stops_mix_between_and_clamp_outside() {
        let s = Stops::new(&[(0.2, Color::BLACK), (0.6, Color::WHITE), (1.0, Color::hex(0xff0000, 1.0))]);
        let near = |a: Color, b: Color| (a.r - b.r).abs() + (a.g - b.g).abs() + (a.b - b.b).abs() < 1e-5;
        assert_eq!(s.at(0.0), Color::BLACK);
        assert!(near(s.at(0.4), Color::rgba(0.5, 0.5, 0.5, 1.0)));
        assert!(near(s.at(0.8), Color::rgba(1.0, 0.5, 0.5, 1.0)));
        assert_eq!(s.at(2.0), Color::hex(0xff0000, 1.0));
    }

    #[test]
    fn a_read_from_the_cache_equals_a_fresh_follow() {
        let l = Levels::new((0..300).map(|i| ((i as f32) * 0.37).sin().abs()).collect::<Vec<f32>>(), 3.0);
        let fresh = |l: &Levels, t: f64| {
            Reader { followed: l.followed().into(), first_at: l.first_at(), rate: l.rate as f64 }.at(t)
        };
        for k in 0..50 {
            let t = k as f64 * 0.07;
            assert_eq!(l.at(t).to_bits(), fresh(&l, t).to_bits(), "{t}");
        }
        // Another follower on the same history is followed again, not read from the cache.
        let slow = Levels { release: 0.9, ..l.clone() };
        assert_eq!(slow.at(2.5).to_bits(), fresh(&slow, 2.5).to_bits());
        assert_ne!(slow.at(2.5), l.at(2.5));
    }

    #[test]
    fn the_reader_holds_the_newest_level_and_is_silent_before_the_oldest() {
        let l = Levels { attack: 1e-4, release: 1e-4, ..Levels::new(vec![0.2, 0.4, 0.6], 1.0) };
        let r = l.reader();
        assert!((l.first_at() - 0.98).abs() < 1e-9);
        assert_eq!(r.at(0.5), 0.0);
        assert!((r.at(0.995) - 0.5).abs() < 1e-3);
        assert!((r.at(5.0) - 0.6).abs() < 1e-6);
        assert!(l.moving(0.99) && !l.moving(1.0));
    }
}
