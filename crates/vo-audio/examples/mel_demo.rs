//! The mel spectrogram texture (FX-114) over a recording: a contact sheet in four colour
//! settings, and the cost per frame with the texture filling a 600 x 300 and a 1920 x 1080
//! surface.
//!
//!   cargo run --release -p vo-audio --example mel_demo -- [--frames FILE.json] [--out DIR]
//!
//! `--frames` reads `{"frames": [SignalFrame, ...], "seams": [seconds, ...]}`; without it the
//! demo uses seeded speech (`vo_dots::sample_speech`) with no seams. Writes `DIR/mel-sheet.png`,
//! DIR defaulting to `/var/tmp/claude/m4-dots/mel`.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use vo_audio::{ColorMap, MelTexture};
use vo_core::signals::{Signals, SignalsCell};
use vo_render::Offscreen;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::Scene;

fn scene(mel: MelTexture, size: Size) -> Scene {
    let view: Element = Element::panel("root")
        .size(size.w, size.h)
        .pad(16.0)
        .fill(Color::hex(0x14171c, 1.0))
        .radius(14.0)
        .child(Element::gpu("mel", Arc::new(mel)).size(size.w - 32.0, size.h - 32.0));
    let mut s: Scene = Scene::new();
    s.set_viewport(0.0, size, 1.0);
    s.apply(view, 0.0);
    s
}

fn main() {
    let (out, frames) = common::args("/var/tmp/claude/m4-dots/mel");
    let (frames, seams) = common::load(frames.as_ref());
    let secs = frames.last().map_or(1.0, |f| f.clock);
    let cell = SignalsCell::new(Signals::recorded(frames, 0.0));
    let g = common::gpu();

    let mut off = Offscreen::new(&g.device, &g.queue);
    vo_audio::install(&mut off.renderer);
    let size = Size::new(900.0, 150.0);
    let t = secs * 0.85;
    let looks = [
        MelTexture::new(cell.clone(), 6.0).seams(seams.clone()),
        MelTexture { grain: 0.0, ..MelTexture::new(cell.clone(), 6.0).map(ColorMap::MAGMA) },
        MelTexture { range: (0.2, 0.9), ..MelTexture::new(cell.clone(), 6.0).map(ColorMap::ICE) },
        MelTexture::new(cell.clone(), 2.0).seams(seams.clone()),
    ];
    let imgs: Vec<_> =
        looks.into_iter().map(|m| off.render_to_rgba(&scene(m, size).sample(t), [900, 150], 1.0)).collect();
    let path = out.join("mel-sheet.png");
    common::sheet(&path, &imgs, 900, 150);
    println!(
        "mel_demo: {} (amber, magma without grain, ice with a wide range, amber over 2 s; t = {t:.2} s)",
        path.display()
    );
    common::bench(
        &g,
        vo_audio::install,
        |size| scene(MelTexture::new(cell.clone(), 6.0).seams(seams.clone()), size),
        secs * 0.3,
    );
}
