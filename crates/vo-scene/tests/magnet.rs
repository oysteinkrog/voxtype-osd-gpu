//! Magnetic caret (MO-89): words snap to the caret along their line, and the caret draws a
//! field that follows the voice and relaxes into rings in silence.

use vo_core::signals::{SignalFrame, Signals, SignalsCell};
use vo_core::words::Word;
use vo_scene::display::{DisplayList, GlyphInstance};
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::magnet::{Magnet, AWAKE_SECS};
use vo_scene::flow::Caret;
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

fn view(w: &[Word], magnet: bool) -> Element<()> {
    let mut live =
        Element::live_text("live", w).text_style(TextStyle::new(0, SIZE, 26.0)).width(400.0).caret(Caret::default());
    if magnet {
        live = live.magnetic_caret(Magnet::default());
    }
    Element::column("root").child(live)
}

fn of(list: &DisplayList, word: u32) -> Vec<GlyphInstance> {
    list.glyphs.iter().filter(|g| g.word == word).copied().collect()
}

#[test]
fn a_new_word_snaps_in_as_one_piece_on_its_baseline() {
    let mut plain = scene();
    plain.apply(view(&words(&[(1, "the"), (2, "there")]), false), 1.0);
    let rest = of(&plain.sample(5.0), 2);

    let mut s = scene();
    s.apply(view(&words(&[(1, "the")]), true), 0.0);
    s.apply(view(&words(&[(1, "the"), (2, "there")]), true), 1.0);
    let m = Magnet::default();
    for f in 0..300 {
        let t = 1.0 + f as f64 * 0.001;
        let g = of(&s.sample(t), 2);
        let shift = g[0].pos.x - rest[0].pos.x;
        for (a, b) in g.iter().zip(&rest) {
            assert_eq!(a.pos.y, b.pos.y, "on its baseline");
            assert!((a.pos.x - b.pos.x - shift).abs() < 1e-3, "spacing unchanged");
        }
        if f == 0 {
            assert!((shift - m.snap).abs() < 1e-3, "starts {shift} right");
        }
    }
    let done = of(&s.sample(1.0 + m.snap_secs + 0.1), 2);
    assert_eq!(done.iter().map(|g| g.pos).collect::<Vec<_>>(), rest.iter().map(|g| g.pos).collect::<Vec<_>>());
}

#[test]
fn the_field_draws_follows_the_voice_and_goes_still() {
    let mut voice = Signals::new();
    let mut c = 0.0;
    while c < 2.0 {
        voice.push(SignalFrame { rms_dbfs: -10.0, ..SignalFrame::silent(c) }, c);
        c += 0.01;
    }
    let mut s = scene();
    s.set_signals(SignalsCell::new(voice));
    s.apply(view(&words(&[(1, "there")]), true), 0.0);
    let loud = s.sample(1.5).shapes.len();
    assert!(loud > 40, "field lines draw: {loud}");
    assert_eq!(s.next_wake(1.5), Some(1.5), "the field moves while the voice runs");
    // Long after: rings, still.
    let quiet = s.sample(10.0);
    assert!(quiet.shapes.len() > 40);
    assert!(s.next_wake(10.0).is_none() || s.next_wake(10.0).unwrap() > 10.0, "still in silence");

    let mut plain = scene();
    plain.apply(view(&words(&[(1, "there")]), false), 0.0);
    assert!(plain.sample(1.5).shapes.len() < 5, "no field without the magnet");
}

#[test]
fn without_voice_features_a_word_holds_the_field_then_it_relaxes() {
    let mut s = scene();
    s.apply(view(&words(&[(1, "there")]), true), 0.0);
    assert_eq!(s.next_wake(0.5), Some(0.5));
    let after = AWAKE_SECS + 0.5;
    assert!(s.next_wake(after).is_none() || s.next_wake(after).unwrap() > after);
}

#[test]
fn reduced_motion_lands_words_in_place_and_draws_still_rings() {
    let mut plain = scene();
    plain.apply(view(&words(&[(1, "the"), (2, "there")]), false), 0.0);
    let mut s = scene().with_reduced_motion(true);
    s.apply(view(&words(&[(1, "the"), (2, "there")]), true), 0.0);
    let a: Vec<_> = of(&s.sample(0.01), 2).iter().map(|g| g.pos).collect();
    let b: Vec<_> = of(&plain.sample(0.01), 2).iter().map(|g| g.pos).collect();
    assert_eq!(a, b);
    assert!(s.sample(0.01).shapes.len() > 40, "the rings stay as the static mark");
}
