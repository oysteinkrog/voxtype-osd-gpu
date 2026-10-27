//! The active-line reading stage (T14): the newest line holds its place, older lines move
//! up, shrink and dim, and a word that rewraps onto a new line does not jump.

use vo_core::words::Word;
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::Reading;
use vo_scene::Scene;

fn style() -> TextStyle {
    TextStyle::new(0, 20.0, 24.0)
}

/// "w00 w01" is 72 units wide with the estimate layout; a third word does not fit.
const WIDTH: f32 = 110.0;
const LINE_Y: f32 = 60.0;

fn words(texts: &[&str]) -> Vec<Word> {
    texts.iter().enumerate().map(|(i, t)| Word { id: i as u32 + 1, text: t.to_string() }).collect()
}

fn view(texts: &[&str]) -> Element<()> {
    let text = Element::live_text("t", &words(texts))
        .text_style(style())
        .width(WIDTH)
        .height(72.0)
        .reading(Reading::teleprompter(LINE_Y));
    Element::column("root").child(text)
}

/// `(x, y, size, alpha)` of the first glyph of word `id`.
fn first_glyph(list: &DisplayList, id: u32) -> Option<(f32, f32, f32, f32)> {
    list.glyphs.iter().find(|g| g.word == id).map(|g| (g.pos.x, g.pos.y, g.size, g.color.a))
}

fn centre_y(scene: &Scene, t: f64) -> f32 {
    let id = scene.find(&["root", "t"]).unwrap();
    let r = scene.reading_line(id, t).expect("a reading line");
    r.y + 0.5 * r.h
}

#[test]
fn the_newest_line_holds_its_place_and_older_lines_recede() {
    let mut scene = Scene::new();
    scene.apply(view(&["w00", "w01"]), 0.0);
    assert!((centre_y(&scene, 1.0) - LINE_Y).abs() < 0.01);
    let before = first_glyph(&scene.sample(1.0), 1).unwrap();

    // A third word starts line 2: it appears on the stage, and line 1 moves up a line.
    scene.apply(view(&["w00", "w01", "w02"]), 2.0);
    for t in [2.0, 2.05, 2.1, 3.0] {
        assert!((centre_y(&scene, t) - LINE_Y).abs() < 0.01, "newest line at {} at {t}", centre_y(&scene, t));
    }
    let new_word = first_glyph(&scene.sample(2.0), 3).unwrap();
    assert!((new_word.1 - before.1).abs() < 0.01, "the new line starts where line 1 was");
    let mid = first_glyph(&scene.sample(2.08), 1).unwrap();
    assert!(mid.1 < before.1 - 1.0 && mid.1 > before.1 - 24.0, "line 1 on its way up: {mid:?}");
    let old = first_glyph(&scene.sample(3.0), 1).unwrap();
    assert!(
        (old.1 - (before.1 - 24.0 - 0.12 * 0.28 * 20.0)).abs() < 0.05,
        "line 1 one line up: {old:?} from {before:?}"
    );
    assert!((old.2 - 0.88 * 20.0).abs() < 0.01, "and at 0.88 scale");
    assert!((old.3 - 0.45 * before.3).abs() < 0.01, "and at 0.45 opacity");
    assert!((old.0 - before.0).abs() < 0.01, "scaled about the left end");
}

#[test]
fn a_rewrapped_word_does_not_jump() {
    let mut scene = Scene::new();
    scene.apply(view(&["w00", "w01"]), 0.0);
    // w01 grows past the line width and rewraps onto a new line.
    let before = first_glyph(&scene.sample(1.0), 2).unwrap();
    scene.apply(view(&["w00", "w01wwww"]), 1.0);
    let at = first_glyph(&scene.sample(1.0), 2).unwrap();
    assert!((at.0 - before.0).abs() < 0.5 && (at.1 - before.1).abs() < 0.5, "{at:?} from {before:?}");
    // It ends at the start of the stage line, and the stage line is where it was.
    let end = first_glyph(&scene.sample(3.0), 2).unwrap();
    assert!(end.0 < 1.0, "{end:?}");
    assert!((end.1 - before.1).abs() < 0.5, "{end:?}");
    assert!((centre_y(&scene, 3.0) - LINE_Y).abs() < 0.01);
}
