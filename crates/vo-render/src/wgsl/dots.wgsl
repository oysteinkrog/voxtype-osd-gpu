// Dot pipeline: filled circles with one colour each, many thousands per draw. `common.wgsl` is
// prepended. The CPU side (`dots.rs`) has folded layer opacity into the colour and snapped
// resting dots to the pixel grid.

struct DotIn {
  @location(0) centre: vec2f,
  @location(1) radius: f32,
  // Straight-alpha sRGB bytes, red in the low byte.
  @location(2) color: u32,
  // Ring width in layer units; 0 draws a filled dot.
  @location(3) ring: f32,
};

struct DotOut {
  @builtin(position) clip: vec4f,
  // Position relative to the centre, in physical pixels.
  @location(0) d: vec2f,
  @location(1) @interpolate(flat) radius_px: f32,
  @location(2) @interpolate(flat) color: vec4f,
  @location(3) @interpolate(flat) ring_px: f32,
};

fn srgb_to_linear (c: vec3f) -> vec3f {
  let lo = c / 12.92;
  let hi = pow ((c + 0.055) / 1.055, vec3f (2.4));
  return select (hi, lo, c <= vec3f (0.04045));
}

@vertex fn vs_dot (@builtin(vertex_index) vi: u32, s: DotIn) -> DotOut {
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u)) * 2.0 - 1.0;
  let ppu = layer.t.z;
  let radius_px = s.radius * ppu;
  // The quad reaches one pixel past the edge for the antialiasing ramp.
  let m = s.radius + 1.0 / ppu;
  let p = s.centre + c * m;
  var o: DotOut;
  o.clip = px_to_ndc (layer_to_px (p));
  o.d = c * m * ppu;
  o.radius_px = radius_px;
  o.ring_px = s.ring * ppu;
  let raw = unpack4x8unorm (s.color);
  o.color = vec4f (srgb_to_linear (raw.rgb) * raw.a, raw.a);
  return o;
}

@fragment fn fs_dot (in: DotOut) -> @location(0) vec4f {
  // A dot smaller than a pixel keeps its area as faint coverage instead of vanishing.
  let seen = warp_scale (frag_target (in.clip.xy));
  let r = max (in.radius_px * seen, 0.5);
  let area = min (in.radius_px * seen * in.radius_px * seen / (r * r), 1.0);
  var cov = clamp (r + 0.5 - length (in.d) * seen, 0.0, 1.0) * area;
  // A hollow dot (FX-011) also fades out inside its ring; the ring is at least a pixel wide
  // so it never breaks up.
  if (in.ring_px > 0.0) {
    let inner = r - max (in.ring_px * seen, 1.0);
    cov *= clamp (length (in.d) * seen - inner + 0.5, 0.0, 1.0);
  }
  return finish (in.color * cov * clip_coverage (in.clip.xy));
}
