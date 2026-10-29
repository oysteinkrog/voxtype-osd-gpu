//! The liquid revision in live text (EG-32): a word revised with `Revise::Liquid` flows into
//! its correction, drawn by the painter `vo_3d::install` adds. Strips of frames over the
//! liquid for a word on the reading line and one on an older line, in flat ink and in a
//! chrome material, at scale 1 and 1.5, and in light mode.
//!
//! GPU test: `just test-gpu liquid_revision`. With `GLYPH_FX_SHEET=DIR` the strips are also
//! written to DIR.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use support::{Image, Tolerance};
use vo_core::words::Word;
use vo_render::{Renderer, Target};
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::liquid::LIQUID_SECS;
use vo_scene::flow::Revise;
use vo_scene::font::{FontId, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::material::Material;
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const W: f32 = 600.0;
const H: f32 = 300.0;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const SIZE: f32 = 52.0;

fn fonts() -> (FontRegistry, FontId) {
    let fonts = FontRegistry::new();
    let id = fonts.add_file(&vo_scene::font::test_font_path()).unwrap();
    (fonts, id)
}

fn gpu() -> &'static support::Gpu {
    static GPU: std::sync::OnceLock<support::Gpu> = std::sync::OnceLock::new();
    GPU.get_or_init(support::gpu)
}

#[derive(Clone, Copy)]
struct Look {
    chrome: bool,
    light: bool,
}

/// "my scream teem" on one line (the reading line) or, wrapped with more words after it, on
/// an older line; then "my cream team".
fn view(font: FontId, revised: bool, older: bool, look: Look) -> Element<()> {
    // A deletion and a change of letter at once.
    let mut text = if revised { vec!["my", "cream", "team"] } else { vec!["my", "scream", "teem"] };
    if older {
        text.extend(["at", "noon"]);
    }
    let words: Vec<Word> = text.iter().enumerate().map(|(i, s)| Word { id: i as u32 + 1, text: (*s).into() }).collect();
    let (back, ink) = if look.light { (0xf3f4f6, 0x111827) } else { (0x0d1117, 0xf5f7fa) };
    let mut live = Element::live_text("live", &words)
        .text_style(TextStyle::new(font, SIZE, SIZE * 1.25))
        .color(Color::hex(ink, 1.0))
        .word_revise(Revise::Liquid)
        .width(W - 80.0);
    if look.chrome {
        live = live.material(Material::chrome_classic());
    }
    Element::panel("root").size(W, H).fill(Color::hex(back, 1.0)).pad(40.0).child(live)
}

/// Frames over the liquid, revised at t = 1, at `scale`.
fn frames(r: &mut Renderer, fonts: &FontRegistry, font: FontId, older: bool, look: Look, scale: f32) -> Vec<Vec<u8>> {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W, H), scale);
    scene.apply(view(font, false, older, look), 0.0);
    scene.sample(0.5);
    scene.apply(view(font, true, older, look), 1.0);
    let (w, h) = ((W * scale) as u32, (H * scale) as u32);
    let gpu = gpu();
    let mut out = Vec::new();
    for k in [0.0, 0.25, 0.5, 0.75, 1.05] {
        let list = scene.sample(1.0 + k * LIQUID_SECS);
        let mut target = Target::new(&gpu.device, w, h, FORMAT);
        r.render_at(&list, scale, &mut target);
        out.push(vo_3d::readback::read_rgba(&gpu.device, &gpu.queue, target.texture()));
    }
    out
}

/// Frames side by side.
fn strip(name: &str, frames: &[Vec<u8>], w: u32, h: u32) -> Image {
    let n = frames.len() as u32;
    let mut px = vec![0u8; (w * n * h * 4) as usize];
    for (i, f) in frames.iter().enumerate() {
        for y in 0..h {
            let src = &f[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
            let at = ((y * w * n + i as u32 * w) * 4) as usize;
            px[at..at + (w * 4) as usize].copy_from_slice(src);
        }
    }
    let img = Image { name: name.into(), w: w * n, h, channels: 4, px };
    if let Some(dir) = std::env::var_os("GLYPH_FX_SHEET") {
        let path = std::path::Path::new(&dir).join(format!("{name}.png"));
        let file = std::fs::File::create(&path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), img.w, img.h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&img.px).unwrap();
    }
    img
}

/// Share of pixels that differ by more than `tol` in any channel.
fn differ(a: &[u8], b: &[u8], tol: u8) -> f64 {
    let n = a
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0.iter())
        .filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > tol))
        .count();
    n as f64 / (a.len() / 4) as f64
}

fn renderer(fonts: &FontRegistry, light: bool) -> Renderer {
    let gpu = gpu();
    let mut r = Renderer::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    r.set_light(light);
    vo_3d::install(&mut r);
    r
}

fn check(name: &str, older: bool, look: Look, scale: f32) {
    let (fonts, font) = fonts();
    let mut r = renderer(&fonts, look.light);
    let f = frames(&mut r, &fonts, font, older, look, scale);
    // The liquid moves: mid-way differs from the start and from the end.
    assert!(differ(&f[2], &f[0], 30) > 1e-4, "{name}: nothing flows");
    assert!(differ(&f[2], &f[4], 30) > 1e-4, "{name}: nothing flows");
    // Once it ends the text draws the word alone: the same as a word that was never revised.
    let mut plain: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    plain.set_viewport(0.0, Size::new(W, H), scale);
    plain.apply(view(font, true, older, look), 0.0);
    let (w, h) = ((W * scale) as u32, (H * scale) as u32);
    let mut target = Target::new(&gpu().device, w, h, FORMAT);
    r.render_at(&plain.sample(5.0), scale, &mut target);
    let still = vo_3d::readback::read_rgba(&gpu().device, &gpu().queue, target.texture());
    let mut end = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    end.set_viewport(0.0, Size::new(W, H), scale);
    end.apply(view(font, false, older, look), 0.0);
    end.sample(0.5);
    end.apply(view(font, true, older, look), 1.0);
    let mut target = Target::new(&gpu().device, w, h, FORMAT);
    r.render_at(&end.sample(5.0), scale, &mut target);
    let settled = vo_3d::readback::read_rgba(&gpu().device, &gpu().queue, target.texture());
    assert!(differ(&settled, &still, 8) < 1e-3, "{name}: the liquid leaves something behind");
    support::check(&strip(name, &f, w, h), Tolerance { per_pixel: 4, max_fraction: 0.01 }).unwrap();
}

const FLAT: Look = Look { chrome: false, light: false };
const CHROME: Look = Look { chrome: true, light: false };
const LIGHT: Look = Look { chrome: false, light: true };

#[test]
fn liquid_revision_on_the_reading_line() {
    check("liquid-revision-reading", false, FLAT, 1.0);
}

#[test]
fn liquid_revision_on_an_older_line() {
    check("liquid-revision-older", true, FLAT, 1.0);
}

#[test]
fn liquid_revision_in_chrome() {
    check("liquid-revision-chrome", true, CHROME, 1.0);
    check("liquid-revision-chrome-reading", false, CHROME, 1.0);
}

#[test]
fn liquid_revision_at_one_and_a_half() {
    check("liquid-revision-older-1.5x", true, FLAT, 1.5);
}

#[test]
fn liquid_revision_in_light_mode() {
    check("liquid-revision-light", true, LIGHT, 1.0);
}

/// A renderer with no `glyph_fx` painter, in a process where the flag is set, still shows the
/// word's ink during the revision (RV-06). On an older line the text draws none of it.
#[test]
fn liquid_revision_without_a_painter_draws_the_word() {
    let (fonts, font) = fonts();
    vo_scene::flow::liquid::install_painter();
    let gpu = gpu();
    let mut r = Renderer::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(view(font, false, true, FLAT), 0.0);
    scene.sample(0.5);
    scene.apply(view(font, true, true, FLAT), 1.0);
    let mut target = Target::new(&gpu.device, W as u32, H as u32, FORMAT);
    r.render_at(&scene.sample(1.0 + 0.5 * LIQUID_SECS), 1.0, &mut target);
    let mid = vo_3d::readback::read_rgba(&gpu.device, &gpu.queue, target.texture());
    // Background is 0x0d1117, so any bright pixel is ink.
    let ink = mid.iter().skip(1).step_by(4).filter(|&&g| g > 0x51).count();
    assert!(ink > 500, "the word has no ink during the revision: {ink} pixels");
}
