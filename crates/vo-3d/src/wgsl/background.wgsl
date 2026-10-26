// The background gradient: one triangle over the whole target, depth untouched.
// `common3d.wgsl` is prepended.

@vertex fn vs_bg (@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
  let c = vec2f (f32 ((vi << 1u) & 2u), f32 (vi & 2u));
  return vec4f (c * vec2f (2.0, -2.0) + vec2f (-1.0, 1.0), 1.0, 1.0);
}

@fragment fn fs_bg (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let k = clamp (frag.y / g.viewport.y, 0.0, 1.0);
  return finish (mix (g.bg_top, g.bg_bottom, k));
}
