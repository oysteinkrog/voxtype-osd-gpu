//! The mic health meter (FX-118) over a recording: a contact sheet at four times, the last one
//! with a clipped peak added, the stats printed for each, and the cost per frame with the meter
//! filling a 600 x 300 and a 1920 x 1080 surface.
//!
//!   cargo run --release -p vo-audio --example mic_demo -- [--frames FILE.json] [--out DIR]
//!
//! `--frames` is the same file `mel_demo` reads. Writes `DIR/mic-sheet.png`, DIR defaulting to
//! `/var/tmp/claude/m4-dots/mic`.

#[path = "common/mod.rs"]
mod common;

use vo_audio::{mic_meter, MicLook, MicStats};
use vo_core::signals::{Signals, SignalsCell};
use vo_render::Offscreen;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::Scene;

fn scene(cell: &SignalsCell, size: Size) -> Scene {
    let view: Element = Element::panel("root")
        .size(size.w, size.h)
        .pad(20.0)
        .fill(Color::hex(0x1b1d22, 1.0))
        .radius(14.0)
        .child(mic_meter("mic", cell.clone(), MicLook::default()).size(size.w - 40.0, size.h - 40.0));
    let mut s: Scene = Scene::new();
    s.set_viewport(0.0, size, 1.0);
    s.apply(view, 0.0);
    s
}

fn main() {
    let (out, frames) = common::args("/var/tmp/claude/m4-dots/mic");
    let (mut frames, _) = common::load(frames.as_ref());
    let secs = frames.last().map_or(1.0, |f| f.clock);
    // A clipped peak 85% of the way in, for the last picture.
    let clip_at = (frames.len() as f64 * 0.85) as usize;
    frames[clip_at].peak_dbfs = -0.2;
    let cell = SignalsCell::new(Signals::recorded(frames, 0.0));
    let g = common::gpu();
    let mut off = Offscreen::new(&g.device, &g.queue);
    let size = Size::new(600.0, 160.0);
    let s = scene(&cell, size);
    let times = [0.25, 0.45, 0.65, 0.86].map(|k| k * secs);
    let mut imgs = Vec::new();
    for t in times {
        let m = MicStats::at(&cell.get(), t, 4.0, &MicLook::default());
        println!(
            "t {t:5.2} s: floor {:.1} dB, speech {:.1} dB, SNR {:.1} dB, peak {:.1} dB, {:?}",
            m.noise_floor, m.speech, m.snr, m.peak_hold, m.verdict
        );
        imgs.push(off.render_to_rgba(&s.sample(t), [600, 160], 1.0));
    }
    let path = out.join("mic-sheet.png");
    common::sheet(&path, &imgs, 600, 160);
    println!("mic_demo: {} (at {times:.2?} s)", path.display());
    common::bench(&g, |_| {}, |size| scene(&cell, size), secs * 0.3);
}
