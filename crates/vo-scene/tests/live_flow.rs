//! Live text motion (T02, T11, T17): revisions crossfade in place, neighbours slide, words
//! that change line glide there in reading order, and a caret follows the last word.

use vo_core::words::Word;
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::{caret_alpha, Caret, Revise, CARET_HOLD, REWRAP_STAGGER};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

fn style() -> TextStyle {
    TextStyle::new(0, 20.0, 26.0)
}

fn scene() -> Scene<()> {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    Scene::new().with_text(Box::new(ShapedLayout::new(fonts)))
}

fn words(list: &[(u32, &str)]) -> Vec<Word> {
    list.iter().map(|&(id, text)| Word { id, text: text.into() }).collect()
}

fn view(w: &[Word], width: Option<f32>, caret: bool) -> Element<()> {
    let mut live = Element::live_text("live", w).text_style(style()).word_revise(Revise::Crossfade);
    if let Some(width) = width {
        live = live.width(width);
    }
    if caret {
        live = live.caret(Caret::default());
    }
    Element::column("root").child(live)
}

/// The first glyph of `word` drawn at full or partial opacity, not counting ghosts: the
/// pen position of the word's first current glyph.
fn first(list: &DisplayList, word: u32) -> (f32, f32) {
    let g = list.glyphs.iter().find(|g| g.word == word).unwrap();
    (g.pos.x, g.pos.y)
}

#[test]
fn a_revised_word_crossfades_and_its_neighbours_slide() {
    let mut s = scene();
    s.apply(view(&words(&[(1, "the"), (2, "quick"), (3, "fox")]), None, false), 0.0);
    let before = s.sample(1.0);
    let n_before = before.glyphs.len();
    s.apply(view(&words(&[(1, "the"), (2, "quickest"), (3, "fox")]), None, false), 1.0);
    let settled = s.sample(5.0);
    assert!(s.next_wake(5.0).is_none());
    assert_eq!(settled.glyphs.len(), n_before + 3, "no ghosts once settled");

    // At the change nothing jumps: "fox" is where it was, and only the old glyphs show.
    let at = s.sample(1.0);
    assert!((first(&at, 3).0 - first(&before, 3).0).abs() < 1e-3);
    let fox_new = first(&settled, 3).0;
    assert!(fox_new > first(&before, 3).0 + 10.0);

    // Mid move: "fox" is between, and old and new glyphs of word 2 both show, part faded.
    let mid = s.sample(1.06);
    let x = first(&mid, 3).0;
    assert!(x > first(&before, 3).0 + 0.5 && x < fox_new - 0.5, "fox at {x}");
    let w2: Vec<f32> = mid.glyphs.iter().filter(|g| g.word == 2).map(|g| g.color.a).collect();
    assert_eq!(w2.len(), 8 + 5, "new and old glyphs of the revised word");
    assert!(w2.iter().all(|&a| a > 0.02 && a < 0.98), "{w2:?}");

    // Settled glyphs sit exactly where a fresh layout puts them.
    let mut fresh = scene();
    fresh.apply(view(&words(&[(1, "the"), (2, "quickest"), (3, "fox")]), None, false), 0.0);
    assert_eq!(fresh.sample(0.0).glyphs, settled.glyphs);
}

#[test]
fn a_removed_word_fades_out_where_it_was() {
    let mut s = scene();
    s.apply(view(&words(&[(1, "one"), (2, "two")]), None, false), 0.0);
    let before = s.sample(1.0);
    let two = first(&before, 2);
    s.apply(view(&words(&[(1, "one")]), None, false), 1.0);
    let mid = s.sample(1.04);
    let ghost: Vec<_> = mid.glyphs.iter().filter(|g| g.word == 2).collect();
    assert_eq!(ghost.len(), 3);
    assert!((ghost[0].pos.x - two.0).abs() < 1e-3 && (ghost[0].pos.y - two.1).abs() < 1e-3);
    assert!(ghost[0].color.a > 0.0 && ghost[0].color.a < 1.0);
    assert!(s.sample(3.0).glyphs.iter().all(|g| g.word != 2));
}

#[test]
fn words_that_change_line_glide_in_reading_order() {
    let mut s = scene();
    let a = words(&[(1, "aaa"), (2, "bbb"), (3, "ccc"), (4, "ddd")]);
    // Wide enough for all four on one line.
    s.apply(view(&a, Some(300.0), false), 0.0);
    let before = s.sample(1.0);
    // A longer first word pushes the last two onto the second line.
    let b = words(&[(1, "aaaaaaaaaaaaaaaaaaaaa"), (2, "bbb"), (3, "ccc"), (4, "ddd")]);
    s.apply(view(&b, Some(300.0), false), 1.0);
    let settled = s.sample(5.0);
    let line2 = first(&settled, 1).1 + 26.0;
    let moved: Vec<u32> = (2..=4).filter(|&w| (first(&settled, w).1 - line2).abs() < 1e-3).collect();
    assert!(moved.len() >= 2, "words on the new line: {moved:?}");
    let (w0, w1) = (moved[0], moved[1]);

    // The first to move starts at once; the next waits one stagger step.
    let early = s.sample(1.0 + 0.5 * REWRAP_STAGGER);
    assert!(first(&early, w0) != first(&before, w0), "first mover is on its way");
    assert_eq!(first(&early, w1), first(&before, w1), "second mover waits");
    let later = s.sample(1.0 + 2.5 * REWRAP_STAGGER);
    assert!(first(&later, w1) != first(&before, w1));
    // Never past the end: mid glide is between the two places.
    let mid = s.sample(1.08);
    let y = first(&mid, w0).1;
    assert!(y > first(&before, w0).1 && y < line2, "y {y}");
}

#[test]
fn reduced_motion_moves_words_at_once() {
    let mut s = scene().with_reduced_motion(true);
    s.apply(view(&words(&[(1, "the"), (2, "quick"), (3, "fox")]), None, false), 0.0);
    s.apply(view(&words(&[(1, "the"), (2, "quickest"), (3, "fox")]), None, false), 1.0);
    let settled = s.sample(5.0);
    assert_eq!(first(&s.sample(1.0), 3), first(&settled, 3));
}

#[test]
fn the_caret_glides_to_the_end_and_blinks_when_idle() {
    let mut s = scene();
    s.apply(view(&words(&[(1, "hello")]), None, true), 0.0);
    let id = s.find(&["root", "live"]).unwrap();
    let (bar0, a0) = s.caret(id, 0.0).unwrap();
    assert_eq!(a0, 1.0);
    let end0 = s.word_bounds(id, 1, 0.0).unwrap().right();
    assert!(bar0.x > end0 && bar0.x < end0 + 4.0, "caret {bar0:?} after the word end {end0}");

    // A new word: the caret glides, no jump.
    s.apply(view(&words(&[(1, "hello"), (2, "world")]), None, true), 1.0);
    assert_eq!(s.caret(id, 1.0).unwrap().0.x, bar0.x);
    let end1 = s.word_bounds(id, 2, 5.0).unwrap().right();
    let mid = s.caret(id, 1.05).unwrap().0.x;
    assert!(mid > bar0.x && mid < end1, "caret mid glide at {mid}");
    assert!((s.caret(id, 1.5).unwrap().0.x - end1 - 2.0).abs() < 1e-2);

    // Solid while words arrive and for the hold after, then it blinks.
    assert_eq!(s.caret(id, 1.0 + CARET_HOLD - 0.01).unwrap().1, 1.0);
    let alphas: Vec<f32> = (0..40).map(|i| s.caret(id, 1.0 + CARET_HOLD + i as f64 * 0.05).unwrap().1).collect();
    assert!(alphas.contains(&0.0) && alphas.contains(&1.0), "{alphas:?}");
    // A blinking caret asks for frames: never settled, but not every frame while it rests.
    let period = 1.0 / vo_scene::flow::CARET_HZ;
    let on = 1.0 + CARET_HOLD + 3.0 * period + 0.1;
    let wake = s.next_wake(on).unwrap();
    assert!(wake > on + 0.2, "wake {wake} at {on}");
}

#[test]
fn caret_blink_shape() {
    assert_eq!(caret_alpha(0.0), 1.0);
    assert_eq!(caret_alpha(CARET_HOLD + 0.1), 1.0);
    let period = 1.0 / vo_scene::flow::CARET_HZ;
    assert_eq!(caret_alpha(CARET_HOLD + 0.8 * period), 0.0);
    let fading = caret_alpha(CARET_HOLD + 0.6 * period + 0.06);
    assert!(fading > 0.0 && fading < 1.0);
}
