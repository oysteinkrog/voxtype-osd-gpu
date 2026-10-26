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
    let max_alpha = px.chunks_exact(4).map(|p| p[3]).max().unwrap();
    assert!((126..=129).contains(&max_alpha), "max alpha {max_alpha}");
}
