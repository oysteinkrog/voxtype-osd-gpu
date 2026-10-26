//! Golden images of `Renderer` drawing display lists, through `Offscreen` readback.
//!
//! GPU test: `just test-gpu` runs it. Goldens are RGBA PNGs of the premultiplied bytes as
//! read back from an sRGB target, so they show darker edges in an image viewer; the contact
//! sheet in /var/tmp/claude/vo-golden/ composites them over gray.

mod support;

use skrifa::instance::{LocationRef, Size as FontSize};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, MetadataProvider};
use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::{
    BlendMode, DisplayList, GlyphInstance, Item, ItemKind, Layer, LayerTarget, Paint, ShapeInstance, NO_WORD,
};
use vo_scene::geom::{Affine, Color, Point, RRect, Rect, Size};

const FONT: &str = "/usr/share/fonts/noto/NotoSans-Regular.ttf";

/// A layer tree to flatten into a display list.
struct L {
    layer: Layer,
    content: Vec<C>,
}

enum C {
    Shapes(Vec<ShapeInstance>),
    Glyphs(Vec<GlyphInstance>),
    Layer(L),
}

impl L {
    fn new(content: Vec<C>) -> L {
        L { layer: Layer::root(), content }
    }

    fn with(mut self, f: impl FnOnce(&mut Layer)) -> L {
        f(&mut self.layer);
        self
    }

    fn list(self, w: f32, h: f32) -> DisplayList {
        let mut list = DisplayList { size: Size::new(w, h), scale: 1.0, ..Default::default() };
        flatten(self, None, &Affine::IDENTITY, &mut list);
        list
    }
}

/// Pushes `node` and its subtree in pre-order. `layer.transform` is taken as relative to the
/// parent and composed here, as the scene does.
fn flatten(node: L, parent: Option<u32>, parent_m: &Affine, list: &mut DisplayList) -> u32 {
    let idx = list.layers.len() as u32;
    let m = node.layer.transform.then(parent_m);
    list.layers.push(Layer { parent, transform: m, ..node.layer });
    let mut items = Vec::new();
    for c in node.content {
        match c {
            C::Shapes(s) => {
                items.push(Item { kind: ItemKind::Shapes, start: list.shapes.len() as u32, len: s.len() as u32 });
                list.shapes.extend(s);
            }
            C::Glyphs(g) => {
                items.push(Item { kind: ItemKind::Glyphs, start: list.glyphs.len() as u32, len: g.len() as u32 });
                list.glyphs.extend(g);
            }
            C::Layer(child) => {
                let ci = flatten(child, Some(idx), &m, list);
                items.push(Item { kind: ItemKind::Layer, start: ci, len: 1 });
            }
        }
    }
    let start = list.items.len() as u32;
    list.items.extend(items);
    list.layers[idx as usize].items = start..list.items.len() as u32;
    idx
}

fn solid(rgb: u32) -> Paint {
    Paint::solid(Color::hex(rgb, 1.0))
}

fn rr(x: f32, y: f32, w: f32, h: f32, r: [f32; 4]) -> RRect {
    RRect { rect: Rect::new(x, y, w, h), radii: r }
}

/// Every shape kind, filled, stroked and both.
fn shapes_scene() -> DisplayList {
    L::new(vec![C::Shapes(vec![
        ShapeInstance::rrect(rr(8.0, 8.0, 70.0, 44.0, [0.0, 6.0, 18.0, 30.0]), solid(0x3b82f6)),
        ShapeInstance::rrect(rr(88.0, 8.0, 70.0, 44.0, [12.0; 4]), solid(0x111827)).with_stroke(solid(0xf59e0b), 3.0),
        ShapeInstance::rrect(rr(168.0, 8.0, 64.0, 44.0, [4.0, 22.0, 4.0, 22.0]), Paint::NONE)
            .with_stroke(solid(0x10b981), 1.5),
        ShapeInstance::circle(Point::new(30.0, 86.0), 22.0, solid(0xef4444)),
        ShapeInstance::circle(Point::new(80.0, 86.0), 18.5, Paint::solid(Color::hex(0xffffff, 0.5)))
            .with_stroke(solid(0xffffff), 2.0),
        ShapeInstance::capsule(Rect::new(108.0, 70.0, 60.0, 22.0), solid(0x8b5cf6)),
        ShapeInstance::capsule(Rect::new(174.0, 64.0, 16.0, 48.0), solid(0xec4899)),
        ShapeInstance::line(Point::new(198.0, 66.0), Point::new(232.0, 110.0), 3.0, solid(0xfacc15)),
        ShapeInstance::line(Point::new(108.0, 104.0), Point::new(168.0, 104.0), 0.6, solid(0xffffff)),
        ShapeInstance::line(
            Point::new(108.0, 112.0),
            Point::new(168.0, 112.0),
            4.0,
            Paint::solid(Color::hex(0x38bdf8, 0.7)),
        ),
    ])])
    .list(240.0, 120.0)
}

/// Hairlines at fractional positions: rect borders, a circle and plain rules.
fn hairline_scene() -> DisplayList {
    let white = solid(0xffffff);
    L::new(vec![C::Shapes(vec![
        ShapeInstance::rrect(rr(10.3, 10.7, 60.0, 40.4, [0.0; 4]), Paint::NONE).with_hairline(white),
        ShapeInstance::rrect(rr(80.6, 10.2, 70.0, 40.0, [10.0; 4]), solid(0x1f2937)).with_hairline(white),
        ShapeInstance::circle(Point::new(185.4, 30.3), 18.0, Paint::NONE).with_hairline(white),
        ShapeInstance::line(Point::new(10.0, 64.3), Point::new(230.0, 64.3), 0.0, white).with_hairline(white),
        ShapeInstance::line(Point::new(10.0, 72.8), Point::new(230.0, 72.8), 0.0, white).with_hairline(white),
        ShapeInstance::line(Point::new(120.45, 80.0), Point::new(120.45, 112.0), 0.0, white).with_hairline(white),
    ])])
    .list(240.0, 120.0)
}

fn gradient_scene() -> DisplayList {
    let red = Color::hex(0xff0000, 1.0);
    let blue = Color::hex(0x0000ff, 1.0);
    L::new(vec![C::Shapes(vec![
        ShapeInstance::rrect(
            rr(8.0, 8.0, 224.0, 40.0, [8.0; 4]),
            Paint::linear(Point::new(8.0, 0.0), red, Point::new(232.0, 0.0), blue),
        ),
        ShapeInstance::rrect(
            rr(8.0, 56.0, 224.0, 24.0, [12.0; 4]),
            Paint::linear(
                Point::new(8.0, 0.0),
                Color::hex(0xffffff, 1.0),
                Point::new(232.0, 0.0),
                Color::hex(0xffffff, 0.0),
            ),
        ),
        ShapeInstance::circle(
            Point::new(40.0, 100.0),
            16.0,
            Paint::linear(
                Point::new(40.0, 84.0),
                Color::hex(0xfde047, 1.0),
                Point::new(40.0, 116.0),
                Color::hex(0x7c3aed, 1.0),
            ),
        )
        .with_stroke(Paint::linear(Point::new(24.0, 0.0), blue, Point::new(56.0, 0.0), red), 2.0),
    ])])
    .list(240.0, 120.0)
}

/// A filled waveform from bands: a mirrored envelope whose segments share their ends, with a
/// gradient stepped per band, then a flat run of bands at fractional ends.
fn bands_scene() -> DisplayList {
    let mut shapes = Vec::new();
    let n = 60;
    let (x0, w, mid) = (8.0f32, 224.0f32, 34.0f32);
    let level = |i: usize| {
        let t = i as f32 / n as f32;
        (0.5 + 0.45 * (t * 17.0).sin() * (t * 5.0).cos()).abs() * 24.0 + 0.5
    };
    let color = |x: f32| {
        let t = ((x - x0) / w).clamp(0.0, 1.0);
        Color::rgba(0.2 + 0.6 * t, 0.6, 0.9 - 0.5 * t, (t * 5.0).min(1.0))
    };
    for i in 0..n {
        let (a, b) = (x0 + w * i as f32 / n as f32, x0 + w * (i + 1) as f32 / n as f32);
        let paint = Paint::linear(Point::new(a, 0.0), color(a), Point::new(b, 0.0), color(b));
        shapes.push(ShapeInstance::band(a, b, mid, level(i), level(i + 1), paint));
    }
    for i in 0..37 {
        let (a, b) = (8.0 + i as f32 * 6.07, 8.0 + (i + 1) as f32 * 6.07);
        shapes.push(ShapeInstance::band(a, b, 92.0, 10.0, 10.0, solid(0xf8fafc)));
    }
    L::new(vec![C::Shapes(shapes)]).list(240.0, 112.0)
}

/// Lays `text` out with cmap and advances (no shaping); one word id per word.
fn run(font: &FontRef, text: &str, x: f32, y: f32, size: f32, color: Color, first_word: u32) -> Vec<GlyphInstance> {
    let upem = font.head().unwrap().units_per_em() as f32;
    let cmap = font.charmap();
    let adv = font.glyph_metrics(FontSize::unscaled(), LocationRef::default());
    let mut pen = x;
    let mut word = first_word;
    let mut out = Vec::new();
    for ch in text.chars() {
        let gid = cmap.map(ch).unwrap_or_default();
        if ch == ' ' {
            word += 1;
        }
        let w = if ch == ' ' { NO_WORD } else { word };
        out.push(GlyphInstance { pos: Point::new(pen, y), size, font: 0, glyph: gid.to_u32(), word: w, color });
        pen += adv.advance_width(gid).unwrap_or(0.0) * size / upem;
    }
    out
}

fn text_scene(font: &FontRef) -> DisplayList {
    L::new(vec![
        C::Shapes(vec![ShapeInstance::rrect(rr(4.0, 4.0, 232.0, 72.0, [10.0; 4]), solid(0x111827))]),
        C::Glyphs(run(font, "Hamburgefonstiv 0123", 12.0, 30.0, 18.0, Color::hex(0xffffff, 1.0), 0)),
        C::Glyphs(run(font, "small text 11 px", 12.0, 48.0, 11.0, Color::hex(0x9ca3af, 1.0), 10)),
        C::Glyphs(run(font, "Half alpha", 12.0, 68.0, 16.0, Color::hex(0xf59e0b, 0.5), 20)),
    ])
    .list(240.0, 80.0)
}

/// A direct layer and an offscreen layer, both at half opacity over overlapping shapes, a
/// rotated clipped child inside the offscreen layer, and an additive layer.
fn layers_scene(font: &FontRef) -> DisplayList {
    let pair = |x: f32| {
        C::Shapes(vec![
            ShapeInstance::rrect(rr(x, 12.0, 50.0, 40.0, [6.0; 4]), solid(0xef4444)),
            ShapeInstance::rrect(rr(x + 25.0, 30.0, 50.0, 40.0, [6.0; 4]), solid(0x3b82f6)),
        ])
    };
    let rotated = L::new(vec![
        C::Shapes(vec![ShapeInstance::rrect(rr(-30.0, -12.0, 60.0, 24.0, [0.0; 4]), solid(0x22c55e))]),
        C::Glyphs(run(font, "clip", -14.0, 6.0, 16.0, Color::WHITE, 0)),
    ])
    .with(|l| {
        l.transform = Affine::rotate(0.35).then(&Affine::translate(160.0, 96.0));
        l.clip = Some(rr(-26.0, -10.0, 52.0, 20.0, [8.0; 4]));
    });
    L::new(vec![
        C::Shapes(vec![ShapeInstance::rrect(rr(0.0, 0.0, 240.0, 130.0, [0.0; 4]), solid(0x1f2937))]),
        C::Layer(L::new(vec![pair(8.0)]).with(|l| l.opacity = 0.5)),
        C::Layer(L::new(vec![pair(120.0), C::Layer(rotated)]).with(|l| {
            l.opacity = 0.5;
            l.target = LayerTarget::Offscreen { effect: 0 };
        })),
        C::Layer(
            L::new(vec![C::Shapes(vec![
                ShapeInstance::circle(Point::new(40.0, 100.0), 18.0, solid(0x404040)),
                ShapeInstance::circle(Point::new(60.0, 100.0), 18.0, solid(0x404040)),
            ])])
            .with(|l| l.blend = BlendMode::Add),
        ),
    ])
    .list(240.0, 130.0)
}

fn load_font() -> Vec<u8> {
    std::fs::read(FONT).unwrap_or_else(|e| panic!("{FONT}: {e} (install noto-fonts)"))
}

fn offscreen(gpu: &support::Gpu, font: &[u8]) -> Offscreen {
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    assert_eq!(off.renderer.add_font(font.to_vec()).unwrap(), 0);
    off
}

fn size_at(list: &DisplayList, scale: f32) -> [u32; 2] {
    [(list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32]
}

fn render(off: &mut Offscreen, name: &str, list: &DisplayList, scale: f32) -> Image {
    let [w, h] = size_at(list, scale);
    let px = off.render_to_rgba(list, [w, h], scale);
    Image { name: name.into(), w, h, channels: 4, px }
}

#[test]
fn display_list_goldens() {
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let shapes = shapes_scene();
    let hair = hairline_scene();
    let images = [
        render(&mut off, "dl-shapes-1x", &shapes, 1.0),
        render(&mut off, "dl-shapes-1.5x", &shapes, 1.5),
        render(&mut off, "dl-hairlines-1x", &hair, 1.0),
        render(&mut off, "dl-hairlines-1.5x", &hair, 1.5),
        render(&mut off, "dl-gradient", &gradient_scene(), 1.0),
        render(&mut off, "dl-text", &text_scene(&font), 1.0),
        render(&mut off, "dl-text-1.5x", &text_scene(&font), 1.5),
        render(&mut off, "dl-layers", &layers_scene(&font), 1.0),
        render(&mut off, "dl-bands", &bands_scene(), 1.0),
        render(&mut off, "dl-bands-1.5x", &bands_scene(), 1.5),
    ];
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// The hairline rule: one row (or column) at full coverage and nothing on either side.
#[test]
fn hairlines_are_one_full_pixel() {
    let data = load_font();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let list = hairline_scene();
    for scale in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
        let [w, h] = size_at(&list, scale);
        let px = off.render_to_rgba(&list, [w, h], scale);
        let alpha = |x: u32, y: u32| px[((y * w + x) * 4 + 3) as usize];
        let check_column = |x: u32, y_lo: f32, y_hi: f32, what: &str| {
            let rows: Vec<u32> = ((y_lo * scale) as u32..(y_hi * scale) as u32).collect();
            let lit: Vec<(u32, u8)> = rows.iter().map(|&y| (y, alpha(x, y))).filter(|&(_, a)| a > 0).collect();
            assert!(lit.len() == 1 && lit[0].1 == 255, "scale {scale}, {what}: column {x} has {lit:?}");
        };
        let check_row = |y: u32, x_lo: f32, x_hi: f32, what: &str| {
            let cols: Vec<u32> = ((x_lo * scale) as u32..(x_hi * scale) as u32).collect();
            let lit: Vec<(u32, u8)> = cols.iter().map(|&x| (x, alpha(x, y))).filter(|&(_, a)| a > 0).collect();
            assert!(lit.len() == 1 && lit[0].1 == 255, "scale {scale}, {what}: row {y} has {lit:?}");
        };
        let at = |v: f32| (v * scale) as u32;
        check_column(at(100.0), 58.0, 69.0, "rule at y 64.3");
        check_column(at(100.0), 69.0, 78.0, "rule at y 72.8");
        check_row(at(96.0), 110.0, 130.0, "rule at x 120.45");
        check_column(at(40.0), 5.0, 20.0, "rect top edge");
        check_row(at(30.0), 5.0, 20.0, "rect left edge");
        // The rounded rect is filled, so check that its edge row is the first lit one and full.
        let x = at(115.0);
        let first = (at(5.0)..at(20.0)).map(|y| (y, alpha(x, y))).find(|&(_, a)| a > 0);
        assert!(first.is_some_and(|(_, a)| a == 255), "scale {scale}, filled rect top edge: {first:?}");
    }
}

/// Bands that share an end cover the pixels between them fully: no seam where they meet.
#[test]
fn bands_tile_with_no_seam() {
    let data = load_font();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let list = bands_scene();
    for scale in [1.0f32, 1.25, 1.5, 2.0] {
        let [w, h] = size_at(&list, scale);
        let px = off.render_to_rgba(&list, [w, h], scale);
        let alpha = |x: u32, y: u32| px[((y * w + x) * 4 + 3) as usize];
        // The flat run spans x 8 to 232.6 and y 82 to 102; look inside it, off the edges.
        let row = (92.0 * scale) as u32;
        let gaps: Vec<(u32, u8)> = ((9.0 * scale) as u32..(231.0 * scale) as u32)
            .map(|x| (x, alpha(x, row)))
            .filter(|&(_, a)| a != 255)
            .collect();
        assert!(gaps.is_empty(), "scale {scale}: seams at {gaps:?}");
    }
}

#[test]
fn rendering_is_deterministic() {
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let list = layers_scene(&font);
    let mut text = text_scene(&font);
    text.scale = 1.5;
    let mut a = offscreen(&gpu, &data);
    let mut b = offscreen(&gpu, &data);
    for (list, scale) in [(&list, 1.0), (&list, 1.5), (&text, 1.5)] {
        let size = size_at(list, scale);
        let first = a.render_to_rgba(list, size, scale);
        let second = a.render_to_rgba(list, size, scale);
        let fresh = b.render_to_rgba(list, size, scale);
        assert!(first == second, "same renderer, two renders differ at scale {scale}");
        assert!(first == fresh, "two renderers differ at scale {scale}");
    }
    // The ring: several frames in flight come back in order and match one-shot renders.
    let size = size_at(&list, 1.0);
    let one = a.render_to_rgba(&list, size, 1.0);
    let mut frames = Vec::new();
    for _ in 0..5 {
        frames.extend(a.submit(&list, size, 1.0));
    }
    frames.extend(a.finish());
    assert_eq!(frames.len(), 5);
    assert!(frames.iter().all(|f| f.rgba == one));
}

/// The CPU renderer, which draws the first frame while the GPU starts, matches the GPU path:
/// shapes to within rounding, text to within anti-aliasing differences.
#[cfg(feature = "cpu")]
#[test]
fn cpu_matches_gpu() {
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let fonts = vo_scene::font::FontRegistry::new();
    fonts.add(data.clone()).unwrap();
    let mut cpu = vo_render::cpu::CpuRenderer::new(fonts);
    // (scene, scale, largest channel difference allowed, mean difference allowed)
    let cases = [
        ("shapes", shapes_scene(), 1.0, 2, 0.05),
        ("shapes-1.5x", shapes_scene(), 1.5, 2, 0.05),
        ("hairlines", hairline_scene(), 1.0, 2, 0.05),
        ("gradient", gradient_scene(), 1.0, 3, 0.2),
        ("bands", bands_scene(), 1.0, 3, 0.2),
        ("text", text_scene(&font), 1.0, 96, 1.0),
    ];
    let mut report = Vec::new();
    for (name, list, scale, max_ok, mean_ok) in cases {
        let [w, h] = size_at(&list, scale);
        let want = off.render_to_rgba(&list, [w, h], scale);
        let mut got = vec![0u8; (w * h * 4) as usize];
        cpu.draw_bgra(&list, scale, w, h, w as usize * 4, &mut got);
        for px in got.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        let diffs: Vec<u32> = want.iter().zip(&got).map(|(a, b)| a.abs_diff(*b) as u32).collect();
        let max = *diffs.iter().max().unwrap();
        let mean = diffs.iter().sum::<u32>() as f64 / diffs.len() as f64;
        report.push(format!("{name}: max {max}, mean {mean:.3}"));
        assert!(max <= max_ok && mean <= mean_ok, "{name}: CPU differs from GPU: max {max}, mean {mean:.3}");
    }
    eprintln!("cpu vs gpu: {}", report.join("; "));
}
