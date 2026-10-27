//! Noise library (FX10): the Rust side of `vo-render`'s `noise.wgsl`, function for function,
//! so CPU code (the first frame, particles, tests) gets the patterns the shaders draw, to
//! within float rounding.
//!
//! Every function is stateless and seeded: the result depends only on its arguments, so a
//! seek or a replay gives the same pattern. Simplex lattice gradients turn by `turn` radians,
//! so a loop over `turn` from 0 to 2 pi repeats exactly, which suits ambient loops.

use crate::hash::rand;

/// A random value in `[0, 1)` from the top 24 bits, as `hash::rand_f32`.
fn rand01(seed: u32, a: u32, b: u32, c: u32) -> f32 {
    (rand(seed, a, b, c) >> 8) as f32 * (1.0 / 16_777_216.0)
}

/// Interleaved gradient noise (Jimenez 2014) in `[0, 1)` at pixel `p`.
pub fn ign(p: [f32; 2]) -> f32 {
    fract(52.982_918 * fract(p[0] * 0.067_110_56 + p[1] * 0.005_837_15))
}

fn fract(v: f32) -> f32 {
    v - v.floor()
}

fn lattice(seed: u32, i: [i32; 2]) -> f32 {
    rand01(seed, i[0] as u32, i[1] as u32, 0)
}

/// Value noise in `[0, 1)`: random values on the integer lattice, blended with smoothstep.
pub fn value2(p: [f32; 2], seed: u32) -> f32 {
    let i = [p[0].floor() as i32, p[1].floor() as i32];
    let f = [fract(p[0]), fract(p[1])];
    let u = f.map(|f| f * f * (3.0 - 2.0 * f));
    let a = lattice(seed, i);
    let b = lattice(seed, [i[0] + 1, i[1]]);
    let c = lattice(seed, [i[0], i[1] + 1]);
    let d = lattice(seed, [i[0] + 1, i[1] + 1]);
    let mix = |x: f32, y: f32, t: f32| x + (y - x) * t;
    mix(mix(a, b, u[0]), mix(c, d, u[0]), u[1])
}

/// The unit gradient at lattice point `i`, turned by `turn` radians.
fn grad2(seed: u32, i: [i32; 2], turn: f32) -> [f32; 2] {
    let a = rand01(seed, i[0] as u32, i[1] as u32, 1) * std::f32::consts::TAU + turn;
    [a.cos(), a.sin()]
}

/// 2D simplex noise and its gradient: `[value, d/dx, d/dy]`, the value in about -1..1.
pub fn simplex2_grad(p: [f32; 2], seed: u32, turn: f32) -> [f32; 3] {
    const F2: f32 = 0.366_025_4;
    const G2: f32 = 0.211_324_87;
    let s = (p[0] + p[1]) * F2;
    let i = [(p[0] + s).floor(), (p[1] + s).floor()];
    let t0 = (i[0] + i[1]) * G2;
    let x0 = [p[0] - (i[0] - t0), p[1] - (i[1] - t0)];
    let o = if x0[0] > x0[1] { [1.0, 0.0] } else { [0.0, 1.0] };
    let ii = [i[0] as i32, i[1] as i32];
    let mut out = [0.0f32; 3];
    for (k, off) in [[0.0, 0.0], o, [1.0, 1.0]].into_iter().enumerate() {
        let x = [x0[0] - off[0] + k as f32 * G2, x0[1] - off[1] + k as f32 * G2];
        let t = 0.5 - (x[0] * x[0] + x[1] * x[1]);
        if t > 0.0 {
            let g = grad2(seed, [ii[0] + off[0] as i32, ii[1] + off[1] as i32], turn);
            let gx = g[0] * x[0] + g[1] * x[1];
            let t2 = t * t;
            let t4 = t2 * t2;
            out[0] += t4 * gx;
            out[1] += t4 * g[0] - 8.0 * t2 * t * gx * x[0];
            out[2] += t4 * g[1] - 8.0 * t2 * t * gx * x[1];
        }
    }
    out.map(|v| 99.0 * v)
}

/// 2D simplex noise in about -1..1.
pub fn simplex2(p: [f32; 2], seed: u32, turn: f32) -> f32 {
    simplex2_grad(p, seed, turn)[0]
}

/// Fractal noise: `octaves` (1 to 8) of simplex, each at twice the frequency and half the
/// amplitude of the last, turned against each other so their grids do not line up. About
/// -1..1.
pub fn fbm2(p: [f32; 2], seed: u32, octaves: i32, turn: f32) -> f32 {
    let mut q = p;
    let (mut amp, mut sum, mut norm) = (0.5f32, 0.0f32, 0.0f32);
    for i in 0..octaves.clamp(1, 8) {
        sum += amp * simplex2(q, seed.wrapping_add(i as u32), turn);
        norm += amp;
        q = [(0.8 * q[0] - 0.6 * q[1]) * 2.0, (0.6 * q[0] + 0.8 * q[1]) * 2.0];
        amp *= 0.5;
    }
    sum / norm
}

/// A divergence-free flow at `p`: the curl of simplex noise, `[d/dy, -d/dx]`. Particles
/// moved by it swirl without bunching up. Its length is up to about 10.
pub fn curl2(p: [f32; 2], seed: u32, turn: f32) -> [f32; 2] {
    let n = simplex2_grad(p, seed, turn);
    [n[2], -n[1]]
}

/// `p` moved by two fbm fields `amount` units at most (Quilez's domain warp). Feed the
/// result to another noise for marbled, flowing patterns.
pub fn warp2(p: [f32; 2], seed: u32, amount: f32, turn: f32) -> [f32; 2] {
    let qx = fbm2(p, seed, 4, turn);
    let qy = fbm2([p[0] + 5.2, p[1] + 1.3], seed.wrapping_add(101), 4, turn);
    [p[0] + amount * qx, p[1] + amount * qy]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> impl Iterator<Item = [f32; 2]> {
        (0..4000).map(|i| [(i % 80) as f32 * 0.173 - 6.0, (i / 80) as f32 * 0.191 - 4.0])
    }

    #[test]
    fn ranges_hold() {
        for p in grid() {
            let s = simplex2(p, 7, 0.3);
            assert!((-1.0..=1.0).contains(&s), "simplex {s} at {p:?}");
            let v = value2(p, 7);
            assert!((0.0..1.0).contains(&v));
            let f = fbm2(p, 7, 5, 0.0);
            assert!((-1.0..=1.0).contains(&f));
            assert!((0.0..1.0).contains(&ign(p)));
        }
    }

    #[test]
    fn simplex_is_zero_on_the_lattice_and_varies_between() {
        // Gradient noise is zero at the skewed lattice points.
        assert!(simplex2([0.0, 0.0], 1, 0.0).abs() < 1e-6);
        let spread = grid().map(|p| simplex2(p, 1, 0.0)).fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(spread > 0.5, "{spread}");
    }

    #[test]
    fn the_gradient_matches_finite_differences() {
        let h = 1e-3;
        for p in grid().step_by(37) {
            let g = simplex2_grad(p, 3, 1.0);
            let dx = (simplex2([p[0] + h, p[1]], 3, 1.0) - simplex2([p[0] - h, p[1]], 3, 1.0)) / (2.0 * h);
            let dy = (simplex2([p[0], p[1] + h], 3, 1.0) - simplex2([p[0], p[1] - h], 3, 1.0)) / (2.0 * h);
            assert!((g[1] - dx).abs() < 0.05 && (g[2] - dy).abs() < 0.05, "{g:?} vs {dx} {dy} at {p:?}");
        }
    }

    #[test]
    fn a_turn_loops_and_seeds_differ() {
        let p = [1.37, -2.11];
        let full = simplex2(p, 5, std::f32::consts::TAU);
        assert!((simplex2(p, 5, 0.0) - full).abs() < 1e-4);
        assert_ne!(simplex2(p, 5, 0.0), simplex2(p, 6, 0.0));
        assert!((simplex2(p, 5, 0.0) - simplex2(p, 5, 1.0)).abs() > 1e-3);
    }

    #[test]
    fn curl_has_no_divergence() {
        let h = 1e-3;
        for p in grid().step_by(53) {
            let c = |q: [f32; 2]| curl2(q, 9, 0.0);
            let div = (c([p[0] + h, p[1]])[0] - c([p[0] - h, p[1]])[0]) / (2.0 * h)
                + (c([p[0], p[1] + h])[1] - c([p[0], p[1] - h])[1]) / (2.0 * h);
            assert!(div.abs() < 0.5, "divergence {div} at {p:?}");
        }
    }

    #[test]
    fn warp_moves_points_at_most_by_its_amount() {
        for p in grid().step_by(11) {
            let w = warp2(p, 2, 0.5, 0.0);
            assert!((w[0] - p[0]).abs() <= 0.5 && (w[1] - p[1]).abs() <= 0.5);
        }
    }
}
