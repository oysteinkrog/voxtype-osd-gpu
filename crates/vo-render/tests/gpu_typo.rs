//! Glyph weight (MO-03): the glyph shader grows or thins a glyph's edges from the signed
//! distance to its outline. Heavier glyphs carry more ink, thinner ones less, by about the
//! outline length times the weight, and the advance never changes.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use skrifa::instance::{LocationRef, Size as FontSize};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, MetadataProvider};
use vo_render::Offscreen;
use vo_scene::display::{DisplayList, GlyphFx, GlyphInstance, Item, ItemKind, Layer, NO_WORD};
use vo_scene::geom::{Color, Point, Size};

const FONT: &str = "/usr/share/fonts/noto/NotoSans-Regular.ttf";

fn word(font: &FontRef, text: &str, x: f32, y: f32, size: f32) -> Vec<GlyphInstance> {
    let upem = font.head().unwrap().units_per_em() as f32;
    let cmap = font.charmap();
    let adv = font.glyph_metrics(FontSize::unscaled(), LocationRef::default());
    let mut pen = x;
    let mut out = Vec::new();
    for ch in text.chars() {
        let gid = cmap.map(ch).unwrap_or_default();
        let color = Color::WHITE;
        out.push(GlyphInstance {
            pos: Point::new(pen, y),
            size,
            font: 0,
            glyph: gid.to_u32(),
            word: NO_WORD,
            color,
            xform: vo_scene::display::GlyphXform::IDENTITY,
        });
        pen += adv.advance_width(gid).unwrap_or(0.0) * size / upem;
    }
    out
}

/// "ink" in white on transparent, with `weight` on every glyph.
fn scene(font: &FontRef, weight: f32) -> DisplayList {
    let mut list = DisplayList { size: Size::new(120.0, 50.0), scale: 1.0, ..Default::default() };
    list.layers.push(Layer::root());
    list.glyphs = word(font, "ink", 10.0, 36.0, 28.0);
    list.items.push(Item { kind: ItemKind::Glyphs, start: 0, len: list.glyphs.len() as u32 });
    list.layers[0].items = 0..1;
    if weight != 0.0 {
        list.glyph_fx = (0..list.glyphs.len() as u32).map(|i| (i, GlyphFx { weight, ..Default::default() })).collect();
    }
    list
}

/// Total alpha in the image, in pixels of full coverage.
fn ink(off: &mut Offscreen, list: &DisplayList) -> f32 {
    let px = off.render_to_rgba(list, [120, 50], 1.0);
    px.chunks(4).map(|p| p[3] as f32 / 255.0).sum()
}

#[test]
fn weight_adds_and_removes_ink() {
    let data = std::fs::read(FONT).unwrap_or_else(|e| panic!("{FONT}: {e} (install noto-fonts)"));
    let font = FontRef::new(&data).unwrap();
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    off.renderer.add_font(data.clone()).unwrap();
    let plain = ink(&mut off, &scene(&font, 0.0));
    let zero = {
        // A weight of zero through the effect path matches the plain path closely.
        let mut l = scene(&font, 0.0);
        l.glyph_fx = (0..l.glyphs.len() as u32).map(|i| (i, GlyphFx { weight: 1e-6, ..Default::default() })).collect();
        ink(&mut off, &l)
    };
    let heavy = ink(&mut off, &scene(&font, 0.6));
    let light = ink(&mut off, &scene(&font, -0.4));
    assert!((zero - plain).abs() < 0.03 * plain, "plain {plain} zero {zero}");
    // Outline length of "ink" at 28 px is well over 100 px, so 0.6 px per edge adds a lot.
    assert!(heavy > plain + 50.0, "plain {plain} heavy {heavy}");
    assert!(light < plain - 30.0, "plain {plain} light {light}");
    // Heavier again is monotone.
    let heavier = ink(&mut off, &scene(&font, 1.2));
    assert!(heavier > heavy + 50.0, "heavy {heavy} heavier {heavier}");
}
