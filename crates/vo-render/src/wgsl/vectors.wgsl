// Vector strokes and blobs (`vector.rs`, `vo_scene::path`). `common.wgsl` is prepended.
//
// One instance is one contour: a quad over its bounding box in physical pixels, and a
// fragment loop over its segments in the storage buffer that keeps the nearest one. Taking the
// minimum distance gives round joins and caps, and a translucent stroke covers each pixel
// once. Everything is in physical pixels; the CPU has already mapped the segments through the
// layer transform, so antialiasing is a fixed one-pixel ramp with no derivatives.

const KIND_STROKE: u32 = 0u;
const KIND_BLOB: u32 = 1u;

// Two texels per segment: p0.xy p1.xy, then p2.xy s0 s1.
@group(0) @binding(0) var<storage, read> segs: array<vec4f>;

struct VecIn {
  // kind, first segment, segment count, unused.
  @location(0) head: vec4u,
  // Bounding box in physical pixels: x0, y0, x1, y1.
  @location(1) bbox: vec4f,
  // Half width (px), glow radius (px), colour ramp start and end (arc length).
  @location(2) params: vec4f,
  // Linear light, premultiplied.
  @location(3) c0: vec4f,
  @location(4) c1: vec4f,
  // Blob circles in px: cx, cy, r, k.
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

fn ramp (s: f32, r: vec2f) -> f32 {
  if (r.y <= r.x) {
    return select (0.0, 1.0, s >= r.x);
  }
  return clamp ((s - r.x) / (r.y - r.x), 0.0, 1.0);
}

@fragment fn fs_vector (in: VecOut) -> @location(0) vec4f {
  // Target pixels; a layer texture sized to its bounds sits at `layer.origin`.
  let p = in.clip.xy + layer.origin.xy;
  var col: vec4f;
  var cov: f32;
  var dist: f32;
  if (in.head.x == KIND_BLOB) {
    let d1 = length (p - in.a.xy) - in.a.z;
    let d2 = length (p - in.b.xy) - in.b.z;
    let k = max (in.a.w, 1e-3);
    let h = clamp (0.5 + 0.5 * (d2 - d1) / k, 0.0, 1.0);
    dist = mix (d2, d1, h) - k * h * (1.0 - h);
    cov = clamp (0.5 - dist, 0.0, 1.0);
    col = mix (in.c1, in.c0, h);
    dist = max (dist, 0.0);
  } else {
    var best = vec2f (1e30, 0.0);
    let end = in.head.y + in.head.z;
    for (var i = in.head.y; i < end; i++) {
      let t0 = segs[2u * i];
      let t1 = segs[2u * i + 1u];
      let n = quad_nearest (p, t0.xy, t0.zw, t1.xy);
      if (n.x < best.x) {
        best = vec2f (n.x, mix (t1.z, t1.w, n.y));
      }
    }
    cov = clamp (in.params.x - best.x + 0.5, 0.0, 1.0);
    col = mix (in.c0, in.c1, ramp (best.y, in.params.zw));
    dist = max (best.x - in.params.x, 0.0);
  }
  var out = col * cov;
  if (in.params.y > 0.0) {
    let g = dist / in.params.y;
    out += col * (0.6 * exp (-g * g) * (1.0 - cov));
  }
  return finish (out * clip_coverage (in.clip.xy));
}
