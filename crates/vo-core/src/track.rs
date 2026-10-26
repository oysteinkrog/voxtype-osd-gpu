//! Keyframe tracks for one-shot beats: a confirm pulse, an error shake, a title card.
//!
//! A track runs on a clip's local time, `t - start`, where `start` is the timestamp of the
//! event that started the clip. A track never writes a final value. It either sets a
//! channel's target at each keyframe (the spring does the easing, so a later event can still
//! interrupt it), or adds an eased offset on top of the channel's output.

/// CSS-style `cubic-bezier(x1, y1, x2, y2)` easing from `(0, 0)` to `(1, 1)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CubicBezier {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

impl CubicBezier {
    pub const LINEAR: CubicBezier = CubicBezier::new(0.0, 0.0, 1.0, 1.0);
    /// The `out` motion token.
    pub const OUT: CubicBezier = CubicBezier::new(0.2, 0.8, 0.2, 1.0);
    /// The `in` motion token.
    pub const IN: CubicBezier = CubicBezier::new(0.4, 0.0, 1.0, 1.0);

    /// `x1` and `x2` are clamped to `[0, 1]`, so time never runs backwards.
    pub const fn new(x1: f64, y1: f64, x2: f64, y2: f64) -> CubicBezier {
        CubicBezier { x1: x1.clamp(0.0, 1.0), y1, x2: x2.clamp(0.0, 1.0), y2 }
    }

    /// Eased progress for linear progress `p`. Exactly 0 at `p <= 0` and 1 at `p >= 1`.
    pub fn ease(&self, p: f64) -> f64 {
        if p <= 0.0 {
            return 0.0;
        }
        if p >= 1.0 {
            return 1.0;
        }
        let s = self.solve_x(p);
        bezier(self.y1, self.y2, s)
    }

    /// The curve parameter `s` with `x(s) = p`: Newton steps, then bisection if Newton
    /// stalls on a flat stretch.
    fn solve_x(&self, p: f64) -> f64 {
        let mut s = p;
        for _ in 0..8 {
            let err = bezier(self.x1, self.x2, s) - p;
            if err.abs() < 1e-12 {
                return s;
            }
            let d = bezier_slope(self.x1, self.x2, s);
            if d.abs() < 1e-9 {
                break;
            }
            s -= err / d;
        }
        let (mut lo, mut hi) = (0.0, 1.0);
        s = p;
        for _ in 0..64 {
            if bezier(self.x1, self.x2, s) < p {
                lo = s;
            } else {
                hi = s;
            }
            s = 0.5 * (lo + hi);
        }
        s
    }
}

/// One coordinate of the curve with end points 0 and 1.
fn bezier(c1: f64, c2: f64, s: f64) -> f64 {
    let u = 1.0 - s;
    3.0 * u * u * s * c1 + 3.0 * u * s * s * c2 + s * s * s
}

fn bezier_slope(c1: f64, c2: f64, s: f64) -> f64 {
    let u = 1.0 - s;
    3.0 * u * u * c1 + 6.0 * u * s * (c2 - c1) + 3.0 * s * s * (1.0 - c2)
}

/// A keyframe at local time `t`. `ease` shapes the stretch that ends here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub t: f64,
    pub value: f64,
    pub ease: CubicBezier,
}

impl Key {
    pub const fn new(t: f64, value: f64, ease: CubicBezier) -> Key {
        Key { t, value, ease }
    }
}

/// What a track does to its channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackMode {
    /// Each keyframe retargets the channel at `start + key.t`. Easing is ignored: the
    /// channel's spring moves between targets.
    Target,
    /// The eased keyframe value is added to the channel's sampled value.
    Offset,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    mode: TrackMode,
    keys: Vec<Key>,
}

impl Track {
    /// Keys are sorted by time. Panics on an empty list, which is an authoring error.
    pub fn new(mode: TrackMode, mut keys: Vec<Key>) -> Track {
        assert!(!keys.is_empty(), "a track needs at least one key");
        keys.sort_by(|a, b| a.t.total_cmp(&b.t));
        Track { mode, keys }
    }

    pub fn mode(&self) -> TrackMode {
        self.mode
    }

    pub fn keys(&self) -> &[Key] {
        &self.keys
    }

    /// Local time of the last key.
    pub fn duration(&self) -> f64 {
        self.keys.last().expect("never empty").t
    }

    /// The eased value at local time `lt`, holding the first and last key values outside the
    /// keyed range.
    pub fn value(&self, lt: f64) -> f64 {
        let i = self.keys.partition_point(|k| k.t <= lt);
        if i == 0 {
            return self.keys[0].value;
        }
        if i == self.keys.len() {
            return self.keys[i - 1].value;
        }
        let (a, b) = (self.keys[i - 1], self.keys[i]);
        let p = (lt - a.t) / (b.t - a.t);
        a.value + (b.value - a.value) * b.ease.ease(p)
    }

    /// For an offset track: what to add to the channel's value at time `t`, for a clip that
    /// started at `start`. Zero before the clip starts and for target tracks.
    pub fn offset_at(&self, start: f64, t: f64) -> f64 {
        if self.mode != TrackMode::Offset || t < start {
            return 0.0;
        }
        self.value(t - start)
    }

    /// For a target track: the `(t, target)` retargets, in time order, for a clip that
    /// started at `start`. The scene merges them into its event stream. Empty for offset
    /// tracks.
    pub fn target_events(&self, start: f64) -> impl Iterator<Item = (f64, f64)> + '_ {
        let keys = if self.mode == TrackMode::Target { &self.keys[..] } else { &[] };
        keys.iter().map(move |k| (start + k.t, k.value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::Channel;
    use crate::spring::Spring;

    const CURVES: [CubicBezier; 5] = [
        CubicBezier::LINEAR,
        CubicBezier::OUT,
        CubicBezier::IN,
        CubicBezier::new(0.0, 0.0, 0.0, 1.0),
        CubicBezier::new(0.68, -0.6, 0.32, 1.6),
    ];

    #[test]
    fn easing_endpoints_are_exact() {
        for c in CURVES {
            assert_eq!(c.ease(0.0), 0.0);
            assert_eq!(c.ease(1.0), 1.0);
            assert_eq!(c.ease(-0.5), 0.0);
            assert_eq!(c.ease(2.0), 1.0);
            // Continuous into the end points.
            assert!(c.ease(1e-9).abs() < 1e-6, "{c:?}");
            assert!((c.ease(1.0 - 1e-9) - 1.0).abs() < 1e-6, "{c:?}");
        }
    }

    #[test]
    fn easing_solves_x() {
        for c in CURVES {
            for i in 1..1000 {
                let p = i as f64 / 1000.0;
                let s = c.solve_x(p);
                assert!((bezier(c.x1, c.x2, s) - p).abs() < 1e-9, "{c:?} {p}");
            }
        }
        // Linear is the identity; out is ahead of linear, in behind it.
        for i in 1..100 {
            let p = i as f64 / 100.0;
            assert!((CubicBezier::LINEAR.ease(p) - p).abs() < 1e-9);
            assert!(CubicBezier::OUT.ease(p) > p);
            assert!(CubicBezier::IN.ease(p) < p);
        }
        // Monotone curves give monotone easing.
        let mut last = 0.0;
        for i in 0..=1000 {
            let e = CubicBezier::OUT.ease(i as f64 / 1000.0);
            assert!(e >= last);
            last = e;
        }
    }

    #[test]
    fn track_hits_its_keys() {
        let tr = Track::new(
            TrackMode::Offset,
            vec![
                Key::new(0.0, 0.0, CubicBezier::LINEAR),
                Key::new(0.05, -6.0, CubicBezier::OUT),
                Key::new(0.10, 4.0, CubicBezier::IN),
                Key::new(0.16, 0.0, CubicBezier::OUT),
            ],
        );
        for k in tr.keys() {
            assert_eq!(tr.value(k.t), k.value);
        }
        assert_eq!(tr.value(-1.0), 0.0);
        assert_eq!(tr.value(5.0), 0.0);
        assert_eq!(tr.duration(), 0.16);
        // Halfway into the `in` stretch, the value is behind linear.
        assert!(tr.value(0.075) < -1.0);
        assert_eq!(tr.offset_at(2.0, 1.9), 0.0);
        assert_eq!(tr.offset_at(2.0, 2.05), -6.0);
        assert_eq!(tr.target_events(0.0).count(), 0);
    }

    #[test]
    fn target_track_retargets_a_channel() {
        let tr = Track::new(
            TrackMode::Target,
            vec![Key::new(0.2, 1.0, CubicBezier::LINEAR), Key::new(0.0, 1.2, CubicBezier::LINEAR)],
        );
        let ev: Vec<_> = tr.target_events(3.0).collect();
        assert_eq!(ev, vec![(3.0, 1.2), (3.2, 1.0)]);
        assert_eq!(tr.offset_at(3.0, 3.1), 0.0);
        let mut ch = Channel::new(0.0, 1.0, Spring::SNAP);
        for (t, x) in ev {
            ch.retarget(t, x);
        }
        // A spring, not a jump: the value moves toward 1.2 and comes back to 1.0.
        assert_eq!(ch.value(3.0), 1.0);
        assert!(ch.value(3.1) > 1.1);
        assert_eq!(ch.value(10.0), 1.0);
    }
}
