// Materials (FX15, `material.rs`, `vo_scene::material`). Appended after `common.wgsl` and the
// shape, glyph or vector shader, which build a `MatIn` per fragment and call `material`.
//
// A material function takes the block and the fragment's inputs and returns premultiplied
// linear colour. It owns the whole look, coverage included: return `i.color * i.cov` for
// plain paint. Light added rather than painted over is colour with less alpha, so plain
// premultiplied over blending adds it. Scale colour and alpha together for opacity.
//
// To add one: write `fn my_look (m: Material, i: MatIn) -> vec4f` and register it with
// `Renderer::add_material(id, "my_look", source)`, id 64 or more. The renderer appends the
// source and a case for it to the switch in `material` below. Params are `m.p[0]` to
// `m.p[5]`; slots the scene marked as colours arrive as OKLab plus straight alpha, which
// `mat_lab` turns into linear light.

struct Material {
  // x: function id, y: flags.
  head: vec4u,
  // x: seconds since the element appeared, y: reach in layer units.
  timing: vec4f,
  p: array<vec4f, 6>,
};

@group(2) @binding(0) var<storage, read> materials: array<Material>;

struct MatIn {
  // The paint at this point: premultiplied linear light, the instance's opacity in alpha,
  // coverage not applied.
  color: vec4f,
  // How much of the pixel the shape's body covers, 0 to 1.
  cov: f32,
  // Whether `edge`, `centre` and `half_w` are known. Glyphs have coverage only.
  has_dist: bool,
  // Signed distance to the body's edge in physical pixels, positive outside.
  edge: f32,
  // Distance to a stroke's centre line in physical pixels, and the stroke's half width; a
  // fill has a half width of 0.
  centre: f32,
  half_w: f32,
  // The point in layer units and in target pixels.
  p: vec2f,
  px: vec2f,
  // A glyph's em coordinates (baseline at y 0, y up); along a vector stroke, x is the arc
  // length in layer units. Zero on shapes.
  em: vec2f,
  // Physical pixels per layer unit.
  ppu: f32,
  // Differs per instance, for per-glyph phase and flicker.
  seed: f32,
};

fn mat_lab_to_linear (lab: vec3f) -> vec3f {
  let l_ = lab.x + 0.3963377774 * lab.y + 0.2158037573 * lab.z;
  let m_ = lab.x - 0.1055613458 * lab.y - 0.0638541728 * lab.z;
  let s_ = lab.x - 0.0894841775 * lab.y - 1.2914855480 * lab.z;
  return vec3f (
     4.0767416621 * l_ * l_ * l_ - 3.3077115913 * m_ * m_ * m_ + 0.2309699292 * s_ * s_ * s_,
    -1.2684380046 * l_ * l_ * l_ + 2.6097574011 * m_ * m_ * m_ - 0.3413193965 * s_ * s_ * s_,
    -0.0041960863 * l_ * l_ * l_ - 0.7034186147 * m_ * m_ * m_ + 1.7076147010 * s_ * s_ * s_);
}

fn mat_linear_to_lab (c: vec3f) -> vec3f {
  let l = 0.4122214708 * c.r + 0.5363325363 * c.g + 0.0514459929 * c.b;
  let m = 0.2119034982 * c.r + 0.6806995451 * c.g + 0.1073969566 * c.b;
  let s = 0.0883024619 * c.r + 0.2817188376 * c.g + 0.6299787005 * c.b;
  let l_ = pow (max (l, 0.0), 1.0 / 3.0);
  let m_ = pow (max (m, 0.0), 1.0 / 3.0);
  let s_ = pow (max (s, 0.0), 1.0 / 3.0);
  return vec3f (
    0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
    1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
    0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_);
}

// A colour slot as linear light, straight alpha.
fn mat_lab (v: vec4f) -> vec4f {
  return vec4f (max (mat_lab_to_linear (v.xyz), vec3f (0.0)), v.w);
}

// Two colour slots mixed in premultiplied OKLab, as linear light with straight alpha.
fn mat_mix (a: vec4f, b: vec4f, t: f32) -> vec4f {
  let lab = mix (vec4f (a.xyz * a.w, a.w), vec4f (b.xyz * b.w, b.w), t);
  if (lab.w <= 0.0) {
    return vec4f (0.0);
  }
  return vec4f (max (mat_lab_to_linear (lab.xyz / lab.w), vec3f (0.0)), lab.w);
}

// A hash of two numbers to [0, 1), the same on every GPU (integer PCG).
fn mat_rand (a: f32, b: f32) -> f32 {
  var v = bitcast<u32> (a) * 747796405u + bitcast<u32> (b) * 2891336453u + 1u;
  v = ((v >> ((v >> 28u) + 4u)) ^ v) * 277803737u;
  v = (v >> 22u) ^ v;
  return f32 (v >> 8u) / 16777216.0;
}

// A layer point from a target pixel: the inverse of the layer's transform.
fn mat_px_to_layer (px: vec2f) -> vec2f {
  let q = px - layer.t.xy;
  let det = layer.m.x * layer.m.w - layer.m.z * layer.m.y;
  if (abs (det) < 1e-12) {
    return q;
  }
  return vec2f (layer.m.w * q.x - layer.m.z * q.y, -layer.m.y * q.x + layer.m.x * q.y) / det;
}

// Neon tube. p0: core width (fraction of the tube), core whiteness, hue shift at the edge
// (turns). p1: bloom radius (layer units), bloom strength, flicker-on seconds, steady flicker.
fn mat_neon (m: Material, i: MatIn) -> vec4f {
  let a = i.color.a;
  if (a <= 0.0) {
    return vec4f (0.0);
  }
  let base = i.color.rgb / a;
  let t = m.timing.x;
  // Flicker: while coming on, each 1/20 s step is lit with a chance that rises to 1; then a
  // steady dip at 30 steps a second.
  var bright = 1.0;
  let on = m.p[1].z;
  if (t < on) {
    let lit = mat_rand (floor (t * 20.0), 3.0) < pow (t / on, 0.6);
    bright = select (0.12, 1.0, lit);
  } else if (m.p[1].w > 0.0) {
    bright = 1.0 - m.p[1].w * mat_rand (floor (t * 30.0), 5.0);
  }
  // Across the tube: 0 on the centre line, 1 at the edge. A fill counts from its edge in.
  var r = 1.0 - i.cov;
  if (i.has_dist) {
    if (i.half_w > 0.0) {
      r = clamp (i.centre / max (i.half_w, 1e-3), 0.0, 1.0);
    } else {
      r = clamp (1.0 + i.edge / 3.0, 0.0, 1.0);
    }
  }
  // The tube's colour turns in OKLab hue toward the edge.
  let lab = mat_linear_to_lab (base);
  let ang = 6.2831853 * m.p[0].z * r * r;
  let ab = vec2f (lab.y * cos (ang) - lab.z * sin (ang), lab.y * sin (ang) + lab.z * cos (ang));
  let edge_rgb = max (mat_lab_to_linear (vec3f (lab.x, ab)), vec3f (0.0));
  let tube_rgb = max (mat_lab_to_linear (vec3f (lab.x, mix (lab.yz, ab, 0.5))), vec3f (0.0));
  // The core's edge is softened by half a pixel, so a tube a few pixels wide still has one.
  let aa = select (0.5 / max (i.half_w, 0.5), 0.15, i.half_w <= 0.0);
  let core = 1.0 - smoothstep (m.p[0].x - aa, m.p[0].x + aa, r);
  // Brighter toward the centre, as a lit tube is.
  let body_rgb = mix (mix (tube_rgb, edge_rgb, r), mix (base, vec3f (1.0), m.p[0].y), core) * (1.0 + 0.35 * (1.0 - r));
  var out = vec4f (body_rgb * i.cov, i.cov);
  if (i.has_dist && m.p[1].x > 0.0) {
    // The bloom outside the tube: light added (a third painted over, so it reads on a light
    // backdrop too), falling off exponentially from the edge.
    let d = max (i.edge, 0.0) / (m.p[1].x * i.ppu);
    let g = m.p[1].y * (0.65 * exp (-d) + 0.35 * exp (-4.0 * d * d)) * (1.0 - i.cov);
    out += vec4f (edge_rgb * g, 0.33 * min (g, 1.0));
  }
  return out * (a * bright);
}

// Hologram. p0: tint colour (alpha: how far toward it). p1: line period (layer units),
// depth, scroll (lines a second), additive share. p2: flicker amount, flicker rate, edge.
fn mat_hologram (m: Material, i: MatIn) -> vec4f {
  let a = i.color.a;
  if (a <= 0.0 || i.cov <= 0.0) {
    return vec4f (0.0);
  }
  let tint = mat_lab (m.p[0]);
  let rgb = mix (i.color.rgb / a, tint.rgb, tint.a);
  let t = m.timing.x;
  let period = max (m.p[1].x, 1e-3);
  // Thin bright lines over dark gaps, scrolling down. The phase is in layer units, so the
  // lines keep their spacing at any scale; a slow wide band drifts over them.
  let phase = i.p.y / period - t * m.p[1].z;
  let line = pow (0.5 + 0.5 * cos (6.2831853 * phase), 3.0);
  let scan = 1.0 - m.p[1].y * (1.0 - line);
  let band = 1.0 + 0.25 * exp (-pow (fract (i.p.y / (period * 40.0) - t * 0.15) * 2.0 - 1.0, 2.0) * 12.0);
  let flick = 1.0 - m.p[2].x * mat_rand (floor (t * m.p[2].y), 7.0);
  var edge = 0.0;
  if (i.has_dist) {
    edge = m.p[2].z * exp (-abs (i.edge) / 1.5);
  }
  let k = scan * band * flick + edge;
  let cov = i.cov * a;
  return vec4f (rgb * (k * cov), cov * (1.0 - m.p[1].w) * min (k, 1.0));
}

// Chrome. p0 to p3: sky top, sky at the horizon, ground at the horizon, ground bottom. p4:
// heights of the sky top, the horizon and the ground bottom, and the horizon's softness. p5:
// horizon line strength and half width. Heights are em (glyph) or negated layer y (flag 1).
fn mat_chrome (m: Material, i: MatIn) -> vec4f {
  let a = i.color.a;
  if (a <= 0.0 || i.cov <= 0.0) {
    return vec4f (0.0);
  }
  var y = i.em.y;
  if ((m.head.y & 1u) != 0u) {
    y = -i.p.y;
  }
  let h = m.p[4];
  let up = clamp ((y - h.y) / max (h.x - h.y, 1e-4), 0.0, 1.0);
  let down = clamp ((h.y - y) / max (h.y - h.z, 1e-4), 0.0, 1.0);
  // The sky brightens fast toward the horizon; the ground glows hottest at the bottom.
  let sky = mat_mix (m.p[1], m.p[0], pow (up, 0.7));
  let ground = mat_mix (m.p[2], m.p[3], pow (down, 0.8));
  let k = smoothstep (-h.w, h.w, y - h.y);
  var c = mix (ground, sky, k);
  let lw = max (m.p[5].y, 1e-4);
  let line = m.p[5].x * exp (-pow ((y - h.y) / lw, 2.0));
  c = vec4f (mix (c.rgb, vec3f (1.0), clamp (line, 0.0, 1.0)), c.a);
  let cov = i.cov * a * c.a;
  return vec4f (c.rgb * cov, cov);
}

// Gradient. p0 to p2: stop colours. p3: the axis, x0 y0 x1 y1 in layer units. p4: where the
// middle stop sits (0 to 1) and the number of stops (2 or 3).
fn mat_gradient (m: Material, i: MatIn) -> vec4f {
  let a = i.color.a;
  if (a <= 0.0 || i.cov <= 0.0) {
    return vec4f (0.0);
  }
  let d = m.p[3].zw - m.p[3].xy;
  let k = clamp (dot (i.p - m.p[3].xy, d) / max (dot (d, d), 1e-12), 0.0, 1.0);
  var c: vec4f;
  if (m.p[4].y < 2.5) {
    c = mat_mix (m.p[0], m.p[2], k);
  } else {
    let mid = m.p[4].x;
    if (k < mid) {
      c = mat_mix (m.p[0], m.p[1], k / mid);
    } else {
      c = mat_mix (m.p[1], m.p[2], (k - mid) / (1.0 - mid));
    }
  }
  let cov = i.cov * a * c.a;
  return vec4f (c.rgb * cov, cov);
}

// Material `id` (1-based) at one fragment. 0 is plain paint.
fn material (id: u32, i: MatIn) -> vec4f {
  if (id == 0u) {
    return i.color * i.cov;
  }
  let m = materials[id - 1u];
  switch m.head.x {
    case 1u: { return mat_neon (m, i); }
    case 2u: { return mat_hologram (m, i); }
    case 3u: { return mat_chrome (m, i); }
    case 4u: { return mat_gradient (m, i); }
    // vo: custom material cases
    default: { return i.color * i.cov; }
  }
}
