//! Gradient caret (FX-037): `Caret::gradient` fades the bar from its colour at the top to a
//! second colour at the bottom.

use vo_core::words::Word;
use vo_scene::display::PaintKind;
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::Caret;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::Color;
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

fn scene() -> Scene<()> {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    Scene::new().with_text(Box::new(ShapedLayout::new(fonts)))
}

fn view(caret: Caret) -> Element<()> {
    let w = [Word { id: 1, text: "hello".into() }];
    Element::column("root").child(Element::live_text("live", &w).text_style(TextStyle::new(0, 20.0, 26.0)).caret(caret))
}

#[test]
fn the_caret_fades_from_its_colour_to_the_gradient_colour() {
    let (top, bottom) = (Color::hex(0x6cb6ff, 1.0), Color::hex(0xff6cb6, 1.0));
    let mut s = scene();
    s.apply(view(Caret { color: Some(top), gradient: Some(bottom), ..Caret::default() }), 0.0);
    let list = s.sample(0.0);
    let bar = list.shapes.last().unwrap();
    assert_eq!(bar.fill.kind, PaintKind::Linear);
    assert_eq!((bar.fill.c0, bar.fill.c1), (top, bottom));
    // Top to bottom of the bar: same x, from its top edge to its bottom edge.
    let [x, y, w, h] = bar.geom;
    assert!(w > 0.0 && h > 0.0);
    assert_eq!(bar.fill.p0.x, bar.fill.p1.x);
    assert!(
        (bar.fill.p0.y - y).abs() < 1e-3 && (bar.fill.p1.y - (y + h)).abs() < 1e-3,
        "{:?} {:?}",
        bar.fill,
        [x, y, w, h]
    );
}

#[test]
fn a_caret_without_a_gradient_stays_solid() {
    let mut s = scene();
    s.apply(view(Caret::default()), 0.0);
    let list = s.sample(0.0);
    assert_eq!(list.shapes.last().unwrap().fill.kind, PaintKind::Solid);
}
