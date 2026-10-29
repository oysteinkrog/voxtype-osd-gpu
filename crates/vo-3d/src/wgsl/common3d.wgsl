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
  // Fog start and end view distance, 1 when fog is on, then physical pixels per logical
  // pixel: every pixel size in a frame (blur, minimum sprite size, field reach) is logical.
  fog: vec4f,
  // Background gradient, linear and premultiplied.
  bg_top: vec4f,
  bg_bottom: vec4f,
  // Field texture size in its own pixels, then target pixels per field pixel.
  field: vec4f,
  // Floor (D305): height, fade distance below it, reflection strength, 1 when there is one.
  floor: vec4f,
  // Shadows (EG-18): world to the light's clip space.
  light_view_proj: mat4x4f,
  // 1 when shadows are on, 1 / map size, softness in target pixels, strength.
  shadow: vec4f,
  // World size of a target pixel: x + y times view depth (x for orthographic cameras, y for
  // perspective ones). Then the world size of a map texel, and the largest disc radius in
  // map uv (the free margin round the casters).
  shadow_k: vec4f,
  // The half drawn (EG-20): keep world points where dot (xyz, p) + w >= 0. All zero keeps all.
  clip: vec4f,
};

@group(0) @binding(0) var<uniform> g: Globals;
// The text distance field (TY19): r signed distance in target pixels (negative inside the
// ink), gb unit direction away from the nearest ink, a coverage. A 1x1 far-away texel when no
// field is bound.
@group(0) @binding(1) var field_tex: texture_2d<f32>;
@group(0) @binding(2) var field_smp: sampler;
// The key light's shadow map (EG-18), a 1x1 cleared map when shadows are off.
@group(0) @binding(3) var shadow_map: texture_depth_2d;
@group(0) @binding(4) var shadow_smp: sampler_comparison;

const TAU: f32 = 6.283185307;

// True when world point `p` lies in the half this pass leaves out (EG-20).
fn split_out (p: vec3f) -> bool {
  return dot (g.clip.xyz, p) + g.clip.w < 0.0;
}

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

// How strongly a mirrored draw shows at world height `y`: the floor's reflection strength
// right under the floor, nothing `fade` below it, nothing above it.
fn reflect_at (y: f32) -> f32 {
  let below = g.floor.x - y;
  if (below < 0.0) {
    return 0.0;
  }
  return g.floor.z * (1.0 - smoothstep (0.0, max (g.floor.y, 1e-4), below));
}

// Blur disc diameter in pixels for something at view distance `d`.
fn coc_px (d: f32) -> f32 {
  return g.focus.z * g.fog.w * clamp (abs (d - g.focus.x) / max (g.focus.y, 1e-4), 0.0, 1.0);
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

// A Poisson disc of 16 taps in the unit circle.
const POISSON = array<vec2f, 16> (
  vec2f (-0.94201624, -0.39906216), vec2f (0.94558609, -0.76890725),
  vec2f (-0.09418410, -0.92938870), vec2f (0.34495938, 0.29387760),
  vec2f (-0.91588581, 0.45771432), vec2f (-0.81544232, -0.87912464),
  vec2f (-0.38277543, 0.27676845), vec2f (0.97484398, 0.75648379),
  vec2f (0.44323325, -0.97511554), vec2f (0.53742981, -0.47373420),
  vec2f (-0.26496911, -0.41893023), vec2f (0.79197514, 0.19090188),
  vec2f (-0.24188840, 0.99706507), vec2f (-0.81409955, 0.91437590),
  vec2f (0.19984126, 0.78641367), vec2f (0.14383161, -0.14100790),
);

// How much key light reaches world point `p` (1 lit, 1 - strength in full shadow). `n` is
// the surface normal for a mesh, zero for the floor (which casts nothing, so needs no
// offset). `px` is the fragment's pixel, which turns the disc so its steps become fine
// grain; the same pixel always gets the same turn.
fn shadow_at (p: vec3f, n: vec3f, px: vec2f) -> f32 {
  if (g.shadow.x == 0.0) {
    return 1.0;
  }
  let depth = max (-(g.view * vec4f (p, 1.0)).z, 0.0);
  let radius = g.shadow.z * (g.shadow_k.x + g.shadow_k.y * depth);
  // Lift a mesh off its own map by a texel plus the disc, so a wide disc does not shade it.
  let q = g.light_view_proj * vec4f (p + n * (1.5 * g.shadow_k.z + radius), 1.0);
  let uv = vec2f (q.x * 0.5 + 0.5, 0.5 - q.y * 0.5);
  if (any (uv < vec2f (0.0)) || any (uv > vec2f (1.0))) {
    return 1.0;
  }
  let r = min (radius / g.shadow_k.z * g.shadow.y, g.shadow_k.w);
  let z = clamp (q.z, 0.0, 1.0);
  let a = TAU * fract (52.9829189 * fract (dot (px, vec2f (0.06711056, 0.00583715))));
  let rot = mat2x2f (cos (a), sin (a), -sin (a), cos (a));
  var lit = 0.0;
  for (var i = 0u; i < 16u; i++) {
    lit += textureSampleCompareLevel (shadow_map, shadow_smp, uv + rot * POISSON[i] * r, z);
  }
  return 1.0 - g.shadow.w * (1.0 - lit / 16.0);
}
