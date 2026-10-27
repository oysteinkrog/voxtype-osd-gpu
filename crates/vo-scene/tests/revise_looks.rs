//! Revision looks for live words: a roll swap (FX-065) and a displaced ghost that floats
//! above its replacement while the line opens (FX-064).

use vo_core::words::Word;
use vo_scene::display::{DisplayList, GlyphInstance};
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::{Revise, GHOST_ALPHA, GHOST_HOLD, GHOST_LIFT_EMS, GHOST_OPEN_EMS, GHOST_SCALE, ROLL_WORD_EMS};
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

fn view(w: &[Word], revise: Revise) -> Element<()> {
    Element::column("root")
        .child(Element::live_text("live", w).text_style(TextStyle::new(0, SIZE, 26.0)).word_revise(revise).width(400.0))
}

fn of(list: &DisplayList, word: u32) -> Vec<&GlyphInstance> {
    list.glyphs.iter().filter(|g| g.word == word).collect()
}

const BEFORE: &[(u32, &str)] = &[(1, "meet"), (2, "their"), (3, "team")];
const AFTER: &[(u32, &str)] = &[(1, "meet"), (2, "there"), (3, "team")];

#[test]
fn roll_swap_sends_the_old_word_up_and_brings_the_new_one_from_below() {
    let mut s = scene();
    s.apply(view(&words(BEFORE), Revise::Roll), 0.0);
    let base = of(&s.sample(1.0), 2)[0].pos.y;
    s.apply(view(&words(AFTER), Revise::Roll), 1.0);
    let mid = s.sample(1.06);
    let glyphs = of(&mid, 2);
    // Old and new glyphs both show: "their" (5) and "there" (5).
    assert_eq!(glyphs.len(), 10);
    assert!(glyphs.iter().any(|g| g.pos.y < base - 1.0 && g.color.a < 0.99), "an old glyph on its way up");
    assert!(glyphs.iter().any(|g| g.pos.y > base + 1.0 && g.color.a < 0.99), "a new glyph on its way in");
    assert!(glyphs.iter().all(|g| (g.pos.y - base).abs() <= ROLL_WORD_EMS * SIZE + 0.5));
    // Settled: just the new word, where a fresh layout puts it.
    let settled = s.sample(3.0);
    let mut fresh = scene();
    fresh.apply(view(&words(AFTER), Revise::Roll), 0.0);
    assert_eq!(fresh.sample(0.0).glyphs, settled.glyphs);
}

#[test]
fn displaced_ghost_floats_above_its_replacement_while_the_line_opens() {
    let mut s = scene();
    s.apply(view(&words(BEFORE), Revise::Ghost), 0.0);
    let before = s.sample(1.0);
    let base = of(&before, 1)[0].pos.y;
    s.apply(view(&words(AFTER), Revise::Ghost), 1.0);
    let held = s.sample(1.0 + 0.6 * GHOST_HOLD);
    let small: Vec<_> = of(&held, 2).into_iter().filter(|g| g.size < SIZE - 1.0).collect();
    let full: Vec<_> = of(&held, 2).into_iter().filter(|g| g.size >= SIZE - 1e-3).collect();
    assert_eq!(small.len(), 5, "the old word's glyphs, shrunk");
    for g in &small {
        assert!((g.size - GHOST_SCALE * SIZE).abs() < 0.05, "{}", g.size);
        assert!((g.color.a - GHOST_ALPHA).abs() < 0.02, "{}", g.color.a);
    }
    // The line opened: every word on it sits lower by the opening.
    let open = GHOST_OPEN_EMS * SIZE;
    let line = of(&held, 1)[0].pos.y;
    assert!((line - base - open).abs() < 0.1, "{line} {base}");
    // The ghost floats above the new word's baseline and is centred over it.
    let ghost_y = small[0].pos.y;
    assert!((ghost_y - (line - GHOST_LIFT_EMS * SIZE)).abs() < 0.5, "{ghost_y} {line}");
    // Pen positions leave out the last glyph's advance, so allow a few units.
    let mid = |gs: &[&GlyphInstance]| {
        let l = gs.iter().map(|g| g.pos.x).fold(f32::INFINITY, f32::min);
        let r = gs.iter().map(|g| g.pos.x).fold(f32::NEG_INFINITY, f32::max);
        0.5 * (l + r)
    };
    assert!((mid(&small) - mid(&full)).abs() < 4.0, "{} {}", mid(&small), mid(&full));
    // After the hold the ghost fades out and the line closes again.
    let after = s.sample(1.0 + GHOST_HOLD + 1.0);
    assert_eq!(of(&after, 2).len(), 5);
    assert!((of(&after, 1)[0].pos.y - base).abs() < 0.05);
    assert!(s.next_wake(1.0 + GHOST_HOLD + 1.0).is_none());
}
