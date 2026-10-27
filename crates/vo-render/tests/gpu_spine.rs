//! The overflow spine (MO-34): a line that a line cap drops leaves word-wide blocks in the
//! margin above the text, which close into one history tick. The golden stacks the frames
//! of two drops top to bottom.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use support::{Image, Tolerance};
use vo_core::words::Word;
use vo_render::Offscreen;
use vo_scene::display::{DisplayList, PaintKind};
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::spine::Spine;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const W: f32 = 360.0;
const H: f32 = 96.0;
const INK: u32 = 0xf2b84b;

const ALL: [(u32, &str); 24] = [
    (1, "every"),
    (2, "line"),
    (3, "that"),
    (4, "leaves"),
    (5, "the"),
    (6, "text"),
    (7, "leaves"),
    (8, "a"),
    (9, "small"),
    (10, "mark"),
    (11, "above"),
    (12, "it"),
    (13, "so"),
    (14, "you"),
    (15, "see"),
    (16, "how"),
    (17, "much"),
    (18, "was"),
    (19, "said"),
    (20, "since"),
    (21, "the"),
    (22, "start"),
    (23, "of"),
    (24, "it"),
];

fn words(list: &[(u32, &str)]) -> Vec<Word> {
    list.iter().map(|&(id, text)| Word { id, text: text.into() }).collect()
}

fn view(w: &[Word]) -> Element<()> {
    let live = Element::live_text("live", w)
        .text_style(TextStyle::new(0, 22.0, 28.0))
        .color(Color::hex(0xf2f3f5, 1.0))
        .overflow_spine(Spine::new(Color::hex(INK, 1.0)))
        .width(W - 32.0);
    Element::panel("panel")
        .pad_edges(16.0, 28.0, 16.0, 12.0)
        .radius(12.0)
        .fill(Color::hex(0x2a2d33, 1.0))
        .width(W)
        .height(H)
        .child(live)
}

fn scene(fonts: &FontRegistry) -> Scene<()> {
    let mut s = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    s.set_viewport(0.0, Size::new(W, H), 1.0);
    s
}

/// Where each line of `ALL` starts, as word counts.
fn line_starts(fonts: &FontRegistry) -> Vec<usize> {
    let mut probe = scene(fonts);
    probe.apply(view(&words(&ALL)), 0.0);
    let list = probe.sample(5.0);
    let y = |id: u32| list.glyphs.iter().find(|g| g.word == id).map(|g| g.pos.y);
    let mut starts = vec![0];
    for i in 1..ALL.len() {
        if y(ALL[i].0) != y(ALL[i - 1].0) {
            starts.push(i);
        }
    }
    assert!(starts.len() >= 4, "the text needs four lines: {starts:?}");
    starts
}

/// Shapes in the spine's colour as `(x, width, alpha)`.
fn marks(list: &DisplayList) -> Vec<(f32, f32, f32)> {
    let ink = Color::hex(INK, 1.0);
    list.shapes
        .iter()
        .filter(|s| s.fill.kind == PaintKind::Solid)
        .filter(|s| (s.fill.c0.r - ink.r).abs() < 0.01 && (s.fill.c0.g - ink.g).abs() < 0.01)
        .map(|s| (s.geom[0], s.geom[2], s.fill.c0.a))
        .collect()
}

/// Two lines on screen, then the first drops at 1 s and the next at 2.5 s.
fn steps(starts: &[usize]) -> [(f64, Vec<Word>); 3] {
    [
        (0.0, words(&ALL[..starts[2]])),
        (1.0, words(&ALL[starts[1]..starts[3]])),
        (2.5, words(&ALL[starts[2]..starts[3] + 1])),
    ]
}

#[test]
fn spine_goldens() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let steps = steps(&line_starts(&fonts));
    let mut s = scene(&fonts);
    let times = [0.9, 1.1, 1.6, 1.75, 2.0, 2.7, 3.4];
    let (mut px, mut next) = (Vec::new(), 0);
    for &t in &times {
        while next < steps.len() && steps[next].0 <= t {
            s.apply(view(&steps[next].1), steps[next].0);
            next += 1;
        }
        px.extend(off.render_to_rgba(&s.sample(t), [W as u32, H as u32], 1.0));
    }
    let img = Image { name: "overflow-spine".into(), w: W as u32, h: H as u32 * times.len() as u32, channels: 4, px };
    support::check(&img, Tolerance::default()).unwrap();
}

/// A drop leaves one block per word, which close into one tick; the next drop adds a
/// second tick beside it; empty text clears the spine.
#[test]
fn each_drop_leaves_one_tick() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let starts = line_starts(&fonts);
    let steps = steps(&starts);
    let mut s = scene(&fonts);
    s.apply(view(&steps[0].1), 0.0);
    assert!(marks(&s.sample(0.9)).is_empty(), "nothing before a drop");
    s.apply(view(&steps[1].1), 1.0);
    let blocks = marks(&s.sample(1.3));
    assert_eq!(blocks.len(), starts[1], "one block per word of the dropped line");
    assert!(blocks.windows(2).all(|b| b[0].0 < b[1].0), "blocks in reading order");
    let tick = marks(&s.sample(2.2));
    assert_eq!(tick.len(), 1, "closed into one tick");
    assert!(tick[0].1 < 3.0 && tick[0].2 > 0.99, "a thin, solid tick: {tick:?}");
    s.apply(view(&steps[2].1), 2.5);
    let ticks = marks(&s.sample(4.0));
    assert_eq!(ticks.len(), 2, "two drops, two ticks");
    assert!(ticks[1].0 > ticks[0].0, "the new tick sits after the first");
    s.apply(view(&[]), 5.0);
    assert!(marks(&s.sample(5.1)).is_empty(), "empty text clears the spine");
}
