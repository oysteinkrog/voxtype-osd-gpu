// Display-list glyphs: the Slug vertex stage (`vs_glyph` in slug-glyph.wgsl) with a fragment
// stage that adds the layer clip and the output encode. Appended after the Slug shader and
// `common.wgsl`. Instances are already in physical pixels, and `u.mvp` is the pixel camera
// of the target.

@fragment fn fs_dl_glyph (in: VsOut) -> @location(0) vec4f {
  let cov = hb_gpu_draw (in.tc, in.loc);
  return finish (in.color * (cov * clip_coverage (in.clip.xy)));
}
