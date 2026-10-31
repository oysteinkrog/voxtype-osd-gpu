//! A word's letters come in together: while words arrive faster than a centred line slides
//! to make room, the letters a word shows are always its first letters in order. Never
//! "bove" for "above" because the "a" waits for the word before it to slide away. Checked at
//! 120 fps on every alignment.

use std::collections::HashMap;

use vo_core::spring::Spring;
use vo_core::words::Word;
use vo_scene::display::NO_WORD;
use vo_scene::element::{Element, Motion, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::text::{Align, ShapedLayout, TextLayout};
use vo_scene::Scene;

const SIZE: f32 = 26.0;
const LINE: f32 = 34.0;
/// Below this ink a letter is not seen.
const SEEN: f32 = 0.05;

const SAID: &str = "the live words in the middle of the screen with the waveform above them then press \
                    the key again to stop and let the final pass fix the casing";

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

fn style(align: Align) -> TextStyle {
    TextStyle::new(0, SIZE, LINE).align(align)
}

fn view(words: &[Word], align: Align) -> Element<()> {
    let live = Element::live_text("live", words).text_style(style(align)).width(520.0);
    Element::column("root").child(live)
}

#[test]
fn a_word_shows_its_first_letters_in_order() {
    let fonts = fonts();
    let layout = ShapedLayout::new(fonts.clone());
    let all: Vec<Word> = SAID.split(' ').enumerate().map(|(i, w)| Word { id: i as u32 + 1, text: w.into() }).collect();
    let shape = |text: &str| -> Vec<u32> {
        layout.layout(text, &style(Align::Center), None).glyphs.iter().map(|g| g.glyph).collect()
    };
    for align in [Align::Center, Align::Start, Align::End] {
        // Words slide to their places, as the popup's looks have them.
        let mut scene = Scene::new()
            .with_text(Box::new(ShapedLayout::new(fonts.clone())))
            .with_default_motion(Motion::Spring(Spring::SOFT));
        scene.set_viewport(0.0, vo_scene::geom::Size::new(600.0, 400.0), 1.0);
        // A word every 90 ms, as fast as the popup's demo dictation speaks, so a line is
        // still sliding from one word when the next arrives.
        let every = 0.09;
        let end = every * all.len() as f64 + 1.0;
        let partial = |t: f64| all[..((t / every) as usize + 1).min(all.len())].to_vec();
        let mut shown: Vec<Word> = Vec::new();
        let mut worst: Option<(f64, String, Vec<u32>, Vec<u32>)> = None;
        let mut frames = 0;
        while (frames as f64) / 120.0 < end {
            let t = frames as f64 / 120.0;
            let now = partial(t);
            if now != shown {
                scene.apply(view(&now, align), t);
                shown = now;
            }
            let list = scene.sample(t);
            let mut by: HashMap<u32, Vec<(f32, u32)>> = HashMap::new();
            for g in list.glyphs.iter().filter(|g| g.word != NO_WORD && g.color.a > SEEN) {
                by.entry(g.word).or_default().push((g.pos.x, g.glyph));
            }
            for (word, mut seen) in by {
                let Some(w) = shown.iter().find(|w| w.id == word) else { continue };
                let own = shape(&w.text);
                seen.sort_by(|a, b| a.0.total_cmp(&b.0));
                let ids: Vec<u32> = seen.iter().map(|s| s.1).collect();
                // A letter of the word's last text that still fades out (a ligature that
                // split, "fi" to "f") is the revision's, not a gap.
                let current = ids.iter().all(|i| own.contains(i));
                if current && !own.starts_with(&ids) && worst.is_none() {
                    worst = Some((t, w.text.clone(), ids, own));
                }
            }
            frames += 1;
        }
        if let Some((t, text, ids, own)) = worst {
            panic!("{align:?}: at {t:.3} s word {text:?} shows glyphs {ids:?} of {own:?}");
        }
    }
}

/// Over this ink a glyph can be read.
const READABLE: f32 = 0.35;

/// The first frame, at 120 fps from `from` for a second, where a word shows two readable
/// glyphs in one place or readable glyphs on two rows: two versions of it at once.
fn two_versions(scene: &Scene<()>, from: f64) -> Option<(f64, u32)> {
    (0..120).map(|i| from + i as f64 / 120.0).find_map(|t| {
        let list = scene.sample(t);
        let mut by: HashMap<u32, Vec<_>> = HashMap::new();
        for g in list.glyphs.iter().filter(|g| g.word != NO_WORD && g.color.a > READABLE) {
            by.entry(g.word).or_default().push(*g);
        }
        by.into_iter()
            .find(|(_, gs)| {
                gs.iter().enumerate().any(|(i, a)| {
                    gs[i + 1..].iter().any(|b| {
                        let (dx, dy) = ((a.pos.x - b.pos.x).abs(), (a.pos.y - b.pos.y).abs());
                        (a.glyph != b.glyph && dx < 0.1 * SIZE && dy < 0.3 * SIZE) || dy > 0.6 * SIZE
                    })
                })
            })
            .map(|(w, _)| (t, w))
    })
}

fn words(text: &str) -> Vec<Word> {
    text.split(' ').enumerate().map(|(i, w)| Word { id: i as u32 + 1, text: w.into() }).collect()
}

#[test]
fn a_revised_word_never_shows_old_and_new_letters_readable_in_one_place() {
    for reduced in [false, true] {
        let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts()))).with_reduced_motion(reduced);
        scene.set_viewport(0.0, vo_scene::geom::Size::new(600.0, 400.0), 1.0);
        scene.apply(view(&words("set the popup theme to swish so the"), Align::Start), 0.0);
        scene.sample(1.0);
        scene.apply(view(&words("set the popup theme to swiss so the"), Align::Start), 1.0);
        assert_eq!(two_versions(&scene, 1.0), None, "reduced {reduced}");
    }
}

#[test]
fn under_reduced_motion_a_rewrap_never_shows_a_word_twice() {
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts()))).with_reduced_motion(true);
    scene.set_viewport(0.0, vo_scene::geom::Size::new(600.0, 400.0), 1.0);
    let long = "with the waveform above them then press the key again to stop and let the final pass";
    scene.apply(view(&words(long), Align::Start), 0.0);
    scene.sample(1.0);
    // A wider first word moves every line break.
    let wider = long.replacen("with", "Wherewith", 1);
    scene.apply(view(&words(&wider), Align::Start), 1.0);
    assert_eq!(two_versions(&scene, 1.0), None);
}

#[test]
fn a_replaced_letter_leaves_no_copy_under_the_new_one_and_none_goes_missing() {
    // "open" to "Open": the wider "O" waits for "pen" to slide; the "o" stays until it shows.
    for reduced in [false, true] {
        let mut scene = Scene::new()
            .with_text(Box::new(ShapedLayout::new(fonts())))
            .with_default_motion(Motion::Spring(Spring::SOFT))
            .with_reduced_motion(reduced);
        scene.set_viewport(0.0, vo_scene::geom::Size::new(600.0, 400.0), 1.0);
        let first = |l: &vo_scene::display::DisplayList| -> f32 {
            let xs = l.glyphs.iter().filter(|g| g.word == 1).map(|g| g.pos.x);
            let (a, b) = xs.fold((f32::MAX, f32::MIN), |(a, b), x| (a.min(x), b.max(x)));
            b - a
        };
        scene.apply(view(&words("open the file"), Align::Start), 0.0);
        let before = first(&scene.sample(1.0));
        let mut done = Scene::new().with_text(Box::new(ShapedLayout::new(fonts())));
        done.set_viewport(0.0, vo_scene::geom::Size::new(600.0, 400.0), 1.0);
        done.apply(view(&words("Open the file"), Align::Start), 0.0);
        let after = first(&done.sample(1.0));
        scene.apply(view(&words("Open the file"), Align::Start), 1.0);
        for i in 0..120 {
            let t = 1.0 + i as f64 / 120.0;
            let list = scene.sample(t);
            let first: Vec<_> = list.glyphs.iter().filter(|g| g.word == 1 && g.color.a > 0.02).collect();
            for (k, a) in first.iter().enumerate() {
                for b in &first[k + 1..] {
                    assert!(
                        a.glyph == b.glyph || (a.pos.x - b.pos.x).abs() > 0.3 * SIZE,
                        "reduced {reduced}: at {t:.3} s two letters over each other at {:.0}",
                        a.pos.x
                    );
                }
            }
            let readable = first.iter().filter(|g| g.color.a > READABLE).count();
            assert_eq!(readable, 4, "reduced {reduced}: at {t:.3} s the word shows {readable} letters");
            // No gap opens beside the old letter: the word is as wide as before or as after.
            let span = |gs: &[&vo_scene::display::GlyphInstance]| {
                let xs = gs.iter().filter(|g| g.color.a > READABLE).map(|g| g.pos.x);
                let (a, b) = xs.fold((f32::MAX, f32::MIN), |(a, b), x| (a.min(x), b.max(x)));
                b - a
            };
            let now = span(&first);
            assert!(
                (now - before).abs() < 0.5 || (now - after).abs() < 0.5,
                "reduced {reduced}: at {t:.3} s the word spans {now:.1} px, {before:.1} before and {after:.1} after"
            );
        }
    }
}

#[test]
fn a_deleted_letter_still_leaves_a_faint_ghost() {
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts())));
    scene.set_viewport(0.0, vo_scene::geom::Size::new(600.0, 400.0), 1.0);
    scene.apply(view(&words("cream, then"), Align::Start), 0.0);
    scene.sample(1.0);
    scene.apply(view(&words("cream then"), Align::Start), 1.0);
    let list = scene.sample(1.2);
    let ghost = list.glyphs.iter().filter(|g| g.word == 1).count();
    assert_eq!(ghost, 6, "five letters and the comma's ghost");
}

#[test]
fn under_reduced_motion_a_rewrapped_word_is_never_faint() {
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts()))).with_reduced_motion(true);
    scene.set_viewport(0.0, vo_scene::geom::Size::new(600.0, 400.0), 1.0);
    let long = "with the waveform above them then press the key again to stop and let the final pass";
    scene.apply(view(&words(long), Align::Start), 0.0);
    scene.sample(1.0);
    let wider = long.replacen("with", "Wherewith", 1);
    scene.apply(view(&words(&wider), Align::Start), 1.0);
    for i in 0..60 {
        let t = 1.0 + i as f64 / 120.0;
        // Word 1 is the revised one: its new letters arrive at arrival ink.
        let faint =
            scene.sample(t).glyphs.iter().filter(|g| g.word > 1 && g.word != NO_WORD && g.color.a < 0.99).count();
        assert_eq!(faint, 0, "at {t:.3} s");
    }
}
