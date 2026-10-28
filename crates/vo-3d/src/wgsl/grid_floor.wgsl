// A perspective grid floor (FX-152) drawn straight over the surface, premultiplied over. The
// floor is a plane below the eye; each pixel below the horizon finds where its ray meets the
// plane, in closed form, and draws the grid lines there with antialiasing from the screen
// derivatives. No marching and no textures.

struct Grid {
  // Quad x, y, w, h in surface pixels (the element's box, cut to the scissor).
  rect: vec4f,
  // Surface width and height, opacity, 1 to encode sRGB here.
  viewport: vec4f,
  // Vanishing point x, horizon y (surface pixels), focal length (pixels), eye height (world).
  view: vec4f,
  // Cell size (world), scroll (world, along depth), line width (pixels), far fade (world).
  cells: vec4f,
  // Glow pool on the floor: x (world), depth (world), radius (world), strength.
  pool: vec4f,
  // Horizon glow height (pixels), horizon glow strength, streak width (pixels), streak strength.
  glow: vec4f,
  // Linear straight colours: lines at the left, lines at the right, horizon and pool glow,
  // floor fill (alpha is its opacity).
  left: vec4f,
  right: vec4f,
  haze: vec4f,
  fill: vec4f,
};

@group(0) @binding(0) var<uniform> grid: Grid;

@vertex fn vs_grid (@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  let px = grid.rect.xy + c * grid.rect.zw;
  let n = px / grid.viewport.xy * 2.0 - 1.0;
  return vec4f (n.x, -n.y, 0.0, 1.0);
}

fn encode (c: vec3f) -> vec3f {
  let lo = c * 12.92;
  let hi = 1.055 * pow (max (c, vec3f (0.0)), vec3f (1.0 / 2.4)) - 0.055;
  return select (hi, lo, c <= vec3f (0.0031308));
}

// Coverage of a line of `width` pixels at every whole value of `u`.
fn lines (u: f32, width: f32) -> f32 {
  let w = max (fwidth (u), 1e-5);
  let d = abs (fract (u + 0.5) - 0.5) / w;
  // Lines thinner than a pixel fade instead of breaking up.
  let cover = clamp (0.5 * width + 0.5 - d, 0.0, 1.0);
  return cover * clamp (1.0 / (w * 2.5), 0.0, 1.0);
}

@fragment fn fs_grid (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let vp = grid.view.xy;
  let f = grid.view.z;
  let eye_h = grid.view.w;
  let dy = frag.y - vp.y;
  let haze_h = max (grid.glow.x, 1e-3);
  // The horizon glow, above and below the line.
  let band = grid.glow.y * exp (-pow (dy / haze_h, 2.0));
  // Below the horizon: depth and side position where this pixel's ray meets the floor.
  // Computed for every pixel so the derivatives stay in uniform control flow, then masked.
  let below = step (0.0, dy);
  let z = eye_h * f / max (dy, 1e-3);
  let x = (frag.x - vp.x) * z / f;
  let u = x / grid.cells.x;
  let v = (z + grid.cells.y) / grid.cells.x;
  let line = max (lines (u, grid.cells.z), lines (v, grid.cells.z)) * below;
  // Faint at the far end, full nearer the viewer.
  let near = (1.0 - smoothstep (0.0, grid.cells.w, z)) * below;
  let across = clamp ((frag.x - grid.rect.x) / grid.rect.z, 0.0, 1.0);
  let tone = mix (grid.left.rgb, grid.right.rgb, across);
  // The pool of light on the floor, and the bright streak under it.
  let p = vec2f (x - grid.pool.x, z - grid.pool.y) / max (grid.pool.z, 1e-3);
  let pool = grid.pool.w * exp (-dot (p, p)) * below;
  let sx = vp.x + grid.pool.x * f / max (grid.pool.y, 1e-3);
  let streak = grid.glow.w * exp (-pow ((frag.x - sx) / max (grid.glow.z, 1e-3), 2.0)) * exp (-max (dy, 0.0) / (4.0 * haze_h)) * below;
  let lit = line * near * (1.0 + 2.0 * pool);
  var col = grid.haze.rgb * band + tone * lit + grid.haze.rgb * (0.6 * pool + streak);
  let a = max (grid.fill.a * near, min (lit, 1.0));
  col += grid.fill.rgb * grid.fill.a * near * (1.0 - min (lit, 1.0));
  let k = grid.viewport.z;
  var c = col * k;
  if (grid.viewport.w > 0.5) {
    c = select (encode (c), encode (c / (a * k)) * a * k, a > 1e-4);
  }
  if (all (c <= vec3f (1e-4)) && a <= 1e-4) {
    discard;
  }
  return vec4f (c, a * k);
}
