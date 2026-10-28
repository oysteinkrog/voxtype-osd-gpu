//! The final-pass sweep (T10): an outside fraction places a hairline in reading order, each
//! word brightens briefly as the pass reaches it, words reached together start 40 ms apart,
//! and `Scene::final_pass_reached` tells the app when to release a word's final text.
//!
//! The width estimate lays out 11 units per character and 6 per space at size 20: "one" is
//! 0 to 33, "two" 39 to 72, and in a box 80 wide "three" wraps to the second line, 0 to 55.
//! Reading length: 72 on line 1 and 55 on line 2, 127 in all.

use vo_core::words::Word;
use vo_scene::display::{DisplayList, ShapeKind};
use vo_scene::element::{Element, TextStyle};
use vo_scene::final_pass::{FinalPass, PULSE};
use vo_scene::geom::Color;
use vo_scene::Scene;

const INK: Color = Color::rgba(0.5, 0.5, 0.5, 1.0);

fn view(at: Option<f32>) -> Element<()> {
    view_with(at, &[])
}

fn view_with(at: Option<f32>, finals: &[Word]) -> Element<()> {
    let words: Vec<Word> =
        [(1, "one"), (2, "two"), (3, "three")].into_iter().map(|(id, t)| Word { id, text: t.into() }).collect();
    let mut live = Element::live_text("live", &words).text_style(TextStyle::new(0, 20.0, 26.0)).width(80.0).color(INK);
    if let Some(at) = at {
        live = live.final_pass(FinalPass::new(at));
    }
    if !finals.is_empty() {
        live = live.final_words(finals);
    }
    Element::column("root").child(live)
}

/// The hairline's x and its top and bottom, if drawn.
fn hairline(list: &DisplayList) -> Option<(f32, f32, f32)> {
    list.shapes.iter().find(|s| s.kind == ShapeKind::Line).map(|s| (s.geom[0], s.geom[1], s.geom[3]))
}

/// The red channel of `word`'s first glyph: 0.5 is the text colour, more is brighter.
fn ink(list: &DisplayList, word: u32) -> f32 {
    list.glyphs.iter().find(|g| g.word == word).unwrap().color.r
}

#[test]
fn the_hairline_reads_line_after_line() {
    let mut s = Scene::<()>::new();
    s.apply(view(Some(0.5)), 0.0);
    let (x, top, bottom) = hairline(&s.sample(0.0)).expect("a hairline");
    assert!((x - 63.5).abs() < 1e-3, "half of 127 is on line 1: {x}");
    assert!(top < 16.0 && bottom > 16.0, "it spans the first baseline");
    s.apply(view(Some(0.8)), 1.0);
    let (x, top, _) = hairline(&s.sample(1.0)).unwrap();
    assert!((x - 29.6).abs() < 1e-3 && top > 16.0, "on line 2: {x} {top}");
    s.apply(view(None), 2.0);
    assert!(hairline(&s.sample(2.0)).is_none());
}

#[test]
fn words_brighten_briefly_as_the_pass_reaches_them() {
    let mut s = Scene::<()>::new();
    s.apply(view(Some(0.0)), 0.0);
    assert!((ink(&s.sample(0.5), 1) - 0.5).abs() < 1e-6, "nothing reached at 0");
    // 0.35 of 127 is 44.5: past the starts of "one" (0) and "two" (39).
    s.apply(view(Some(0.35)), 1.0);
    let early = s.sample(1.03);
    assert!(ink(&early, 1) > 0.7, "the first word lights at once: {}", ink(&early, 1));
    assert!((ink(&early, 2) - 0.5).abs() < 1e-6, "the second starts 40 ms later");
    let later = s.sample(1.08);
    assert!(ink(&later, 2) > 0.7, "{}", ink(&later, 2));
    assert!((ink(&later, 3) - 0.5).abs() < 1e-6, "the pass has not reached the third");
    let done = s.sample(1.04 + PULSE);
    assert!((ink(&done, 1) - 0.5).abs() < 1e-6 && (ink(&done, 2) - 0.5).abs() < 1e-6, "back to the text colour");
    assert_eq!(s.next_wake(1.04 + PULSE), None);
    // Moving on to the end lights the last word once.
    s.apply(view(Some(1.0)), 3.0);
    assert!(ink(&s.sample(3.03), 3) > 0.7);
    assert!((ink(&s.sample(3.03), 1) - 0.5).abs() < 1e-6, "earlier words do not light again");
}

#[test]
fn the_app_asks_whether_the_pass_reached_a_word() {
    let mut s = Scene::<()>::new();
    s.apply(view(Some(0.35)), 0.0);
    let id = s.find(&["root", "live"]).unwrap();
    assert!(s.final_pass_reached(id, 1) && s.final_pass_reached(id, 2));
    assert!(!s.final_pass_reached(id, 3));
    let (baseline, x) = s.final_pass_at(id).unwrap();
    assert!(baseline == 16.0 && (x - 44.45).abs() < 1e-3, "{x}");
    s.apply(view(None), 1.0);
    assert!(!s.final_pass_reached(id, 1), "no pass, nothing reached");
}

#[test]
fn a_word_takes_its_final_text_once_the_pass_reaches_it() {
    let finals: Vec<Word> =
        [(1, "One"), (2, "two,"), (3, "three.")].into_iter().map(|(id, t)| Word { id, text: t.into() }).collect();
    let first =
        |list: &DisplayList, word: u32| char::from_u32(list.glyphs.iter().find(|g| g.word == word).unwrap().glyph);
    let last =
        |list: &DisplayList, word: u32| char::from_u32(list.glyphs.iter().rfind(|g| g.word == word).unwrap().glyph);
    let mut s = Scene::<()>::new();
    s.apply(view_with(Some(0.0), &finals), 0.0);
    assert_eq!(first(&s.sample(0.5), 1), Some('o'), "nothing reached yet");
    // 0.35 reaches "one" and "two" but not "three".
    s.apply(view_with(Some(0.35), &finals), 1.0);
    let list = s.sample(2.0);
    assert_eq!(first(&list, 1), Some('O'));
    assert_eq!(last(&list, 2), Some(','));
    assert_eq!(last(&list, 3), Some('e'), "the third keeps its text");
    s.apply(view_with(Some(1.0), &finals), 3.0);
    assert_eq!(last(&s.sample(4.0), 3), Some('.'));
}
