//! Materials (FX15): a neon tube on a shape's stroke and on a vector wave, chrome type keyed
//! to em height, a hologram panel with text, a three-stop gradient across a word, and an app
//! material registered at run time. Goldens at 1x and 2x, a determinism check, and checks
//! that the neon blooms past its stroke and the hologram adds light. Phosphor: a white trace
//! sweeping on a trail layer whose channels fade at their own rates leaves a green afterglow.
//! Aurora and haze (FX-130, FX-149): a mesh of drifting blobs over a panel and a warm haze
//! inside one, with a check that energy brightens the mesh.
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
    DisplayList, GlyphFx, GlyphInstance, Item, ItemKind, Layer, LayerTarget, Paint, ShapeInstance, Trail, NO_WORD,
};
use vo_scene::geom::{Color, Point, RRect, Rect, Size};
use vo_scene::material::{Material, CUSTOM_FIRST};
use vo_scene::path::{make, push_stroke, StrokeStyle};

const FONT: &str = "/usr/share/fonts/noto/NotoSans-Bold.ttf";
const W: f32 = 600.0;
const H: f32 = 300.0;
const STRIPES: u32 = CUSTOM_FIRST;

/// An app material: diagonal stripes in two colours (slots 1 and 2), `p[0].x` apart.
const STRIPES_WGSL: &str = "
fn mat_stripes (m: Material, i: MatIn) -> vec4f {
  let s = step (0.5, fract ((i.p.x + i.p.y) / m.p[0].x));
  let c = mix (mat_lab (m.p[1]).rgb, mat_lab (m.p[2]).rgb, s);
  let cov = i.cov * i.color.a;
  return vec4f (c * cov, cov);
}
";

fn hex(rgb: u32) -> Color {
    Color::hex(rgb, 1.0)
}

fn rr(x: f32, y: f32, w: f32, h: f32, r: f32) -> RRect {
    RRect::new(Rect::new(x, y, w, h), r)
}

/// Glyphs of `text` from pen position `(x, y)`, and the pen's end.
fn run(font: &FontRef, text: &str, x: f32, y: f32, size: f32, color: Color) -> (Vec<GlyphInstance>, f32) {
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
    (out, pen)
}

struct Builder {
    list: DisplayList,
}

impl Builder {
    fn new() -> Builder {
        let mut list = DisplayList { size: Size::new(W, H), scale: 1.0, ..Default::default() };
        list.layers.push(Layer::root());
        Builder { list }
    }

    fn shape(&mut self, s: ShapeInstance) {
        let i = self.list.shapes.len() as u32;
        self.list.shapes.push(s);
        self.list.items.push(Item { kind: ItemKind::Shapes, start: i, len: 1 });
    }

    fn glyphs(&mut self, glyphs: Vec<GlyphInstance>, material: u32) {
        let start = self.list.glyphs.len() as u32;
        for (k, g) in glyphs.into_iter().enumerate() {
            self.list.glyph_fx.push((start + k as u32, GlyphFx { material, ..Default::default() }));
            self.list.glyphs.push(g);
        }
        let len = self.list.glyphs.len() as u32 - start;
        self.list.items.push(Item { kind: ItemKind::Glyphs, start, len });
    }

    fn finish(mut self) -> DisplayList {
        self.list.layers[0].items = 0..self.list.items.len() as u32;
        self.list
    }
}

/// The test card at `t` seconds after the materials appeared.
fn card(font: &FontRef, t: f32) -> DisplayList {
    let mut b = Builder::new();
    b.shape(ShapeInstance::rrect(rr(0.0, 0.0, W, H, 0.0), Paint::solid(hex(0x07080d))));

    let neon = b.list.add_material(Material::from(Material::neon(7.0).hue(0.08)).at(t));
    b.shape(
        ShapeInstance::rrect(rr(28.0, 28.0, 166.0, 66.0, 18.0), Paint::NONE)
            .with_stroke(Paint::solid(hex(0xff2fd0)), 4.0)
            .with_material(neon),
    );
    let tube = b.list.add_material(Material::from(Material::neon(6.0).core(0.4, 0.9)).at(t));
    let wave = make::wave(228.0, 572.0, 60.0, 18.0, 86.0, 0.0);
    let item = push_stroke(&mut b.list, &wave, &StrokeStyle::solid(3.5, hex(0x26e8ff)).material(tube)).unwrap();
    b.list.items.push(item);

    let chrome = b.list.add_material(Material::from(Material::chrome_classic()).at(t));
    let (glyphs, _) = run(font, "CHROME", 24.0, 194.0, 62.0, Color::WHITE);
    b.glyphs(glyphs, chrome);

    let holo = b.list.add_material(Material::from(Material::hologram(Color::hex(0x40e0ff, 0.6), 3.0)).at(t));
    b.shape(
        ShapeInstance::rrect(rr(332.0, 118.0, 244.0, 82.0, 10.0), Paint::solid(Color::hex(0x40e0ff, 0.35)))
            .with_material(holo),
    );
    b.shape(
        ShapeInstance::rrect(rr(332.0, 118.0, 244.0, 82.0, 10.0), Paint::NONE)
            .with_stroke(Paint::solid(Color::hex(0x9ff4ff, 0.9)), 1.5)
            .with_material(holo),
    );
    let (glyphs, _) = run(font, "HOLO 42", 352.0, 174.0, 38.0, Color::hex(0xc8fbff, 1.0));
    b.glyphs(glyphs, holo);

    let (glyphs, end) = run(font, "newest word", 24.0, 268.0, 36.0, Color::WHITE);
    let gradient = b.list.add_material(Material::gradient3(
        Point::new(24.0, 0.0),
        hex(0xffffff),
        0.45,
        hex(0x38e8ff),
        Point::new(end, 0.0),
        hex(0x9b5cff),
    ));
    b.glyphs(glyphs, gradient);

    let stripes = b.list.add_material(
        Material::custom(STRIPES).param(0, [8.0, 0.0, 0.0, 0.0]).color(1, hex(0xffd23f)).color(2, hex(0xff5a1f)),
    );
    b.shape(ShapeInstance::circle(Point::new(536.0, 252.0), 30.0, Paint::solid(Color::WHITE)).with_material(stripes));
    b.finish()
}

fn load_font() -> Vec<u8> {
    std::fs::read(FONT).unwrap_or_else(|e| panic!("{FONT}: {e} (install noto-fonts)"))
}

fn offscreen(gpu: &support::Gpu, font: &[u8]) -> Offscreen {
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    assert_eq!(off.renderer.add_font(font.to_vec()).unwrap(), 0);
    off.renderer.add_material(STRIPES, "mat_stripes", STRIPES_WGSL);
    off
}

fn render(off: &mut Offscreen, name: &str, list: &DisplayList, scale: f32) -> Image {
    let (w, h) = ((list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32);
    let px = off.render_to_rgba(list, [w, h], scale);
    Image { name: name.into(), w, h, channels: 4, px }
}

fn px(img: &Image, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.w + x) * 4) as usize;
    [img.px[i], img.px[i + 1], img.px[i + 2], img.px[i + 3]]
}

#[test]
fn material_goldens() {
    let gpu = support::gpu();
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let mut off = offscreen(&gpu, &data);
    let mut failures = Vec::new();
    for (name, scale) in [("material-1x", 1.0), ("material-2x", 2.0)] {
        let img = render(&mut off, name, &card(&font, 1.0), scale);
        if let Err(e) = support::check(&img, Tolerance::default()) {
            failures.push(e);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn materials_are_deterministic() {
    let gpu = support::gpu();
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let list = card(&font, 2.5);
    let a = render(&mut offscreen(&gpu, &data), "a", &list, 1.0);
    let b = render(&mut offscreen(&gpu, &data), "b", &list, 1.0);
    assert_eq!(a.px, b.px);
}

/// Neon light reaches past the stroke; without the material the same stroke has none there.
#[test]
fn neon_blooms_past_its_stroke() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let draw = |material: bool| {
        let mut b = Builder::new();
        let id = if material { b.list.add_material(Material::neon(6.0).into()) } else { 0 };
        b.shape(
            ShapeInstance::rrect(rr(100.0, 100.0, 200.0, 80.0, 12.0), Paint::NONE)
                .with_stroke(Paint::solid(hex(0xff2fd0)), 4.0)
                .with_material(id),
        );
        b.finish()
    };
    let lit = render(&mut off, "lit", &draw(true), 1.0);
    let plain = render(&mut off, "plain", &draw(false), 1.0);
    // 8 px outside the stroke's outer edge (x 98), and the tube's centre on the top edge.
    assert_eq!(px(&plain, 90, 140)[3], 0);
    assert!(px(&lit, 90, 140)[3] > 4, "{:?}", px(&lit, 90, 140));
    // The tube is brighter and whiter on its centre line (y 100) than at its edge.
    let (centre, edge) = (px(&lit, 200, 100), px(&lit, 200, 98));
    assert!(centre[1] as i32 > edge[1] as i32 + 40, "centre {centre:?}, edge {edge:?}");
}

/// Most of a hologram's light is added: its colour is above its alpha.
#[test]
fn a_hologram_adds_light() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let mut b = Builder::new();
    let id = b.list.add_material(Material::from(Material::hologram(Color::hex(0x40e0ff, 1.0), 6.0).flicker(0.0, 0.0)));
    b.shape(ShapeInstance::rrect(rr(50.0, 50.0, 200.0, 100.0, 0.0), Paint::solid(Color::WHITE)).with_material(id));
    let img = render(&mut off, "holo", &b.finish(), 1.0);
    let brightest = (50..150).map(|y| px(&img, 150, y)).max_by_key(|p| p[1]).unwrap();
    let a = brightest[3] as f32 / 255.0;
    assert!(srgb_to_linear(brightest[1] as f32 / 255.0) > 1.5 * a, "{brightest:?}");
}

/// The app material draws its stripes: two colours across the circle.
#[test]
fn an_app_material_draws() {
    let gpu = support::gpu();
    let data = load_font();
    let font = FontRef::new(&data).unwrap();
    let img = render(&mut offscreen(&gpu, &data), "card", &card(&font, 1.0), 1.0);
    let colours: std::collections::BTreeSet<[u8; 4]> =
        (0..16).map(|i| px(&img, 522 + i, 252)).filter(|p| p[3] == 255).collect();
    assert!(colours.len() >= 2, "{colours:?}");
    assert!(colours.iter().all(|p| p[0] > 200 && p[2] < 120), "yellow and orange only: {colours:?}");
}

/// Frame `i` of a 60 fps oscilloscope: a white trace whose wave slides, and the beam's dot
/// sweeping right, on a phosphor layer with a green afterglow (half-life multipliers as
/// `Element::phosphor` makes them for 0x33ff66).
fn scope_frame(i: u32) -> DisplayList {
    let t = i as f32 / 60.0;
    let mut list = DisplayList { size: Size::new(W, 120.0), scale: 1.0, ..Default::default() };
    list.layers.push(Layer::root());
    list.shapes.push(ShapeInstance::rrect(rr(0.0, 0.0, W, 120.0, 0.0), Paint::solid(hex(0x050806))));
    list.items.push(Item { kind: ItemKind::Shapes, start: 0, len: 1 });
    list.items.push(Item { kind: ItemKind::Layer, start: 1, len: 1 });
    list.layers[0].items = 0..2;
    list.layers.push(Layer {
        parent: Some(0),
        target: LayerTarget::Offscreen { bloom: None },
        trail: Some(Trail { key: 9, half_life: 0.12, t: t as f64, decay: [0.24, 1.0, 0.37], drift: None }),
        ..Layer::root()
    });
    let start = list.items.len() as u32;
    let wave = make::wave(20.0, W - 20.0, 60.0, 30.0, 140.0, -9.0 * t);
    let item = push_stroke(&mut list, &wave, &StrokeStyle::solid(2.0, Color::WHITE)).unwrap();
    list.items.push(item);
    let x = 20.0 + (W - 40.0) * (t * 1.8).fract();
    let dot = list.shapes.len() as u32;
    list.shapes.push(ShapeInstance::circle(Point::new(x, 100.0), 4.0, Paint::solid(Color::WHITE)));
    list.items.push(Item { kind: ItemKind::Shapes, start: dot, len: 1 });
    list.layers[1].items = start..list.items.len() as u32;
    list
}

#[test]
fn phosphor_golden() {
    let gpu = support::gpu();
    let run = |name: &str| {
        let mut off = Offscreen::new(&gpu.device, &gpu.queue);
        (0..=24).map(|i| render(&mut off, name, &scope_frame(i), 1.0)).last().unwrap()
    };
    let img = run("material-phosphor-1x");
    support::check(&img, Tolerance::default()).unwrap();
    // The beam's dot is at x 423 in the last frame, 16.8 px a frame: two frames back (x 390)
    // it glows green, green well above red and blue.
    let (r, g, b, _) = {
        let p = px(&img, 390, 100);
        (p[0] as i32, p[1] as i32, p[2] as i32, p[3])
    };
    assert!(g > r + 30 && g > b + 30, "afterglow is green: {:?}", px(&img, 390, 100));
    assert_eq!(img.px, run("material-phosphor-again").px);
}

/// An aurora mesh over the left panel at `energy`, and a warm haze inside the right one.
fn aurora_card(t: f32, energy: f32) -> DisplayList {
    let mut b = Builder::new();
    b.shape(ShapeInstance::rrect(rr(0.0, 0.0, W, H, 0.0), Paint::solid(hex(0x07080d))));
    let mesh =
        Material::from(Material::aurora(&[hex(0x18c8b0), hex(0x6a2cff), hex(0xff3d9a), hex(0x2a7bff)]).energy(energy));
    let mesh = b.list.add_material(mesh.at(t).in_box(Rect::new(20.0, 20.0, 340.0, 260.0)));
    b.shape(ShapeInstance::rrect(rr(20.0, 20.0, 340.0, 260.0, 22.0), Paint::solid(Color::WHITE)).with_material(mesh));
    b.shape(ShapeInstance::rrect(rr(380.0, 20.0, 200.0, 260.0, 18.0), Paint::solid(hex(0x1a1410))));
    let haze = Material::from(Material::haze(Color::hex(0xffa860, 0.6)));
    let haze = b.list.add_material(haze.at(t).in_box(Rect::new(380.0, 20.0, 200.0, 260.0)));
    b.shape(ShapeInstance::rrect(rr(380.0, 20.0, 200.0, 260.0, 18.0), Paint::solid(Color::WHITE)).with_material(haze));
    b.finish()
}

#[test]
fn aurora_golden() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let img = render(&mut off, "material-aurora-1x", &aurora_card(2.0, 0.3), 1.0);
    support::check(&img, Tolerance::default()).unwrap();
    // The haze adds light inside its panel but leaves room dark between its glows.
    let haze: Vec<u32> = (40..260).step_by(20).map(|y| px(&img, 480, y)[0] as u32).collect();
    assert!(haze.iter().max().unwrap() > &(haze.iter().min().unwrap() + 20), "haze varies: {haze:?}");
    // It moves: a second later the mesh is somewhere else.
    let later = render(&mut off, "later", &aurora_card(3.0, 0.3), 1.0);
    assert_ne!(img.px, later.px);
}

#[test]
fn energy_brightens_the_aurora() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let sum = |img: &Image| -> u64 {
        let mut s = 0u64;
        for y in (30..270).step_by(4) {
            for x in (30..350).step_by(4) {
                let p = px(img, x, y);
                s += p[0] as u64 + p[1] as u64 + p[2] as u64;
            }
        }
        s
    };
    let calm = sum(&render(&mut off, "calm", &aurora_card(2.0, 0.0), 1.0));
    let loud = sum(&render(&mut off, "loud", &aurora_card(2.0, 1.0), 1.0));
    assert!(loud * 100 > calm * 115, "energy 1 is brighter: {loud} vs {calm}");
}

/// A still frame on a phosphor trail layer: a translucent disc and a white ring, the same
/// every frame, at `t`.
fn still_frame(t: f64) -> DisplayList {
    let mut list = DisplayList { size: Size::new(200.0, 120.0), scale: 1.0, ..Default::default() };
    list.layers.push(Layer::root());
    list.shapes.push(ShapeInstance::rrect(rr(0.0, 0.0, 200.0, 120.0, 0.0), Paint::solid(hex(0x050806))));
    list.items.push(Item { kind: ItemKind::Shapes, start: 0, len: 1 });
    list.items.push(Item { kind: ItemKind::Layer, start: 1, len: 1 });
    list.layers[0].items = 0..2;
    list.layers.push(Layer {
        parent: Some(0),
        target: LayerTarget::Offscreen { bloom: None },
        trail: Some(Trail { key: 11, half_life: 0.25, t, decay: [0.24, 1.0, 0.37], drift: None }),
        ..Layer::root()
    });
    let start = list.items.len() as u32;
    let first = list.shapes.len() as u32;
    list.shapes.push(ShapeInstance::circle(Point::new(60.0, 60.0), 30.0, Paint::solid(Color::hex(0x66ff99, 0.4))));
    list.shapes.push(
        ShapeInstance::circle(Point::new(140.0, 60.0), 30.0, Paint::NONE).with_stroke(Paint::solid(Color::WHITE), 1.3),
    );
    list.items.push(Item { kind: ItemKind::Shapes, start: first, len: 2 });
    list.layers[1].items = start..list.items.len() as u32;
    list
}

/// Content that holds still on a trail layer keeps its brightness: the trail never adds a
/// frame onto the same frame again.
#[test]
fn a_still_frame_stays_as_bright_on_a_trail() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let frames: Vec<Image> = (0..=45).map(|i| render(&mut off, "still", &still_frame(i as f64 / 30.0), 1.0)).collect();
    let (first, last) = (&frames[0], &frames[45]);
    // The translucent disc's middle, and a pixel the ring's edge covers only in part.
    for (x, y) in [(60, 60), (60, 75), (170, 60), (140, 31)] {
        let (a, b) = (px(first, x, y), px(last, x, y));
        for c in 0..4 {
            assert!((a[c] as i32 - b[c] as i32).abs() <= 1, "({x}, {y}) went from {a:?} to {b:?}");
        }
    }
}
