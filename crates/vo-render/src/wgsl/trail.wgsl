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

// Trails that drift and turn (EG-34): last frame's pixels, read back through the drift's map
// from this frame's pixel to where its afterglow was, bilinear. The pipeline multiplies by
// the fade in the blend constant. Light from outside the texture is gone, not smeared in.
struct Drift {
  // The map's turn and scale: the source of p is (m.x p.x - m.y p.y, m.y p.x + m.x p.y) + b.
  m: vec2f,
  b: vec2f,
};

@group(0) @binding(2) var samp: sampler;
@group(1) @binding(0) var<uniform> drift: Drift;

@fragment fn fs_trail_drift (@builtin(position) pos: vec4f) -> @location(0) vec4f {
  let p = pos.xy;
  let q = vec2f (drift.m.x * p.x - drift.m.y * p.y, drift.m.y * p.x + drift.m.x * p.y) + drift.b;
  let size = vec2f (textureDimensions (frame));
  if (any (q < vec2f (0.0)) || any (q > size)) {
    return vec4f (0.0);
  }
  return textureSampleLevel (frame, samp, q / size, 0.0);
}
