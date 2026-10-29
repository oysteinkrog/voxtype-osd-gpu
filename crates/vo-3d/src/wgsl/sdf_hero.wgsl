// One raymarched hero object (a chrome blob, an aurora orb, a jellyfish bell) drawn straight
// over the surface, premultiplied over. Space is surface pixels, y down, z toward the viewer;
// the eye is the stage camera's. The quad covers only the object's bounds, and each ray
// marches only between the entry and exit of a bounding sphere, at most `march.y` times.
// Shape 3 is a scene of up to 8 parts in two layers (skin and core); with the inside flag the
// skin is see-through, the core shows behind it and an aurora of 3D noise glows within it.
// A chrome look with an environment (EG-17, `env.rs`) reflects the equirectangular texture at
// binding 1 instead of the analytic studio: the studio sky with the popup's own layers in its
// front half.

struct Hero {
  // Quad x, y, w, h in surface pixels.
  rect: vec4f,
  // Surface width and height in pixels, opacity, 1 to encode sRGB here.
  viewport: vec4f,
  // Eye position (stage origin, eye distance in front), surface pixels; 1 when the chrome
  // reflects the environment texture.
  eye: vec4f,
  // Object centre in surface pixels (z 0); radius in surface pixels.
  centre: vec4f,
  // Shape (0 orb, 1 blob, 2 bell, 3 parts), wobble 0..1, phase in seconds, spin in radians.
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
  // Part count, smooth union radius in radii, 1 for a see-through skin, aurora steps.
  parts: vec4f,
  // Skin opacity facing the viewer, core glow, aurora brightness, aurora scale.
  inside: vec4f,
  // Core colour; the aurora's two colours. Linear.
  core: vec4f,
  fog_a: vec4f,
  fog_b: vec4f,
  // Per part: position and kind (0 sphere, 1 ellipsoid, 2 capsule, 3 bell); sizes and layer
  // (0 skin, 1 core); the capsule's far end, relative to the position, and the copies around
  // the vertical axis; the bob in radii.
  part: array<vec4f, 32>,
};

@group(0) @binding(0) var<uniform> hero: Hero;
@group(0) @binding(1) var env_tex: texture_2d<f32>;
@group(0) @binding(2) var env_sampler: sampler;

// Where direction `r` lands in the environment texture: longitude about the vertical axis
// (0.5 facing the viewer), angle from up. `env_uv` in env.rs is the same.
fn env_uv (r: vec3f) -> vec2f {
  let d = normalize (r);
  return vec2f (0.5 + atan2 (d.x, d.z) / 6.2831853, acos (clamp (-d.y, -1.0, 1.0)) / 3.14159265);
}

// What the chrome reflects along `r`: the environment when it has one, else the studio.
fn surroundings (r: vec3f) -> vec3f {
  if (hero.eye.w > 0.5) {
    return textureSampleLevel (env_tex, env_sampler, env_uv (r), 0.0).rgb;
  }
  return studio (r);
}

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

// One part's distance from `p` (object space after spin and stretch), in radii.
fn part_d (i: u32, p0: vec3f) -> f32 {
  let a = hero.part[4u * i];
  let s = hero.part[4u * i + 1u].xyz;
  let end = hero.part[4u * i + 2u];
  let bob = hero.part[4u * i + 3u].x;
  // Copies around the vertical axis: fold the angle into the first copy's sector. Each copy
  // bobs a step later, a wave running around.
  var q = p0;
  var id = 0.0;
  if (end.w > 1.5) {
    let sector = 6.2831853 / end.w;
    let ang = atan2 (q.z, q.x);
    id = round (ang / sector);
    let a2 = ang - id * sector;
    let r = length (q.xz);
    q = vec3f (r * cos (a2), q.y, r * sin (a2));
  }
  if (bob != 0.0) {
    q.y -= bob * sin (1.6 * hero.shape.z + 6.2831853 * id / max (end.w, 1.0));
  }
  let p = q - a.xyz;
  if (a.w < 0.5) {
    return length (p) - s.x;
  }
  if (a.w < 1.5) {
    let k0 = length (p / s);
    let k1 = length (p / (s * s));
    return k0 * (k0 - 1.0) / max (k1, 1e-6);
  }
  if (a.w < 2.5) {
    let ba = end.xyz;
    let h = clamp (dot (p, ba) / max (dot (ba, ba), 1e-8), 0.0, 1.0);
    return length (p - ba * h) - s.x;
  }
  // A bell like shape 2's, without the breathing: a dome cut open below, a swaying hem.
  let b = p / s;
  let dome = length (b * vec3f (1.0, 1.25, 1.0)) - 1.0;
  let hem = 0.3 + 0.05 * sin (8.0 * atan2 (b.z, b.x) + 1.5 * hero.shape.z);
  let cut = b.y - hem;
  let hollow = length ((b - vec3f (0.0, 0.55, 0.0)) * vec3f (1.05, 1.4, 1.05)) - 0.9;
  return smax (smax (dome, cut, 0.12), -hollow, 0.08) * 0.8 * min (s.x, min (s.y, s.z));
}

// Distances in radii to the skin (x) and the core (y) of a parts scene, each layer's parts
// melted with radius `parts.y`. Ripples with the wobble run over the skin only.
fn layers (q0: vec3f) -> vec2f {
  let st = hero.stretch.xyz;
  let q = spin_y (q0, hero.shape.w) / st;
  let k = hero.parts.y;
  var skin = 1e9;
  var core = 1e9;
  let n = u32 (hero.parts.x);
  for (var i = 0u; i < n; i++) {
    let d = part_d (i, q);
    if (hero.part[4u * i + 1u].w < 0.5) {
      skin = smin (skin, d, k);
    } else {
      core = smin (core, d, k);
    }
  }
  let ph = hero.shape.z;
  let r = sin (5.0 * q.x + 1.7 * ph) * sin (4.0 * q.y - 1.3 * ph) * sin (4.5 * q.z + 0.9 * ph);
  skin += 0.06 * hero.shape.y * r;
  return vec2f (skin, core) * min (st.x, min (st.y, st.z));
}

// The scene's distance: 0 everything, 1 the skin, 2 the core (none for shapes 0 to 2).
fn field (q: vec3f, which: u32) -> f32 {
  if (hero.shape.x < 2.5) {
    if (which == 2u) {
      return 1e9;
    }
    return sdf (q);
  }
  let l = layers (q);
  if (which == 1u) {
    return l.x;
  }
  if (which == 2u) {
    return l.y;
  }
  return min (l.x, l.y);
}

fn normal_at (q: vec3f, which: u32) -> vec3f {
  let e = vec2f (1.0, -1.0) * 0.0015;
  return normalize (e.xyy * field (q + e.xyy, which) + e.yyx * field (q + e.yyx, which) + e.yxy * field (q + e.yxy, which) + e.xxx * field (q + e.xxx, which));
}

// The core's light at a point with normal `n`: its colour, brighter facing and at the rim.
fn core_light (n: vec3f, dir: vec3f) -> vec3f {
  let facing = clamp (dot (-dir, n), 0.0, 1.0);
  return hero.core.rgb * hero.inside.y * (0.5 + 0.5 * facing + 0.8 * pow (1.0 - facing, 2.0));
}

fn hash3 (c: vec3i) -> f32 {
  var h = (bitcast<u32> (c.x) * 73856093u) ^ (bitcast<u32> (c.y) * 19349663u) ^ (bitcast<u32> (c.z) * 83492791u);
  h = (h ^ (h >> 16u)) * 0x45d9f3bu;
  h = (h ^ (h >> 16u)) * 0x45d9f3bu;
  h = h ^ (h >> 16u);
  return f32 (h) / 4294967295.0;
}

fn vnoise (p: vec3f) -> f32 {
  let i = vec3i (floor (p));
  let f = fract (p);
  let u = f * f * (3.0 - 2.0 * f);
  let x00 = mix (hash3 (i), hash3 (i + vec3i (1, 0, 0)), u.x);
  let x10 = mix (hash3 (i + vec3i (0, 1, 0)), hash3 (i + vec3i (1, 1, 0)), u.x);
  let x01 = mix (hash3 (i + vec3i (0, 0, 1)), hash3 (i + vec3i (1, 0, 1)), u.x);
  let x11 = mix (hash3 (i + vec3i (0, 1, 1)), hash3 (i + vec3i (1, 1, 1)), u.x);
  return mix (mix (x00, x10, u.y), mix (x01, x11, u.y), u.z);
}

// Aurora light at object-space `q`: folded curtains of noise, drifting up with the phase
// (frozen at phase 0), coloured between the two fog colours.
fn aurora (q: vec3f) -> vec3f {
  let ph = hero.shape.z;
  let qs = spin_y (q, hero.shape.w);
  let w = qs * hero.inside.w + vec3f (0.0, -0.35 * ph, 0.2 * ph);
  let n1 = vnoise (w);
  let n2 = vnoise (w * 2.1 + vec3f (3.7, 1.3, 5.1));
  let curtain = pow (1.0 - abs (2.0 * n1 - 1.0), 4.0);
  let colour = mix (hero.fog_a.rgb, hero.fog_b.rgb, smoothstep (0.25, 0.75, n2 + 0.3 * qs.y));
  return colour * curtain * (0.6 + 0.4 * n2);
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
    let d = field (o + dir * t, 0u);
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
  let t_hit = select (t_near, t, hit);
  let q = o + dir * t_hit;
  let parts = hero.shape.x > 2.5;
  // Which layer the ray met: a core part outside the skin (a drip bead) is drawn as it is.
  var on_core = false;
  if (parts) {
    let l = layers (q);
    on_core = l.y < l.x;
  }
  let n = normal_at (q, select (select (0u, 1u, parts), 2u, on_core));
  let facing = clamp (dot (-dir, n), 0.0, 1.0);
  var col: vec3f;
  var body = 1.0;
  if (on_core) {
    col = core_light (n, dir);
  } else if (!glow) {
    let r = reflect (dir, n);
    let fresnel = 0.55 + 0.45 * pow (1.0 - facing, 4.0);
    col = surroundings (r) * hero.c0.rgb * fresnel + hero.c3.rgb * 0.35 * pow (1.0 - facing, 6.0);
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
  if (hero.parts.z < 0.5 || on_core) {
    let a = cover * body;
    return finish (col * a + halo.rgb * (1.0 - a), a);
  }
  // A see-through skin: opaque toward its edges, `inside.x` where it faces the viewer.
  let skin = mix (hero.inside.x, 1.0, pow (1.0 - facing, select (3.0, 2.5, glow)));
  // The core behind the skin.
  var t_back = t_end;
  var back = vec4f (0.0);
  if (hero.parts.x > 0.5) {
    var tc = t_hit + px;
    for (var i = 0u; i < 32u; i++) {
      let d = field (o + dir * tc, 2u);
      if (d < 0.25 * px) {
        let qc = o + dir * tc;
        back = vec4f (core_light (normal_at (qc, 2u), dir), 1.0);
        t_back = tc;
        break;
      }
      tc += max (d * 0.9, 0.5 * px);
      if (tc > t_end) {
        break;
      }
    }
  }
  // The aurora within the skin, sampled evenly up to the core or the far side of the bound;
  // the first sample is offset per pixel, the same every frame.
  let fog_steps = u32 (hero.parts.w);
  if (hero.inside.z > 0.0 && fog_steps > 0u) {
    let dt = (t_back - t_hit) / f32 (fog_steps);
    let jitter = hash3 (vec3i (vec2i (frag.xy), 7));
    var light = vec3f (0.0);
    for (var i = 0u; i < fog_steps; i++) {
      let p = o + dir * (t_hit + (f32 (i) + jitter) * dt);
      let inside = smoothstep (0.02, -0.06, field (p, 1u));
      if (inside > 0.0) {
        light += aurora (p) * inside;
      }
    }
    back = vec4f (back.rgb + light * hero.inside.z * dt, back.a);
  }
  let a = cover * (skin + (1.0 - skin) * back.a);
  let pm = cover * (col * skin + (1.0 - skin) * back.rgb);
  return finish (pm + halo.rgb * (1.0 - a), a);
}
