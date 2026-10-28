// Glass panel material (FX06) with its animated rim (FX08) and a soft drop shadow. One
// instanced quad per panel. Fragment units are physical pixels; colours are linear light,
// premultiplied.
//
// A prelude goes in front of this file and defines `vs_glass`, `finish`, `glass_clip`,
// `glass_point`, `glass_origin`, `glass_turn` and `glass_aa`: `glass_view.wgsl` for
// `GlassPass` on its own, or `common.wgsl` plus `glass_layer.wgsl` inside the renderer's
// display list pass.
//
// Fragment positions are in the panel's own frame, in physical pixels: on a rotated layer
// the frame turns with the panel, so tint, bevel, rim and sheen follow it, and the light
// (fixed on screen) is turned into that frame by `glass_turn`. A panel is either the rounded
// rect, drawn exactly, or a closed polygon outline (EG-06) read from `glass_point`.

struct GlassIn {
  // Panel rect x, y, w, h.
  @location(0) rect: vec4f,
  // Corner radius, physical pixels per logical pixel, time, kind (0 light, 1 dark).
  @location(1) shape: vec4f,
  // Interior tint, linear straight rgb, and the interior alpha.
  @location(2) tint: vec4f,
  // Accent, linear straight rgb, and the alpha right at the edge.
  @location(3) accent: vec4f,
  // Orbit head 0..1 along the border (negative: off), orbit strength, sheen progress 0..1
  // (negative: off), overall opacity.
  @location(4) anim: vec4f,
  // Bevel width, rim light strength, grain amount, dispersion; lengths in logical pixels.
  @location(5) fx: vec4f,
  // Shadow alpha, shadow blur, shadow y offset (logical pixels), blur behind (0 or 1).
  @location(6) shadow: vec4f,
  // Outline (EG-06): first point, point count (0: the rounded rect), arc length of the top
  // centre and the perimeter, in the units of the points.
  @location(7) outline: vec4f,
};

struct GlassOut {
  @builtin(position) clip: vec4f,
  @location(0) p: vec2f,
  @location(1) @interpolate(flat) rect: vec4f,
  @location(2) @interpolate(flat) shape: vec4f,
  @location(3) @interpolate(flat) tint: vec4f,
  @location(4) @interpolate(flat) accent: vec4f,
  @location(5) @interpolate(flat) anim: vec4f,
  @location(6) @interpolate(flat) fx: vec4f,
  @location(7) @interpolate(flat) shadow: vec4f,
  @location(8) @interpolate(flat) outline: vec4f,
};

// The nearest point of an outline: signed distance (negative inside), outward unit normal,
// and arc length from the first point, all in physical pixels.
struct Near {
  d: f32,
  n: vec2f,
  s: f32,
};

// `p` against the closed polygon `o` (see `GlassIn::outline`), clockwise on screen, its
// points scaled by `k` into the panel frame. Inside by the nonzero winding rule.
fn outline_near (p: vec2f, o: vec4f, k: f32) -> Near {
  let first = u32 (o.x);
  let count = u32 (o.y);
  let origin = glass_origin ();
  var best = 1e30;
  var bv = vec2f (0.0);
  var be = vec2f (1.0, 0.0);
  var bs = 0.0;
  var wind = 0;
  var a4 = glass_point (first + count - 1u);
  var a = a4.xy * k + origin;
  for (var i = 0u; i < count; i++) {
    let b4 = glass_point (first + i);
    let b = b4.xy * k + origin;
    let e = b - a;
    let w = p - a;
    let h = clamp (dot (w, e) / max (dot (e, e), 1e-12), 0.0, 1.0);
    let v = w - e * h;
    let d2 = dot (v, v);
    if (d2 < best) {
      best = d2;
      bv = v;
      be = e;
      // The closing edge (last point to the first) starts at the last point's arc length.
      bs = a4.z * k + h * length (e);
    }
    let c = e.x * w.y - e.y * w.x;
    if (a.y <= p.y) {
      if (b.y > p.y && c > 0.0) {
        wind += 1;
      }
    } else if (b.y <= p.y && c < 0.0) {
      wind -= 1;
    }
    a4 = b4;
    a = b;
  }
  let sgn = select (1.0, -1.0, wind != 0);
  let dist = sqrt (best);
  var n: vec2f;
  if (dist > 1e-3) {
    n = sgn * bv / dist;
  } else {
    // On the edge: clockwise on screen (y down), outward is the edge turned left.
    n = normalize (vec2f (be.y, -be.x));
  }
  var r: Near;
  r.d = sgn * dist;
  r.n = n;
  r.s = bs;
  return r;
}

fn sd_box (q: vec2f, half: vec2f, r: f32) -> f32 {
  let d = abs (q) - half + vec2f (r);
  return length (max (d, vec2f (0.0))) + min (max (d.x, d.y), 0.0) - r;
}

// Outward unit gradient of `sd_box`.
fn grad_box (q: vec2f, half: vec2f, r: f32) -> vec2f {
  let d = abs (q) - half + vec2f (r);
  var n: vec2f;
  if (d.x > 0.0 && d.y > 0.0) {
    n = normalize (d);
  } else if (d.x > d.y) {
    n = vec2f (1.0, 0.0);
  } else {
    n = vec2f (0.0, 1.0);
  }
  return n * select (vec2f (-1.0), vec2f (1.0), q >= vec2f (0.0));
}

// Where `q` sits along the border, 0..1 clockwise from the top centre (y down), measured by
// arc length so a point moving at a steady rate keeps a steady speed round the corners.
fn border_param (q: vec2f, half: vec2f, r: f32) -> f32 {
  let h = half - vec2f (r);
  let quarter = 1.5707963 * r;
  let total = 4.0 * (h.x + h.y) + 4.0 * quarter;
  let x = abs (q.x);
  var s: f32;
  // Distances to the straight sides, to pick a side when inside the straight part.
  let to_tb = half.y - abs (q.y);
  let to_side = half.x - x;
  if (x > h.x && abs (q.y) > h.y) {
    let d = vec2f (x - h.x, abs (q.y) - h.y);
    if (q.y < 0.0) {
      s = h.x + r * atan2 (d.x, d.y);
    } else {
      s = h.x + quarter + 2.0 * h.y + r * atan2 (d.y, d.x);
    }
  } else if (to_tb <= to_side) {
    if (q.y < 0.0) {
      s = x;
    } else {
      s = h.x + 2.0 * quarter + 2.0 * h.y + (h.x - x);
    }
  } else {
    s = h.x + quarter + (q.y + h.y);
  }
  // The left half mirrors the right half.
  if (q.x < 0.0) {
    s = total - s;
  }
  return fract (s / total);
}

// FX10 hash: a cheap per-pixel value in [0, 1).
fn hash21 (p: vec2f) -> f32 {
  var q = fract (p * vec2f (123.34, 456.21));
  q += dot (q, q + 45.32);
  return fract (q.x * q.y);
}

// Slope of the bevel at `t` (0 on the edge, 1 where the flat top starts): a quarter circle.
fn bevel_slope (t: f32) -> f32 {
  let u = clamp (1.0 - t, 0.0, 0.995);
  return min (u / sqrt (1.0 - u * u), 6.0);
}

@fragment fn fs_glass (in: GlassOut) -> @location(0) vec4f {
  let k = in.shape.y;
  let half = 0.5 * in.rect.zw;
  let q = in.p - (in.rect.xy + half);
  let r = min (in.shape.x, min (half.x, half.y));
  let shaped = in.outline.y > 0.0;
  // Distance, outward normal and border position: exact for the rounded rect, from the
  // polygon for an outline.
  var d: f32;
  var gn: vec2f;
  var near: Near;
  if (shaped) {
    near = outline_near (in.p, in.outline, k);
    d = near.d;
    gn = near.n;
  } else {
    d = sd_box (q, half, r);
    gn = grad_box (q, half, r);
  }
  let cover = clamp (0.5 - d * glass_aa (in.clip.xy), 0.0, 1.0);
  let dark = in.shape.w;
  let opacity = in.anim.w;
  // Screen directions in the panel frame: the light and the shadow's drop.
  let turn = transpose (glass_turn ());

  // Drop shadow, outside the panel only: the glass is see-through, so a shadow under it
  // would darken the backdrop the blur shows.
  var shadow = 0.0;
  if (in.shadow.x > 0.0) {
    let drop = turn * vec2f (0.0, in.shadow.z * k);
    var ds: f32;
    if (shaped) {
      ds = outline_near (in.p - drop, in.outline, k).d;
    } else {
      ds = sd_box (q - drop, half, r);
    }
    let blur = max (in.shadow.y * k, 1.0);
    shadow = in.shadow.x * exp (-pow (max (ds, 0.0) / blur, 2.0)) * smoothstep (-blur, 0.0, ds);
    shadow *= 1.0 - cover;
  }
  if (cover <= 0.0 && shadow <= 0.0) {
    discard;
  }
  let inside = max (-d, 0.0);

  // Bevel: normal tilted outward along the SDF gradient.
  let bw = max (in.fx.x * k, 1.0);
  let t = clamp (inside / bw, 0.0, 1.0);
  let n = normalize (vec3f (gn * bevel_slope (t), 1.0));

  // Light from the upper left of the screen, a little in front.
  let l0 = normalize (vec3f (-0.45, -0.75, 0.55));
  let l = vec3f (turn * l0.xy, l0.z);
  let h = normalize (l + vec3f (0.0, 0.0, 1.0));
  let spec = pow (max (dot (n, h), 0.0), 48.0);
  let fres = 0.04 + 0.96 * pow (1.0 - n.z, 5.0);
  let facing = dot (n, l) - l.z;

  // Tint alpha: the edge alpha at the rim rising to the interior alpha over 12 logical
  // pixels, inside the panel padding, so text always sits on the interior alpha that the
  // contrast gate (AX01) checks.
  let ramp = smoothstep (0.0, 1.0, clamp (inside / (12.0 * k), 0.0, 1.0));
  let a_tint = mix (in.accent.w, in.tint.w, ramp);
  // A lit sheet: a little lighter at the top, a little darker at the bottom.
  let vy = clamp ((in.p.y - in.rect.y) / in.rect.w, 0.0, 1.0);
  let grain = (hash21 (floor (in.p)) - 0.5) * in.fx.z;
  var base = in.tint.rgb * mix (1.12, 0.92, vy) * (1.0 + grain);
  if (dark < 0.5) {
    // Light glass: grain darkens a little instead of scaling a near-white tint.
    base = in.tint.rgb * mix (1.0, 0.97, vy) + vec3f (grain * 0.04);
  }

  // Rim light with dispersion: each channel reads the bevel a little further in.
  var rim = vec3f (0.0);
  for (var c = 0; c < 3; c++) {
    let tc = clamp ((inside - f32 (c) * in.fx.w * k) / bw, 0.0, 1.0);
    let nc = normalize (vec3f (gn * bevel_slope (tc), 1.0));
    rim[c] = max (dot (nc, l) - l.z, 0.0);
  }
  var light = (rim * 0.6 + vec3f (spec * 0.9 + fres * 0.3)) * in.fx.y * (1.0 - 0.7 * t);
  // The shaded side of the bevel darkens the tint.
  let shade = max (-facing, 0.0) * mix (0.35, 0.25, dark);

  // The edge line, one physical pixel: bright on the lit side; on light glass the shaded
  // side gets a faint dark line so the panel holds its shape over white.
  let line = clamp (1.0 - abs (d + 0.5), 0.0, 1.0);
  let lit = 0.5 + 0.5 * dot (gn, -normalize (l.xy));
  var edge_add = line * mix (0.08, 0.6, lit) * in.fx.y;
  var edge_dark = 0.0;
  if (dark < 0.5) {
    edge_add = line * mix (0.0, 0.5, lit) * in.fx.y;
    edge_dark = line * mix (0.22, 0.0, lit);
  }

  // Sheen: a soft diagonal band crossing once on show.
  var sheen = 0.0;
  if (in.anim.z >= 0.0) {
    let s = ((in.p.x - in.rect.x) + 0.6 * (in.p.y - in.rect.y)) / (in.rect.z + 0.6 * in.rect.w);
    let x = (s - (in.anim.z * 1.5 - 0.25)) / 0.08;
    sheen = exp (-x * x) * mix (0.30, 0.20, dark) * (1.0 - smoothstep (0.8, 1.0, in.anim.z));
  }

  // Orbiting rim: a bright head with a tail behind it, travelling clockwise along the border
  // at an even speed, on the edge line plus a short inner glow.
  var orbit = vec3f (0.0);
  if (in.anim.x >= 0.0) {
    var total = 4.0 * (half.x + half.y - 2.0 * r) + 6.2831853 * r;
    var at = 0.0;
    if (shaped) {
      total = in.outline.w * k;
      at = fract ((near.s - in.outline.z * k) / max (total, 1e-3));
    } else {
      at = border_param (q, half, r);
    }
    let behind = fract (in.anim.x - at) * total;
    let ahead = total - behind;
    let tail = 0.16 * total;
    let along = exp (-behind / tail) + exp (-pow (ahead / (3.0 * k), 2.0));
    let across = exp (-pow (inside / (1.6 * k), 2.0)) + 0.35 * exp (-pow (inside / (6.0 * k), 2.0));
    orbit = in.accent.rgb * along * across * in.anim.y;
  }
  // On light glass an added colour washes out to white, so the orbit paints its colour over
  // the tint instead.
  var paint = 0.0;
  if (dark < 0.5) {
    paint = clamp (max (orbit.r, max (orbit.g, orbit.b)) / max (max (in.accent.r, max (in.accent.g, in.accent.b)), 1e-3), 0.0, 1.0);
    orbit = vec3f (0.0);
  }

  // Compose: the tint as a premultiplied layer, light added on top like a reflection (it
  // adds coverage too), the dark edge multiplied in.
  let rgb = base * (1.0 - shade) * (1.0 - edge_dark);
  let add = light + vec3f (edge_add + sheen) + orbit;
  let add_a = clamp (max (add.r, max (add.g, add.b)), 0.0, 1.0);
  let a_panel = clamp (a_tint + add_a * (1.0 - a_tint) + edge_dark * (1.0 - a_tint), 0.0, 1.0);
  var panel = vec4f (min (rgb * a_tint + add, vec3f (a_panel)), a_panel);
  panel = mix (panel, vec4f (in.accent.rgb, 1.0), paint * 0.9) * cover;
  // The shadow goes under the panel's antialiased edge.
  panel = panel + vec4f (0.0, 0.0, 0.0, shadow) * (1.0 - panel.a);
  return finish (panel * opacity * glass_clip (in.clip.xy));
}
