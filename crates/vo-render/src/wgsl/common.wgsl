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
  // Size in physical pixels of the texture drawn into; z is 1 when the shader must encode
  // sRGB itself.
  viewport: vec4f,
  // xy: where that texture's top left sits in target pixels (0 except in an offscreen layer
  // sized to its bounds). zw: the same for the layer texture a composite reads.
  origin: vec4f,
  // A composite's source layer texture: width, height in pixels, bloom strength, 1 in light mode.
  src: vec4f,
  // Motion blur: the smear in layer units (xy) and in target pixels (zw); zero for none.
  smear: vec4f,
  // Edge fades of the clip in clip units: left, top, right, bottom; zero for a hard edge.
  clip_fade: vec4f,
  // A masked layer's composite (FX04): where the mask texture's top left sits in target
  // pixels (xy), and 1 in z when the mask is inverted.
  mask: vec4f,
  // A styled layer's composite (FX13): the style's box in target pixels (x0, y0, x1, y1);
  // chroma in pixels, glitch, scanline depth and period in pixels; curvature, vignette, the
  // flicker's brightness factor and the glitch step.
  style_box: vec4f,
  style_a: vec4f,
  style_b: vec4f,
  // The stage warp (vo_scene::stage) in target pixels: rows of the 3x3 map from pixels as laid
  // out to pixels as seen, then the rows of its inverse. warp0.w is 1 when the layer is warped.
  warp0: vec4f,
  warp1: vec4f,
  warp2: vec4f,
  unwarp0: vec4f,
  unwarp1: vec4f,
  unwarp2: vec4f,
};

@group(1) @binding(0) var<uniform> layer: Layer;

// The signal bus block (vo_core::signals::SIGNALS_WGSL, kept equal by a test): audio level,
// pitch, onsets, 24 mel bands and a 0.64 s level history, for every layer pipeline.
struct Signals {
    head: array<vec4<f32>, 4>,
    bands: array<vec4<f32>, 6>,
    ring: array<vec4<f32>, 16>,
};
@group(3) @binding(0) var<uniform> signals: Signals;
fn sig_presence() -> f32 { return signals.head[0].y; }
fn sig_age() -> f32 { return signals.head[0].z; }
fn sig_onset_age() -> f32 { return signals.head[0].w; }
fn sig_level() -> f32 { return signals.head[1].x; }
fn sig_peak() -> f32 { return signals.head[1].y; }
fn sig_pitch() -> f32 { return signals.head[1].z; }
fn sig_voiced() -> f32 { return signals.head[1].w; }
fn sig_centroid() -> f32 { return signals.head[2].x; }
fn sig_flatness() -> f32 { return signals.head[2].y; }
fn sig_onset() -> f32 { return signals.head[2].z; }
fn sig_level_smooth() -> f32 { return signals.head[2].w; }
fn sig_band(i: u32) -> f32 { let b = min(i, 23u); return signals.bands[b / 4u][b % 4u]; }
fn sig_ring(i: u32) -> f32 { let r = min(i, 63u); return signals.ring[r / 4u][r % 4u]; }

fn layer_to_px (p: vec2f) -> vec2f {
  return vec2f (layer.m.x * p.x + layer.m.z * p.y, layer.m.y * p.x + layer.m.w * p.y) + layer.t.xy;
}

fn warped () -> bool {
  return layer.warp0.w != 0.0;
}

// Target pixels as laid out to clip space. A warped layer keeps the divide in w, so varyings
// are interpolated in perspective.
fn px_to_ndc (p: vec2f) -> vec4f {
  if (!warped ()) {
    let n = (p - layer.origin.xy) / layer.viewport.xy * 2.0 - 1.0;
    return vec4f (n.x, -n.y, 0.0, 1.0);
  }
  let h = vec3f (p, 1.0);
  let q = vec3f (dot (layer.warp0.xyz, h), dot (layer.warp1.xyz, h), dot (layer.warp2.xyz, h));
  let n = (q.xy / q.z - layer.origin.xy) / layer.viewport.xy * 2.0 - 1.0;
  return vec4f (n.x * q.z, -n.y * q.z, 0.0, q.z);
}

// The target pixel, as laid out, of a fragment at `frag_tex` in the texture drawn into.
fn frag_target (frag_tex: vec2f) -> vec2f {
  let f = frag_tex + layer.origin.xy;
  if (!warped ()) {
    return f;
  }
  let h = vec3f (f, 1.0);
  let q = vec3f (dot (layer.unwarp0.xyz, h), dot (layer.unwarp1.xyz, h), dot (layer.unwarp2.xyz, h));
  return q.xy / q.z;
}

// Pixels as seen per pixel as laid out, at target pixel `p` as laid out: 1 on a flat layer,
// less where a warped layer recedes. Antialiasing ramps scale by it.
fn warp_scale (p: vec2f) -> f32 {
  if (!warped ()) {
    return 1.0;
  }
  let h = vec3f (p, 1.0);
  let w = dot (layer.warp2.xyz, h);
  let x = dot (layer.warp0.xyz, h) / w;
  let y = dot (layer.warp1.xyz, h) / w;
  let j = mat2x2f (
    vec2f (layer.warp0.x - x * layer.warp2.x, layer.warp1.x - y * layer.warp2.x) / w,
    vec2f (layer.warp0.y - x * layer.warp2.y, layer.warp1.y - y * layer.warp2.y) / w);
  return sqrt (abs (determinant (j)));
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

// Coverage of the layer clip at a fragment position in pixels of the texture drawn into.
fn clip_coverage (frag_tex: vec2f) -> f32 {
  if (layer.clip_t.w == 0.0) {
    return 1.0;
  }
  let frag = frag_target (frag_tex);
  let p = vec2f (layer.clip_m.x * frag.x + layer.clip_m.z * frag.y,
                 layer.clip_m.y * frag.x + layer.clip_m.w * frag.y) + layer.clip_t.xy;
  let d = sd_rrect (p, layer.clip_rect, layer.clip_radii) * layer.clip_t.z;
  // Edge fades (FX04): a smoothstep over the fade width from each edge.
  let r = layer.clip_rect;
  let e = vec4f (p.x - r.x, p.y - r.y, r.x + r.z - p.x, r.y + r.w - p.y);
  let ramp = smoothstep (vec4f (0.0), max (layer.clip_fade, vec4f (1e-6)), e);
  let f = select (vec4f (1.0), ramp, layer.clip_fade > vec4f (0.0));
  return clamp (0.5 - d, 0.0, 1.0) * f.x * f.y * f.z * f.w;
}

fn srgb_encode (c: vec3f) -> vec3f {
  let lo = c * 12.92;
  let hi = 1.055 * pow (max (c, vec3f (0.0)), vec3f (1.0 / 2.4)) - 0.055;
  return select (hi, lo, c <= vec3f (0.0031308));
}

// Last step of every content fragment shader.
fn finish (c: vec4f) -> vec4f {
  // A mask's content (viewport.w 1 or 2) writes its coverage or its luminance; on the R8
  // mask texture, premultiplied over then gives the union of what the mask draws.
  if (layer.viewport.w != 0.0) {
    let v = select (c.a, dot (c.rgb, vec3f (0.2126, 0.7152, 0.0722)), layer.viewport.w == 2.0);
    return vec4f (v, v, v, c.a);
  }
  if (layer.viewport.z == 0.0 || c.a <= 0.0) {
    return c;
  }
  let straight = clamp (c.rgb / c.a, vec3f (0.0), vec3f (1.0));
  return vec4f (srgb_encode (straight) * c.a, c.a);
}
