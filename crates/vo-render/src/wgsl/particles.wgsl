// GPU particles (CAPABILITIES PA01 to PA09, D302, FX16). The CPU side is `particles.rs`.
//
// Two pools share this file:
// - Free particles live in fixed-slot pools, one per emitter. `cs_step` advances all of them by
//   one fixed tick. Every random number is `rand (seed, emitter, slot, counter)`, so a particle's
//   numbers depend only on its own counters, never on what ran before it. Positions are 3D in the
//   emitter's own space; a 2D emitter keeps z at 0.
// - Word and point particles do not step. Each holds an anchor (start, start velocity, goal,
//   t0) and its position at any time is a closed-form critically damped spring, evaluated at
//   the frame time in `vs_word`. `cs_anchor` re-anchors one slot when its targets change, by
//   evaluating the old spring exactly at the change time.
//
// Positions are logical units. The draw adds the origin and maps to pixels with the frame's 2D
// affine (`frame.m`, `frame.t`, as a layer block lays it out) or, for 3D emitters, with
// `frame.view_proj`.

const PI: f32 = 3.14159265;
const WG: u32 = 64u;
// Ticks of history per hero particle, for ribbons (PA07).
const TRAIL: u32 = 16u;

// Free particle flags (`Free::info.w`).
const F_ALIVE: u32 = 1u;

// Emitter shapes (`EmitStep::spawn.x`).
const S_POINT: u32 = 0u;
const S_RING: u32 = 1u;
const S_DISC: u32 = 2u;
const S_LINE: u32 = 3u;
const S_BOX: u32 = 4u;
const S_TORUS: u32 = 5u;
// Along a path, in strands (`Shape::Path`).
const S_PATH: u32 = 6u;
// Points per resampled path, and segments per ribbon.
const PATH_N: u32 = 128u;
const RIBBON_SEGS: u32 = 96u;

// Emitter flags (`EmitStep::spawn.w`, `EmitDraw::pool.w` high bits).
const E_3D: u32 = 1u;

// Word particle flags.
const W_ACTIVE: u32 = 1u;
// Fresh: fades in at its first anchor.
const W_FRESH: u32 = 2u;
// Leaving: a target that went away; fades out where the scatter takes it.
const W_DYING: u32 = 4u;
// Stays visible once formed: point shapes and dots, which have no solid text to hand over to.
const W_PERSIST: u32 = 8u;
// Target flags, set by the CPU per change.
const T_ACTIVE: u32 = 1u;
const T_CHANGED: u32 = 2u;
const T_PERSIST: u32 = 4u;
// The particle starts at `Target::start` instead of a cloud around its goal.
const T_START: u32 = 8u;
// Held as particles: the glyph forms but does not hand over until it is set again.
const T_HELD: u32 = 16u;
// A reveal delay that never runs out.
const HELD_DELAY: f32 = 1.0e9;

// Sprite styles.
const ST_SOFT: u32 = 0u;
const ST_DOT: u32 = 1u;
const ST_BOKEH: u32 = 2u;
// Points of light: a crisp Gaussian core whose energy is kept, so a point smaller than a pixel
// gets dimmer, not blurrier.
const ST_SPARK: u32 = 3u;
// A glowing ribbon: a sharp core with a faint wide halo, across the ribbon only.
const ST_RIBBON: u32 = 4u;

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

// Divergence-free 3D flow: the curl of three noise potentials, drifting with time.
fn curl3 (p: vec3f, t: f32) -> vec3f {
  let e = 0.05;
  let q1 = p + vec3f (0.0, 0.0, t);
  let q2 = p + vec3f (31.4, 0.0, -t);
  let q3 = p + vec3f (0.0, 47.2, t);
  let dx = vec3f (e, 0.0, 0.0);
  let dy = vec3f (0.0, e, 0.0);
  let dz = vec3f (0.0, 0.0, e);
  let p3y = noise3 (q3 + dy) - noise3 (q3 - dy);
  let p2z = noise3 (q2 + dz) - noise3 (q2 - dz);
  let p1z = noise3 (q1 + dz) - noise3 (q1 - dz);
  let p3x = noise3 (q3 + dx) - noise3 (q3 - dx);
  let p2x = noise3 (q2 + dx) - noise3 (q2 - dx);
  let p1y = noise3 (q1 + dy) - noise3 (q1 - dy);
  return vec3f (p3y - p2z, p1z - p3x, p2x - p1y) / (2.0 * e);
}

// --- paths ---

// Strand `k`'s sideways offset at arc length `s` of a path `len` long at time `t`: a wave
// with its own amplitude, wavelength and phase, tapering to 0 at both ends. `w` is
// `EmitStep::path`.
fn strand_offset (k: f32, s: f32, len: f32, t: f32, w: vec4f) -> f32 {
  let kk = u32 (k);
  let h1 = to_unit (rand (0x51ab7u, kk, 1u, 0u));
  let h2 = to_unit (rand (0x51ab7u, kk, 2u, 0u));
  let h3 = to_unit (rand (0x51ab7u, kk, 3u, 0u));
  let u = clamp (s / max (len, 1e-3), 0.0, 1.0);
  let env = pow (sin (PI * u), 0.8);
  let amp = w.y * (0.45 + 0.55 * h1) * select (-1.0, 1.0, h2 < 0.5);
  let lambda = max (w.z * (0.75 + 0.5 * h2), 1.0);
  return env * amp * sin (2.0 * PI * (s / lambda - w.w * t + h3));
}

// A point on a resampled path (`a`, `b` the samples around it, `f` between them), with its
// unit normal: x, y and the normal's x, y.
fn path_point (a: vec4f, b: vec4f, f: f32) -> vec4f {
  let d = b.xy - a.xy;
  let l = length (d);
  let tangent = select (vec2f (1.0, 0.0), d / l, l > 1e-6);
  return vec4f (mix (a.xy, b.xy, f), -tangent.y, tangent.x);
}

// --- free particles ---

struct Free {
  // xyz position, w seconds since spawn (negative before the first spawn).
  pos: vec4f,
  // xyz position one tick earlier, for interpolation at draw time; w life in seconds.
  prev: vec4f,
  // xyz velocity; w a per-life random in [0, 1) for size and colour variety.
  vel: vec4f,
  // x spawn count, y emitter, z burst epoch seen, w flags.
  info: vec4u,
};

// One emitter's simulation inputs at one tick (`EmitterSim` in particles.rs).
struct EmitStep {
  // x shape, y seed, z burst epoch, w flags.
  spawn: vec4u,
  // Shape parameters: ring (radius, spread), disc (radius), line (half length, angle),
  // box (half x, y, z), torus (major radius, minor radius).
  shape: vec4f,
  // x mean life, y life jitter (fraction), z share of deaths that respawn, w spawn speed.
  life: vec4f,
  // x swirl in rad/s about z, y ring pull, z ring pull radius, w drag per second.
  swirl: vec4f,
  // x curl strength, y curl scale, z curl time speed, w the first tick that steps the pool.
  noise: vec4f,
  // xyz attractor, w its strength (negative repels).
  attract: vec4f,
  // xyz gravity plus wind, w mask strength (PA08: positive avoids text, negative hugs edges).
  gravity: vec4f,
  // x seconds since a shockwave (negative: none), y front speed, z front width, w strength.
  shock: vec4f,
  // x pool base, y pool capacity, z trail base, w trail count.
  pool: vec4u,
  // Path emitters: x strands, y sway, z wavelength, w wave speed in cycles per second.
  path: vec4f,
};

struct Step {
  // Index of this tick in the batch, into `emits`.
  index: u32,
  count: u32,
  emitters: u32,
  // The tick being computed: the state after it is the state at (tick + 1) * dt.
  tick: u32,
  t: f32,
  dt: f32,
  // Ticks before this one sample mask A, the rest mask B.
  mask_switch: u32,
  _pad: u32,
  // Mask rectangles in emitter space: x, y, w, h.
  rect_a: vec4f,
  rect_b: vec4f,
};

// --- word and point particles ---

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
  // Where the particle starts when `T_START` is set.
  start: vec2f,
};

struct Anchor {
  slot: u32,
  capacity: u32,
  gen: u32,
  seed: u32,
  t: f32,
  // Size of the shape (font size for words) and the spring rate.
  size: f32,
  w: f32,
  _pad: f32,
  // Box in slot space: x0, width; the scatter speed in sizes per second; the stroke.
  x0: f32,
  width: f32,
  scatter: f32,
  stroke: f32,
};

// Closed-form critically damped spring from (start, v0) at t0 + delay to `goal`.
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
  // motion, and the spring rate of the slowest word particle (for the reveal).
  time: f32,
  alpha: f32,
  stretch: f32,
  w_min: f32,
  reveal0: f32,
  reveal1: f32,
  // Ticks run so far, and the ribbon length in ticks.
  tick: u32,
  trail_len: u32,
  // World (layer units, z towards the viewer) to clip space, for 3D emitters.
  view_proj: mat4x4f,
  // x: 1 when 3D is on, y: focal length in pixels (sprite size at depth), z: 1 for alpha-over
  // blending (colours are drawn opaque-ish instead of as light).
  cam: vec4f,
};

// One emitter's look at the frame time (`EmitterLook`).
struct EmitDraw {
  // xyz origin, w sprite radius in logical units.
  origin: vec4f,
  // Rotation rows applied to emitter space; w: brightness, unused, unused.
  rot0: vec4f,
  rot1: vec4f,
  rot2: vec4f,
  // Colour at birth, in the middle of life and at death: linear, straight alpha.
  c0: vec4f,
  c1: vec4f,
  c2: vec4f,
  // x style; dots: y pitch in pixels; bokeh: z focus depth (clip w), w aperture (blur pixels
  // per unit of depth away from focus); sparks: y accent share, z gain, w twinkle.
  look: vec4f,
  // x ribbon width as a share of the sprite, y ribbon opacity, z colour by: 0 age, 1 angle.
  trail: vec4f,
  // x pool base, y capacity, z trail base, w flags.
  pool: vec4u,
  // As `EmitStep::path`, from the last tick.
  path: vec4f,
  // x ribbons, y ribbon core width in units, z ribbon opacity.
  ribbon: vec4f,
};

struct SlotDraw {
  origin: vec2f,
  // 1 shows the slot, 0 hides it.
  visible: f32,
  // Sprite radius in logical units; 0 for one from the stroke.
  size: f32,
  color: vec4f,
  // x style, y dot pitch in pixels, z ink per particle in px² (the light a particle carries
  // at full strength, set by the CPU so the formed word matches the solid word's light).
  look: vec4f,
};
