// Text distance field (TY19), exact, in two compute passes over a half-resolution coverage
// image (EG-39; it was a jump flood of twelve passes before):
//   cs_columns: per column, each texel's nearest texel of the other kind (ink or not) in the
//               same column, from a bit mask of the column's ink in workgroup memory.
//   cs_rows:    per texel, the nearest texel of the other kind anywhere: the column distances
//               of its row, searched outward until no nearer column can win (Euclidean
//               distance is at least the horizontal one), then the signed distance in target
//               pixels, the direction away from the ink and the coverage.
// A texel is ink when its coverage alpha is at least 0.5. Distances are between texel
// centres, so they match a brute-force search exactly.

struct Params {
  size: vec2u,
  // 1 when the list draws nothing: every texel is far outside, with no search.
  empty: u32,
  // Target pixels per field pixel.
  scale: f32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var cov_tex: texture_2d<f32>;
@group(0) @binding(2) var col_tex: texture_2d<u32>;
@group(0) @binding(3) var col_out: texture_storage_2d<r32uint, write>;
@group(0) @binding(4) var field_out: texture_storage_2d<rgba16float, write>;

// A column texel packs: bits 0-15 the row distance to the nearest texel of the other kind
// (NONE for none in the column), bit 16 set when that texel is above, bit 17 set on ink.
const NONE: u32 = 65535u;
const ABOVE: u32 = 65536u;
const INK: u32 = 131072u;
// Rows a column can hold: 1024 words of 32 bits.
const WORDS: u32 = 1024u;

var<workgroup> bits: array<atomic<u32>, WORDS>;

fn ink (x: u32, y: u32) -> bool {
  return textureLoad (cov_tex, vec2u (x, y), 0).a >= 0.5;
}

// Word `i` of the column with a set bit for each texel of the kind sought (ink when
// `want_ink`), and no bits past the last row.
fn sought (i: u32, want_ink: bool) -> u32 {
  let v = atomicLoad (&bits[i]);
  var m = select (~v, v, want_ink);
  let rest = p.size.y - i * 32u;
  if (rest < 32u) {
    m &= (1u << rest) - 1u;
  }
  return m;
}

@compute @workgroup_size(64) fn cs_columns (@builtin(workgroup_id) wg: vec3u, @builtin(local_invocation_index) li: u32) {
  let x = wg.x;
  let h = p.size.y;
  let n = (h + 31u) / 32u;
  for (var i = li; i < n; i += 64u) {
    atomicStore (&bits[i], 0u);
  }
  workgroupBarrier ();
  for (var y = li; y < h; y += 64u) {
    if (ink (x, y)) {
      atomicOr (&bits[y / 32u], 1u << (y % 32u));
    }
  }
  workgroupBarrier ();
  for (var y = li; y < h; y += 64u) {
    let i = y / 32u;
    let b = y % 32u;
    let on_ink = ((atomicLoad (&bits[i]) >> b) & 1u) == 1u;
    let want = !on_ink;
    // Below (rows after y).
    var below = NONE;
    var j = i;
    var m = sought (j, want) & ~((2u << b) - 1u);
    while (m == 0u && j + 1u < n) {
      j += 1u;
      m = sought (j, want);
    }
    if (m != 0u) {
      below = j * 32u + firstTrailingBit (m) - y;
    }
    // Above (rows before y).
    var above = NONE;
    j = i;
    m = sought (j, want) & ((1u << b) - 1u);
    while (m == 0u && j > 0u) {
      j -= 1u;
      m = sought (j, want);
    }
    if (m != 0u) {
      above = y - (j * 32u + firstLeadingBit (m));
    }
    var v = select (below, above | ABOVE, above < below);
    if (on_ink) {
      v |= INK;
    }
    textureStore (col_out, vec2u (x, y), vec4u (v, 0u, 0u, 0u));
  }
}

const FAR: u32 = 0xffffffffu;

// Column `x`'s nearest texel of the kind sought (ink when `want_ink`) in row `y`, as a row
// offset in `dy` and the squared row distance as the result (FAR for none).
fn column (x: u32, y: u32, want_ink: bool, dy: ptr<function, i32>) -> u32 {
  let v = textureLoad (col_tex, vec2u (x, y), 0).x;
  if (((v & INK) != 0u) == want_ink) {
    *dy = 0;
    return 0u;
  }
  let g = v & NONE;
  if (g == NONE) {
    return FAR;
  }
  *dy = select (i32 (g), -i32 (g), (v & ABOVE) != 0u);
  return g * g;
}

@compute @workgroup_size(64) fn cs_rows (@builtin(global_invocation_id) id: vec3u) {
  if (any (id.xy >= p.size)) {
    return;
  }
  let x = id.x;
  let y = id.y;
  let a = textureLoad (cov_tex, id.xy, 0).a;
  if (p.empty == 1u) {
    textureStore (field_out, id.xy, vec4f (1e4 * p.scale, 0.0, 0.0, a));
    return;
  }
  let on_ink = a >= 0.5;
  let want = !on_ink;
  var dy = 0;
  var best = column (x, y, want, &dy);
  var bx = i32 (x);
  var by = dy;
  // Outward from x while a column r away could still be nearer than the best so far.
  var r = 1u;
  let w = p.size.x;
  while (r * r < best && (r <= x || x + r < w)) {
    let rr = r * r;
    if (r <= x) {
      let g = column (x - r, y, want, &dy);
      if (g != FAR && g + rr < best) {
        best = g + rr;
        bx = i32 (x - r);
        by = dy;
      }
    }
    if (x + r < w) {
      let g = column (x + r, y, want, &dy);
      if (g != FAR && g + rr < best) {
        best = g + rr;
        bx = i32 (x + r);
        by = dy;
      }
    }
    r += 1u;
  }

  let c = vec2f (id.xy) + 0.5;
  var d = 0.0;
  var dir = vec2f (0.0);
  if (best == FAR) {
    d = select (1e4, -1e4, on_ink);
  } else {
    let s = vec2f (f32 (bx), f32 (i32 (y) + by)) + 0.5;
    let len = distance (c, s);
    if (on_ink) {
      // Inside: negative, away from the ink is toward the nearest outside texel.
      d = -(len - 0.5);
      dir = normalize (s - c);
    } else {
      d = len - 0.5;
      dir = normalize (c - s);
    }
  }
  // On the edge the coverage is a better estimate than texel distances.
  if (a > 0.0 && a < 1.0) {
    d = 0.5 - a;
  }
  textureStore (field_out, id.xy, vec4f (d * p.scale, dir, a));
}
