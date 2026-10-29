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

/// The opacity of `word`'s last glyph.
fn alpha(list: &DisplayList, word: u32) -> f32 {
    list.glyphs.iter().rfind(|g| g.word == word).unwrap().color.a
}

#[test]
fn words_ahead_of_the_front_are_spoken_ink_and_behind_it_set_type() {
    let mut s = Scene::<()>::new();
    s.apply(view(None), 0.0);
    assert_eq!(alpha(&s.sample(0.5), 1), 1.0, "no pass, no spoken ink");
    // 0.6 of 127 is 76: past line 1 (72), short of "three" (72 to 127).
    s.apply(view(Some(0.6)), 1.0);
    let list = s.sample(2.0);
    assert_eq!(alpha(&list, 1), 1.0, "set type behind the front");
    let a3 = alpha(&list, 3);
    assert!((a3 - 0.72).abs() < 0.02, "spoken ink ahead: {a3}");
    assert!(a3 >= 0.7, "the reading-line envelope keeps ink at 70 % or more");
    // Spoken ink comes in over 200 ms, not in a step.
    let mut s = Scene::<()>::new();
    s.apply(view(None), 0.0);
    s.apply(view(Some(0.0)), 1.0);
    assert!(alpha(&s.sample(1.05), 3) > 0.8, "it eases in");
    // Spoken ink is tracked looser: "three" is wider ahead of the front than set.
    let span = |list: &DisplayList| {
        let xs: Vec<f32> = list.glyphs.iter().filter(|g| g.word == 3).map(|g| g.pos.x).collect();
        xs.last().unwrap() - xs[0]
    };
    let mut s = Scene::<()>::new();
    s.apply(view(Some(0.0)), 0.0);
    let loose = span(&s.sample(1.0));
    s.apply(view(Some(1.0)), 1.0);
    let set = span(&s.sample(2.0));
    assert!(loose > set && loose - set <= 0.018 * 20.0 * 4.0 + 1e-3, "{loose} {set}");
}

#[test]
fn a_changed_word_nods_and_gets_a_bracket_and_only_changed_words_do() {
    let finals: Vec<Word> =
        [(1, "one"), (2, "Two"), (3, "three.")].into_iter().map(|(id, t)| Word { id, text: t.into() }).collect();
    let y = |list: &DisplayList, word: u32| list.glyphs.iter().find(|g| g.word == word).unwrap().pos.y;
    let brackets = |list: &DisplayList| list.shapes.iter().filter(|s| s.kind == ShapeKind::Line).count();
    let mut s = Scene::<()>::new();
    s.apply(view_with(Some(0.0), &finals), 0.0);
    let rest = y(&s.sample(0.5), 2);
    assert_eq!(brackets(&s.sample(0.5)), 1, "only the hairline");
    s.apply(view_with(Some(0.6), &finals), 1.0);
    let dip = y(&s.sample(1.11), 2);
    assert!(dip > rest + 1.5 && dip <= rest + 2.0 + 1e-3, "a 2 px nod: {rest} {dip}");
    assert_eq!(y(&s.sample(1.11), 1), y(&s.sample(0.5), 1), "an unchanged word does not nod");
    let later = s.sample(2.0);
    assert!((y(&later, 2) - rest).abs() < 1e-3, "and back");
    assert_eq!(brackets(&later), 1 + 2 * 3, "the hairline and a bracket of three strokes per changed word");
    // The capital grows on line 1, which is not the newest line.
    let size = |list: &DisplayList| list.glyphs.iter().find(|g| g.word == 2).unwrap().size;
    assert!(size(&s.sample(1.0)) < 0.85 * 20.0 && (size(&later) - 20.0).abs() < 1e-3);
    // The new full stop pops in. It is on the newest line, so it is readable at once (the
    // flow's arrival ink, at least 70 % of the letters beside it), not faded in from clear.
    let comma = |list: &DisplayList| {
        list.glyphs.iter().find(|g| g.word == 3 && g.glyph == '.' as u32).map_or(0.0, |g| g.color.a)
    };
    let mut s = Scene::<()>::new();
    s.apply(view_with(Some(0.0), &finals), 0.0);
    s.apply(view_with(Some(0.6), &finals), 1.0);
    let letter = |list: &DisplayList| {
        list.glyphs.iter().find(|g| g.word == 3 && g.glyph == 'e' as u32).map_or(0.0, |g| g.color.a)
    };
    let first = s.sample(1.0);
    let (arrives, beside) = (comma(&first), letter(&first));
    assert!(arrives >= 0.7 * beside, "pops in readable: {arrives} beside {beside}");
    assert!(comma(&s.sample(1.2)) > 0.7, "in, as spoken ink since it is ahead of the front");
}

#[test]
fn reduced_motion_keeps_the_swap_and_bracket_without_the_nod() {
    let finals: Vec<Word> =
        [(1, "one"), (2, "Two"), (3, "three.")].into_iter().map(|(id, t)| Word { id, text: t.into() }).collect();
    let mut s = Scene::<()>::new().with_reduced_motion(true);
    s.apply(view_with(Some(0.0), &finals), 0.0);
    let rest = s.sample(0.5).glyphs.iter().find(|g| g.word == 2).unwrap().pos.y;
    s.apply(view_with(Some(0.6), &finals), 1.0);
    let list = s.sample(1.11);
    let g: Vec<_> = list.glyphs.iter().filter(|g| g.word == 2).collect();
    assert_eq!(g[0].pos.y, rest, "no nod");
    assert_eq!(char::from_u32(g[0].glyph), Some('T'), "the swap");
    assert_eq!(g[0].size, 20.0, "no growth");
    assert_eq!(list.shapes.iter().filter(|s| s.kind == ShapeKind::Line).count(), 1 + 6, "the brackets at once");
}

#[test]
fn an_app_built_on_the_final_words_sets_every_live_text_in_its_view() {
    let finals: Vec<Word> =
        [(1, "One"), (2, "two"), (3, "three")].into_iter().map(|(id, t)| Word { id, text: t.into() }).collect();
    let live: Vec<Word> =
        [(1, "one"), (2, "two"), (3, "three")].into_iter().map(|(id, t)| Word { id, text: t.into() }).collect();
    let build = |at: f32| {
        let text = Element::live_text("live", &finals).text_style(TextStyle::new(0, 20.0, 26.0)).width(80.0).color(INK);
        let root = Element::column("root").child(Element::column("inner").child(text));
        let mut seen = 0;
        let root = root.map_live_texts(&mut |e| {
            seen += 1;
            assert_eq!(e.live_words().map(|w| w.len()), Some(3));
            e.final_from(&live).final_pass(FinalPass::new(at))
        });
        assert_eq!(seen, 1);
        root
    };
    let first = |list: &DisplayList| char::from_u32(list.glyphs.iter().find(|g| g.word == 1).unwrap().glyph);
    let mut s = Scene::<()>::new();
    s.apply(build(0.0), 0.0);
    assert_eq!(first(&s.sample(0.5)), Some('o'), "the live text until the pass reaches it");
    s.apply(build(0.2), 1.0);
    assert_eq!(first(&s.sample(1.5)), Some('O'));
}
