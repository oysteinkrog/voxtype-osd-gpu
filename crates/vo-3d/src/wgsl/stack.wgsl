// Composites a full-size 2D layer over the stack's target, 1:1, premultiplied over (the blend
// is on the pipeline). Values pass through unconverted.

@group(0) @binding(0) var layer_tex: texture_2d<f32>;

@vertex fn vs_full (@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
  let c = vec2f (f32 ((vi << 1u) & 2u), f32 (vi & 2u));
  return vec4f (c * vec2f (2.0, -2.0) + vec2f (-1.0, 1.0), 0.0, 1.0);
}

@fragment fn fs_over (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  return textureLoad (layer_tex, vec2i (frag.xy), 0);
}
