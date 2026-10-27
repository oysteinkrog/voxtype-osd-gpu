//! Effects: glow and drop shadow from the shape shader, and bloom on the effect path.
//! Goldens at 1x and 1.5x, a determinism check across two renderers, and the bloom's
//! stability when a shape moves by fractions of a pixel.
//!
//! GPU test: `just test-gpu` runs it. Goldens hold the premultiplied bytes as read back.

mod support;

use skrifa::instance::{LocationRef, Size as FontSize};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, MetadataProvider};
use support::{Image, Tolerance};
use vo_render::color::srgb_to_linear;
use vo_render::Offscreen;
use vo_scene::display::{
    Bloom, DisplayList, GlyphInstance, Item, ItemKind, Layer, LayerTarget, Paint, ShapeInstance, Trail, NO_WORD,
};
use vo_scene::geom::{Color, Point, RRect, Rect, Size};

const FONT: &str = "/usr/share/fonts/noto/NotoSans-Regular.ttf";

/// A root layer with `content`, then an optional offscreen child layer with bloom.
struct Builder {
    list: DisplayList,
}

impl Builder {
    fn new(w: f32, h: f32) -> Builder {
        let mut list = DisplayList { size: Size::new(w, h), scale: 1.0, ..Default::default() };
        list.layers.push(Layer::root());
        Builder { list }
    }

    /// Items for layer `layer`, added in order: shapes and glyphs.
    fn items(&mut self, layer: u32, shapes: Vec<ShapeInstance>, glyphs: Vec<GlyphInstance>) -> Vec<Item> {
        let mut items = Vec::new();
        if !shapes.is_empty() {
            items.push(Item { kind: ItemKind::Shapes, start: self.list.shapes.len() as u32, len: shapes.len() as u32 });
            self.list.shapes.extend(shapes);
        }
        if !glyphs.is_empty() {
            items.push(Item { kind: ItemKind::Glyphs, start: self.list.glyphs.len() as u32, len: glyphs.len() as u32 });
            self.list.glyphs.extend(glyphs);
        }
        let _ = layer;
        items
    }

    /// Root content below, a bloom layer with its own content, then root content above.
    fn build(
        mut self,
        below: Vec<ShapeInstance>,
        bloom: Option<(Bloom, Vec<ShapeInstance>, Vec<GlyphInstance>)>,
        above: Vec<ShapeInstance>,
    ) -> DisplayList {
        let mut root_items = self.items(0, below, Vec::new());
        let mut child_items = Vec::new();
        if let Some((b, shapes, glyphs)) = bloom {
            self.list.layers.push(Layer {
                parent: Some(0),
                target: LayerTarget::Offscreen { bloom: Some(b) },
                ..Layer::root()
            });
            root_items.push(Item { kind: ItemKind::Layer, start: 1, len: 1 });
            child_items = self.items(1, shapes, glyphs);
        }
        root_items.extend(self.items(0, above, Vec::new()));
        let start = self.list.items.len() as u32;
        self.list.items.extend(root_items);
        self.list.layers[0].items = start..self.list.items.len() as u32;
        if self.list.layers.len() > 1 {
            let start = self.list.items.len() as u32;
            self.list.items.extend(child_items);
            self.list.layers[1].items = start..self.list.items.len() as u32;
        }
        self.list
    }
}

fn hex(rgb: u32) -> Color {
    Color::hex(rgb, 1.0)
}

fn rr(x: f32, y: f32, w: f32, h: f32, r: f32) -> RRect {
    RRect::new(Rect::new(x, y, w, h), r)
}

fn run(font: &FontRef, text: &str, x: f32, y: f32, size: f32, color: Color) -> Vec<GlyphInstance> {
    let upem = font.head().unwrap().units_per_em() as f32;
    let cmap = font.charmap();
    let adv = font.glyph_metrics(FontSize::unscaled(), LocationRef::default());
    let mut pen = x;
    let mut out = Vec::new();
    for ch in text.chars() {
        let gid = cmap.map(ch).unwrap_or_default();
        out.push(GlyphInstance { pos: Point::new(pen, y), size, font: 0, glyph: gid.to_u32(), word: NO_WORD, color });
        pen += adv.advance_width(gid).unwrap_or(0.0) * size / upem;
    }
    out
}

/// A copy of `s` with a halo fill, drawn under it.
fn halo(s: ShapeInstance, fill: Paint) -> ShapeInstance {
    ShapeInstance { fill, stroke: Paint::NONE, flags: 0, ..s }
}

/// Glows on each shape kind over a transparent background, so the glow's alpha shows: a
/// capsule at strength 1 and at 2.5, a circle, a stroked rect whose glow starts at the
/// stroke's outer edge, and a line.
fn glow_scene() -> DisplayList {
    let blue = hex(0x4cc9f0);
    let capsule = ShapeInstance::capsule(Rect::new(16.0, 20.0, 80.0, 24.0), Paint::solid(blue));
    let hot = ShapeInstance::capsule(Rect::new(16.0, 76.0, 80.0, 24.0), Paint::solid(blue));
    let dot = ShapeInstance::circle(Point::new(140.0, 32.0), 12.0, Paint::solid(hex(0xf72585)));
    let ring = ShapeInstance::rrect(rr(120.0, 70.0, 50.0, 36.0, 8.0), Paint::NONE)
        .with_stroke(Paint::solid(hex(0xffd166)), 3.0);
    let line = ShapeInstance::line(Point::new(196.0, 20.0), Point::new(226.0, 104.0), 4.0, Paint::solid(hex(0x80ffdb)));
    let shapes = vec![
        halo(capsule, Paint::glow(blue, 8.0, 1.0)),
        capsule,
        halo(hot, Paint::glow(blue, 8.0, 2.5)),
        hot,
        halo(dot, Paint::glow(hex(0xf72585), 6.0, 1.0)),
        dot,
        halo(ring, Paint::glow(hex(0xffd166), 5.0, 1.0)),
        ring,
        halo(line, Paint::glow(hex(0x80ffdb), 4.0, 1.0)),
        line,
    ];
    Builder::new(240.0, 124.0).build(shapes, None, Vec::new())
}

/// Drop shadows on a light card colour over a light background, a sharp one, a wide one, a
/// circle and a capsule.
fn shadow_scene() -> DisplayList {
    let bg = ShapeInstance::rrect(rr(0.0, 0.0, 240.0, 124.0, 0.0), Paint::solid(hex(0xe5e7eb)));
    let card = |x: f32, y: f32| ShapeInstance::rrect(rr(x, y, 60.0, 40.0, 10.0), Paint::solid(hex(0xffffff)));
    let shadow =
        |c: f32, dy: f32, blur: f32| Paint::shadow(Color::rgba(0.0, 0.0, 0.0, c), Point::new(0.0, dy), blur, 0.0);
    let (a, b) = (card(14.0, 14.0), card(94.0, 14.0));
    let c = ShapeInstance::circle(Point::new(196.0, 34.0), 20.0, Paint::solid(hex(0xffffff)));
    let d = ShapeInstance::capsule(Rect::new(30.0, 80.0, 180.0, 24.0), Paint::solid(hex(0xffffff)));
    let shapes = vec![
        bg,
        halo(a, shadow(0.5, 2.0, 1.5)),
        a,
        halo(b, shadow(0.45, 8.0, 10.0)),
        b,
        halo(c, shadow(0.5, 6.0, 6.0)),
        c,
        halo(d, shadow(0.4, 4.0, 5.0)),
        d,
    ];
    Builder::new(240.0, 124.0).build(shapes, None, Vec::new())
}

/// A dark panel that stops short of the right edge, a bloom layer with a capsule and a word
/// reaching past it (so the halo shows over transparent pixels too), and a plain rule on
/// top, which must stay sharp.
fn bloom_scene(font: &FontRef) -> DisplayList {
    let panel = ShapeInstance::rrect(rr(0.0, 0.0, 190.0, 124.0, 14.0), Paint::solid(hex(0x101420)));
    let capsule = ShapeInstance::capsule(Rect::new(20.0, 22.0, 70.0, 20.0), Paint::solid(hex(0x4cc9f0)));
    let word = run(font, "bloom", 110.0, 44.0, 30.0, hex(0xffd6ff));
    let dot = ShapeInstance::circle(Point::new(200.0, 92.0), 10.0, Paint::solid(hex(0xffffff)));
    let rule = ShapeInstance::rrect(rr(20.0, 100.0, 120.0, 2.0, 0.0), Paint::solid(hex(0x9aa3b8)));
    Builder::new(240.0, 124.0).build(
        vec![panel],
        Some((Bloom { strength: 1.2, radius: 8.0 }, vec![capsule, dot], word)),
        vec![rule],
    )
}

fn load_font() -> Vec<u8> {
    std::fs::read(FONT).unwrap_or_else(|e| panic!("{FONT}: {e} (install noto-fonts)"))
}

fn offscreen(gpu: &support::Gpu, font: &[u8]) -> Offscreen {
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    assert_eq!(off.renderer.add_font(font.to_vec()).unwrap(), 0);
    off
}

fn render(off: &mut Offscreen, name: &str, list: &DisplayList, scale: f32) -> Image {
    let (w, h) = ((list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32);
    let px = off.render_to_rgba(list, [w, h], scale);
    Image { name: name.into(), w, h, channels: 4, px }
}

/// Colour may not exceed alpha in linear light: the output stays valid premultiplied.
fn assert_premultiplied(img: &Image) {
    for (i, p) in img.px.as_chunks::<4>().0.iter().enumerate() {
        let a = p[3] as f32 / 255.0;
        for c in &p[..3] {
            let lin = srgb_to_linear(*c as f32 / 255.0);
            assert!(lin <= a + 2.5 / 255.0, "{}: pixel {i} {p:?} has colour above alpha", img.name);
        }
    }
}

#[test]
fn effect_goldens() {
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let (glow, shadow, bloom) = (glow_scene(), shadow_scene(), bloom_scene(&font));
    let images = [
        render(&mut off, "fx-glow-1x", &glow, 1.0),
        render(&mut off, "fx-glow-1.5x", &glow, 1.5),
        render(&mut off, "fx-shadow-1x", &shadow, 1.0),
        render(&mut off, "fx-shadow-1.5x", &shadow, 1.5),
        render(&mut off, "fx-bloom-1x", &bloom, 1.0),
        render(&mut off, "fx-bloom-1.5x", &bloom, 1.5),
    ];
    for img in &images {
        assert_premultiplied(img);
    }
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// Two renderers, two renders each, interleaved with other scenes: the same bytes.
#[test]
fn effects_are_deterministic() {
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let scenes = [glow_scene(), shadow_scene(), bloom_scene(&font)];
    let mut first = offscreen(&gpu, &data);
    let mut second = offscreen(&gpu, &data);
    for scale in [1.0, 1.5] {
        for (i, list) in scenes.iter().enumerate() {
            let a = render(&mut first, "a", list, scale).px;
            let _ = render(&mut second, "other", &scenes[(i + 1) % scenes.len()], scale);
            let b = render(&mut second, "b", list, scale).px;
            let c = render(&mut first, "c", list, scale).px;
            assert!(a == b && a == c, "scene {i} at {scale}x differs between renders");
        }
    }
}

/// A small bright dot in a bloom layer moved across one pixel in eighths. The halo's total
/// light stays flat and its centroid follows the dot, so moving content does not shimmer.
#[test]
fn bloom_is_stable_under_subpixel_motion() {
    let data = load_font();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let mut energies = Vec::new();
    let mut centroids = Vec::new();
    for k in 0..=8 {
        let x = 60.0 + k as f32 / 8.0;
        let dot = ShapeInstance::circle(Point::new(x, 40.0), 3.0, Paint::solid(hex(0xffffff)));
        let list = Builder::new(128.0, 80.0).build(
            Vec::new(),
            Some((Bloom { strength: 1.0, radius: 10.0 }, vec![dot], Vec::new())),
            Vec::new(),
        );
        let img = render(&mut off, "subpixel", &list, 1.0);
        // Light outside the dot itself: the halo.
        let (mut sum, mut sx) = (0.0f64, 0.0f64);
        for y in 0..img.h {
            for px in 0..img.w {
                let a = img.px[((y * img.w + px) * 4 + 3) as usize] as f64 / 255.0;
                let (dx, dy) = (px as f64 + 0.5 - x as f64, y as f64 + 0.5 - 40.0);
                if dx * dx + dy * dy > 25.0 {
                    sum += a;
                    sx += a * (px as f64 + 0.5);
                }
            }
        }
        energies.push(sum);
        centroids.push(sx / sum - x as f64);
    }
    let mean = energies.iter().sum::<f64>() / energies.len() as f64;
    let spread = energies.iter().map(|e| (e - mean).abs() / mean).fold(0.0, f64::max);
    // Measured 2.3% (the pyramid's grid is fixed to the texture, so it is not exactly
    // translation invariant), with the centroid within 0.06 px.
    assert!(spread < 0.03, "halo light varies by {:.2}% over a pixel: {energies:?}", spread * 100.0);
    let drift = centroids.iter().map(|c| c.abs()).fold(0.0, f64::max);
    assert!(drift < 0.15, "halo centroid lags the dot by up to {drift:.3} px: {centroids:?}");
}

/// A glow stronger than 1 on the direct path is capped at 1, not clipped per channel.
#[test]
fn direct_path_caps_glow_strength() {
    let data = load_font();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let at = |strength: f32, off: &mut Offscreen| {
        let c = ShapeInstance::circle(Point::new(40.0, 40.0), 8.0, Paint::NONE);
        let list =
            Builder::new(80.0, 80.0).build(vec![halo(c, Paint::glow(hex(0x4cc9f0), 6.0, strength))], None, Vec::new());
        render(off, "cap", &list, 1.0).px
    };
    assert_eq!(at(1.0, &mut off), at(3.0, &mut off));
    // The renderer chooses the direct path for a list with no bloom.
    assert!(!off.renderer.last_frame_had_effects());
}

/// Quality tiers: `NoBloom` takes the direct path and draws a bloom layer as a plain layer,
/// `Minimal` also leaves out glow and shadow, and `Full` is the default.
#[test]
fn quality_tiers_drop_effects() {
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    assert_eq!(off.renderer.quality(), vo_render::Quality::Full);
    let bloom = bloom_scene(&font);
    let full = render(&mut off, "q", &bloom, 1.0).px;
    assert!(off.renderer.last_frame_had_effects());
    off.renderer.set_quality(vo_render::Quality::NoBloom);
    let no_bloom = render(&mut off, "q", &bloom, 1.0).px;
    assert!(!off.renderer.last_frame_had_effects());
    assert_ne!(full, no_bloom);
    // Glow stays at NoBloom and goes at Minimal, where only the plain shapes are left.
    let glow = glow_scene();
    let with_glow = render(&mut off, "q", &glow, 1.0).px;
    off.renderer.set_quality(vo_render::Quality::Minimal);
    let without = render(&mut off, "q", &glow, 1.0).px;
    let plain =
        DisplayList { shapes: glow.shapes.iter().filter(|s| !s.fill.is_halo()).copied().collect(), ..glow.clone() };
    // The plain list has fewer shapes, so its single shape item must shrink with it.
    let mut plain = plain;
    plain.items[0].len = plain.shapes.len() as u32;
    let expected = render(&mut off, "q", &plain, 1.0).px;
    assert_ne!(with_glow, without);
    assert_eq!(without, expected);
}

/// Light mode: a bloom is a soft dark shadow under its layer, not added light.
#[test]
fn light_mode_bloom_golden() {
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    off.renderer.set_light(true);
    let bg = ShapeInstance::rrect(rr(0.0, 0.0, 240.0, 124.0, 0.0), Paint::solid(hex(0xf3f4f6)));
    let capsule = ShapeInstance::capsule(Rect::new(20.0, 22.0, 70.0, 20.0), Paint::solid(hex(0x2563eb)));
    let word = run(&font, "light", 110.0, 44.0, 30.0, hex(0x111827));
    let list = Builder::new(240.0, 124.0).build(
        vec![bg],
        Some((Bloom { strength: 1.0, radius: 6.0 }, vec![capsule], word)),
        Vec::new(),
    );
    let img = render(&mut off, "fx-bloom-light-1x", &list, 1.0);
    assert_premultiplied(&img);
    support::check(&img, Tolerance::default()).unwrap();
}

/// Motion blur: a capsule and a word in a layer smeared 24 px to the right, next to the same
/// content at rest.
#[test]
fn motion_blur_golden() {
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let mut list = Builder::new(240.0, 124.0).build(
        vec![ShapeInstance::rrect(rr(0.0, 0.0, 240.0, 124.0, 0.0), Paint::solid(hex(0x101420)))],
        Some((
            Bloom { strength: 0.0, radius: 1.0 },
            vec![ShapeInstance::capsule(Rect::new(20.0, 20.0, 60.0, 20.0), Paint::solid(hex(0x4cc9f0)))],
            run(&font, "swift", 110.0, 40.0, 26.0, hex(0xffffff)),
        )),
        vec![ShapeInstance::capsule(Rect::new(20.0, 76.0, 60.0, 20.0), Paint::solid(hex(0x4cc9f0)))],
    );
    list.layers[1].motion_blur = Point::new(24.0, 0.0);
    let img = render(&mut off, "fx-motion-blur-1x", &list, 1.0);
    assert_premultiplied(&img);
    support::check(&img, Tolerance::default()).unwrap();
    // The smear spreads the capsule: a pixel just past its right end is partly covered.
    let lit = |x: u32| {
        let p = &img.px[((30 * img.w + x) * 4) as usize..][..3];
        p[2] > 60
    };
    assert!(lit(84) && !lit(94), "smear reach");
}

/// A dot on a trail layer at frame `i` of a 60 fps run, moving 16 px a frame.
fn trail_frame(i: u32) -> DisplayList {
    let x = 20.0 + 16.0 * i as f32;
    let mut list = Builder::new(240.0, 60.0).build(
        vec![ShapeInstance::rrect(rr(0.0, 0.0, 240.0, 60.0, 0.0), Paint::solid(hex(0x101420)))],
        Some((
            Bloom { strength: 0.0, radius: 1.0 },
            vec![ShapeInstance::circle(Point::new(x, 30.0), 8.0, Paint::solid(hex(0x4cc9f0)))],
            Vec::new(),
        )),
        Vec::new(),
    );
    list.layers[1].trail = Some(Trail { key: 7, half_life: 0.03, t: i as f64 / 60.0 });
    list
}

#[test]
fn trail_golden() {
    let gpu = support::gpu();
    let data = load_font();
    let run_trail = |name: &str| {
        let mut off = offscreen(&gpu, &data);
        (0..=10).map(|i| render(&mut off, name, &trail_frame(i), 1.0)).last().unwrap()
    };
    let img = run_trail("fx-trail-1x");
    assert_premultiplied(&img);
    support::check(&img, Tolerance::default()).unwrap();
    // Older positions are fainter: the dot is at x = 180, earlier ones every 16 px back.
    let blue = |x: u32| img.px[((30 * img.w + x) * 4 + 2) as usize];
    let tail: Vec<u8> = (0..6).map(|k| blue(180 - 16 * k)).collect();
    assert!(tail.windows(2).all(|w| w[0] > w[1]), "trail fades with age: {tail:?}");
    // Ten frames is 5.6 half lives: about 2% is left, which sRGB shows as some 18 levels over the dark background.
    assert!(blue(20) < blue(0) + 24, "the oldest position has faded: {} over {}", blue(20), blue(0));
    // Deterministic across renderers.
    assert_eq!(img.px, run_trail("fx-trail-again").px, "trail differs between runs");
}

#[test]
fn trail_starts_fresh_after_a_seek_back() {
    let gpu = support::gpu();
    let data = load_font();
    let mut off = offscreen(&gpu, &data);
    for i in 0..=6 {
        render(&mut off, "fx-trail-seek", &trail_frame(i), 1.0);
    }
    let back = render(&mut off, "fx-trail-seek", &trail_frame(2), 1.0);
    let mut fresh_off = offscreen(&gpu, &data);
    let fresh = render(&mut fresh_off, "fx-trail-seek", &trail_frame(2), 1.0);
    assert_eq!(back.px, fresh.px, "a seek backward clears the trail");
}

/// A capsule with a conic rim (FX08) on its outline and a sheen band half way across it.
fn rim_scene() -> DisplayList {
    let pill = Rect::new(20.0, 24.0, 200.0, 48.0);
    let clear = |c: Color| c.fade(0.0);
    let rim = hex(0x7dd3fc);
    let sheen = Color::rgba(1.0, 1.0, 1.0, 0.35);
    Builder::new(240.0, 96.0).build(
        vec![
            ShapeInstance::rrect(rr(0.0, 0.0, 240.0, 96.0, 0.0), Paint::solid(hex(0x101420))),
            ShapeInstance::capsule(pill, Paint::solid(hex(0x1d2433))),
            ShapeInstance::capsule(
                pill,
                Paint::sheen(Point::new(96.0, 48.0), Point::new(15.0, 5.5), sheen, clear(sheen)),
            ),
            ShapeInstance::capsule(pill, Paint::NONE)
                .with_stroke(Paint::conic(Point::new(120.0, 48.0), rim, clear(rim), -0.125, 0.15), 3.0),
        ],
        None,
        Vec::new(),
    )
}

#[test]
fn rim_and_sheen_golden() {
    let data = load_font();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let list = rim_scene();
    let mut failures = Vec::new();
    for (name, scale) in [("fx-rim-sheen-1x", 1.0), ("fx-rim-sheen-1.5x", 1.5)] {
        let img = render(&mut off, name, &list, scale);
        assert_premultiplied(&img);
        failures.extend(support::check(&img, Tolerance::default()).err());
    }
    assert!(failures.is_empty(), "{failures:#?}");
    // The rim is bright at its angle (up and right) and gone on the far side.
    let img = render(&mut off, "rim", &list, 1.0);
    let blue = |x: u32, y: u32| img.px[((y * img.w + x) * 4 + 2) as usize];
    assert!(blue(190, 25) > blue(50, 71) + 60, "rim {} against {}", blue(190, 25), blue(50, 71));
}

/// The first frame's CPU renderer draws the conic and sheen paints as the shader does.
#[cfg(feature = "cpu")]
#[test]
fn rim_and_sheen_on_the_cpu_match_the_gpu() {
    let data = load_font();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let list = rim_scene();
    let img = render(&mut off, "rim", &list, 1.0);
    let fonts = vo_scene::font::FontRegistry::new();
    let mut cpu = vo_render::cpu::CpuRenderer::new(fonts);
    let (w, h) = (img.w, img.h);
    let mut got = vec![0u8; (w * h * 4) as usize];
    cpu.draw_bgra(&list, 1.0, w, h, w as usize * 4, &mut got);
    for px in got.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let max = img.px.iter().zip(&got).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(max <= 3, "CPU differs from GPU by {max}");
}

/// A ticker-like clip (FX04): a bar and a word cut by a clip whose left and right edges fade.
fn fade_scene(font: &FontRef) -> DisplayList {
    let mut list = Builder::new(240.0, 80.0).build(
        vec![ShapeInstance::rrect(rr(0.0, 0.0, 240.0, 80.0, 0.0), Paint::solid(hex(0x101420)))],
        Some((
            Bloom { strength: 0.0, radius: 1.0 },
            vec![ShapeInstance::rrect(rr(0.0, 16.0, 240.0, 12.0, 6.0), Paint::solid(hex(0x4cc9f0)))],
            run(font, "ticker words", 6.0, 62.0, 26.0, hex(0xffffff)),
        )),
        Vec::new(),
    );
    let ticker = &mut list.layers[1];
    ticker.target = LayerTarget::Direct;
    ticker.clip = Some(rr(20.0, 8.0, 200.0, 64.0, 8.0));
    ticker.clip_fade = [40.0, 0.0, 40.0, 0.0];
    list
}

#[test]
fn edge_fade_golden() {
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let list = fade_scene(&font);
    let mut failures = Vec::new();
    for (name, scale) in [("fx-edge-fade-1x", 1.0), ("fx-edge-fade-1.5x", 1.5)] {
        let img = render(&mut off, name, &list, scale);
        assert_premultiplied(&img);
        failures.extend(support::check(&img, Tolerance::default()).err());
    }
    assert!(failures.is_empty(), "{failures:#?}");
    // The bar fades in over the first 40 units of the clip: dim near the edge, full past it.
    let img = render(&mut off, "fade", &list, 1.0);
    let blue = |x: u32| img.px[((22 * img.w + x) * 4 + 2) as usize];
    assert!(
        blue(24) < blue(40) && blue(40) < blue(64) && blue(64) == blue(120),
        "{} {} {} {}",
        blue(24),
        blue(40),
        blue(64),
        blue(120)
    );
    assert_eq!(blue(10), blue(230), "outside the clip both sides show the background");
}

#[cfg(feature = "cpu")]
#[test]
fn edge_fade_on_the_cpu_matches_the_gpu() {
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let list = fade_scene(&font);
    let mut shapes_only = list.clone();
    shapes_only.items.retain(|i| i.kind != ItemKind::Glyphs);
    // Item ranges moved: rebuild each layer's range over the kept items.
    let mut at = 0u32;
    for layer in &mut shapes_only.layers {
        let n = list.items[layer.items.start as usize..layer.items.end as usize]
            .iter()
            .filter(|i| i.kind != ItemKind::Glyphs)
            .count() as u32;
        layer.items = at..at + n;
        at += n;
    }
    let img = render(&mut off, "fade-shapes", &shapes_only, 1.0);
    let mut cpu = vo_render::cpu::CpuRenderer::new(vo_scene::font::FontRegistry::new());
    let (w, h) = (img.w, img.h);
    let mut got = vec![0u8; (w * h * 4) as usize];
    cpu.draw_bgra(&shapes_only, 1.0, w, h, w as usize * 4, &mut got);
    for px in got.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let max = img.px.iter().zip(&got).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(max <= 3, "CPU differs from GPU by {max}");
}

/// One word per glyph effect (TY17), each at a few amounts, on a dark card.
fn glyph_fx_scene(font: &FontRef) -> DisplayList {
    use vo_scene::display::GlyphFx;
    type Row = (&'static str, fn(f32) -> GlyphFx);
    let rows: [Row; 5] = [
        ("blur", |a| GlyphFx { blur: 6.0 * a, ..Default::default() }),
        ("dissolve", |a| GlyphFx { dissolve: a, ..Default::default() }),
        ("wipe", |a| GlyphFx { wipe: a, ..Default::default() }),
        ("chroma", |a| GlyphFx { chroma: 3.0 * a, ..Default::default() }),
        ("halo", |a| GlyphFx { halo: Color::rgba(0.3, 0.8, 1.0, a), halo_radius: 10.0, ..Default::default() }),
    ];
    let mut glyphs = Vec::new();
    let mut fx = Vec::new();
    for (r, (word, f)) in rows.iter().enumerate() {
        for (c, amount) in [0.0, 0.35, 0.7].into_iter().enumerate() {
            let run = run(font, word, 12.0 + 120.0 * c as f32, 30.0 + 34.0 * r as f32, 26.0, hex(0xf2f5fa));
            for g in run {
                let e = f(amount);
                if !e.is_none() {
                    fx.push((glyphs.len() as u32, e));
                }
                glyphs.push(g);
            }
        }
    }
    let mut list = Builder::new(372.0, 186.0).build(
        vec![ShapeInstance::rrect(rr(0.0, 0.0, 372.0, 186.0, 0.0), Paint::solid(hex(0x101420)))],
        None,
        Vec::new(),
    );
    let start = list.glyphs.len() as u32;
    fx.iter_mut().for_each(|(i, _)| *i += start);
    list.items.push(Item { kind: ItemKind::Glyphs, start, len: glyphs.len() as u32 });
    list.layers[0].items.end += 1;
    list.glyphs.extend(glyphs);
    list.glyph_fx = fx;
    list
}

#[test]
fn glyph_effect_goldens() {
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let mut off = offscreen(&gpu, &data);
    let list = glyph_fx_scene(&font);
    let mut failures = Vec::new();
    for (name, scale) in [("fx-glyphs-1x", 1.0), ("fx-glyphs-1.5x", 1.5)] {
        let img = render(&mut off, name, &list, scale);
        assert_premultiplied(&img);
        failures.extend(support::check(&img, Tolerance::default()).err());
    }
    assert!(failures.is_empty(), "{failures:#?}");
    // Deterministic across renderers.
    let again = render(&mut offscreen(&gpu, &data), "again", &list, 1.0);
    assert_eq!(render(&mut off, "once", &list, 1.0).px, again.px);
}
