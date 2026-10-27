// Display-list glyphs: the Slug vertex stage with the layer's texture origin, and a fragment
// stage that adds the layer clip and the output encode. Appended after the Slug shader and
// `common.wgsl`. Instances are in target pixels, and `u.mvp` is the pixel camera of the
// target; a layer texture sized to its bounds moves the result by its origin.
//
// Glyph effects (TY17) come from the coverage itself, with no offscreen pass:
// - blur averages 12 coverage samples on a disc as wide as the blur (a Vogel spiral), each
//   with Slug's antialiasing window widened to a third of it, so the taps blend smoothly;
// - a halo is the same soft coverage, as wide as its radius, in its own colour, under the
//   glyph;
// - dissolve keeps the parts where a value noise fixed to the glyph's em space is above a
//   threshold that rises with the amount;
// - wipe hides the glyph from the right edge of its ink box, with a soft front;
// - chromatic offset takes the red and blue coverage a few pixels left and right.

@vertex fn vs_dl_glyph (@builtin(vertex_index) vi: u32, g: GlyphInstance) -> VsOut {
  // The body of `vs_glyph`, which as an entry point cannot be called.
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  let m = mat2x2f (g.lin.xy, g.lin.zw);
  // Motion blur widens the ink box by half the smear (in em units) on each side, and the
  // effects by how far they reach in pixels.
  let det0 = g.lin.x * g.lin.w - g.lin.z * g.lin.y;
  let inv = mat2x2f (vec2f (g.lin.w, -g.lin.y), vec2f (-g.lin.z, g.lin.x)) * (1.0 / det0);
  // A blur tap reaches half the blur plus half its window, a halo twice that per radius.
  let reach_px = 0.7 * g.fx.x + abs (g.fx.w) + 1.4 * g.fx2.x;
  let em_per_px = abs (inv[0]) + abs (inv[1]);
  let grow = 0.5 * abs (inv * layer.smear.zw) + reach_px * em_per_px;
  let em = mix (g.ext.xy - grow, g.ext.zw + grow, c);
  let pos = m * em + g.offset;
  let s = c * 2.0 - 1.0;
  let normal = normalize (m * vec2f (s.x, 0.0)) + normalize (m * vec2f (0.0, s.y));
  let det = g.lin.x * g.lin.w - g.lin.z * g.lin.y;
  let jac = vec4f (g.lin.w, -g.lin.z, -g.lin.y, g.lin.x) / det;
  let d = hb_gpu_dilate (pos, em, normal, jac, u.mvp, u.viewport);

  var out: VsOut;
  out.clip = u.mvp * vec4f (d[0], 0.0, 1.0);
  if (any (layer.origin.xy != vec2f (0.0)) || any (layer.viewport.xy != u.viewport)) {
    let n = (d[0] - layer.origin.xy) / layer.viewport.xy * 2.0 - 1.0;
    out.clip = vec4f (n.x, -n.y, out.clip.z, out.clip.w);
  }
  out.tc = d[1];
  out.loc = g.loc;
  out.color = g.color;
  out.word = g.word;
  out.fx = g.fx;
  out.fx2 = g.fx2;
  out.halo = g.halo;
  return out;
}

// Coverage at `tc` with an antialiasing window `k` pixels wide, averaged over the motion
// blur smear. Slug coverage is only right inside the glyph's ink box (grown by any window
// past the usual pixel);
// a smear sample outside it is empty.
fn glyph_coverage (tc: vec2f, loc: u32, ppe: vec2f, k: f32, smear_em: vec2f, ext: vec4f) -> f32 {
  if (all (layer.smear.zw == vec2f (0.0))) {
    return _hb_gpu_slug (tc, ppe / k, loc);
  }
  let margin = 0.5 * (k - 1.0) / ppe;
  var cov = 0.0;
  let n = smear_samples ();
  for (var i = 0; i < n; i++) {
    let f = (f32 (i) + 0.5) / f32 (n) - 0.5;
    let p = tc - f * smear_em;
    let c = _hb_gpu_slug (p, ppe / k, loc);
    if (all (p >= ext.xy - margin) && all (p <= ext.zw + margin)) {
      cov += c;
    }
  }
  return cov / f32 (n);
}

// Coverage softened over a disc `width` pixels across: 12 taps on a Vogel spiral, each
// with a window a third of the width, single sample (the blur makes Slug's small-size
// supersampling pointless). `px_x` and `px_y` are one pixel in em units.
fn soft_coverage (tc: vec2f, loc: u32, ppe: vec2f, width: f32, px_x: vec2f, px_y: vec2f, ext: vec4f) -> f32 {
  let k = 1.0 + width / 3.0;
  let r = 0.5 * width;
  var cov = 0.0;
  for (var i = 0; i < 12; i++) {
    let d = r * sqrt ((f32 (i) + 0.5) / 12.0);
    let a = f32 (i) * 2.3999632;
    let p = tc + px_x * (d * cos (a)) + px_y * (d * sin (a));
    cov += _hb_gpu_slug_single (p, ppe / k, loc);
  }
  return cov / 12.0;
}

// Value noise in 0..1 on a unit grid, smooth between lattice points.
fn glyph_noise (p: vec2f) -> f32 {
  let i = floor (p);
  let f = fract (p);
  let u = f * f * (3.0 - 2.0 * f);
  return mix (mix (hash21 (i), hash21 (i + vec2f (1.0, 0.0)), u.x),
              mix (hash21 (i + vec2f (0.0, 1.0)), hash21 (i + vec2f (1.0, 1.0)), u.x), u.y);
}

fn hash21 (p: vec2f) -> f32 {
  let q = vec2u (vec2i (p) + vec2i (4096));
  var h = q.x * 0x27d4eb2du ^ (q.y * 0x165667b1u);
  h = (h ^ (h >> 15u)) * 0x2c1b3c6du;
  h = (h ^ (h >> 12u)) * 0x297a2d39u;
  h = h ^ (h >> 15u);
  return f32 (h) * (1.0 / 4294967296.0);
}

@fragment fn fs_dl_glyph (in: VsOut) -> @location(0) vec4f {
  // Plain glyphs take Slug's own path, unchanged. Sharing the derivatives below with Slug's
  // `fwidth` changes rotated text by a few pixels on some drivers. The branch is uniform
  // over a quad in practice: every lane of it belongs to one instance.
  if (all (in.fx == vec4f (0.0)) && in.fx2.x == 0.0 && all (layer.smear.zw == vec2f (0.0))) {
    return finish (in.color * (hb_gpu_draw (in.tc, in.loc) * clip_coverage (in.clip.xy)));
  }
  let ppe = 1.0 / fwidth (in.tc);
  let px_em = dpdx (in.tc);
  // The smear in em units, from how the em coordinate changes per pixel.
  let smear_em = px_em * layer.smear.z + dpdy (in.tc) * layer.smear.w;
  let ext = vec4f (hb_gpu_fetch (i32 (in.loc))) * HB_GPU_INV_UNITS;

  let py_em = dpdy (in.tc);
  let blurred = in.fx.x > 0.75;
  let k = select (1.0 + max (in.fx.x, 0.0), 1.0, blurred);
  var cov: f32;
  if (blurred) {
    cov = soft_coverage (in.tc, in.loc, ppe, in.fx.x, px_em, py_em, ext);
  } else {
    cov = glyph_coverage (in.tc, in.loc, ppe, k, smear_em, ext);
  }
  var c = in.color * cov;
  if (in.fx.w != 0.0) {
    // Red from the left, blue from the right; alpha covers all three.
    let off = px_em * in.fx.w;
    var r: f32;
    var b: f32;
    if (blurred) {
      r = soft_coverage (in.tc + off, in.loc, ppe, in.fx.x, px_em, py_em, ext);
      b = soft_coverage (in.tc - off, in.loc, ppe, in.fx.x, px_em, py_em, ext);
    } else {
      r = glyph_coverage (in.tc + off, in.loc, ppe, k, smear_em, ext);
      b = glyph_coverage (in.tc - off, in.loc, ppe, k, smear_em, ext);
    }
    let rgb = in.color.rgb * vec3f (r, cov, b);
    c = vec4f (rgb, in.color.a * max (cov, max (r, b)));
  }
  if (in.fx2.x > 0.0 && in.halo.a > 0.0) {
    let h = soft_coverage (in.tc, in.loc, ppe, 2.0 * in.fx2.x, px_em, py_em, ext);
    c = c + in.halo * h * (1.0 - c.a);
  }
  var keep = 1.0;
  if (in.fx.y > 0.0) {
    // About 6 noise cells per em, fixed to the glyph, so it dissolves in place.
    let w = 0.12;
    let e = in.fx.y * (1.0 + w);
    keep *= smoothstep (e - w, e, glyph_noise (in.tc * 6.0 + vec2f (f32 (in.loc % 97u) * 3.1)));
  }
  if (in.fx.z > 0.0) {
    let s = 0.3;
    let u = (in.tc.x - ext.x) / max (ext.z - ext.x, 1e-4);
    let e = (1.0 - in.fx.z) * (1.0 + s);
    keep *= 1.0 - smoothstep (e - s, e, u);
  }
  return finish (c * (keep * clip_coverage (in.clip.xy)));
}

// Motion blur takes one sample per pixel of smear, 4 to 32 of them, so thin stems do not show
// as separate copies.
fn smear_samples () -> i32 {
  return clamp (i32 (ceil (length (layer.smear.zw))), 4, 32);
}
