//! The margin mark (MO-32): a bar beside the newest line that splits when the text wraps,
//! its top half staying with the previous line and fading, its bottom half travelling to
//! the new line and growing back. The golden stacks the frames of one wrap top to bottom.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use support::{Image, Tolerance};
use vo_core::words::Word;
use vo_render::Offscreen;
use vo_scene::display::{DisplayList, PaintKind};
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::margin::MarginMark;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const W: f32 = 360.0;
const H: f32 = 84.0;
const MARK: u32 = 0x5ce1b0;

const ALL: [(u32, &str); 10] = [
    (1, "a"),
    (2, "mark"),
    (3, "beside"),
    (4, "the"),
    (5, "newest"),
    (6, "line"),
    (7, "splits"),
    (8, "on"),
    (9, "the"),
    (10, "wrap"),
];

fn words(list: &[(u32, &str)]) -> Vec<Word> {
    list.iter().map(|&(id, text)| Word { id, text: text.into() }).collect()
}

fn view(w: &[Word]) -> Element<()> {
    let live = Element::live_text("live", w)
        .text_style(TextStyle::new(0, 22.0, 28.0))
        .color(Color::hex(0xf2f3f5, 1.0))
        .margin_mark(MarginMark::new(Color::hex(MARK, 1.0)))
        .width(220.0);
    Element::panel("panel").pad(16.0).radius(12.0).fill(Color::hex(0x2a2d33, 1.0)).width(W).height(H).child(live)
}

fn scene(fonts: &FontRegistry) -> Scene<()> {
    let mut s = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    s.set_viewport(0.0, Size::new(W, H), 1.0);
    s
}

/// How many words fit on the first line.
fn first_line(fonts: &FontRegistry) -> usize {
    let mut probe = scene(fonts);
    probe.apply(view(&words(&ALL)), 0.0);
    let list = probe.sample(5.0);
    let y = |id: u32| list.glyphs.iter().find(|g| g.word == id).map(|g| g.pos.y);
    let n = ALL.iter().take_while(|w| y(w.0) == y(1)).count();
    assert!((2..ALL.len()).contains(&n), "first line holds {n} words");
    n
}

/// The mark's bars in `list`: shapes in its colour, as (top, height, alpha).
fn bars(list: &DisplayList) -> Vec<(f32, f32, f32)> {
    let mark = Color::hex(MARK, 1.0);
    list.shapes
        .iter()
        .filter(|s| s.fill.kind == PaintKind::Solid)
        .filter(|s| (s.fill.c0.r - mark.r).abs() < 0.01 && (s.fill.c0.g - mark.g).abs() < 0.01)
        .map(|s| (s.geom[1], s.geom[3], s.fill.c0.a))
        .collect()
}

#[test]
fn margin_goldens() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let n = first_line(&fonts);
    let mut s = scene(&fonts);
    s.apply(view(&words(&ALL[..n])), 0.0);
    let times = [0.9, 1.03, 1.08, 1.15, 1.3, 1.8];
    let mut px = Vec::new();
    for &t in &times {
        if t >= 1.0 {
            s.apply(view(&words(&ALL[..n + 1])), 1.0);
        }
        px.extend(off.render_to_rgba(&s.sample(t), [W as u32, H as u32], 1.0));
    }
    let img = Image { name: "margin-split".into(), w: W as u32, h: H as u32 * times.len() as u32, channels: 4, px };
    support::check(&img, Tolerance::default()).unwrap();
}

/// On the wrap the mark splits into two half bars; the top one fades and the bottom one
/// ends whole beside the new line. A front drop, which is not a wrap, does not split it.
#[test]
fn mark_splits_on_a_wrap_only() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let n = first_line(&fonts);
    let mut s = scene(&fonts);
    s.apply(view(&words(&ALL[..n])), 0.0);
    let before = bars(&s.sample(0.9));
    assert_eq!(before.len(), 1, "one bar before the wrap");
    s.apply(view(&words(&ALL[..n + 1])), 1.0);
    let mid = bars(&s.sample(1.02));
    assert_eq!(mid.len(), 2, "two halves just after the wrap: {mid:?}");
    assert!(mid.iter().all(|b| b.1 < 0.6 * before[0].1), "both are about half height: {mid:?}");
    let after = bars(&s.sample(2.0));
    assert_eq!(after.len(), 1, "the top half is gone: {after:?}");
    assert!((after[0].1 - before[0].1).abs() < 0.5, "full height again");
    assert!(after[0].0 > before[0].0 + 20.0, "beside the second line");
    // Drop the first line: the mark stays one bar and moves up with its line.
    s.apply(view(&words(&ALL[n..n + 1])), 3.0);
    assert_eq!(bars(&s.sample(3.05)).len(), 1, "a front drop does not split the mark");
}
