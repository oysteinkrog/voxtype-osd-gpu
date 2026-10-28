// Displacement (EG-03, `displace.rs`): draws a layer's backdrop (EG-01) back into its surface
// with each pixel taken from somewhere else. `common.wgsl` and `noise.wgsl` come first.
//
// The layer block is the backdrop's: `origin.xy` is where the surface drawn into sits in
// target pixels, `origin.zw` where the backdrop texture's top left sits, the clip is the
// backdrop's rounded box and `t.w` the layer's opacity. Group 2 is the displacement block,
// the sampler and the four image slots (`Renderer::set_material_image`).
//
// The draw replaces what the surface holds inside the scissor: the output mixes the pixel as
// captured with the moved one by the clip coverage, the opacity and the kept box, so outside
// the box and inside the kept reading line it writes back exactly what was there.

@group(0) @binding(0) var layer_tex: texture_2d<f32>;

struct Displace {
  // Kind (0 ripples, 1 noise, 2 curl, 3 page curl, 4 texture), strength, ring count, seed.
  head: vec4f,
  // Ripples: wavelength, width (px). Noise and curl: cell size, amount (px), turn.
  // Page curl: a point on the fold line (px), the unit direction the page lifts toward.
  // Texture: amount x, y (px), bias x, y.
  a: vec4f,
  // Page curl: radius (px). Texture: the image slot.
  b: vec4f,
  // The box kept as captured, x0, y0, x1, y1 in target pixels; x1 < x0 for none.
  keep: vec4f,
  // The layer's box in target pixels, x0, y0, x1, y1: uv 0 to 1 for the texture source.
  rect: vec4f,
  // What the backdrop captured, x0, y0, x1, y1 in target pixels: moved pixels read inside it.
  valid: vec4f,
  // Ripple rings: centre x, y, radius, amplitude, in target pixels.
  rings: array<vec4f, 8>,
};

@group(2) @binding(0) var<uniform> dp: Displace;
@group(2) @binding(1) var dp_samp: sampler;
@group(2) @binding(2) var dp_image0: texture_2d<f32>;
@group(2) @binding(3) var dp_image1: texture_2d<f32>;
@group(2) @binding(4) var dp_image2: texture_2d<f32>;
@group(2) @binding(5) var dp_image3: texture_2d<f32>;

@vertex fn vs_displace (@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
  let c = vec2f (f32 ((vi << 1u) & 2u), f32 (vi & 2u));
  return vec4f (c * vec2f (2.0, -2.0) + vec2f (-1.0, 1.0), 0.0, 1.0);
}

// The backdrop texel under target pixel `p`, clamped to the texture.
fn dp_load (q: vec2i) -> vec4f {
  let size = vec2i (textureDimensions (layer_tex));
  return textureLoad (layer_tex, clamp (q, vec2i (0), size - 1), 0);
}

// The backdrop at target pixel `p`, read bilinearly. `inside` false reads clamped edges;
// true reads transparent past the texture.
fn dp_sample (p: vec2f, inside: bool) -> vec4f {
  let t = p - layer.origin.zw - 0.5;
  let i = vec2i (floor (t));
  let f = t - floor (t);
  let size = vec2i (textureDimensions (layer_tex));
  var c = array<vec4f, 4> ();
  for (var n = 0; n < 4; n++) {
    let q = i + vec2i (n & 1, n >> 1);
    if (inside && (any (q < vec2i (0)) || any (q >= size))) {
      c[n] = vec4f (0.0);
    } else {
      c[n] = textureLoad (layer_tex, clamp (q, vec2i (0), size - 1), 0);
    }
  }
  return mix (mix (c[0], c[1], f.x), mix (c[2], c[3], f.x), f.y);
}

fn dp_image (slot: u32, uv: vec2f) -> vec4f {
  switch slot {
    case 0u: { return textureSampleLevel (dp_image0, dp_samp, uv, 0.0); }
    case 1u: { return textureSampleLevel (dp_image1, dp_samp, uv, 0.0); }
    case 2u: { return textureSampleLevel (dp_image2, dp_samp, uv, 0.0); }
    case 3u: { return textureSampleLevel (dp_image3, dp_samp, uv, 0.0); }
    default: { return vec4f (0.0); }
  }
}

// The offset at target pixel `p` for every kind but the page curl, before the strength.
fn dp_offset (p: vec2f, kind: u32) -> vec2f {
  switch kind {
    case 0u: {
      var off = vec2f (0.0);
      let n = u32 (dp.head.z);
      for (var i = 0u; i < n; i++) {
        let r = dp.rings[i];
        let d = p - r.xy;
        let dist = length (d);
        let x = dist - r.z;
        let wave = sin (6.2831853 * x / dp.a.x) * exp (-(x * x) / (dp.a.y * dp.a.y));
        off += d / max (dist, 1e-3) * (r.w * wave);
      }
      return off;
    }
    case 1u: {
      let q = p / dp.a.x;
      let seed = bitcast<u32> (dp.head.w);
      let v = vec2f (noise_simplex2 (q, seed, dp.a.z), noise_simplex2 (q + vec2f (31.7, 11.3), seed + 1u, dp.a.z));
      return v * dp.a.y;
    }
    case 2u: {
      let q = p / dp.a.x;
      let v = noise_curl2 (q, bitcast<u32> (dp.head.w), dp.a.z) * 0.4;
      let len = length (v);
      return select (v, v / len, len > 1.0) * dp.a.y;
    }
    case 4u: {
      let uv = (p - dp.rect.xy) / max (dp.rect.zw - dp.rect.xy, vec2f (1.0));
      let v = dp_image (u32 (dp.b.y), uv).xy;
      return (v - dp.a.zw) * dp.a.xy;
    }
    default: {
      return vec2f (0.0);
    }
  }
}

// A page lifting over a cylinder of radius R: before the fold line it lies flat; within R
// past it the page bends up, its front squeezed and its back (dimmed) rolled over it; past R
// it is gone.
fn dp_page_curl (p: vec2f) -> vec4f {
  let fold = dp.a.xy;
  let dir = dp.a.zw;
  let r = dp.b.x;
  let d = dot (p - fold, dir);
  if (d <= 0.0) {
    return dp_sample (p, true);
  }
  if (d >= r) {
    return vec4f (0.0);
  }
  let arc = r * asin (d / r);
  let front = dp_sample (p + dir * (arc - d), true);
  // The back of the page: its own content, washed pale.
  let b = dp_sample (p + dir * (3.14159265 * r - arc - d), true);
  let back = vec4f (b.rgb * 0.55 + vec3f (0.3 * b.a), b.a);
  return back + front * (1.0 - back.a);
}

@fragment fn fs_displace (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let p = frag.xy + layer.origin.xy;
  let orig = dp_load (vec2i (floor (p - layer.origin.zw)));
  let kind = u32 (dp.head.x);
  var moved: vec4f;
  if (kind == 3u) {
    moved = dp_page_curl (p);
  } else {
    let q = clamp (p + dp_offset (p, kind) * dp.head.y, dp.valid.xy + 0.5, dp.valid.zw - 0.5);
    moved = dp_sample (q, false);
  }
  var w = clip_coverage (frag.xy) * layer.t.w;
  if (dp.keep.z >= dp.keep.x) {
    // Zero inside the kept box, rising to one over 3 px outside it.
    let gap = max (max (dp.keep.xy - p, p - dp.keep.zw), vec2f (0.0));
    w *= smoothstep (0.0, 3.0, length (gap));
  }
  return mix (orig, moved, w);
}
