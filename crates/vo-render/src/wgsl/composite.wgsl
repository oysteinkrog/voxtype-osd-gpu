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

// A layer with bloom: the layer plus `layer.src.z` times its blurred copy, both premultiplied,
// alpha saturated; in light mode (`layer.src.w`) the copy is a dark shadow under the layer. Only on the effect path, where every texture is linear Rgba16Float.
// Binding 1 is the bloom for `fs_composite_bloom` and the mask for `fs_composite_mask`.
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

// A masked layer (FX04): the layer times its mask at the same target pixel, or one minus the
// mask when inverted. The mask is an R8 texture with its top left at `layer.mask.xy` in
// target pixels; outside it the mask is empty.
@fragment fn fs_composite_mask (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let c = textureLoad (layer_tex, layer_texel (frag.xy), 0);
  let p = vec2i (floor (frag.xy + layer.origin.xy - layer.mask.xy));
  var m = 0.0;
  if (all (p >= vec2i (0)) && all (p < vec2i (textureDimensions (bloom_tex)))) {
    m = textureLoad (bloom_tex, p, 0).r;
  }
  m = select (m, 1.0 - m, layer.mask.z != 0.0);
  return c * (m * layer.t.w * clip_coverage (frag.xy));
}

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

// A styled layer (FX13): CRT curvature, glitch slices and chromatic aberration move where the
// layer is read; scanlines, the vignette and the flicker darken what is read. Darkening
// scales colour, not alpha, so it shows over a transparent desktop too.
@fragment fn fs_composite_style (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let b = layer.style_box;
  let centre = 0.5 * (b.xy + b.zw);
  let half = max (0.5 * (b.zw - b.xy), vec2f (1.0));
  var p = frag.xy + layer.origin.xy;
  // Curvature: read further out the further from the centre, so the content bows out; the
  // corners past the curved edge are empty.
  let k = layer.style_b.x;
  if (k > 0.0) {
    let n = (p - centre) / half;
    let m = n * (1.0 + k * dot (n, n));
    if (any (abs (m) > vec2f (1.0))) {
      return vec4f (0.0);
    }
    p = centre + m * half;
  }
  // Glitch: of twelve slices, some jump sideways; a new pattern each step.
  let g = layer.style_a.y;
  if (g > 0.0) {
    let seed = u32 (layer.style_b.w);
    let slice = u32 (max (floor ((p.y - b.y) / (2.0 * half.y) * 12.0), 0.0));
    if (noise_rand01 (seed, slice, 0u, 0u) < 0.6 * g) {
      p.x += (noise_rand01 (seed, slice, 1u, 0u) - 0.5) * 0.16 * g * 2.0 * half.x;
    }
  }
  // Chromatic aberration: red read from further out, blue from further in.
  let ca = layer.style_a.x;
  var c = layer_bilinear (p);
  if (ca != 0.0) {
    let off = (p - centre) / half * ca;
    let r = layer_bilinear (p - off);
    let bl = layer_bilinear (p + off);
    c = vec4f (r.r, c.g, bl.b, max (c.a, max (r.a, bl.a)));
  }
  var dim = layer.style_b.z;
  let scan = layer.style_a.z;
  if (scan > 0.0) {
    dim *= 1.0 - scan * (0.5 - 0.5 * cos (6.2831853 * (p.y - b.y) / max (layer.style_a.w, 1.0)));
  }
  let v = layer.style_b.y;
  if (v > 0.0) {
    let n = (frag.xy + layer.origin.xy - centre) / half;
    dim *= 1.0 - v * smoothstep (0.3, 2.0, dot (n, n));
  }
  return vec4f (c.rgb * dim, c.a) * (layer.t.w * clip_coverage (frag.xy));
}
