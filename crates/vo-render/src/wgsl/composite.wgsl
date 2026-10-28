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

// Backdrops (EG-01). The layer block of a backdrop: `origin.xy` is where the surface it is
// taken from (and drawn back into) sits in target pixels, `origin.zw` where the backdrop
// texture's top left sits, `src.xy` that texture's size and `src.z` target pixels per texel
// (1, or 2 at half resolution). `style_a` is the tint, linear and premultiplied.

// Copies the surface, bound as `layer_tex`, into the backdrop texture: each texel is the
// average of the `src.z` by `src.z` target pixels it covers, empty past the surface.
@fragment fn fs_capture (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let k = i32 (layer.src.z);
  let base = vec2i (floor (frag.xy)) * k + vec2i (layer.origin.zw) - vec2i (layer.origin.xy);
  let size = vec2i (textureDimensions (layer_tex));
  var sum = vec4f (0.0);
  for (var y = 0; y < k; y++) {
    for (var x = 0; x < k; x++) {
      let q = base + vec2i (x, y);
      if (all (q >= vec2i (0)) && all (q < size)) {
        sum += textureLoad (layer_tex, q, 0);
      }
    }
  }
  return sum / f32 (k * k);
}

// Draws the backdrop texture, bound as `layer_tex`, back into its surface: read bilinearly
// (a 1:1 copy at full resolution), tinted, with the layer's opacity, cut to the backdrop's
// rounded box and the clip around it.
@fragment fn fs_backdrop (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let k = layer.src.z;
  let t = (frag.xy + layer.origin.xy - layer.origin.zw) / k - 0.5;
  let i = vec2i (floor (t));
  let f = t - floor (t);
  let size = vec2i (textureDimensions (layer_tex));
  var c = array<vec4f, 4> ();
  for (var n = 0; n < 4; n++) {
    let q = clamp (i + vec2i (n & 1, n >> 1), vec2i (0), size - 1);
    c[n] = textureLoad (layer_tex, q, 0);
  }
  let b = mix (mix (c[0], c[1], f.x), mix (c[2], c[3], f.x), f.y);
  return b * layer.style_a * (layer.t.w * clip_coverage (frag.xy));
}
