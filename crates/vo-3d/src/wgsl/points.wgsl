// Point clouds (D302). Every point is evaluated in closed form from its index, the cloud's
// seed and the cloud's `t`, so there is no simulation state. Round sprites, size by depth,
// optional depth of field (FX16), optional reaction to the text field (TY19). Drawn additively,
// depth tested, no depth write. `common3d.wgsl` is prepended.

struct Cloud {
  model: mat4x4f,
  // Linear straight colours; a is the cloud's opacity.
  color: vec4f,
  color2: vec4f,
  // Shape kind (0 torus, 1 ball), major radius or ball radius, minor radius, sprite size.
  shape: vec4f,
  // t, speed, level, level_amp.
  motion: vec4f,
  // reach_px, push_px, glow, min_px.
  react: vec4f,
  // seed, count, defocus.
  ids: vec4u,
};

@group(1) @binding(0) var<uniform> c: Cloud;

struct PointOut {
  @builtin(position) clip: vec4f,
  // Sprite coordinate in pixels from the centre.
  @location(0) offset: vec2f,
  // Disc radius in pixels, edge softness in pixels.
  @location(1) @interpolate(flat) disc: vec2f,
  @location(2) @interpolate(flat) color: vec4f,
};

// A point's position in the cloud's local space.
fn cloud_point (i: u32) -> vec3f {
  let seed = c.ids.x;
  let t = c.motion.x;
  let h0 = rand01 (seed, i, 0u, 0u);
  let h1 = rand01 (seed, i, 1u, 0u);
  let h2 = rand01 (seed, i, 2u, 0u);
  let h3 = rand01 (seed, i, 3u, 0u);
  let h4 = rand01 (seed, i, 4u, 0u);
  let speed = c.motion.y * (0.5 + h1);
  if (c.shape.x < 0.5) {
    let major = c.shape.y;
    let minor = c.shape.z;
    let u = h0 * TAU + t * speed;
    let v = h2 * TAU;
    // sqrt fills the tube's cross-section evenly; the extra power thins the outside.
    let r = minor * pow (h3, 0.7);
    // The level pushes points outward in a wave that travels round the ring.
    let wave = 0.55 + 0.45 * sin (3.0 * u + t * 4.0 + h4 * TAU);
    let lift = c.motion.z * c.motion.w * minor * wave * (0.4 + h4);
    let ring = major + r * cos (v) + lift;
    return vec3f (cos (u) * ring, r * sin (v) * (1.0 + 0.5 * c.motion.z), sin (u) * ring);
  }
  let radius = c.shape.y;
  let z = h0 * 2.0 - 1.0;
  let a = h2 * TAU + t * speed;
  let s = sqrt (max (1.0 - z * z, 0.0));
  let r = radius * pow (h3, 0.6) * (1.0 + c.motion.z * c.motion.w * (0.3 + 0.7 * h4));
  return vec3f (s * cos (a), z, s * sin (a)) * r;
}

@vertex fn vs_point (@builtin(vertex_index) vi: u32, @builtin(instance_index) i: u32) -> PointOut {
  let w = c.model * vec4f (cloud_point (i), 1.0);
  var clip = g.view_proj * w;
  let depth = max (-(g.view * w).z, 1e-3);

  var size = max (c.shape.w * g.viewport.z / depth, c.react.w * g.fog.w);
  var energy = 1.0;
  var soft = 1.0;
  if (c.ids.z != 0u) {
    let blur = coc_px (depth);
    let dia = max (size, blur);
    energy = (size / dia) * (size / dia);
    soft = max (1.0, 0.25 * dia * clamp (blur / max (g.focus.z * g.fog.w, 1e-3), 0.0, 1.0));
    size = dia;
  }

  var glow = 1.0;
  if (c.react.x > 0.0) {
    let f = field_at (clip_to_px (clip));
    let k = 1.0 - smoothstep (0.0, c.react.x * g.fog.w, f.r);
    clip = clip_offset_px (clip, f.gb * (c.react.y * g.fog.w * k));
    glow = 1.0 + c.react.z * k;
  }

  let mixk = rand01 (c.ids.x, i, 5u, 0u);
  let rgb = mix (c.color.rgb, c.color2.rgb, mixk);
  let a = c.color.a * energy * fog_at (depth);

  let radius = 0.5 * size;
  let corner = vec2f (f32 (vi & 1u), f32 (vi >> 1u)) * 2.0 - 1.0;
  let ext = (radius + soft) * corner;
  var out: PointOut;
  out.clip = clip_offset_px (clip, ext);
  out.offset = ext;
  out.disc = vec2f (radius, soft);
  out.color = vec4f (rgb * glow, 1.0) * a;
  return out;
}

@fragment fn fs_point (in: PointOut) -> @location(0) vec4f {
  let r = length (in.offset);
  let cov = clamp ((in.disc.x - r) / in.disc.y + 0.5, 0.0, 1.0);
  return finish (in.color * cov);
}
