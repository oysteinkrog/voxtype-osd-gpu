// SDF primitive uber-shader: rounded rect (per-corner radii), circle, capsule, line and band in one
// instanced draw. Fill and stroke, solid, linear-gradient, conic-band or sheen-band paint
// interpolated in OKLab, analytic antialiasing with a one-pixel box filter. `common.wgsl` is
// prepended.
//
// A fill paint of kind glow or shadow makes the instance a halo: a glow from the shape's
// distance, or a Gaussian drop shadow of it in closed form (Evan Wallace's rounded box
// shadow). No blur pass, no texture reads.
//
// An instance with a material (FX15) hands its paint, coverage and distances to `material`
// (`materials.wgsl`, appended) instead: the stroke's where it has one, else the fill's.
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
const PAINT_GLOW: u32 = 3u;
const PAINT_SHADOW: u32 = 4u;
const PAINT_CONIC: u32 = 5u;
const PAINT_SHEEN: u32 = 6u;

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
  // Material id, 0 for none, and how far it draws outside the shape (layer units).
  @location(10) material: u32,
  @location(11) reach: f32,
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
  // Material id and a seed that differs per instance.
  @location(12) @interpolate(flat) mat: vec2u,
};

@vertex fn vs_shape (@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32, s: ShapeIn) -> ShapeOut {
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  // Grow the quad by half the stroke plus one physical pixel for the antialiasing ramp, and
  // by the material's reach.
  var m = 0.5 * s.stroke_width + 1.0 / layer.t.z + s.reach;
  let kind = s.kinds & 0xffu;
  // A halo reaches further: a glow until it falls under 1/512, a shadow by its offset, three
  // standard deviations and its spread.
  let fill_kind = (s.kinds >> 8u) & 0xffu;
  if (fill_kind == PAINT_GLOW) {
    m += s.fill_p.x * log (max (512.0 * s.fill_p.y, 1.0));
  } else if (fill_kind == PAINT_SHADOW) {
    m += max (abs (s.fill_p.x), abs (s.fill_p.y)) + 3.0 * s.fill_p.z + max (s.fill_p.w, 0.0);
  }
  // Motion blur smears the shape half its smear either way.
  m += 0.5 * length (layer.smear.xy);
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
  o.mat = vec2u (s.material, ii);
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
  if (kind == PAINT_CONIC) {
    // Turns from the band's direction, -0.5 to 0.5.
    let v = p - ends.xy;
    let da = fract (atan2 (v.y, v.x) * 0.15915494 - ends.z + 0.5) - 0.5;
    t = smoothstep (0.0, 1.0, abs (da) / ends.w);
  } else if (kind == PAINT_SHEEN) {
    let len2 = dot (ends.zw, ends.zw);
    t = select (1.0, smoothstep (0.0, 1.0, abs (dot (p - ends.xy, ends.zw)) / len2), len2 > 0.0);
  } else if (kind != PAINT_SOLID) {
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

// Signed distance to the shape at `p` (layer units); `frag_x` is the pixel centre in target
// pixels, for bands.
fn sd_shape (kind: u32, p: vec2f, frag_x: f32, in: ShapeOut) -> f32 {
  if (kind == KIND_CIRCLE) {
    return length (p - in.geom.xy) - in.geom.z;
  } else if (kind == KIND_LINE) {
    return sd_segment (p, in.geom.xy, in.geom.zw);
  } else if (kind == KIND_BAND) {
    return sd_band (p, in.geom, in.radii.xy, frag_x, in.band_px);
  }
  return sd_rrect (p, in.geom, in.radii);
}

// Error function, the 4-term approximation (max error about 5e-4).
fn erf2 (x: vec2f) -> vec2f {
  let s = sign (x);
  let a = abs (x);
  var y = 1.0 + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
  y = y * y;
  return s - s / (y * y);
}

fn gaussian (x: f32, sigma: f32) -> f32 {
  return exp (-(x * x) / (2.0 * sigma * sigma)) / (2.5066283 * sigma);
}

// The blur of one row of a rounded box: the integral along x of the Gaussian across the row's
// span at height `y` (box centred on 0).
fn box_shadow_x (x: f32, y: f32, sigma: f32, corner: f32, half: vec2f) -> f32 {
  let delta = min (half.y - corner - abs (y), 0.0);
  let curved = half.x - corner + sqrt (max (0.0, corner * corner - delta * delta));
  let integral = 0.5 + 0.5 * erf2 ((x + vec2f (-curved, curved)) * (0.70710678 / sigma));
  return integral.y - integral.x;
}

// Gaussian blur of a rounded box at `p`: exact in x, four samples in y.
fn box_shadow (rect: vec4f, corner: f32, p: vec2f, sigma: f32) -> f32 {
  let half = 0.5 * rect.zw;
  let q = p - (rect.xy + half);
  let lo = q.y - half.y;
  let hi = q.y + half.y;
  let start = clamp (-3.0 * sigma, lo, hi);
  let end = clamp (3.0 * sigma, lo, hi);
  let step = (end - start) / 4.0;
  var y = start + 0.5 * step;
  var v = 0.0;
  for (var i = 0; i < 4; i++) {
    v += box_shadow_x (q.x, q.y - y, sigma, corner, half) * gaussian (y, sigma) * step;
    y += step;
  }
  return v;
}

// A glow or shadow instance. The paint's stops are OKLab with straight alpha; the result is
// premultiplied linear light, with colour above alpha where a glow's strength exceeds 1.
fn halo (in: ShapeOut, fill_kind: u32, d: f32) -> vec4f {
  let kind = in.kinds & 0xffu;
  let hw = 0.5 * in.stroke_width;
  var v: f32;
  if (fill_kind == PAINT_GLOW) {
    // Around a stroke the glow falls off on both sides of it, so a stroked outline glows
    // inside too; a fill drawn over it hides that part.
    let r = max (in.fill_p.x, 1e-4);
    let dd = select (d, abs (d), hw > 0.0) - hw;
    v = in.fill_p.y * exp (-max (dd, 0.0) / r);
  } else {
    // At least a quarter pixel, so a hard shadow still has an antialiased edge.
    let sigma = max (in.fill_p.z, 0.25 / layer.t.z);
    let spread = in.fill_p.w;
    let p = in.p - in.fill_p.xy;
    if (kind == KIND_LINE || kind == KIND_BAND) {
      let ds = sd_shape (kind, p, frag_target (in.clip.xy).x - in.fill_p.x * layer.t.z, in) - hw - spread;
      v = 0.5 - 0.5 * erf2 (vec2f (ds * 0.70710678 / sigma)).x;
    } else {
      var rect = in.geom;
      if (kind == KIND_CIRCLE) {
        rect = vec4f (in.geom.xy - in.geom.z, vec2f (2.0 * in.geom.z));
      }
      rect = vec4f (rect.xy - spread - hw, rect.zw + 2.0 * (spread + hw));
      // A negative spread can shrink the box past zero: then nothing casts a shadow (the CPU
      // path in cpu.rs skips it the same way).
      if (rect.z <= 0.0 || rect.w <= 0.0) {
        v = 0.0;
      } else {
        let q = p - (rect.xy + 0.5 * rect.zw);
        var r: f32;
        if (q.y < 0.0) {
          r = select (in.radii.x, in.radii.y, q.x >= 0.0);
        } else {
          r = select (in.radii.w, in.radii.z, q.x >= 0.0);
        }
        let corner = clamp (r + spread + hw, 0.0, 0.5 * min (rect.z, rect.w));
        v = box_shadow (rect, corner, p, sigma);
      }
    }
  }
  let a = in.fill_c0.w * max (v, 0.0);
  let rgb = clamp (oklab_to_linear (in.fill_c0.xyz), vec3f (0.0), vec3f (1.0));
  return vec4f (rgb * a, min (a, 1.0));
}

// Premultiplied colour of the shape at layer point `p`, whose pixel centre is at `frag_x` in
// target pixels (for bands). No clip, no output encode.
fn shade (in: ShapeOut, p: vec2f, frag_x: f32) -> vec4f {
  let kind = in.kinds & 0xffu;
  let fill_kind = (in.kinds >> 8u) & 0xffu;
  if (fill_kind == PAINT_GLOW || fill_kind == PAINT_SHADOW) {
    let d = sd_shape (kind, p, frag_x, in);
    return halo (in, fill_kind, d);
  }
  var d: f32;
  if (kind == KIND_CIRCLE) {
    d = length (p - in.geom.xy) - in.geom.z;
  } else if (kind == KIND_LINE) {
    d = sd_segment (p, in.geom.xy, in.geom.zw);
  } else if (kind == KIND_BAND) {
    d = sd_band (p, in.geom, in.radii.xy, frag_x, in.band_px);
  } else {
    d = sd_rrect (p, in.geom, in.radii);
  }
  // Distance and half stroke width in physical pixels. Coverage is the overlap of the pixel's
  // one-pixel box with the filled half-plane or the stroke band, exact for straight edges
  // and for strokes thinner than a pixel.
  let ppu = layer.t.z * warp_scale (frag_target (in.clip.xy));
  let dp = d * ppu;
  let hw = 0.5 * in.stroke_width * ppu;
  var fill_cov = clamp (0.5 - dp, 0.0, 1.0);
  if (kind == KIND_LINE) {
    fill_cov = 0.0;
  }
  let stroke_cov = clamp (min (dp + 0.5, hw) - max (dp - 0.5, -hw), 0.0, 1.0);

  if (in.mat.x != 0u) {
    let stroke_kind = (in.kinds >> 16u) & 0xffu;
    let stroked = hw > 0.0 && stroke_kind != PAINT_NONE;
    var mi: MatIn;
    mi.has_dist = true;
    mi.p = p;
    mi.px = layer_to_px (p);
    mi.ppu = layer.t.z;
    mi.seed = f32 (in.mat.y);
    mi.em = vec2f (0.0);
    // The fill and the stroke each go through the material, the stroke over the fill.
    var out = vec4f (0.0);
    if (fill_kind != PAINT_NONE) {
      mi.color = paint (fill_kind, in.fill_c0, in.fill_c1, in.fill_p, p);
      mi.cov = fill_cov;
      mi.edge = dp;
      mi.centre = 0.0;
      mi.half_w = 0.0;
      out = material (in.mat.x, mi);
    }
    if (stroked) {
      mi.color = paint (stroke_kind, in.stroke_c0, in.stroke_c1, in.stroke_p, p);
      mi.cov = stroke_cov;
      mi.edge = abs (dp) - hw;
      mi.centre = abs (dp);
      mi.half_w = hw;
      let s = material (in.mat.x, mi);
      out = s + out * (1.0 - s.a);
    }
    return out;
  }

  let fill = paint ((in.kinds >> 8u) & 0xffu, in.fill_c0, in.fill_c1, in.fill_p, p) * fill_cov;
  let stroke = paint ((in.kinds >> 16u) & 0xffu, in.stroke_c0, in.stroke_c1, in.stroke_p, p) * stroke_cov;
  return stroke + fill * (1.0 - stroke.a);
}

// Motion blur takes one sample per pixel of smear, 4 to 32 of them, so thin stems do not show
// as separate copies.
fn smear_samples () -> i32 {
  return clamp (i32 (ceil (length (layer.smear.zw))), 4, 32);
}

@fragment fn fs_shape (in: ShapeOut) -> @location(0) vec4f {
  let frag_x = frag_target (in.clip.xy).x;
  var c: vec4f;
  if (all (layer.smear.xy == vec2f (0.0))) {
    c = shade (in, in.p, frag_x);
  } else {
    // The average over the shutter: samples spread evenly over the smear, centred on the
    // shape's position.
    let n = smear_samples ();
    for (var i = 0; i < n; i++) {
      let k = (f32 (i) + 0.5) / f32 (n) - 0.5;
      c += shade (in, in.p - k * layer.smear.xy, frag_x - k * layer.smear.z);
    }
    c /= f32 (n);
  }
  return finish (c * clip_coverage (in.clip.xy));
}
