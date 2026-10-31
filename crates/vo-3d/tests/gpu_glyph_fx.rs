//! Glyph field effects (`vo_3d::glyph_fx`): ink diffusion (MO-82), light through letters
//! (MO-84) and liquid morph (MO-87), each as a strip of frames over its moment, drawn by the
//! painter `vo_3d::install` adds over real Slug text.
//!
//! GPU test: `just test-gpu glyph_fx`. With `GLYPH_FX_SHEET=DIR` the strips are also written
//! to DIR, and each effect's GPU time per frame is printed.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use support::{Image, Tolerance};
use vo_3d::glyph_fx::{Effect, FxGlyph, GlyphFx, Ink, Light, Morph, Part, Side};
use vo_render::{Renderer, Target};
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{FontId, FontRegistry};
use vo_scene::geom::{Color, Point, Size};
use vo_scene::taffy;
use vo_scene::taffy::prelude::{auto, length};
use vo_scene::text::{ShapedLayout, TextLayout};
use vo_scene::Scene;

const W: u32 = 600;
const H: u32 = 300;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const BACK: u32 = 0x0d1117;
const TEXT: (f32, f32) = (60.0, 90.0);
const SIZE: f32 = 96.0;

fn fonts() -> (FontRegistry, FontId) {
    let fonts = FontRegistry::new();
    let id = fonts.add_file(&vo_scene::font::test_font_path()).unwrap();
    (fonts, id)
}

fn style(font: FontId) -> TextStyle {
    TextStyle::new(font, SIZE, SIZE * 1.2)
}

fn at(e: Element<()>, x: f32, y: f32) -> Element<()> {
    e.layout(|s| {
        s.position = taffy::Position::Absolute;
        s.inset = taffy::Rect { left: length(x), top: length(y), right: auto(), bottom: auto() };
    })
}

/// The glyphs of `text` as the label lays them out, in the label's box, starting at `start`.
fn glyphs(fonts: &FontRegistry, font: FontId, text: &str, start: f64) -> Vec<FxGlyph> {
    let laid = ShapedLayout::new(fonts.clone()).layout(text, &style(font), None);
    laid.glyphs.iter().map(|g| FxGlyph::new(g.font, g.glyph, g.pos, SIZE, Color::WHITE, start)).collect()
}

/// A panel with `text` (if any) and GPU elements under and over it.
fn view(font: FontId, text: Option<&str>, under: Option<GlyphFx>, over: Option<GlyphFx>) -> Element<()> {
    let mut root = Element::panel("root").size(W as f32, H as f32).fill(Color::hex(BACK, 1.0));
    if let Some(u) = under {
        // The light covers the whole panel; its glyphs are moved by the text's place.
        root = root.child(at(Element::gpu("under", u.item()).size(W as f32, H as f32), 0.0, 0.0));
    }
    if let Some(text) = text {
        root = root.child(at(Element::label("text", text).text_style(style(font)).color(Color::WHITE), TEXT.0, TEXT.1));
    }
    if let Some(o) = over {
        root = root.child(at(Element::gpu("over", o.item()).size(W as f32 - TEXT.0, SIZE * 1.4), TEXT.0, TEXT.1));
    }
    root
}

fn gpu() -> &'static support::Gpu {
    static GPU: std::sync::OnceLock<support::Gpu> = std::sync::OnceLock::new();
    GPU.get_or_init(support::gpu)
}

fn render(r: &mut Renderer, fonts: &FontRegistry, view: Element<()>, t: f64) -> Vec<u8> {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W as f32, H as f32), 1.0);
    scene.apply(view, 0.0);
    let list = scene.sample(t);
    let gpu = gpu();
    let mut target = Target::new(&gpu.device, W, H, FORMAT);
    r.render_at(&list, 1.0, &mut target);
    vo_3d::readback::read_rgba(&gpu.device, &gpu.queue, target.texture())
}

/// Frames side by side.
fn strip(name: &str, frames: &[Vec<u8>]) -> Image {
    let n = frames.len() as u32;
    let mut px = vec![0u8; (W * n * H * 4) as usize];
    for (i, f) in frames.iter().enumerate() {
        for y in 0..H {
            let src = &f[(y * W * 4) as usize..((y + 1) * W * 4) as usize];
            let at = ((y * W * n + i as u32 * W) * 4) as usize;
            px[at..at + (W * 4) as usize].copy_from_slice(src);
        }
    }
    let img = Image { name: name.into(), w: W * n, h: H, channels: 4, px };
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

fn rgb(f: &[u8], x: u32, y: u32) -> [i32; 3] {
    let i = ((y * W + x) * 4) as usize;
    [f[i] as i32, f[i + 1] as i32, f[i + 2] as i32]
}

/// Pixels where blue clearly beats red: ink.
fn blue_pixels(f: &[u8]) -> usize {
    (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let c = rgb(f, x, y);
            c[2] > c[0] + 40 && c[2] > 60
        })
        .count()
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
    n as f64 / (W * H) as f64
}

fn renderer(fonts: &FontRegistry) -> Renderer {
    let gpu = gpu();
    let mut r = Renderer::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    vo_3d::install(&mut r);
    r
}

#[test]
fn ink_blooms_out_of_each_letter() {
    let (fonts, font) = fonts();
    let mut r = renderer(&fonts);
    let text = "Ink";
    let fx = |reduced| {
        GlyphFx::new(Effect::Ink(Ink::default()), glyphs(&fonts, font, text, 1.0)).fonts(fonts.clone()).reduced(reduced)
    };
    let plain = render(&mut r, &fonts, view(font, Some(text), None, None), 1.0);
    let mut frames = Vec::new();
    for age in [0.0, 0.085, 0.17, 0.255, 0.34, 1.2] {
        frames.push(render(&mut r, &fonts, view(font, Some(text), None, Some(fx(false))), 1.0 + age));
    }
    let still = render(&mut r, &fonts, view(font, Some(text), None, Some(fx(true))), 1.0);
    frames.push(still.clone());
    // Ink shows at once, spreads, then the letters clean up.
    let ink: Vec<usize> = frames.iter().map(|f| blue_pixels(f)).collect();
    assert!(ink[1] > 500, "no ink in the bloom: {ink:?}");
    assert!(ink[4] < ink[1], "the ink does not clear: {ink:?}");
    // Long after, the overlay draws nothing.
    let gone = render(&mut r, &fonts, view(font, Some(text), None, Some(fx(false))), 5.0);
    assert!(differ(&gone, &plain, 1) < 1e-4, "ink left after the trace ends");
    // Reduced motion: only the faint trace, inside the letters.
    assert!(blue_pixels(&still) < ink[1] / 2, "reduced motion shows the bloom");
    support::check(&strip("glyph-fx-ink", &frames), Tolerance { per_pixel: 4, max_fraction: 0.01 }).unwrap();
}

#[test]
fn light_casts_shadows_and_caustics() {
    let (fonts, font) = fonts();
    let mut r = renderer(&fonts);
    let text = "DICTATE";
    let moved: Vec<FxGlyph> = glyphs(&fonts, font, text, 0.0)
        .into_iter()
        .map(|g| FxGlyph { pos: Point::new(g.pos.x + TEXT.0, g.pos.y + TEXT.1), ..g })
        .collect();
    let light = Light::default();
    let under =
        |reduced| GlyphFx::new(Effect::Light(light, Side::Under), moved.clone()).fonts(fonts.clone()).reduced(reduced);
    let over = |reduced| {
        GlyphFx::new(Effect::Light(light, Side::Over), glyphs(&fonts, font, text, 0.0))
            .fonts(fonts.clone())
            .reduced(reduced)
    };
    let plain = render(&mut r, &fonts, view(font, Some(text), None, None), 1.0);
    let mut frames = Vec::new();
    for t in [0.5, 3.5, 7.0, 10.5] {
        frames.push(render(&mut r, &fonts, view(font, Some(text), Some(under(false)), Some(over(false))), t));
    }
    frames.push(render(&mut r, &fonts, view(font, Some(text), Some(under(true)), Some(over(true))), 0.0));
    // The light lights the panel, and it moves.
    let back = rgb(&plain, 5, 5);
    for f in &frames {
        let lit =
            (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).filter(|&(x, y)| rgb(f, x, y)[2] > back[2] + 25).count();
        assert!(lit > 2000, "the light lights too little: {lit}");
    }
    assert!(differ(&frames[0], &frames[2], 3) > 0.05, "the light does not move");
    // The text itself is unchanged where the light is far: the reading line keeps its ink.
    support::check(&strip("glyph-fx-light", &frames), Tolerance { per_pixel: 4, max_fraction: 0.01 }).unwrap();
}

#[test]
fn morph_flows_from_old_word_to_new() {
    let (fonts, font) = fonts();
    let mut r = renderer(&fonts);
    let (old, new) = ("teh", "tor");
    let mut gs: Vec<FxGlyph> = Vec::new();
    let a = glyphs(&fonts, font, old, 1.0);
    let b = glyphs(&fonts, font, new, 1.0);
    // The t is shared; the rest flows.
    gs.push(b[0]);
    gs.extend(a[1..].iter().map(|g| g.part(Part::Old, 0)));
    gs.extend(b[1..].iter().map(|g| g.part(Part::New, 0)));
    let m = Morph::default();
    let fx = |reduced| GlyphFx::new(Effect::Morph(m), gs.clone()).fonts(fonts.clone()).reduced(reduced);
    let plain_old = render(&mut r, &fonts, view(font, Some(old), None, None), 1.0);
    let plain_new = render(&mut r, &fonts, view(font, Some(new), None, None), 1.0);
    let mut frames = Vec::new();
    for k in [0.0, 0.3, 0.5, 0.7, 1.0] {
        frames.push(render(&mut r, &fonts, view(font, None, None, Some(fx(false))), 1.0 + k * m.secs as f64));
    }
    frames.push(render(&mut r, &fonts, view(font, None, None, Some(fx(true))), 1.0));
    let sheet = strip("glyph-fx-morph", &frames);
    // It starts as the old word and ends as the new one, drawn from the fields.
    assert!(
        differ(&frames[0], &plain_old, 60) < 0.004,
        "the start is not the old word: {}",
        differ(&frames[0], &plain_old, 60)
    );
    assert!(
        differ(&frames[4], &plain_new, 60) < 0.004,
        "the end is not the new word: {}",
        differ(&frames[4], &plain_new, 60)
    );
    // Mid-way it is neither, and still has plenty of ink.
    let (mo, mn) = (differ(&frames[2], &plain_old, 60), differ(&frames[2], &plain_new, 60));
    assert!(mo > 0.004 && mn > 0.004, "the middle is one of the words: {mo} {mn}");
    let ink = |f: &[u8]| f.as_chunks::<4>().0.iter().filter(|p| p[0] > 128).count();
    assert!(ink(&frames[2]) as f64 > 0.7 * ink(&plain_new) as f64, "the middle loses its ink");
    // Reduced motion: the new word at once.
    assert!(differ(&frames[5], &plain_new, 60) < 0.004);
    support::check(&sheet, Tolerance { per_pixel: 4, max_fraction: 0.01 }).unwrap();
}

/// The light's colour and accent take their alpha as strength: at half alpha the light adds
/// half as much to the panel, and an accent at alpha 0 leaves the letters as they were.
#[test]
fn light_takes_its_colours_alpha() {
    let (fonts, font) = fonts();
    let mut r = renderer(&fonts);
    let text = "DICTATE";
    let moved: Vec<FxGlyph> = glyphs(&fonts, font, text, 0.0)
        .into_iter()
        .map(|g| FxGlyph { pos: Point::new(g.pos.x + TEXT.0, g.pos.y + TEXT.1), ..g })
        .collect();
    // Shadows darken too; leave them out so the sum is the light alone.
    let light =
        |alpha: f32| Light { color: Color { a: alpha, ..Light::default().color }, shadow: 0.0, ..Light::default() };
    let under =
        |l: Light| GlyphFx::new(Effect::Light(l, Side::Under), moved.clone()).fonts(fonts.clone()).reduced(true);
    let plain = render(&mut r, &fonts, view(font, None, None, None), 0.0);
    // In linear light: the target is sRGB, where half the light reads as more than half.
    let lin = |v: i32| {
        let c = v as f64 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let added = |f: &[u8]| -> f64 {
        (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .map(|(x, y)| lin(rgb(f, x, y)[2]) - lin(rgb(&plain, x, y)[2]))
            .sum()
    };
    let full = added(&render(&mut r, &fonts, view(font, None, Some(under(light(1.0))), None), 0.0));
    let half = added(&render(&mut r, &fonts, view(font, None, Some(under(light(0.5))), None), 0.0));
    assert!(full > 50.0, "the light adds too little: {full}");
    assert!((half / full - 0.5).abs() < 0.12, "half alpha adds {:.2} of full", half / full);
    // An accent at alpha 0 paints nothing over the letters.
    let text_only = render(&mut r, &fonts, view(font, Some(text), None, None), 0.0);
    let clear = Light { accent: Color { a: 0.0, ..Light::default().accent }, ..Light::default() };
    let over = GlyphFx::new(Effect::Light(clear, Side::Over), glyphs(&fonts, font, text, 0.0))
        .fonts(fonts.clone())
        .reduced(true);
    let with_over = render(&mut r, &fonts, view(font, Some(text), None, Some(over)), 0.0);
    assert!(differ(&text_only, &with_over, 2) < 0.001, "a clear accent changed the letters");
}
