// Feedback trails (FX11): merges this frame, drawn on its own into a cleared texture, into
// the trail's faded pixels. The pipeline blends with max, so each channel keeps the larger
// of the afterglow and the new frame.

@group(0) @binding(0) var frame: texture_2d<f32>;

@vertex fn vs_trail (@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
  let c = vec2f (f32 ((vi << 1u) & 2u), f32 (vi & 2u));
  return vec4f (c * vec2f (2.0, -2.0) + vec2f (-1.0, 1.0), 0.0, 1.0);
}

@fragment fn fs_trail_max (@builtin(position) pos: vec4f) -> @location(0) vec4f {
  return textureLoad (frame, vec2i (pos.xy), 0);
}
