// Material textures (EG-02, `material.rs`): replaces the stub helpers in `materials.wgsl` in the
// textured shape, glyph and vector pipelines. Group 2 holds the material blocks (binding 0),
// the backdrop, a bilinear clamped sampler and the image slots.
//
// The backdrop's place comes in the layer block, in fields content draws do not otherwise
// use: `style_box.xy` is the backdrop texture's top left in target pixels, `style_box.zw` its
// size in texels, `style_a.x` target pixels per texel (1, or 2 at half resolution), 0 when
// the layer has no backdrop.

@group(2) @binding(1) var mat_backdrop: texture_2d<f32>;
@group(2) @binding(2) var mat_sampler: sampler;
@group(2) @binding(3) var mat_image0: texture_2d<f32>;
@group(2) @binding(4) var mat_image1: texture_2d<f32>;
@group(2) @binding(5) var mat_image2: texture_2d<f32>;
@group(2) @binding(6) var mat_image3: texture_2d<f32>;

fn tex_backdrop (px: vec2f) -> vec4f {
  let k = layer.style_a.x;
  if (k <= 0.0) {
    return vec4f (0.0);
  }
  let t = (px - layer.style_box.xy) / k;
  if (any (t < vec2f (0.0)) || any (t > layer.style_box.zw)) {
    return vec4f (0.0);
  }
  let size = vec2f (textureDimensions (mat_backdrop));
  return textureSampleLevel (mat_backdrop, mat_sampler, t / size, 0.0);
}

fn tex_image (slot: u32, uv: vec2f) -> vec4f {
  switch slot {
    case 0u: { return textureSampleLevel (mat_image0, mat_sampler, uv, 0.0); }
    case 1u: { return textureSampleLevel (mat_image1, mat_sampler, uv, 0.0); }
    case 2u: { return textureSampleLevel (mat_image2, mat_sampler, uv, 0.0); }
    case 3u: { return textureSampleLevel (mat_image3, mat_sampler, uv, 0.0); }
    default: { return vec4f (0.0); }
  }
}
