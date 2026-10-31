// Display-list glyphs: the Slug vertex stage with the layer's texture origin, and a fragment
// stage that adds the layer clip and the output encode. Appended after the Slug shader,
// `common.wgsl` and `noise.wgsl`. Instances are in target pixels, and `u.mvp` is the pixel camera of the
// target; a layer texture sized to its bounds moves the result by its origin.
//
// Glyph effects (TY17) come from the coverage itself, with no offscreen pass:
// - blur averages 12 to 24 coverage samples on a disc as wide as the blur (a Vogel spiral), each
//   with Slug's antialiasing window widened to a third of it, so the taps blend smoothly;
// - a halo is the same soft coverage, as wide as its radius, in its own colour, under the
//   glyph, moved by its offset for a drop shadow;
// - an outline strokes the glyph's edge instead of filling it, where the distance to the
//   edge (`edge_distance_px`) is under half the stroke width;
// - dissolve keeps the parts where a value noise fixed to the glyph's em space is above a
//   threshold that rises with the amount;
// - wipe hides the glyph from the right edge of its ink box, with a soft front;
// - chromatic offset takes the red and blue coverage a few pixels left and right;
// - weight (MO-03, `fx3.w`) grows each edge that many pixels, or thins it when negative:
//   coverage from the signed distance to the outline, moved by the weight.
//
// A material (FX15, `materials.wgsl`, appended) gets the glyph's colour, its coverage and its
// em coordinates; `fx2.y` holds the material id, `fx2.z` a seed per glyph and `fx2.w` em units
// per outline unit.

@vertex fn vs_dl_glyph (@builtin(vertex_index) vi: u32, g: GlyphInstance) -> VsOut {
  // The body of `vs_glyph`, which as an entry point cannot be called.
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  let m = mat2x2f (g.lin.xy, g.lin.zw);
  // Motion blur widens the ink box by half the smear (in em units) on each side, and the
  // effects by how far they reach in pixels.
  let det0 = g.lin.x * g.lin.w - g.lin.z * g.lin.y;
  let inv = mat2x2f (vec2f (g.lin.w, -g.lin.y), vec2f (-g.lin.z, g.lin.x)) * (1.0 / det0);
  // A blur tap reaches half the blur plus half its window, a halo twice that per radius. A
  // weight grows the quad by whole pixels: a quad that changed size by a hair each frame
  // would move every pixel's em coordinate by a rounding error, and the ink would flicker
  // as a weight eases (EG-23).
  let reach_px = 0.7 * g.fx.x + abs (g.fx.w) + 1.4 * g.fx2.x + length (g.fx3.xy) + 0.5 * g.fx3.z + ceil (max (g.fx3.w, 0.0));
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
  if (warped ()) {
    // A turned layer (vo_scene::stage): through the warp. Slug's coverage takes its window
    // from screen derivatives, so it stays sharp in perspective.
    out.clip = px_to_ndc (d[0]);
  } else if (any (layer.origin.xy != vec2f (0.0)) || any (layer.viewport.xy != u.viewport)) {
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
  out.fx3 = g.fx3;
  return out;
}

// Coverage at `tc` with an antialiasing window `k` pixels wide, averaged over the motion
// blur smear. Slug coverage is only right inside the glyph's ink box (grown by any window
// past the usual pixel);
// a smear sample outside it is empty.
fn glyph_coverage (tc: vec2f, loc: u32, ppe: vec2f, k: f32, smear_em: vec2f, ext: vec4f) -> f32 {
  if (all (layer.smear.zw == vec2f (0.0))) {
    // An effect's reach grows the quad past the ink box; out there, past the antialiasing
    // window and the small-size supersampling taps (a third of a pixel out), there is no ink.
    let margin = (0.5 * k + 1.0) / ppe;
    if (any (tc < ext.xy - margin) || any (tc > ext.zw + margin)) {
      return 0.0;
    }
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

// Coverage softened over a disc `width` pixels across: 12 to 24 taps on a Vogel spiral,
// more for a wider disc, each with a window a third of the width, single sample (the blur
// makes Slug's small-size supersampling pointless). A tap outside the ink box, grown by the
// window, is empty: Slug coverage there is not valid and shows as streaks. The spiral turns
// by `turn` radians, a different amount per pixel, so the taps' pattern shows as fine grain
// instead of streaks. `px_x` and `px_y` are one pixel in em units.
fn soft_coverage (tc: vec2f, loc: u32, ppe: vec2f, width: f32, px_x: vec2f, px_y: vec2f, ext: vec4f, turn: f32) -> f32 {
  let k = 1.0 + width / 3.0;
  let r = 0.5 * width;
  let margin = 0.5 * k / ppe;
  let n = clamp (i32 (ceil (width / 2.0)), 12, 24);
  // The disc's reach in em units along each axis: when it misses the grown ink box, every tap
  // would be skipped.
  let reach = r * sqrt (px_x * px_x + px_y * px_y);
  if (any (tc + reach < ext.xy - margin) || any (tc - reach > ext.zw + margin)) {
    return 0.0;
  }
  var cov = 0.0;
  for (var i = 0; i < n; i++) {
    let d = r * sqrt ((f32 (i) + 0.5) / f32 (n));
    let a = f32 (i) * 2.3999632 + turn;
    let p = tc + px_x * (d * cos (a)) + px_y * (d * sin (a));
    if (all (p >= ext.xy - margin) && all (p <= ext.zw + margin)) {
      cov += _hb_gpu_slug_single (p, ppe / k, loc);
    }
  }
  return cov / f32 (n);
}

@fragment fn fs_dl_glyph (in: VsOut) -> @location(0) vec4f {
  // Plain glyphs take Slug's own path, unchanged. Sharing the derivatives below with Slug's
  // `fwidth` changes rotated text by a few pixels on some drivers. The branch is uniform
  // over a quad in practice: every lane of it belongs to one instance.
  if (all (in.fx == vec4f (0.0)) && in.fx2.x == 0.0 && all (in.fx3.zw == vec2f (0.0)) && in.fx2.y == 0.0 && all (layer.smear.zw == vec2f (0.0))) {
    return finish (in.color * (hb_gpu_draw (in.tc, in.loc) * clip_coverage (in.clip.xy)));
  }
  let ppe = 1.0 / fwidth (in.tc);
  let px_em = dpdx (in.tc);
  // The smear in em units, from how the em coordinate changes per pixel.
  let smear_em = px_em * layer.smear.z + dpdy (in.tc) * layer.smear.w;
  let ext = vec4f (hb_gpu_fetch (i32 (in.loc))) * HB_GPU_INV_UNITS;

  let py_em = dpdy (in.tc);
  let turn = 6.2831853 * noise_ign (floor (in.clip.xy));
  let blurred = in.fx.x > 0.75;
  let k = select (1.0 + max (in.fx.x, 0.0), 1.0, blurred);
  var cov: f32;
  if (blurred) {
    cov = soft_coverage (in.tc, in.loc, ppe, in.fx.x, px_em, py_em, ext, turn);
  } else {
    cov = glyph_coverage (in.tc, in.loc, ppe, k, smear_em, ext);
  }
  if (in.fx3.z > 0.0) {
    cov = clamp (0.5 * in.fx3.z + 0.5 - edge_distance_px (in.tc, in.loc, ppe), 0.0, 1.0);
  } else if (in.fx3.w != 0.0 && !blurred) {
    // Signed distance, outside positive, with the side from the coverage itself. The weight
    // adds what growing the edge adds to the distance's own coverage, on top of Slug's: a
    // small weight then changes the ink by the outline length times the weight and no more,
    // and nothing at all at zero, where the distance's coverage alone would differ from
    // Slug's by a few percent of the ink (EG-23).
    let d = edge_distance_px (in.tc, in.loc, ppe);
    let s = select (d, -d, cov > 0.5);
    cov = clamp (cov + clamp (0.5 - s + in.fx3.w, 0.0, 1.0) - clamp (0.5 - s, 0.0, 1.0), 0.0, 1.0);
  }
  var c = in.color * cov;
  if (in.fx.w != 0.0) {
    // Red from the left, blue from the right; alpha covers all three.
    let off = px_em * in.fx.w;
    var r: f32;
    var b: f32;
    if (blurred) {
      r = soft_coverage (in.tc + off, in.loc, ppe, in.fx.x, px_em, py_em, ext, turn);
      b = soft_coverage (in.tc - off, in.loc, ppe, in.fx.x, px_em, py_em, ext, turn);
    } else {
      r = glyph_coverage (in.tc + off, in.loc, ppe, k, smear_em, ext);
      b = glyph_coverage (in.tc - off, in.loc, ppe, k, smear_em, ext);
    }
    let rgb = in.color.rgb * vec3f (r, cov, b);
    c = vec4f (rgb, in.color.a * max (cov, max (r, b)));
  }
  let mat = u32 (in.fx2.y);
  if (mat != 0u) {
    var mi: MatIn;
    mi.color = in.color;
    mi.cov = cov;
    mi.has_dist = false;
    mi.edge = 0.0;
    mi.centre = 0.0;
    mi.half_w = 0.0;
    mi.px = in.clip.xy + layer.origin.xy;
    mi.p = mat_px_to_layer (mi.px);
    mi.em = in.tc * in.fx2.w;
    mi.ppu = layer.t.z;
    mi.seed = in.fx2.z;
    c = material (mat, mi);
  }
  if (in.fx2.x > 0.0 && in.halo.a > 0.0 && c.a < 1.0) {
    let at = in.tc - px_em * in.fx3.x - py_em * in.fx3.y;
    let h = soft_coverage (at, in.loc, ppe, 2.0 * in.fx2.x, px_em, py_em, ext, turn);
    c = c + in.halo * h * (1.0 - c.a);
  }
  var keep = 1.0;
  if (in.fx.y > 0.0) {
    // About 6 noise cells per em, fixed to the glyph and seeded by it, so it dissolves in
    // place and each glyph differently.
    let w = 0.12;
    let e = in.fx.y * (1.0 + w);
    keep *= smoothstep (e - w, e, noise_value2 (in.tc * 6.0, in.loc));
  }
  if (in.fx.z > 0.0) {
    let s = 0.3;
    let u = (in.tc.x - ext.x) / max (ext.z - ext.x, 1e-4);
    let e = (1.0 - in.fx.z) * (1.0 + s);
    keep *= 1.0 - smoothstep (e - s, e, u);
  }
  return finish (c * (keep * clip_coverage (in.clip.xy)));
}

// Distance in pixels from `tc` to the glyph's outline, from the band data Slug draws with: the
// nearest crossing along the pixel's row and along its column, taken as the distance to a
// straight edge through both, or the nearest curve end when that is closer (outer corners,
// where the row and the column both miss the glyph). Exact for straight edges and close for
// curves. A corner in a band the row and the column do not touch is missed, which can nick a
// thick stroke's corner.
fn edge_distance_px (tc: vec2f, loc: u32, ppe: vec2f) -> f32 {
  let gi = _hb_gpu_decode_glyph (tc, loc);
  let far = 1e6;
  var dh = far;
  var dv = far;
  var de = far;
  let hband = hb_gpu_fetch (gi.bandBase + gi.bandIndex.y);
  for (var ci = 0; ci < hband.r; ci++) {
    let at = hb_gpu_fetch (gi.glyphLoc + hband.g + 32768 + ci).r + 32768;
    let q12 = vec4f (hb_gpu_fetch (gi.glyphLoc + at)) * HB_GPU_INV_UNITS;
    let raw3 = hb_gpu_fetch (gi.glyphLoc + at + 1);
    let q3 = vec2f (vec2<i32> (raw3.r, raw3.g)) * HB_GPU_INV_UNITS;
    let p12 = q12 - vec4f (tc, tc);
    let p3 = q3 - tc;
    de = min (de, min (length (p12.xy * ppe), length (p3 * ppe)));
    let code = _hb_gpu_calc_root_code (p12.y, p12.w, p3.y);
    if (code != 0u) {
      let r = abs (_hb_gpu_solve_horiz_poly (q12.xy - q12.zw * 2.0 + q3, q12.xy - q12.zw, p12.xy)) * ppe.x;
      if ((code & 1u) != 0u) { dh = min (dh, r.x); }
      if (code > 1u) { dh = min (dh, r.y); }
    }
  }
  let vband = hb_gpu_fetch (gi.bandBase + gi.numHBands + gi.bandIndex.x);
  for (var ci = 0; ci < vband.r; ci++) {
    let at = hb_gpu_fetch (gi.glyphLoc + vband.g + 32768 + ci).r + 32768;
    let q12 = vec4f (hb_gpu_fetch (gi.glyphLoc + at)) * HB_GPU_INV_UNITS;
    let raw3 = hb_gpu_fetch (gi.glyphLoc + at + 1);
    let q3 = vec2f (vec2<i32> (raw3.r, raw3.g)) * HB_GPU_INV_UNITS;
    let p12 = q12 - vec4f (tc, tc);
    let p3 = q3 - tc;
    de = min (de, min (length (p12.xy * ppe), length (p3 * ppe)));
    let code = _hb_gpu_calc_root_code (p12.x, p12.z, p3.x);
    if (code != 0u) {
      let r = abs (_hb_gpu_solve_vert_poly (q12.xy - q12.zw * 2.0 + q3, q12.xy - q12.zw, p12.xy)) * ppe.y;
      if ((code & 1u) != 0u) { dv = min (dv, r.x); }
      if (code > 1u) { dv = min (dv, r.y); }
    }
  }
  var d = min (dh, dv);
  if (dh < far && dv < far) {
    d = dh * dv / max (sqrt (dh * dh + dv * dv), 1e-6);
  }
  return min (d, de);
}

// Motion blur takes one sample per pixel of smear, 4 to 32 of them, so thin stems do not show
// as separate copies.
fn smear_samples () -> i32 {
  return clamp (i32 (ceil (length (layer.smear.zw))), 4, 32);
}
