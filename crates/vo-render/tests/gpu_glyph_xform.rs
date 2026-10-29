//! Per-glyph transforms (EG-15): each glyph turns, leans and moves on its own in the one
//! glyph draw.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use skrifa::instance::{LocationRef, Size as FontSize};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, MetadataProvider};
use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::{DisplayList, GlyphInstance, GlyphXform, Item, ItemKind, Layer, Paint, ShapeInstance, NO_WORD};
use vo_scene::geom::{Affine, Color, Point, RRect, Rect, Size};

const FONT: &str = "/usr/share/fonts/noto/NotoSans-Regular.ttf";

fn load_font() -> Vec<u8> {
    std::fs::read(FONT).unwrap_or_else(|e| panic!("{FONT}: {e} (install noto-fonts)"))
}

fn offscreen(gpu: &support::Gpu, font: &[u8]) -> Offscreen {
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    assert_eq!(off.renderer.add_font(font.to_vec()).unwrap(), 0);
    off
}

/// Glyphs of `text` from `x` on the baseline `y`, the n-th glyph given `xf(n)`.
fn run(
    font: &FontRef,
    text: &str,
    x: f32,
    y: f32,
    size: f32,
    color: Color,
    xf: impl Fn(usize) -> GlyphXform,
) -> Vec<GlyphInstance> {
    let upem = font.head().unwrap().units_per_em() as f32;
    let cmap = font.charmap();
    let adv = font.glyph_metrics(FontSize::unscaled(), LocationRef::default());
    let mut pen = x;
    let mut out = Vec::new();
    for (n, ch) in text.chars().enumerate() {
        let gid = cmap.map(ch).unwrap_or_default();
        out.push(GlyphInstance {
            pos: Point::new(pen, y),
            size,
            font: 0,
            glyph: gid.to_u32(),
            word: NO_WORD,
            color,
            xform: xf(n),
        });
        pen += adv.advance_width(gid).unwrap_or(0.0) * size / upem;
    }
    out
}

/// One root layer over a backdrop of `bg`, holding `glyphs`.
fn list(w: f32, h: f32, bg: Color, glyphs: Vec<GlyphInstance>, transform: Affine) -> DisplayList {
    let backdrop = ShapeInstance::rrect(RRect::new(Rect::new(0.0, 0.0, w, h), 0.0), Paint::solid(bg));
    let mut root = Layer::root();
    root.items = 0..2;
    root.transform = transform;
    DisplayList {
        size: Size::new(w, h),
        scale: 1.0,
        layers: vec![root],
        items: vec![
            Item { kind: ItemKind::Shapes, start: 0, len: 1 },
            Item { kind: ItemKind::Glyphs, start: 0, len: glyphs.len() as u32 },
        ],
        shapes: vec![backdrop],
        glyphs,
        ..Default::default()
    }
}

/// Three rows: each letter turned a different amount, a leaning row and a row moved letter
/// by letter on a wave.
fn xform_scene(font: &FontRef, bg: Color, ink: Color) -> DisplayList {
    let mut g = run(font, "Turning", 10.0, 34.0, 22.0, ink, |n| GlyphXform::rotate(n as f32 * 30.0 - 45.0));
    g.extend(run(font, "Leaning 0123", 10.0, 66.0, 18.0, ink, |n| GlyphXform {
        skew_deg: (n as f32 * 4.0) - 12.0,
        ..GlyphXform::IDENTITY
    }));
    g.extend(run(font, "on a wave", 10.0, 96.0, 16.0, ink, |n| GlyphXform {
        rot_deg: (n as f32 * 0.9).sin() * 12.0,
        offset: Point::new(0.0, (n as f32 * 0.9).cos() * 4.0),
        ..GlyphXform::IDENTITY
    }));
    list(180.0, 110.0, bg, g, Affine::IDENTITY)
}

fn render(off: &mut Offscreen, name: &str, list: &DisplayList, scale: f32) -> Image {
    let [w, h] = [(list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32];
    let px = off.render_to_rgba(list, [w, h], scale);
    Image { name: name.into(), w, h, channels: 4, px }
}

const DARK: Color = Color { r: 0.07, g: 0.09, b: 0.15, a: 1.0 };
const LIGHT: Color = Color { r: 0.96, g: 0.96, b: 0.94, a: 1.0 };

#[test]
fn glyph_xform_goldens() {
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let dark = xform_scene(&font, DARK, Color::WHITE);
    let light = xform_scene(&font, LIGHT, Color::hex(0x111827, 1.0));
    let images = [
        render(&mut off, "glyph-xform-1x", &dark, 1.0),
        render(&mut off, "glyph-xform-1.5x", &dark, 1.5),
        render(&mut off, "glyph-xform-light", &light, 1.0),
    ];
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// One `R` (no symmetry) with its ink box centred on the image centre, turned by `deg`.
fn single(font: &FontRef, deg: f32, side: f32) -> DisplayList {
    let gid = font.charmap().map('R').unwrap();
    let size = 40.0;
    let upem = font.head().unwrap().units_per_em() as f32;
    let k = size / upem;
    let b = font.glyph_metrics(FontSize::unscaled(), LocationRef::default()).bounds(gid).unwrap();
    let c = side / 2.0;
    // Pen position that puts the ink box centre on (c, c).
    let pos = Point::new(c - k * 0.5 * (b.x_min + b.x_max), c + k * 0.5 * (b.y_min + b.y_max));
    let g = GlyphInstance {
        pos,
        size,
        font: 0,
        glyph: gid.to_u32(),
        word: NO_WORD,
        color: Color::WHITE,
        xform: GlyphXform::rotate(deg),
    };
    list(side, side, Color::rgba(0.0, 0.0, 0.0, 0.0), vec![g], Affine::IDENTITY)
}

/// A glyph turned a quarter turn is the upright glyph's image turned a quarter turn
/// clockwise about its ink box centre.
#[test]
fn a_quarter_turn_is_the_upright_image_turned() {
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let side = 64u32;
    let upright = off.render_to_rgba(&single(&font, 0.0, side as f32), [side, side], 1.0);
    let turned = off.render_to_rgba(&single(&font, 90.0, side as f32), [side, side], 1.0);
    let n = side as usize;
    let (mut sum, mut ink_up, mut ink_turned) = (0u64, 0u64, 0u64);
    for y in 0..n {
        for x in 0..n {
            // Clockwise with y down: the pixel at (x, y) comes from (y, n - 1 - x) upright.
            let src = ((n - 1 - x) * n + y) * 4;
            let dst = (y * n + x) * 4;
            for ch in 0..4 {
                sum += turned[dst + ch].abs_diff(upright[src + ch]) as u64;
            }
            ink_up += upright[dst + 3] as u64;
            ink_turned += turned[dst + 3] as u64;
        }
    }
    let mean = sum as f64 / (n * n * 4) as f64;
    eprintln!("quarter turn: mean difference {mean:.3}/255, ink {ink_up} upright, {ink_turned} turned");
    assert!(ink_up > 20_000, "the glyph drew too little ink: {ink_up}");
    assert!(mean <= 2.0, "turned glyph differs from the turned image: mean {mean:.3}/255");
}

/// The CPU first frame turns glyphs the same way, within the text tolerance of
/// `gpu_display::cpu_matches_gpu`.
#[cfg(feature = "cpu")]
#[test]
fn cpu_matches_gpu_for_turned_glyphs() {
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let fonts = vo_scene::font::FontRegistry::new();
    fonts.add(data.clone()).unwrap();
    let mut cpu = vo_render::cpu::CpuRenderer::new(fonts);
    let mut compare = |list: &DisplayList, scale: f32| {
        let [w, h] = [(list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32];
        let want = off.render_to_rgba(list, [w, h], scale);
        let mut got = vec![0u8; (w * h * 4) as usize];
        cpu.draw_bgra(list, scale, w, h, w as usize * 4, &mut got);
        for px in got.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        let diffs: Vec<u32> = want.iter().zip(&got).map(|(a, b)| a.abs_diff(*b) as u32).collect();
        let max = *diffs.iter().max().unwrap();
        (max, diffs.iter().sum::<u32>() as f64 / diffs.len() as f64)
    };
    for scale in [1.0, 1.5] {
        let turned = xform_scene(&font, DARK, Color::WHITE);
        let mut upright = turned.clone();
        for g in &mut upright.glyphs {
            g.xform = GlyphXform::IDENTITY;
        }
        let (max, mean) = compare(&turned, scale);
        let (max0, mean0) = compare(&upright, scale);
        eprintln!("cpu vs gpu at {scale}x: turned max {max}, mean {mean:.3}; upright max {max0}, mean {mean0:.3}");
        assert!(max <= 96.max(max0) && mean <= 1.0, "{scale}x: CPU differs from GPU: max {max}, mean {mean:.3}");
    }
}
