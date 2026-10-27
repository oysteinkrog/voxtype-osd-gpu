//! Live text motion on the GPU (T02, T11, T17): film strips of a revision that crossfades
//! while its neighbours slide, a rewrap that glides in reading order, and a caret that
//! glides and blinks. Each golden stacks the frames of one scenario top to bottom.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use support::{Image, Tolerance};
use vo_core::words::Word;
use vo_render::Offscreen;
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::{Caret, Revise};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::{Scene, Transition};

const W: f32 = 360.0;
const H: f32 = 84.0;

fn words(list: &[(u32, &str)]) -> Vec<Word> {
    list.iter().map(|&(id, text)| Word { id, text: text.into() }).collect()
}

fn view(w: &[Word]) -> Element<()> {
    let live = Element::live_text("live", w)
        .text_style(TextStyle::new(0, 22.0, 28.0))
        .color(Color::hex(0xf2f3f5, 1.0))
        .word_revise(Revise::Crossfade)
        .word_enter(Transition::fade())
        .caret(Caret { color: Some(Color::hex(0x6cb6ff, 1.0)), ..Caret::default() })
        .width(W - 32.0);
    Element::panel("panel").pad(16.0).radius(12.0).fill(Color::hex(0x1d1f24, 1.0)).width(W).height(H).child(live)
}

/// Applies `steps` (time, words) and renders the frames at `times`, stacked.
fn strip(off: &mut Offscreen, fonts: &FontRegistry, name: &str, steps: &[(f64, Vec<Word>)], times: &[f64]) -> Image {
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    let mut px = Vec::new();
    let mut next = 0;
    for &t in times {
        while next < steps.len() && steps[next].0 <= t {
            scene.apply(view(&steps[next].1), steps[next].0);
            next += 1;
        }
        let list = scene.sample(t);
        px.extend(off.render_to_rgba(&list, [W as u32, H as u32], 1.0));
    }
    Image { name: name.into(), w: W as u32, h: H as u32 * times.len() as u32, channels: 4, px }
}

#[test]
fn live_flow_goldens() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let frames = [0.9, 1.02, 1.05, 1.08, 1.13, 1.6];

    let revise = strip(
        &mut off,
        &fonts,
        "live-revise",
        &[
            (0.0, words(&[(1, "the"), (2, "quick"), (3, "brown"), (4, "fox")])),
            (1.0, words(&[(1, "the"), (2, "quickest"), (3, "brown"), (4, "fox")])),
        ],
        &frames,
    );
    let rewrap = strip(
        &mut off,
        &fonts,
        "live-rewrap",
        &[
            (0.0, words(&[(1, "a"), (2, "short"), (3, "line"), (4, "of"), (5, "words")])),
            (1.0, words(&[(1, "a"), (2, "considerably"), (6, "longer"), (3, "line"), (4, "of"), (5, "words")])),
        ],
        &frames,
    );
    // The caret: glides after "world", solid for the hold, then on and off.
    let caret = strip(
        &mut off,
        &fonts,
        "live-caret",
        &[(0.0, words(&[(1, "hello")])), (1.0, words(&[(1, "hello"), (2, "world")]))],
        &[0.9, 1.04, 1.1, 1.3, 1.6, 2.2],
    );
    let failures: Vec<String> =
        [revise, rewrap, caret].iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}
