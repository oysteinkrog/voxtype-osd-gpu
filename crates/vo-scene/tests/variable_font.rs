//! Variable font instances (MO-81): a variable font at a variation location gets a font id
//! of its own, shaping and metrics follow the location, and live text can set single words
//! in an instance.

use vo_core::words::Word;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::FontRegistry;
use vo_scene::text::{ShapedLayout, TextLayout};
use vo_scene::Scene;

fn flex() -> (FontRegistry, u32) {
    let fonts = FontRegistry::new();
    let id = fonts.asset("flex").unwrap();
    (fonts, id)
}

fn width(fonts: &FontRegistry, font: u32, text: &str) -> f32 {
    ShapedLayout::new(fonts.clone()).layout(text, &TextStyle::new(font, 40.0, 48.0), None).size.w
}

#[test]
fn the_bundled_flex_font_has_four_axes() {
    let (fonts, id) = flex();
    let axes = fonts.get(id).unwrap().axes();
    let tags: Vec<[u8; 4]> = axes.iter().map(|a| a.0).collect();
    for t in [*b"wght", *b"wdth", *b"opsz", *b"slnt"] {
        assert!(tags.contains(&t), "{:?}", String::from_utf8_lossy(&t));
    }
}

#[test]
fn instances_are_cached_and_change_shaping() {
    let (fonts, id) = flex();
    let narrow = fonts.add_instance(id, &[(*b"wdth", 75.0)]).unwrap();
    let wide = fonts.add_instance(id, &[(*b"wdth", 125.0)]).unwrap();
    assert_ne!(narrow, wide);
    assert_eq!(fonts.add_instance(id, &[(*b"wdth", 75.0)]), Some(narrow), "the same values give the same id");
    let (n, d, w) = (width(&fonts, narrow, "WILDCARD"), width(&fonts, id, "WILDCARD"), width(&fonts, wide, "WILDCARD"));
    assert!(n < d && d < w, "{n} {d} {w}");
    let heavy = fonts.add_instance(id, &[(*b"wght", 900.0)]).unwrap();
    assert!(width(&fonts, heavy, "WILDCARD") > d);
    assert!(fonts.add_instance(999, &[]).is_none());
}

#[test]
fn live_text_sets_one_word_in_its_own_instance() {
    let (fonts, id) = flex();
    let wide = fonts.add_instance(id, &[(*b"wdth", 125.0), (*b"wght", 700.0)]).unwrap();
    let words: Vec<Word> =
        [(1, "same"), (2, "word"), (3, "here")].iter().map(|&(id, t)| Word { id, text: t.into() }).collect();
    let view = |fonts_by_word: Vec<(u32, u32)>| {
        Element::<()>::column("root").child(
            Element::live_text("live", &words)
                .text_style(TextStyle::new(id, 20.0, 26.0))
                .width(600.0)
                .word_fonts(fonts_by_word),
        )
    };
    let mut plain = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    plain.apply(view(Vec::new()), 0.0);
    let a = plain.sample(1.0);
    let mut s = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    s.apply(view(vec![(2, wide)]), 0.0);
    let b = s.sample(1.0);
    let font_of = |l: &vo_scene::display::DisplayList, w: u32| {
        l.glyphs.iter().filter(|g| g.word == w).map(|g| g.font).collect::<Vec<_>>()
    };
    assert!(font_of(&b, 2).iter().all(|&f| f == wide));
    assert!(font_of(&b, 1).iter().all(|&f| f == id) && font_of(&b, 3).iter().all(|&f| f == id));
    // The wider word pushes the word after it right.
    let x = |l: &vo_scene::display::DisplayList, w: u32| l.glyphs.iter().find(|g| g.word == w).unwrap().pos.x;
    assert!(x(&b, 3) > x(&a, 3) + 2.0, "{} {}", x(&b, 3), x(&a, 3));
    assert_eq!(x(&b, 1), x(&a, 1));
}
