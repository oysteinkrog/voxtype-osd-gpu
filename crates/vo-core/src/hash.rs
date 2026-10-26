//! Stateless randomness and stagger.
//!
//! Every random number is a hash of a seed and up to three counters, for example
//! `(seed, emitter, slot, spawn_tick)`. A particle's numbers then do not depend on how many
//! numbers anything else drew before it, so a seek, a replay or a render on another thread
//! gives the same result. Stateful generators are not used in motion or render code. The
//! hash is 32-bit so the same function runs in WGSL, which has no 64-bit integers.

/// PCG hash (Jarzynski and Olano, "Hash Functions for GPU Rendering", 2020).
pub const fn pcg(v: u32) -> u32 {
    let state = v.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let word = ((state >> ((state >> 28) + 4)) ^ state).wrapping_mul(277_803_737);
    (word >> 22) ^ word
}

/// A random `u32` from a seed and three counters.
pub const fn rand(seed: u32, a: u32, b: u32, c: u32) -> u32 {
    pcg(pcg(pcg(pcg(seed) ^ a) ^ b) ^ c)
}

/// A random `f32` in `[0, 1)`, from the top 24 bits so every value is exact.
pub fn rand_f32(seed: u32, a: u32, b: u32, c: u32) -> f32 {
    (rand(seed, a, b, c) >> 8) as f32 * (1.0 / 16_777_216.0)
}

/// A random `f64` in `[0, 1)`, from the same 32 bits.
pub fn rand_f64(seed: u32, a: u32, b: u32, c: u32) -> f64 {
    rand(seed, a, b, c) as f64 * (1.0 / 4_294_967_296.0)
}

/// Start delay of item `i` in a stagger of `gap` seconds per item, never later than `cap`.
/// Items past the cap all start at `cap`.
pub fn stagger(i: usize, gap: f64, cap: f64) -> f64 {
    (i as f64 * gap).min(cap)
}

/// Start delay of item `i` of `n`, with the gap shrunk so the last item starts at `cap` at
/// the latest. Keeps an even rhythm when many items arrive at once.
pub fn stagger_fit(i: usize, n: usize, gap: f64, cap: f64) -> f64 {
    if n < 2 {
        return 0.0;
    }
    i as f64 * gap.min(cap / (n - 1) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_fixed() {
        // Pinned so a change to the hash, which would change every seeded render and golden,
        // fails here first.
        assert_eq!(pcg(0), 129_708_002);
        assert_eq!(pcg(1), 2_831_084_092);
        assert_eq!(rand(0, 0, 0, 0), pcg(pcg(pcg(pcg(0)))));
        let pinned = [rand(1, 2, 3, 4), rand(42, 0, 7, 99), rand(u32::MAX, 1, 1, 1)];
        // Computed independently in Python from the published algorithm.
        assert_eq!(pinned, [1_693_730_102, 196_623_458, 339_540_928]);
    }

    #[test]
    fn every_argument_matters() {
        let base = rand(5, 6, 7, 8);
        assert_ne!(base, rand(4, 6, 7, 8));
        assert_ne!(base, rand(5, 7, 7, 8));
        assert_ne!(base, rand(5, 6, 8, 8));
        assert_ne!(base, rand(5, 6, 7, 9));
        // Swapped counters give different numbers.
        assert_ne!(rand(1, 2, 3, 0), rand(1, 3, 2, 0));
    }

    #[test]
    fn floats_are_uniform() {
        let n = 200_000u32;
        let mut buckets = [0u32; 16];
        let mut sum = 0.0f64;
        for i in 0..n {
            let x = rand_f32(9, i, 0, 0);
            assert!((0.0..1.0).contains(&x));
            buckets[(x * 16.0) as usize] += 1;
            sum += x as f64;
        }
        let mean = sum / n as f64;
        assert!((mean - 0.5).abs() < 0.005, "{mean}");
        let expected = n as f64 / 16.0;
        let chi2: f64 = buckets.iter().map(|&b| (b as f64 - expected).powi(2) / expected).sum();
        // 15 degrees of freedom: chi-square above 37.7 has a 0.1% chance.
        assert!(chi2 < 37.7, "{chi2} {buckets:?}");
        // Neighbouring counters are not correlated.
        let corr: f64 =
            (0..n).map(|i| (rand_f64(9, i, 0, 0) - 0.5) * (rand_f64(9, i + 1, 0, 0) - 0.5)).sum::<f64>() / n as f64;
        assert!(corr.abs() < 0.002, "{corr}");
    }

    #[test]
    fn stagger_caps() {
        assert_eq!(stagger(0, 0.035, 0.2), 0.0);
        assert!((stagger(3, 0.035, 0.2) - 0.105).abs() < 1e-12);
        assert_eq!(stagger(10, 0.035, 0.2), 0.2);
        // Ten words at 35 ms would need 315 ms; the gap shrinks to 200/9 ms.
        assert!((stagger_fit(9, 10, 0.035, 0.2) - 0.2).abs() < 1e-12);
        assert!((stagger_fit(1, 10, 0.035, 0.2) - 0.2 / 9.0).abs() < 1e-12);
        // Three words fit, so the gap stays.
        assert!((stagger_fit(2, 3, 0.035, 0.2) - 0.07).abs() < 1e-12);
        assert_eq!(stagger_fit(0, 1, 0.035, 0.2), 0.0);
    }
}
