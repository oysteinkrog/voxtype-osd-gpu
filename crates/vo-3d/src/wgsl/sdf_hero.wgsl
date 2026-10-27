// One raymarched hero object (a chrome blob, an aurora orb, a jellyfish bell) drawn straight
// over the surface, premultiplied over. Space is surface pixels, y down, z toward the viewer;
// the eye is the stage camera's. The quad covers only the object's bounds, and each ray
// marches only between the entry and exit of a bounding sphere, at most `march.y` times.

struct Hero {
  // Quad x, y, w, h in surface pixels.
  rect: vec4f,
  // Surface width and height in pixels, opacity, 1 to encode sRGB here.
  viewport: vec4f,
  // Eye position (stage origin, eye distance in front), surface pixels.
  eye: vec4f,
  // Object centre in surface pixels (z 0); radius in surface pixels.
  centre: vec4f,
  // Shape (0 orb, 1 blob, 2 bell), wobble 0..1, phase in seconds, spin in radians.
  shape: vec4f,
  // Lobes (blob), max steps, bounding radius in radii (halo included), halo width in radii.
  march: vec4f,
  // Stretch of the shape along x, y, z; material (0 chrome, 1 glow).
  stretch: vec4f,
  // Chrome: reflection tint, studio top, bottom and lights. Glow: three body colours and the
  // rim. Linear, straight alpha; `a` of the first is the body's opacity (glow), of the
  // second the halo strength.
  c0: vec4f,
  c1: vec4f,
  c2: vec4f,
  c3: vec4f,
};

@group(0) @binding(0) var<uniform> hero: Hero;

@vertex fn vs_hero (@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  let px = hero.rect.xy + c * hero.rect.zw;
  let n = px / hero.viewport.xy * 2.0 - 1.0;
  return vec4f (n.x, -n.y, 0.0, 1.0);
}

fn smin (a: f32, b: f32, k: f32) -> f32 {
  let h = clamp (0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
  return mix (b, a, h) - k * h * (1.0 - h);
}

fn smax (a: f32, b: f32, k: f32) -> f32 {
  return -smin (-a, -b, k);
}

fn spin_y (p: vec3f, a: f32) -> vec3f {
  let s = sin (a);
  let c = cos (a);
  return vec3f (c * p.x + s * p.z, p.y, -s * p.x + c * p.z);
}

// Distance in radii from `q0` (object space: radius 1, y down) to the surface. Stretch
// divides space, so the distance is scaled by the smallest stretch to stay a lower bound.
fn sdf (q0: vec3f) -> f32 {
  let st = hero.stretch.xyz;
  let q = spin_y (q0, hero.shape.w) / st;
  let wobble = hero.shape.y;
  let ph = hero.shape.z;
  var d: f32;
  if (hero.shape.x < 0.5) {
    d = length (q) - 1.0;
  } else if (hero.shape.x < 1.5) {
    // Lobes circling the centre, melted together.
    let n = u32 (hero.march.x);
    d = 1e9;
    for (var i = 0u; i < n; i++) {
      let a = f32 (i) * 6.2831853 / f32 (n) + ph * (0.6 + 0.17 * f32 (i));
      let o = vec3f (cos (a), 0.5 * sin (1.3 * a + ph), sin (a)) * (0.34 + 0.14 * wobble);
      d = smin (d, length (q - o) - 0.6, 0.45);
    }
  } else {
    // A bell: a dome cut open below, hollow underneath, breathing with the phase.
    let breathe = 1.0 + 0.06 * sin (2.4 * ph) + 0.08 * wobble;
    let b = q / vec3f (breathe, 1.0 / breathe, breathe);
    let dome = length (b * vec3f (1.0, 1.25, 1.0)) - 1.0;
    // A scalloped hem that sways.
    let hem = 0.3 + 0.05 * sin (8.0 * atan2 (b.z, b.x) + 1.5 * ph);
    let cut = b.y - hem;
    let hollow = length ((b - vec3f (0.0, 0.55, 0.0)) * vec3f (1.05, 1.4, 1.05)) - 0.9;
    d = smax (smax (dome, cut, 0.12), -hollow, 0.08) * 0.8;
  }
  // Ripples that grow with the wobble (a voice level).
  let r = sin (5.0 * q.x + 1.7 * ph) * sin (4.0 * q.y - 1.3 * ph) * sin (4.5 * q.z + 0.9 * ph);
  return (d + 0.06 * wobble * r) * min (st.x, min (st.y, st.z));
}

fn normal_at (q: vec3f) -> vec3f {
  let e = vec2f (1.0, -1.0) * 0.0015;
  return normalize (e.xyy * sdf (q + e.xyy) + e.yyx * sdf (q + e.yyx) + e.yxy * sdf (q + e.yxy) + e.xxx * sdf (q + e.xxx));
}

// A studio around a chrome object: a vertical gradient, a bright horizon and two soft light
// strips. `r` is in surface space, so up is -y.
fn studio (r: vec3f) -> vec3f {
  let up = -r.y;
  var c = mix (hero.c2.rgb, hero.c1.rgb, smoothstep (-0.6, 0.8, up));
  c += hero.c3.rgb * 0.6 * exp (-pow (up / 0.05, 2.0));
  let strip = exp (-pow ((up - 0.45) / 0.08, 2.0)) * smoothstep (0.95, 0.2, abs (r.x + 0.15));
  let side = exp (-pow ((r.x - 0.75) / 0.07, 2.0)) * smoothstep (-0.4, 0.3, up) * step (0.0, r.z);
  let low = exp (-pow ((up + 0.35) / 0.12, 2.0)) * 0.35;
  return c + hero.c3.rgb * (2.4 * strip + 1.6 * side + low);
}

fn encode (c: vec3f) -> vec3f {
  let lo = c * 12.92;
  let hi = 1.055 * pow (max (c, vec3f (0.0)), vec3f (1.0 / 2.4)) - 0.055;
  return select (hi, lo, c <= vec3f (0.0031308));
}

// Premultiplied linear `pm` with alpha `a`, times the opacity, encoded for the target. Light
// with no alpha (a halo) adds over what is below.
fn finish (pm: vec3f, a: f32) -> vec4f {
  let k = hero.viewport.z;
  var c = pm * k;
  if (hero.viewport.w > 0.5) {
    c = select (encode (c), encode (c / (a * k)) * a * k, a > 1e-4);
  }
  return vec4f (c, a * k);
}

@fragment fn fs_hero (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let eye = hero.eye.xyz;
  let dir = normalize (vec3f (frag.xy, 0.0) - eye);
  let rad = hero.centre.w;
  let o = (eye - hero.centre.xyz) / rad;
  let bound = hero.march.z;
  let b = dot (o, dir);
  let disc = b * b - (dot (o, o) - bound * bound);
  if (disc <= 0.0) {
    discard;
  }
  let s = sqrt (disc);
  var t = max (-b - s, 0.0);
  let t_end = -b + s;
  // One pixel at the object, in radii: the hit threshold and the edge's antialiasing.
  let px = length (eye - hero.centre.xyz) / (hero.eye.z * rad);
  var near = 1e9;
  var t_near = t;
  var hit = false;
  let steps = u32 (hero.march.y);
  for (var i = 0u; i < steps; i++) {
    let d = sdf (o + dir * t);
    if (d < near) {
      near = d;
      t_near = t;
    }
    if (d < 0.25 * px) {
      hit = true;
      break;
    }
    t += max (d * 0.9, 0.5 * px);
    if (t > t_end) {
      break;
    }
  }
  let glow = hero.stretch.w > 0.5;
  let cover = select (clamp (1.0 - near / px, 0.0, 1.0), 1.0, hit);
  // The halo around a glowing object, from how near the ray passed.
  var halo = vec4f (0.0);
  if (glow && hero.march.w > 0.0) {
    let k = hero.c1.a * exp (-max (near, 0.0) / hero.march.w);
    halo = vec4f (hero.c1.rgb * k, 0.0);
  }
  if (cover <= 0.0) {
    if (halo.r + halo.g + halo.b < 0.002) {
      discard;
    }
    return finish (halo.rgb, 0.0);
  }
  let q = o + dir * select (t_near, t, hit);
  let n = normal_at (q);
  let facing = clamp (dot (-dir, n), 0.0, 1.0);
  var col: vec3f;
  var body = 1.0;
  if (!glow) {
    let r = reflect (dir, n);
    let fresnel = 0.55 + 0.45 * pow (1.0 - facing, 4.0);
    col = studio (r) * hero.c0.rgb * fresnel + hero.c3.rgb * 0.35 * pow (1.0 - facing, 6.0);
  } else {
    // Colours drifting over the surface, brighter toward the middle, and a bright rim.
    let ph = hero.shape.z;
    let qs = spin_y (q, hero.shape.w);
    let w1 = 0.5 + 0.5 * sin (2.6 * qs.x + 1.9 * qs.y + 0.8 * ph);
    let w2 = 0.5 + 0.5 * sin (-2.1 * qs.y + 2.4 * qs.z + 1.1 * ph + 1.7);
    let inner = mix (mix (hero.c0.rgb, hero.c1.rgb, w1), hero.c2.rgb, 0.6 * w2);
    let rim = pow (1.0 - facing, 2.5);
    col = inner * (0.45 + 0.75 * facing) + hero.c3.rgb * 1.4 * rim;
    body = mix (hero.c0.a, 1.0, rim);
  }
  let a = cover * body;
  return finish (col * a + halo.rgb * (1.0 - a), a);
}
