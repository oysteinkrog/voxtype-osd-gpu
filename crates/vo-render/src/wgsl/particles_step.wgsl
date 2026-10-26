// `cs_step`, with `particles.wgsl` in front.

// --- compute: one fixed tick of the free pool ---

@group(0) @binding(0) var<uniform> step: Step;
@group(0) @binding(1) var<storage, read_write> free_rw: array<Free>;
@group(0) @binding(2) var<storage, read> emits: array<EmitStep>;
@group(0) @binding(3) var<storage, read_write> trail_rw: array<vec4f>;
@group(0) @binding(4) var mask_a: texture_2d<f32>;
@group(0) @binding(5) var mask_b: texture_2d<f32>;
@group(0) @binding(6) var mask_sampler: sampler;

struct Spawned {
  pos: vec3f,
  out: vec3f,
  tangent: vec3f,
};

fn spawn_at (e: EmitStep, h0: f32, h1: f32, h2: f32) -> Spawned {
  var s: Spawned;
  let a = h0 * 2.0 * PI;
  let dir = vec3f (cos (a), sin (a), 0.0);
  s.out = dir;
  s.tangent = vec3f (-dir.y, dir.x, 0.0);
  switch (e.spawn.x) {
    case S_RING: {
      s.pos = dir * e.shape.x * (1.0 + e.shape.y * (h1 - 0.5));
    }
    case S_DISC: {
      s.pos = dir * e.shape.x * sqrt (h1);
    }
    case S_LINE: {
      let along = vec3f (cos (e.shape.y), sin (e.shape.y), 0.0);
      s.pos = along * e.shape.x * (2.0 * h1 - 1.0);
      s.out = vec3f (-along.y, along.x, 0.0) * select (-1.0, 1.0, h2 < 0.5);
      s.tangent = along;
    }
    case S_BOX: {
      s.pos = (vec3f (h1, h2, fract (h1 * 7.31 + h2 * 3.17)) * 2.0 - 1.0) * e.shape.xyz;
    }
    case S_TORUS: {
      let v = h2 * 2.0 * PI;
      let r = e.shape.y * sqrt (h1);
      s.pos = dir * (e.shape.x + r * cos (v)) + vec3f (0.0, 0.0, r * sin (v));
      s.out = normalize (s.pos - dir * e.shape.x + vec3f (1e-5, 0.0, 0.0));
    }
    default: {
      s.pos = vec3f (0.0);
    }
  }
  return s;
}

// Coverage of the text mask at `p` (emitter space) and its gradient per unit.
fn mask_sample (p: vec2f, use_b: bool) -> vec3f {
  var rect = step.rect_a;
  if (use_b) {
    rect = step.rect_b;
  }
  if (rect.z <= 0.0 || rect.w <= 0.0) {
    return vec3f (0.0);
  }
  let uv = (p - rect.xy) / rect.zw;
  if (any (uv < vec2f (0.0)) || any (uv > vec2f (1.0))) {
    return vec3f (0.0);
  }
  var size = vec2f (textureDimensions (mask_a));
  if (use_b) {
    size = vec2f (textureDimensions (mask_b));
  }
  let du = vec2f (1.0 / size.x, 0.0);
  let dv = vec2f (0.0, 1.0 / size.y);
  var f: f32;
  var gx: f32;
  var gy: f32;
  if (use_b) {
    f = textureSampleLevel (mask_b, mask_sampler, uv, 0.0).r;
    gx = textureSampleLevel (mask_b, mask_sampler, uv + du, 0.0).r - textureSampleLevel (mask_b, mask_sampler, uv - du, 0.0).r;
    gy = textureSampleLevel (mask_b, mask_sampler, uv + dv, 0.0).r - textureSampleLevel (mask_b, mask_sampler, uv - dv, 0.0).r;
  } else {
    f = textureSampleLevel (mask_a, mask_sampler, uv, 0.0).r;
    gx = textureSampleLevel (mask_a, mask_sampler, uv + du, 0.0).r - textureSampleLevel (mask_a, mask_sampler, uv - du, 0.0).r;
    gy = textureSampleLevel (mask_a, mask_sampler, uv + dv, 0.0).r - textureSampleLevel (mask_a, mask_sampler, uv - dv, 0.0).r;
  }
  // Per texel to per unit.
  let g = vec2f (gx * size.x / rect.z, gy * size.y / rect.w) * 0.5;
  return vec3f (f, g);
}

@compute @workgroup_size(64)
fn cs_step (@builtin(global_invocation_id) gid: vec3u) {
  let i = gid.x;
  if (i >= step.count) {
    return;
  }
  var p = free_rw[i];
  let em = p.info.y;
  let e = emits[step.index * step.emitters + em];
  let local = i - e.pool.x;
  let dt = step.dt;
  var pos = p.pos.xyz;
  var vel = p.vel.xyz;
  var age = p.pos.w + dt;
  var life = p.prev.w;
  var r = p.vel.w;
  var flags = p.info.w;
  var spawn = p.info.x;
  var epoch = p.info.z;
  var prev = pos;
  // A burst (the emitter's epoch moved on) respawns the whole pool at this tick.
  let burst = epoch != e.spawn.z;
  if (burst || (age >= life && age >= 0.0)) {
    spawn += 1u;
    let k = spawn * 8u;
    let h0 = to_unit (rand (e.spawn.y, em, local, k));
    let h1 = to_unit (rand (e.spawn.y, em, local, k + 1u));
    let h2 = to_unit (rand (e.spawn.y, em, local, k + 2u));
    let h3 = to_unit (rand (e.spawn.y, em, local, k + 3u));
    let h4 = to_unit (rand (e.spawn.y, em, local, k + 4u));
    if (burst || h4 < e.life.z) {
      let s = spawn_at (e, h0, h1, h2);
      pos = s.pos;
      prev = pos;
      let rxy = length (pos.xy);
      vel = s.out * e.life.w * (0.5 + h3) + s.tangent * e.swirl.x * rxy * (0.6 + 0.8 * h3);
      // Carry the overshoot so spawns stay on the tick grid; a burst starts at 0.
      age = select (age - max (life, 0.0), 0.0, burst || life <= 0.0);
      life = e.life.x * (1.0 + e.life.y * (2.0 * h2 - 1.0));
      r = h3;
      flags |= F_ALIVE;
    } else {
      // Not this time: wait a while and try again, so the share of live particles follows
      // the emit setting.
      flags &= ~F_ALIVE;
      age = -e.life.x * 0.25 * h1;
      life = 0.0;
    }
    epoch = e.spawn.z;
    if (local < e.pool.w) {
      let base = (e.pool.z + local) * TRAIL;
      for (var k2 = 0u; k2 < TRAIL; k2++) {
        trail_rw[base + k2] = vec4f (pos, 1e9);
      }
    }
  }
  if ((flags & F_ALIVE) != 0u && age >= 0.0) {
    let rxy = length (pos.xy);
    let dir = select (vec3f (1.0, 0.0, 0.0), vec3f (pos.xy / rxy, 0.0), rxy > 1e-4);
    let tangent = vec3f (-dir.y, dir.x, 0.0);
    var acc = vec3f (0.0);
    // Swirl about z.
    if (e.swirl.x != 0.0) {
      acc += tangent * (e.swirl.x * rxy - dot (vel, tangent)) * 6.0;
    }
    // Ring pull: towards the circle of radius swirl.z in the xy plane (a torus core in 3D).
    if (e.swirl.y != 0.0) {
      acc += (dir * e.swirl.z - pos) * e.swirl.y;
    }
    // Curl noise.
    if (e.noise.x != 0.0) {
      let tn = step.t * e.noise.z + f32 (em) * 13.0;
      if ((e.spawn.w & E_3D) != 0u) {
        acc += curl3 (pos * e.noise.y, tn) * e.noise.x;
      } else {
        acc += vec3f (curl (pos.xy * e.noise.y, tn), 0.0) * e.noise.x;
      }
    }
    // Point attractor.
    if (e.attract.w != 0.0) {
      acc += (e.attract.xyz - pos) * e.attract.w;
    }
    acc += e.gravity.xyz;
    // Shockwave: a ring front moving out from the origin.
    if (e.shock.x >= 0.0) {
      let d = length (pos);
      let front = e.shock.x * e.shock.y;
      let x = (d - front) / max (e.shock.z, 1e-3);
      let out = select (vec3f (1.0, 0.0, 0.0), pos / d, d > 1e-4);
      acc += out * e.shock.w * exp (-x * x);
    }
    // Text mask (PA08).
    if (e.gravity.w != 0.0) {
      let m = mask_sample (pos.xy, step.tick >= step.mask_switch);
      let g = m.yz;
      let gl = length (g);
      if (gl > 1e-5) {
        if (e.gravity.w > 0.0) {
          // Avoid: out of the text, harder the deeper inside.
          acc -= vec3f (g / gl, 0.0) * e.gravity.w * m.x;
        } else {
          // Hug: towards the glyph edge, where coverage is one half.
          acc += vec3f (g / gl, 0.0) * (0.5 - m.x) * (-e.gravity.w);
        }
      }
    }
    vel = (vel + acc * dt) * exp (-e.swirl.w * dt);
    pos += vel * dt;
    if (local < e.pool.w) {
      trail_rw[(e.pool.z + local) * TRAIL + (step.tick + 1u) % TRAIL] = vec4f (pos, age);
    }
  }
  p.pos = vec4f (pos, age);
  p.prev = vec4f (prev, life);
  p.vel = vec4f (vel, r);
  p.info = vec4u (spawn, em, epoch, flags);
  free_rw[i] = p;
}
