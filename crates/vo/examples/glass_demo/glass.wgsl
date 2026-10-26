// Glass panel material (FX06) with an animated rim (FX08), drawn as one quad under the
// scene. The pass blends "destination over" (src * (1 - dst.a) + dst), so it can run after
// the scene's own pass and still land behind the text.
//
// Units are physical pixels. Colours are linear light, premultiplied; `finish` encodes sRGB
// when the target is a plain UNORM format.

struct Glass {
  // Panel rect x, y, w, h.
  rect: vec4f,
  // Corner radius, physical pixels per logical pixel, time in seconds, compositor blur on (1).
  shape: vec4f,
  // Tint, linear straight rgb, and its alpha in the panel interior.
  tint: vec4f,
  // Rim orbit phase 0..1 (negative: off), orbit strength, sheen progress 0..1 (negative:
  // off), dark glass (1) or light glass (0).
  anim: vec4f,
  // Rim accent colour, linear straight rgb, and the edge alpha (interior alpha at the bevel).
  accent: vec4f,
  // Target width and height; z is 1 when the shader must encode sRGB itself.
  viewport: vec4f,
};

@group(0) @binding(0) var<uniform> g: Glass;

struct VsOut {
  @builtin(position) clip: vec4f,
  @location(0) p: vec2f,
};

@vertex fn vs_glass (@builtin(vertex_index) vi: u32) -> VsOut {
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  let m = 2.0;
  let p = mix (g.rect.xy - m, g.rect.xy + g.rect.zw + m, c);
  let n = p / g.viewport.xy * 2.0 - 1.0;
  var o: VsOut;
  o.clip = vec4f (n.x, -n.y, 0.0, 1.0);
  o.p = p;
  return o;
}

fn sd_box (p: vec2f, half: vec2f, r: f32) -> f32 {
  let q = abs (p) - half + vec2f (r);
  return length (max (q, vec2f (0.0))) + min (max (q.x, q.y), 0.0) - r;
}

// Outward unit gradient of `sd_box`, exact away from the medial axis.
fn grad_box (p: vec2f, half: vec2f, r: f32) -> vec2f {
  let q = abs (p) - half + vec2f (r);
  var n: vec2f;
  if (q.x > 0.0 && q.y > 0.0) {
    n = normalize (q);
  } else if (q.x > q.y) {
    n = vec2f (1.0, 0.0);
  } else {
    n = vec2f (0.0, 1.0);
  }
  return n * sign (p + vec2f (1e-6));
}

// FX10 hash: a cheap per-pixel value in [0, 1).
fn hash21 (p: vec2f) -> f32 {
  var q = fract (p * vec2f (123.34, 456.21));
  q += dot (q, q + 45.32);
  return fract (q.x * q.y);
}

fn srgb_encode (c: vec3f) -> vec3f {
  let lo = c * 12.92;
  let hi = 1.055 * pow (max (c, vec3f (0.0)), vec3f (1.0 / 2.4)) - 0.055;
  return select (hi, lo, c <= vec3f (0.0031308));
}

fn finish (c: vec4f) -> vec4f {
  if (g.viewport.z == 0.0 || c.a <= 0.0) {
    return c;
  }
  let straight = clamp (c.rgb / c.a, vec3f (0.0), vec3f (1.0));
  return vec4f (srgb_encode (straight) * c.a, c.a);
}

// Height of the bevel at `t` (0 on the edge, 1 where the flat top starts): a quarter circle.
fn bevel_slope (t: f32) -> f32 {
  let u = clamp (1.0 - t, 0.0, 0.999);
  return u / sqrt (1.0 - u * u);
}

@fragment fn fs_glass (in: VsOut) -> @location(0) vec4f {
  let k = g.shape.y;
  let half = 0.5 * g.rect.zw;
  let centre = g.rect.xy + half;
  let q = in.p - centre;
  let r = min (g.shape.x, min (half.x, half.y));
  let d = sd_box (q, half, r);
  let cover = clamp (0.5 - d, 0.0, 1.0);
  if (cover <= 0.0) {
    discard;
  }
  let inside = max (-d, 0.0);
  let dark = g.anim.w;

  // Bevel: 12 logical pixels wide, normal tilted outward along the SDF gradient.
  let bw = 12.0 * k;
  let t = clamp (inside / bw, 0.0, 1.0);
  let gn = grad_box (q, half, r);
  let slope = select (0.0, bevel_slope (t), t < 1.0);
  let n = normalize (vec3f (gn * min (slope, 6.0), 1.0));

  // Light from the upper left, a little in front.
  let l = normalize (vec3f (-0.45, -0.75, 0.55));
  let v = vec3f (0.0, 0.0, 1.0);
  let h = normalize (l + v);
  let spec = pow (max (dot (n, h), 0.0), 40.0);
  let lambert = dot (n, l) - l.z;

  // Fresnel (Schlick, F0 0.04) on the bevel's view angle.
  let fres = 0.04 + 0.96 * pow (1.0 - max (n.z, 0.0), 5.0);

  // Tint: the interior alpha meets the contrast bar; the bevel thins towards the edge so the
  // backdrop shows more where no text sits.
  // The alpha ramp spans 12 logical pixels, inside the panel padding, so text always sits
  // on the full interior alpha.
  let a_base = mix (g.accent.w, g.tint.w, smoothstep (0.0, 1.0, clamp (inside / (12.0 * k), 0.0, 1.0)));
  var col = g.tint.rgb;
  // A faint vertical gradient: lighter on top as in a lit sheet.
  let vy = clamp ((in.p.y - g.rect.y) / g.rect.w, 0.0, 1.0);
  col *= mix (1.25, 0.85, vy);

  // Light: rim from the bevel facing the light, with dispersion (each channel reads the
  // bevel a little further in), Fresnel and the specular glint.
  var rim = vec3f (0.0);
  for (var c = 0; c < 3; c++) {
    let tc = clamp ((inside - f32 (c) * 0.9 * k) / bw, 0.0, 1.0);
    let sc = select (0.0, bevel_slope (tc), tc < 1.0);
    let nc = normalize (vec3f (gn * min (sc, 6.0), 1.0));
    let lc = max (dot (nc, l) - l.z, 0.0);
    rim[c] = lc;
  }
  let light = rim * 0.55 + vec3f (spec * 0.9 + fres * 0.35);
  let shade = max (-lambert, 0.0) * 0.25;

  // The edge line: 1 physical pixel, brighter on the lit side.
  let line = clamp (1.0 - abs (d + 0.5 * k), 0.0, 1.0);
  let lit_edge = 0.5 + 0.5 * dot (gn, -normalize (l.xy));
  let edge = line * mix (0.10, 0.55, lit_edge);

  // Sheen: a soft diagonal band crossing once on show.
  var sheen = 0.0;
  if (g.anim.z >= 0.0) {
    let s = ((in.p.x - g.rect.x) + 0.6 * (in.p.y - g.rect.y)) / (g.rect.z + 0.6 * g.rect.w);
    let x = (s - (g.anim.z * 1.5 - 0.25)) / 0.07;
    sheen = exp (-x * x) * 0.22 * (1.0 - smoothstep (0.85, 1.0, g.anim.z));
  }

  // Orbiting rim segment: angle around the centre, corrected for the aspect so it moves at
  // an even speed along the long sides.
  var orbit = vec3f (0.0);
  if (g.anim.x >= 0.0) {
    let a = atan2 (q.y / half.y, q.x / half.x) / 6.2831853 + 0.5;
    var da = abs (a - g.anim.x);
    da = min (da, 1.0 - da);
    let seg = exp (-(da * da) / (2.0 * 0.04 * 0.04));
    let ring = exp (-pow (inside / (3.5 * k), 2.0)) + 0.25 * exp (-pow (inside / (6.0 * k), 2.0));
    orbit = g.accent.rgb * seg * ring * g.anim.y;
  }

  // Grain: static, so a settled panel costs no frames.
  let grain = (hash21 (floor (in.p)) - 0.5) * 0.024;

  // Grain scales the tint, so it reads the same on light and dark glass.
  var rgb = col * (1.0 - shade) * (1.0 + 6.0 * grain);
  let add = light * mix (0.6, 1.0, dark) * (1.0 - t * 0.7) + vec3f (edge + sheen) + orbit;
  // Light adds to the colour and to the coverage, like a reflection on top of the tint.
  let add_a = clamp (max (add.r, max (add.g, add.b)), 0.0, 1.0);
  let a = clamp (a_base + add_a * (1.0 - a_base), 0.0, 1.0);
  let premul = rgb * a_base + add;
  return finish (vec4f (min (premul, vec3f (a)), a) * cover);
}
