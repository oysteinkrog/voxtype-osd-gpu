//! Goldens for the mic health meter (FX-118): seeded speech in the target zone, and a clipped
//! peak.
//!
//! GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use support::{Image, Tolerance};
use vo_audio::{mic_meter, MicLook};
use vo_core::signals::{Signals, SignalsCell};
use vo_render::Offscreen;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::Scene;

#[test]
fn mic_meter_goldens() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let mut images = Vec::new();
    for (name, clip) in [("mic-speech", false), ("mic-clipped", true)] {
        let mut frames = vo_dots::sample_speech(7, 6.0);
        if clip {
            frames[380].peak_dbfs = -0.3;
        }
        let cell = SignalsCell::new(Signals::recorded(frames, 0.0));
        let view: Element = Element::panel("root")
            .size(480.0, 140.0)
            .pad(20.0)
            .fill(Color::hex(0x1b1d22, 1.0))
            .child(mic_meter("mic", cell, MicLook::default()).size(440.0, 100.0));
        let mut scene: Scene = Scene::new();
        scene.set_viewport(0.0, Size::new(480.0, 140.0), 1.0);
        scene.apply(view, 0.0);
        let px = off.render_to_rgba(&scene.sample(4.0), [480, 140], 1.0);
        images.push(Image { name: name.into(), w: 480, h: 140, channels: 4, px });
    }
    let errors: Vec<String> = images.iter().filter_map(|i| support::check(i, Tolerance::default()).err()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
