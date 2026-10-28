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
