//! Around a revision (MO-21 to MO-23): width escrow holds the neighbours through a burst,
//! a hinge brackets moved word breaks, and margin marks point from an older changed line
//! to the newest one.

use vo_core::words::Word;
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::revisions::{
    RevisionMotion, ADDRESS_IN, ADDRESS_MEET, ESCROW_QUIET, HINGE_IN, HINGE_MOVE, HINGE_OUT,
};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const SIZE: f32 = 20.0;

fn scene() -> Scene<()> {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    Scene::new().with_text(Box::new(ShapedLayout::new(fonts)))
}

fn words(list: &[(u32, &str)]) -> Vec<Word> {
    list.iter().map(|&(id, text)| Word { id, text: text.into() }).collect()
}

fn view(w: &[(u32, &str)], width: f32, r: RevisionMotion) -> Element<()> {
    Element::column("root").child(
        Element::live_text("live", &words(w))
            .text_style(TextStyle::new(0, SIZE, 26.0))
            .revision_motion(r)
            .width(width)
            .margin_edges(0.0, 0.0, 0.0, 20.0),
    )
}

fn x_of(list: &DisplayList, word: u32) -> f32 {
    list.glyphs.iter().filter(|g| g.word == word && g.color.a > 0.99).map(|g| g.pos.x).fold(f32::INFINITY, f32::min)
}

const ESCROW: RevisionMotion = RevisionMotion { escrow: true, hinge: false, address: false };

#[test]
fn escrow_holds_the_neighbours_until_the_burst_is_quiet() {
    let mut s = scene();
    s.apply(view(&[(1, "meet"), (2, "there"), (3, "team")], 400.0, ESCROW), 0.0);
    let before = x_of(&s.sample(1.0), 3);
    // A burst: three changes 60 ms apart.
    s.apply(view(&[(1, "meet"), (2, "their"), (3, "team")], 400.0, ESCROW), 1.0);
    s.apply(view(&[(1, "meet"), (2, "theirs"), (3, "team")], 400.0, ESCROW), 1.06);
    s.apply(view(&[(1, "meet"), (2, "their"), (3, "team")], 400.0, ESCROW), 1.12);
    for t in [1.0, 1.05, 1.1, 1.15, 1.12 + ESCROW_QUIET - 0.005] {
        assert_eq!(x_of(&s.sample(t), 3), before, "held at {t}");
    }
    // Then one move to where a fresh layout puts it.
    let mut fresh = scene();
    fresh.apply(view(&[(1, "meet"), (2, "their"), (3, "team")], 400.0, ESCROW), 0.0);
    let after = s.sample(2.0);
    assert!((x_of(&after, 3) - x_of(&fresh.sample(0.0), 3)).abs() < 0.01);
    assert!(x_of(&s.sample(1.12 + ESCROW_QUIET + 0.08), 3) != before, "moving after the quiet");
}

#[test]
fn without_escrow_or_past_half_an_em_neighbours_move_at_once() {
    let mut s = scene();
    s.apply(view(&[(1, "meet"), (2, "there"), (3, "team")], 400.0, ESCROW), 0.0);
    let before = x_of(&s.sample(1.0), 3);
    // Far more than half an em wider: no escrow.
    s.apply(view(&[(1, "meet"), (2, "therefore"), (3, "team")], 400.0, ESCROW), 1.0);
    assert!(x_of(&s.sample(1.05), 3) > before + 1.0);
    let mut s = scene();
    s.apply(view(&[(1, "meet"), (2, "there"), (3, "team")], 400.0, RevisionMotion::default()), 0.0);
    s.apply(view(&[(1, "meet"), (2, "theirs"), (3, "team")], 400.0, RevisionMotion::default()), 1.0);
    assert!(x_of(&s.sample(1.05), 3) > before + 0.5);
}

const HINGE: RevisionMotion = RevisionMotion { escrow: false, hinge: true, address: false };

#[test]
fn a_moved_word_break_draws_a_hinge_that_retracts() {
    let mut s = scene();
    s.apply(view(&[(1, "my"), (2, "ice"), (3, "cream")], 400.0, HINGE), 0.0);
    assert!(s.sample(0.5).shapes.is_empty());
    s.apply(view(&[(1, "my"), (4, "I"), (3, "scream")], 400.0, HINGE), 1.0);
    // Bracket line, two end ticks and one joint while it is open.
    let open = s.sample(1.0 + HINGE_IN + 0.5 * HINGE_MOVE);
    assert_eq!(open.shapes.len(), 4, "{:?}", open.shapes.len());
    assert!(s.next_wake(1.0 + HINGE_IN).is_some());
    let end = 1.0 + HINGE_IN + HINGE_MOVE + HINGE_OUT;
    assert!(s.sample(end + 0.01).shapes.is_empty());
    // A plain revision draws none.
    s.apply(view(&[(1, "my"), (4, "I"), (3, "screams")], 400.0, HINGE), 2.0);
    assert!(s.sample(2.05).shapes.is_empty());
}

const ADDRESS: RevisionMotion = RevisionMotion { escrow: false, hinge: false, address: true };

#[test]
fn a_change_on_an_older_line_lights_paired_marks() {
    let text: &[(u32, &str)] = &[(1, "meet"), (2, "there"), (3, "team"), (4, "at"), (5, "noon"), (6, "today")];
    let mut s = scene();
    s.apply(view(text, 130.0, ADDRESS), 0.0);
    let lines = {
        let mut ys: Vec<i32> = s.sample(0.5).glyphs.iter().map(|g| g.pos.y.round() as i32).collect();
        ys.sort();
        ys.dedup();
        ys.len()
    };
    assert!(lines >= 2, "the text wraps: {lines}");
    let mut changed = text.to_vec();
    changed[1] = (2, "their");
    s.apply(view(&changed, 130.0, ADDRESS), 1.0);
    let lit = s.sample(1.0 + ADDRESS_IN);
    assert_eq!(lit.shapes.len(), 2, "one mark beside each line");
    assert!(s.sample(1.0 + ADDRESS_IN + ADDRESS_MEET + 0.01).shapes.is_empty());
    // A change on the newest line needs no address.
    let mut newest = changed.clone();
    newest[5] = (6, "todays");
    s.apply(view(&newest, 130.0, ADDRESS), 2.0);
    assert!(s.sample(2.0 + ADDRESS_IN).shapes.is_empty());
}

#[test]
fn reduced_motion_still_shows_the_marks() {
    let mut s = scene().with_reduced_motion(true);
    s.apply(view(&[(1, "my"), (2, "ice"), (3, "cream")], 400.0, HINGE), 0.0);
    s.apply(view(&[(1, "my"), (4, "I"), (3, "scream")], 400.0, HINGE), 1.0);
    assert_eq!(s.sample(1.0 + 0.01).shapes.len(), 4, "the hinge at its new breaks at once");
}

// ---- EG-32: liquid revision ----------------------------------------------------------------

mod liquid {
    use super::*;
    use vo_scene::flow::liquid::{painter_on_this_thread, LiquidPart, LiquidRevision, LIQUID_SECS};
    use vo_scene::flow::Revise;

    const LINE: &[(u32, &str)] = &[(1, "my"), (2, "scream"), (3, "team")];
    const WRAPPED: &[(u32, &str)] = &[(1, "my"), (2, "scream"), (3, "team"), (4, "at"), (5, "noon"), (6, "today")];

    fn after<'a>(text: &[(u32, &'a str)]) -> Vec<(u32, &'a str)> {
        text.iter().map(|&(id, s)| (id, if id == 2 { "cream" } else { s })).collect()
    }

    fn view(w: &[(u32, &str)], width: f32, revise: Revise) -> Element<()> {
        Element::column("root").child(
            Element::live_text("live", &words(w))
                .text_style(TextStyle::new(0, SIZE, 26.0))
                .word_revise(revise)
                .width(width)
                .margin_edges(0.0, 0.0, 0.0, 20.0),
        )
    }

    /// A scene revised at t = 1 from `before` to `after`, with or without the painter.
    fn revised(before: &[(u32, &str)], width: f32, revise: Revise, painter: bool, reduced: bool) -> Scene<()> {
        painter_on_this_thread(Some(painter));
        let mut s = scene().with_reduced_motion(reduced);
        s.apply(view(before, width, revise), 0.0);
        s.sample(0.5);
        s.apply(view(&after(before), width, revise), 1.0);
        s
    }

    fn liquids(list: &DisplayList) -> Vec<&LiquidRevision> {
        list.gpu.iter().filter_map(|g| g.source.0.as_any().downcast_ref::<LiquidRevision>()).collect()
    }

    fn frames() -> impl Iterator<Item = f64> {
        (0..60).map(|i| 1.0 + i as f64 / 120.0)
    }

    #[test]
    fn without_a_painter_it_is_the_palimpsest() {
        for (text, width) in [(LINE, 400.0), (WRAPPED, 130.0)] {
            for reduced in [false, true] {
                let a = revised(text, width, Revise::Liquid, false, reduced);
                let b = revised(text, width, Revise::Palimpsest, false, reduced);
                for t in frames() {
                    assert_eq!(a.sample(t), b.sample(t), "at {t}");
                }
            }
        }
        painter_on_this_thread(None);
    }

    #[test]
    fn on_the_reading_line_the_text_keeps_the_word_and_the_liquid_drains_the_old_letters() {
        let s = revised(LINE, 400.0, Revise::Liquid, true, false);
        let p = revised(LINE, 400.0, Revise::Palimpsest, true, false);
        painter_on_this_thread(None);
        for t in frames() {
            let (list, pal) = (s.sample(t), p.sample(t));
            let ours: Vec<_> = list.glyphs.iter().filter(|g| g.word == 2).collect();
            // Every letter of the new word, at reading ink, from the first frame.
            assert_eq!(ours.len(), 5, "at {t}");
            for g in &ours {
                assert!(g.color.a >= 0.7, "ink {} at {t}", g.color.a);
            }
            // Kept letters ride the same springs as the palimpsest's survivors: no jump.
            let pals: Vec<_> = pal.glyphs.iter().filter(|g| g.word == 2).take(5).collect();
            for (a, b) in ours.iter().zip(&pals) {
                assert!((a.pos.x - b.pos.x).abs() < 1e-3 && (a.pos.y - b.pos.y).abs() < 1e-3, "at {t}");
            }
            let l = liquids(&list);
            if t - 1.0 < LIQUID_SECS {
                assert_eq!(l.len(), 1, "a liquid item at {t}");
                assert!(l[0].reading);
                assert_eq!(l[0].glyphs.iter().filter(|g| g.part == LiquidPart::Old).count(), 1, "the s drains");
                assert_eq!(l[0].glyphs.iter().filter(|g| g.part == LiquidPart::Kept).count(), 5);
            } else {
                assert!(l.is_empty(), "the liquid ends within {LIQUID_SECS} s, at {t}");
            }
        }
        const { assert!(LIQUID_SECS <= 0.4) };
        // The scene stays awake for the liquid.
        assert!(s.next_wake(1.0).is_some_and(|w| w <= 1.0 + LIQUID_SECS));
    }

    #[test]
    fn on_an_older_line_the_liquid_draws_the_whole_word_and_kept_letters_slide() {
        let s = revised(WRAPPED, 130.0, Revise::Liquid, true, false);
        let p = revised(WRAPPED, 130.0, Revise::Palimpsest, true, false);
        painter_on_this_thread(None);
        let mut last: Option<Vec<f32>> = None;
        for t in frames().filter(|t| t - 1.0 < LIQUID_SECS) {
            let (list, pal) = (s.sample(t), p.sample(t));
            assert!(list.glyphs.iter().all(|g| g.word != 2), "the text hides the word at {t}");
            let l = liquids(&list);
            assert_eq!(l.len(), 1, "at {t}");
            assert!(!l[0].reading);
            let item = list.gpu.iter().find(|g| g.source.0.as_any().is::<LiquidRevision>()).unwrap();
            let kept: Vec<f32> =
                l[0].glyphs.iter().filter(|g| g.part == LiquidPart::Kept).map(|g| item.rect.x + g.pos.x).collect();
            let pals: Vec<f32> = pal.glyphs.iter().filter(|g| g.word == 2).take(5).map(|g| g.pos.x).collect();
            assert_eq!(kept.len(), 5);
            for (a, b) in kept.iter().zip(&pals) {
                assert!((a - b).abs() < 1e-3, "kept letter off its spring at {t}: {a} {b}");
            }
            // Per frame each kept letter moves at most a small step toward its place.
            if let Some(prev) = &last {
                for (a, b) in kept.iter().zip(prev) {
                    assert!((a - b).abs() < 0.25 * SIZE, "jump at {t}: {b} to {a}");
                }
            }
            last = Some(kept);
        }
        // Then the text has the word back.
        let done = s.sample(1.0 + LIQUID_SECS + 0.01);
        assert!(liquids(&done).is_empty());
        assert_eq!(done.glyphs.iter().filter(|g| g.word == 2).count(), 5);
    }

    #[test]
    fn reduced_motion_shows_the_corrected_word_at_once() {
        let s = revised(LINE, 400.0, Revise::Liquid, true, true);
        let swap = revised(LINE, 400.0, Revise::Swap, true, true);
        painter_on_this_thread(None);
        for t in frames() {
            let (a, b) = (s.sample(t), swap.sample(t));
            assert!(liquids(&a).is_empty());
            assert_eq!(a, b, "at {t}");
        }
    }
}
