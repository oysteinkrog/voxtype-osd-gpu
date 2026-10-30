//! Live text with font fallback: Japanese words are drawn in the fallback face while they
//! sit, while they crossfade and while they fade out, and Latin words stay in the primary.

use vo_core::words::Word;
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::Revise;
use vo_scene::font::{FontId, FontRegistry};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

fn scene() -> (Scene<()>, FontId, FontId) {
    let fonts = FontRegistry::new();
    let latin = fonts.asset("sans").unwrap();
    let jp = fonts.asset("jp-labels").unwrap();
    fonts.set_fallbacks(latin, &[jp]);
    (Scene::new().with_text(Box::new(ShapedLayout::new(fonts))), latin, jp)
}

fn view(list: &[(u32, &str)], latin: FontId) -> Element<()> {
    let w: Vec<Word> = list.iter().map(|&(id, text)| Word { id, text: text.into() }).collect();
    let live =
        Element::live_text("live", &w).text_style(TextStyle::new(latin, 20.0, 26.0)).word_revise(Revise::Crossfade);
    Element::column("root").child(live)
}

#[test]
fn fallback_glyphs_keep_their_face_through_revisions_and_removal() {
    let (mut s, latin, jp) = scene();
    s.apply(view(&[(1, "音声"), (2, "ok")], latin), 0.0);
    let fonts_of = |list: &vo_scene::DisplayList, word: u32| -> Vec<FontId> {
        list.glyphs.iter().filter(|g| g.word == word).map(|g| g.font).collect()
    };
    let rest = s.sample(1.0);
    assert_eq!(fonts_of(&rest, 1), [jp, jp]);
    assert_eq!(fonts_of(&rest, 2), [latin, latin]);
    assert!(rest.glyphs.iter().all(|g| g.glyph != 0));

    // Mid crossfade the old ghosts and the new glyphs of word 1 are all Japanese.
    s.apply(view(&[(1, "入力"), (2, "ok")], latin), 1.0);
    let mid = s.sample(1.05);
    let w1 = fonts_of(&mid, 1);
    assert_eq!(w1.len(), 4, "two ghosts and two new glyphs");
    assert!(w1.iter().all(|&f| f == jp), "{w1:?}");

    // A removed word fades out in its own face. It is the last word: a word dropped from the
    // front goes as soon as the words sliding into its place reach it.
    s.apply(view(&[(1, "入力"), (2, "ok"), (3, "音声")], latin), 2.0);
    s.apply(view(&[(1, "入力"), (2, "ok")], latin), 3.0);
    let gone = s.sample(3.05);
    let w3 = fonts_of(&gone, 3);
    assert!(!w3.is_empty() && w3.iter().all(|&f| f == jp), "{w3:?}");
}
