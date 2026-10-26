//! Golden images of vector paths (`vo_scene::path`): filled icons through the Slug glyph
//! path, strokes through the vector pipeline, trim, a light head on a panel border, contour
//! morph and blob merge.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::{DisplayList, Item, ItemKind, Layer, Paint, ShapeInstance};
use vo_scene::font::FontRegistry;
use vo_scene::geom::{Affine, Color, Point, RRect, Rect, Size};
use vo_scene::path::kurbo::{self, Shape};
use vo_scene::path::{fill_glyph, icons, push_blob, push_stroke, Contour, Morph, Outline, StrokeStyle};

const ACCENT: u32 = 0x4ade80;

fn white() -> Color {
    Color::WHITE
}

/// A one-layer list that items are pushed into in paint order.
struct Builder {
    list: DisplayList,
    items: Vec<Item>,
}

impl Builder {
    fn new(w: f32, h: f32) -> Builder {
        Builder { list: DisplayList { size: Size::new(w, h), scale: 1.0, ..Default::default() }, items: Vec::new() }
    }

    fn shape(&mut self, s: ShapeInstance) {
        self.items.push(Item { kind: ItemKind::Shapes, start: self.list.shapes.len() as u32, len: 1 });
        self.list.shapes.push(s);
    }

    fn item(&mut self, item: Option<Item>) {
        self.items.extend(item);
    }

    fn stroke(&mut self, c: &Contour, style: &StrokeStyle) {
        let item = push_stroke(&mut self.list, c, style);
        self.item(item);
    }

    fn fill(&mut self, path: u32, pos: Point, scale: f32, color: Color) {
        self.items.push(Item { kind: ItemKind::Glyphs, start: self.list.glyphs.len() as u32, len: 1 });
        self.list.glyphs.push(fill_glyph(path, pos, scale, color));
    }

    fn finish(mut self) -> DisplayList {
        let mut root = Layer::root();
        root.items = 0..self.items.len() as u32;
        self.list.layers.push(root);
        self.list.items = self.items;
        self.list
    }
}

/// An icon flattened and placed: `scale` units per icon unit, top left at `at`.
fn placed(path: &kurbo::BezPath, at: Point, scale: f32) -> Outline {
    Outline::new(path, 0.01).transform(&Affine::scale(scale).then(&Affine::translate(at.x, at.y)))
}

fn icons_scene(fonts: &FontRegistry) -> DisplayList {
    let mut b = Builder::new(260.0, 180.0);
    b.shape(ShapeInstance::rrect(
        RRect::new(Rect::new(0.0, 0.0, 260.0, 180.0), 0.0),
        Paint::solid(Color::hex(0x111318, 1.0)),
    ));
    let set = [icons::mic(), icons::check(), icons::stop()];
    for (i, icon) in set.iter().enumerate() {
        let x = 20.0 + 80.0 * i as f32;
        // Filled through the glyph path: the stroke outline from kurbo.
        let id = fonts.add_path(icons::outline(icon, 2.0));
        b.fill(id, Point::new(x, 10.0), 3.0, white());
        // Stroked through the vector pipeline.
        for c in placed(icon, Point::new(x, 95.0), 3.0).contours {
            b.stroke(&c, &StrokeStyle::solid(6.0, Color::hex(ACCENT, 1.0)));
        }
    }
    b.finish()
}

fn trim_scene() -> DisplayList {
    let mut b = Builder::new(320.0, 200.0);
    b.shape(ShapeInstance::rrect(
        RRect::new(Rect::new(0.0, 0.0, 320.0, 200.0), 0.0),
        Paint::solid(Color::hex(0x111318, 1.0)),
    ));
    for (i, f) in [0.2f32, 0.55, 1.0].into_iter().enumerate() {
        let o = placed(&icons::check(), Point::new(10.0 + 100.0 * i as f32, 0.0), 3.5);
        let len = o.length();
        for c in o.trim(0.0, f * len).contours {
            b.stroke(&c, &StrokeStyle::solid(7.0, Color::hex(ACCENT, 1.0)));
        }
    }
    // A panel with a hairline border and a light head running along it, tail fading out.
    let panel = RRect::new(Rect::new(30.0, 110.0, 260.0, 70.0), 18.0);
    b.shape(
        ShapeInstance::rrect(panel, Paint::solid(Color::hex(0x1c1f26, 1.0)))
            .with_hairline(Paint::solid(Color::hex(0x3a3f4b, 1.0))),
    );
    let border = Outline::new(&kurbo::RoundedRect::new(30.0, 110.0, 290.0, 180.0, 18.0).to_path(0.01), 0.01);
    let head = 0.8 * border.length();
    let tail = 140.0;
    let style = StrokeStyle::solid(2.0, white())
        .ramp(head - tail, Color::rgba(0.4, 0.8, 1.0, 0.0), head, Color::rgba(0.85, 0.95, 1.0, 1.0))
        .glow(6.0);
    b.stroke(&border.window(0, head, tail), &style);
    b.finish()
}

fn morph_scene() -> DisplayList {
    let mut b = Builder::new(420.0, 200.0);
    b.shape(ShapeInstance::rrect(
        RRect::new(Rect::new(0.0, 0.0, 420.0, 200.0), 0.0),
        Paint::solid(Color::hex(0x111318, 1.0)),
    ));
    let m = Morph::new(&Outline::new(&icons::mic(), 0.01), &Outline::new(&icons::check(), 0.01), 96);
    for (i, t) in [0.0f32, 0.25, 0.5, 0.75, 1.0].into_iter().enumerate() {
        let at = Affine::scale(3.0).then(&Affine::translate(10.0 + 82.0 * i as f32, 5.0));
        for part in m.sample(t) {
            let style = StrokeStyle::solid(6.0 * part.weight, Color::hex(ACCENT, part.weight.sqrt()));
            b.stroke(&part.contour.transform(&at), &style);
        }
    }
    // Two dots merging as they come closer.
    for (i, gap) in [70.0f32, 44.0, 30.0, 18.0].into_iter().enumerate() {
        let cx = 60.0 + 100.0 * i as f32;
        let a = (Point::new(cx - gap * 0.5, 150.0), 14.0);
        let c = (Point::new(cx + gap * 0.5, 150.0), 10.0);
        let item = push_blob(&mut b.list, a, c, 14.0, Color::hex(0x60a5fa, 1.0), Color::hex(0xf472b6, 1.0));
        b.item(Some(item));
    }
    b.finish()
}

fn render(off: &mut Offscreen, name: &str, list: &DisplayList, scale: f32) -> Image {
    let [w, h] = [(list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32];
    let px = off.render_to_rgba(list, [w, h], scale);
    Image { name: name.into(), w, h, channels: 4, px }
}

#[test]
fn vector_goldens() {
    let gpu = support::gpu();
    let fonts = FontRegistry::new();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let icons = icons_scene(&fonts);
    let images = [
        render(&mut off, "vec-icons", &icons, 1.0),
        render(&mut off, "vec-icons-2x", &icons, 2.0),
        render(&mut off, "vec-trim", &trim_scene(), 1.0),
        render(&mut off, "vec-morph", &morph_scene(), 1.0),
    ];
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// A translucent stroke covers each pixel once, joins included: no pixel is brighter than
/// the stroke colour.
#[test]
fn translucent_joins_blend_once() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let mut b = Builder::new(120.0, 120.0);
    let o = placed(&icons::check(), Point::new(0.0, 0.0), 5.0);
    for c in &o.contours {
        b.stroke(c, &StrokeStyle::solid(12.0, Color::rgba(1.0, 1.0, 1.0, 0.5)));
    }
    let list = b.finish();
    let px = off.render_to_rgba(&list, [120, 120], 1.0);
    let max_alpha = px.as_chunks::<4>().0.iter().map(|p| p[3]).max().unwrap();
    assert!((126..=129).contains(&max_alpha), "max alpha {max_alpha}");
}

/// `Element::path` through a `Scene`: a path from an SVG string and an icon outline, each
/// fitted to its box, in a row with a background panel.
#[test]
fn element_path_golden() {
    use vo_scene::element::Element;
    use vo_scene::Scene;
    let fonts = FontRegistry::new();
    // A play triangle in a 22 unit viewBox, as an SVG icon gives it.
    let play = fonts.add_path(kurbo::BezPath::from_svg("M7 4 L18 11 L7 18 Z").unwrap());
    let mic = fonts.add_path(icons::outline(&icons::mic(), 2.0));
    let view = |row: Element<()>| {
        Element::panel("bg").pad(12.0).gap(12.0).direction_row().fill(Color::hex(0x111318, 1.0)).child(row)
    };
    let row = Element::row("row")
        .gap(12.0)
        .child(Element::path("play", play, Rect::new(0.0, 0.0, 22.0, 22.0)).size(44.0, 44.0))
        .child(
            Element::path("mic", mic, Rect::new(0.0, 0.0, 24.0, 24.0)).size(64.0, 44.0).fill(Color::hex(ACCENT, 1.0)),
        );
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(160.0, 68.0), 1.0);
    scene.apply(view(row), 0.0);
    let list = scene.sample(5.0);
    assert_eq!(list.glyphs.iter().filter(|g| g.font == vo_scene::path::PATH_FONT).count(), 2);
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let img = render(&mut off, "vec-element-path", &list, 2.0);
    support::check(&img, Tolerance::default()).unwrap();
}

fn dark(b: &mut Builder, w: f32, h: f32) {
    b.shape(ShapeInstance::rrect(RRect::new(Rect::new(0.0, 0.0, w, h), 0.0), Paint::solid(Color::hex(0x111318, 1.0))));
}

/// Hairlines, dashes, a ribbon, linear and conic gradients, a sparkline, corner brackets and
/// straight and wavy underlines.
fn strokes_scene() -> DisplayList {
    use vo_scene::path::make;
    let mut b = Builder::new(360.0, 220.0);
    dark(&mut b, 360.0, 220.0);
    let ink = Color::hex(0xe8eaf0, 1.0);
    b.stroke(&make::panel(10.0, 10.0, 160.0, 60.0, 12.0), &StrokeStyle::hairline(Color::hex(0x6b7280, 1.0)));
    for d in make::panel(20.0, 20.0, 140.0, 40.0, 8.0).dash(&[8.0, 6.0], 3.0) {
        b.stroke(&d, &StrokeStyle::solid(2.0, Color::hex(ACCENT, 1.0)));
    }
    let pts: Vec<Point> =
        (0..=40).map(|i| Point::new(190.0 + 4.0 * i as f32, 40.0 + 14.0 * (i as f32 * 0.35).sin())).collect();
    let widths: Vec<f32> = (0..=40).map(|i| 0.2 + (std::f32::consts::PI * i as f32 / 40.0).sin()).collect();
    b.stroke(
        &make::ribbon(&pts, &widths),
        &StrokeStyle::solid(10.0, white()).linear(
            Point::new(190.0, 0.0),
            Color::hex(0x60a5fa, 1.0),
            Point::new(350.0, 0.0),
            Color::hex(0xf472b6, 1.0),
        ),
    );
    let ring = make::ring(Point::new(50.0, 130.0), 32.0);
    b.stroke(
        &ring,
        &StrokeStyle::solid(8.0, white()).conic(
            Point::new(50.0, 130.0),
            0.0,
            Color::hex(0x22d3ee, 1.0),
            Color::hex(0xa855f7, 1.0),
        ),
    );
    let values = [0.2, 0.5, 0.35, 0.8, 0.6, 0.9, 0.4, 0.55, 0.3, 0.7, 0.65, 1.0];
    b.stroke(&make::sparkline(&values, 100.0, 100.0, 110.0, 60.0), &StrokeStyle::solid(2.0, Color::hex(ACCENT, 1.0)));
    for c in make::brackets(230.0, 95.0, 110.0, 70.0, 14.0) {
        b.stroke(&c, &StrokeStyle::solid(2.5, ink));
    }
    b.stroke(&make::underline(20.0, 150.0, 195.0), &StrokeStyle::solid(2.0, ink));
    b.stroke(&make::wave(190.0, 340.0, 195.0, 3.0, 12.0, 0.0), &StrokeStyle::solid(2.0, Color::hex(0xf87171, 1.0)));
    b.finish()
}

/// Spinners at three times, progress arcs, and the newer icons.
fn components_scene() -> DisplayList {
    use vo_scene::path::make;
    let mut b = Builder::new(380.0, 170.0);
    dark(&mut b, 380.0, 170.0);
    let track = Color::hex(0x2a2f3a, 1.0);
    for (i, t) in [0.1, 0.5, 0.95].into_iter().enumerate() {
        let c = Point::new(40.0 + 70.0 * i as f32, 45.0);
        b.stroke(&make::ring(c, 22.0), &StrokeStyle::solid(5.0, track));
        b.stroke(&make::spinner(c, 22.0, t), &StrokeStyle::solid(5.0, Color::hex(0x60a5fa, 1.0)));
    }
    for (i, f) in [0.25f32, 0.6, 1.0].into_iter().enumerate() {
        let c = Point::new(250.0 + 45.0 * i as f32, 45.0);
        b.stroke(&make::ring(c, 16.0), &StrokeStyle::solid(4.0, track));
        b.stroke(&make::progress(c, 16.0, f), &StrokeStyle::solid(4.0, Color::hex(ACCENT, 1.0)));
    }
    for (i, icon) in [icons::x(), icons::timer(), icons::chip(), icons::gear()].iter().enumerate() {
        for c in placed(icon, Point::new(20.0 + 90.0 * i as f32, 90.0), 2.6).contours {
            b.stroke(&c, &StrokeStyle::solid(4.0, white()));
        }
    }
    b.finish()
}

/// Fills that change every frame: a glyph morph "a" to "g" with holes, and a filled icon morph.
fn fills_scene(fonts: &FontRegistry) -> DisplayList {
    use vo_scene::path::push_fill;
    let face = fonts.get(0).expect("font 0");
    let gid = |c: char| {
        use skrifa::MetadataProvider;
        face.skrifa().charmap().map(c).expect("glyph").to_u32()
    };
    let a = Outline::new(&vo_scene::path::glyph_path(&face, gid('a'), 90.0), 0.02);
    let g = Outline::new(&vo_scene::path::glyph_path(&face, gid('g'), 90.0), 0.02);
    let m = Morph::new(&a, &g, 160);
    let mut b = Builder::new(420.0, 120.0);
    dark(&mut b, 420.0, 120.0);
    for (i, t) in [0.0f32, 0.33, 0.66, 1.0].into_iter().enumerate() {
        let at = Affine::translate(15.0 + 70.0 * i as f32, 80.0);
        let parts: Vec<_> =
            m.sample(t).into_iter().filter(|p| p.weight > 0.01).map(|p| p.contour.transform(&at)).collect();
        let item = push_fill(&mut b.list, &parts, &StrokeStyle::solid(0.0, Color::hex(0xe8eaf0, 1.0)));
        b.item(item);
    }
    let stop = Outline::new(&icons::outline(&icons::stop(), 2.0), 0.01);
    let at = Affine::scale(3.5).then(&Affine::translate(320.0, 18.0));
    let item = push_fill(
        &mut b.list,
        &stop.transform(&at).contours,
        &StrokeStyle::solid(0.0, white()).linear(
            Point::new(320.0, 18.0),
            Color::hex(0xf87171, 1.0),
            Point::new(404.0, 102.0),
            Color::hex(0xfbbf24, 1.0),
        ),
    );
    b.item(item);
    b.finish()
}

#[test]
fn vector_feature_goldens() {
    let gpu = support::gpu();
    let fonts = FontRegistry::new();
    fonts.add_file(&vo_scene::font::test_font_path()).unwrap();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let images = [
        render(&mut off, "vec-strokes", &strokes_scene(), 1.0),
        render(&mut off, "vec-strokes-2x", &strokes_scene(), 2.0),
        render(&mut off, "vec-components", &components_scene(), 1.0),
        render(&mut off, "vec-fills", &fills_scene(&fonts), 1.0),
    ];
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// A hairline is one physical pixel wide at 1x and at 2x.
#[test]
fn vector_hairline_is_one_pixel() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let mut b = Builder::new(40.0, 40.0);
    b.stroke(
        &Contour::polyline(&[Point::new(2.0, 20.5), Point::new(38.0, 20.5)], false),
        &StrokeStyle::hairline(white()),
    );
    let list = b.finish();
    for scale in [1.0f32, 2.0] {
        let n = (40.0 * scale) as u32;
        let px = off.render_to_rgba(&list, [n, n], scale);
        let col = n as usize / 2;
        let lit: u32 = (0..n as usize).map(|y| px[(y * n as usize + col) * 4 + 3] as u32).sum();
        assert!((240..=270).contains(&lit), "scale {scale}: column alpha sum {lit}");
    }
}

/// `Element::stroke_path` drawing on through its enter transition, with a light head and a
/// fill, and `Element::morph` part way from mic to check, through a `Scene`.
#[test]
fn element_stroke_and_morph_goldens() {
    use std::sync::Arc;
    use vo_core::spring::Spring;
    use vo_scene::element::Element;
    use vo_scene::path::make;
    use vo_scene::{Prop, Scene, Transition};
    let border = Arc::new(Outline { contours: vec![make::panel(0.0, 0.0, 200.0, 60.0, 16.0)] });
    let mic = Outline::new(&icons::mic(), 0.01);
    let morph = Arc::new(Morph::new(&mic, &Outline::new(&icons::check(), 0.01), 96));
    let view = |m: f32| {
        Element::<()>::panel("bg")
            .pad(10.0)
            .gap(10.0)
            .direction_row()
            .fill(Color::hex(0x111318, 1.0))
            .child(
                Element::stroke_path(
                    "border",
                    border.clone(),
                    Rect::new(-2.0, -2.0, 204.0, 64.0),
                    StrokeStyle::solid(2.0, Color::hex(0x6b7280, 1.0)),
                )
                .size(204.0, 64.0)
                .path_fill(Color::hex(0x1c1f26, 1.0))
                .light_head(0.3, 0.5, Color::hex(0x7dd3fc, 1.0), 5.0)
                .motion(Prop::Trim, Spring::critical_to_98(0.8))
                .presence(Transition::NONE.trim(0.0, 0.0), Transition::NONE.trim(1.0, 1.0)),
            )
            .child(
                Element::morph(
                    "icon",
                    morph.clone(),
                    Rect::new(0.0, 0.0, 24.0, 24.0),
                    StrokeStyle::solid(4.0, Color::hex(ACCENT, 1.0)),
                )
                .size(64.0, 64.0)
                .motion(Prop::Morph, Spring::SOFT)
                .morph_to(m),
            )
    };
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(310.0, 84.0), 1.0);
    scene.apply(view(0.0), 0.0);
    let mid_enter = scene.sample(0.25);
    scene.apply(view(1.0), 2.0);
    let mid_morph = scene.sample(2.1);
    // The light head keeps the scene awake; without it everything settles.
    assert!(scene.next_wake(10.0).is_some());
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let images = [
        render(&mut off, "vec-el-enter", &mid_enter, 1.0),
        render(&mut off, "vec-el-morph", &mid_morph, 1.0),
        render(&mut off, "vec-el-settled", &scene.sample(4.3), 1.0),
    ];
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}
