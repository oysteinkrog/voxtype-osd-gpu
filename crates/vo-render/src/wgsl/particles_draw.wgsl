// `vs_free`, `vs_word` and `fs_sprite`, with `particles.wgsl` in front.

// --- draw: sprites by vertex pulling, 6 vertices per particle ---

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<storage, read> free_ro: array<Free>;
@group(0) @binding(2) var<storage, read> emit_draw: array<EmitDraw>;
@group(0) @binding(3) var<storage, read> words_ro: array<Word>;
@group(0) @binding(4) var<storage, read> slot_draw: array<SlotDraw>;

struct VOut {
  @builtin(position) pos: vec4f,
  // Quad coordinates in pixels from the segment centre: along, across.
  @location(0) q: vec2f,
  // Half length of the core segment and the sprite radius, in pixels.
  @location(1) seg: vec2f,
  // Linear premultiplied colour.
  @location(2) color: vec4f,
};

fn to_px (p: vec2f) -> vec2f {
  return vec2f (frame.m.x * p.x + frame.m.z * p.y, frame.m.y * p.x + frame.m.w * p.y) + frame.t.xy;
}

fn hidden () -> VOut {
  var o: VOut;
  o.pos = vec4f (2.0, 2.0, 2.0, 1.0);
  o.q = vec2f (0.0);
  o.seg = vec2f (0.0, 1.0);
  o.color = vec4f (0.0);
  return o;
}

// A soft sprite at `p_px`, stretched along `v_px` (pixels per second).
fn sprite (corner: u32, p_px: vec2f, v_px: vec2f, radius: f32, color: vec4f) -> VOut {
  let speed = length (v_px);
  let along = select (vec2f (1.0, 0.0), v_px / speed, speed > 1e-3);
  let across = vec2f (-along.y, along.x);
  let half = 0.5 * speed * frame.stretch;
  // Two triangles: 0 1 2, 2 1 3.
  var c = array<vec2f, 6> (vec2f (-1.0, -1.0), vec2f (1.0, -1.0), vec2f (-1.0, 1.0),
                           vec2f (-1.0, 1.0), vec2f (1.0, -1.0), vec2f (1.0, 1.0));
  let k = c[corner];
  let ext = vec2f (half + radius, radius);
  let q = k * ext;
  let px = p_px + along * q.x + across * q.y;
  var o: VOut;
  let n = px / frame.viewport.xy * 2.0 - 1.0;
  o.pos = vec4f (n.x, -n.y, 0.0, 1.0);
  o.q = q;
  o.seg = vec2f (half, radius);
  // A stretched sprite spreads the same light over a longer streak.
  o.color = color * (radius / (radius + half));
  return o;
}

// The solid glyph's opacity `tau` seconds into its reveal: a smoothstep over the convergence
// of the slowest particle's spring.
fn reveal (tau: f32) -> f32 {
  let x = frame.w_min * max (tau, 0.0);
  let conv = 1.0 - (1.0 + x) * exp (-x);
  return smoothstep (frame.reveal0, frame.reveal1, conv);
}

fn ramp (e: EmitDraw, u: f32) -> vec4f {
  if (u < 0.5) {
    return mix (e.c0, e.c1, u * 2.0);
  }
  return mix (e.c1, e.c2, u * 2.0 - 1.0);
}

@vertex
fn vs_free (@builtin(vertex_index) vid: u32) -> VOut {
  let i = vid / 6u;
  let p = free_ro[i];
  if (p.age < 0.0 || p.life <= 0.0) {
    return hidden ();
  }
  let e = emit_draw[p.emitter];
  let u = clamp (p.age / p.life, 0.0, 1.0);
  let fade = smoothstep (0.0, 0.1, u) * (1.0 - smoothstep (0.65, 1.0, u));
  var c = ramp (e, u);
  let a = c.a * fade * e.brightness * (0.6 + 0.8 * p.r);
  let pos = mix (p.prev, p.pos, frame.alpha) + e.origin;
  let p_px = to_px (pos);
  let v_px = to_px (pos + p.vel) - p_px;
  let ppu = frame.t.z;
  let radius = e.size * ppu * (0.7 + 0.6 * p.r);
  return sprite (vid % 6u, p_px, v_px, max (radius, 0.75), vec4f (c.rgb * a, a));
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
  } else {
    // 1 minus the solid glyph's opacity (`reveal` in particles.rs), so the word's light
    // moves from the particles to the glyph.
    light = 1.0 - reveal (t - p.reveal_t0 - p.reveal_delay);
    if ((p.flags & W_FRESH) != 0u) {
      light *= smoothstep (0.0, 0.05, t - p.t0);
    }
  }
  if (light <= 0.0) {
    return hidden ();
  }
  // Wobble fades with the square of the remaining distance, so it is gone before the reveal.
  let wob = curl (p.goal * 0.15, t * 1.3 + f32 (i % 7u)) * ev.rem * ev.rem * 0.08 * p.stroke * 8.0;
  let pos = ev.x + wob + sd.origin;
  let p_px = to_px (pos);
  let v_px = to_px (pos + ev.v) - p_px;
  let ppu = frame.t.z;
  // Ink condensing: large and soft while far, about 0.6 of a stroke at arrival.
  let radius = max (p.stroke * ppu * mix (0.6, 1.4, ev.rem), 0.6);
  // Each particle stands for about 1.25 px² of ink (`set_word`), and a sprite of radius r
  // spreads its light over about pi r² / 3 px². Scale so the forming word carries about the
  // light of the solid word, with a little extra for glow.
  let ink = clamp (1.4 * 1.25 * 3.0 / (PI * radius * radius), 0.0, 1.0);
  let a = sd.color.a * light * sd.visible * ink;
  return sprite (vid % 6u, p_px, v_px, radius, vec4f (sd.color.rgb * a, a));
}

fn srgb_encode (c: vec3f) -> vec3f {
  let lo = c * 12.92;
  let hi = 1.055 * pow (max (c, vec3f (0.0)), vec3f (1.0 / 2.4)) - 0.055;
  return select (hi, lo, c <= vec3f (0.0031308));
}

@fragment
fn fs_sprite (v: VOut) -> @location(0) vec4f {
  // Distance from the core segment, in sprite radii; a Gaussian-ish falloff that reaches 0
  // at the quad edge.
  let d = length (vec2f (max (abs (v.q.x) - v.seg.x, 0.0), v.q.y)) / v.seg.y;
  let k = exp (-d * d * 3.0) * (1.0 - smoothstep (0.8, 1.0, d));
  var c = v.color * k;
  if (frame.viewport.z != 0.0 && c.a > 0.0) {
    let straight = clamp (c.rgb / c.a, vec3f (0.0), vec3f (1.0));
    c = vec4f (srgb_encode (straight) * c.a, c.a);
  }
  return c;
}
