//! Type effects (WP18): an offset text shadow (FX-013) and outlined text (FX-018), both drawn
//! in the glyph shader.
//!
//! `type-shadow`: serif text on a light card with a soft shadow 2 units down, as concept 13,
//! and a hard shadow up and to the left. `type-outline`: hollow condensed text at two stroke
//! widths, at two scales, and outline with a glow.
//!
//! GPU test: `just test-gpu type_fx`.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::FontRegistry;
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::{DisplayList, Scene};

fn render(fonts: &FontRegistry, view: Element<()>, size: Size, scale: f32) -> (Image, DisplayList) {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, size, scale);
    scene.apply(view, 0.0);
    let list = scene.sample(10.0);
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let (w, h) = ((size.w * scale) as u32, (size.h * scale) as u32);
    let px = off.render_to_rgba(&list, [w, h], scale);
    (Image { name: String::new(), w, h, channels: 4, px }, list)
}

fn ink(list: &DisplayList) -> usize {
    list.glyphs.len()
}

#[test]
fn offset_text_shadow() {
    let fonts = FontRegistry::new();
    let serif = fonts.asset("serif").unwrap();
    let style = TextStyle::new(serif, 28.0, 36.0);
    let size = Size::new(420.0, 130.0);
    let view = Element::panel("card")
        .size(size.w, size.h)
        .pad(16.0)
        .gap(8.0)
        .fill(Color::hex(0xece6da, 1.0))
        .child(
            Element::label("soft", "Soft shadow, 0 2 6")
                .text_style(style)
                .color(Color::hex(0x1d1a16, 1.0))
                .text_shadow(Color::hex(0x000000, 0.45), 0.0, 2.0, 6.0),
        )
        .child(
            Element::label("hard", "Hard shadow, -2 -2 0")
                .text_style(style)
                .color(Color::hex(0xc0392b, 1.0))
                .text_shadow(Color::hex(0x1d1a16, 0.9), -2.0, -2.0, 0.0),
        );
    let (img, list) = render(&fonts, view, size, 1.0);
    assert!(ink(&list) > 20);
    // Every glyph carries the shadow offset.
    assert!(list.glyph_fx.iter().all(|(_, e)| e.halo_offset.x != 0.0 || e.halo_offset.y == 2.0));
    support::check(&Image { name: "type-shadow".into(), ..img }, Tolerance::default()).unwrap();
}

#[test]
fn outlined_text() {
    let fonts = FontRegistry::new();
    let bold = fonts.asset("condensed-bold").unwrap();
    let style = TextStyle::new(bold, 44.0, 50.0);
    let size = Size::new(420.0, 180.0);
    let orange = Color::hex(0xff8a1f, 1.0);
    let view = || {
        Element::panel("bg")
            .size(size.w, size.h)
            .pad(12.0)
            .gap(4.0)
            .fill(Color::hex(0x0b0806, 1.0))
            .child(Element::label("thin", "OUTLINE 1.0").text_style(style).color(orange).text_outline(1.0))
            .child(Element::label("thick", "OUTLINE 3.0").text_style(style).color(orange).text_outline(3.0))
            .child(
                Element::label("glow", "OUTLINE + GLOW")
                    .text_style(style)
                    .color(Color::hex(0x7fe6ff, 1.0))
                    .text_outline(1.5)
                    .text_halo(Color::hex(0x2fb8ff, 0.6), 6.0),
            )
    };
    for (name, scale) in [("type-outline", 1.0), ("type-outline-2x", 2.0)] {
        let (img, list) = render(&fonts, view(), size, scale);
        assert!(list.glyph_fx.iter().all(|(_, e)| e.outline > 0.0));
        support::check(&Image { name: name.into(), ..img }, Tolerance::default()).unwrap();
    }
}
