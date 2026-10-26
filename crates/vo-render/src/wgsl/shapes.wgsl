// SDF primitive uber-shader: rounded rect (per-corner radii), circle, capsule, line and band in one
// instanced draw. Fill and stroke, solid or linear-gradient paint interpolated in OKLab,
// analytic antialiasing with a one-pixel box filter. `common.wgsl` is prepended.
//
// The CPU side (`shapes.rs`) has already resolved hairlines to a stroke width of one physical
// pixel, snapped their geometry to the pixel grid, and folded layer opacity into the alpha
// of every paint.

const KIND_RRECT: u32 = 0u;
const KIND_CIRCLE: u32 = 1u;
const KIND_CAPSULE: u32 = 2u;
const KIND_LINE: u32 = 3u;
const KIND_BAND: u32 = 4u;

const PAINT_NONE: u32 = 0u;
const PAINT_SOLID: u32 = 1u;

struct ShapeIn {
  // Shape kind in bits 0..8, fill paint kind in 8..16, stroke paint kind in 16..24.
  @location(0) kinds: u32,
  @location(1) stroke_width: f32,
  @location(2) geom: vec4f,
  @location(3) radii: vec4f,
  // Paint stops: OKLab and straight alpha. `*_p` is p0.xy, p1.xy in layer space.
  @location(4) fill_c0: vec4f,
  @location(5) fill_c1: vec4f,
  @location(6) fill_p: vec4f,
  @location(7) stroke_c0: vec4f,
  @location(8) stroke_c1: vec4f,
  @location(9) stroke_p: vec4f,
};

struct ShapeOut {
  @builtin(position) clip: vec4f,
  @location(0) p: vec2f,
  @location(1) @interpolate(flat) kinds: u32,
  @location(2) @interpolate(flat) stroke_width: f32,
  @location(3) @interpolate(flat) geom: vec4f,
  @location(4) @interpolate(flat) radii: vec4f,
  @location(5) @interpolate(flat) fill_c0: vec4f,
  @location(6) @interpolate(flat) fill_c1: vec4f,
  @location(7) @interpolate(flat) fill_p: vec4f,
  @location(8) @interpolate(flat) stroke_c0: vec4f,
  @location(9) @interpolate(flat) stroke_c1: vec4f,
  @location(10) @interpolate(flat) stroke_p: vec4f,
  // A band's ends in physical pixels along x, compared with the exact pixel centre.
  @location(11) @interpolate(flat) band_px: vec2f,
};

@vertex fn vs_shape (@builtin(vertex_index) vi: u32, s: ShapeIn) -> ShapeOut {
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  // Grow the quad by half the stroke plus one physical pixel for the antialiasing ramp.
  let m = 0.5 * s.stroke_width + 1.0 / layer.t.z;
  let kind = s.kinds & 0xffu;
  var p: vec2f;
  if (kind == KIND_CIRCLE) {
    let r = s.geom.z + m;
    p = s.geom.xy + (c * 2.0 - 1.0) * r;
  } else if (kind == KIND_LINE) {
    let d = s.geom.zw - s.geom.xy;
    let len = length (d);
    var dir = vec2f (1.0, 0.0);
    if (len > 0.0) {
      dir = d / len;
    }
    let n = vec2f (-dir.y, dir.x);
    p = s.geom.xy + dir * mix (-m, len + m, c.x) + n * mix (-m, m, c.y);
  } else if (kind == KIND_BAND) {
    let h = max (s.radii.x, s.radii.y) + m;
    p = vec2f (mix (s.geom.x - m, s.geom.z + m, c.x), s.geom.y + mix (-h, h, c.y));
  } else {
    p = mix (s.geom.xy - m, s.geom.xy + s.geom.zw + m, c);
  }

  var o: ShapeOut;
  o.clip = px_to_ndc (layer_to_px (p));
  o.p = p;
  o.kinds = s.kinds;
  o.stroke_width = s.stroke_width;
  o.geom = s.geom;
  o.radii = s.radii;
  o.fill_c0 = s.fill_c0;
  o.fill_c1 = s.fill_c1;
  o.fill_p = s.fill_p;
  o.stroke_c0 = s.stroke_c0;
  o.stroke_c1 = s.stroke_c1;
  o.stroke_p = s.stroke_p;
  o.band_px = vec2f (layer_to_px (s.geom.xy).x, layer_to_px (s.geom.zy).x);
  return o;
}

fn oklab_to_linear (lab: vec3f) -> vec3f {
  let l_ = lab.x + 0.3963377774 * lab.y + 0.2158037573 * lab.z;
  let m_ = lab.x - 0.1055613458 * lab.y - 0.0638541728 * lab.z;
  let s_ = lab.x - 0.0894841775 * lab.y - 1.2914855480 * lab.z;
  let l = l_ * l_ * l_;
  let m = m_ * m_ * m_;
  let s = s_ * s_ * s_;
  return vec3f (
     4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s);
}

// Premultiplied linear colour of a paint at `p`. Stops are interpolated as premultiplied
// OKLab, as CSS does, so a fade to transparent keeps its hue.
fn paint (kind: u32, c0: vec4f, c1: vec4f, ends: vec4f, p: vec2f) -> vec4f {
  if (kind == PAINT_NONE) {
    return vec4f (0.0);
  }
  var t = 0.0;
  if (kind != PAINT_SOLID) {
    let d = ends.zw - ends.xy;
    let len2 = dot (d, d);
    if (len2 > 0.0) {
      t = clamp (dot (p - ends.xy, d) / len2, 0.0, 1.0);
    }
  }
  let lab = mix (vec4f (c0.xyz * c0.w, c0.w), vec4f (c1.xyz * c1.w, c1.w), t);
  if (lab.w <= 0.0) {
    return vec4f (0.0);
  }
  let rgb = clamp (oklab_to_linear (lab.xyz / lab.w), vec3f (0.0), vec3f (1.0));
  return vec4f (rgb * lab.w, lab.w);
}

// Distance to the straight top or bottom edge of a band. A pixel whose centre `frag_x` is
// outside the band's ends `ends` (both in pixels) is not covered at all. The ends are the same
// numbers for two bands that share one, and the pixel centre is exact, so neighbouring bands
// tile without a seam; comparing the interpolated layer position would not, because two quads
// interpolate it with different rounding.
fn sd_band (p: vec2f, g: vec4f, h: vec2f, frag_x: f32, ends: vec2f) -> f32 {
  if (frag_x < min (ends.x, ends.y) || frag_x >= max (ends.x, ends.y)) {
    return 1e9;
  }
  let w = max (g.z - g.x, 1e-6);
  let slope = (h.y - h.x) / w;
  let half = h.x + slope * (p.x - g.x);
  return (abs (p.y - g.y) - half) / sqrt (1.0 + slope * slope);
}

fn sd_segment (p: vec2f, a: vec2f, b: vec2f) -> f32 {
  let pa = p - a;
  let ba = b - a;
  let h = clamp (dot (pa, ba) / max (dot (ba, ba), 1e-12), 0.0, 1.0);
  return length (pa - ba * h);
}

@fragment fn fs_shape (in: ShapeOut) -> @location(0) vec4f {
  let kind = in.kinds & 0xffu;
  var d: f32;
  if (kind == KIND_CIRCLE) {
    d = length (in.p - in.geom.xy) - in.geom.z;
  } else if (kind == KIND_LINE) {
    d = sd_segment (in.p, in.geom.xy, in.geom.zw);
  } else if (kind == KIND_BAND) {
    d = sd_band (in.p, in.geom, in.radii.xy, in.clip.x, in.band_px);
  } else {
    d = sd_rrect (in.p, in.geom, in.radii);
  }
  // Distance and half stroke width in physical pixels. Coverage is the overlap of the pixel's
  // one-pixel box with the filled half-plane or the stroke band, exact for straight edges
  // and for strokes thinner than a pixel.
  let dp = d * layer.t.z;
  let hw = 0.5 * in.stroke_width * layer.t.z;
  var fill_cov = clamp (0.5 - dp, 0.0, 1.0);
  if (kind == KIND_LINE) {
    fill_cov = 0.0;
  }
  let stroke_cov = clamp (min (dp + 0.5, hw) - max (dp - 0.5, -hw), 0.0, 1.0);

  let fill = paint ((in.kinds >> 8u) & 0xffu, in.fill_c0, in.fill_c1, in.fill_p, in.p) * fill_cov;
  let stroke = paint ((in.kinds >> 16u) & 0xffu, in.stroke_c0, in.stroke_c1, in.stroke_p, in.p) * stroke_cov;
  let c = stroke + fill * (1.0 - stroke.a);
  return finish (c * clip_coverage (in.clip.xy));
}
