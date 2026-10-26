// Dot pipeline: filled circles with one colour each, many thousands per draw. `common.wgsl` is
// prepended. The CPU side (`dots.rs`) has folded layer opacity into the colour and snapped
// resting dots to the pixel grid.

struct DotIn {
  @location(0) centre: vec2f,
  @location(1) radius: f32,
  // Straight-alpha sRGB bytes, red in the low byte.
  @location(2) color: u32,
};

struct DotOut {
  @builtin(position) clip: vec4f,
  // Position relative to the centre, in physical pixels.
  @location(0) d: vec2f,
  @location(1) @interpolate(flat) radius_px: f32,
  @location(2) @interpolate(flat) color: vec4f,
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
  let raw = unpack4x8unorm (s.color);
  o.color = vec4f (srgb_to_linear (raw.rgb) * raw.a, raw.a);
  return o;
}

@fragment fn fs_dot (in: DotOut) -> @location(0) vec4f {
  // A dot smaller than a pixel keeps its area as faint coverage instead of vanishing.
  let r = max (in.radius_px, 0.5);
  let area = min (in.radius_px * in.radius_px / (r * r), 1.0);
  let cov = clamp (r + 0.5 - length (in.d), 0.0, 1.0) * area;
  return finish (in.color * cov * clip_coverage (in.clip.xy));
}
