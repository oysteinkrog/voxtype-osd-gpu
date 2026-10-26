// Shared by every pipeline of `Renderer3d`. Colours are linear light; vertex outputs carry
// premultiplied alpha. `finish` encodes to sRGB only when the target is a plain UNORM format,
// the same rule as vo-render's `common.wgsl`.

struct Globals {
  view_proj: mat4x4f,
  view: mat4x4f,
  // xyz: unit vector toward the light; w: rim strength.
  light_dir: vec4f,
  // Linear colour times intensity.
  light_color: vec4f,
  ambient: vec4f,
  // xyz: camera position.
  eye: vec4f,
  // Target width and height in pixels, focal length in pixels, 1 when `finish` encodes sRGB.
  viewport: vec4f,
  // Focus distance, range, largest blur in pixels, 1 when a text field is bound.
  focus: vec4f,
  // Fog start and end view distance, 1 when fog is on.
  fog: vec4f,
  // Background gradient, linear and premultiplied.
  bg_top: vec4f,
  bg_bottom: vec4f,
  // Field texture size in its own pixels, then target pixels per field pixel.
  field: vec4f,
};

@group(0) @binding(0) var<uniform> g: Globals;
// The text distance field (TY19): r signed distance in target pixels (negative inside the
// ink), gb unit direction away from the nearest ink, a coverage. A 1x1 far-away texel when no
// field is bound.
@group(0) @binding(1) var field_tex: texture_2d<f32>;
@group(0) @binding(2) var field_smp: sampler;

const TAU: f32 = 6.283185307;

// PCG hash, the same function as vo_core::hash::pcg, so CPU and GPU agree.
fn pcg (v: u32) -> u32 {
  let state = v * 747796405u + 2891336453u;
  let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
  return (word >> 22u) ^ word;
}

// vo_core::hash::rand_f32.
fn rand01 (seed: u32, a: u32, b: u32, c: u32) -> f32 {
  let h = pcg (pcg (pcg (pcg (seed) ^ a) ^ b) ^ c);
  return f32 (h >> 8u) * (1.0 / 16777216.0);
}

fn srgb_encode (c: vec3f) -> vec3f {
  let lo = c * 12.92;
  let hi = 1.055 * pow (c, vec3f (1.0 / 2.4)) - 0.055;
  return select (hi, lo, c <= vec3f (0.0031308));
}

fn finish (c: vec4f) -> vec4f {
  if (g.viewport.w == 0.0 || c.a <= 0.0) {
    return c;
  }
  let straight = clamp (c.rgb / c.a, vec3f (0.0), vec3f (1.0));
  return vec4f (srgb_encode (straight) * c.a, c.a);
}

// Fog factor at view distance `d`: 1 near, 0 past the end.
fn fog_at (d: f32) -> f32 {
  if (g.fog.z == 0.0) {
    return 1.0;
  }
  return 1.0 - smoothstep (g.fog.x, g.fog.y, d);
}

// Blur disc diameter in pixels for something at view distance `d`.
fn coc_px (d: f32) -> f32 {
  return g.focus.z * clamp (abs (d - g.focus.x) / max (g.focus.y, 1e-4), 0.0, 1.0);
}

// The field at a target pixel position: signed distance, direction away from ink, coverage.
fn field_at (px: vec2f) -> vec4f {
  if (g.focus.w == 0.0) {
    return vec4f (1e6, 0.0, 0.0, 0.0);
  }
  let uv = px / (g.field.xy * g.field.z);
  return textureSampleLevel (field_tex, field_smp, uv, 0.0);
}

// Clip position to target pixels (y down).
fn clip_to_px (c: vec4f) -> vec2f {
  let n = c.xy / c.w;
  return vec2f ((n.x * 0.5 + 0.5) * g.viewport.x, (0.5 - n.y * 0.5) * g.viewport.y);
}

// Moves a clip position by `d` target pixels.
fn clip_offset_px (c: vec4f, d: vec2f) -> vec4f {
  return c + vec4f (d.x * 2.0 / g.viewport.x, -d.y * 2.0 / g.viewport.y, 0.0, 0.0) * c.w;
}
