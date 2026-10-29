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
//
// Refraction (EG-05): with `refract.x` 0 or more, the panel reads what the popup drew under
// it (`tex_backdrop`, the layer's backdrop) at an offset: magnified about the box centre,
// bent along the outline's normal by the profile, one offset per colour channel. The prelude
// also defines `glass_frag_px`, a fragment's target pixel. The textured pipeline swaps the
// stub helpers below for `material_tex.wgsl` (`material::with_textures`).

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
  // Outline (EG-06): first point, point count (0: the rounded rect; negative: the ellipse
  // filling the rect, drawn exactly, EG-36), arc length of the top centre and the perimeter,
  // in the units of the points. Two header entries sit before the first point (EG-38).
  @location(7) outline: vec4f,
  // Refraction (EG-05): profile (-1 off, 0 flat, 1 dome, 2 slab, 3 prism, 4 bubble), bend
  // (logical pixels), magnification, dispersion.
  @location(8) refract: vec4f,
  // Film amount, wobble (logical pixels), the prism's unit direction.
  @location(9) refract2: vec4f,
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
  @location(9) @interpolate(flat) refract: vec4f,
  @location(10) @interpolate(flat) refract2: vec4f,
};

// vo: texture helpers begin
fn tex_backdrop (px: vec2f) -> vec4f {
  return vec4f (0.0);
}

fn tex_image (slot: u32, uv: vec2f) -> vec4f {
  return vec4f (0.0);
}
// vo: texture helpers end

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

// `p` against the ellipse filling `rect` (EG-36): `outline_near` for `GlassShape::Ellipse`
// without the loop over every point. Its points (`o`, count `-o.y`) sit at even steps of the
// ellipse's angle, clockwise from the top. The nearest point on the true ellipse (a few fixed
// trig-free iterations, exact for a circle) gives its angle, and only the five edges of the
// polygon around that angle are tested, so the result is the polygon's, as before. More than
// `inner` pixels inside or `outer` outside, where the bevel, rim and edge line no longer
// reach, the true ellipse answers alone (within 0.1 px of the polygon) and no edge is read.
fn ellipse_near (p: vec2f, rect: vec4f, o: vec4f, k: f32, inner: f32, outer: f32, want_s: bool) -> Near {
  let ab = max (0.5 * rect.zw, vec2f (1e-3));
  let c = p - (rect.xy + 0.5 * rect.zw);
  let q = abs (c);
  var t = vec2f (0.70710678);
  let e2 = ab.x * ab.x - ab.y * ab.y;
  for (var j = 0; j < 3; j++) {
    let ev = vec2f (e2 / ab.x, -e2 / ab.y) * t * t * t;
    let rr = length (ab * t - ev);
    let qq = max (length (q - ev), 1e-6);
    t = clamp (((q - ev) * rr / qq + ev) / ab, vec2f (0.0), vec2f (1.0));
    t = t / max (length (t), 1e-6);
  }
  let sg = select (vec2f (-1.0), vec2f (1.0), c >= vec2f (0.0));
  let first = u32 (o.x);
  let count = u32 (-o.y);
  let f = fract ((atan2 (t.y * sg.y, t.x * sg.x) + 1.5707963) / 6.2831853) * f32 (count);
  let on = ab * t * sg;
  let de = select (1.0, -1.0, dot (c / ab, c / ab) < 1.0) * length (c - on);
  if (de < -inner || de > outer) {
    var r: Near;
    r.d = de;
    r.n = normalize (t * sg / ab);
    // Arc length between the two points around the nearest point's angle, for the orbit.
    r.s = 0.0;
    if (want_s) {
      let i = min (u32 (f), count - 1u);
      var s1 = o.w;
      if (i + 1u < count) {
        s1 = glass_point (first + i + 1u).z;
      }
      r.s = mix (glass_point (first + i).z, s1, fract (f)) * k;
    }
    return r;
  }
  let origin = glass_origin ();
  var best = 1e30;
  var bv = vec2f (0.0);
  var be = vec2f (1.0, 0.0);
  var bs = 0.0;
  // Inside a convex polygon: on the inner side of every edge near the nearest point.
  var inside = true;
  // Edge `i` runs from point i - 1 to point i, as in `outline_near`.
  var ia = (min (u32 (f), count - 1u) + count - 2u) % count;
  var a4 = glass_point (first + ia);
  var a = a4.xy * k + origin;
  for (var j = 0u; j < 5u; j++) {
    let ib = (ia + 1u) % count;
    let b4 = glass_point (first + ib);
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
      bs = a4.z * k + h * length (e);
    }
    // Clockwise on screen (y down): inside is to the right of each edge.
    if (e.x * w.y - e.y * w.x < 0.0) {
      inside = false;
    }
    ia = ib;
    a4 = b4;
    a = b;
  }
  let sgn = select (1.0, -1.0, inside);
  let dist = sqrt (best);
  var r: Near;
  r.d = sgn * dist;
  if (dist > 1e-3) {
    r.n = sgn * bv / dist;
  } else {
    r.n = normalize (vec2f (be.y, -be.x));
  }
  r.s = bs;
  return r;
}

// Point `j` (any integer, taken round the loop) of the outline `o`, scaled into the panel
// frame, with its arc length.
fn ring_point (o: vec4f, j: i32, k: f32) -> vec3f {
  let n = i32 (o.y);
  let i = u32 (((j % n) + n) % n);
  let a = glass_point (u32 (o.x) + i);
  return vec3f (a.xy * k + glass_origin (), a.z * k);
}

// The edges of `o` that end at points `lo` to `lo + len - 1` (an edge ends at point `j` and
// starts at `j - 1`), folded into the nearest one so far: `best` (squared distance), `bj`
// (its end point), `bh` (where along it), `bv` (from it to `p`), `be` (the edge), `bs` (arc
// length).
struct Window {
  best: f32,
  bj: i32,
  bh: f32,
  bv: vec2f,
  be: vec2f,
  bs: f32,
};

fn window_scan (p: vec2f, o: vec4f, k: f32, lo: i32, len: i32, w0: Window) -> Window {
  var w = w0;
  let n = u32 (o.y);
  let first = u32 (o.x);
  let origin = glass_origin ();
  // Walk the points by index, wrapping without a division per point.
  var i = u32 ((((lo - 1) % i32 (n)) + i32 (n)) % i32 (n));
  var a4 = glass_point (first + i);
  var a = a4.xy * k + origin;
  for (var j = lo; j < lo + len; j++) {
    i += 1u;
    if (i == n) {
      i = 0u;
    }
    let b4 = glass_point (first + i);
    let b = b4.xy * k + origin;
    let e = b - a;
    let v0 = p - a;
    let h = clamp (dot (v0, e) / max (dot (e, e), 1e-12), 0.0, 1.0);
    let v = v0 - e * h;
    let d2 = dot (v, v);
    if (d2 < w.best) {
      w.best = d2;
      w.bj = j;
      w.bh = h;
      w.bv = v;
      w.be = e;
      w.bs = a4.z * k + h * length (e);
    }
    a4 = b4;
    a = b;
  }
  return w;
}

// Outward unit normal of the edge `e` (clockwise on screen, y down: the edge turned left).
fn edge_out (e: vec2f) -> vec2f {
  return vec2f (e.y, -e.x) / max (length (e), 1e-12);
}

// `outline_near` for a long outline running once round the rounded box `S` (half size
// `hs`, corner radius `rho`, `q` the pixel from its centre, `sd` its distance), testing only
// the edges whose points can be nearest (EG-38). `h1` is the second header entry of
// `outline_header` in glass.rs. The edges near the pixel's place on `S` give a first
// distance; any nearer edge has an end point whose place on `S` is within `rw` of the pixel,
// which bounds how far round `S` (and so how many points) to look. Inside or outside comes
// from the nearest edge, or at a corner point from both its edges (the polygon is simple).
fn window_near (p: vec2f, q: vec2f, hs: vec2f, rho: f32, sd: f32, o: vec4f, h1: vec4f, k: f32) -> Near {
  let n = f32 (o.y);
  let hl = hs - vec2f (rho);
  let per = 4.0 * (hl.x + hl.y) + 6.2831853 * rho;
  let i0 = i32 (floor (border_param (q, hs, rho) * n - h1.x));
  var w: Window;
  w.best = 1e30;
  w = window_scan (p, o, k, i0 - 1, 3, w);
  let rw = sqrt (w.best) + h1.z * k;
  let delta = abs (sd);
  let x = abs (q);
  // How far round S the end points can sit: on a straight side whose ends and opposite side
  // are all beyond `rw`, along that side only; else a chord bound (an arc of S is at most
  // pi / 2 times its chord when the chord is shorter than the box).
  var arc = -1.0;
  if (!(x.x > hl.x && x.y > hl.y)) {
    var along = hl.y - x.y;
    var across = 2.0 * hs.x;
    if (hs.y - x.y <= hs.x - x.x) {
      along = hl.x - x.x;
      across = 2.0 * hs.y;
    }
    if (along > rw && across - delta > rw) {
      arc = sqrt (max (rw * rw - delta * delta, 0.0));
    }
  }
  if (arc < 0.0 && delta + rw < 2.0 * min (hs.x, hs.y)) {
    arc = 1.6 * (delta + rw);
  }
  let span = i32 (ceil (arc / per * n + h1.y)) + 1;
  if (arc < 0.0 || 2 * span + 2 >= i32 (o.y)) {
    return outline_near (p, o, k);
  }
  w = window_scan (p, o, k, i0 - span, 2 * span + 2, w);
  var nrm = edge_out (w.be);
  if (w.bh <= 0.0) {
    // The edge's start point: add the edge before it.
    nrm += edge_out (ring_point (o, w.bj - 1, k).xy - ring_point (o, w.bj - 2, k).xy);
  } else if (w.bh >= 1.0) {
    nrm += edge_out (ring_point (o, w.bj + 1, k).xy - ring_point (o, w.bj, k).xy);
  }
  let sgn = select (-1.0, 1.0, dot (w.bv, nrm) > 0.0);
  let dist = sqrt (w.best);
  var r: Near;
  r.d = sgn * dist;
  if (dist > 1e-3) {
    r.n = sgn * w.bv / dist;
  } else {
    r.n = edge_out (w.be);
  }
  r.s = w.bs;
  return r;
}

// `p` against a polygon outline (EG-38). Its header (two entries before its first point, see
// `outline_header` in glass.rs) holds a rounded box `S` and `g`: the pixel is at least
// `g - sd_S` inside. Past `deep` the exact distance no longer shows, so that bound stands in
// for it with `S`'s normal and no edge is read. Otherwise a long outline tests the edges
// near the pixel (`window_near`) and a short one all of them.
fn poly_near (p: vec2f, rect: vec4f, o: vec4f, k: f32, deep: f32, outside: f32) -> Near {
  let first = u32 (o.x);
  let h0 = glass_point (first - 2u);
  let h1 = glass_point (first - 1u);
  let half = 0.5 * rect.zw;
  let hs = half - h0.xy * k;
  let rho = h0.z * k;
  let q = p - (rect.xy + half);
  let sd = sd_box (q, hs, rho);
  if (h0.w * k - sd > deep) {
    var r: Near;
    r.d = sd - h0.w * k;
    r.n = grad_box (q, hs, rho);
    r.s = 0.0;
    return r;
  }
  // Likewise past `outside` out, where only the coverage reads it (zero either way); the
  // outline keeps within `h1.w` of `S`.
  if (sd - h1.w * k > outside) {
    var r: Near;
    r.d = sd - h1.w * k;
    r.n = grad_box (q, hs, rho);
    r.s = 0.0;
    return r;
  }
  if (h1.y < 0.0) {
    return outline_near (p, o, k);
  }
  return window_near (p, q, hs, rho, sd, o, h1, k);
}

// `poly_near` or, for the ellipse (a negative count), `ellipse_near` with its exact band
// (`inner`, `outer`); `deep` and `outside` are how far in and out the polygon's exact
// distance still shows, and `want_s` asks for the ellipse's arc length.
fn shape_near (p: vec2f, rect: vec4f, o: vec4f, k: f32, inner: f32, outer: f32, deep: f32, outside: f32, want_s: bool) -> Near {
  if (o.y < 0.0) {
    return ellipse_near (p, rect, o, k, inner, outer, want_s);
  }
  return poly_near (p, rect, o, k, deep, outside);
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

// The refraction offset at `p` in the panel frame (physical pixels), before dispersion:
// where the backdrop is read, relative to `p`. `inside` is the distance in from the edge,
// `gn` the outward normal, `centre` the box centre, `k` physical pixels per logical pixel;
// `rf` and `rf2` the instance's refraction blocks, `bevel` its bevel width (logical pixels)
// and `time` its age.
fn refract_offset (rf: vec4f, rf2: vec4f, bevel: f32, time: f32, p: vec2f, inside: f32, gn: vec2f, centre: vec2f, half: vec2f, k: f32) -> vec2f {
  let profile = rf.x;
  let bend = rf.y * k;
  // Magnification about the centre: read nearer to it.
  var off = (centre - p) * (1.0 - 1.0 / rf.z);
  let bw = max (bevel * k, 1.0);
  if (profile > 0.5 && profile < 1.5) {
    // Dome: the slope rises over the bevel band towards the edge.
    let t = clamp (inside / bw, 0.0, 1.0);
    off -= gn * bend * (1.0 - t) * (1.0 - t);
  } else if (profile > 1.5 && profile < 2.5) {
    // Slab: flat on top, the bevel's quarter circle at the edge.
    let t = clamp (inside / bw, 0.0, 1.0);
    off -= gn * bend * bevel_slope (t) / 6.0;
  } else if (profile > 2.5 && profile < 3.5) {
    // Prism: one tilt over the whole face.
    off += rf2.zw * bend;
  } else if (profile > 3.5) {
    // Bubble: a sphere cap over the whole shape, steepest at the rim.
    let t = clamp (inside / max (min (half.x, half.y), 1.0), 0.0, 1.0);
    off -= gn * bend * (1.0 - t) * (1.0 - t);
  }
  let wobble = rf2.y * k;
  if (wobble != 0.0) {
    // A slow wave over the face: a pure function of the panel's age.
    let q = (p - centre) / k;
    off += wobble * vec2f (sin (q.y * 0.09 + time * 1.7), cos (q.x * 0.08 + time * 1.3));
  }
  return off;
}

// The backdrop under fragment `frag` read through the glass: each channel at the offset
// `off` (panel frame) scaled by 1 - d, 1 and 1 + d, turned onto the screen.
fn refract_read (frag: vec2f, off: vec2f, disp: f32) -> vec4f {
  let at = glass_frag_px (frag);
  let turn = glass_turn ();
  let r = tex_backdrop (at + turn * (off * (1.0 - disp)));
  let g = tex_backdrop (at + turn * off);
  let b = tex_backdrop (at + turn * (off * (1.0 + disp)));
  return vec4f (r.r, g.g, b.b, max (g.a, max (r.a, b.a)));
}

// Thin-film colour (EG-05) for a film thickness `th` (about 0 to 1): interference bands.
fn film_colour (th: f32) -> vec3f {
  return 0.5 + 0.5 * cos (6.2831853 * (2.0 * th + vec3f (0.0, 0.33, 0.67)));
}

@fragment fn fs_glass (in: GlassOut) -> @location(0) vec4f {
  let k = in.shape.y;
  let half = 0.5 * in.rect.zw;
  let q = in.p - (in.rect.xy + half);
  let r = min (in.shape.x, min (half.x, half.y));
  let shaped = in.outline.y != 0.0;
  // Distance, outward normal and border position: exact for the rounded rect, from the
  // polygon for an outline.
  var d: f32;
  var gn: vec2f;
  var near: Near;
  // How far in the outline's own shape still shows (EG-36): the bevel and its dispersion,
  // and with an orbit its inner glow; everywhere for the bubble, which bends along the
  // outline's normal over the whole face.
  var exact_in = (max (in.fx.x, 1.0) + 3.0 * abs (in.fx.w)) * k + 2.0;
  if (in.anim.x >= 0.0) {
    exact_in = max (exact_in, 30.0 * k + 2.0);
  }
  if (in.refract.x > 3.5) {
    exact_in = 1e30;
  }
  // Deeper still a polygon's distance stands in as a bound (EG-38): past the tint's 12 px
  // ramp too, and nowhere under the thin film, which shades the whole face.
  var deep_in = max (exact_in, 12.0 * k + 2.0);
  if (in.refract2.x > 0.0) {
    deep_in = 1e30;
  }
  if (shaped) {
    // Past this far out the pixel is uncovered and nothing but the shadow reads its distance.
    let uncovered = 0.5 / max (glass_aa (in.clip.xy), 1e-3) + 1.0;
    near = shape_near (in.p, in.rect, in.outline, k, exact_in, 1.0, deep_in, uncovered, in.anim.x >= 0.0);
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
  // would darken the backdrop the blur shows. Fully covered pixels skip the second
  // distance (EG-36): their shadow is zero anyway.
  var shadow = 0.0;
  if (in.shadow.x > 0.0 && cover < 1.0) {
    let drop = turn * vec2f (0.0, in.shadow.z * k);
    var ds: f32;
    if (shaped) {
      ds = shape_near (in.p - drop, in.rect, in.outline, k, exact_in, 1.0, deep_in, 1e30, false).d;
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
  // Thin film (EG-05): colour bands by a thickness that thins towards the rim, drifting only
  // while the panel wobbles; strongest at the rim and none at the centre.
  if (in.refract2.x > 0.0) {
    let rr = clamp (inside / max (min (half.x, half.y), 1.0), 0.0, 1.0);
    var drift = 0.0;
    if (in.refract2.y != 0.0) {
      drift = 0.08 * sin (in.shape.z * 0.9 + (in.p.x - in.p.y) / (60.0 * k));
    }
    let th = 0.25 + 0.6 * rr + drift;
    // Nothing at the centre, where text sits (AX01 counts little of the glass's own light).
    let rim_w = pow (1.0 - rr, 1.5);
    light += film_colour (th) * in.refract2.x * rim_w * 0.3;
  }
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
  panel = mix (panel, vec4f (in.accent.rgb, 1.0), paint * 0.9);
  if (in.refract.x >= 0.0) {
    // The panel over the backdrop read through it (EG-05). Over opaque drawing this covers
    // the unmoved backdrop under it exactly.
    let centre = in.rect.xy + half;
    let off = refract_offset (in.refract, in.refract2, in.fx.x, in.shape.z, in.p, inside, gn, centre, half, k);
    let seen = refract_read (in.clip.xy, off, in.refract.w);
    panel = panel + seen * (1.0 - panel.a);
  }
  panel = panel * cover;
  // The shadow goes under the panel's antialiased edge.
  panel = panel + vec4f (0.0, 0.0, 0.0, shadow) * (1.0 - panel.a);
  return finish (panel * opacity * glass_clip (in.clip.xy));
}
