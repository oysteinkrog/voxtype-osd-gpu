// Composites an offscreen layer texture into its parent target, 1:1 in pixels, with the
// layer's opacity, the parent's clip and the layer's blend mode (set on the pipeline).
// `common.wgsl` is prepended.
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

// A layer with bloom: the layer plus `layer.src.z` times its blurred copy, both premultiplied,
// alpha saturated; in light mode (`layer.src.w`) the copy is a dark shadow under the layer. Only on the effect path, where every texture is linear Rgba16Float.
@group(0) @binding(1) var bloom_tex: texture_2d<f32>;
@group(0) @binding(2) var bloom_samp: sampler;

@fragment fn fs_composite_bloom (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let base = textureLoad (layer_tex, layer_texel (frag.xy), 0);
  let uv = (frag.xy + layer.origin.xy - layer.origin.zw) / layer.src.xy;
  let glow = textureSampleLevel (bloom_tex, bloom_samp, uv, 0.0);
  var c = base + layer.src.z * glow;
  if (layer.src.w != 0.0) {
    // Light mode: the blurred copy is a soft dark shadow under the layer, not added light.
    c = base + (1.0 - base.a) * vec4f (0.0, 0.0, 0.0, min (layer.src.z * glow.a, 0.85));
  }
  return vec4f (c.rgb, min (c.a, 1.0)) * (layer.t.w * clip_coverage (frag.xy));
}
