// Text distance field (TY19) by jump flooding (Rong and Tan 2006). Three compute entry
// points over a half-resolution coverage image:
//   cs_seed:    each texel becomes an inside seed (xy) or an outside seed (zw).
//   cs_step:    one flood step at distance `p.step`, nearest inside and outside seed kept apart.
//   cs_resolve: signed distance in target pixels, the direction away from the ink, coverage.
// Seeds are texel centres in field pixels; -1 means none found yet.

struct Params {
  size: vec2u,
  step: u32,
  // Target pixels per field pixel.
  scale: f32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var cov_tex: texture_2d<f32>;
@group(0) @binding(2) var src: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(8, 8) fn cs_seed (@builtin(global_invocation_id) id: vec3u) {
  if (any (id.xy >= p.size)) {
    return;
  }
  let a = textureLoad (cov_tex, id.xy, 0).a;
  let c = vec2f (id.xy) + 0.5;
  var s = vec4f (-1.0);
  if (a >= 0.5) {
    s = vec4f (c, -1.0, -1.0);
  } else {
    s = vec4f (-1.0, -1.0, c);
  }
  textureStore (dst, id.xy, s);
}

@compute @workgroup_size(8, 8) fn cs_step (@builtin(global_invocation_id) id: vec3u) {
  if (any (id.xy >= p.size)) {
    return;
  }
  let c = vec2f (id.xy) + 0.5;
  var best_in = vec2f (-1.0);
  var best_out = vec2f (-1.0);
  var d_in = 1e30;
  var d_out = 1e30;
  for (var dy = -1; dy <= 1; dy++) {
    for (var dx = -1; dx <= 1; dx++) {
      let q = vec2i (id.xy) + vec2i (dx, dy) * i32 (p.step);
      if (any (q < vec2i (0)) || any (q >= vec2i (p.size))) {
        continue;
      }
      let s = textureLoad (src, q, 0);
      if (s.x >= 0.0) {
        let d = distance (c, s.xy);
        if (d < d_in) {
          d_in = d;
          best_in = s.xy;
        }
      }
      if (s.z >= 0.0) {
        let d = distance (c, s.zw);
        if (d < d_out) {
          d_out = d;
          best_out = s.zw;
        }
      }
    }
  }
  textureStore (dst, id.xy, vec4f (best_in, best_out));
}

@group(0) @binding(4) var field_out: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8) fn cs_resolve (@builtin(global_invocation_id) id: vec3u) {
  if (any (id.xy >= p.size)) {
    return;
  }
  let c = vec2f (id.xy) + 0.5;
  let s = textureLoad (src, id.xy, 0);
  let a = textureLoad (cov_tex, id.xy, 0).a;
  var d = 0.0;
  var dir = vec2f (0.0);
  if (a >= 0.5) {
    // Inside: negative, away from the ink is toward the nearest outside texel.
    if (s.z >= 0.0) {
      d = -(distance (c, s.zw) - 0.5);
      dir = normalize (s.zw - c);
    } else {
      d = -1e4;
    }
  } else {
    if (s.x >= 0.0) {
      d = distance (c, s.xy) - 0.5;
      dir = normalize (c - s.xy);
    } else {
      d = 1e4;
    }
  }
  // On the edge the coverage is a better estimate than texel distances.
  if (a > 0.0 && a < 1.0) {
    d = 0.5 - a;
  }
  textureStore (field_out, id.xy, vec4f (d * p.scale, dir, a));
}
