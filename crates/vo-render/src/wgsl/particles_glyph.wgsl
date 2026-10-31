// Glyph particles (EG-27): `vs_glyph` and `fs_glyph`, after the HarfBuzz helpers
// (`slug_gpu::hb_source`), `particles.wgsl` and `particles_draw.wgsl`. Each free particle of an
// emitter with the glyph style is one Slug glyph, the quad and its coverage worked out as the
// glyph pipeline works them out for text (`slug-glyph.wgsl`), so a letter at rest is the same
// pixels as the letter drawn as text.

// One letter an emitter can show: its ink box in font units, where the atlas holds it, and
// the font's units per em.
struct GlyphRec {
  ext: vec4f,
  loc: u32,
  upem: f32,
  _pad: vec2f,
};

@group(1) @binding(0) var<storage, read> hbgpu_atlas_buf: array<vec4<i32>>;
@group(1) @binding(1) var<storage, read> glyph_recs: array<GlyphRec>;

struct GOut {
  @builtin(position) pos: vec4f,
  @location(0) tc: vec2f,
  @location(1) @interpolate(flat) loc: u32,
  // Linear premultiplied colour.
  @location(2) @interpolate(flat) color: vec4f,
};

fn glyph_hidden () -> GOut {
  var o: GOut;
  o.pos = vec4f (2.0, 2.0, 2.0, 1.0);
  o.tc = vec2f (0.0);
  o.loc = 0u;
  o.color = vec4f (0.0);
  return o;
}

@vertex
fn vs_glyph (@builtin(vertex_index) vid: u32) -> GOut {
  let i = vid / 6u;
  let p = free_ro[i];
  if ((p.info.w & F_ALIVE) == 0u || p.pos.w < 0.0 || p.prev.w <= 0.0) {
    return glyph_hidden ();
  }
  let e = emit_draw[p.info.y];
  // `look`: x style, y em size in units, z spin in turns a second, w the emitter's letters
  // in `glyph_recs` as first * 32 + count.
  let packed = u32 (e.look.w);
  let count = packed & 31u;
  if (u32 (e.look.x) != ST_GLYPH || count == 0u) {
    return glyph_hidden ();
  }
  let is3d = (e.pool.w & E_3D) != 0u;
  let u = clamp (p.pos.w / p.prev.w, 0.0, 1.0);
  let fade = smoothstep (0.0, 0.1, u) * (1.0 - smoothstep (0.65, 1.0, u));
  let local = mix (p.prev.xyz, p.pos.xyz, frame.alpha);
  let c = particle_color (e, local, u);
  // Letters carry coverage, not light: no random brightness, and never past opaque.
  let a = min (c.a * fade * e.rot0.w, 1.0);
  let world = e.origin.xyz + rotate (e, local);
  let pr = project (world, is3d);
  if (!pr.ok || a <= 0.0) {
    return glyph_hidden ();
  }
  var vel = p.vel.xyz;
  if (e.path.x > 0.0) {
    vel = (p.pos.xyz - p.prev.xyz) * frame.viewport.w;
  }
  let p1 = project (world + rotate (e, vel) * 0.01, is3d);
  let v_px = (p1.px - pr.px) / 0.01;
  // The turn, clockwise on screen: by the spin, from a phase of its own, or along the travel.
  var turn = 0.0;
  if (e.look.z != 0.0) {
    turn = 2.0 * PI * (e.look.z * p.pos.w + p.vel.w);
  } else if (length (v_px) > 1e-3) {
    turn = atan2 (v_px.y, v_px.x);
  }
  let r = glyph_recs[(packed >> 5u) + pcg (i - e.pool.x) % count];
  // Em (y up) to pixels (y down), turned: the em x axis along the turn.
  let k = e.look.y * pr.scale / r.upem;
  let cs = vec2f (cos (turn), sin (turn));
  let m = mat2x2f (k * cs, k * vec2f (cs.y, -cs.x));
  // The ink box's centre sits on the particle.
  let mid = 0.5 * (r.ext.xy + r.ext.zw);
  let offset = pr.px - m * mid;
  // Two triangles: 0 1 2, 2 1 3, as the glyph pipeline's strip.
  var corners = array<vec2f, 6> (vec2f (0.0, 0.0), vec2f (1.0, 0.0), vec2f (0.0, 1.0),
                                 vec2f (0.0, 1.0), vec2f (1.0, 0.0), vec2f (1.0, 1.0));
  let cn = corners[vid % 6u];
  let em = mix (r.ext.xy, r.ext.zw, cn);
  let pos = m * em + offset;
  let s = cn * 2.0 - 1.0;
  let normal = normalize (m * vec2f (s.x, 0.0)) + normalize (m * vec2f (0.0, s.y));
  let lin = vec4f (m[0], m[1]);
  let det = lin.x * lin.w - lin.z * lin.y;
  let jac = vec4f (lin.w, -lin.z, -lin.y, lin.x) / det;
  let vp = frame.viewport.xy;
  let mvp = mat4x4f (vec4f (2.0 / vp.x, 0.0, 0.0, 0.0), vec4f (0.0, -2.0 / vp.y, 0.0, 0.0),
                     vec4f (0.0, 0.0, 1.0, 0.0), vec4f (-1.0, 1.0, 0.0, 1.0));
  let d = hb_gpu_dilate (pos, em, normal, jac, mvp, vp);
  var o: GOut;
  let n = d[0] / vp * 2.0 - 1.0;
  o.pos = vec4f (n.x, -n.y, pr.depth, 1.0);
  o.tc = d[1];
  o.loc = r.loc;
  o.color = vec4f (c.rgb * a, a);
  return o;
}

@fragment
fn fs_glyph (v: GOut) -> @location(0) vec4f {
  var c = v.color * hb_gpu_draw (v.tc, v.loc);
  if (frame.viewport.z != 0.0 && c.a > 0.0) {
    let straight = clamp (c.rgb / c.a, vec3f (0.0), vec3f (1.0));
    c = vec4f (srgb_encode (straight) * c.a, c.a);
  }
  return c;
}
