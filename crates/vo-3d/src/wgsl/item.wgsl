// Composites a 3D item's scratch texture over the surface at the item's rect, times the
// layer opacity. Premultiplied over is on the pipeline; values pass through unconverted.

struct Item {
  // x, y, w, h in surface pixels; x and y are whole pixels.
  rect: vec4f,
  // Surface width and height in pixels, opacity.
  viewport: vec4f,
};

@group(0) @binding(0) var item_tex: texture_2d<f32>;
@group(0) @binding(1) var<uniform> item: Item;

@vertex fn vs_item (@builtin(vertex_index) vi: u32) -> @builtin(position) vec4f {
  let c = vec2f (f32 (vi & 1u), f32 (vi >> 1u));
  let px = item.rect.xy + c * item.rect.zw;
  let n = px / item.viewport.xy * 2.0 - 1.0;
  return vec4f (n.x, -n.y, 0.0, 1.0);
}

@fragment fn fs_item (@builtin(position) frag: vec4f) -> @location(0) vec4f {
  let p = vec2i (frag.xy - item.rect.xy);
  let size = vec2i (textureDimensions (item_tex));
  if (any (p < vec2i (0)) || any (p >= size)) {
    return vec4f (0.0);
  }
  return textureLoad (item_tex, p, 0) * item.viewport.z;
}
