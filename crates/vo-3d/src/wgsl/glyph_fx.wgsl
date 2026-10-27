// Glyph field effects (`glyph_fx.rs`): ink that blooms out of a new letter (MO-82), light a
// word stencils onto the panel behind it (MO-84) and one word flowing into its correction
// (MO-87). Every effect reads the glyphs' distance fields from one atlas (`glyph_sdf.rs`).
//
// Each draw is a quad over a range of glyphs. Quads and glyphs are in surface pixels; the
// pass's own target (the surface, or the half size stencil mask) is `fx.dst`.

struct Fx {
  // Surface width and height in pixels, opacity, 1 to encode sRGB here.
  viewport: vec4f,
  // The pass target in surface pixels: origin, size.
  dst: vec4f,
  // The item's box in surface pixels: origin, size. The stencil mask covers it.
  rect: vec4f,
  // Mode, frame time in seconds, voice level 0..1, seconds since the last onset.
  time: vec4f,
  // Light position in surface pixels, intensity, reduced motion (1).
  light: vec4f,
  // Per effect, see `glyph_fx.rs`.
  p0: vec4f,
  p1: vec4f,
  // Colours: linear light, straight alpha.
  c0: vec4f,
  c1: vec4f,
};

struct Glyph {
  // Atlas texel rect: x, y, w, h.
  atlas: vec4f,
  // The field's em corners: x0, y0, x1, y1 (y up).
  em: vec4f,
  // Pen position (surface pixels, baseline), pixels per em, seconds since the glyph's start.
  place: vec4f,
  // Linear colour, straight alpha.
  color: vec4f,
  // Part (0 both, 1 old, 2 new), seed, x offset of the old place in pixels, 0.
  info: vec4f,
};

struct Quad {
  rect: vec4f,
  // First glyph, count.
  range: vec4u,
};

@group(0) @binding(0) var<uniform> fx: Fx;
@group(0) @binding(1) var<storage, read> glyphs: array<Glyph>;
@group(0) @binding(2) var<storage, read> quads: array<Quad>;
@group(0) @binding(3) var atlas: texture_2d<f32>;
@group(0) @binding(4) var samp: sampler;
@group(0) @binding(5) var mask: texture_2d<f32>;

const TEXELS_PER_EM: f32 = 64.0;
const PAD_EM: f32 = 0.35;
const PI: f32 = 3.14159265;

struct VsOut {
  @builtin(position) clip: vec4f,
  @location(0) px: vec2f,
  @location(1) @interpolate(flat) quad: u32,
};

@vertex fn vs_fx (@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VsOut {
  let q = quads[ii];
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  let px = mix (q.rect.xy, q.rect.zw, c);
  let n = (px - fx.dst.xy) / fx.dst.zw * 2.0 - 1.0;
  var out: VsOut;
  out.clip = vec4f (n.x, -n.y, 0.0, 1.0);
  out.px = px;
  out.quad = ii;
  return out;
}

// ---- Fields -------------------------------------------------------------------------------

// A glyph's em coordinates at surface pixel `px`, with its pen moved by `dx` pixels.
fn em_at (g: Glyph, px: vec2f, dx: f32) -> vec2f {
  return vec2f (px.x - g.place.x - dx, g.place.y - px.y) / g.place.z;
}

// Signed distance in em from `e` to the glyph's edge. Past the grid, the distance to the
// grid's edge is added, which is close enough for far points.
fn field_em (g: Glyph, e: vec2f) -> f32 {
  let h = 0.5 / TEXELS_PER_EM;
  let q = clamp (e, g.em.xy + h, g.em.zw - h);
  let size = vec2f (textureDimensions (atlas));
  let tex = g.atlas.xy + vec2f ((q.x - g.em.x) * TEXELS_PER_EM, (g.em.w - q.y) * TEXELS_PER_EM);
  return textureSampleLevel (atlas, samp, tex / size, 0.0).r + length (e - q);
}

// The glyph's ink box centre and half diagonal, in em.
fn ink_centre (g: Glyph) -> vec2f {
  return 0.5 * (g.em.xy + g.em.zw);
}

fn ink_radius (g: Glyph) -> f32 {
  return 0.5 * length (g.em.zw - g.em.xy - 2.0 * PAD_EM);
}

fn smin (a: f32, b: f32, k: f32) -> f32 {
  if (k <= 0.0) {
    return min (a, b);
  }
  let h = clamp (0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
  return mix (b, a, h) - k * h * (1.0 - h);
}

// ---- Noise --------------------------------------------------------------------------------

fn hash2 (p: vec2f) -> f32 {
  let v = vec2u (vec2i (floor (p)));
  var h = v.x * 747796405u + v.y * 2891336453u + 1u;
  h = ((h >> ((h >> 28u) + 4u)) ^ h) * 277803737u;
  h = (h >> 22u) ^ h;
  return f32 (h >> 8u) / 16777216.0;
}

fn vnoise (p: vec2f) -> f32 {
  let i = floor (p);
  let f = fract (p);
  let u = f * f * (3.0 - 2.0 * f);
  let a = hash2 (i);
  let b = hash2 (i + vec2f (1.0, 0.0));
  let c = hash2 (i + vec2f (0.0, 1.0));
  let d = hash2 (i + vec2f (1.0, 1.0));
  return mix (mix (a, b, u.x), mix (c, d, u.x), u.y);
}

fn fbm (p: vec2f) -> f32 {
  var s = 0.0;
  var a = 0.5;
  var q = p;
  for (var i = 0; i < 4; i++) {
    s += a * vnoise (q);
    q = mat2x2f (1.6, 1.2, -1.2, 1.6) * q + vec2f (17.0, 9.0);
    a *= 0.5;
  }
  return s / 0.9375;
}

// ---- Output -------------------------------------------------------------------------------

fn encode (c: vec3f) -> vec3f {
  let lo = c * 12.92;
  let hi = 1.055 * pow (max (c, vec3f (0.0)), vec3f (1.0 / 2.4)) - 0.055;
  return select (hi, lo, c <= vec3f (0.0031308));
}

// Premultiplied linear `pm` with alpha `a`, times the opacity, encoded for the target. Light
// with less alpha than colour adds over what is below.
fn finish (pm: vec3f, a: f32) -> vec4f {
  let k = fx.viewport.z;
  var c = pm * k;
  if (fx.viewport.w > 0.5) {
    c = select (encode (c), encode (c / (a * k)) * a * k, a > 1e-4);
  }
  return vec4f (c, a * k);
}

// ---- MO-82: ink diffusion -----------------------------------------------------------------
//
// p0: seconds of the bloom, how far the cloud reaches past the edge (em), seconds the trace
// at the centre lasts after the bloom, the trace's strength. c0: the ink. p1.x: how much
// the voice level at arrival widens the bloom.
//
// The letter itself is drawn by the text, readable from its first frame. Over it, ink
// spreads from the letter's centre through its body along noisy radial fibres; the part of
// the bloom that has not found the letter's edge yet hangs outside it as a soft cloud, which
// is drawn back into the edge as the bloom ends. A faint trace stays at the centre.

fn ink (g: Glyph, px: vec2f) -> vec4f {
  let secs = fx.p0.x;
  let age = g.place.w;
  let a = clamp (age / secs, 0.0, 1.0);
  let e = em_at (g, px, 0.0);
  let d = field_em (g, e);
  let cov = clamp (0.5 - d * g.place.z, 0.0, 1.0);
  let rel = e - ink_centre (g);
  let r = length (rel) / max (ink_radius (g), 1e-3);
  let dir = rel / max (length (rel), 1e-4);
  let seed = vec2f (g.info.y * 13.1, g.info.y * 7.7);
  // Fibres: warped noise that flows outward from the centre, so the ink creeps in veins
  // rather than spreading as a disc.
  let q = rel * 2.6 + seed;
  let warp = vec2f (fbm (q * 1.7 + seed), fbm (q * 1.7 + seed + vec2f (5.2, 1.3))) - 0.5;
  let fib = fbm (q + 1.1 * warp - dir * age * 3.0);
  let fine = fbm (rel * 8.0 + seed * 1.7 + 2.0 * warp - dir * age * 1.5);
  let grow = 1.0 - pow (1.0 - a, 3.0);
  let front = grow * 1.35 + (fib - 0.5) * 0.6;
  let spread = smoothstep (front + 0.05, front - 0.25, r);
  let level = fx.p1.y;
  let fade = pow (1.0 - a, 1.4);

  // Inside: pigment veins, strongest near the centre, fading as the bloom ends.
  let veins = pow (1.0 - abs (2.0 * fine - 1.0), 5.0);
  let core = exp (-r * r * 5.0);
  var pig = spread * fade * (0.4 + 0.6 * veins + 0.7 * core) * cov;
  // The trace: a faint stain at the centre, still at full strength under reduced motion.
  let trace_end = secs + fx.p0.z;
  let trace = fx.p0.w * cov * exp (-r * r * 6.0) * (0.6 + 0.4 * fine)
    * (1.0 - smoothstep (secs, trace_end, age));
  pig = clamp (max (pig, trace), 0.0, 0.85);

  // Outside: the cloud past the edge, pulled back in as the bloom ends.
  let reach = fx.p0.y * (1.0 + fx.p1.x * level) * fade + 1e-3;
  let out = max (d, 0.0);
  var cloud = spread * exp (-out / reach) * (0.25 + 1.3 * fib * fib) * smoothstep (-0.01, 0.02, d);
  cloud *= 0.7 * fade * (1.0 - cov);

  let ink_lin = fx.c0.rgb;
  let pale = mix (ink_lin, vec3f (0.8, 0.86, 1.0), 0.1 + 0.3 * fine);
  let a_out = pig + cloud * (1.0 - pig);
  let rgb = ink_lin * pig + pale * cloud * (1.0 - pig);
  return finish (rgb, a_out);
}

// ---- MO-87: liquid morph ------------------------------------------------------------------
//
// p0: seconds of the morph, how far each word's edge moves in while it is the other's
// turn (em), the radius of the join mid-way (em), how far the shapes melt mid-way (em). c0: the bridge tint (alpha 0 for none).
//
// One quad per word: the old glyphs (part 1) and the new ones (part 2) as two fields, and
// glyphs both words share (part 0) in both, sliding from their old place to the new one.
// The changed letters drain and fill through a smooth union and melt mid-way, so they flow
// like liquid; the shared letters are drawn on their own and stay exactly as they are.

// Symmetric ease in and out, so the liquid middle gets as much time as either end.
fn ease_morph (x: f32) -> f32 {
  return x * x * (3.0 - 2.0 * x);
}

// The liquid field at `px`: the old letters drain (their edge moves in by up to `shrink`) as
// the new ones fill, joined in a smooth union whose radius peaks mid-way.
fn liquid (q: Quad, px: vec2f, p: f32) -> f32 {
  var da = 1e3;
  var db = 1e3;
  for (var i = 0u; i < q.range.y; i++) {
    let g = glyphs[q.range.x + i];
    let part = u32 (g.info.x);
    if (part == 0u) {
      continue;
    }
    let d = field_em (g, em_at (g, px, 0.0));
    if (part == 1u) {
      da = min (da, d);
    } else {
      db = min (db, d);
    }
  }
  let shrink = fx.p0.y;
  return smin (da + p * shrink, db + (1.0 - p) * shrink, fx.p0.z * sin (PI * p) + 1e-3);
}

fn morph (q: Quad, px: vec2f) -> vec4f {
  let first = glyphs[q.range.x];
  let p = ease_morph (clamp (first.place.w / fx.p0.x, 0.0, 1.0));
  let bulge = sin (PI * p);
  let ppe = first.place.z;
  // Shared letters: solid, sliding from their old place to the new one.
  var ds = 1e3;
  var col = first.color;
  for (var i = 0u; i < q.range.y; i++) {
    let g = glyphs[q.range.x + i];
    let part = u32 (g.info.x);
    if (part == 0u) {
      ds = min (ds, field_em (g, em_at (g, px, g.info.z * (1.0 - p))));
    }
    if (part != 1u) {
      col = g.color;
    }
  }
  // The changed letters, melted: the liquid field averaged over a small disc, which rounds
  // every corner into a drop, then swollen back by about what the average thins.
  var dl = liquid (q, px, p);
  let melt = fx.p0.w * bulge;
  if (melt * ppe > 0.5) {
    var sum = dl;
    for (var i = 0; i < 8; i++) {
      let a = f32 (i) * PI * 0.25;
      sum += liquid (q, px + vec2f (cos (a), sin (a)) * melt * ppe, p);
    }
    dl = sum / 9.0 - 0.3 * melt;
  }
  let d = min (ds, dl) * ppe;
  // The melted field changes slower than one per pixel; antialias by its own slope so the
  // drops keep crisp edges.
  let cov = clamp (0.5 - d / clamp (fwidth (d), 0.25, 1.0), 0.0, 1.0);
  let tint = fx.c0.a * bulge * clamp ((ds - dl) * ppe, 0.0, 1.0);
  let rgb = mix (col.rgb, fx.c0.rgb, tint);
  let a = cov * col.a;
  return finish (rgb * a, a);
}

// ---- MO-84: light through letters ---------------------------------------------------------
//
// p0: shadow length (pixels), shadow strength, caustic strength, caustic scale (pixels per
// cell). p1: glow falloff (pixels), caustic speed, sheen on the letters, 0. c0: the light,
// c1: the accent the letters nearest the light take.

// A soft caustic net at `uv` (cells), 0..1: the classic iterated water pattern.
fn caustic (uv: vec2f, time: f32) -> f32 {
  let p = uv * 6.2831853 - vec2f (250.0);
  var i = p;
  var c = 1.0;
  let inten = 0.005;
  for (var n = 0; n < 4; n++) {
    let t = time * (1.0 - 3.5 / f32 (n + 1));
    i = p + vec2f (cos (t - i.x) + sin (t + i.y), sin (t - i.y) + cos (t + i.x));
    c += 1.0 / length (vec2f (p.x / (sin (i.x + t) / inten), p.y / (cos (i.y + t) / inten)));
  }
  c /= 4.0;
  c = 1.17 - pow (c, 1.4);
  return clamp (pow (abs (c), 8.0), 0.0, 1.0);
}

fn mask_at (px: vec2f) -> f32 {
  let uv = (px - fx.rect.xy) / fx.rect.zw;
  if (any (uv < vec2f (0.0)) || any (uv > vec2f (1.0))) {
    return 0.0;
  }
  return textureSampleLevel (mask, samp, uv, 0.0).r;
}

fn light_under (px: vec2f) -> vec4f {
  let l = fx.light.xy;
  let inten = fx.light.z;
  let to = l - px;
  let dist = length (to);
  let dir = to / max (dist, 1e-3);
  // Shadow: march toward the light; a letter on the way shades this point, less the
  // farther back it stands, so the shadow is long and soft at its tail.
  let reach = min (fx.p0.x, dist);
  let jitter = fract (52.9829189 * fract (dot (floor (px), vec2f (0.06711056, 0.00583715))));
  var occ = 0.0;
  for (var i = 0; i < 16; i++) {
    let s = (f32 (i) + jitter) / 16.0;
    let m = mask_at (px + dir * s * reach);
    occ = max (occ, m * (1.0 - s * s));
  }
  let glow = inten * pow (exp (-dist / fx.p1.x), 1.5);
  var t = fx.time.y * fx.p1.y;
  if (fx.light.w > 0.5) {
    t = 0.0;
  }
  let cells = (px - l * 0.35) / fx.p0.w;
  let net = caustic (cells, t + 23.0) * fx.p0.z * (0.4 + 0.6 * fx.time.z);
  // An onset sends a ring of light out from the source.
  let ring_r = fx.time.w * 600.0;
  let ring = exp (-pow ((dist - ring_r) / 40.0, 2.0)) * exp (-fx.time.w * 4.0) * 0.5;
  let lit = glow * (0.3 + net * (0.5 + glow) + ring) * (1.0 - fx.p0.y * occ);
  let rgb = fx.c0.rgb * lit;
  // Shadows also darken a little of what is below, so they read on a lit panel.
  let shade = 0.25 * fx.p0.y * occ * glow;
  return finish (rgb, shade);
}

// Coverage into the stencil mask.
fn coverage (g: Glyph, px: vec2f) -> f32 {
  let d = field_em (g, em_at (g, px, 0.0)) * g.place.z;
  return clamp (0.5 - d, 0.0, 1.0);
}

// On the letters: the ones nearest the light take the accent.
fn light_over (g: Glyph, px: vec2f) -> vec4f {
  let cov = coverage (g, px);
  let l = fx.light.xy;
  let near = exp (-abs (px.x - l.x) / (1.2 * g.place.z)) * fx.light.z;
  // Painted over the letter rather than added, so white type turns the accent.
  let k = clamp (cov * near * fx.p1.z, 0.0, 0.8);
  return finish (fx.c1.rgb * k, k);
}

@fragment fn fs_fx (in: VsOut) -> @location(0) vec4f {
  let q = quads[in.quad];
  let mode = u32 (fx.time.x);
  switch (mode) {
    case 0u: { return ink (glyphs[q.range.x], in.px); }
    case 1u: { return morph (q, in.px); }
    case 2u: { return light_under (in.px); }
    case 4u: { return light_over (glyphs[q.range.x], in.px); }
    default: { return vec4f (0.0); }
  }
}

// The stencil mask: the coverage of every glyph, merged by max blending.
@fragment fn fs_mask (in: VsOut) -> @location(0) vec4f {
  let q = quads[in.quad];
  return vec4f (coverage (glyphs[q.range.x], in.px), 0.0, 0.0, 1.0);
}
