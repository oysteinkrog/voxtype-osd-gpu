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

// Text (EG-31): the text field (binding 7) and the word table (binding 8). The table's head
// is (word count, target pixels per field texel, field width, field height); a field
// scale of 0 means no field.

struct TextWord {
  rect: vec4f,
  id: u32,
  age: f32,
  pad: vec2f,
};

struct TextWords {
  head: vec4f,
  words: array<TextWord, 64>,
};

@group(2) @binding(7) var mat_text: texture_2d<f32>;
@group(2) @binding(8) var<storage, read> mat_words: TextWords;

fn text_dist (px: vec2f) -> vec4f {
  let k = mat_words.head.y;
  if (k <= 0.0) {
    return vec4f (1e4, 0.0, 0.0, 0.0);
  }
  let size = mat_words.head.zw;
  let t = px / k;
  // Past the field's edge, add the distance to it.
  let inside = clamp (t, vec2f (0.5), size - 0.5);
  var f = textureSampleLevel (mat_text, mat_sampler, inside / size, 0.0);
  f.x += distance (t, inside) * k;
  let n = length (f.yz);
  if (n > 1e-4) {
    f = vec4f (f.x, f.yz / n, f.w);
  }
  return f;
}

fn text_word_count () -> u32 {
  return min (u32 (mat_words.head.x), 64u);
}

fn text_word (k: u32, px: vec2f) -> WordHit {
  if (k >= text_word_count ()) {
    return WordHit (NO_WORD_ID, 0.0, 1e9, vec4f (0.0));
  }
  let w = mat_words.words[k];
  let q = max (max (w.rect.xy - px, px - w.rect.zw), vec2f (0.0));
  return WordHit (w.id, w.age, length (q), w.rect);
}

fn word_near (px: vec2f) -> WordHit {
  var best = WordHit (NO_WORD_ID, 0.0, 1e9, vec4f (0.0));
  let n = text_word_count ();
  for (var k = 0u; k < n; k++) {
    let h = text_word (k, px);
    if (h.dist < best.dist) {
      best = h;
    }
  }
  return best;
}
