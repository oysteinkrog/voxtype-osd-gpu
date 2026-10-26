// `cs_step`, with `particles.wgsl` in front.

// --- compute: one fixed tick of the free pool ---

@group(0) @binding(0) var<uniform> step: Step;
@group(0) @binding(1) var<storage, read_write> free_rw: array<Free>;
@group(0) @binding(2) var<storage, read> emits: array<EmitStep>;

@compute @workgroup_size(64)
fn cs_step (@builtin(global_invocation_id) gid: vec3u) {
  let i = gid.x;
  if (i >= step.count) {
    return;
  }
  var p = free_rw[i];
  let e = emits[step.index * step.emitters + p.emitter];
  let dt = step.dt;
  p.prev = p.pos;
  p.age += dt;
  if (p.age >= p.life) {
    // Respawn on the ring. Counter-based: this life's numbers come from the spawn count.
    let n = p.spawn + 1u;
    let h0 = to_unit (rand (e.seed, p.emitter, i, n * 4u));
    let h1 = to_unit (rand (e.seed, p.emitter, i, n * 4u + 1u));
    let h2 = to_unit (rand (e.seed, p.emitter, i, n * 4u + 2u));
    let h3 = to_unit (rand (e.seed, p.emitter, i, n * 4u + 3u));
    let a = h0 * 2.0 * PI;
    let dir = vec2f (cos (a), sin (a));
    let r = e.radius * (1.0 + e.spread * (h1 - 0.5));
    p.pos = dir * r;
    p.prev = p.pos;
    p.vel = vec2f (-dir.y, dir.x) * e.swirl * r * (0.6 + 0.8 * h3);
    // Carry the overshoot so the spawn time stays on the tick grid.
    p.age = select (p.age - p.life, 0.0, p.life <= 0.0);
    p.life = e.life * (0.5 + h2);
    p.spawn = n;
    p.r = h3;
  }
  if (p.age < 0.0) {
    free_rw[i] = p;
    return;
  }
  let d = length (p.pos);
  let dir = select (vec2f (1.0, 0.0), p.pos / d, d > 1e-4);
  let tangent = vec2f (-dir.y, dir.x);
  // Pull towards the ring, swirl along it, curl noise on top.
  var acc = dir * (e.radius - d) * 40.0;
  let v_t = dot (p.vel, tangent);
  acc += tangent * (e.swirl * d - v_t) * 6.0;
  acc += curl (p.pos * e.scale, step.t * 0.7 + f32 (p.emitter)) * e.noise;
  p.vel = (p.vel + acc * dt) * exp (-1.5 * dt);
  p.pos += p.vel * dt;
  free_rw[i] = p;
}
