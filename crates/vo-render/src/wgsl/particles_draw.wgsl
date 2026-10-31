// `vs_free`, `vs_trail`, `vs_word` and `fs_sprite`, with `particles.wgsl` in front.

// --- draw: sprites by vertex pulling, 6 vertices per sprite ---

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<storage, read> free_ro: array<Free>;
@group(0) @binding(2) var<storage, read> emit_draw: array<EmitDraw>;
@group(0) @binding(3) var<storage, read> words_ro: array<Word>;
@group(0) @binding(4) var<storage, read> slot_draw: array<SlotDraw>;
@group(0) @binding(5) var<storage, read> trail_ro: array<vec4f>;
@group(0) @binding(6) var<storage, read> trail_owner: array<u32>;
@group(0) @binding(7) var<storage, read> paths: array<vec4f>;

// Emitter `em`'s path at arc length `s`: position and normal (as in `particles_step.wgsl`).
fn path_at (em: u32, s: f32) -> vec4f {
  let base = em * PATH_N;
  let len = paths[base + PATH_N - 1u].z;
  let f = clamp (s / max (len, 1e-6), 0.0, 1.0) * f32 (PATH_N - 1u);
  let i = min (u32 (f), PATH_N - 2u);
  return path_point (paths[base + i], paths[base + i + 1u], f - f32 (i));
}

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

// A point of light (ST_SPARK) carrying `energy` px² of `rgb` light: a Gaussian core of
// sigma at least `min_sigma` pixels, drawn out to 2.5 sigma. Its peak may pass 1; a dense
// spot then saturates per channel towards white. A moving spark is drawn out along its
// motion and keeps the same energy, so a fast one is a dimmer streak, not a brighter one.
// The fragment shader reads sigma from `style.y`.
fn spark (corner: u32, p_px: vec2f, v_px: vec2f, r_px: f32, depth: f32, rgb: vec3f, energy: f32) -> VOut {
  let sigma = max (r_px * 0.7, 0.55);
  // The streak's light: a Gaussian disc plus a core segment `2 half` long with a Gaussian
  // cross-section (`moving` sets `half`). `sprite` dims a streak by radius / (radius + half)
  // for soft sprites; that is undone here, since this is exact.
  let half = 0.5 * length (v_px) * frame.stretch;
  let radius = 2.5 * sigma;
  let peak = energy / (PI * sigma * sigma + 2.0 * half * sqrt (PI) * sigma);
  let c = vec4f (rgb * peak, min (peak, 1.0)) * ((radius + half) / radius);
  return moving (corner, p_px, v_px, radius, depth, c, vec2f (f32 (ST_SPARK), sigma));
}

// Colour of free particle `p` at life fraction `u`: by age, by its angle about the
// emitter's axis (seamless around the circle), or by its x as drawn, after the rotation
// (`trail.w` is the half width).
fn particle_color (e: EmitDraw, local: vec3f, u: f32) -> vec4f {
  if (e.trail.z == 1.0) {
    let turn = fract (atan2 (local.y, local.x) / (2.0 * PI) + 1.0);
    let c = ramp (e, 1.0 - abs (2.0 * turn - 1.0));
    return vec4f (c.rgb, c.a * ramp (e, u).a / max (e.c0.a, 1e-3));
  }
  if (e.trail.z == 2.0) {
    let x = rotate (e, local).x;
    let c = ramp (e, clamp (0.5 + 0.5 * x / max (e.trail.w, 1e-3), 0.0, 1.0));
    return vec4f (c.rgb, c.a * ramp (e, u).a / max (e.c0.a, 1e-3));
  }
  return ramp (e, u);
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

// Dot centres (PA09), snapped as vo-dots snaps its dots: onto the grid of `pitch` pixels
// (rounded to whole pixels; 0 for no grid), then onto the pixel grid by the dot's diameter,
// so an even diameter sits on a pixel corner and an odd one on a pixel centre. The move is
// weighted by `1 - 0.5 * speed in cells per second`, so snapping fades in as a dot slows.
fn snap_dot (px: vec2f, radius: f32, pitch: f32, v_px: vec2f) -> vec2f {
  var s = px;
  var cells = length (v_px);
  if (pitch > 0.0) {
    let p = max (round (pitch), 1.0);
    s = (floor (px / p) + 0.5) * p;
    cells = cells / p;
  }
  if (round (2.0 * radius) % 2.0 == 0.0) {
    s = round (s);
  } else {
    s = floor (s) + 0.5;
  }
  return mix (px, s, clamp (1.0 - 0.5 * cells, 0.0, 1.0));
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
  if (u32 (e.look.x) == ST_GLYPH) {
    return hidden ();
  }
  let is3d = (e.pool.w & E_3D) != 0u;
  let u = clamp (p.pos.w / p.prev.w, 0.0, 1.0);
  let fade = smoothstep (0.0, 0.1, u) * (1.0 - smoothstep (0.65, 1.0, u));
  let local = mix (p.prev.xyz, p.pos.xyz, frame.alpha);
  var c = particle_color (e, local, u);
  var a = c.a * fade * e.rot0.w * (0.6 + 0.8 * p.vel.w);
  let world = e.origin.xyz + rotate (e, local);
  let pr = project (world, is3d);
  if (!pr.ok) {
    return hidden ();
  }
  // Path particles keep their arc length in `vel`; their velocity is their last step.
  var vel = p.vel.xyz;
  if (e.path.x > 0.0) {
    vel = (p.pos.xyz - p.prev.xyz) * frame.viewport.w;
  }
  let p1 = project (world + rotate (e, vel) * 0.01, is3d);
  let v_px = (p1.px - pr.px) / 0.01;
  let style = u32 (e.look.x);
  var radius = e.origin.w * pr.scale * (0.7 + 0.6 * p.vel.w);
  if (style == ST_SPARK) {
    // A share of brighter, whiter accent points; the rest twinkle a little.
    var gain = e.look.z * (1.0 + e.look.w * sin (frame.time * 9.0 + p.vel.w * 113.0));
    var rgb = c.rgb;
    if (p.vel.w > 1.0 - e.look.y) {
      gain *= 4.0;
      radius *= 1.4;
      rgb = mix (rgb, vec3f (1.0), 0.6);
    }
    // Light of a disc of the sprite's radius, so a point twice as near is four times as bright.
    let energy = PI * radius * radius * a * gain;
    return spark (vid % 6u, pr.px, v_px, radius, pr.depth, rgb, energy);
  }
  let b = bokeh (e, radius, clip_w (world, is3d));
  radius = b.x;
  a *= b.y;
  radius = max (radius, 0.75);
  var px = pr.px;
  if (style == ST_DOT) {
    px = snap_dot (px, radius, e.look.y, v_px);
  }
  return moving (vid % 6u, px, v_px, radius, pr.depth, vec4f (c.rgb * a, a), vec2f (f32 (style), b.z));
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
  let c = particle_color (e, a_local, u);
  let a = c.a * fade * e.rot0.w * e.trail.y * pow (1.0 - f, 1.5);
  let radius = max (e.origin.w * pa.scale * e.trail.x * (1.0 - 0.7 * f), 0.5);
  return sprite (vid % 6u, 0.5 * (pa.px + pb.px), axis, 0.5 * len, radius, pa.depth, vec4f (c.rgb * a, a), vec2f (f32 (ST_SOFT), 0.0));
}

// Ribbons along a path's strands: segment `i` of ribbon `k` of emitter `em` (the instance),
// at the frame time, so they flow as smoothly as the particles on the same strands.
@vertex
fn vs_ribbon (@builtin(vertex_index) vid: u32, @builtin(instance_index) em: u32) -> VOut {
  let e = emit_draw[em];
  let seg = vid / 6u;
  let k = seg / RIBBON_SEGS;
  let i = seg % RIBBON_SEGS;
  let len = paths[em * PATH_N + PATH_N - 1u].z;
  if (f32 (k) >= e.ribbon.x || len <= 0.0) {
    return hidden ();
  }
  let is3d = (e.pool.w & E_3D) != 0u;
  let t = frame.time;
  let s0 = f32 (i) / f32 (RIBBON_SEGS) * len;
  let s1 = f32 (i + 1u) / f32 (RIBBON_SEGS) * len;
  let a = path_at (em, s0);
  let b = path_at (em, s1);
  let la = vec3f (a.xy + a.zw * strand_offset (f32 (k), s0, len, t, e.path), 0.0);
  let lb = vec3f (b.xy + b.zw * strand_offset (f32 (k), s1, len, t, e.path), 0.0);
  let pa = project (e.origin.xyz + rotate (e, la), is3d);
  let pb = project (e.origin.xyz + rotate (e, lb), is3d);
  if (!pa.ok || !pb.ok) {
    return hidden ();
  }
  let u = (f32 (i) + 0.5) / f32 (RIBBON_SEGS);
  let c = ramp (e, u);
  // Fade in and out at the path's ends.
  let a_end = smoothstep (0.0, 0.08, u) * (1.0 - smoothstep (0.92, 1.0, u));
  let alpha = c.a * e.ribbon.z * e.rot0.w * a_end;
  let sigma = max (0.5 * e.ribbon.y * pa.scale, 0.45);
  // Light per pixel of length that a core 1 px wide at full strength would have.
  let col = vec4f (c.rgb * alpha, alpha);
  return band (vid % 6u, pa.px, pb.px, 6.0 * sigma, pa.depth, col, vec2f (f32 (ST_RIBBON), sigma));
}

// A quad from `a` to `b` reaching `half_w` pixels to each side, with no end caps, so
// consecutive segments of a ribbon join without overlapping.
fn band (corner: u32, a: vec2f, b: vec2f, half_w: f32, depth: f32, color: vec4f, style: vec2f) -> VOut {
  let d = b - a;
  let l = length (d);
  let axis = select (vec2f (1.0, 0.0), d / l, l > 1e-4);
  let across = vec2f (-axis.y, axis.x);
  var c = array<vec2f, 6> (vec2f (0.0, -1.0), vec2f (1.0, -1.0), vec2f (0.0, 1.0),
                           vec2f (0.0, 1.0), vec2f (1.0, -1.0), vec2f (1.0, 1.0));
  let k = c[corner];
  let px = a + d * k.x + across * (k.y * half_w);
  var o: VOut;
  let n = px / frame.viewport.xy * 2.0 - 1.0;
  o.pos = vec4f (n.x, -n.y, depth, 1.0);
  o.q = vec2f (0.0, k.y * half_w);
  o.seg = vec2f (0.0, half_w);
  o.color = color;
  o.style = style;
  return o;
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
  let sd = slot_draw[p.slot];
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
  // The light this particle carries: its share of the glyph's ink, so the formed word holds
  // the solid word's light and the hand-over neither dims nor flashes.
  let ink = max (sd.look.z, 0.1) * light * sd.color.a * sd.visible;
  if (style == ST_SPARK) {
    let r = select (sqrt (ink / PI) / 0.7, sd.size * ppu, sd.size > 0.0);
    return spark (vid % 6u, p_px, v_px, r, 0.0, sd.color.rgb, ink);
  }
  if (style == ST_DOT) {
    // A dot: its own size, snapped as it comes to rest.
    radius = max (sd.size * ppu, 0.75);
    p_px = snap_dot (p_px, radius, sd.look.y, v_px);
  } else {
    // Ink condensing: large and soft while far, about 0.6 of a stroke at arrival.
    let base = select (p.stroke, sd.size, sd.size > 0.0);
    radius = max (base * ppu * mix (0.6, 1.4, ev.rem), 0.6);
    // A soft sprite of radius r spreads its light over about pi r² / 3 px.
    if ((p.flags & W_PERSIST) == 0u) {
      a *= clamp (max (sd.look.z, 0.1) * 3.0 / (PI * radius * radius), 0.0, 1.0);
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
  } else if (style == ST_SPARK) {
    let x = dpx / v.style.y;
    k = exp (-x * x) * (1.0 - smoothstep (2.0, 2.5, x));
  } else if (style == ST_RIBBON) {
    // Across the ribbon only: a hot core and a faint wide halo, fading out at the quad edge.
    let x = abs (v.q.y) / v.style.y;
    k = (exp (-x * x) + 0.12 * exp (-x * x / 9.0)) * (1.0 - smoothstep (4.5, 6.0, x));
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
  if (style == ST_SPARK || style == ST_RIBBON) {
    // Light, not coverage: each channel saturates on its own, so dense spots go white.
    c = vec4f (c.rgb, min (c.a, 1.0));
    if (frame.viewport.z != 0.0) {
      c = vec4f (srgb_encode (min (c.rgb, vec3f (1.0))), c.a);
    }
    return c;
  }
  if (frame.viewport.z != 0.0 && c.a > 0.0) {
    let straight = clamp (c.rgb / c.a, vec3f (0.0), vec3f (1.0));
    c = vec4f (srgb_encode (straight) * c.a, c.a);
  }
  return c;
}
