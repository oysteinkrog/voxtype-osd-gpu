//! End to end: a view, a `Scene`, `sample(t)`, then `Offscreen`. The toast from
//! `vo-scene`'s example, with real text from the vendored Noto Sans subset, mid enter and
//! settled.
//!
//! GPU test: `just test-gpu` runs it. The scene and the renderer share one `FontRegistry`,
//! so the glyph ids the text layout places are the ones the renderer draws.

mod support;

use support::{Image, Tolerance};
use vo_core::spring::Spring;
use vo_render::Offscreen;
use vo_scene::display::ShapeKind;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::{Prop, Scene, Transition};

#[derive(Clone, Debug)]
enum Msg {
    Dismiss,
}

/// The same view as `crates/vo-scene/examples/toast.rs`, inside a padded screen so the
/// enter offset stays on the surface.
fn toast(title: &str, body: &str) -> Element<Msg> {
    let close = Element::shape("close", ShapeKind::Circle)
        .size(18.0, 18.0)
        .fill(Color::hex(0xffffff, 0.12))
        .hairline(Color::hex(0xffffff, 0.4))
        .on_click(Msg::Dismiss);
    let header = Element::row("header")
        .gap(8.0)
        .child(Element::shape("dot", ShapeKind::Circle).size(10.0, 10.0).fill(Color::hex(0x4cc38a, 1.0)))
        .child(Element::label("title", title).text_style(TextStyle::new(0, 15.0, 19.0)).grow(1.0))
        .child(close);
    let toast = Element::panel("toast")
        .width(320.0)
        .pad(14.0)
        .gap(6.0)
        .radius(14.0)
        .fill_gradient(Color::hex(0x24262b, 0.96), Color::hex(0x1b1c20, 0.96))
        .stroke(Color::hex(0xffffff, 0.08), 1.0)
        .motion(Prop::Size, Spring::SNAP)
        .presence(Transition::fade().offset(0.0, -12.0).scale(0.96), Transition::fade().scale(0.98))
        .interactive()
        .child(header)
        .child(Element::label("body", body).color(Color::hex(0xc8cad0, 1.0)))
        .child(Element::shape("rule", ShapeKind::Line).height(0.0).hairline(Color::hex(0xffffff, 0.1)));
    Element::column("screen").pad(16.0).child(toast)
}

const MID_ENTER: f64 = 0.06;
const SETTLED: f64 = 3.0;

#[test]
fn toast_goldens() {
    let fonts = FontRegistry::new();
    assert_eq!(fonts.add_file(&test_font_path()).unwrap(), 0);
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(352.0, 150.0), 1.0);
    scene.apply(toast("Saved", "The recording was transcribed and copied to the clipboard."), 0.0);
    assert!(scene.next_wake(MID_ENTER).is_some(), "still entering at {MID_ENTER}");
    assert!(scene.next_wake(SETTLED).is_none(), "settled by {SETTLED}");

    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let mut images = Vec::new();
    for (name, t, scale) in
        [("toast-mid-enter", MID_ENTER, 1.0), ("toast-settled", SETTLED, 1.0), ("toast-settled-1.5x", SETTLED, 1.5)]
    {
        let list = scene.sample(t);
        // The body wraps to two lines of real glyphs, so the toast has text to show.
        assert!(list.glyphs.len() > 40, "{name}: {} glyphs", list.glyphs.len());
        assert!(list.glyphs.iter().all(|g| g.font == 0 && g.glyph != 0));
        let (w, h) = ((list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32);
        let px = off.render_to_rgba(&list, [w, h], scale);
        images.push(Image { name: name.into(), w, h, channels: 4, px });
    }
    // Mid enter is fainter than settled: the presence fade is part way.
    let ink = |img: &Image| img.px.as_chunks::<4>().0.iter().map(|p| p[3] as u64).sum::<u64>();
    assert!(ink(&images[0]) < ink(&images[1]), "mid enter should be fainter than settled");

    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}
