//! Words on a time axis (FX-121) over a recording: a contact sheet with the three looks while
//! live and the default look after the recording, and the cost per frame with the axis across
//! a 600 x 300 and a 1920 x 1080 surface.
//!
//!   cargo run --release -p vo-audio --example axis_demo -- [--frames FILE.json] [--out DIR]
//!
//! `--frames` is the same file `mel_demo` reads; its `"words"` give the word times. Writes
//! `DIR/axis-sheet.png`, DIR defaulting to `/var/tmp/claude/m4-dots/axis`.

#[path = "common/mod.rs"]
mod common;

use vo_audio::{word_axis, Axis, AxisLook, TimedWord};
use vo_core::signals::{Signals, SignalsCell};
use vo_render::Offscreen;
use vo_scene::element::Element;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

/// The axis as it stands at `t`: words the engine has given by then (0.3 s behind the audio),
/// rows laid out to the live edge.
fn scene(
    fonts: &FontRegistry,
    cell: &SignalsCell,
    words: &[TimedWord],
    look: &AxisLook,
    t: f64,
    live: bool,
    size: Size,
) -> Scene {
    let known: Vec<TimedWord> = words.iter().filter(|w| !live || (w.end as f64) < t - 0.3).cloned().collect();
    let axis = Axis { words: known, origin: 0.0, live, until: t as f32 };
    let view: Element = Element::panel("root")
        .size(size.w, size.h)
        .pad(20.0)
        .fill(Color::hex(0x16181d, 1.0))
        .radius(14.0)
        .child(word_axis("axis", cell.clone(), &axis, look, size.w - 40.0));
    let mut s: Scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    s.set_viewport(0.0, size, 1.0);
    s.apply(view, t);
    s
}

fn main() {
    let (out, path) = common::args("/var/tmp/claude/m4-dots/axis");
    let (frames, _) = common::load(path.as_ref());
    let words = common::load_words(path.as_ref(), &frames);
    let secs = frames.last().map_or(1.0, |f| f.clock);
    let cell = SignalsCell::new(Signals::recorded(frames, 0.0));
    let g = common::gpu();
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).expect("test font");
    let mut off = Offscreen::with_fonts(&g.device, &g.queue, fonts.clone());
    let (w, h) = (640u32, 300u32);
    let shots = [
        (AxisLook::default(), 0.45, true),
        (AxisLook::dots(), 0.75, true),
        (AxisLook::hud(), 0.98, true),
        (AxisLook::default(), 1.0, false),
    ];
    let mut imgs = Vec::new();
    for (look, k, live) in &shots {
        let t = k * secs + 0.2;
        let s = scene(&fonts, &cell, &words, look, t, *live, Size::new(w as f32, h as f32));
        imgs.push(off.render_to_rgba(&s.sample(t), [w, h], 1.0));
    }
    let sheet = out.join("axis-sheet.png");
    common::sheet(&sheet, &imgs, w, h);
    println!("axis_demo: {} ({} words over {secs:.1} s)", sheet.display(), words.len());
    let t0 = secs * 0.6;
    common::bench_with_fonts(
        &g,
        fonts.clone(),
        |_| {},
        |size| scene(&fonts, &cell, &words, &AxisLook::hud(), t0, true, size),
        t0,
    );
}
