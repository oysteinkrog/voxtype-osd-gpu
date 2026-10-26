//! Closed-form damped springs.
//!
//! A spring moves a value toward a target with `x'' = -k (x - target) - c x'` and unit mass.
//! The solution from any start state is closed form, so evaluating it at time `dt` after the
//! start needs no stepping and gives the same number at any frame rate. Units: seconds, and
//! whatever unit the value has (pixels, opacity).

/// Stiffness `k` and damping `c` of a unit-mass spring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub stiffness: f64,
    pub damping: f64,
}

/// `|ζ - 1|` below this is treated as critically damped, which avoids dividing by a tiny
/// damped frequency.
const CRITICAL_BAND: f64 = 1e-6;

impl Spring {
    /// Critically damped, about 120 ms from rest to 98% of a step.
    pub const SNAP: Spring = Spring::critical_to_98(0.120);
    /// Critically damped, about 220 ms to 98%.
    pub const SOFT: Spring = Spring::critical_to_98(0.220);
    /// Critically damped, about 360 ms to 98%.
    pub const SLOW: Spring = Spring::critical_to_98(0.360);
    /// Underdamped, damping ratio 0.7: about 4.6% overshoot, inside 2% of a step after
    /// about 280 ms.
    pub const POP: Spring = Spring::from_frequency(20.0, 0.7);

    /// The named presets, for token files and tests.
    pub const PRESETS: [(&'static str, Spring); 4] =
        [("snap", Spring::SNAP), ("soft", Spring::SOFT), ("slow", Spring::SLOW), ("pop", Spring::POP)];

    /// From natural frequency `omega` (rad/s) and damping ratio `zeta`.
    pub const fn from_frequency(omega: f64, zeta: f64) -> Spring {
        Spring { stiffness: omega * omega, damping: 2.0 * zeta * omega }
    }

    /// A critically damped spring that covers 98% of a step from rest in `secs`.
    /// `(1 + u) e^-u = 0.02` at `u = 5.8339`.
    pub const fn critical_to_98(secs: f64) -> Spring {
        Spring::from_frequency(5.833_9 / secs, 1.0)
    }

    /// Looks up a preset by its token name.
    pub fn preset(name: &str) -> Option<Spring> {
        Spring::PRESETS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
    }

    pub fn omega(&self) -> f64 {
        self.stiffness.sqrt()
    }

    pub fn damping_ratio(&self) -> f64 {
        self.damping / (2.0 * self.omega())
    }

    /// Offset and velocity `dt` seconds after starting at offset `y0` (value minus target)
    /// with velocity `v0`. Exact for any `dt >= 0`.
    pub fn eval(&self, y0: f64, v0: f64, dt: f64) -> (f64, f64) {
        match Solution::new(*self, y0, v0) {
            Solution::Under { sigma, wd, a, b } => {
                let e = (-sigma * dt).exp();
                let (s, c) = (wd * dt).sin_cos();
                let y = e * (a * c + b * s);
                let v = e * ((wd * b - sigma * a) * c - (sigma * b + wd * a) * s);
                (y, v)
            }
            Solution::Critical { w, a, b } => {
                let e = (-w * dt).exp();
                (e * (a + b * dt), e * (b - w * (a + b * dt)))
            }
            Solution::Over { r1, r2, c1, c2 } => {
                let (e1, e2) = ((r1 * dt).exp(), (r2 * dt).exp());
                (c1 * e1 + c2 * e2, r1 * c1 * e1 + r2 * c2 * e2)
            }
        }
    }

    /// The first `dt` after which offset and velocity stay inside `eps_x` and `eps_v` for good.
    /// Infinite when the spring has no damping.
    pub fn settle_time(&self, y0: f64, v0: f64, eps_x: f64, eps_v: f64) -> f64 {
        if y0 == 0.0 && v0 == 0.0 {
            return 0.0;
        }
        match Solution::new(*self, y0, v0) {
            Solution::Under { sigma, a, b, .. } => {
                // Offset and velocity are bounded by R e^-σt and R ω e^-σt, and they touch
                // those bounds once per half period, so this is tight.
                if sigma <= 0.0 {
                    return f64::INFINITY;
                }
                let r = a.hypot(b);
                let t = (r / eps_x).ln().max((r * self.omega() / eps_v).ln()) / sigma;
                t.max(0.0)
            }
            Solution::Critical { w, a, b } => {
                // y, v and acceleration are each (p + q t) e^-wt. After the last zero of all
                // three, |y| and |v| only shrink, so a bisection from there is exact.
                let d = b - w * a;
                let crossings = [linear_root(a, b), linear_root(d, -w * b), linear_root(-w * b - w * d, w * w * b)];
                self.settle_after(y0, v0, eps_x, eps_v, crossings)
            }
            Solution::Over { r1, r2, c1, c2 } => {
                let crossings = [
                    exp_root(c1, c2, r1, r2),
                    exp_root(r1 * c1, r2 * c2, r1, r2),
                    exp_root(r1 * r1 * c1, r2 * r2 * c2, r1, r2),
                ];
                self.settle_after(y0, v0, eps_x, eps_v, crossings)
            }
        }
    }

    /// Bisection for the settle time, from the point after which `|y|` and `|v|` only shrink.
    fn settle_after(&self, y0: f64, v0: f64, eps_x: f64, eps_v: f64, crossings: [f64; 3]) -> f64 {
        let inside = |dt: f64| {
            let (y, v) = self.eval(y0, v0, dt);
            y.abs() <= eps_x && v.abs() <= eps_v
        };
        let mut lo = crossings.into_iter().fold(0.0, f64::max);
        if inside(lo) {
            return lo;
        }
        let mut step = 1.0 / self.omega();
        let mut hi = lo + step;
        while !inside(hi) {
            lo = hi;
            step *= 2.0;
            hi += step;
        }
        for _ in 0..64 {
            let mid = 0.5 * (lo + hi);
            if inside(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi
    }
}

/// Root `t > 0` of `p + q t`, or 0.
fn linear_root(p: f64, q: f64) -> f64 {
    if q == 0.0 {
        0.0
    } else {
        (-p / q).max(0.0)
    }
}

/// Root `t > 0` of `p1 e^(r1 t) + p2 e^(r2 t)`, or 0.
fn exp_root(p1: f64, p2: f64, r1: f64, r2: f64) -> f64 {
    let ratio = -p2 / p1;
    if p1 == 0.0 || ratio <= 0.0 {
        0.0
    } else {
        (ratio.ln() / (r1 - r2)).max(0.0)
    }
}

/// Coefficients of the closed form for one start state.
enum Solution {
    /// `y = e^-σt (a cos ωd t + b sin ωd t)`
    Under { sigma: f64, wd: f64, a: f64, b: f64 },
    /// `y = (a + b t) e^-ωt`
    Critical { w: f64, a: f64, b: f64 },
    /// `y = c1 e^(r1 t) + c2 e^(r2 t)`, `r1` the slower root
    Over { r1: f64, r2: f64, c1: f64, c2: f64 },
}

impl Solution {
    fn new(s: Spring, y0: f64, v0: f64) -> Solution {
        let w = s.omega();
        let zeta = s.damping_ratio();
        if (zeta - 1.0).abs() < CRITICAL_BAND {
            Solution::Critical { w, a: y0, b: v0 + w * y0 }
        } else if zeta < 1.0 {
            let sigma = zeta * w;
            let wd = w * (1.0 - zeta * zeta).sqrt();
            Solution::Under { sigma, wd, a: y0, b: (v0 + sigma * y0) / wd }
        } else {
            let root = (zeta * zeta - 1.0).sqrt();
            // r1 written without the cancellation in -ω(ζ - sqrt(ζ² - 1)).
            let r1 = -w / (zeta + root);
            let r2 = -w * (zeta + root);
            let c2 = (v0 - r1 * y0) / (r2 - r1);
            Solution::Over { r1, r2, c1: y0 - c2, c2 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fourth-order Runge-Kutta with a fine fixed step, as the reference.
    fn integrate(s: Spring, y0: f64, v0: f64, t: f64, h: f64) -> (f64, f64) {
        let f = |y: f64, v: f64| (v, -s.stiffness * y - s.damping * v);
        let (mut y, mut v) = (y0, v0);
        let n = (t / h).round() as usize;
        for _ in 0..n {
            let (k1y, k1v) = f(y, v);
            let (k2y, k2v) = f(y + 0.5 * h * k1y, v + 0.5 * h * k1v);
            let (k3y, k3v) = f(y + 0.5 * h * k2y, v + 0.5 * h * k2v);
            let (k4y, k4v) = f(y + h * k3y, v + h * k3v);
            y += h / 6.0 * (k1y + 2.0 * k2y + 2.0 * k3y + k4y);
            v += h / 6.0 * (k1v + 2.0 * k2v + 2.0 * k3v + k4v);
        }
        (y, v)
    }

    fn regimes() -> [(&'static str, Spring); 5] {
        [
            ("under", Spring::from_frequency(20.0, 0.3)),
            ("pop", Spring::POP),
            ("critical", Spring::SOFT),
            ("over", Spring::from_frequency(15.0, 2.5)),
            ("near-critical", Spring::from_frequency(30.0, 0.999_99)),
        ]
    }

    #[test]
    fn closed_form_matches_numeric_integration() {
        for (name, s) in regimes() {
            for (y0, v0) in [(100.0, 0.0), (-40.0, 900.0), (0.0, -500.0)] {
                for t in [0.01, 0.05, 0.13, 0.4, 1.0] {
                    let (y, v) = s.eval(y0, v0, t);
                    let (yr, vr) = integrate(s, y0, v0, t, 1e-5);
                    assert!((y - yr).abs() < 1e-6, "{name} y0={y0} v0={v0} t={t}: y {y} vs {yr}");
                    assert!((v - vr).abs() < 1e-5, "{name} y0={y0} v0={v0} t={t}: v {v} vs {vr}");
                }
            }
        }
    }

    #[test]
    fn eval_at_zero_is_start_state() {
        for (_, s) in regimes() {
            let (y, v) = s.eval(12.5, -3.0, 0.0);
            assert!((y - 12.5).abs() < 1e-12 && (v + 3.0).abs() < 1e-9);
        }
    }

    #[test]
    fn settle_time_is_the_last_exit() {
        let (ex, ev) = (1e-3, 1e-2);
        for (name, s) in regimes() {
            for (y0, v0) in [(100.0, 0.0), (-40.0, 900.0), (0.0, -500.0), (5.0, -400.0)] {
                let ts = s.settle_time(y0, v0, ex, ev);
                assert!(ts.is_finite() && ts > 0.0, "{name}");
                // Inside from ts on, checked on a fine grid well past it.
                for i in 0..20_000 {
                    let (y, v) = s.eval(y0, v0, ts + i as f64 * 1e-4);
                    assert!(y.abs() <= ex * 1.000_001 && v.abs() <= ev * 1.000_001, "{name} {y0} {v0} at +{i}");
                }
                // Outside somewhere in the stretch just before it. Critical and over are
                // exact. Under uses the envelope, which the motion touches once per half
                // period, so look back half a period.
                let back = if s.damping_ratio() < 1.0 - CRITICAL_BAND {
                    std::f64::consts::PI / (s.omega() * (1.0 - s.damping_ratio().powi(2)).sqrt())
                } else {
                    1e-6
                };
                let outside = (1..=2000).any(|i| {
                    let (y, v) = s.eval(y0, v0, (ts - back * i as f64 / 2000.0).max(0.0));
                    y.abs() > ex || v.abs() > ev
                });
                assert!(outside, "{name} {y0} {v0}: settle time {ts} is later than needed");
            }
        }
    }

    #[test]
    fn preset_timings() {
        // A step of 1 from rest: the critical presets reach 98% at their named time.
        for (s, secs) in [(Spring::SNAP, 0.120), (Spring::SOFT, 0.220), (Spring::SLOW, 0.360)] {
            let (y, _) = s.eval(-1.0, 0.0, secs);
            assert!((y + 0.02).abs() < 1e-4, "{y}");
            assert!((s.damping_ratio() - 1.0).abs() < CRITICAL_BAND);
        }
        // Pop overshoots by about 4.6%.
        let peak = (1..1000).map(|i| Spring::POP.eval(-1.0, 0.0, i as f64 * 1e-3).0).fold(f64::MIN, f64::max);
        assert!((peak - 0.046).abs() < 0.002, "{peak}");
        assert_eq!(Spring::preset("soft"), Some(Spring::SOFT));
        assert_eq!(Spring::preset("nope"), None);
    }

    #[test]
    fn undamped_never_settles() {
        let s = Spring { stiffness: 100.0, damping: 0.0 };
        assert_eq!(s.settle_time(1.0, 0.0, 1e-3, 1e-3), f64::INFINITY);
    }
}
