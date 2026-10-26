// Shared by the shape, glyph and composite pipelines of `Renderer`.
//
// Group 1 is the per-run layer block, bound with a dynamic offset. Colours in the shaders are
// linear light with premultiplied alpha. `finish` encodes to sRGB only when the target is a
// plain UNORM format (the direct path in ROADMAP 4.3); an sRGB or float target gets linear
// values and the hardware does the encode.

struct Layer {
  // Layer space (logical units) to physical pixels:
  // x' = m.x x + m.z y + t.x, y' = m.y x + m.w y + t.y.
  m: vec4f,
  // t.xy translation, t.z physical pixels per layer unit, t.w composite opacity.
  t: vec4f,
  // Clip rounded rect in the clip owner's layer space: x, y, w, h; radii TL, TR, BR, BL.
  clip_rect: vec4f,
  clip_radii: vec4f,
  // Physical pixels to clip space, laid out like m and t. clip_t.z is physical pixels per
  // clip unit, clip_t.w is 1 when there is a clip.
  clip_m: vec4f,
  clip_t: vec4f,
  // Target size in physical pixels; z is 1 when the shader must encode sRGB itself.
  viewport: vec4f,
};

@group(1) @binding(0) var<uniform> layer: Layer;

fn layer_to_px (p: vec2f) -> vec2f {
  return vec2f (layer.m.x * p.x + layer.m.z * p.y, layer.m.y * p.x + layer.m.w * p.y) + layer.t.xy;
}

fn px_to_ndc (p: vec2f) -> vec4f {
  let n = p / layer.viewport.xy * 2.0 - 1.0;
  return vec4f (n.x, -n.y, 0.0, 1.0);
}

// Signed distance to a rounded rect `rect` (x, y, w, h) with corner radii TL, TR, BR, BL
// (y down). Radii larger than half the short side are clamped.
fn sd_rrect (p: vec2f, rect: vec4f, radii: vec4f) -> f32 {
  let half = 0.5 * rect.zw;
  let q0 = p - (rect.xy + half);
  var r: f32;
  if (q0.y < 0.0) {
    r = select (radii.x, radii.y, q0.x >= 0.0);
  } else {
    r = select (radii.w, radii.z, q0.x >= 0.0);
  }
  r = clamp (r, 0.0, min (half.x, half.y));
  let q = abs (q0) - half + vec2f (r);
  return length (max (q, vec2f (0.0))) + min (max (q.x, q.y), 0.0) - r;
}

// Coverage of the layer clip at a fragment position in physical pixels.
fn clip_coverage (frag: vec2f) -> f32 {
  if (layer.clip_t.w == 0.0) {
    return 1.0;
  }
  let p = vec2f (layer.clip_m.x * frag.x + layer.clip_m.z * frag.y,
                 layer.clip_m.y * frag.x + layer.clip_m.w * frag.y) + layer.clip_t.xy;
  let d = sd_rrect (p, layer.clip_rect, layer.clip_radii) * layer.clip_t.z;
  return clamp (0.5 - d, 0.0, 1.0);
}

fn srgb_encode (c: vec3f) -> vec3f {
  let lo = c * 12.92;
  let hi = 1.055 * pow (max (c, vec3f (0.0)), vec3f (1.0 / 2.4)) - 0.055;
  return select (hi, lo, c <= vec3f (0.0031308));
}

// Last step of every content fragment shader.
fn finish (c: vec4f) -> vec4f {
  if (layer.viewport.z == 0.0 || c.a <= 0.0) {
    return c;
  }
  let straight = clamp (c.rgb / c.a, vec3f (0.0), vec3f (1.0));
  return vec4f (srgb_encode (straight) * c.a, c.a);
}
