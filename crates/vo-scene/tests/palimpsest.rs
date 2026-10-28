//! Palimpsest correction (MO-20), the default revision: survivors stay, removed letters
//! sink into a short ghost, new letters arrive in place.

use vo_core::words::Word;
use vo_scene::display::{DisplayList, GlyphInstance};
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::palimpsest::{ARRIVE_INK, GHOST_HOLD, GHOST_INK, SINK_PX};
use vo_scene::flow::Revise;
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

/// Live text with no `word_revise`: the default revision.
fn view(w: &[Word]) -> Element<()> {
    Element::column("root").child(Element::live_text("live", w).text_style(TextStyle::new(0, SIZE, 26.0)).width(400.0))
}

fn of(list: &DisplayList, word: u32) -> Vec<GlyphInstance> {
    list.glyphs.iter().filter(|g| g.word == word).copied().collect()
}

const BEFORE: &[(u32, &str)] = &[(1, "meet"), (2, "there"), (3, "team")];
const AFTER: &[(u32, &str)] = &[(1, "meet"), (2, "their"), (3, "team")];

#[test]
fn palimpsest_is_the_default() {
    assert_eq!(Revise::default(), Revise::Palimpsest);
}

#[test]
fn survivors_stay_removed_letters_sink_and_new_ones_arrive_in_place() {
    let mut s = scene();
    s.apply(view(&words(BEFORE)), 0.0);
    let before = of(&s.sample(1.0), 2);
    assert_eq!(before.len(), 5);
    let base = before[0].pos.y;
    s.apply(view(&words(AFTER)), 1.0);

    // The first frame: "the" is exactly where it was, at full ink; "ir" is already readable.
    let first = of(&s.sample(1.0), 2);
    for old in &before[..3] {
        let g = first.iter().find(|g| g.glyph == old.glyph && (g.pos.x - old.pos.x).abs() < 1e-3).expect("survivor");
        assert_eq!(g.pos, old.pos);
        assert!(g.color.a > 0.999, "survivor ink {}", g.color.a);
    }
    let fresh: Vec<_> = first.iter().filter(|g| !before.iter().any(|b| b.glyph == g.glyph && b.pos == g.pos)).collect();
    assert!(fresh.iter().all(|g| (g.pos.y - base).abs() < 1e-3), "new letters arrive on the baseline");
    assert!(fresh.iter().filter(|g| g.color.a >= ARRIVE_INK - 1e-3).count() >= 2, "new letters readable at once");

    // 300 ms on: the ghost of "re" sits 4 px lower at 20 %; survivors never moved.
    let held = of(&s.sample(1.3), 2);
    let ghosts: Vec<_> = held.iter().filter(|g| g.color.a < 0.5).collect();
    assert_eq!(ghosts.len(), 2, "the ghost of \"re\"");
    for g in &ghosts {
        assert!((g.pos.y - base - SINK_PX).abs() < 0.05, "{} {}", g.pos.y, base);
        assert!((g.color.a - GHOST_INK).abs() < 0.02, "{}", g.color.a);
    }
    for old in &before[..3] {
        assert!(held.iter().any(|g| g.pos == old.pos && g.color.a > 0.999));
    }
    assert!(held.iter().filter(|g| g.color.a > 0.999).count() == 5, "\"their\" at full ink");

    // After the hold the ghost is gone and the word is what a fresh layout draws.
    let settled = s.sample(1.0 + GHOST_HOLD + 1.0);
    let mut fresh = scene();
    fresh.apply(view(&words(AFTER)), 0.0);
    assert_eq!(fresh.sample(0.0).glyphs, settled.glyphs);
    assert!(s.next_wake(1.0 + GHOST_HOLD + 1.0).is_none());
}

#[test]
fn reduced_motion_keeps_the_ghost_on_the_baseline() {
    let mut s = scene().with_reduced_motion(true);
    s.apply(view(&words(BEFORE)), 0.0);
    let base = of(&s.sample(1.0), 2)[0].pos.y;
    s.apply(view(&words(AFTER)), 1.0);
    let held = of(&s.sample(1.3), 2);
    let ghosts: Vec<_> = held.iter().filter(|g| g.color.a < 0.5).collect();
    assert_eq!(ghosts.len(), 2);
    assert!(ghosts.iter().all(|g| (g.pos.y - base).abs() < 1e-3 && (g.color.a - GHOST_INK).abs() < 0.02));
}

#[test]
fn the_reading_line_keeps_its_envelope() {
    // Every frame of a revision burst: no glyph leaves its baseline by more than the
    // 4 px ghost sink, nothing scales, and only fading ghosts leave the baseline.
    let mut s = scene();
    let steps: &[&[(u32, &str)]] = &[
        &[(1, "I"), (2, "scream")],
        &[(1, "ice"), (2, "cream")],
        &[(1, "ice"), (2, "creams")],
        &[(1, "Ice"), (2, "cream,")],
    ];
    s.apply(view(&words(steps[0])), 0.0);
    let base = s.sample(0.5).glyphs[0].pos.y;
    for (i, st) in steps.iter().enumerate().skip(1) {
        let t0 = i as f64 * 0.15;
        s.apply(view(&words(st)), t0);
        for f in 0..9 {
            let list = s.sample(t0 + f as f64 / 60.0);
            for g in &list.glyphs {
                assert!((g.size - SIZE).abs() < 1e-3, "no scale on the reading line");
                let dy = g.pos.y - base;
                assert!((-0.5..=SINK_PX + 0.5).contains(&dy), "travel {dy} step {i} frame {f}");
                // Only a ghost on its way out leaves the baseline, and it is already fading.
                if dy > 0.5 {
                    assert!(g.color.a < 0.9, "a sinking glyph fades: {dy} {}", g.color.a);
                }
            }
        }
    }
}
