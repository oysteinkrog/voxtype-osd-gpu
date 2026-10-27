//! Goldens for words on a time axis (FX-121): seeded speech with stand-in word times, the
//! default look while live and the HUD look with its ruler after the recording.
//!
//! GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use support::{Image, Tolerance};
use vo_audio::{level_spans, word_axis, Axis, AxisLook, TimedWord};
use vo_core::signals::{Signals, SignalsCell};
use vo_render::Offscreen;
use vo_scene::element::Element;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

#[test]
fn word_axis_goldens() {
    let gpu = support::gpu();
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).expect("test font");
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let frames = vo_dots::sample_speech(7, 6.0);
    let say = ["Book", "the", "train", "to", "Bergen", "for", "Thursday", "morning,", "and", "pack"];
    let words: Vec<TimedWord> = level_spans(&frames, -45.0, 0.06)
        .into_iter()
        .enumerate()
        .map(|(i, (s, e))| TimedWord { id: i as u32, text: say[i % say.len()].into(), start: s as f32, end: e as f32 })
        .collect();
    let cell = SignalsCell::new(Signals::recorded(frames, 0.0));
    let (w, h) = (520u32, 220u32);
    let mut images = Vec::new();
    for (name, look, t, live) in
        [("axis-live", AxisLook::default(), 4.0, true), ("axis-hud-done", AxisLook::hud(), 6.5, false)]
    {
        let known: Vec<TimedWord> = words.iter().filter(|w| !live || (w.end as f64) < t - 0.3).cloned().collect();
        let axis = Axis { words: known, origin: 0.0, live, until: t as f32 };
        let view: Element = Element::panel("root")
            .size(w as f32, h as f32)
            .pad(16.0)
            .fill(Color::hex(0x16181d, 1.0))
            .child(word_axis("axis", cell.clone(), &axis, &look, w as f32 - 32.0));
        let mut scene: Scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
        scene.set_viewport(0.0, Size::new(w as f32, h as f32), 1.0);
        scene.apply(view, t);
        let px = off.render_to_rgba(&scene.sample(t), [w, h], 1.0);
        images.push(Image { name: name.into(), w, h, channels: 4, px });
    }
    let errors: Vec<String> = images.iter().filter_map(|i| support::check(i, Tolerance::default()).err()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
