// The shadow map's caster pass (EG-18): depth only, from the key light. Not prepended with
// `common3d.wgsl`: group 0 holds just the light's matrix, group 1 the same per-draw slots as
// the mesh pass (only `model` is read).

struct Draw {
  model: mat4x4f,
};

@group(0) @binding(0) var<uniform> light_view_proj: mat4x4f;
@group(1) @binding(0) var<uniform> d: Draw;

@vertex fn vs_caster (@location(0) pos: vec3f) -> @builtin(position) vec4f {
  return light_view_proj * (d.model * vec4f (pos, 1.0));
}
