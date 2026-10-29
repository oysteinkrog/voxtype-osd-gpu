//! Live text motion (T02, T11, T17): revisions crossfade in place, neighbours slide, words
//! that change line glide there in reading order, and a caret follows the last word.

use vo_core::words::Word;
use vo_scene::display::{DisplayList, GlyphInstance};
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::{caret_alpha, Caret, CaseChange, Revise, Rewrap, SettleInk, CARET_HOLD, REWRAP_STAGGER, X_TO_CAP};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::Color;
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

/// Live text 300 wide whose words glide to a new line (T11).
fn glide(w: &[Word]) -> Element<()> {
    let live = Element::live_text("live", w).text_style(style()).word_revise(Revise::Crossfade);
    Element::column("root").child(live.word_rewrap(Rewrap::Glide).width(300.0))
}

#[test]
fn words_that_change_line_glide_in_reading_order() {
    let mut s = scene();
    let a = words(&[(1, "aaa"), (2, "bbb"), (3, "ccc"), (4, "ddd")]);
    // Wide enough for all four on one line.
    s.apply(glide(&a), 0.0);
    let before = s.sample(1.0);
    // A longer first word pushes the last two onto the second line.
    let b = words(&[(1, "aaaaaaaaaaaaaaaaaaaaa"), (2, "bbb"), (3, "ccc"), (4, "ddd")]);
    s.apply(glide(&b), 1.0);
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

fn glyph_view(w: &[Word], case: CaseChange) -> Element<()> {
    let live = Element::live_text("live", w).text_style(style()).word_revise(Revise::Glyphs).word_case(case);
    Element::column("root").child(live)
}

/// The glyphs of `word` in draw order: current glyphs first, then ghosts.
fn of(list: &DisplayList, word: u32) -> Vec<GlyphInstance> {
    list.glyphs.iter().filter(|g| g.word == word).copied().collect()
}

#[test]
fn a_case_fix_rolls_only_the_changed_letter() {
    let mut s = scene();
    s.apply(glyph_view(&words(&[(1, "hello"), (2, "there")]), CaseChange::Roll), 0.0);
    let before = s.sample(1.0);
    s.apply(glyph_view(&words(&[(1, "Hello"), (2, "there")]), CaseChange::Roll), 1.0);
    let settled = s.sample(5.0);
    let base_y = of(&before, 1)[0].pos.y;

    let mid = of(&s.sample(1.06), 1);
    assert_eq!(mid.len(), 6, "five glyphs and the old h on its way out");
    let (new_h, old_h) = (mid[0], mid[5]);
    assert!(new_h.pos.y > base_y && new_h.color.a < 1.0, "new H comes in from below: {new_h:?}");
    assert!(old_h.pos.y < base_y && old_h.color.a < 1.0, "old h leaves upward: {old_h:?}");
    // The kept letters stay opaque and only slide, toward their new places.
    let (old, new) = (of(&before, 1), of(&settled, 1));
    for ((g, a), b) in mid[1..5].iter().zip(&old[1..5]).zip(&new[1..5]) {
        assert_eq!(g.color.a, 1.0);
        assert_eq!(g.pos.y, base_y);
        let (a, b) = (a.pos.x, b.pos.x);
        assert!(g.pos.x >= a.min(b) - 1e-3 && g.pos.x <= a.max(b) + 1e-3);
    }
    // At the change nothing jumps: the new H is still transparent, so the first glyph drawn
    // is the "e", where it was.
    let at = of(&s.sample(1.0), 1);
    assert_eq!(at[0].pos, of(&before, 1)[1].pos);
    assert_eq!(of(&settled, 1).len(), 5);
}

#[test]
fn an_inserted_comma_pops_in_from_the_baseline_and_pushes_the_next_word() {
    let mut s = scene();
    s.apply(glyph_view(&words(&[(1, "hello"), (2, "there")]), CaseChange::Roll), 0.0);
    let before = s.sample(1.0);
    s.apply(glyph_view(&words(&[(1, "hello,"), (2, "there")]), CaseChange::Roll), 1.0);
    let sizes: Vec<f32> = (0..30).map(|i| of(&s.sample(1.0 + i as f64 * 0.01), 1)[5].size).collect();
    assert!(sizes[1] < 20.0 * 0.8, "starts small: {sizes:?}");
    let peak = sizes.iter().copied().fold(0.0, f32::max);
    assert!(peak > 20.0 * 1.02 && peak < 20.0 * 1.2, "small overshoot: {peak}");
    assert!((sizes[29] - 20.0).abs() < 0.1, "settles at full size: {sizes:?}");
    // The comma sits on the baseline the whole way.
    let base_y = of(&before, 1)[0].pos.y;
    assert!((0..20).all(|i| of(&s.sample(1.0 + i as f64 * 0.01), 1)[5].pos.y == base_y));
    // The next word slides right by the comma's advance, without a jump.
    assert_eq!(first(&s.sample(1.0), 2), first(&before, 2));
    assert!(first(&s.sample(5.0), 2).0 > first(&before, 2).0 + 2.0);
    // The other letters of "hello" never move or fade.
    let mid = of(&s.sample(1.05), 1);
    for (g, old) in mid[..5].iter().zip(of(&before, 1)) {
        assert_eq!(g.pos, old.pos);
        assert_eq!(g.color.a, 1.0);
    }
}

#[test]
fn case_grow_scales_the_letter_from_x_height() {
    let mut s = scene();
    s.apply(glyph_view(&words(&[(1, "word")]), CaseChange::Grow), 0.0);
    s.apply(glyph_view(&words(&[(1, "Word")]), CaseChange::Grow), 1.0);
    let at = of(&s.sample(1.0001), 1);
    assert!((at[0].size - 20.0 * X_TO_CAP).abs() < 0.05, "new W starts at x-height: {}", at[0].size);
    let mid = of(&s.sample(1.05), 1);
    let ghost = mid.last().unwrap();
    assert!(ghost.size > 20.0 && mid[0].size < 20.0, "old grows, new grows to full: {mid:?}");
    assert_eq!(of(&s.sample(5.0), 1)[0].size, 20.0);
}

#[test]
fn a_mostly_new_word_crossfades_instead() {
    let mut s = scene();
    s.apply(glyph_view(&words(&[(1, "cat")]), CaseChange::Roll), 0.0);
    s.apply(glyph_view(&words(&[(1, "dog")]), CaseChange::Roll), 1.0);
    let mid = of(&s.sample(1.05), 1);
    assert_eq!(mid.len(), 6);
    assert!(mid.iter().all(|g| g.color.a > 0.0 && g.color.a < 1.0));
}

fn ink_view(w: &[Word], stable: Option<&[u32]>) -> Element<()> {
    let mut live = Element::live_text("live", w).text_style(style()).settling_ink(SettleInk::default());
    if let Some(ids) = stable {
        live = live.stable_words(ids);
    }
    Element::column("root").child(live)
}

fn ink(list: &DisplayList, word: u32) -> f32 {
    of(list, word)[0].color.a
}

fn spread(list: &DisplayList, word: u32) -> f32 {
    let g = of(list, word);
    g.last().unwrap().pos.x - g[0].pos.x
}

#[test]
fn new_words_arrive_light_and_loose_and_settle() {
    let mut s = scene();
    s.apply(ink_view(&words(&[(1, "settle")]), None), 0.0);
    let ink0 = SettleInk::default().ink;
    assert!((ink(&s.sample(0.0), 1) - ink0).abs() < 1e-3);
    let mid = ink(&s.sample(0.12), 1);
    assert!(mid > ink0 && mid < 1.0, "{mid}");
    assert_eq!(ink(&s.sample(1.0), 1), 1.0);
    assert!(spread(&s.sample(0.0), 1) > spread(&s.sample(1.0), 1) + 0.5, "looser on arrival");
    assert!(s.next_wake(1.0).is_none());
}

#[test]
fn a_revision_restarts_the_settle_and_busy_words_settle_slower() {
    let mut s = scene();
    s.apply(ink_view(&words(&[(1, "one"), (2, "two")]), None), 0.0);
    // Word 2 is revised three times; word 1 never.
    for (i, text) in ["too", "tow", "two"].iter().enumerate() {
        s.apply(ink_view(&words(&[(1, "one"), (2, text)]), None), 1.0 + i as f64);
    }
    assert!(ink(&s.sample(2.9), 2) > 0.999, "settled before the last revision");
    let ink0 = SettleInk::default().ink;
    assert!((ink(&s.sample(3.0), 2) - ink0).abs() < 1e-3, "starts again");
    assert_eq!(ink(&s.sample(3.0), 1), 1.0, "the untouched word stays settled");
    // A fresh word settles in about 300 ms, the busy one in about 600 ms.
    s.apply(ink_view(&words(&[(1, "one"), (2, "two"), (3, "three")]), None), 3.0);
    let (fresh, busy) = (ink(&s.sample(3.2), 3), ink(&s.sample(3.2), 2));
    assert!(busy < fresh - 0.05, "busy {busy}, fresh {fresh}");
}

#[test]
fn with_a_stable_list_a_word_holds_until_it_is_stable() {
    let mut s = scene();
    let w = words(&[(1, "pending")]);
    s.apply(ink_view(&w, Some(&[])), 0.0);
    let ink0 = SettleInk::default().ink;
    assert!((ink(&s.sample(2.0), 1) - ink0).abs() < 1e-3, "held at arrival ink");
    s.apply(ink_view(&w, Some(&[1])), 2.0);
    let mid = ink(&s.sample(2.1), 1);
    assert!(mid > ink0 && mid < 1.0);
    assert_eq!(ink(&s.sample(3.0), 1), 1.0);
}

#[test]
fn a_new_word_moves_with_the_words_its_line_moved() {
    let mut s = scene();
    s.apply(view(&words(&[(1, "so"), (2, "the")]), None, false), 0.0);
    let before = s.sample(1.0);
    // "so" grows, which pushes "the" right, and "fox" arrives after it at the same time.
    s.apply(view(&words(&[(1, "sooooo"), (2, "the"), (3, "fox")]), None, false), 1.0);
    let settled = s.sample(5.0);
    let the = (first(&before, 2).0, first(&settled, 2).0);
    assert!(the.1 > the.0 + 10.0, "the moves right: {the:?}");
    let gap = first(&settled, 3).0 - first(&settled, 2).0;
    // From the change on, "fox" keeps its settled distance from "the", so they never overlap
    // while "the" slides.
    for t in [1.0, 1.05, 1.1, 1.2] {
        let f = s.sample(t);
        let d = first(&f, 3).0 - first(&f, 2).0;
        assert!((d - gap).abs() < 0.5, "at {t} the gap is {d}, settled {gap}");
    }
}

/// Ten words over about three lines at 200 wide.
const TEN: [(u32, &str); 10] = [
    (1, "every"),
    (2, "word"),
    (3, "here"),
    (4, "keeps"),
    (5, "its"),
    (6, "line"),
    (7, "when"),
    (8, "the"),
    (9, "front"),
    (10, "drops"),
];

#[test]
fn when_the_front_line_drops_the_rest_scroll_up_as_one_block() {
    let mut s = scene();
    s.apply(view(&words(&TEN), Some(200.0), true), 0.0);
    let before = s.sample(1.0);
    // Drop the whole first line, as a line cap does.
    let y0 = first(&before, 1).1;
    let dropped: Vec<u32> = (1..=10).filter(|&w| first(&before, w).1 == y0).collect();
    let mut kept: Vec<(u32, &str)> = TEN.iter().copied().filter(|(id, _)| !dropped.contains(id)).collect();
    assert!(kept.len() >= 4 && dropped.len() >= 2, "dropped {dropped:?}");
    // A new word arrives in the same update, on the newest line.
    let block = kept.clone();
    kept.push((11, "next"));
    s.apply(view(&words(&kept), Some(200.0), true), 1.0);
    let settled = s.sample(5.0);
    let line = first(&before, kept[0].0).1 - first(&settled, kept[0].0).1;
    assert!(line > 10.0, "the block moves up a line: {line}");

    for t in [1.0, 1.03, 1.08, 1.121, 1.15] {
        let f = s.sample(t);
        // The new word is on the reading line: it waits while the line before scrolls out of
        // its row, then shows whole at its place, never sliding (MO-33, reading-line envelope).
        let new: Vec<&GlyphInstance> = f.glyphs.iter().filter(|g| g.word == 11).collect();
        if t < 1.0 + vo_scene::flow::overflow::DROP_HOLD {
            assert!(new.is_empty(), "at {t} the new word waits");
        } else {
            // In one step, so no frame catches it half shown.
            assert!(!new.is_empty() && new.iter().all(|g| g.color.a > 0.999), "at {t} the new word shows whole");
            assert!((first(&f, 11).1 - first(&settled, 11).1).abs() < 1e-3, "at {t} the new word is in place");
        }
        // Every kept word is the same distance below its new place, with no sideways move.
        let offsets: Vec<(f32, f32)> = block
            .iter()
            .map(|(id, _)| (first(&f, *id).0 - first(&settled, *id).0, first(&f, *id).1 - first(&settled, *id).1))
            .collect();
        let (_, dy) = offsets[0];
        for (id, (ox, oy)) in block.iter().map(|k| k.0).zip(&offsets) {
            assert!(ox.abs() < 1e-3 && (oy - dy).abs() < 1e-3, "at {t} word {id} is off by ({ox}, {oy}), block {dy}");
        }
        if t == 1.0 {
            assert!((dy - line).abs() < 1e-3, "nothing jumps at the change");
        } else {
            assert!(dy > 0.0 && dy < line, "at {t} the block is part way: {dy}");
            // The dropped words scroll out with it while they fade.
            let gone = dropped[0];
            if let Some(g) = f.glyphs.iter().find(|g| g.word == gone) {
                assert!((first(&before, gone).1 - g.pos.y - (line - dy)).abs() < 1e-2, "at {t}");
            }
        }
    }
    // The caret ends after the new word, on its line.
    let id = s.find(&["root", "live"]).unwrap();
    let cs = s.caret(id, 5.0).unwrap().0;
    let y = first(&settled, 11).1;
    assert!(cs.y < y && cs.y + cs.h > y - 1.0, "caret {cs:?} beside the new word at {y}");
}

#[test]
fn a_word_that_joins_a_held_line_waits_for_it() {
    // The front drops and word 11 is held on the new line; word 12 arrives on that line
    // while 11 still waits. It waits too, so the line never shows its second word first.
    let mut s = scene();
    s.apply(view(&words(&TEN), Some(200.0), false), 0.0);
    let before = s.sample(1.0);
    let y0 = first(&before, 1).1;
    let mut kept: Vec<(u32, &str)> = TEN.iter().copied().filter(|(id, _)| first(&before, *id).1 != y0).collect();
    kept.push((11, "a"));
    s.apply(view(&words(&kept), Some(200.0), false), 1.0);
    kept.push((12, "b"));
    s.apply(view(&words(&kept), Some(200.0), false), 1.05);
    let settled = s.sample(5.0);
    let rows = (first(&settled, 11).1, first(&settled, 12).1);
    assert_eq!(rows.0, rows.1, "both on the new line: {rows:?}");
    let shown = |t: f64, id: u32| s.sample(t).glyphs.iter().any(|g| g.word == id && g.color.a > 0.01);
    for t in [1.06, 1.1] {
        assert!(!shown(t, 11) && !shown(t, 12), "at {t} the line waits");
    }
    assert!(shown(1.2, 11) && shown(1.2, 12), "both show once the line is clear");
}

#[test]
fn a_new_first_line_that_shifts_along_scrolls_with_the_block() {
    // A look that puts "…" on the new first word moves that line along as the front drops.
    // Its words still scroll with the block and slide along their row, drawn once: no copy
    // fades out where the line was.
    let mut s = scene();
    s.apply(view(&words(&TEN), Some(200.0), false), 0.0);
    let before = s.sample(1.0);
    let y0 = first(&before, 1).1;
    let mut kept: Vec<(u32, String)> =
        TEN.iter().filter(|(id, _)| first(&before, *id).1 != y0).map(|&(id, w)| (id, w.to_string())).collect();
    kept[0].1 = format!("…{}", kept[0].1);
    let row: Vec<u32> =
        kept.iter().map(|k| k.0).filter(|&id| first(&before, id).1 == first(&before, kept[0].0).1).collect();
    assert!(row.len() >= 2, "a line of words to follow: {row:?}");
    let list: Vec<(u32, &str)> = kept.iter().map(|(id, w)| (*id, w.as_str())).collect();
    s.apply(view(&words(&list), Some(200.0), false), 1.0);
    let settled = s.sample(5.0);
    let line = first(&before, row[1]).1 - first(&settled, row[1]).1;
    let count = |l: &DisplayList, id: u32| l.glyphs.iter().filter(|g| g.word == id).count();
    let f = s.sample(1.08);
    let dy = first(&f, kept.last().unwrap().0).1 - first(&settled, kept.last().unwrap().0).1;
    assert!(dy > 0.0 && dy < line, "the block is part way: {dy}");
    for &id in &row[1..] {
        assert_eq!(count(&f, id), count(&settled, id), "word {id} is drawn once");
        let oy = first(&f, id).1 - first(&settled, id).1;
        assert!((oy - dy).abs() < 1e-2, "word {id} scrolls with the block: {oy}, block {dy}");
        assert!(first(&settled, id).0 > first(&before, id).0, "word {id} moves along its line");
    }
}

#[test]
fn a_true_rewrap_still_glides_word_by_word() {
    // Words that leave from the front and also change the wrap of the rest: only the words
    // that kept their place relative to the block move with it.
    let mut s = scene();
    s.apply(view(&words(&TEN), Some(200.0), false), 0.0);
    let before = s.sample(1.0);
    let kept: Vec<Word> = words(&TEN[1..]);
    s.apply(view(&kept, Some(200.0), false), 1.0);
    let settled = s.sample(5.0);
    // Word 2 moves to the start of the line: a sideways glide, not a scroll.
    assert!(first(&before, 2).0 > first(&settled, 2).0 + 10.0);
    let mid = s.sample(1.08);
    let x = first(&mid, 2).0;
    assert!(x > first(&settled, 2).0 + 0.5 && x < first(&before, 2).0 - 0.5, "x {x}");
}

fn accent_view(w: &[Word]) -> Element<()> {
    let ink = SettleInk::default().accent(Color::rgba(1.0, 0.0, 0.0, 1.0));
    Element::column("root")
        .child(Element::live_text("live", w).text_style(style()).color(Color::WHITE).settling_ink(ink))
}

#[test]
fn with_an_accent_new_words_arrive_in_it_and_mix_to_the_text_colour() {
    let mut s = scene();
    s.apply(accent_view(&words(&[(1, "settle")])), 0.0);
    let c = |s: &Scene<()>, t: f64| of(&s.sample(t), 1)[0].color;
    let (c0, mid, end) = (c(&s, 0.0), c(&s, 0.15), c(&s, 1.0));
    assert!(c0.r > 0.99 && c0.g < 0.01 && c0.b < 0.01, "arrives in the accent: {c0:?}");
    assert!(mid.g > 0.05 && mid.g < 0.95, "mixing: {mid:?}");
    assert!(end.g > 0.999 && end.b > 0.999, "settled to the text colour: {end:?}");
    // A revision brings the accent back.
    s.apply(accent_view(&words(&[(1, "settled")])), 2.0);
    assert!(c(&s, 2.0).g < 0.01);
}

/// Words as an app with a line cap shows them: the newest that fit in `lines` lines, the
/// first marked with "…" when words were dropped.
fn capped(all: &[(u32, &str)], width: f32, lines: usize) -> Vec<Word> {
    let mut start = 0;
    loop {
        let mut shown = words(&all[start..]);
        if start > 0 {
            shown[0].text = format!("…{}", shown[0].text);
        }
        let mut probe = scene();
        probe.apply(view(&shown, Some(width), false), 0.0);
        let list = probe.sample(0.0);
        let mut ys: Vec<f32> = list.glyphs.iter().map(|g| g.pos.y).collect();
        ys.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        if ys.len() <= lines || start + 1 == all.len() {
            return shown;
        }
        start += 1;
    }
}

/// Every glyph that shows sits on a text row 26 apart from `base`. The text may scroll as a
/// whole, so all rows may be off by the same amount, but no word travels up or down on its
/// own, across the words between.
fn on_rows(list: &DisplayList, base: f32, what: &str) {
    let rows: Vec<f32> =
        list.glyphs.iter().filter(|g| g.color.a > 0.1).map(|g| (g.pos.y - base).rem_euclid(26.0)).collect();
    let r0 = rows[0];
    let off = |r: f32| ((r - r0).rem_euclid(26.0)).min((r0 - r).rem_euclid(26.0));
    let spread = rows.iter().map(|&r| off(r)).fold(0.0, f32::max);
    assert!(spread < 2.0, "{what}: a glyph is {spread} px off the rows");
}

#[test]
fn with_a_line_cap_no_word_glides_across_the_other_lines() {
    let text = "open the configuration file and set the popup theme to swiss so the dictation \
                window shows the live words in the middle of the screen with the waveform above \
                them then press the key again to stop";
    let all: Vec<(u32, &str)> = text.split(' ').enumerate().map(|(i, w)| (i as u32 + 1, w)).collect();
    let mut s = scene();
    // Words arrive every 120 ms; once the text fills three lines each one drops a front word
    // and the rest reflow.
    for n in 1..=all.len() {
        let t = n as f64 * 0.12;
        let shown = capped(&all[..n], 260.0, 3);
        s.apply(view(&shown, Some(260.0), false), t);
        let mut fresh = scene();
        fresh.apply(view(&shown, Some(260.0), false), 0.0);
        let target = fresh.sample(0.0);
        for dt in [0.02, 0.05, 0.08, 0.11] {
            let now = s.sample(t + dt);
            on_rows(&now, first(&target, shown[0].id).1, &format!("{n} words at +{dt}"));
        }
    }
}

#[test]
fn words_pushed_to_another_line_by_a_revision_stay_on_the_rows() {
    // The second word grows and shrinks every 120 ms, pushing the words after it back and
    // forth across line ends.
    let mut s = scene();
    let rest = [(3, "the"), (4, "popup"), (5, "theme"), (6, "to"), (7, "swiss"), (8, "so"), (9, "the"), (10, "words")];
    for (n, second) in ["set", "settings", "set", "settle", "settled", "set"].iter().enumerate() {
        let mut all = vec![(1, "and"), (2, *second)];
        all.extend(rest);
        let t = 0.12 * n as f64;
        s.apply(view(&words(&all), Some(200.0), false), t);
        let base = first(&s.sample(5.0 + t), 1).1;
        for dt in [0.02, 0.05, 0.08, 0.11] {
            on_rows(&s.sample(t + dt), base, &format!("{second} at +{dt}"));
        }
    }
}

/// Per row, the span of each word's visible glyph pens, `(row, word, left, right)`.
fn spans(list: &DisplayList, base: f32) -> Vec<(i32, u32, f32, f32)> {
    let mut out: Vec<(i32, u32, f32, f32)> = Vec::new();
    for g in list.glyphs.iter().filter(|g| g.color.a > 0.05) {
        let row = ((g.pos.y - base) / 26.0).round() as i32;
        match out.iter_mut().find(|s| s.0 == row && s.1 == g.word) {
            Some(s) => (s.2, s.3) = (s.2.min(g.pos.x), s.3.max(g.pos.x)),
            None => out.push((row, g.word, g.pos.x, g.pos.x)),
        }
    }
    out
}

#[test]
fn a_rewrapped_word_waits_for_the_words_sliding_through_its_place() {
    // "popup" is pushed to the front of the second line; the words there slide right from
    // where it lands. It shows once they have left its place, never under or over them, and
    // its ghost leaves the first line before "the" slides into it.
    let mut s = scene();
    let before = [(1, "and"), (2, "set"), (3, "the"), (4, "popup"), (5, "theme"), (6, "to")];
    s.apply(view(&words(&before), Some(200.0), false), 0.0);
    let base = first(&s.sample(1.0), 1).1;
    assert_eq!(first(&s.sample(1.0), 4).1, base, "popup starts on the first line");
    let mut after = before;
    after[1] = (2, "settings");
    s.apply(view(&words(&after), Some(200.0), false), 1.0);
    let settled = s.sample(5.0);
    assert!((first(&settled, 4).1 - (base + 26.0)).abs() < 1e-3, "popup ends on the second line");
    assert!(settled.glyphs.iter().all(|g| g.pos.y < base + 39.0), "the second line is the newest");
    let mut shown = None;
    for k in 0..=40 {
        let t = 1.0 + k as f64 / 120.0;
        let list = s.sample(t);
        let sp = spans(&list, base);
        // Its ghost on the first line is gone before the words sliding along reach it.
        if let Some(&(_, _, a, b)) = sp.iter().find(|w| w.0 == 0 && w.1 == 4) {
            for w in sp.iter().filter(|w| w.0 == 0 && w.1 != 4) {
                assert!(w.3 < a || w.2 > b, "word {} over popup's ghost at +{:.3}", w.1, t - 1.0);
            }
        }
        let popup = sp.iter().find(|w| w.0 == 1 && w.1 == 4);
        if let Some(&(_, _, a, b)) = popup {
            // Pens of other words on its row stay out of its span.
            for w in sp.iter().filter(|w| w.0 == 1 && w.1 != 4) {
                assert!(w.3 < a || w.2 > b, "word {} overlaps popup at +{:.3}: {w:?} vs {a}..{b}", w.1, t - 1.0);
            }
            if shown.is_none() {
                // The newest line: it shows whole, readable from its first frame.
                let on_row = |g: &&GlyphInstance| g.word == 4 && (g.pos.y - base - 26.0).abs() < 13.0;
                let ink = list.glyphs.iter().filter(on_row).map(|g| g.color.a).fold(1.0, f32::min);
                assert!(ink >= 0.7, "popup arrives at {ink} ink");
                shown = Some(t);
            }
        }
    }
    assert!(shown.is_some_and(|t| t > 1.0), "popup waits, then shows: {shown:?}");
}

#[test]
fn under_reduced_motion_a_word_rewrapped_onto_the_newest_line_arrives_readable() {
    // As above with reduced motion: nothing slides, so "popup" does not wait; it fades in
    // at its new place from reading ink, not from clear.
    let mut s = scene().with_reduced_motion(true);
    let before = [(1, "and"), (2, "set"), (3, "the"), (4, "popup"), (5, "theme"), (6, "to")];
    s.apply(view(&words(&before), Some(200.0), false), 0.0);
    let base = first(&s.sample(1.0), 1).1;
    let mut after = before;
    after[1] = (2, "settings");
    s.apply(view(&words(&after), Some(200.0), false), 1.0);
    let list = s.sample(1.0 + 1.0 / 120.0);
    let on_row = |g: &&GlyphInstance| g.word == 4 && (g.pos.y - base - 26.0).abs() < 13.0;
    let ink = list.glyphs.iter().filter(on_row).map(|g| g.color.a).fold(1.0, f32::min);
    assert!(list.glyphs.iter().any(|g| on_row(&g)), "popup shows on the second line at once");
    assert!(ink >= 0.7, "popup arrives at {ink} ink");
}
