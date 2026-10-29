// The chrome hero's environment (EG-17, `env.rs`): one equirectangular texture, 256 x 128,
// built by one full-screen triangle. Column u is the direction's longitude about the vertical
// axis, 0.5 facing the viewer (+z); row v its angle from up (-y, since y is down). Every
// texel holds the studio sky (the same gradient `sdf_hero.wgsl` draws without an
// environment). The front half (directions toward the viewer) also holds the popup's own
// layers under the hero, from its layer's backdrop (EG-01), spread over the front half like
// an azimuthal map: direction d sees the plane at the hero's centre plus a distance along
// d.xy that grows evenly with d's angle from the viewer, from 0 facing the viewer to the reach
// at the horizon, where it fades into the sky.

struct EnvBuild {
  // The hero's centre in surface pixels, the reach in surface pixels, the capture strength.
  centre: vec4f,
  // The backdrop texture's top left in surface pixels, surface pixels per texel, 1 when the
  // backdrop is bound.
  backdrop: vec4f,
  // Studio top, bottom and lights, linear (the chrome look's c1, c2, c3).
  c1: vec4f,
  c2: vec4f,
  c3: vec4f,
};

@group(0) @binding(0) var<uniform> env: EnvBuild;
@group(0) @binding(1) var env_backdrop: texture_2d<f32>;
@group(0) @binding(2) var env_sampler: sampler;

@vertex fn vs_env (@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
  let c = vec2f (f32 ((vi << 1u) & 2u), f32 (vi & 2u));
  return vec4f (c * 2.0 - 1.0, 0.0, 1.0);
}

// The direction a texel's centre stands for: the inverse of `sdf_hero.wgsl`'s `env_uv`.
fn env_dir (uv: vec2f) -> vec3f {
  let lon = (uv.x - 0.5) * 6.2831853;
  let lat = uv.y * 3.14159265;
  let s = sin (lat);
  return vec3f (s * sin (lon), -cos (lat), s * cos (lon));
}

// Kept the same as `studio` in sdf_hero.wgsl (a unit test compares them), with the colours
// from this block.
fn studio (r: vec3f) -> vec3f {
  let up = -r.y;
  var c = mix (env.c2.rgb, env.c1.rgb, smoothstep (-0.6, 0.8, up));
  c += env.c3.rgb * 0.6 * exp (-pow (up / 0.05, 2.0));
  let strip = exp (-pow ((up - 0.45) / 0.08, 2.0)) * smoothstep (0.95, 0.2, abs (r.x + 0.15));
  let side = exp (-pow ((r.x - 0.75) / 0.07, 2.0)) * smoothstep (-0.4, 0.3, up) * step (0.0, r.z);
  let low = exp (-pow ((up + 0.35) / 0.12, 2.0)) * 0.35;
  return c + env.c3.rgb * (2.4 * strip + 1.6 * side + low);
}

// The backdrop at surface pixel `p`, bilinear, transparent past the capture.
fn backdrop_at (p: vec2f) -> vec4f {
  let size = vec2f (textureDimensions (env_backdrop));
  let tc = (p - env.backdrop.xy) / env.backdrop.z;
  if (any (tc < vec2f (0.0)) || any (tc > size)) {
    return vec4f (0.0);
  }
  return textureSampleLevel (env_backdrop, env_sampler, tc / size, 0.0);
}

@fragment fn fs_env (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let uv = frag.xy / vec2f (256.0, 128.0);
  let d = env_dir (uv);
  let sky = studio (d);
  if (env.backdrop.w < 0.5 || d.z <= 0.0) {
    return vec4f (sky, 1.0);
  }
  // The plane seen this way, box filtered over the texel's footprint there (four bilinear
  // taps): the capture held at the front half's resolution.
  let side = length (d.xy);
  let away = select (vec2f (0.0), d.xy / side, side > 1e-6);
  let p = env.centre.xy + away * env.centre.z * acos (clamp (d.z, -1.0, 1.0)) / 1.5707963;
  let foot = 0.25 * env.centre.z / 64.0;
  let b = 0.25 * (backdrop_at (p + vec2f (-foot, -foot)) + backdrop_at (p + vec2f (foot, -foot))
    + backdrop_at (p + vec2f (-foot, foot)) + backdrop_at (p + vec2f (foot, foot)));
  let seen = b.rgb + (1.0 - b.a) * sky;
  let w = env.centre.w * smoothstep (0.0, 0.2, d.z);
  return vec4f (mix (sky, seen, w), 1.0);
}
