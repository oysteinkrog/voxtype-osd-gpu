// Lit meshes: one directional light (Lambert plus Blinn-Phong), ambient, a Fresnel rim and an
// emissive term. Depth tested and written. A mirrored draw (the floor reflection, D305) is
// lit as the original seen from the mirrored eye, and fades with depth below the floor.
// `common3d.wgsl` is prepended.

struct Draw {
  model: mat4x4f,
  // Inverse transpose of the model's 3x3, in a mat4 for alignment.
  normal_m: mat4x4f,
  // Linear, premultiplied.
  color: vec4f,
  // Emissive, specular, shininess, 1 for a mirrored draw.
  params: vec4f,
};

@group(1) @binding(0) var<uniform> d: Draw;

struct MeshOut {
  @builtin(position) clip: vec4f,
  @location(0) world: vec3f,
  @location(1) normal: vec3f,
};

@vertex fn vs_mesh (@location(0) pos: vec3f, @location(1) normal: vec3f) -> MeshOut {
  let w = d.model * vec4f (pos, 1.0);
  var out: MeshOut;
  out.clip = g.view_proj * w;
  out.world = w.xyz;
  out.normal = (d.normal_m * vec4f (normal, 0.0)).xyz;
  return out;
}

@fragment fn fs_mesh (in: MeshOut, @builtin(front_facing) front: bool) -> @location(0) vec4f {
  let mirrored = d.params.w > 0.5;
  var k = 1.0;
  var l = g.light_dir.xyz;
  var eye = g.eye.xyz;
  if (mirrored) {
    k = reflect_at (in.world.y);
    if (k <= 0.0) {
      discard;
    }
    l.y = -l.y;
    eye.y = 2.0 * g.floor.x - eye.y;
  }
  // Mirroring flips the winding, so a mirrored face toward the camera reads as a back face.
  var n = normalize (in.normal);
  if (front == mirrored) {
    n = -n;
  }
  let v = normalize (eye - in.world);
  let a = max (d.color.a, 1e-6);
  let base = d.color.rgb / a;
  let diffuse = max (dot (n, l), 0.0);
  let h = normalize (l + v);
  let spec = pow (max (dot (n, h), 0.0), d.params.z) * d.params.y * step (0.0, dot (n, l));
  let rim = g.light_dir.w * pow (1.0 - max (dot (n, v), 0.0), 3.0);
  // The rim takes the surface colour, half way to white, so grazing faces do not wash out.
  let lit = base * (g.ambient.rgb + g.light_color.rgb * diffuse)
    + g.light_color.rgb * spec
    + mix (base, vec3f (1.0), 0.5) * g.light_color.rgb * (rim * 0.35)
    + base * d.params.x;
  return finish (vec4f (lit * d.color.a, d.color.a) * k);
}
