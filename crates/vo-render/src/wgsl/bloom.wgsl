// Bloom pyramid (Jimenez, "Next generation post processing in Call of Duty: Advanced
// Warfare", SIGGRAPH 2014): a 13-tap downsample per level, with a Karis average on the first
// one against fireflies, then a 3x3 tent upsample that lerps each level with the one below.
// Everything stays premultiplied and linear; nothing is unpremultiplied, so transparent
// texels add no dark fringe. Also the effect path's last pass: tone map, encode, dither.

struct Fx {
  // One texel of the source texture in uv.
  texel: vec2f,
  // Upsample: how much of the lower level goes into the mix.
  weight: f32,
  // Downsample: 1 for the Karis average. Final pass: the output mode.
  mode: f32,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var low: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(1) @binding(0) var<uniform> fx: Fx;

struct FullOut {
  @builtin(position) pos: vec4f,
  @location(0) uv: vec2f,
};

@vertex fn vs_full (@builtin(vertex_index) vi: u32) -> FullOut {
  let c = vec2f (f32 ((vi << 1u) & 2u), f32 (vi & 2u));
  var o: FullOut;
  o.pos = vec4f (c * vec2f (2.0, -2.0) + vec2f (-1.0, 1.0), 0.0, 1.0);
  o.uv = c;
  return o;
}

fn tap (uv: vec2f, dx: f32, dy: f32) -> vec4f {
  return textureSampleLevel (src, samp, uv + vec2f (dx, dy) * fx.texel, 0.0);
}

// Karis weight of a 4-tap box: 1 / (1 + luma), on premultiplied values.
fn karis (c: vec4f) -> f32 {
  return 1.0 / (1.0 + dot (c.rgb, vec3f (0.2126, 0.7152, 0.0722)));
}

@fragment fn fs_down (in: FullOut) -> @location(0) vec4f {
  let a = tap (in.uv, -2.0, -2.0);
  let b = tap (in.uv, 0.0, -2.0);
  let c = tap (in.uv, 2.0, -2.0);
  let d = tap (in.uv, -1.0, -1.0);
  let e = tap (in.uv, 1.0, -1.0);
  let f = tap (in.uv, -2.0, 0.0);
  let g = tap (in.uv, 0.0, 0.0);
  let h = tap (in.uv, 2.0, 0.0);
  let i = tap (in.uv, -1.0, 1.0);
  let j = tap (in.uv, 1.0, 1.0);
  let k = tap (in.uv, -2.0, 2.0);
  let l = tap (in.uv, 0.0, 2.0);
  let m = tap (in.uv, 2.0, 2.0);
  // Five overlapping 4-tap boxes: the centre one weighted 0.5, the corner ones 0.125.
  let b0 = (d + e + i + j) * 0.25;
  let b1 = (a + b + f + g) * 0.25;
  let b2 = (b + c + g + h) * 0.25;
  let b3 = (f + g + k + l) * 0.25;
  let b4 = (g + h + l + m) * 0.25;
  if (fx.mode == 0.0) {
    return b0 * 0.5 + (b1 + b2 + b3 + b4) * 0.125;
  }
  let w0 = 0.5 * karis (b0);
  let w1 = 0.125 * karis (b1);
  let w2 = 0.125 * karis (b2);
  let w3 = 0.125 * karis (b3);
  let w4 = 0.125 * karis (b4);
  return (b0 * w0 + b1 * w1 + b2 * w2 + b3 * w3 + b4 * w4) / (w0 + w1 + w2 + w3 + w4);
}

// `src` is this level's downsample, `low` the next level's upsample, read with a 3x3 tent.
@fragment fn fs_up (in: FullOut) -> @location(0) vec4f {
  let t = fx.texel;
  var s = textureSampleLevel (low, samp, in.uv, 0.0) * 4.0;
  s += (textureSampleLevel (low, samp, in.uv + vec2f (-t.x, 0.0), 0.0)
      + textureSampleLevel (low, samp, in.uv + vec2f (t.x, 0.0), 0.0)
      + textureSampleLevel (low, samp, in.uv + vec2f (0.0, -t.y), 0.0)
      + textureSampleLevel (low, samp, in.uv + vec2f (0.0, t.y), 0.0)) * 2.0;
  s += textureSampleLevel (low, samp, in.uv + vec2f (-t.x, -t.y), 0.0)
      + textureSampleLevel (low, samp, in.uv + vec2f (t.x, -t.y), 0.0)
      + textureSampleLevel (low, samp, in.uv + vec2f (-t.x, t.y), 0.0)
      + textureSampleLevel (low, samp, in.uv + vec2f (t.x, t.y), 0.0);
  let here = textureSampleLevel (src, samp, in.uv, 0.0);
  return mix (here, s / 16.0, fx.weight);
}

fn srgb_encode3 (c: vec3f) -> vec3f {
  let lo = c * 12.92;
  let hi = 1.055 * pow (max (c, vec3f (0.0)), vec3f (1.0 / 2.4)) - 0.055;
  return select (hi, lo, c <= vec3f (0.0031308));
}

fn srgb_decode3 (c: vec3f) -> vec3f {
  let lo = c / 12.92;
  let hi = pow ((max (c, vec3f (0.0)) + 0.055) / 1.055, vec3f (2.4));
  return select (hi, lo, c <= vec3f (0.04045));
}

// Interleaved gradient noise in [0, 1), from the pixel position only, so it is the same in
// every frame and every run.
fn ign (p: vec2f) -> f32 {
  return fract (52.9829189 * fract (dot (p, vec2f (0.06711056, 0.00583715))));
}

// The effect path's last pass: the linear scene to the target. A shoulder above 0.95 of the
// brightest straight channel brings light above 1 back under 1 with its hue kept, then sRGB
// encode and a dither of one code value. Modes: 0 float target (no encode, no dither),
// 1 sRGB target (the hardware encodes, so the dither is applied in encoded space and decoded
// back), 2 plain UNORM target (the shader encodes).
@fragment fn fs_final (in: FullOut) -> @location(0) vec4f {
  var c = textureLoad (src, vec2i (in.pos.xy), 0);
  if (c.a <= 0.0) {
    return vec4f (0.0);
  }
  c.a = min (c.a, 1.0);
  let k = 0.95;
  let m = max (max (c.r, c.g), c.b) / max (c.a, 1e-4);
  if (m > k) {
    let s = (k + (1.0 - k) * (1.0 - exp (-(m - k) / (1.0 - k)))) / m;
    c = vec4f (c.rgb * s, c.a);
  }
  if (fx.mode == 0.0) {
    return c;
  }
  let n = (ign (in.pos.xy) - 0.5) / 255.0;
  if (fx.mode == 1.0) {
    let e = srgb_encode3 (c.rgb) + n;
    return vec4f (min (srgb_decode3 (clamp (e, vec3f (0.0), vec3f (1.0))), vec3f (c.a)), c.a);
  }
  let straight = clamp (c.rgb / c.a, vec3f (0.0), vec3f (1.0));
  return vec4f (clamp (srgb_encode3 (straight) * c.a + n, vec3f (0.0), vec3f (c.a)), c.a);
}

// A feedback trail's fade: the blend keeps the blend constant times what is there.
@fragment fn fs_fade (in: FullOut) -> @location(0) vec4f {
  return vec4f (0.0);
}
