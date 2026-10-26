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
