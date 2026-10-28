// Prelude of `glass.wgsl` inside the renderer's display list pass, after `common.wgsl`:
// instances in layer units, mapped to physical pixels by the layer block, and clipped by
// the layer clip. The quad's corners go through the layer transform and its warp, so a
// rotated or tilted layer turns the panel with it; fragment positions stay in the panel's
// own frame (layer units times pixels per unit, plus the layer's translation), which on an
// unturned layer are target pixels.

// Outline points (EG-06): x, y and arc length in layer units. The renderer shares the
// vector segment buffer for them.
@group(0) @binding(0) var<storage, read> glass_points: array<vec4f>;

fn glass_point (i: u32) -> vec4f {
  return glass_points[i];
}

fn glass_origin () -> vec2f {
  return layer.t.xy;
}

// The panel frame to target pixels: the layer's rotation without its scale.
fn glass_turn () -> mat2x2f {
  return mat2x2f (layer.m.xy, layer.m.zw) * (1.0 / layer.t.z);
}

// Pixels as seen per pixel of the panel frame, for antialiasing on a tilted layer.
fn glass_aa (frag: vec2f) -> f32 {
  if (!warped ()) {
    return 1.0;
  }
  return warp_scale (frag_target (frag));
}

@vertex fn vs_glass (@builtin(vertex_index) vi: u32, g: GlassIn) -> GlassOut {
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  let k = layer.t.z;
  // Room for the shadow below and around, and one pixel for antialiasing, in layer units.
  let m = (max (2.5 * g.shadow.y + abs (g.shadow.z), 0.0) * k + 2.0) / k;
  let corner = mix (g.rect.xy - m, g.rect.xy + g.rect.zw + m, c);
  var o: GlassOut;
  o.clip = px_to_ndc (layer_to_px (corner));
  o.p = corner * k + layer.t.xy;
  o.rect = vec4f (g.rect.xy * k + layer.t.xy, g.rect.zw * k);
  o.shape = vec4f (g.shape.x * k, k, g.shape.zw);
  o.tint = g.tint;
  o.accent = g.accent;
  o.anim = g.anim;
  o.fx = g.fx;
  o.shadow = g.shadow;
  o.outline = g.outline;
  o.refract = g.refract;
  o.refract2 = g.refract2;
  return o;
}

fn glass_clip (frag: vec2f) -> f32 {
  return clip_coverage (frag);
}

// Target pixels (as seen) of a fragment, where the backdrop (EG-01) holds what is under it.
fn glass_frag_px (frag: vec2f) -> vec2f {
  return frag + layer.origin.xy;
}
