//! Goldens for the mel spectrogram texture (FX-114): seeded speech, seams at word edges, the
//! amber and ice colour maps.
//!
//! GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_audio::{ColorMap, MelTexture};
use vo_core::signals::{Sig, Signals, SignalsCell, FRAME_SECS};
use vo_render::Offscreen;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::Scene;

/// Times where the voice starts or stops for at least 60 ms: word edges, near enough.
fn word_edges(s: &Signals, until: f64) -> Vec<f64> {
    let (mut edges, mut on, mut quiet) = (Vec::new(), false, 0.0);
    let mut t = 0.0;
    while t < until {
        let loud = s.sample(Sig::Level, t + s.playout()) > 0.3;
        if loud && !on {
            edges.push(t);
            on = true;
        } else if !loud && on {
            quiet += FRAME_SECS;
            if quiet >= 0.06 {
                edges.push(t - quiet);
                on = false;
            }
        }
        if loud {
            quiet = 0.0;
        }
        t += FRAME_SECS;
    }
    edges
}

#[test]
fn mel_texture_goldens() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    vo_audio::install(&mut off.renderer);
    let signals = Signals::recorded(vo_dots::sample_speech(7, 6.0), 0.0);
    let edges = word_edges(&signals, 6.0);
    let cell = SignalsCell::new(signals);
    let (w, h) = (600.0, 140.0);
    let mut images = Vec::new();
    for (name, map, t) in [("mel-amber", ColorMap::AMBER, 4.0), ("mel-ice-tail", ColorMap::ICE, 6.6)] {
        let mel = Arc::new(MelTexture::new(cell.clone(), 4.0).map(map).seams(edges.clone()));
        let view: Element = Element::panel("root")
            .size(w, h)
            .pad(20.0)
            .fill(Color::hex(0x14171c, 1.0))
            .child(Element::gpu("mel", mel).size(w - 40.0, h - 40.0));
        let mut scene: Scene = Scene::new();
        scene.set_viewport(0.0, Size::new(w, h), 1.0);
        scene.apply(view, 0.0);
        let px = off.render_to_rgba(&scene.sample(t), [w as u32, h as u32], 1.0);
        images.push(Image { name: name.into(), w: w as u32, h: h as u32, channels: 4, px });
    }
    let errors: Vec<String> = images.iter().filter_map(|i| support::check(i, Tolerance::default()).err()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
