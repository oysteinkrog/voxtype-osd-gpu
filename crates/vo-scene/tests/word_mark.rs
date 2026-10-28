//! The current-word mark (T04, T05): a decoration owned by the "current word" role. It
//! springs from word to word, the karaoke underline stretches before it contracts, the
//! accent colour leaves the old word in about 150 ms, and a line jump runs the old piece
//! off its line while the new one enters on the next.
//!
//! The width estimate lays out 11 units per character and 6 per space at size 20: "one" is
//! 0 to 33, "two" 39 to 72, and in a box 80 wide "three" wraps to the second line, 0 to 55.

use vo_core::words::Word;
use vo_scene::display::{DisplayList, ShapeKind};
use vo_scene::element::{Element, TextStyle};
use vo_scene::geom::Color;
use vo_scene::{MarkShape, Scene, WordMark};

const ACCENT: Color = Color::rgba(1.0, 0.3, 0.2, 1.0);

fn words() -> Vec<Word> {
    [(1, "one"), (2, "two"), (3, "three")].into_iter().map(|(id, t)| Word { id, text: t.into() }).collect()
}

fn view(current: Option<u32>, shape: MarkShape) -> Element<()> {
    let live = Element::live_text("live", &words())
        .text_style(TextStyle::new(0, 20.0, 26.0))
        .width(80.0)
        .current_word(current)
        .word_mark(WordMark::new(shape, Color::WHITE).accent(ACCENT));
    Element::column("root").child(live)
}

/// The mark's pieces as `(x0, x1, y)`, left to right.
fn pieces(list: &DisplayList) -> Vec<(f32, f32, f32)> {
    let mut v: Vec<_> = list
        .shapes
        .iter()
        .filter(|s| matches!(s.kind, ShapeKind::Capsule | ShapeKind::RRect) && s.fill.c0.a > 0.0)
        .map(|s| (s.geom[0], s.geom[0] + s.geom[2], s.geom[1]))
        .collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v
}

/// The green channel of the first glyph of `word`.
fn green(list: &DisplayList, word: u32) -> f32 {
    list.glyphs.iter().find(|g| g.word == word).unwrap().color.g
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.05
}

#[test]
fn the_mark_sits_under_the_current_word() {
    let mut s = Scene::<()>::new();
    s.apply(view(Some(1), MarkShape::Underline), 0.0);
    let list = s.sample(1.0);
    let p = pieces(&list);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(near(p[0].0, 0.0) && near(p[0].1, 33.0), "{p:?}");
    // Under the first line's baseline at 16.
    assert!(p[0].2 > 16.0 && p[0].2 < 21.0, "{p:?}");
    assert!(near(green(&list, 1), ACCENT.g), "the current word takes the accent");
    assert!(near(green(&list, 2), 1.0), "the others keep the text colour");
}

#[test]
fn a_box_springs_both_edges_together() {
    let mut s = Scene::<()>::new();
    s.apply(view(Some(1), MarkShape::Box), 0.0);
    s.apply(view(Some(2), MarkShape::Box), 1.0);
    let p = pieces(&s.sample(1.06))[0];
    // Both edges are part way, and the width stays near one word's.
    let w = p.1 - p.0;
    assert!(p.0 > 0.0 && p.0 < 39.0, "{p:?}");
    assert!((w - (33.0 + 2.0 * 3.6)).abs() < 2.0, "{w}");
    let p = pieces(&s.sample(2.0))[0];
    assert!(near(p.0, 39.0 - 3.6) && near(p.1, 72.0 + 3.6), "{p:?}");
}

#[test]
fn karaoke_stretches_to_the_next_word_then_contracts() {
    let mut s = Scene::<()>::new();
    s.apply(view(Some(1), MarkShape::Karaoke), 0.0);
    s.apply(view(Some(2), MarkShape::Karaoke), 1.0);
    let widths: Vec<f32> = [1.04, 1.08, 1.12, 1.2, 1.3, 2.0]
        .iter()
        .map(|&t| {
            let p = pieces(&s.sample(t))[0];
            p.1 - p.0
        })
        .collect();
    let widest = widths.iter().cloned().fold(0.0, f32::max);
    assert!(widest > 50.0, "it spans parts of both words on the way: {widths:?}");
    assert!(near(widths[5], 33.0), "then fits the new word: {widths:?}");
    // The leading edge gets there well before the trailing edge.
    let p = pieces(&s.sample(1.12))[0];
    assert!(p.1 > 70.0 && p.0 < 36.0, "{p:?}");
}

#[test]
fn the_accent_leaves_the_old_word_in_about_150_ms() {
    let mut s = Scene::<()>::new();
    s.apply(view(Some(1), MarkShape::Underline), 0.0);
    s.apply(view(Some(2), MarkShape::Underline), 1.0);
    let mid = s.sample(1.05);
    assert!(green(&mid, 1) > ACCENT.g + 0.02 && green(&mid, 1) < 0.98, "part way back: {}", green(&mid, 1));
    let done = s.sample(1.16);
    assert!(green(&done, 1) > 0.97, "back to the text colour: {}", green(&done, 1));
    assert!(green(&done, 2) < ACCENT.g + 0.03, "and on the new word: {}", green(&done, 2));
}

#[test]
fn a_line_jump_runs_off_the_old_line_and_enters_the_new_one() {
    let mut s = Scene::<()>::new();
    s.apply(view(Some(2), MarkShape::Underline), 0.0);
    s.apply(view(Some(3), MarkShape::Underline), 1.0);
    let p = pieces(&s.sample(1.06));
    assert_eq!(p.len(), 2, "one piece on each line: {p:?}");
    let (new, old) = if p[0].2 > p[1].2 { (p[0], p[1]) } else { (p[1], p[0]) };
    assert!(old.0 > 39.0 && old.1 <= 72.0 + 1e-3, "the old piece runs off line 1's end: {old:?}");
    assert!(near(new.0, 0.0) && new.1 < 55.0, "the new piece enters at line 2's start: {new:?}");
    assert!(new.2 > 42.0 && new.2 < 47.0, "under the second baseline: {new:?}");
    let p = pieces(&s.sample(2.0));
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(near(p[0].0, 0.0) && near(p[0].1, 55.0), "{p:?}");
}

#[test]
fn no_current_word_fades_the_mark_out() {
    let mut s = Scene::<()>::new();
    s.apply(view(Some(1), MarkShape::Underline), 0.0);
    s.apply(view(None, MarkShape::Underline), 1.0);
    assert!(pieces(&s.sample(2.0)).is_empty());
    assert!(near(green(&s.sample(2.0), 1), 1.0));
    assert_eq!(s.next_wake(2.0), None);
    // An id that is not among the words counts as none.
    s.apply(view(Some(9), MarkShape::Underline), 3.0);
    assert!(pieces(&s.sample(4.0)).is_empty());
}

#[test]
fn an_accent_gradient_runs_across_the_current_word() {
    let (c0, mid, c1) =
        (Color::rgba(1.0, 0.0, 0.0, 1.0), Color::rgba(0.0, 1.0, 0.0, 1.0), Color::rgba(0.0, 0.0, 1.0, 1.0));
    let mut s = Scene::<()>::new();
    let live = Element::live_text("live", &words())
        .text_style(TextStyle::new(0, 20.0, 26.0))
        .width(80.0)
        .current_word(Some(3))
        .word_mark(WordMark::new(MarkShape::Underline, Color::WHITE).accent_gradient(c0, mid, c1));
    s.apply(Element::column("root").child(live), 0.0);
    let list = s.sample(1.0);
    let glyphs: Vec<_> = list.glyphs.iter().filter(|g| g.word == 3).collect();
    assert_eq!(glyphs.len(), 5);
    let (first, middle, last) = (glyphs[0].color, glyphs[2].color, glyphs[4].color);
    assert!(first.r > 0.7 && first.b < 0.05, "red at the left: {first:?}");
    assert!(middle.g > 0.9, "green in the middle: {middle:?}");
    assert!(last.b > 0.7 && last.r < 0.05, "blue at the right: {last:?}");
    assert!(near(green(&list, 1), 1.0), "other words keep the text colour");
}
