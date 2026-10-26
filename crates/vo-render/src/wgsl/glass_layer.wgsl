// Prelude of `glass.wgsl` inside the renderer's display list pass, after `common.wgsl`:
// instances in layer units, mapped to physical pixels by the layer block, and clipped by
// the layer clip. Glass is drawn axis-aligned: a rotated layer moves its corners but not
// the material.

@vertex fn vs_glass (@builtin(vertex_index) vi: u32, g: GlassIn) -> GlassOut {
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  let k = layer.t.z;
  let rect = vec4f (layer_to_px (g.rect.xy), g.rect.zw * k);
  // Room for the shadow below and around, and one pixel for antialiasing.
  let m = max (2.5 * g.shadow.y + abs (g.shadow.z), 0.0) * k + 2.0;
  let p = mix (rect.xy - m, rect.xy + rect.zw + m, c);
  var o: GlassOut;
  o.clip = px_to_ndc (p);
  o.p = p;
  o.rect = rect;
  o.shape = vec4f (g.shape.x * k, k, g.shape.zw);
  o.tint = g.tint;
  o.accent = g.accent;
  o.anim = g.anim;
  o.fx = g.fx;
  o.shadow = g.shadow;
  return o;
}

fn glass_clip (frag: vec2f) -> f32 {
  return clip_coverage (frag);
}
