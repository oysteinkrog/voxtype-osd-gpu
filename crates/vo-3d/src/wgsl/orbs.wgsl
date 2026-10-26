// Bokeh orbs (FX16): a disc per light whose diameter is the larger of its own projected size
// and the blur disc for its depth. Brightness goes with the area ratio, so a defocused orb
// spreads the same light over more pixels. A faint bright rim and a soft edge make it read as
// a lens highlight. Additive, depth tested, no depth write. `common3d.wgsl` is prepended.

struct OrbIn {
  // xyz position, w radius in world units.
  @location(0) pos: vec4f,
  // Linear, premultiplied, times intensity.
  @location(1) color: vec4f,
};

struct OrbOut {
  @builtin(position) clip: vec4f,
  @location(0) offset: vec2f,
  // Disc radius in pixels, edge softness in pixels, rim strength.
  @location(1) @interpolate(flat) disc: vec3f,
  @location(2) @interpolate(flat) color: vec4f,
};

@vertex fn vs_orb (@builtin(vertex_index) vi: u32, o: OrbIn) -> OrbOut {
  let w = vec4f (o.pos.xyz, 1.0);
  let clip = g.view_proj * w;
  let depth = max (-(g.view * w).z, 1e-3);
  let own = max (2.0 * o.pos.w * g.viewport.z / depth, 2.0 * g.fog.w);
  let blur = coc_px (depth);
  let dia = max (own, blur);
  let defocus = clamp ((dia - own) / max (dia, 1e-3), 0.0, 1.0);
  // Never let a tiny light vanish entirely: count at least a 6 px core.
  let k = max (own, 6.0 * g.fog.w) / dia;
  let energy = min (k * k, 1.0);
  let radius = 0.5 * dia;
  let soft = max (1.0, radius * mix (0.5, 0.12, defocus));
  let corner = vec2f (f32 (vi & 1u), f32 (vi >> 1u)) * 2.0 - 1.0;
  let ext = (radius + soft) * corner;
  var out: OrbOut;
  out.clip = clip_offset_px (clip, ext);
  out.offset = ext;
  out.disc = vec3f (radius, soft, defocus);
  out.color = o.color * (energy * fog_at (depth));
  return out;
}

@fragment fn fs_orb (in: OrbOut) -> @location(0) vec4f {
  let r = length (in.offset);
  let edge = clamp ((in.disc.x - r) / in.disc.y + 0.5, 0.0, 1.0);
  // In focus: a soft gaussian-like blob. Out of focus: a flat disc with a brighter rim.
  let q = r / max (in.disc.x, 1e-3);
  let blob = exp (-3.0 * q * q);
  let disc = 0.8 + 0.35 * smoothstep (0.55, 0.95, q);
  let shape = mix (blob, disc, in.disc.z);
  return finish (in.color * (edge * shape));
}
