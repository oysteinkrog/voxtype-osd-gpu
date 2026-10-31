//! Per-letter motions (EG-16): the poses at key times, on live text and alone, and their
//! reduced-motion forms.

use vo_core::words::Word;
use vo_scene::display::{DisplayList, GlyphXform, NO_WORD};
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::letter_motion::{
    digit_lean, italic_skew, tumble, wave, wave_share, LetterMotions, ITALIC_SECS, ITALIC_SKEW, LEAN_MAX, TUMBLE_FROM,
    TUMBLE_SECS, TUMBLE_TURN, WAVE_AMP, WAVE_EASE_SECS, WAVE_HOLD_SECS,
};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

fn scene(reduced: bool) -> Scene<()> {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    Scene::new().with_text(Box::new(ShapedLayout::new(fonts))).with_reduced_motion(reduced)
}

fn words(text: &str) -> Vec<Word> {
    text.split(' ').enumerate().map(|(i, w)| Word { id: i as u32 + 1, text: w.into() }).collect()
}

fn view(w: &[Word], m: LetterMotions) -> Element<()> {
    let live = Element::live_text("live", w).text_style(TextStyle::new(0, 20.0, 26.0)).width(160.0).letter_motion(m);
    Element::column("root").child(live)
}

const TUMBLE: LetterMotions = LetterMotions { tumble: true, ..LetterMotions::NONE };
const ITALIC: LetterMotions = LetterMotions { italic: true, ..LetterMotions::NONE };
const WAVE: LetterMotions = LetterMotions { wave: true, ..LetterMotions::NONE };

fn copies(list: &DisplayList) -> usize {
    list.glyphs.iter().filter(|g| g.word == NO_WORD).count()
}

#[test]
fn a_tumbling_copy_flies_in_and_is_gone_by_350_ms() {
    for seed in 0..64 {
        let p = tumble(1.0, 1.0, seed).expect("in flight at arrival");
        assert!(p.offset.x.hypot(p.offset.y) <= TUMBLE_FROM + 1e-3);
        assert!(p.rot_deg.abs() <= TUMBLE_TURN + 1e-3);
        let late = tumble(1.0, 1.0 + 0.95 * TUMBLE_SECS, seed).unwrap();
        assert!(late.offset.x.hypot(late.offset.y) < 0.05 * TUMBLE_FROM, "lands");
        assert!(tumble(1.0, 1.0 + TUMBLE_SECS, seed).is_none());
        assert!(tumble(1.0, 0.99, seed).is_none());
    }
}

#[test]
fn on_live_text_the_real_glyph_keeps_its_ink_while_its_copy_tumbles() {
    let w = words("tumbling letters");
    let mut plain = scene(false);
    plain.apply(view(&w, LetterMotions::NONE), 1.0);
    let mut s = scene(false);
    s.apply(view(&w, TUMBLE), 1.0);
    for t in [1.0, 1.1, 1.2] {
        let (a, b) = (plain.sample(t), s.sample(t));
        assert!(copies(&b) > 0, "copies in flight at {t}");
        let real: Vec<_> = b.glyphs.iter().filter(|g| g.word != NO_WORD).collect();
        assert_eq!(real.len(), a.glyphs.len());
        for (p, q) in a.glyphs.iter().zip(real) {
            assert_eq!(p.color, q.color, "the copy never dims its letter");
            assert!(q.xform.is_identity());
        }
    }
    assert_eq!(copies(&s.sample(1.0 + TUMBLE_SECS)), 0, "the copies are gone at a + 0.35 s");
    assert_eq!(copies(&s.sample(3.0)), 0);
}

#[test]
fn a_new_word_arrives_leaning_and_straightens_in_200_ms() {
    assert!((italic_skew(2.0, 2.0) - ITALIC_SKEW).abs() < 1e-6);
    assert_eq!(italic_skew(2.0, 2.0 + ITALIC_SECS), 0.0);
    let mut s = scene(false);
    s.apply(view(&words("leaning in"), ITALIC), 1.0);
    let at = s.sample(1.0);
    assert!(at.glyphs.iter().all(|g| (g.xform.skew_deg - ITALIC_SKEW).abs() < 1e-3));
    let mid = s.sample(1.0 + 0.5 * ITALIC_SECS);
    assert!(mid.glyphs.iter().all(|g| g.xform.skew_deg > 0.0 && g.xform.skew_deg < ITALIC_SKEW));
    assert!(s.sample(1.0 + ITALIC_SECS).glyphs.iter().all(|g| g.xform.is_identity()));
}

#[test]
fn the_wave_moves_older_lines_and_never_the_newest() {
    for i in 0..200 {
        let (dy, turn) = wave(i as f32 * 3.7, i as f64 * 0.031);
        assert!(dy.abs() <= WAVE_AMP + 1e-4);
        assert!(turn.abs() < 10.0);
    }
    assert_eq!(wave_share(2.0, 2.0 + WAVE_HOLD_SECS), 0.0, "no jump when the next line shows");
    assert_eq!(wave_share(2.0, 2.0 + WAVE_HOLD_SECS + WAVE_EASE_SECS), 1.0);
    let mut s = scene(false);
    let text = "the older lines ride a slow wave while the newest line holds still for reading";
    s.apply(view(&words(text), WAVE), 0.0);
    let newest_y = |l: &DisplayList| l.glyphs.iter().map(|g| g.pos.y).fold(f32::MIN, f32::max);
    let mut older_moved = false;
    for k in 0..60 {
        let t = 2.0 + k as f64 * 0.05;
        let l = s.sample(t);
        let y = newest_y(&l);
        for g in &l.glyphs {
            if g.pos.y > y - 10.0 {
                assert_eq!(g.xform, GlyphXform::IDENTITY, "newest line glyph at {t}");
            } else {
                older_moved |= !g.xform.is_identity();
            }
        }
    }
    assert!(older_moved, "the older lines ride the wave");
}

#[test]
fn digits_lean_with_pace_and_stand_up_in_a_pause() {
    assert_eq!(digit_lean(0.0, 0.0, false), 0.0);
    assert!((digit_lean(400.0, 0.0, false) - LEAN_MAX).abs() < 1e-6, "at most 12 degrees");
    let fast = digit_lean(150.0, 0.0, false);
    assert!(fast > 0.0 && fast < LEAN_MAX);
    assert!(digit_lean(150.0, 1.0, false) < 0.1 * fast, "back upright in a pause");
    assert_eq!(digit_lean(400.0, 0.0, true), 0.0, "reduced motion");
}

#[test]
fn reduced_motion_has_no_copy_no_lean_and_still_lines() {
    let all = LetterMotions { tumble: true, italic: true, wave: true };
    let mut s = scene(true);
    let text = "the older lines ride a slow wave while the newest line holds still for reading";
    s.apply(view(&words(text), all), 1.0);
    for t in [1.0, 1.1, 2.0, 3.3] {
        let l = s.sample(t);
        assert_eq!(copies(&l), 0);
        assert!(l.glyphs.iter().all(|g| g.xform.is_identity()), "at {t}");
    }
}

#[test]
fn two_runs_draw_the_same_frames() {
    let all = LetterMotions { tumble: true, italic: true, wave: true };
    let text = "the older lines ride a slow wave while the newest line holds still";
    let run = || {
        let mut s = scene(false);
        let w = words(text);
        let mut frames = Vec::new();
        for n in 1..=w.len() {
            let t = n as f64 * 0.3;
            s.apply(view(&w[..n], all), t);
            frames.push(format!("{:?}", s.sample(t + 0.1).glyphs));
        }
        frames
    };
    assert_eq!(run(), run());
}

#[test]
fn a_counter_label_leans_its_digits_and_keeps_its_layout() {
    let label = |lean: f32| -> Element<()> {
        let l = Element::label("wpm", "142").text_style(TextStyle::new(0, 20.0, 26.0)).glyph_lean(lean);
        Element::column("root").child(l)
    };
    let draw = |lean: f32| {
        let mut s = scene(false);
        s.apply(label(lean), 0.0);
        s.sample(0.0).glyphs
    };
    let lean = digit_lean(150.0, 0.0, false);
    let (upright, leaning) = (draw(0.0), draw(lean));
    assert_eq!(upright.len(), 3);
    for (u, l) in upright.iter().zip(&leaning) {
        assert!(u.xform.is_identity());
        assert_eq!(l.xform.skew_deg, lean);
        assert_eq!(u.pos, l.pos, "the lean moves no glyph");
    }
}
