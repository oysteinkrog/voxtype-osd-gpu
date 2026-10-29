// Text distance field (TY19), exact, in two compute passes over a half-resolution coverage
// image (EG-39; it was a jump flood of twelve passes before):
//   cs_columns: per column, each texel's nearest texel of the other kind (ink or not) in the
//               same column, from a bit mask of the column's ink in workgroup memory.
//   cs_rows:    per texel, the nearest texel of the other kind anywhere: the column distances
//               of its row, held in workgroup memory and searched outward block by block
//               until no nearer column can win (Euclidean distance is at least the horizontal
//               one), skipping blocks whose least distance cannot win; then the signed
//               distance in target pixels, the direction away from the ink and the coverage.
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
// Columns a row can hold in workgroup memory, and columns per block. The row and the two
// block minimum tables fit the 16 KiB WebGPU guarantees.
const ROW: u32 = 3584u;
const BLOCK: u32 = 16u;
const BLOCKS: u32 = 224u;

var<workgroup> row: array<u32, ROW>;
// Per block of the row, the least squared column distance to ink (`to_ink`) and to a texel
// without ink (`to_clear`), FAR for none.
var<workgroup> to_ink: array<u32, BLOCKS>;
var<workgroup> to_clear: array<u32, BLOCKS>;

// Column `x`'s nearest texel of the kind sought (ink when `want_ink`) in this row, as a row
// offset in `dy` and the squared row distance as the result (FAR for none).
fn column (x: u32, want_ink: bool, dy: ptr<function, i32>) -> u32 {
  let v = row[x];
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

// One workgroup per row: the row's column results and their block minimums go to workgroup
// memory once, and each thread searches them for its texels.
@compute @workgroup_size(256) fn cs_rows (@builtin(workgroup_id) wg: vec3u, @builtin(local_invocation_index) li: u32) {
  let y = wg.y;
  let w = p.size.x;
  if (p.empty == 1u) {
    for (var x = li; x < w; x += 256u) {
      let a = textureLoad (cov_tex, vec2u (x, y), 0).a;
      textureStore (field_out, vec2u (x, y), vec4f (1e4 * p.scale, 0.0, 0.0, a));
    }
    return;
  }
  for (var x = li; x < w; x += 256u) {
    row[x] = textureLoad (col_tex, vec2u (x, y), 0).x;
  }
  workgroupBarrier ();
  let nb = (w + BLOCK - 1u) / BLOCK;
  for (var b = li; b < nb; b += 256u) {
    var mi = FAR;
    var mc = FAR;
    var dy = 0;
    for (var x = b * BLOCK; x < min (w, b * BLOCK + BLOCK); x++) {
      mi = min (mi, column (x, true, &dy));
      mc = min (mc, column (x, false, &dy));
    }
    to_ink[b] = mi;
    to_clear[b] = mc;
  }
  workgroupBarrier ();
  for (var x = li; x < w; x += 256u) {
    resolve (x, y, w, nb);
  }
}

fn block_min (b: u32, want_ink: bool) -> u32 {
  return select (to_clear[b], to_ink[b], want_ink);
}

fn resolve (x: u32, y: u32, w: u32, nb: u32) {
  let a = textureLoad (cov_tex, vec2u (x, y), 0).a;
  let on_ink = a >= 0.5;
  let want = !on_ink;
  var best = FAR;
  var bx = 0u;
  var by = 0;
  var dy = 0;
  // The texel's own block, then blocks outward on both sides while a block's nearest column
  // is nearer than the best so far; a block whose least distance cannot win is skipped.
  let home = x / BLOCK;
  var left = true;
  var right = true;
  for (var k = 0u; left || right; k++) {
    for (var side = 0u; side < 2u; side++) {
      if ((side == 0u && !left) || (side == 1u && !right) || (k == 0u && side == 1u)) {
        continue;
      }
      var b = home;
      var gap = 0u;
      if (side == 0u && k > 0u) {
        if (k > home) {
          left = false;
          continue;
        }
        b = home - k;
        gap = x - (b * BLOCK + BLOCK - 1u);
      } else if (side == 1u) {
        b = home + k;
        if (b >= nb) {
          right = false;
          continue;
        }
        gap = b * BLOCK - x;
      }
      if (gap * gap >= best) {
        if (side == 0u) {
          left = false;
        } else {
          right = false;
        }
        continue;
      }
      let m = block_min (b, want);
      if (m == FAR || m + gap * gap >= best) {
        continue;
      }
      for (var c = b * BLOCK; c < min (w, b * BLOCK + BLOCK); c++) {
        let g = column (c, want, &dy);
        if (g == FAR) {
          continue;
        }
        let dx = select (c - x, x - c, c < x);
        if (g + dx * dx < best) {
          best = g + dx * dx;
          bx = c;
          by = dy;
        }
      }
    }
  }

  let c = vec2f (f32 (x), f32 (y)) + 0.5;
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
  textureStore (field_out, vec2u (x, y), vec4f (d * p.scale, dir, a));
}
