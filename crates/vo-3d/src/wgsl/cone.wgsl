// Light cones (D305): an open cone from the apex along the axis, built from the vertex index
// as one triangle strip. Seen through, a volume of light is thickest where the surface faces
// the camera, so brightness goes with |n . v|, and it fades toward the far end. Both sides
// are drawn. Additive, depth tested, no depth write. `common3d.wgsl` is prepended.

struct Cone {
  // xyz apex, w length.
  apex: vec4f,
  // xyz unit axis, w tangent of the half angle.
  axis: vec4f,
  // Linear colour times intensity.
  color: vec4f,
};

@group(1) @binding(0) var<uniform> k: Cone;

// Segments around the cone; the strip has 2 * (SEGMENTS + 1) vertices.
const SEGMENTS: u32 = 48u;

struct ConeOut {
  @builtin(position) clip: vec4f,
  @location(0) world: vec3f,
  @location(1) normal: vec3f,
  // 0 at the apex, 1 at the far end.
  @location(2) along: f32,
};

@vertex fn vs_cone (@builtin(vertex_index) vi: u32) -> ConeOut {
  let axis = k.axis.xyz;
  var helper = vec3f (0.0, 1.0, 0.0);
  if (abs (axis.y) > 0.99) {
    helper = vec3f (1.0, 0.0, 0.0);
  }
  let u = normalize (cross (axis, helper));
  let w = cross (axis, u);
  let a = f32 (vi / 2u) / f32 (SEGMENTS) * TAU;
  let radial = cos (a) * u + sin (a) * w;
  let along = f32 (vi & 1u);
  let len = k.apex.w;
  let p = k.apex.xyz + (axis + radial * k.axis.w) * (len * along);
  var out: ConeOut;
  out.clip = g.view_proj * vec4f (p, 1.0);
  out.world = p;
  out.normal = normalize (radial - axis * k.axis.w);
  out.along = along;
  return out;
}

@fragment fn fs_cone (in: ConeOut) -> @location(0) vec4f {
  let v = normalize (g.eye.xyz - in.world);
  let facing = abs (dot (normalize (in.normal), v));
  let fade = pow (1.0 - in.along, 1.5) * smoothstep (0.0, 0.08, in.along);
  let c = k.color.rgb * (0.5 * facing * facing * fade * fog_at (length (g.eye.xyz - in.world)));
  return finish (vec4f (c, clamp (max (c.r, max (c.g, c.b)), 0.0, 1.0)));
}
