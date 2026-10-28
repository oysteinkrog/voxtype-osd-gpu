// Gaussian blur (EG-04, `blur.rs`): one direction per pass, two passes for a 2D blur.
// `common.wgsl` comes first, for the layer block and its clip (frost only).
//
// Each pass reads `src` with a bilinear sampler, clamped at its edges, and pairs neighbouring
// taps into one fetch (the linear-sampling trick), so a reach of n texels takes about n / 2
// fetches per side. The weights are normalised in the shader, so the kernel sums to 1.
//
// `fs_blur` writes the blurred texel and leaves the kept box alone (it discards there; the
// pass loads what the target held). `fs_frost` is the frost's second pass: it writes into the
// surface the backdrop was taken from, mixing the sharp capture with the blurred one by the
// backdrop's clip coverage and opacity, so outside the rounded box the surface is unchanged.

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var sharp: texture_2d<f32>;
@group(0) @binding(2) var blur_samp: sampler;

struct Blur {
  // Direction (a unit axis), standard deviation in texels, reach in texels.
  head: vec4f,
  // Where `src`'s top left sits in target pixels (xy), and the texture drawn into (zw).
  // Unused by `fs_frost`, which takes both from the layer block.
  at: vec4f,
  // The box kept as it was, x0, y0, x1, y1 in target pixels; x1 < x0 for none.
  keep: vec4f,
  pad: vec4f,
};

@group(2) @binding(0) var<uniform> bl: Blur;

@vertex fn vs_blur (@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
  let c = vec2f (f32 ((vi << 1u) & 2u), f32 (vi & 2u));
  return vec4f (c * vec2f (2.0, -2.0) + vec2f (-1.0, 1.0), 0.0, 1.0);
}

fn gauss (x: f32, sigma: f32) -> f32 {
  return exp (-0.5 * x * x / (sigma * sigma));
}

// The blur of `src` along the pass's direction at `p`, in texels of `src` (texel centres at
// half integers).
fn blur_at (p: vec2f) -> vec4f {
  let size = vec2f (textureDimensions (src));
  let dir = bl.head.xy;
  let sigma = max (bl.head.z, 1e-3);
  let reach = i32 (bl.head.w);
  var acc = textureSampleLevel (src, blur_samp, p / size, 0.0);
  var sum = 1.0;
  for (var i = 1; i <= reach; i += 2) {
    let w1 = gauss (f32 (i), sigma);
    let w2 = select (0.0, gauss (f32 (i + 1), sigma), i + 1 <= reach);
    let w = w1 + w2;
    let o = (f32 (i) * w1 + f32 (i + 1) * w2) / w;
    let a = textureSampleLevel (src, blur_samp, (p + dir * o) / size, 0.0);
    let b = textureSampleLevel (src, blur_samp, (p - dir * o) / size, 0.0);
    acc += (a + b) * w;
    sum += 2.0 * w;
  }
  return acc / sum;
}

@fragment fn fs_blur (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let target_px = frag.xy + bl.at.zw;
  if (all (target_px >= bl.keep.xy) && all (target_px < bl.keep.zw)) {
    discard;
  }
  return blur_at (target_px - bl.at.xy);
}

@fragment fn fs_frost (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  // Backdrop texels: the surface's pixel moved by where each texture sits.
  let p = frag.xy + layer.origin.xy - layer.origin.zw;
  let size = vec2i (textureDimensions (sharp));
  let was = textureLoad (sharp, clamp (vec2i (floor (p)), vec2i (0), size - 1), 0);
  let k = clamp (clip_coverage (frag.xy) * layer.t.w, 0.0, 1.0);
  return mix (was, blur_at (p), k);
}
