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
use vo_scene::flow::{Caret, CaseChange, Revise, SettleInk};
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
    view_with(w, Revise::Crossfade, CaseChange::Roll)
}

fn view_with(w: &[Word], revise: Revise, case: CaseChange) -> Element<()> {
    let live = Element::live_text("live", w)
        .text_style(TextStyle::new(0, 22.0, 28.0))
        .color(Color::hex(0xf2f3f5, 1.0))
        .word_revise(revise)
        .word_case(case)
        .word_enter(Transition::fade())
        .caret(Caret { color: Some(Color::hex(0x6cb6ff, 1.0)), ..Caret::default() })
        .width(W - 32.0);
    Element::panel("panel").pad(16.0).radius(12.0).fill(Color::hex(0x1d1f24, 1.0)).width(W).height(H).child(live)
}

/// Applies `steps` (time, words) and renders the frames at `times`, stacked.
fn strip(off: &mut Offscreen, fonts: &FontRegistry, name: &str, steps: &[(f64, Vec<Word>)], times: &[f64]) -> Image {
    strip_with(off, fonts, name, steps, times, view)
}

fn strip_with(
    off: &mut Offscreen,
    fonts: &FontRegistry,
    name: &str,
    steps: &[(f64, Vec<Word>)],
    times: &[f64],
    view: impl Fn(&[Word]) -> Element<()>,
) -> Image {
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

/// The glyph diff (T08, T09): a case fix rolls one letter, a comma pops in from the
/// baseline, and with `CaseChange::Grow` a letter grows from x-height to cap height.
#[test]
fn glyph_diff_goldens() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let frames = [0.9, 1.03, 1.06, 1.09, 1.14, 1.6];
    let steps = [
        (0.0, words(&[(1, "so"), (2, "the"), (3, "plan"), (4, "is"), (5, "simple")])),
        (1.0, words(&[(1, "So"), (2, "the"), (3, "plan"), (4, "is"), (5, "simple.")])),
    ];
    let roll = strip_with(&mut off, &fonts, "live-glyph-roll", &steps, &frames, |w| {
        view_with(w, Revise::Glyphs, CaseChange::Roll)
    });
    let grow = strip_with(&mut off, &fonts, "live-glyph-grow", &steps, &frames, |w| {
        view_with(w, Revise::Glyphs, CaseChange::Grow)
    });
    let comma = strip_with(
        &mut off,
        &fonts,
        "live-glyph-comma",
        &[
            (0.0, words(&[(1, "well"), (2, "I"), (3, "think"), (4, "so")])),
            (1.0, words(&[(1, "well,"), (2, "I"), (3, "think"), (4, "so")])),
        ],
        &frames,
        |w| view_with(w, Revise::Glyphs, CaseChange::Roll),
    );
    let failures: Vec<String> =
        [roll, grow, comma].iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// Settling ink (T03): each new word arrives lighter and a little looser and settles; the
/// revised "their" starts its settle again.
#[test]
fn settling_ink_goldens() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let ink = |w: &[Word]| {
        let live = Element::live_text("live", w)
            .text_style(TextStyle::new(0, 22.0, 28.0))
            .color(Color::hex(0xf2f3f5, 1.0))
            .settling_ink(SettleInk::default())
            .caret(Caret { color: Some(Color::hex(0x6cb6ff, 1.0)), ..Caret::default() })
            .width(W - 32.0);
        Element::panel("panel").pad(16.0).radius(12.0).fill(Color::hex(0x1d1f24, 1.0)).width(W).height(H).child(live)
    };
    let steps = [
        (0.0, words(&[(1, "words")])),
        (0.3, words(&[(1, "words"), (2, "settle")])),
        (0.6, words(&[(1, "words"), (2, "settle"), (3, "there")])),
        (1.0, words(&[(1, "words"), (2, "settle"), (3, "their"), (4, "ink")])),
    ];
    let times = [0.29, 0.62, 0.8, 1.02, 1.2, 2.0];
    let img = strip_with(&mut off, &fonts, "live-settle-ink", &steps, &times, ink);
    // The accent variant: new words arrive in blue and mix to the text colour as they settle.
    let accent = strip_with(&mut off, &fonts, "live-settle-accent", &steps, &times, |w| {
        let live = Element::live_text("live", w)
            .text_style(TextStyle::new(0, 22.0, 28.0))
            .color(Color::hex(0xf2f3f5, 1.0))
            .settling_ink(SettleInk::default().accent(Color::hex(0x6cb6ff, 1.0)))
            .width(W - 32.0);
        Element::panel("panel").pad(16.0).radius(12.0).fill(Color::hex(0x1d1f24, 1.0)).width(W).height(H).child(live)
    });
    let failures: Vec<String> =
        [img, accent].iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// A line cap: when the text fills its two lines, the first line drops and the rest scroll
/// up as one block while the next words arrive, instead of each word taking its own path.
#[test]
fn front_drop_golden() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let all = [
        (1, "the"),
        (2, "first"),
        (3, "line"),
        (4, "drops"),
        (5, "away"),
        (6, "and"),
        (7, "the"),
        (8, "second"),
        (9, "line"),
        (10, "moves"),
        (11, "up"),
        (12, "as"),
        (13, "one"),
        (14, "block"),
        (15, "while"),
    ];
    // Which words the layout puts on the first line.
    let mut probe = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    probe.set_viewport(0.0, Size::new(W, H), 1.0);
    probe.apply(view(&words(&all[..12])), 0.0);
    let list = probe.sample(5.0);
    let y = |id: u32| list.glyphs.iter().find(|g| g.word == id).map(|g| g.pos.y);
    let first_line = all.iter().take_while(|w| y(w.0) == y(1)).count();
    assert!((2..12).contains(&first_line), "first line holds {first_line} words");
    let steps = [(0.0, words(&all[..12])), (1.0, words(&all[first_line..]))];
    let img = strip(&mut off, &fonts, "live-front-drop", &steps, &[0.9, 1.03, 1.06, 1.1, 1.16, 1.6]);
    support::check(&img, Tolerance::default()).unwrap();
}

/// A line cap that drops one word at a time from the front, with "…" on the new first word,
/// as the popup does: the text reflows on every new word, and words that change line fade
/// across instead of gliding over the other words.
#[test]
fn line_cap_reflow_golden() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let text = "open the configuration file and set the popup theme to swiss so the dictation window shows";
    let all: Vec<(u32, &str)> = text.split(' ').enumerate().map(|(i, w)| (i as u32 + 1, w)).collect();
    let lines = |shown: &[Word]| {
        let mut probe = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
        probe.set_viewport(0.0, Size::new(W, H), 1.0);
        probe.apply(view(shown), 0.0);
        let mut ys: Vec<f32> = probe.sample(5.0).glyphs.iter().map(|g| g.pos.y).collect();
        ys.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        ys.len()
    };
    let capped = |n: usize| {
        let mut start = 0;
        loop {
            let mut shown = words(&all[start..n]);
            if start > 0 {
                shown[0].text = format!("…{}", shown[0].text);
            }
            if lines(&shown) <= 2 || start + 1 == n {
                return shown;
            }
            start += 1;
        }
    };
    let steps: Vec<(f64, Vec<Word>)> = (1..=all.len()).map(|n| (n as f64 * 0.12, capped(n))).collect();
    let last = all.len() as f64 * 0.12;
    let times = [last - 0.13, last - 0.1, last - 0.07, last - 0.01, last + 0.03, last + 0.6];
    let img = strip(&mut off, &fonts, "live-line-cap", &steps, &times);
    support::check(&img, Tolerance::default()).unwrap();
}
