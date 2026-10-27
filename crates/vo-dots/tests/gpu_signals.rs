//! Goldens for the signal bus examples: a voice orb whose scale, hop and colour follow level,
//! onsets and centroid, a mel spectrogram as dots and a pitch contour, all fed from seeded
//! speech features.
//!
//! GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use support::{Image, Tolerance};
use vo_core::signals::{Sig, Signals, SignalsCell};
use vo_dots::{sample_speech, signals::signals_panel};
use vo_render::Offscreen;
use vo_scene::geom::Size;
use vo_scene::Scene;

fn cell() -> SignalsCell {
    SignalsCell::new(Signals::recorded(sample_speech(7, 6.0), 0.0))
}

#[test]
fn signals_panel_goldens() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let size = Size::new(600.0, 300.0);
    let mut scene: Scene = Scene::new();
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(signals_panel(cell(), (size.w, size.h)), 0.0);
    let mut images = Vec::new();
    for (name, t) in [("dots-signals-speech", 2.4), ("dots-signals-tail", 6.3)] {
        let px = off.render_to_rgba(&scene.sample(t), [600, 300], 1.0);
        images.push(Image { name: name.into(), w: 600, h: 300, channels: 4, px });
    }
    let errors: Vec<String> = images.iter().filter_map(|i| support::check(i, Tolerance::default()).err()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// The bound orb grows with the voice and the scene sleeps once the stream has ended.
#[test]
fn orb_follows_the_level_and_the_scene_sleeps() {
    let mut scene: Scene = Scene::new();
    scene.set_viewport(0.0, Size::new(600.0, 300.0), 1.0);
    let cell = cell();
    scene.apply(signals_panel(cell.clone(), (600.0, 300.0)), 0.0);
    let orb = scene.find(&["signals", "top", "orb-box", "orb"]).unwrap();
    let scale = |t: f64| scene.sampled(orb, t).unwrap().transform.a;
    let signals = cell.get();
    let level = |t: f64| signals.sample(Sig::Level, t);
    let loudest = (100..500).map(|i| i as f64 * 0.01).max_by(|a, b| level(*a).total_cmp(&level(*b))).unwrap();
    assert!(scale(loudest + 0.05) > scale(0.1) + 0.1, "{} vs {}", scale(loudest + 0.05), scale(0.1));
    assert!(scene.next_wake(3.0).is_some());
    assert_eq!(scene.next_wake(12.0), None);
}
