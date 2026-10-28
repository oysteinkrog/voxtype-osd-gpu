//! Speech rate and rhythm (FX-119) over a recording: a contact sheet with the rate numbers,
//! the curve and the syllable ticks at three times, the last in the dot-matrix look, and the
//! cost per frame with the views across a 600 x 300 and a 1920 x 1080 surface.
//!
//!   cargo run --release -p vo-audio --example rate_demo -- [--frames FILE.json] [--out DIR]
//!
//! `--frames` is the same file `mel_demo` reads; its `"words"` give the words per minute.
//! Writes `DIR/rate-sheet.png`, DIR defaulting to `/var/tmp/claude/m4-dots/rate`.

#[path = "common/mod.rs"]
mod common;

use vo_audio::{rate_curve, rate_ticks, RateLook, RateStats, TimedWord};
use vo_core::signals::{Signals, SignalsCell};
use vo_render::Offscreen;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

fn scene(fonts: &FontRegistry, cell: &SignalsCell, words: &[TimedWord], look: &RateLook, t: f64, size: Size) -> Scene {
    let known: Vec<TimedWord> = words.iter().filter(|w| (w.end as f64) < t - 0.3).cloned().collect();
    let m = RateStats::at(&cell.get(), t, look.secs, &known, 0.0);
    let big = TextStyle::new(0, 34.0, 40.0);
    let small = TextStyle::new(0, 12.0, 15.0);
    let grey = Color::hex(0x9a9ea8, 1.0);
    let white = Color::hex(0xf2f2f2, 1.0);
    let stat = |key: &str, value: String, unit: &str| {
        Element::column(key.to_string())
            .child(Element::label("v", value).text_style(big).color(white))
            .child(Element::label("u", unit.to_string()).text_style(small).color(grey))
    };
    let wpm = m.wpm.map_or("-".to_string(), |w| format!("{w:.0}"));
    let w = size.w - 40.0;
    let view: Element = Element::panel("root")
        .size(size.w, size.h)
        .pad(20.0)
        .gap(12.0)
        .fill(Color::hex(0x16181d, 1.0))
        .radius(14.0)
        .child(
            Element::row("stats")
                .gap(24.0)
                .child(stat("syl", format!("{:.1}", m.syllables_per_sec), "syllables/s"))
                .child(stat("wpm", wpm, "wpm"))
                .child(rate_curve("curve", cell.clone(), look.clone()).size(w - 220.0, 60.0)),
        )
        .child(rate_ticks("ticks", cell.clone(), look.clone()).size(w, 44.0));
    let mut s: Scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    s.set_viewport(0.0, size, 1.0);
    s.apply(view, t);
    s
}

fn main() {
    let (out, path) = common::args("/var/tmp/claude/m4-dots/rate");
    let (frames, _) = common::load(path.as_ref());
    let words = common::load_words(path.as_ref(), &frames);
    let secs = frames.last().map_or(1.0, |f| f.clock);
    let cell = SignalsCell::new(Signals::recorded(frames, 0.0));
    let g = common::gpu();
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).expect("test font");
    let mut off = Offscreen::with_fonts(&g.device, &g.queue, fonts.clone());
    let (w, h) = (640u32, 200u32);
    let shots = [(RateLook::default(), 0.35), (RateLook::default(), 0.6), (RateLook::dots(), 0.9)];
    let mut imgs = Vec::new();
    for (look, k) in &shots {
        let t = k * secs;
        let m = RateStats::at(&cell.get(), t, look.secs, &words, 0.0);
        println!(
            "t {t:5.2} s: {:.2} syllables/s ({} in {:.2} s of speech), wpm {:?}",
            m.syllables_per_sec, m.syllables, m.speech_secs, m.wpm
        );
        let s = scene(&fonts, &cell, &words, look, t, Size::new(w as f32, h as f32));
        imgs.push(off.render_to_rgba(&s.sample(t), [w, h], 1.0));
    }
    let sheet = out.join("rate-sheet.png");
    common::sheet(&sheet, &imgs, w, h);
    println!("rate_demo: {}", sheet.display());
    let t0 = secs * 0.5;
    common::bench_with_fonts(
        &g,
        fonts.clone(),
        |_| {},
        |size| scene(&fonts, &cell, &words, &RateLook::default(), t0, size),
        t0,
    );
}
