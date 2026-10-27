// Noise library (FX10), shared by the shaders that need it and mirrored in Rust by
// `vo_core::noise`, so CPU and GPU agree to within float rounding.
//
// Every function is stateless: the result depends only on its arguments, including a `seed`,
// so a seek or a replay gives the same pattern. Names start with `noise_` so they do not
// clash with the helpers of the shaders this file is appended to.
//
// - `noise_pcg`, `noise_rand`, `noise_rand01`: the PCG hash and seeded random numbers, bit
//   for bit as `vo_core::hash`.
// - `noise_ign`: interleaved gradient noise in 0..1 at a pixel, for dither and per-pixel
//   rotation.
// - `noise_value2`: value noise in 0..1 on a unit grid.
// - `noise_simplex2`: 2D simplex noise in about -1..1, with its gradient. Its lattice
//   gradients turn by `turn` radians, so a loop over `turn` from 0 to 2 pi repeats exactly.
// - `noise_fbm2`, `noise_curl2`, `noise_warp2`: octaves of simplex, a divergence-free flow
//   field from it, and a domain warp.

fn noise_pcg (v: u32) -> u32 {
  let state = v * 747796405u + 2891336453u;
  let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
  return (word >> 22u) ^ word;
}

fn noise_rand (seed: u32, a: u32, b: u32, c: u32) -> u32 {
  return noise_pcg (noise_pcg (noise_pcg (noise_pcg (seed) ^ a) ^ b) ^ c);
}

// A random value in [0, 1) from the top 24 bits, so every value is exact.
fn noise_rand01 (seed: u32, a: u32, b: u32, c: u32) -> f32 {
  return f32 (noise_rand (seed, a, b, c) >> 8u) * (1.0 / 16777216.0);
}

// Interleaved gradient noise (Jimenez 2014) in [0, 1) at pixel `p`.
fn noise_ign (p: vec2f) -> f32 {
  return fract (52.9829189 * fract (dot (p, vec2f (0.06711056, 0.00583715))));
}

fn noise_lattice (seed: u32, i: vec2i) -> f32 {
  return noise_rand01 (seed, bitcast<u32> (i.x), bitcast<u32> (i.y), 0u);
}

// Value noise in [0, 1): random values on the integer lattice, blended with smoothstep.
fn noise_value2 (p: vec2f, seed: u32) -> f32 {
  let i = vec2i (floor (p));
  let f = fract (p);
  let u = f * f * (3.0 - 2.0 * f);
  let a = noise_lattice (seed, i);
  let b = noise_lattice (seed, i + vec2i (1, 0));
  let c = noise_lattice (seed, i + vec2i (0, 1));
  let d = noise_lattice (seed, i + vec2i (1, 1));
  return mix (mix (a, b, u.x), mix (c, d, u.x), u.y);
}

// The unit gradient at lattice point `i`, turned by `turn` radians.
fn noise_grad2 (seed: u32, i: vec2i, turn: f32) -> vec2f {
  let a = noise_rand01 (seed, bitcast<u32> (i.x), bitcast<u32> (i.y), 1u) * 6.2831853 + turn;
  return vec2f (cos (a), sin (a));
}

// 2D simplex noise and its gradient: `(value, d/dx, d/dy)`, the value in about -1..1.
fn noise_simplex2_grad (p: vec2f, seed: u32, turn: f32) -> vec3f {
  let f2 = 0.36602540378;
  let g2 = 0.21132486540;
  let i = floor (p + (p.x + p.y) * f2);
  let x0 = p - (i - (i.x + i.y) * g2);
  let o = select (vec2f (0.0, 1.0), vec2f (1.0, 0.0), x0.x > x0.y);
  let ii = vec2i (i);
  let offsets = array<vec2f, 3> (vec2f (0.0), o, vec2f (1.0));
  var out = vec3f (0.0);
  for (var k = 0; k < 3; k++) {
    let x = x0 - offsets[k] + f32 (k) * g2;
    let t = 0.5 - dot (x, x);
    if (t > 0.0) {
      let g = noise_grad2 (seed, ii + vec2i (offsets[k]), turn);
      let gx = dot (g, x);
      let t2 = t * t;
      let t4 = t2 * t2;
      // d/dx (t^4 (g . x)) = t^4 g - 8 t^3 (g . x) x
      out += vec3f (t4 * gx, t4 * g - 8.0 * t2 * t * gx * x);
    }
  }
  return 99.0 * out;
}

fn noise_simplex2 (p: vec2f, seed: u32, turn: f32) -> f32 {
  return noise_simplex2_grad (p, seed, turn).x;
}

// Fractal noise: `octaves` (1 to 8) of simplex, each at twice the frequency and half the
// amplitude of the last, turned against each other so their grids do not line up. About
// -1..1.
fn noise_fbm2 (p: vec2f, seed: u32, octaves: i32, turn: f32) -> f32 {
  let r = mat2x2f (vec2f (0.8, 0.6), vec2f (-0.6, 0.8));
  var q = p;
  var amp = 0.5;
  var sum = 0.0;
  var norm = 0.0;
  for (var i = 0; i < clamp (octaves, 1, 8); i++) {
    sum += amp * noise_simplex2 (q, seed + u32 (i), turn);
    norm += amp;
    q = r * q * 2.0;
    amp *= 0.5;
  }
  return sum / norm;
}

// A divergence-free flow at `p`: the curl of simplex noise, `(d/dy, -d/dx)`. Particles moved
// by it swirl without bunching up. Its length is up to about 10.
fn noise_curl2 (p: vec2f, seed: u32, turn: f32) -> vec2f {
  let n = noise_simplex2_grad (p, seed, turn);
  return vec2f (n.z, -n.y);
}

// `p` moved by two fbm fields `amount` units at most (Quilez's domain warp). Feed the result
// to another noise for marbled, flowing patterns.
fn noise_warp2 (p: vec2f, seed: u32, amount: f32, turn: f32) -> vec2f {
  let q = vec2f (noise_fbm2 (p, seed, 4, turn), noise_fbm2 (p + vec2f (5.2, 1.3), seed + 101u, 4, turn));
  return p + amount * q;
}
