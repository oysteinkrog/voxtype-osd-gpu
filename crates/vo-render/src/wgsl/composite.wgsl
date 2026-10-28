// Composites an offscreen layer texture into its parent target, 1:1 in pixels, with the
// layer's opacity, the parent's clip and the layer's blend mode (set on the pipeline).
// `common.wgsl` is prepended.
//
// `noise.wgsl` comes before this file, for the glitch.
//
// The layer texture covers the layer's bounds: `layer.origin.zw` is where its top left sits
// in target pixels, `layer.origin.xy` the same for the texture drawn into, and the scissor
// keeps the draw inside the layer texture.
//
// Values pass through unconverted: on an sRGB or float target they are linear, on a plain
// UNORM target both textures hold the same encoded values.

@group(0) @binding(0) var layer_tex: texture_2d<f32>;

@vertex fn vs_composite (@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
  // One triangle that covers the whole target.
  let c = vec2f (f32 ((vi << 1u) & 2u), f32 (vi & 2u));
  return vec4f (c * vec2f (2.0, -2.0) + vec2f (-1.0, 1.0), 0.0, 1.0);
}

fn layer_texel (frag: vec2f) -> vec2i {
  return vec2i (floor (frag + layer.origin.xy - layer.origin.zw));
}

@fragment fn fs_composite (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let c = textureLoad (layer_tex, layer_texel (frag.xy), 0);
  return c * (layer.t.w * clip_coverage (frag.xy));
}

// `compose.wgsl` composites a layer with bloom, a style or a mask: binding 1 is its mask,
// binding 2 the sampler for the bloom levels.
@group(0) @binding(1) var mask_tex: texture_2d<f32>;
@group(0) @binding(2) var bloom_samp: sampler;

// The layer texture at target pixel `p`, read bilinearly; empty outside the texture.
fn layer_bilinear (p: vec2f) -> vec4f {
  let t = p - layer.origin.zw - 0.5;
  let i = vec2i (floor (t));
  let f = t - floor (t);
  let size = vec2i (textureDimensions (layer_tex));
  var c = array<vec4f, 4> ();
  for (var k = 0; k < 4; k++) {
    let q = i + vec2i (k & 1, k >> 1);
    if (all (q >= vec2i (0)) && all (q < size)) {
      c[k] = textureLoad (layer_tex, q, 0);
    }
  }
  return mix (mix (c[0], c[1], f.x), mix (c[2], c[3], f.x), f.y);
}
