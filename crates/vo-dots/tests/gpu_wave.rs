//! Goldens for the dot waveform, the dot level meter and the dot ticker.
//!
//! GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_core::hash::rand_f32;
use vo_dots::{DotFont, DotMeter, DotSource, DotTicker, DotWave, Dots};
use vo_render::Offscreen;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::Scene;

/// Seeded speech-like levels at 100 Hz: bursts with gaps.
fn levels(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let syllable = (i / 14) as u32;
            let phase = (i % 14) as f32 / 14.0;
            let amp = if rand_f32(1, syllable, 0, 0) < 0.2 { 0.0 } else { 0.25 + 0.7 * rand_f32(1, syllable, 1, 0) };
            amp * (std::f32::consts::PI * phase).sin()
        })
        .collect()
}

fn render(off: &mut Offscreen, view: Element, size: Size, t: f64, scale: f32) -> (u32, u32, Vec<u8>) {
    let mut scene: Scene = Scene::new();
    scene.set_viewport(0.0, size, scale);
    scene.apply(view, 0.0);
    let (w, h) = ((size.w * scale).ceil() as u32, (size.h * scale).ceil() as u32);
    (w, h, off.render_to_rgba(&scene.sample(t), [w, h], scale))
}

fn panel(child: Element) -> Element {
    Element::column("root").pad(6.0).fill(Color::hex(0x0b0b0d, 1.0)).child(child)
}

#[test]
fn dot_wave_meter_and_ticker_goldens() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let mut images = Vec::new();
    let lv = levels(300);
    let pitch = 5.0;
    let (cols, rows) = (100u32, 13u32);
    let wave_size = Size::new(cols as f32 * pitch + 12.0, rows as f32 * pitch + 12.0);
    let wave_el = |w: DotWave| {
        panel(Element::dots("w", Dots::new(Arc::new(w), pitch)).size(cols as f32 * pitch, rows as f32 * pitch))
    };

    // Speech, mid-way through opening from the centre, and while collapsing.
    let mut wave = DotWave::new(&lv, 100.0, 3.0, cols, rows);
    wave.column_secs = 0.03;
    wave.stops = vec![(0.0, Color::hex(0xffffff, 0.35)), (0.5, Color::WHITE)];
    for (name, t, open_at, collapse) in [
        ("dots-wave-speech", 3.0, f64::NEG_INFINITY, None),
        ("dots-wave-opening", 3.0, 2.85, None),
        ("dots-wave-collapsing", 3.1, f64::NEG_INFINITY, Some(3.0)),
    ] {
        let mut w = wave.clone();
        w.open_at = open_at;
        w.collapse_at = collapse;
        let (iw, ih, px) = render(&mut off, wave_el(w), wave_size, t, 1.0);
        images.push(Image { name: name.into(), w: iw, h: ih, channels: 4, px });
    }

    // A meter just after a peak: level falling, peak dot holding.
    let mut l2 = lv[..150].to_vec();
    l2.extend([1.0; 6]);
    l2.extend([0.1; 30]);
    let meter = DotMeter::new(&l2, 100.0, 1.85, 24);
    assert!(meter.peak(1.85) > meter.level(1.85) + 3.0);
    let meter_el = panel(Element::dots("m", Dots::new(Arc::new(meter), pitch)).size(24.0 * pitch, pitch));
    let (iw, ih, px) = render(&mut off, meter_el, Size::new(24.0 * pitch + 12.0, pitch + 12.0), 1.85, 1.0);
    images.push(Image { name: "dots-meter".into(), w: iw, h: ih, channels: 4, px });

    // A ticker, stepped, at two times a column apart, and at 1.5x.
    let ticker = DotTicker::new("PASTED INTO TERMINAL · ENTER SENT", DotFont::SMALL, 80, 0.0);
    let rows = ticker.rows() as f32;
    let size = Size::new(80.0 * 3.0 + 12.0, rows * 3.0 + 12.0);
    let el = panel(Element::dots("t", Dots::new(Arc::new(ticker), 3.0)).size(80.0 * 3.0, rows * 3.0));
    for (name, t, scale) in [("dots-ticker", 2.0, 1.0), ("dots-ticker-1.5x", 2.0, 1.5)] {
        let (iw, ih, px) = render(&mut off, el.clone(), size, t, scale);
        images.push(Image { name: name.into(), w: iw, h: ih, channels: 4, px });
    }

    let errors: Vec<String> = images.iter().filter_map(|i| support::check(i, Tolerance::default()).err()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// The wave keeps the scene awake while levels arrive and the release falls, then sleeps.
#[test]
fn wave_sleeps_after_the_release() {
    let w = DotWave::new(&levels(100), 100.0, 1.0, 40, 9);
    assert!(w.moving(1.5));
    assert!(!w.moving(2.1));
}
