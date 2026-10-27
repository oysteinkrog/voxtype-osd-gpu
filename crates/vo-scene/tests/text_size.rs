//! Text size morph (FX-048): with `.motion(Prop::TextSize, spring)` a new font size grows or
//! shrinks from the old one, over the new size's line breaks; without it the size jumps.

use vo_core::spring::Spring;
use vo_core::words::Word;
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::text::ShapedLayout;
use vo_scene::{Prop, Scene};

fn scene() -> Scene<()> {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    Scene::new().with_text(Box::new(ShapedLayout::new(fonts)))
}

const TEXT: &str = "pasted text that wraps";

fn label(size: f32, morph: bool) -> Element<()> {
    let mut l = Element::label("l", TEXT).text_style(TextStyle::new(0, size, size * 1.3)).width(200.0);
    if morph {
        l = l.motion(Prop::TextSize, Spring::SOFT);
    }
    Element::column("root").child(l)
}

fn live(size: f32) -> Element<()> {
    let w: Vec<Word> = TEXT.split(' ').enumerate().map(|(i, s)| Word { id: i as u32 + 1, text: s.into() }).collect();
    let l = Element::live_text("l", &w)
        .text_style(TextStyle::new(0, size, size * 1.3))
        .width(200.0)
        .motion(Prop::TextSize, Spring::SOFT);
    Element::column("root").child(l)
}

fn sizes(list: &DisplayList) -> Vec<f32> {
    list.glyphs.iter().map(|g| g.size).collect()
}

#[test]
fn a_label_grows_from_its_old_size_over_the_new_wrap() {
    let mut s = scene();
    s.apply(label(40.0, true), 0.0);
    s.apply(label(28.0, true), 1.0);
    // At the change it is still drawn at 40, but already on the 28 layout's lines.
    let at = s.sample(1.0);
    assert!(sizes(&at).iter().all(|&z| (z - 40.0).abs() < 1e-3), "{:?}", sizes(&at));
    let mid = s.sample(1.08);
    assert!(sizes(&mid).iter().all(|&z| z > 28.1 && z < 39.9), "{:?}", sizes(&mid));
    let settled = s.sample(4.0);
    assert!(s.next_wake(4.0).is_none());
    assert!(sizes(&settled).iter().all(|&z| (z - 28.0).abs() < 1e-3));
    // The glyphs keep the 28 layout's order and lines throughout: positions at the change
    // are the settled ones times 40 / 28 from the text box's corner.
    let origin = settled.glyphs[0].pos;
    let corner = |list: &DisplayList| list.glyphs[0].pos;
    for (a, b) in at.glyphs.iter().zip(&settled.glyphs) {
        let k = 40.0 / 28.0;
        let c0 = corner(&at);
        assert!((a.pos.x - c0.x - (b.pos.x - origin.x) * k).abs() < 1e-2);
        assert!((a.pos.y - c0.y - (b.pos.y - origin.y) * k).abs() < 1e-2);
    }
}

#[test]
fn without_the_motion_a_new_size_jumps() {
    let mut s = scene();
    s.apply(label(40.0, false), 0.0);
    s.apply(label(28.0, false), 1.0);
    assert!(sizes(&s.sample(1.0)).iter().all(|&z| (z - 28.0).abs() < 1e-3));
}

#[test]
fn live_text_morphs_too() {
    let mut s = scene();
    s.apply(live(40.0), 0.0);
    s.sample(2.0);
    s.apply(live(28.0), 2.0);
    let at = s.sample(2.0);
    assert!(sizes(&at).iter().all(|&z| (z - 40.0).abs() < 0.05), "{:?}", sizes(&at));
    let settled = s.sample(6.0);
    assert!(sizes(&settled).iter().all(|&z| (z - 28.0).abs() < 1e-3));
}
