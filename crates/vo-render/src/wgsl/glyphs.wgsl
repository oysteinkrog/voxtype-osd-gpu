// Display-list glyphs: the Slug vertex stage with the layer's texture origin, and a fragment
// stage that adds the layer clip and the output encode. Appended after the Slug shader and
// `common.wgsl`. Instances are in target pixels, and `u.mvp` is the pixel camera of the
// target; a layer texture sized to its bounds moves the result by its origin.

@vertex fn vs_dl_glyph (@builtin(vertex_index) vi: u32, g: GlyphInstance) -> VsOut {
  // The body of `vs_glyph`, which as an entry point cannot be called.
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  let m = mat2x2f (g.lin.xy, g.lin.zw);
  // Motion blur widens the ink box by half the smear (in em units) on each side.
  let det0 = g.lin.x * g.lin.w - g.lin.z * g.lin.y;
  let inv = mat2x2f (vec2f (g.lin.w, -g.lin.y), vec2f (-g.lin.z, g.lin.x)) * (1.0 / det0);
  let grow = 0.5 * abs (inv * layer.smear.zw);
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
  return out;
}

@fragment fn fs_dl_glyph (in: VsOut) -> @location(0) vec4f {
  // The smear in em units, from how the em coordinate changes per pixel.
  let smear_em = dpdx (in.tc) * layer.smear.z + dpdy (in.tc) * layer.smear.w;
  var cov: f32;
  if (all (layer.smear.zw == vec2f (0.0))) {
    cov = hb_gpu_draw (in.tc, in.loc);
  } else {
    // Slug coverage is only right inside the glyph's ink box; a sample outside it is empty.
    let ext = vec4f (hb_gpu_fetch (i32 (in.loc))) * HB_GPU_INV_UNITS;
    let n = smear_samples ();
    for (var i = 0; i < n; i++) {
      let k = (f32 (i) + 0.5) / f32 (n) - 0.5;
      let tc = in.tc - k * smear_em;
      let c = hb_gpu_draw (tc, in.loc);
      if (all (tc >= ext.xy) && all (tc <= ext.zw)) {
        cov += c;
      }
    }
    cov /= f32 (n);
  }
  return finish (in.color * (cov * clip_coverage (in.clip.xy)));
}

// Motion blur takes one sample per pixel of smear, 4 to 32 of them, so thin stems do not show
// as separate copies.
fn smear_samples () -> i32 {
  return clamp (i32 (ceil (length (layer.smear.zw))), 4, 32);
}
