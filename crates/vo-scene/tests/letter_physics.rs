//! Letter physics (MO-83): new letters drop into the line, bounce, nudge their neighbours
//! and lock into place, within the reading-line envelope on the newest line.

use vo_core::words::Word;
use vo_scene::display::{DisplayList, GlyphInstance};
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::letter_physics::{LetterPhysics, READING_DROP, READING_SIDE};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::Color;
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

fn view(w: &[Word], p: Option<LetterPhysics>) -> Element<()> {
    let mut live = Element::live_text("live", w).text_style(TextStyle::new(0, SIZE, 26.0)).width(400.0);
    if let Some(p) = p {
        live = live.letter_physics(p);
    }
    Element::column("root").child(live)
}

fn of(list: &DisplayList, word: u32) -> Vec<GlyphInstance> {
    list.glyphs.iter().filter(|g| g.word == word).copied().collect()
}

#[test]
fn letters_drop_bounce_and_lock_within_the_envelope() {
    let physics = LetterPhysics { drop: 30.0, ..LetterPhysics::default() }.vectors(Color::WHITE);
    let mut plain = scene();
    plain.apply(view(&words(&[(1, "hello"), (2, "world")]), None), 1.0);
    let rest = of(&plain.sample(5.0), 2);

    let mut s = scene();
    s.apply(view(&words(&[(1, "hello")]), Some(physics)), 0.0);
    s.apply(view(&words(&[(1, "hello"), (2, "world")]), Some(physics)), 1.0);

    // The first frame: the first letter of "world" is a full reading-line drop up, and
    // every letter is visible where it will land (no scale).
    let first = of(&s.sample(1.0), 2);
    assert_eq!(first.len(), rest.len());
    assert!((first[0].pos.y - (rest[0].pos.y - READING_DROP)).abs() < 1e-3, "{:?}", first[0].pos);

    let mut moved = false;
    let mut vectors = false;
    for f in 0..600 {
        let t = 1.0 + f as f64 * 0.001;
        let list = s.sample(t);
        vectors |= !list.shapes.is_empty();
        for (g, r) in of(&list, 2).iter().zip(&rest) {
            let (dx, dy) = (g.pos.x - r.pos.x, g.pos.y - r.pos.y);
            assert!((-READING_DROP - 1e-3..=1e-3).contains(&dy), "baseline shift {dy} at {t}");
            assert!(dx.abs() <= READING_SIDE * SIZE + 1e-3, "side {dx} at {t}");
            assert_eq!(g.size, r.size, "no scale");
            moved |= dy.abs() > 0.5;
        }
    }
    assert!(moved, "the letters fall");
    assert!(vectors, "motion vectors draw while letters fly");

    // Locked: exactly what the plain layout draws, and nothing keeps the scene awake.
    let locked = of(&s.sample(1.0 + physics.lock + 0.016 * 5.0 + 0.5), 2);
    for (g, r) in locked.iter().zip(&rest) {
        assert_eq!(g.pos, r.pos);
    }
    assert!(s.next_wake(3.0).is_none());
}

#[test]
fn kept_words_do_not_fall_again() {
    let p = LetterPhysics::default();
    let mut s = scene();
    s.apply(view(&words(&[(1, "hello")]), Some(p)), 0.0);
    let settled = of(&s.sample(2.0), 1);
    s.apply(view(&words(&[(1, "hello"), (2, "there")]), Some(p)), 2.0);
    assert_eq!(
        of(&s.sample(2.0), 1).iter().map(|g| g.pos).collect::<Vec<_>>(),
        settled.iter().map(|g| g.pos).collect::<Vec<_>>()
    );
}

#[test]
fn reduced_motion_lands_letters_in_place() {
    let p = LetterPhysics::default();
    let mut plain = scene();
    plain.apply(view(&words(&[(1, "hello")]), None), 0.0);
    let mut s = scene().with_reduced_motion(true);
    s.apply(view(&words(&[(1, "hello")]), Some(p)), 0.0);
    let a: Vec<_> = of(&s.sample(0.02), 1).iter().map(|g| g.pos).collect();
    let b: Vec<_> = of(&plain.sample(0.02), 1).iter().map(|g| g.pos).collect();
    assert_eq!(a, b);
}
