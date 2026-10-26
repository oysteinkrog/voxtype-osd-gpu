// GPU particles (CAPABILITIES PA01, PA03, PA04, PA06, PA07). The CPU side is `particles.rs`.
//
// Two pools share this file:
// - Free particles live in fixed-slot pools per emitter. `cs_step` advances all of them by one
//   fixed tick. Every random number is `rand (seed, emitter, slot, spawn)`, so a particle's
//   numbers depend only on its own counters, never on what ran before it.
// - Word particles do not step. Each one holds an anchor (start, start velocity, target, t0)
//   and its position at any time is a closed-form critically damped spring, evaluated at the
//   frame time in `vs_word`. `cs_anchor` re-anchors one word slot when its text changes, by
//   evaluating the old spring exactly at the change time.
//
// Positions are in the emitter's or the word's own space (logical units); the draw adds the
// origin and maps to pixels with `frame.m` and `frame.t`, as a layer block does.

const PI: f32 = 3.14159265;
const WG: u32 = 64u;

// Word particle flags.
const W_ACTIVE: u32 = 1u;
// Fresh: fades in at its first anchor.
const W_FRESH: u32 = 2u;
// Leaving: a target that went away; fades out where the scatter takes it.
const W_DYING: u32 = 4u;
// Target flags, set by the CPU per revision.
const T_ACTIVE: u32 = 1u;
const T_CHANGED: u32 = 2u;

// --- hashing (vo_core::hash, the same functions bit for bit) ---

fn pcg (v: u32) -> u32 {
  let state = v * 747796405u + 2891336453u;
  let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
  return (word >> 22u) ^ word;
}

fn rand (seed: u32, a: u32, b: u32, c: u32) -> u32 {
  return pcg (pcg (pcg (pcg (seed) ^ a) ^ b) ^ c);
}

fn to_unit (h: u32) -> f32 {
  return f32 (h >> 8u) * (1.0 / 16777216.0);
}

// --- noise ---

fn lattice (i: vec3i) -> f32 {
  return to_unit (rand (0x9e3779b9u, bitcast<u32> (i.x), bitcast<u32> (i.y), bitcast<u32> (i.z))) * 2.0 - 1.0;
}

// Value noise in 3D with quintic fade, about -1 to 1.
fn noise3 (p: vec3f) -> f32 {
  let i = vec3i (floor (p));
  let f = fract (p);
  let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
  let x00 = mix (lattice (i), lattice (i + vec3i (1, 0, 0)), u.x);
  let x10 = mix (lattice (i + vec3i (0, 1, 0)), lattice (i + vec3i (1, 1, 0)), u.x);
  let x01 = mix (lattice (i + vec3i (0, 0, 1)), lattice (i + vec3i (1, 0, 1)), u.x);
  let x11 = mix (lattice (i + vec3i (0, 1, 1)), lattice (i + vec3i (1, 1, 1)), u.x);
  return mix (mix (x00, x10, u.y), mix (x01, x11, u.y), u.z);
}

// Divergence-free 2D flow: the curl of a noise potential (Bridson et al. 2007), with time as
// the third axis. About unit magnitude.
fn curl (p: vec2f, t: f32) -> vec2f {
  let e = 0.05;
  let dx = noise3 (vec3f (p.x + e, p.y, t)) - noise3 (vec3f (p.x - e, p.y, t));
  let dy = noise3 (vec3f (p.x, p.y + e, t)) - noise3 (vec3f (p.x, p.y - e, t));
  return vec2f (dy, -dx) / (2.0 * e);
}

// --- free particles ---

struct Free {
  pos: vec2f,
  // Position one tick earlier, for interpolation between ticks at draw time.
  prev: vec2f,
  vel: vec2f,
  // Seconds since spawn; negative before the first spawn.
  age: f32,
  life: f32,
  spawn: u32,
  emitter: u32,
  // Per-life random in [0, 1) for size and colour variety.
  r: f32,
  _pad: f32,
};

// One emitter's simulation inputs at one tick.
struct EmitStep {
  // Ring radius, swirl in radians per second, level 0 to 1, curl noise strength.
  radius: f32,
  swirl: f32,
  level: f32,
  noise: f32,
  // Mean life in seconds, radial spread as a fraction of the radius, noise scale, seed.
  life: f32,
  spread: f32,
  scale: f32,
  seed: u32,
};

struct Step {
  // Index of this tick in the batch, into `emits`.
  index: u32,
  count: u32,
  emitters: u32,
  _pad: u32,
  t: f32,
  dt: f32,
  _pad2: vec2f,
};

// --- word particles ---

struct Word {
  start: vec2f,
  v0: vec2f,
  goal: vec2f,
  t0: f32,
  // Spring rate in 1/s and the start delay in seconds.
  w: f32,
  delay: f32,
  gen: u32,
  flags: u32,
  // Stroke size in logical units, for the sprite size.
  stroke: f32,
  // When the solid glyph this particle belongs to starts its reveal, and its delay. The CPU
  // computes the solid glyph's opacity from the same two numbers.
  reveal_t0: f32,
  reveal_delay: f32,
  _pad: vec2f,
};

struct Target {
  pos: vec2f,
  flags: u32,
  glyph: u32,
  reveal_t0: f32,
  reveal_delay: f32,
  _pad: vec2f,
};

struct Anchor {
  slot: u32,
  capacity: u32,
  gen: u32,
  seed: u32,
  t: f32,
  // Font size of the word and the spring rate.
  size: f32,
  w: f32,
  _pad: f32,
  // Word box in word space: x0, width; and the scatter speed in sizes per second.
  x0: f32,
  width: f32,
  scatter: f32,
  stroke: f32,
};

// Closed-form critically damped spring from (start, v0) at t0 + delay to `target`.
// Returns position and velocity.
struct Eval {
  x: vec2f,
  v: vec2f,
  // Remaining fraction of the start offset, (1 + w tau) e^(-w tau), 1 before the start.
  rem: f32,
  tau: f32,
};

fn eval_word (p: Word, t: f32) -> Eval {
  let tau = max (t - p.t0 - p.delay, 0.0);
  let d0 = p.start - p.goal;
  let e = exp (-p.w * tau);
  let b = p.v0 + p.w * d0;
  var out: Eval;
  out.x = p.goal + (d0 + b * tau) * e;
  out.v = (p.v0 - p.w * b * tau) * e;
  out.rem = (1.0 + p.w * tau) * e;
  out.tau = tau;
  return out;
}

// --- draw ---

struct Frame {
  // Layer space to physical pixels, as `common.wgsl` lays it out.
  m: vec4f,
  t: vec4f,
  // Target size in pixels; z: 1 to encode sRGB in the shader.
  viewport: vec4f,
  // Frame time, interpolation factor between the last two ticks, the stretch in seconds of
  // motion, and the reveal window start.
  time: f32,
  alpha: f32,
  stretch: f32,
  reveal0: f32,
  reveal1: f32,
  // Spring rate of the slowest particle, for the reveal.
  w_min: f32,
  _pad: vec2f,
};

// One emitter's look at the frame time.
struct EmitDraw {
  origin: vec2f,
  size: f32,
  brightness: f32,
  // Colour at birth, in the middle of life and at death: linear, straight alpha.
  c0: vec4f,
  c1: vec4f,
  c2: vec4f,
};

struct SlotDraw {
  origin: vec2f,
  // 1 shows the slot, 0 hides it.
  visible: f32,
  _pad: f32,
  color: vec4f,
};
