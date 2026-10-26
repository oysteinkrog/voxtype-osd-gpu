// `vs_free`, `vs_trail`, `vs_word` and `fs_sprite`, with `particles.wgsl` in front.

// --- draw: sprites by vertex pulling, 6 vertices per sprite ---

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<storage, read> free_ro: array<Free>;
@group(0) @binding(2) var<storage, read> emit_draw: array<EmitDraw>;
@group(0) @binding(3) var<storage, read> words_ro: array<Word>;
@group(0) @binding(4) var<storage, read> slot_draw: array<SlotDraw>;
@group(0) @binding(5) var<storage, read> trail_ro: array<vec4f>;
@group(0) @binding(6) var<storage, read> trail_owner: array<u32>;

struct VOut {
  @builtin(position) pos: vec4f,
  // Quad coordinates in pixels from the segment centre: along, across.
  @location(0) q: vec2f,
  // Half length of the core segment and the sprite radius, in pixels.
  @location(1) seg: vec2f,
  // Linear premultiplied colour.
  @location(2) color: vec4f,
  // x style, y softness 0 to 1 (bokeh: how far out of focus).
  @location(3) @interpolate(flat) style: vec2f,
};

// A point projected to pixels, its depth (0 to 1) and its pixels per logical unit there.
struct Proj {
  px: vec2f,
  depth: f32,
  scale: f32,
  ok: bool,
};

fn to_px (p: vec2f) -> vec2f {
  return vec2f (frame.m.x * p.x + frame.m.z * p.y, frame.m.y * p.x + frame.m.w * p.y) + frame.t.xy;
}

fn project (world: vec3f, is3d: bool) -> Proj {
  var o: Proj;
  if (is3d && frame.cam.x > 0.0) {
    let c = frame.view_proj * vec4f (world, 1.0);
    o.ok = c.w > 1e-4;
    let w = max (c.w, 1e-4);
    let ndc = c.xyz / w;
    o.px = vec2f (ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5) * frame.viewport.xy;
    o.depth = ndc.z;
    o.scale = frame.cam.y / w;
  } else {
    o.ok = true;
    o.px = to_px (world.xy);
    o.depth = 0.0;
    o.scale = frame.t.z;
  }
  return o;
}

fn hidden () -> VOut {
  var o: VOut;
  o.pos = vec4f (2.0, 2.0, 2.0, 1.0);
  o.q = vec2f (0.0);
  o.seg = vec2f (0.0, 1.0);
  o.color = vec4f (0.0);
  o.style = vec2f (0.0);
  return o;
}

// A sprite of `radius` pixels around a segment through `c_px` along `axis` (unit) with half
// length `half`.
fn sprite (corner: u32, c_px: vec2f, axis: vec2f, half: f32, radius: f32, depth: f32, color: vec4f, style: vec2f) -> VOut {
  let across = vec2f (-axis.y, axis.x);
  // Two triangles: 0 1 2, 2 1 3.
  var c = array<vec2f, 6> (vec2f (-1.0, -1.0), vec2f (1.0, -1.0), vec2f (-1.0, 1.0),
                           vec2f (-1.0, 1.0), vec2f (1.0, -1.0), vec2f (1.0, 1.0));
  let k = c[corner];
  let q = k * vec2f (half + radius, radius);
  let px = c_px + axis * q.x + across * q.y;
  var o: VOut;
  let n = px / frame.viewport.xy * 2.0 - 1.0;
  o.pos = vec4f (n.x, -n.y, depth, 1.0);
  o.q = q;
  o.seg = vec2f (half, radius);
  // A stretched sprite spreads the same light over a longer streak.
  o.color = color * (radius / (radius + half));
  o.style = style;
  return o;
}

// A sprite at `p_px` stretched along its velocity `v_px` (pixels per second).
fn moving (corner: u32, p_px: vec2f, v_px: vec2f, radius: f32, depth: f32, color: vec4f, style: vec2f) -> VOut {
  let speed = length (v_px);
  let axis = select (vec2f (1.0, 0.0), v_px / speed, speed > 1e-3);
  var half = 0.5 * speed * frame.stretch;
  if (style.x == f32 (ST_DOT)) {
    half = 0.0;
  }
  return sprite (corner, p_px, axis, half, radius, depth, color, style);
}

fn ramp (e: EmitDraw, u: f32) -> vec4f {
  if (u < 0.5) {
    return mix (e.c0, e.c1, u * 2.0);
  }
  return mix (e.c1, e.c2, u * 2.0 - 1.0);
}

fn rotate (e: EmitDraw, p: vec3f) -> vec3f {
  return vec3f (dot (e.rot0.xyz, p), dot (e.rot1.xyz, p), dot (e.rot2.xyz, p));
}

fn snap (px: vec2f, pitch: f32) -> vec2f {
  if (pitch <= 0.0) {
    return px;
  }
  return (floor (px / pitch) + 0.5) * pitch;
}

// Radius, colour scale and softness of a sprite of `r_px` at clip depth `w` for emitter `e`.
fn bokeh (e: EmitDraw, r_px: f32, w: f32) -> vec3f {
  if (u32 (e.look.x) != ST_BOKEH) {
    return vec3f (r_px, 1.0, 0.0);
  }
  let blur = abs (w - e.look.z) * e.look.w;
  let r = r_px + blur;
  // The same light over a larger disc.
  let k = clamp ((r_px * r_px) / (r * r), 0.02, 1.0);
  return vec3f (r, k, clamp (blur / max (r, 1e-3), 0.0, 1.0));
}

fn clip_w (world: vec3f, is3d: bool) -> f32 {
  if (is3d && frame.cam.x > 0.0) {
    return (frame.view_proj * vec4f (world, 1.0)).w;
  }
  return 0.0;
}

@vertex
fn vs_free (@builtin(vertex_index) vid: u32) -> VOut {
  let i = vid / 6u;
  let p = free_ro[i];
  if ((p.info.w & F_ALIVE) == 0u || p.pos.w < 0.0 || p.prev.w <= 0.0) {
    return hidden ();
  }
  let e = emit_draw[p.info.y];
  let is3d = (e.pool.w & E_3D) != 0u;
  let u = clamp (p.pos.w / p.prev.w, 0.0, 1.0);
  let fade = smoothstep (0.0, 0.1, u) * (1.0 - smoothstep (0.65, 1.0, u));
  let c = ramp (e, u);
  var a = c.a * fade * e.rot0.w * (0.6 + 0.8 * p.vel.w);
  let local = mix (p.prev.xyz, p.pos.xyz, frame.alpha);
  let world = e.origin.xyz + rotate (e, local);
  let pr = project (world, is3d);
  if (!pr.ok) {
    return hidden ();
  }
  let p1 = project (world + rotate (e, p.vel.xyz) * 0.01, is3d);
  let v_px = (p1.px - pr.px) / 0.01;
  let style = u32 (e.look.x);
  var radius = e.origin.w * pr.scale * (0.7 + 0.6 * p.vel.w);
  let b = bokeh (e, radius, clip_w (world, is3d));
  radius = b.x;
  a *= b.y;
  var px = pr.px;
  if (style == ST_DOT) {
    px = snap (px, e.look.y);
  }
  return moving (vid % 6u, px, v_px, max (radius, 0.75), pr.depth, vec4f (c.rgb * a, a), vec2f (f32 (style), b.z));
}

// Ribbons (PA07): segment k of hero h joins its positions k and k + 1 ticks ago.
@vertex
fn vs_trail (@builtin(vertex_index) vid: u32) -> VOut {
  let segs = frame.trail_len - 1u;
  let idx = vid / 6u;
  let h = idx / segs;
  let k = idx % segs;
  let i = trail_owner[h];
  let p = free_ro[i];
  if ((p.info.w & F_ALIVE) == 0u || p.pos.w < 0.0 || p.prev.w <= 0.0) {
    return hidden ();
  }
  let e = emit_draw[p.info.y];
  let is3d = (e.pool.w & E_3D) != 0u;
  let n = frame.tick;
  if (k + 1u > n) {
    return hidden ();
  }
  var a_local: vec3f;
  if (k == 0u) {
    a_local = mix (p.prev.xyz, p.pos.xyz, frame.alpha);
  } else {
    a_local = trail_ro[h * TRAIL + (n - k) % TRAIL].xyz;
  }
  let b_rec = trail_ro[h * TRAIL + (n - k - 1u) % TRAIL];
  // History from before this life is marked with a huge age: hide those segments.
  if (b_rec.w > p.pos.w || (k > 0u && trail_ro[h * TRAIL + (n - k) % TRAIL].w > p.pos.w)) {
    return hidden ();
  }
  let pa = project (e.origin.xyz + rotate (e, a_local), is3d);
  let pb = project (e.origin.xyz + rotate (e, b_rec.xyz), is3d);
  if (!pa.ok || !pb.ok) {
    return hidden ();
  }
  let d = pb.px - pa.px;
  let len = length (d);
  let axis = select (vec2f (1.0, 0.0), d / len, len > 1e-4);
  let f = (f32 (k) + frame.alpha) / f32 (segs);
  let u = clamp (p.pos.w / p.prev.w, 0.0, 1.0);
  let fade = smoothstep (0.0, 0.1, u) * (1.0 - smoothstep (0.65, 1.0, u));
  let c = ramp (e, u);
  let a = c.a * fade * e.rot0.w * e.trail.y * pow (1.0 - f, 1.5);
  let radius = max (e.origin.w * pa.scale * e.trail.x * (1.0 - 0.7 * f), 0.5);
  return sprite (vid % 6u, 0.5 * (pa.px + pb.px), axis, 0.5 * len, radius, pa.depth, vec4f (c.rgb * a, a), vec2f (f32 (ST_SOFT), 0.0));
}

// The solid glyph's opacity `tau` seconds into its reveal: a smoothstep over the convergence
// of the slowest particle's spring.
fn reveal (tau: f32) -> f32 {
  let x = frame.w_min * max (tau, 0.0);
  let conv = 1.0 - (1.0 + x) * exp (-x);
  return smoothstep (frame.reveal0, frame.reveal1, conv);
}

@vertex
fn vs_word (@builtin(vertex_index) vid: u32) -> VOut {
  let i = vid / 6u;
  let p = words_ro[i];
  if (p.flags == 0u) {
    return hidden ();
  }
  let capacity = arrayLength (&words_ro) / arrayLength (&slot_draw);
  let sd = slot_draw[i / capacity];
  if (sd.visible <= 0.0) {
    return hidden ();
  }
  let t = frame.time;
  let ev = eval_word (p, t);
  var light: f32;
  if ((p.flags & W_DYING) != 0u) {
    light = 1.0 - smoothstep (0.0, 0.16, t - p.t0);
  } else if ((p.flags & W_PERSIST) != 0u) {
    light = 1.0;
  } else {
    // 1 minus the solid glyph's opacity (`reveal` in particles.rs), so the word's light
    // moves from the particles to the glyph.
    light = 1.0 - reveal (t - p.reveal_t0 - p.reveal_delay);
  }
  if ((p.flags & W_FRESH) != 0u) {
    light *= smoothstep (0.0, 0.05, t - p.t0);
  }
  if (light <= 0.0) {
    return hidden ();
  }
  let style = u32 (sd.look.x);
  // Wobble fades with the square of the remaining distance, so it is gone before the reveal.
  var wob = curl (p.goal * 0.15, t * 1.3 + f32 (i % 7u)) * ev.rem * ev.rem * 0.64 * p.stroke;
  if (style == ST_DOT) {
    wob *= 0.25;
  }
  let pos = ev.x + wob + sd.origin;
  var p_px = to_px (pos);
  let v_px = to_px (pos + ev.v) - p_px;
  let ppu = frame.t.z;
  var radius: f32;
  var a = sd.color.a * light * sd.visible;
  if (style == ST_DOT) {
    // A dot: its own size, snapped to the grid once it has nearly arrived.
    radius = max (sd.size * ppu, 0.75);
    p_px = mix (p_px, snap (p_px, sd.look.y), smoothstep (0.15, 0.02, ev.rem));
  } else {
    // Ink condensing: large and soft while far, about 0.6 of a stroke at arrival.
    let base = select (p.stroke, sd.size, sd.size > 0.0);
    radius = max (base * ppu * mix (0.6, 1.4, ev.rem), 0.6);
    // Each particle stands for about 1.25 px² of ink (`set_word`), and a sprite of radius r
    // spreads its light over about pi r² / 3 px². Scale so the forming word carries about
    // twice the light of the solid word, which reads as glow.
    if ((p.flags & W_PERSIST) == 0u) {
      a *= clamp (2.0 * 1.25 * 3.0 / (PI * radius * radius), 0.0, 1.0);
    }
  }
  return moving (vid % 6u, p_px, v_px, radius, 0.0, vec4f (sd.color.rgb * a, a), vec2f (f32 (style), 0.0));
}

fn srgb_encode (c: vec3f) -> vec3f {
  let lo = c * 12.92;
  let hi = 1.055 * pow (max (c, vec3f (0.0)), vec3f (1.0 / 2.4)) - 0.055;
  return select (hi, lo, c <= vec3f (0.0031308));
}

@fragment
fn fs_sprite (v: VOut) -> @location(0) vec4f {
  // Distance from the core segment, in pixels and in sprite radii.
  let dpx = length (vec2f (max (abs (v.q.x) - v.seg.x, 0.0), v.q.y));
  let d = dpx / v.seg.y;
  var k: f32;
  let style = u32 (v.style.x);
  if (style == ST_DOT) {
    // A hard disc, antialiased over one pixel.
    k = clamp (v.seg.y - dpx + 0.5, 0.0, 1.0);
  } else {
    // Soft: a Gaussian-ish falloff that reaches 0 at the quad edge.
    let soft = exp (-d * d * 3.0) * (1.0 - smoothstep (0.8, 1.0, d));
    if (style == ST_BOKEH) {
      // Out of focus: a flat disc with a slightly brighter rim.
      let disc = (1.0 - smoothstep (0.85, 1.0, d)) * (0.8 + 0.35 * smoothstep (0.6, 0.95, d));
      k = mix (soft, disc, v.style.y);
    } else {
      k = soft;
    }
  }
  var c = v.color * k;
  if (frame.viewport.z != 0.0 && c.a > 0.0) {
    let straight = clamp (c.rgb / c.a, vec3f (0.0), vec3f (1.0));
    c = vec4f (srgb_encode (straight) * c.a, c.a);
  }
  return c;
}
