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

// Textures (EG-02). A material whose block says it reads textures (`Material::reads_backdrop`,
// `Material::reads_images` in vo_scene) is drawn with textured pipelines that bind them at
// group 2 next to the blocks. Two helpers read them, from any material function:
//
//   tex_backdrop (px: vec2f) -> vec4f
//     The backdrop (EG-01) of the layer the material draws in, at `px` in target pixels (use
//     `i.px`, plus an offset to shift it). Premultiplied linear light, bilinear. Transparent
//     black outside the captured box, and when there is no backdrop: the layer (or one above
//     it drawing into the same surface) needs `Element::backdrop`, and the frame must be on
//     the full quality tier. A transparent tint captures without drawing the backdrop back.
//
//   tex_image (slot: u32, uv: vec2f) -> vec4f
//     Image slot `slot` (0 to 3, `Renderer::set_material_image`) at `uv`, 0 to 1 across the
//     image, bilinear, clamped at the edges. Colour as the texture holds it. Transparent black
//     for an empty slot or a slot past 3.
//
// Text (EG-31). A material that reads text (`Material::reads_text`) also sees the text field
// and the word table the app built with `TextField::update_words` and handed over with
// `TextField::bind` (`vo_render::text_field`):
//
//   text_dist (px: vec2f) -> vec4f
//     x: signed distance from `px` (target pixels) to the nearest ink in pixels, negative
//     inside; yz: the unit direction away from the ink; w: the ink's coverage. The field is
//     at half resolution and bilinear, so trust it to a pixel or two. Without a field, or
//     for a material that does not read text, x is 1e4 and the rest 0.
//
//   word_near (px: vec2f) -> WordHit
//     The word whose box is nearest `px`: its id, its age in seconds (long settled under
//     reduced motion), the distance to its box (0 inside) and the box (left, top, right,
//     bottom in target pixels). `id` is NO_WORD_ID and `dist` 1e9 when there are no words.
//
//   text_word_count () -> u32, text_word (k: u32, px: vec2f) -> WordHit
//     The table itself, newest word first (up to 64), with the distance from `px`.
//
// Pipelines for materials that read no texture define all of these as stubs that return
// transparent black, no ink and no words, so an app function that calls them compiles
// everywhere; their bind groups and cost are unchanged.

// vo: texture helpers begin
// The textured helpers (`material_tex.wgsl`) define these two again, so other shaders
// that swap in the textured helpers have them too.
const NO_WORD_ID: u32 = 0xffffffffu;

struct WordHit {
  id: u32,
  age: f32,
  dist: f32,
  rect: vec4f,
};

fn tex_backdrop (px: vec2f) -> vec4f {
  return vec4f (0.0);
}

fn tex_image (slot: u32, uv: vec2f) -> vec4f {
  return vec4f (0.0);
}

fn text_dist (px: vec2f) -> vec4f {
  return vec4f (1e4, 0.0, 0.0, 0.0);
}

fn text_word_count () -> u32 {
  return 0u;
}

fn text_word (k: u32, px: vec2f) -> WordHit {
  return WordHit (NO_WORD_ID, 0.0, 1e9, vec4f (0.0));
}

fn word_near (px: vec2f) -> WordHit {
  return WordHit (NO_WORD_ID, 0.0, 1e9, vec4f (0.0));
}
// vo: texture helpers end

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
    // Tapered to nothing at the quad's edge (the reach): an sRGB target shows even 1/1000 of
    // linear light, so a tail cut off there would draw the quad's outline.
    let fade = clamp (1.0 - max (i.edge, 0.0) / max (m.timing.y * i.ppu, 1e-3), 0.0, 1.0);
    let g = m.p[1].y * (0.65 * exp (-d) + 0.35 * exp (-4.0 * d * d)) * (1.0 - i.cov) * fade * fade;
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

// Aurora and haze. p0 to p3: blob colours. p4: the box, x y w h in layer units. p5: energy,
// drift (cycles a second), blob radius (a fraction of the box's shorter side), additive share.
// Flag 4: a glow that fades out between the blobs (haze) instead of a mesh filling the box.
fn mat_aurora (m: Material, i: MatIn) -> vec4f {
  let a = i.color.a;
  if (a <= 0.0 || i.cov <= 0.0) {
    return vec4f (0.0);
  }
  let b = m.p[4];
  let s = max (min (b.z, b.w), 1e-3);
  // The point from the box's centre, in units of its shorter side.
  let q = (i.p - b.xy - 0.5 * b.zw) / s;
  let half = 0.5 * b.zw / s;
  let e = clamp (m.p[5].x, 0.0, 1.0);
  let t = 6.2831853 * m.p[5].y * m.timing.x;
  // A slow warp, stronger with energy, so the blobs are not circles.
  let w = q + (0.05 + 0.08 * e) * vec2f (sin (q.y * 3.1 + t * 0.7), cos (q.x * 2.7 - t * 0.6));
  let r = m.p[5].z * (1.0 + 0.45 * e);
  // Each blob drifts on its own Lissajous path inside the box. Colours mix in premultiplied
  // OKLab. A mesh weighs the blobs by inverse square distance, smooth everywhere however
  // far apart they are; a glow by a Gaussian, so it fades out between them.
  let glow = (m.head.y & 4u) != 0u;
  var acc = vec4f (0.0);
  var wsum = 0.0;
  for (var k = 0u; k < 4u; k++) {
    let f = f32 (k);
    let c = half * 0.72 * vec2f (sin (t * (0.9 + 0.23 * f) + f * 1.7), cos (t * (0.7 + 0.31 * f) + f * 2.3));
    let d = w - c;
    let x = dot (d, d) / (r * r);
    var wk = exp (-x);
    if (!glow) {
      let v = x + 0.6;
      wk = 1.0 / (v * sqrt (v));
    }
    let col = m.p[k];
    acc += wk * vec4f (col.xyz * col.w, col.w);
    wsum += wk;
  }
  if (acc.w <= 1e-6) {
    return vec4f (0.0);
  }
  var rgb = max (mat_lab_to_linear (acc.xyz / acc.w), vec3f (0.0));
  var alpha: f32;
  if (glow) {
    alpha = 1.0 - exp (-acc.w * (0.7 + 0.6 * e));
  } else {
    alpha = acc.w / max (wsum, 1e-6);
    rgb *= 0.8 + 0.4 * e;
  }
  // Soft gradients band in 8 bits: a half-step of noise breaks the steps up.
  rgb += (mat_rand (i.px.x, i.px.y) - 0.5) / 255.0;
  let cov = i.cov * a * clamp (alpha, 0.0, 1.0);
  return vec4f (max (rgb, vec3f (0.0)) * cov, cov * (1.0 - m.p[5].w));
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
    case 5u: { return mat_aurora (m, i); }
    // vo: custom material cases
    default: { return i.color * i.cov; }
  }
}
