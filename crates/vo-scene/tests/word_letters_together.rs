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
