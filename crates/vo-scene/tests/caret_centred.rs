//! The end caret never stands inside a glyph of the last word, on a centred line too: there a
//! new word or a longer one moves the whole line to its new middle for about 150 ms, and the
//! caret must stay after the word as it is drawn in every frame of that slide, with or
//! without the magnet (MO-89), whose snap moves a new word a few pixels on.

use vo_core::words::Word;
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::magnet::Magnet;
use vo_scene::flow::Caret;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::text::{Align, ShapedLayout, TextLayout};
use vo_scene::Scene;

const SIZE: f32 = 20.0;
const CARET_W: f32 = 3.0;

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

fn style(align: Align) -> TextStyle {
    TextStyle::new(0, SIZE, 26.0).align(align)
}

fn view(text: &str, align: Align, magnet: bool) -> Element<()> {
    let w: Vec<Word> = text.split(' ').enumerate().map(|(i, t)| Word { id: i as u32 + 1, text: t.into() }).collect();
    let live = Element::live_text("live", &w)
        .text_style(style(align))
        .width(400.0)
        .caret(Caret { width: CARET_W, ..Caret::default() });
    let live = if magnet { live.magnetic_caret(Magnet::default()) } else { live };
    Element::column("root").child(live)
}

/// The caret's left edge and the right edge of the last word's ink: the pen of its last glyph
/// plus that glyph's advance, which `advance` gives for the word. The live text sits at the
/// origin, so its box and the frame share coordinates.
fn caret_and_edge(s: &mut Scene<()>, t: f64, word: u32, advance: f32) -> (f32, f32) {
    let list = s.sample(t);
    let pen = list.glyphs.iter().filter(|g| g.word == word).map(|g| g.pos.x).fold(f32::MIN, f32::max);
    let id = s.find(&["root", "live"]).unwrap();
    (s.caret(id, t).expect("the caret").0.x, pen + advance)
}

#[test]
fn the_caret_stays_after_the_last_word_while_a_centred_line_slides() {
    let layout = ShapedLayout::new(fonts());
    let steps =
        ["the", "the con", "the confi", "the configuration", "the configuration file", "the configuration file is"];
    for align in [Align::Center, Align::Start] {
        for magnet in [false, true] {
            let mut s = Scene::new().with_text(Box::new(ShapedLayout::new(fonts())));
            s.apply(view(steps[0], align, magnet), 0.0);
            for (k, text) in steps.iter().enumerate() {
                let at = 1.0 + 0.5 * k as f64;
                s.apply(view(text, align, magnet), at);
                let last = text.split(' ').count() as u32;
                let word = text.split(' ').next_back().unwrap();
                let laid = layout.layout(word, &style(Align::Start), None);
                let advance = laid.size.w - laid.glyphs.last().unwrap().pos.x;
                for n in 0..60 {
                    let t = at + n as f64 / 120.0;
                    let (caret, edge) = caret_and_edge(&mut s, t, last, advance);
                    assert!(
                        caret >= edge - 0.5,
                        "{align:?}, magnet {magnet}, \"{text}\" at +{n}/120 s: caret at {caret:.1}, \
                         the word's ink ends at {edge:.1}"
                    );
                }
            }
        }
    }
}
