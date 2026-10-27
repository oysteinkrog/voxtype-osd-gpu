//! Overflow (MO-33): when a line cap drops the first line, it folds under about its
//! baseline, or thins to nothing, while the second line moves up into its place. Each golden
//! stacks the frames of one drop top to bottom.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use support::{Image, Tolerance};
use vo_core::words::Word;
use vo_render::Offscreen;
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::overflow::Overflow;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const W: f32 = 360.0;
const H: f32 = 84.0;

const ALL: [(u32, &str); 15] = [
    (1, "the"),
    (2, "first"),
    (3, "line"),
    (4, "folds"),
    (5, "under"),
    (6, "and"),
    (7, "the"),
    (8, "second"),
    (9, "line"),
    (10, "moves"),
    (11, "up"),
    (12, "into"),
    (13, "its"),
    (14, "place"),
    (15, "now"),
];

fn words(list: &[(u32, &str)]) -> Vec<Word> {
    list.iter().map(|&(id, text)| Word { id, text: text.into() }).collect()
}

fn view(w: &[Word], overflow: Overflow) -> Element<()> {
    let live = Element::live_text("live", w)
        .text_style(TextStyle::new(0, 22.0, 28.0))
        .color(Color::hex(0xf2f3f5, 1.0))
        .overflow(overflow)
        .width(W - 32.0);
    Element::panel("panel").pad(16.0).radius(12.0).fill(Color::hex(0x2a2d33, 1.0)).width(W).height(H).child(live)
}

/// The words the layout puts on the first line, with twelve words shown.
fn first_line(fonts: &FontRegistry) -> usize {
    let mut probe = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    probe.set_viewport(0.0, Size::new(W, H), 1.0);
    probe.apply(view(&words(&ALL[..12]), Overflow::Scroll), 0.0);
    let list = probe.sample(5.0);
    let y = |id: u32| list.glyphs.iter().find(|g| g.word == id).map(|g| g.pos.y);
    let n = ALL.iter().take_while(|w| y(w.0) == y(1)).count();
    assert!((2..12).contains(&n), "first line holds {n} words");
    n
}

fn strip(off: &mut Offscreen, fonts: &FontRegistry, name: &str, overflow: Overflow, times: &[f64]) -> Image {
    let first = first_line(fonts);
    let steps = [(0.0, words(&ALL[..12])), (1.0, words(&ALL[first..]))];
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    let mut px = Vec::new();
    let mut next = 0;
    for &t in times {
        while next < steps.len() && steps[next].0 <= t {
            scene.apply(view(&steps[next].1, overflow), steps[next].0);
            next += 1;
        }
        let list = scene.sample(t);
        px.extend(off.render_to_rgba(&list, [W as u32, H as u32], 1.0));
    }
    Image { name: name.into(), w: W as u32, h: H as u32 * times.len() as u32, channels: 4, px }
}

#[test]
fn overflow_goldens() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let frames = [0.9, 1.04, 1.08, 1.12, 1.16, 1.6];
    let mut failures = Vec::new();
    for (name, overflow) in [("overflow-fold", Overflow::FOLD), ("overflow-thin", Overflow::THIN)] {
        let img = strip(&mut off, &fonts, name, overflow, &frames);
        if let Err(e) = support::check(&img, Tolerance::default()) {
            failures.push(e);
        }
    }
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// The leaving line is drawn in the frames of its fold and is gone once it ends; the scene
/// asks for frames until then.
#[test]
fn fold_draws_until_it_ends() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let first = first_line(&fonts);
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(view(&words(&ALL[..12]), Overflow::FOLD), 0.0);
    scene.apply(view(&words(&ALL[first..]), Overflow::FOLD), 1.0);
    let gone = |t: f64| scene.sample(t).glyphs.iter().filter(|g| g.word == 1).count();
    assert!(gone(1.05) > 0, "the first word is still drawn mid-fold");
    assert!(scene.sample(1.08).layers.iter().any(|l| l.warp.is_some()), "the fold turns through the stage");
    assert_eq!(gone(1.3), 0, "the first word is gone after the fold");
    assert!(scene.next_wake(1.1).is_some_and(|w| w <= 1.23), "frames until the fold ends");
}
