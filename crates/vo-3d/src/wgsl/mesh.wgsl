// Lit meshes: one directional light (Lambert plus Blinn-Phong), ambient, a Fresnel rim and an
// emissive term. Depth tested and written. Two-sided: nothing is culled, a back face is lit
// with its normal turned to the viewer and takes the back colour, and fine fibre lines run
// along a ribbon's length from `uv.y` (both off by default: back = color, no fibres). A mirrored draw (the floor reflection, D305) is
// lit as the original seen from the mirrored eye, and fades with depth below the floor.
// `fs_mesh_tex` (EG-14) puts a premultiplied texture over the front face, for text on curved
// surfaces. `common3d.wgsl` is prepended.

struct Draw {
  model: mat4x4f,
  // Inverse transpose of the model's 3x3, in a mat4 for alignment.
  normal_m: mat4x4f,
  // Linear, premultiplied.
  color: vec4f,
  // Emissive, specular, shininess, 1 for a mirrored draw.
  params: vec4f,
  // Back-face colour, linear premultiplied with `color`'s alpha.
  back: vec4f,
  // Fibre lines across the width, their strength.
  fibre: vec4f,
};

@group(1) @binding(0) var<uniform> d: Draw;

struct MeshOut {
  @builtin(position) clip: vec4f,
  @location(0) world: vec3f,
  @location(1) normal: vec3f,
  @location(2) uv: vec2f,
};

@vertex fn vs_mesh (@location(0) pos: vec3f, @location(1) normal: vec3f, @location(2) uv: vec2f) -> MeshOut {
  let w = d.model * vec4f (pos, 1.0);
  var out: MeshOut;
  out.clip = g.view_proj * w;
  out.world = w.xyz;
  out.normal = (d.normal_m * vec4f (normal, 0.0)).xyz;
  out.uv = uv;
  return out;
}

// The surface texture of a textured draw (EG-14): premultiplied, sRGB-encoded bytes as the 2D
// renderer writes them into a plain UNORM target. Only the textured pipeline binds group 2.
@group(2) @binding(0) var surface_tex: texture_2d<f32>;
@group(2) @binding(1) var surface_smp: sampler;

@fragment fn fs_mesh (in: MeshOut, @builtin(front_facing) front: bool) -> @location(0) vec4f {
  // Fibre lines first, while control flow is uniform (derivatives): a line about a pixel
  // wide at each whole step of uv.y times the count, faded out where they get denser than
  // the pixels so they never alias.
  let fx = in.uv.y * d.fibre.x;
  let fw = max (fwidth (fx), 1e-4);
  let off = abs (fract (fx + 0.5) - 0.5);
  let fibre = d.fibre.y * (1.0 - smoothstep (0.0, fw, off)) * (1.0 - smoothstep (0.2, 0.45, fw));
  return shade (in, front, fibre, d.color, d.back);
}

fn srgb_decode (c: vec3f) -> vec3f {
  let lo = c / 12.92;
  let hi = pow ((c + 0.055) / 1.055, vec3f (2.4));
  return select (hi, lo, c <= vec3f (0.04045));
}

// A textured surface: the texture over the paper colour on the front, the back colour alone
// on the back. uv.y runs bottom to top, the texture top to bottom.
@fragment fn fs_mesh_tex (in: MeshOut, @builtin(front_facing) front: bool) -> @location(0) vec4f {
  let s = textureSample (surface_tex, surface_smp, vec2f (in.uv.x, 1.0 - in.uv.y));
  var ink = vec4f (0.0);
  if (s.a > 0.0) {
    ink = vec4f (srgb_decode (clamp (s.rgb / s.a, vec3f (0.0), vec3f (1.0))) * s.a, s.a);
  }
  let face = ink + d.color * (1.0 - ink.a);
  // Clear paper writes no depth, so what lies behind it still draws.
  let mirrored = d.params.w > 0.5;
  let back_face = front == mirrored;
  if (select (face.a, d.back.a, back_face) < 0.002) {
    discard;
  }
  return shade (in, front, 0.0, face, d.back);
}

// Lights one fragment. `front_col` and `back_col` are linear premultiplied.
fn shade (in: MeshOut, front: bool, fibre: f32, front_col: vec4f, back_col: vec4f) -> vec4f {
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
  let back_face = front == mirrored;
  if (back_face) {
    n = -n;
  }
  let v = normalize (eye - in.world);
  let c = select (front_col, back_col, back_face);
  let a = max (c.a, 1e-6);
  let base = c.rgb / a;
  let diffuse = max (dot (n, l), 0.0);
  let h = normalize (l + v);
  let spec = pow (max (dot (n, h), 0.0), d.params.z) * d.params.y * step (0.0, dot (n, l));
  let rim = g.light_dir.w * pow (1.0 - max (dot (n, v), 0.0), 3.0);
  // The rim takes the surface colour, half way to white, so grazing faces do not wash out.
  let lit = base * (g.ambient.rgb + g.light_color.rgb * diffuse)
    + g.light_color.rgb * spec
    + mix (base, vec3f (1.0), 0.5) * g.light_color.rgb * (rim * 0.35)
    + base * d.params.x;
  // Fibres catch the light as a thin sheen.
  let sheen = lit * (1.0 + 0.6 * fibre);
  return finish (vec4f (sheen * c.a, c.a) * k);
}
