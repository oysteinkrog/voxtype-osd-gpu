//! Golden images of the Slug glyph path, rendered offscreen on the GPU.
//!
//! GPU test: `just test-gpu` runs it, `just test` (CPU loop) leaves it out
//! because nextest's default profile filters out `gpu_*` binaries.
//! `just golden-update` rewrites the goldens. Outputs and the contact sheet
//! go to /var/tmp/claude/vo-golden/.

mod support;

use skrifa::instance::{LocationRef, Size};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, MetadataProvider};
use support::{Image, Tolerance};
use vo_render::text::slug_gpu::{Atlas, GlyphInstance, SlugRenderer, Uniforms};

const FONT: &str = "/usr/share/fonts/noto/NotoSans-Regular.ttf";
const TEXT: &str = "Hamburgefonstiv 0123456789 &?!";

struct Line {
    px: f32,
    deg: f32,
    origin: [f32; 2],
}

/// Lays `TEXT` out with cmap and advances (no shaping) for each line and
/// renders all lines into one image.
fn render(gpu: &support::Gpu, font: &FontRef, name: &str, w: u32, h: u32, lines: &[Line]) -> Image {
    let upem = font.head().unwrap().units_per_em() as f32;
    let cmap = font.charmap();
    let adv = font.glyph_metrics(Size::unscaled(), LocationRef::default());
    let mut atlas = Atlas::default();
    let mut glyphs = Vec::new();
    for line in lines {
        let k = line.px / upem;
        let (s, c) = line.deg.to_radians().sin_cos();
        let mut x = 0.0f32;
        for ch in TEXT.chars() {
            let gid = cmap.map(ch).unwrap_or_default();
            if let Some(e) = atlas.glyph(font, gid) {
                let origin = [line.origin[0] + x * c, line.origin[1] - x * s];
                glyphs.push(GlyphInstance::new(&e, origin, k, line.deg.to_radians()));
            }
            x += adv.advance_width(gid).unwrap_or(0.0) * k;
        }
    }
    let mut r = SlugRenderer::new(&gpu.device, wgpu::TextureFormat::Rgba8Unorm);
    r.upload_atlas(&gpu.device, &gpu.queue, &atlas);
    r.set_uniforms(&gpu.queue, &Uniforms::pixels(w as f32, h as f32));
    r.set_instances(&gpu.device, &gpu.queue, &glyphs);
    let px = support::render_alpha(gpu, w, h, |pass| r.draw(pass));
    Image { name: name.into(), w, h, channels: 1, px }
}

#[test]
fn slug_text_goldens() {
    let data = std::fs::read(FONT).unwrap_or_else(|e| panic!("{FONT}: {e} (install noto-fonts)"));
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let images = [
        render(
            &gpu,
            &font,
            "slug-upright",
            720,
            150,
            &[
                Line { px: 11.0, deg: 0.0, origin: [8.0, 18.0] },
                Line { px: 14.0, deg: 0.0, origin: [8.0, 40.0] },
                Line { px: 24.0, deg: 0.0, origin: [8.0, 72.0] },
                Line { px: 40.0, deg: 0.0, origin: [8.0, 126.0] },
            ],
        ),
        render(&gpu, &font, "slug-rotated", 420, 260, &[Line { px: 24.0, deg: 30.0, origin: [12.0, 240.0] }]),
    ];
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}
