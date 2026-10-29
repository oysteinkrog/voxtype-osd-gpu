// The dye material (EG-12, `vo_scene::fluid`): the fluid's dye texture drawn as ink in water,
// luminous smoke or watercolour. Registered by `fluid_render::install`.
//
// m.p[0]: the layer's box in layer units (x, y, w, h).
// m.p[1]: style (0 ink, 1 smoke, 2 watercolour), dye image slot, light mode (0 or 1), opacity.
// m.p[2]: softening reach in layer units, rim strength, paper grain, density.
// m.p[3]: the clear band (the reading line): top, bottom, fade in layer units. None when
//         bottom <= top.
//
// The dye texture (`Fluid::dye_view`) spans the box: uv 0 to 1. It holds premultiplied linear
// colour with the dye amount in alpha (above 1 where splats pile up). An empty slot reads as no
// dye, so nothing is drawn.

fn fluid_grain (p: vec2f) -> f32 {
  return fract (sin (dot (floor (p), vec2f (12.9898, 78.233))) * 43758.5453);
}

fn mat_fluid (m: Material, i: MatIn) -> vec4f {
  if (i.cov <= 0.0 || i.color.a <= 0.0) {
    return vec4f (0.0);
  }
  let bx = m.p[0];
  let size = max (bx.zw, vec2f (1e-3));
  let uv = clamp ((i.p - bx.xy) / size, vec2f (0.0), vec2f (1.0));
  let slot = u32 (m.p[1].y + 0.5);
  let style = u32 (m.p[1].x + 0.5);
  let light = m.p[1].z > 0.5;
  // The clear band (the reading line): no dye inside it, fading in over m.p[3].z outside.
  let band = m.p[3];
  var keep = 1.0;
  if (band.y > band.x) {
    let out = max (band.x - i.p.y, i.p.y - band.y);
    if (out <= 0.0) {
      return vec4f (0.0);
    }
    keep = smoothstep (0.0, max (band.z, 1e-3), out);
  }
  let op = m.p[1].w * i.color.a * i.cov * keep;

  // Four taps a softening reach away, for soft edges and the rim's slope.
  let r = vec2f (m.p[2].x) / size;
  let c0 = tex_image (slot, uv);
  let cl = tex_image (slot, uv - vec2f (r.x, 0.0));
  let cr = tex_image (slot, uv + vec2f (r.x, 0.0));
  let cu = tex_image (slot, uv - vec2f (0.0, r.y));
  let cd = tex_image (slot, uv + vec2f (0.0, r.y));
  let soft = (2.0 * c0 + cl + cr + cu + cd) / 6.0;
  let dye = select (soft, mix (c0, soft, 0.5), style == 0u);
  let a = max (dye.a, 0.0);
  if (a < 1e-4) {
    return vec4f (0.0);
  }
  let col = clamp (max (dye.rgb, vec3f (0.0)) / a, vec3f (0.0), vec3f (1.0));
  // On light paper the eye needs more ink to see it than glowing dye on dark.
  var d = 1.0 - exp (-m.p[2].w * select (1.0, 3.0, light) * a);

  if (style == 1u) {
    // Smoke: light added (little alpha), so the colour glows and a bloom picks it up. On
    // light paper it is a soft grey-tinted shadow instead.
    if (light) {
      return vec4f (col * 0.3 * d, d) * 0.6 * op;
    }
    return vec4f (col * 1.4 * d, 0.15 * d) * op;
  }
  if (style == 2u) {
    // Watercolour: pigment pools at the rim, where the amount falls off fastest, and the
    // paper's grain breaks up the wash a little.
    let grad = vec2f (max (cr.a, 0.0) - max (cl.a, 0.0), max (cd.a, 0.0) - max (cu.a, 0.0)) * 0.5;
    let rim = smoothstep (0.02, 0.2, length (grad)) * m.p[2].y;
    let grain = 1.0 + m.p[2].z * (fluid_grain (i.p * 0.8) - 0.5) * 2.0;
    d = clamp (d * grain * (0.85 + 0.4 * rim), 0.0, 1.0);
    let k = select (1.0, 0.4, light);
    return vec4f (col * k * (1.0 - 0.45 * rim) * d, d) * 0.9 * op;
  }
  // Ink: dense colour painted over; dark ink on light paper.
  let k = select (1.0, 0.25, light);
  return vec4f (col * k * d, d) * 0.92 * op;
}
