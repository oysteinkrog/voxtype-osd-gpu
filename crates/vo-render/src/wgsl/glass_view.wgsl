// Prelude of `glass.wgsl` for `GlassPass` on its own: instances in physical pixels, the
// target size in a uniform. `finish` encodes sRGB only when the target is a plain UNORM
// format.

struct View {
  // Target width and height; z is 1 when the shader must encode sRGB itself.
  size: vec4f,
};

@group(0) @binding(0) var<uniform> view: View;
// Outline points (EG-06): x, y and arc length in logical pixels.
@group(0) @binding(1) var<storage, read> glass_points: array<vec4f>;

fn glass_point (i: u32) -> vec4f {
  return glass_points[i];
}

fn glass_origin () -> vec2f {
  return vec2f (0.0);
}

// Panels here are never turned.
fn glass_turn () -> mat2x2f {
  return mat2x2f (vec2f (1.0, 0.0), vec2f (0.0, 1.0));
}

fn glass_aa (frag: vec2f) -> f32 {
  return 1.0;
}

@vertex fn vs_glass (@builtin(vertex_index) vi: u32, g: GlassIn) -> GlassOut {
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  let k = g.shape.y;
  // Room for the shadow below and around, and one pixel for antialiasing.
  let m = max (2.5 * g.shadow.y + abs (g.shadow.z), 0.0) * k + 2.0;
  let p = mix (g.rect.xy - m, g.rect.xy + g.rect.zw + m, c);
  let n = p / view.size.xy * 2.0 - 1.0;
  var o: GlassOut;
  o.clip = vec4f (n.x, -n.y, 0.0, 1.0);
  o.p = p;
  o.rect = g.rect;
  o.shape = g.shape;
  o.tint = g.tint;
  o.accent = g.accent;
  o.anim = g.anim;
  o.fx = g.fx;
  o.shadow = g.shadow;
  o.outline = g.outline;
  o.refract = g.refract;
  o.refract2 = g.refract2;
  return o;
}

fn srgb_encode (c: vec3f) -> vec3f {
  let lo = c * 12.92;
  let hi = 1.055 * pow (max (c, vec3f (0.0)), vec3f (1.0 / 2.4)) - 0.055;
  return select (hi, lo, c <= vec3f (0.0031308));
}

fn finish (c: vec4f) -> vec4f {
  if (view.size.z == 0.0 || c.a <= 0.0) {
    return c;
  }
  let straight = clamp (c.rgb / c.a, vec3f (0.0), vec3f (1.0));
  return vec4f (srgb_encode (straight) * c.a, c.a);
}

fn glass_clip (frag: vec2f) -> f32 {
  return 1.0;
}

fn glass_frag_px (frag: vec2f) -> vec2f {
  return frag;
}
