// Glyph pipeline entry points around the HarfBuzz hb-gpu helpers.
// `slug_gpu::shader_source` prepends hb-gpu-vertex.wgsl, hb-gpu-fragment.wgsl
// and hb-gpu-draw-fragment.wgsl (patched to read the atlas global directly).

struct Uniforms {
  mvp: mat4x4f,
  viewport: vec2f,
  gamma: f32,
  stem_darkening: f32,
  foreground: vec4f,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> hbgpu_atlas_buf: array<vec4<i32>>;

// One instance per glyph. `lin` is the em-to-object 2x2 matrix by columns
// (col0.x, col0.y, col1.x, col1.y); object space is physical pixels, Y down.
struct GlyphInstance {
  @location(0) ext: vec4f,
  @location(1) lin: vec4f,
  @location(2) offset: vec2f,
  @location(3) loc: u32,
};

struct VsOut {
  @builtin(position) clip: vec4f,
  @location(0) tc: vec2f,
  @location(1) @interpolate(flat) loc: u32,
};

@vertex fn vs_glyph (@builtin(vertex_index) vi: u32, g: GlyphInstance) -> VsOut {
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  let em = mix (g.ext.xy, g.ext.zw, c);
  let m = mat2x2f (g.lin.xy, g.lin.zw);
  let pos = m * em + g.offset;
  // hb_gpu_dilate moves the corner by its half-pixel step times this vector,
  // so it must be in pixel units: the sum of the two outward unit edge
  // normals at the corner, which is (+-1, +-1) for an unrotated glyph.
  let s = c * 2.0 - 1.0;
  let normal = normalize (m * vec2f (s.x, 0.0)) + normalize (m * vec2f (0.0, s.y));
  let det = g.lin.x * g.lin.w - g.lin.z * g.lin.y;
  let jac = vec4f (g.lin.w, -g.lin.z, -g.lin.y, g.lin.x) / det;
  let d = hb_gpu_dilate (pos, em, normal, jac, u.mvp, u.viewport);

  var out: VsOut;
  out.clip = u.mvp * vec4f (d[0], 0.0, 1.0);
  out.tc = d[1];
  out.loc = g.loc;
  return out;
}

@fragment fn fs_glyph (in: VsOut) -> @location(0) vec4f {
  let ppem = hb_gpu_ppem (in.tc, in.loc);
  var cov = hb_gpu_draw (in.tc, in.loc);
  if (u.stem_darkening > 0.0) {
    let brightness = dot (u.foreground.rgb, vec3f (1.0 / 3.0)) / max (u.foreground.a, 1e-6);
    cov = hb_gpu_stem_darken (cov, brightness, ppem);
  }
  if (u.gamma != 1.0) {
    cov = pow (cov, u.gamma);
  }
  return u.foreground * cov;
}
