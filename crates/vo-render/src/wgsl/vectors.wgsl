// Vector strokes, fills and blobs (`vector.rs`, `vo_scene::path`). `common.wgsl` is prepended.
//
// One instance is one contour (a stroke) or one set of contours (a fill): a quad over the
// bounding box in target pixels, and a fragment loop over the segments in the storage buffer.
// A stroke keeps the segment that covers the pixel most, so joins and caps are round and a
// translucent stroke covers each pixel once. A fill counts windings along a ray for inside or
// outside and takes the nearest segment for the antialiased edge. Everything is in target
// pixels; the CPU has already mapped the segments through the layer transform, so
// antialiasing is a fixed one-pixel ramp with no derivatives.

const KIND_STROKE: u32 = 0u;
const KIND_BLOB: u32 = 1u;
const KIND_FILL: u32 = 2u;

const PAINT_ALONG: u32 = 0u;
const PAINT_LINEAR: u32 = 1u;
const PAINT_CONIC: u32 = 2u;
const PAINT_CONIC_MIRROR: u32 = 3u;

// Three texels per segment: p0.xy p1.xy, then p2.xy s0 s1, then the width factors w0 w1.
@group(0) @binding(0) var<storage, read> segs: array<vec4f>;

struct VecIn {
  // kind, first segment, segment count, paint kind.
  @location(0) head: vec4u,
  // Bounding box in target pixels: x0, y0, x1, y1.
  @location(1) bbox: vec4f,
  // Half width (px), glow radius (px), colour ramp start and end.
  @location(2) params: vec4f,
  // OKLab and straight alpha.
  @location(3) c0: vec4f,
  @location(4) c1: vec4f,
  // Blob circles in px (cx, cy, r, k), or the paint geometry in `a`.
  @location(5) a: vec4f,
  @location(6) b: vec4f,
};

struct VecOut {
  @builtin(position) clip: vec4f,
  @location(0) @interpolate(flat) head: vec4u,
  @location(1) @interpolate(flat) params: vec4f,
  @location(2) @interpolate(flat) c0: vec4f,
  @location(3) @interpolate(flat) c1: vec4f,
  @location(4) @interpolate(flat) a: vec4f,
  @location(5) @interpolate(flat) b: vec4f,
};

@vertex fn vs_vector (@builtin(vertex_index) vi: u32, v: VecIn) -> VecOut {
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  var o: VecOut;
  o.clip = px_to_ndc (mix (v.bbox.xy, v.bbox.zw, c));
  o.head = v.head;
  o.params = v.params;
  o.c0 = v.c0;
  o.c1 = v.c1;
  o.a = v.a;
  o.b = v.b;
  return o;
}

fn bez (A: vec2f, a: vec2f, b: vec2f, t: f32) -> vec2f {
  return A + (2.0 * a + b * t) * t;
}

// Distance from `p` to the quadratic A, B, C and the curve parameter of the nearest point.
// Straight segments (B at the midpoint) take the line formula. Curved ones solve the cubic
// in closed form for a seed, then take one Newton step, which removes the ripples the
// closed form shows near a zero discriminant in f32.
fn quad_nearest (p: vec2f, A: vec2f, B: vec2f, C: vec2f) -> vec2f {
  let a = B - A;
  let b = A - 2.0 * B + C;
  let chord = C - A;
  let cc = max (dot (chord, chord), 1e-12);
  if (dot (b, b) < 1e-6 * cc) {
    let t = clamp (dot (p - A, chord) / cc, 0.0, 1.0);
    return vec2f (length (p - (A + chord * t)), t);
  }
  let d = A - p;
  let kk = 1.0 / dot (b, b);
  let kx = kk * dot (a, b);
  let ky = kk * (2.0 * dot (a, a) + dot (d, b)) / 3.0;
  let kz = kk * dot (d, a);
  let pp = ky - kx * kx;
  let q = kx * (2.0 * kx * kx - 3.0 * ky) + kz;
  let h = q * q + 4.0 * pp * pp * pp;
  var cand = vec3f (0.0, 1.0, 0.0);
  if (h >= 0.0) {
    let sh = sqrt (h);
    let x = (vec2f (sh, -sh) - q) * 0.5;
    let uv = sign (x) * pow (abs (x), vec2f (1.0 / 3.0));
    cand.z = uv.x + uv.y - kx;
  } else {
    let z = sqrt (-pp);
    let v = acos (clamp (q / (pp * z * 2.0), -1.0, 1.0)) / 3.0;
    let m = cos (v);
    let n = sin (v) * 1.732050808;
    cand.x = (m + m) * z - kx;
    cand.z = (-n - m) * z - kx;
  }
  var best = vec2f (1e30, 0.0);
  for (var i = 0; i < 3; i++) {
    var t = clamp (cand[i], 0.0, 1.0);
    // Newton on f(t) = (B(t) - p) . B'(t).
    let r = bez (A, a, b, t) - p;
    let d1 = 2.0 * (a + b * t);
    let f = dot (r, d1);
    let df = dot (d1, d1) + dot (r, 2.0 * b);
    if (abs (df) > 1e-12) {
      t = clamp (t - f / df, 0.0, 1.0);
    }
    let dist = length (bez (A, a, b, t) - p);
    if (dist < best.x) {
      best = vec2f (dist, t);
    }
  }
  for (var e = 0; e < 2; e++) {
    let t = f32 (e);
    let dist = length (bez (A, a, b, t) - p);
    if (dist < best.x) {
      best = vec2f (dist, t);
    }
  }
  return best;
}

// Crossing of the ray from `p` towards +x with one y-monotonic piece of the quadratic,
// from parameter ta to tb. Half open in y (an upward piece counts its start and not its end,
// a downward piece the reverse), so a ray through a shared vertex counts it exactly once,
// and twice or not at all where the outline only touches the ray.
fn piece_winding (p: vec2f, A: vec2f, a: vec2f, b: vec2f, ta: f32, tb: f32) -> i32 {
  let ya = A.y + (2.0 * a.y + b.y * ta) * ta;
  let yb = A.y + (2.0 * a.y + b.y * tb) * tb;
  let up = ya <= p.y && p.y < yb;
  let down = yb <= p.y && p.y < ya;
  if (!(up || down)) {
    return 0;
  }
  // y(t) = p.y on [ta, tb].
  var t: f32;
  if (abs (b.y) < 1e-7) {
    t = (p.y - A.y) / (2.0 * a.y);
  } else {
    let disc = max (a.y * a.y - b.y * (A.y - p.y), 0.0);
    let r = sqrt (disc);
    let t1 = (-a.y - r) / b.y;
    let t2 = (-a.y + r) / b.y;
    let mid = 0.5 * (ta + tb);
    t = select (t2, t1, abs (t1 - mid) <= abs (t2 - mid));
  }
  t = clamp (t, min (ta, tb), max (ta, tb));
  let x = A.x + (2.0 * a.x + b.x * t) * t;
  if (x <= p.x) {
    return 0;
  }
  return select (-1, 1, up);
}

// Signed crossings of the ray from `p` towards +x with the quadratic A, B, C, split at its
// y extremum into monotonic pieces.
fn quad_winding (p: vec2f, A: vec2f, B: vec2f, C: vec2f) -> i32 {
  let a = B - A;
  let b = A - 2.0 * B + C;
  var te = 2.0;
  if (abs (b.y) > 1e-7) {
    te = -a.y / b.y;
  }
  if (te > 0.0 && te < 1.0) {
    return piece_winding (p, A, a, b, 0.0, te) + piece_winding (p, A, a, b, te, 1.0);
  }
  return piece_winding (p, A, a, b, 0.0, 1.0);
}

fn ramp (u: f32, r: vec2f) -> f32 {
  if (r.y <= r.x) {
    return select (0.0, 1.0, u >= r.x);
  }
  return clamp ((u - r.x) / (r.y - r.x), 0.0, 1.0);
}

fn lab_to_linear (lab: vec3f) -> vec3f {
  let l_ = lab.x + 0.3963377774 * lab.y + 0.2158037573 * lab.z;
  let m_ = lab.x - 0.1055613458 * lab.y - 0.0638541728 * lab.z;
  let s_ = lab.x - 0.0894841775 * lab.y - 1.2914855480 * lab.z;
  let l = l_ * l_ * l_;
  let m = m_ * m_ * m_;
  let s = s_ * s_ * s_;
  return vec3f (
     4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s);
}

// Premultiplied linear colour at ramp position `k`, mixed as premultiplied OKLab.
fn mix_color (c0: vec4f, c1: vec4f, k: f32) -> vec4f {
  let lab = mix (vec4f (c0.xyz * c0.w, c0.w), vec4f (c1.xyz * c1.w, c1.w), k);
  if (lab.w <= 0.0) {
    return vec4f (0.0);
  }
  let rgb = clamp (lab_to_linear (lab.xyz / lab.w), vec3f (0.0), vec3f (1.0));
  return vec4f (rgb * lab.w, lab.w);
}

// Where `p` sits along the paint: arc length `s`, or the linear or conic position.
fn paint_pos (kind: u32, p: vec2f, s: f32, g: vec4f) -> f32 {
  if (kind == PAINT_LINEAR) {
    let d = g.zw - g.xy;
    return dot (p - g.xy, d) / max (dot (d, d), 1e-12);
  }
  if (kind == PAINT_CONIC || kind == PAINT_CONIC_MIRROR) {
    let q = p - g.xy;
    let turn = fract ((atan2 (q.y, q.x) - g.z) / 6.283185307);
    if (kind == PAINT_CONIC_MIRROR) {
      return 1.0 - abs (2.0 * turn - 1.0);
    }
    return turn;
  }
  return s;
}

@fragment fn fs_vector (in: VecOut) -> @location(0) vec4f {
  // Target pixels; a layer texture sized to its bounds sits at `layer.origin`.
  let p = frag_target (in.clip.xy);
  // Pixels as seen per target pixel as laid out, for the antialiasing ramps.
  let seen = warp_scale (p);
  var cov: f32;
  // Distance outside the shape's edge, for the glow.
  var outside: f32;
  var k: f32;
  if (in.head.x == KIND_BLOB) {
    let d1 = length (p - in.a.xy) - in.a.z;
    let d2 = length (p - in.b.xy) - in.b.z;
    let kb = max (in.a.w, 1e-3);
    let h = clamp (0.5 + 0.5 * (d2 - d1) / kb, 0.0, 1.0);
    let dist = mix (d2, d1, h) - kb * h * (1.0 - h);
    cov = clamp (0.5 - dist * seen, 0.0, 1.0);
    outside = max (dist, 0.0);
    k = 1.0 - h;
  } else {
    let hw = in.params.x;
    let fill = in.head.x == KIND_FILL;
    // Best covering margin (half width minus distance) and the arc length there.
    var best = vec2f (-1e30, 0.0);
    var winding = 0;
    let end = in.head.y + in.head.z;
    for (var i = in.head.y; i < end; i++) {
      let t0 = segs[3u * i];
      let t1 = segs[3u * i + 1u];
      let n = quad_nearest (p, t0.xy, t0.zw, t1.xy);
      var w = hw;
      if (!fill) {
        let t2 = segs[3u * i + 2u];
        w = hw * mix (t2.x, t2.y, n.y);
      }
      let margin = w - n.x;
      if (margin > best.x) {
        best = vec2f (margin, mix (t1.z, t1.w, n.y));
      }
      if (fill) {
        winding += quad_winding (p, t0.xy, t0.zw, t1.xy);
      }
    }
    var signed = -best.x;
    if (fill && winding != 0) {
      signed = best.x;
    }
    cov = clamp (0.5 - signed * seen, 0.0, 1.0);
    outside = max (signed, 0.0);
    k = ramp (paint_pos (in.head.w, p, best.y, in.a), in.params.zw);
  }
  let col = mix_color (in.c0, in.c1, k);
  var out = col * cov;
  if (in.params.y > 0.0) {
    let g = outside / in.params.y;
    out += col * (0.6 * exp (-g * g) * (1.0 - cov));
  }
  return finish (out * clip_coverage (in.clip.xy));
}
