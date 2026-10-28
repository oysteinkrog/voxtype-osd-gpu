//! Goldens for speech rate and rhythm (FX-119): the curve and ticks over seeded speech, in
//! the default and the dot-matrix look.
//!
//! GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use support::{Image, Tolerance};
use vo_audio::{rate_curve, rate_ticks, RateLook};
use vo_core::signals::{Signals, SignalsCell};
use vo_render::Offscreen;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::Scene;

#[test]
fn rate_goldens() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let cell = SignalsCell::new(Signals::recorded(vo_dots::sample_speech(7, 9.0), 0.0));
    let (w, h) = (480u32, 150u32);
    let mut images = Vec::new();
    for (name, look) in [("rate-lines", RateLook::default()), ("rate-dots", RateLook::dots())] {
        let view: Element = Element::panel("root")
            .size(w as f32, h as f32)
            .pad(16.0)
            .gap(10.0)
            .fill(Color::hex(0x16181d, 1.0))
            .child(rate_curve("curve", cell.clone(), look.clone()).size(448.0, 60.0))
            .child(rate_ticks("ticks", cell.clone(), look).size(448.0, 44.0));
        let mut scene: Scene = Scene::new();
        scene.set_viewport(0.0, Size::new(w as f32, h as f32), 1.0);
        scene.apply(view, 0.0);
        let px = off.render_to_rgba(&scene.sample(7.0), [w, h], 1.0);
        images.push(Image { name: name.into(), w, h, channels: 4, px });
    }
    let errors: Vec<String> = images.iter().filter_map(|i| support::check(i, Tolerance::default()).err()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
