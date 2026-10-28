// Composites an offscreen layer that has bloom, a style or a mask, in one fixed order:
//
//   1. content: the layer texture (with its trail already in it), read where the style's
//      curvature and glitch move the read point;
//   2. plus bloom: up to three bands, each a pyramid level read at the same moved point,
//      with its own weight, saturation and tint (bloom v2), or the one-radius bloom (v1);
//   3. style darkening: scanlines, vignette and flicker scale content and glow together;
//   4. mask: by the mask texture at the unmoved target pixel;
//   5. opacity and the clips above the layer;
//   6. the blend mode, set on the pipeline.
//
// `common.wgsl`, `noise.wgsl` and `composite.wgsl` come first. Everything is premultiplied.

struct Compose {
  // x: bloom mode (0 none, 1 one radius, 2 bands); y: 1 with a style; z: 1 with a mask;
  // w: 1 in light mode.
  flags: vec4f,
  // Per band: tint (linear, straight) and how far the band moves toward it.
  tint0: vec4f,
  tint1: vec4f,
  tint2: vec4f,
  // Per band: weight (bloom strength included), saturation, the mix from the lower level
  // to the higher one, unused.
  band0: vec4f,
  band1: vec4f,
  band2: vec4f,
};

@group(0) @binding(3) var lo0: texture_2d<f32>;
@group(0) @binding(4) var hi0: texture_2d<f32>;
@group(0) @binding(5) var lo1: texture_2d<f32>;
@group(0) @binding(6) var hi1: texture_2d<f32>;
@group(0) @binding(7) var lo2: texture_2d<f32>;
@group(0) @binding(8) var hi2: texture_2d<f32>;
@group(0) @binding(9) var<uniform> cmp: Compose;

// A smooth read of a small pyramid level: cubic B-spline from four bilinear taps, so a level
// many times smaller than the layer shows no blocks or diamonds when it is enlarged.
fn bspline (t: texture_2d<f32>, uv: vec2f) -> vec4f {
  let size = vec2f (textureDimensions (t));
  let p = uv * size - 0.5;
  let i = floor (p);
  let f = p - i;
  let f2 = f * f;
  let f3 = f2 * f;
  let w0 = (1.0 - 3.0 * f + 3.0 * f2 - f3) / 6.0;
  let w1 = (4.0 - 6.0 * f2 + 3.0 * f3) / 6.0;
  let w2 = (1.0 + 3.0 * f + 3.0 * f2 - 3.0 * f3) / 6.0;
  let w3 = f3 / 6.0;
  let g0 = w0 + w1;
  let g1 = w2 + w3;
  let h0 = (i - 0.5 + w1 / g0) / size;
  let h1 = (i + 1.5 + w3 / g1) / size;
  return (textureSampleLevel (t, bloom_samp, vec2f (h0.x, h0.y), 0.0) * g0.x
        + textureSampleLevel (t, bloom_samp, vec2f (h1.x, h0.y), 0.0) * g1.x) * g0.y
       + (textureSampleLevel (t, bloom_samp, vec2f (h0.x, h1.y), 0.0) * g0.x
        + textureSampleLevel (t, bloom_samp, vec2f (h1.x, h1.y), 0.0) * g1.x) * g1.y;
}

// One band's light: its two levels mixed, saturated, then tinted, keeping its brightness.
fn band (lo: texture_2d<f32>, hi: texture_2d<f32>, uv: vec2f, tint: vec4f, par: vec4f) -> vec4f {
  if (par.x <= 0.0) {
    return vec4f (0.0);
  }
  var g = bspline (lo, uv);
  if (par.z > 0.0) {
    g = mix (g, bspline (hi, uv), par.z);
  }
  let lum = dot (g.rgb, vec3f (0.2126, 0.7152, 0.0722));
  var rgb = max (vec3f (lum) + (g.rgb - vec3f (lum)) * par.y, vec3f (0.0));
  if (tint.w > 0.0) {
    let tl = max (dot (tint.rgb, vec3f (0.2126, 0.7152, 0.0722)), 1e-4);
    rgb = mix (rgb, tint.rgb * (lum / tl), tint.w);
  }
  return vec4f (rgb, g.a) * par.x;
}

// The glow at layer uv.
fn glow (uv: vec2f) -> vec4f {
  if (cmp.flags.x == 1.0) {
    return textureSampleLevel (lo0, bloom_samp, uv, 0.0) * cmp.band0.x;
  }
  return band (lo0, hi0, uv, cmp.tint0, cmp.band0)
       + band (lo1, hi1, uv, cmp.tint1, cmp.band1)
       + band (lo2, hi2, uv, cmp.tint2, cmp.band2);
}

// Where the style reads the layer for target pixel `p`, or a negative x for "nothing here"
// (outside a CRT curve).
fn style_point (p0: vec2f) -> vec2f {
  let b = layer.style_box;
  let centre = 0.5 * (b.xy + b.zw);
  let half = max (0.5 * (b.zw - b.xy), vec2f (1.0));
  var p = p0;
  // Curvature: read further out the further from the centre, so the content bows out; the
  // corners past the curved edge are empty.
  let k = layer.style_b.x;
  if (k > 0.0) {
    let n = (p - centre) / half;
    let m = n * (1.0 + k * dot (n, n));
    if (any (abs (m) > vec2f (1.0))) {
      return vec2f (-1e9);
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
  return p;
}

// The style's darkening at target pixel `p0` (unmoved) and read point `p`.
fn style_dim (p0: vec2f, p: vec2f) -> f32 {
  let b = layer.style_box;
  let centre = 0.5 * (b.xy + b.zw);
  let half = max (0.5 * (b.zw - b.xy), vec2f (1.0));
  var dim = layer.style_b.z;
  let scan = layer.style_a.z;
  if (scan > 0.0) {
    dim *= 1.0 - scan * (0.5 - 0.5 * cos (6.2831853 * (p.y - b.y) / max (layer.style_a.w, 1.0)));
  }
  let v = layer.style_b.y;
  if (v > 0.0) {
    let n = (p0 - centre) / half;
    dim *= 1.0 - v * smoothstep (0.3, 2.0, dot (n, n));
  }
  return dim;
}

@fragment fn fs_compose (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let p0 = frag.xy + layer.origin.xy;
  var p = p0;
  var base: vec4f;
  if (cmp.flags.y != 0.0) {
    p = style_point (p0);
    if (p.x < -1e8) {
      return vec4f (0.0);
    }
    base = layer_bilinear (p);
    // Chromatic aberration: red read from further out, blue from further in.
    let ca = layer.style_a.x;
    if (ca != 0.0) {
      let b = layer.style_box;
      let off = (p - 0.5 * (b.xy + b.zw)) / max (0.5 * (b.zw - b.xy), vec2f (1.0)) * ca;
      let r = layer_bilinear (p - off);
      let bl = layer_bilinear (p + off);
      base = vec4f (r.r, base.g, bl.b, max (base.a, max (r.a, bl.a)));
    }
  } else {
    base = textureLoad (layer_tex, layer_texel (frag.xy), 0);
  }
  var c = base;
  if (cmp.flags.x != 0.0) {
    let g = glow ((p - layer.origin.zw) / layer.src.xy);
    if (cmp.flags.w != 0.0) {
      // Light mode: the glow is a soft dark shadow under the layer, not added light.
      c = base + (1.0 - base.a) * vec4f (0.0, 0.0, 0.0, min (g.a, 0.85));
    } else {
      c = base + g;
    }
  }
  if (cmp.flags.y != 0.0) {
    c = vec4f (c.rgb * style_dim (p0, p), c.a);
  }
  c = vec4f (c.rgb, min (c.a, 1.0));
  if (cmp.flags.z != 0.0) {
    let q = vec2i (floor (p0 - layer.mask.xy));
    var m = 0.0;
    if (all (q >= vec2i (0)) && all (q < vec2i (textureDimensions (mask_tex)))) {
      m = textureLoad (mask_tex, q, 0).r;
    }
    c *= select (m, 1.0 - m, layer.mask.z != 0.0);
  }
  return c * (layer.t.w * clip_coverage (frag.xy));
}
