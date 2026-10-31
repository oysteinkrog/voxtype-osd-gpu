//! Real text in the scene: labels and live text place the font's glyphs, and live text
//! tags each glyph with its word.

use skrifa::MetadataProvider;
use vo_core::words::Word;
use vo_scene::display::NO_WORD;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

fn scene(fonts: &FontRegistry) -> Scene<()> {
    Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())))
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

#[test]
fn label_places_real_glyphs() {
    let fonts = fonts();
    let mut s = scene(&fonts);
    s.apply(Element::column("root").child(Element::label("l", "Hi")), 0.0);
    let list = s.sample(0.0);
    let face = fonts.get(0).unwrap();
    let cmap = face.skrifa().charmap();
    let ids: Vec<u32> = list.glyphs.iter().map(|g| g.glyph).collect();
    assert_eq!(ids, vec![cmap.map('H').unwrap().to_u32(), cmap.map('i').unwrap().to_u32()]);
    assert!(list.glyphs.iter().all(|g| g.word == NO_WORD && g.font == 0));
}

#[test]
fn live_text_tags_glyphs_with_word_ids() {
    let fonts = fonts();
    let mut s = scene(&fonts);
    let words = [Word { id: 7, text: "good".into() }, Word { id: 9, text: "morning".into() }];
    s.apply(Element::column("root").child(Element::live_text("live", &words)), 0.0);
    let list = s.sample(0.0);
    let tags: Vec<u32> = list.glyphs.iter().map(|g| g.word).collect();
    assert_eq!(tags, [vec![7; 4], vec![9; 7]].concat());
}

#[test]
fn an_added_ellipsis_belongs_to_no_word() {
    let fonts = fonts();
    let mut s = scene(&fonts);
    let words: Vec<Word> = ["one", "two", "three", "four", "five", "six"]
        .iter()
        .zip(1..)
        .map(|(w, id)| Word { id, text: w.to_string() })
        .collect();
    let live = Element::live_text("live", &words).text_style(TextStyle::default().clamp_lines(1)).width(60.0);
    s.apply(Element::column("root").child(live), 0.0);
    let list = s.sample(0.0);
    let last = list.glyphs.last().unwrap();
    assert_eq!(last.word, NO_WORD);
    assert!(list.glyphs.iter().rev().skip(1).all(|g| g.word != NO_WORD));
}

#[test]
fn live_words_enter_and_change_with_their_own_motion() {
    use vo_scene::Transition;
    let fonts = fonts();
    let mut s = scene(&fonts);
    let view = |words: &[Word]| {
        Element::column("root").child(
            Element::live_text("live", words)
                .word_enter(Transition::fade().offset(0.0, 6.0))
                .word_arrival(vo_scene::arrival::Arrival::Free)
                .word_change(Transition::NONE.opacity(0.5))
                // The whole word takes the change: no palimpsest per letter (MO-20).
                .word_revise(vo_scene::flow::Revise::Swap),
        )
    };
    let w = |id, t: &str| Word { id, text: t.into() };
    s.apply(view(&[w(1, "open"), w(2, "the")]), 0.0);
    s.apply(view(&[w(1, "open"), w(2, "the"), w(3, "confi")]), 1.0);
    let alpha_of = |s: &Scene<()>, t: f64, id: u32| {
        s.sample(t).glyphs.iter().filter(|g| g.word == id).map(|g| g.color.a).fold(f32::NAN, f32::min)
    };
    let y_of = |s: &Scene<()>, t: f64, id: u32| s.sample(t).glyphs.iter().find(|g| g.word == id).unwrap().pos.y;
    // Word 3 just entered: transparent and lower; the others are at rest.
    assert!(alpha_of(&s, 1.0, 3).is_nan(), "a transparent word draws no glyphs");
    assert_eq!(alpha_of(&s, 1.0, 1), 1.0);
    let mid = alpha_of(&s, 1.05, 3);
    assert!(mid > 0.0 && mid < 1.0, "{mid}");
    assert!(y_of(&s, 1.05, 3) > y_of(&s, 1.05, 2));
    assert_eq!(s.next_wake(1.05), Some(1.05));
    assert_eq!(alpha_of(&s, 3.0, 3), 1.0);
    assert_eq!(y_of(&s, 3.0, 3), y_of(&s, 3.0, 2));
    assert_eq!(s.next_wake(3.0), None);
    // Same id, new text: the change transition; word 2 dropped.
    s.apply(view(&[w(1, "open"), w(3, "configuration")]), 3.0);
    assert_eq!(alpha_of(&s, 3.0, 3), 0.5);
    assert_eq!(alpha_of(&s, 3.0, 1), 1.0);
    // A dropped word whose place the next word takes goes at once (crate::flow): fading, it
    // would be read under that word.
    assert!(alpha_of(&s, 3.05, 2).is_nan());
    assert!(s.sample(4.0).glyphs.iter().all(|g| g.word != 2));
}
