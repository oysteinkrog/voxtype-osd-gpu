// The water material (EG-10, `vo_scene::water`): dark water with highlights and a wobbling
// reflection of what the popup drew above the pool. Registered by `water_render::install`.
//
// m.p[0]: the pool box in layer units (x, y, w, h).
// m.p[1]: reflection strength at the top edge, squash, bend in layer units, image slot.
// m.p[2]: edge line strength, specular strength, shininess, light mode (0 or 1).
// m.p[3]: highlight colour (a colour slot).
//
// The normal texture (`Water::normal_view`) in the image slot spans the pool: uv 0 to 1. A
// flat or empty slot reads as a normal facing the viewer, so the water is still.

fn mat_water (m: Material, i: MatIn) -> vec4f {
  if (i.cov <= 0.0 || i.color.a <= 0.0) {
    return vec4f (0.0);
  }
  let pool = m.p[0];
  let uv = clamp ((i.p - pool.xy) / max (pool.zw, vec2f (1e-3)), vec2f (0.0), vec2f (1.0));
  let raw = tex_image (u32 (m.p[1].w), uv).xyz;
  let n = normalize (select (raw, vec3f (0.0, 0.0, 1.0), dot (raw, raw) < 1e-6));
  let depth = max (i.p.y - pool.y, 0.0);
  let d01 = clamp (depth / max (pool.w, 1e-3), 0.0, 1.0);

  // The mirror: a point `depth` below the edge shows what is `depth * squash` above it, moved
  // along the wave's slope. Never above the layer's top.
  let src_y = max (pool.y - depth * m.p[1].y, 0.5);
  let off = vec2f (0.0, (src_y - i.p.y) * i.ppu) + n.xy * m.p[1].z * i.ppu;
  let refl = tex_backdrop (i.px + off);

  // Fresnel-style: the reflection is strongest at the far (top) edge, where the eye meets
  // the water at a grazing angle, and where a wave tilts the surface away from the eye.
  let cos_v = clamp (mix (0.15, 0.75, d01) + (n.z - 1.0) * 2.0, 0.0, 1.0);
  let fres = (1.0 - cos_v) * (1.0 - cos_v) / 0.7225;
  let k = clamp (m.p[1].x * fres, 0.0, 1.0);
  var c = mix (i.color, refl * i.color.a, k);

  // A thin bright line along the edge, and specular glints from a light up and to the left.
  let edge = m.p[2].x * exp (-depth * i.ppu / 1.5);
  let h = normalize (vec3f (-0.35, -0.6, 0.72) + vec3f (0.0, 0.0, 1.0));
  let spec = m.p[2].y * pow (max (dot (n, h), 0.0), m.p[2].z);
  let hl = mat_lab (m.p[3]);
  // Light mode: softer glints, since a pale tint has little room above it.
  let glint = select (1.0, 0.6, m.p[2].w > 0.5);
  c = vec4f (c.rgb + hl.rgb * hl.a * (edge + spec * glint) * i.color.a, c.a);
  return c * i.cov;
}
