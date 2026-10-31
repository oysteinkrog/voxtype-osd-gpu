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

/// Words in their own instance, in place: every glyph stands where the plain layout puts it,
/// on the same line, and only the font it is drawn in changes. Also when the style's font is
/// another face than the variable font (a look's own face with the voice breathing in Flex).
#[test]
fn word_fonts_in_place_keep_every_place_and_line_break() {
    let (fonts, flex) = flex();
    let sans = fonts.asset("sans").unwrap();
    let wide = fonts.add_instance(flex, &[(*b"wdth", 125.0), (*b"wght", 900.0)]).unwrap();
    let words: Vec<Word> = [(1, "breathe"), (2, "with"), (3, "the"), (4, "voice"), (5, "here")]
        .iter()
        .map(|&(id, t)| Word { id, text: t.into() })
        .collect();
    for base in [flex, sans] {
        let style = TextStyle::new(base, 20.0, 26.0);
        // Just wide enough for the first three words in the base font.
        let plain = ShapedLayout::new(fonts.clone());
        let three = vo_scene::text::TextLayout::layout(&plain, "breathe with the", &style, None).size.w;
        let view = |in_place: bool, fonts_by_word: Vec<(u32, u32)>| {
            let live = Element::live_text("live", &words).text_style(style).width(three + 2.0);
            let live = if in_place { live.word_fonts_in_place(fonts_by_word) } else { live.word_fonts(fonts_by_word) };
            Element::<()>::column("root").child(live)
        };
        let sample = |in_place: bool, by_word: Vec<(u32, u32)>| {
            let mut s = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
            s.apply(view(in_place, by_word), 0.0);
            s.sample(1.0)
        };
        let all_wide: Vec<(u32, u32)> = (1..=5).map(|w| (w, wide)).collect();
        let a = sample(false, Vec::new());
        let b = sample(true, all_wide.clone());
        // Each word starts at the same place on the same line; its letters may sit a little
        // differently inside it, which is the breath.
        let starts = |l: &vo_scene::display::DisplayList| {
            (1..=5)
                .map(|w| l.glyphs.iter().find(|g| g.word == w).map(|g| (g.pos.x, g.pos.y)).unwrap())
                .collect::<Vec<_>>()
        };
        for (w, (p, q)) in starts(&a).into_iter().zip(starts(&b)).enumerate() {
            assert!((p.0 - q.0).abs() < 0.01 && p.1 == q.1, "base {base}: word {} moved from {p:?} to {q:?}", w + 1);
        }
        assert_eq!(a.glyphs.len(), b.glyphs.len());
        assert!(b.glyphs.iter().all(|g| g.font == wide), "base {base}: drawn in the instance");
        // Without in place the wide words push "the" onto the next line.
        let c = sample(false, all_wide);
        let y = |l: &vo_scene::display::DisplayList, w: u32| l.glyphs.iter().find(|g| g.word == w).unwrap().pos.y;
        assert!(y(&c, 3) > y(&a, 3), "base {base}: the plain word_fonts reflow");
    }
}
