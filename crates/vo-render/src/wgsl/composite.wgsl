// Composites an offscreen layer texture into its parent target. The layer texture has the
// parent's size and format, so this is a 1:1 copy with the layer's opacity, the parent's
// clip and the layer's blend mode (set on the pipeline). `common.wgsl` is prepended.
//
// Values pass through unconverted: on an sRGB or float target they are linear, on a plain
// UNORM target both textures hold the same encoded values.

@group(0) @binding(0) var layer_tex: texture_2d<f32>;

@vertex fn vs_composite (@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
  // One triangle that covers the whole target.
  let c = vec2f (f32 ((vi << 1u) & 2u), f32 (vi & 2u));
  return vec4f (c * vec2f (2.0, -2.0) + vec2f (-1.0, 1.0), 0.0, 1.0);
}

@fragment fn fs_composite (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let c = textureLoad (layer_tex, vec2i (frag.xy), 0);
  return c * (layer.t.w * clip_coverage (frag.xy));
}
