// The floor (D305): one triangle over the whole target that intersects each pixel's camera ray
// with the plane y = floor height. Where the ray hits it in front of the camera, the pass
// writes the plane's depth and adds the pools of light lying there; elsewhere it resets the
// depth to far. Drawn after the mirrored copy, so the copy shows below the floor line but
// never through content above the floor that comes after. `common3d.wgsl` is prepended.

struct FloorBlock {
  // The camera ray at ndc (x, y): origin ray[0] + x ray[1] + y ray[2], direction
  // ray[3] + x ray[4] + y ray[5] (Camera::ray_basis).
  ray: array<vec4f, 6>,
  // The centre's x and z, then the radius; radius 0 for an unused pool.
  pool_at: array<vec4f, 4>,
  // Linear colour times intensity.
  pool_color: array<vec4f, 4>,
};

@group(1) @binding(0) var<uniform> f: FloorBlock;

@vertex fn vs_floor (@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
  let c = vec2f (f32 ((vi << 1u) & 2u), f32 (vi & 2u));
  return vec4f (c * vec2f (2.0, -2.0) + vec2f (-1.0, 1.0), 1.0, 1.0);
}

struct FloorOut {
  @location(0) color: vec4f,
  @builtin(frag_depth) depth: f32,
};

@fragment fn fs_floor (@builtin(position) frag: vec4f) -> FloorOut {
  let ndc = vec2f (frag.x / g.viewport.x * 2.0 - 1.0, 1.0 - frag.y / g.viewport.y * 2.0);
  let o = f.ray[0].xyz + ndc.x * f.ray[1].xyz + ndc.y * f.ray[2].xyz;
  let dir = f.ray[3].xyz + ndc.x * f.ray[4].xyz + ndc.y * f.ray[5].xyz;
  var out: FloorOut;
  out.color = vec4f (0.0);
  out.depth = 1.0;
  if (abs (dir.y) < 1e-6) {
    return out;
  }
  let t = (g.floor.x - o.y) / dir.y;
  if (t <= 0.0) {
    return out;
  }
  let p = o + dir * t;
  let clip = g.view_proj * vec4f (p, 1.0);
  let z = clip.z / clip.w;
  if (z < 0.0 || z > 1.0) {
    return out;
  }
  out.depth = z;
  var c = vec3f (0.0);
  for (var i = 0u; i < 4u; i++) {
    let at = f.pool_at[i];
    if (at.z > 0.0) {
      let q = (p.xz - at.xy) / at.z;
      c += f.pool_color[i].rgb * exp (-3.0 * dot (q, q));
    }
  }
  c *= fog_at (length (p - g.eye.xyz));
  out.color = finish (vec4f (c, clamp (max (c.r, max (c.g, c.b)), 0.0, 1.0)));
  return out;
}
