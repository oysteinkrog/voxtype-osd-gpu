//! Live words drawn by something else (`Element::words_drawn_elsewhere`): they keep their
//! place in the line, so the words after them do not move, but draw no glyphs.

use vo_core::words::Word;
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::Revise;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

fn scene() -> Scene<()> {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    Scene::new().with_text(Box::new(ShapedLayout::new(fonts)))
}

fn view(elsewhere: &[u32]) -> Element<()> {
    let w: Vec<Word> =
        [(1, "meet"), (2, "their"), (3, "team")].iter().map(|&(id, t)| Word { id, text: t.into() }).collect();
    Element::column("root").child(
        Element::live_text("live", &w)
            .text_style(TextStyle::new(0, 20.0, 26.0))
            .word_revise(Revise::Swap)
            .width(400.0)
            .words_drawn_elsewhere(elsewhere),
    )
}

fn xs(list: &DisplayList, word: u32) -> Vec<f32> {
    list.glyphs.iter().filter(|g| g.word == word).map(|g| g.pos.x).collect()
}

#[test]
fn a_word_drawn_elsewhere_keeps_its_place_and_draws_nothing() {
    let mut s = scene();
    s.apply(view(&[]), 0.0);
    let all = s.sample(1.0);
    assert_eq!(xs(&all, 2).len(), 5);
    s.apply(view(&[2]), 1.0);
    let hidden = s.sample(1.5);
    assert!(xs(&hidden, 2).is_empty(), "the word still draws");
    assert_eq!(xs(&hidden, 3), xs(&all, 3), "the next word moved");
    assert_eq!(xs(&hidden, 1), xs(&all, 1));
    // Handed back, it draws again where it was.
    s.apply(view(&[]), 1.5);
    assert_eq!(xs(&s.sample(2.0), 2), xs(&all, 2));
}
