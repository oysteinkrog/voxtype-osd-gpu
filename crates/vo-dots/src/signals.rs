//! Audio visuals driven by the signal bus (`vo_core::signals`): a mel spectrogram as a dot
//! field, and a pitch contour stroke.
//!
//! Both read a [`SignalsCell`] the host pushes frames into, and both are pure in `t`: the
//! columns sit on a fixed grid of absolute time, so a frame drawn twice at one `t` is the same.

use vo_core::hash::rand_f32;
use vo_core::signals::{Sig, SignalFrame, SignalsCell, FRAME_SECS, MEL_BANDS};
use vo_scene::canvas::Painter;
use vo_scene::dots::{Dot, DotSource};
use vo_scene::geom::{Color, Point, Rect};
use vo_scene::path::{Contour, StrokeStyle};
use vo_scene::{Element, Key};

use crate::wave::ramp;

/// A scrolling mel spectrogram: one column per `step` seconds, newest on the right, 24 rows
/// with the lowest band at the bottom. A dot's size and colour follow its band's energy.
#[derive(Debug, Clone)]
pub struct MelField {
    pub cell: SignalsCell,
    pub cols: u32,
    /// Seconds per column. Columns move in whole steps, so resting dots sit on whole pixels.
    pub step: f64,
    /// Band energy (0 is -100 dB, 1 is 0 dB) drawn as nothing and as full size.
    pub range: (f32, f32),
    /// Dot diameter in pitches at no energy and at full energy.
    pub size: (f32, f32),
    /// Colour by energy, 0 to 1.
    pub stops: Vec<(f32, Color)>,
}

impl MelField {
    pub fn new(cell: SignalsCell, cols: u32) -> MelField {
        MelField {
            cell,
            cols,
            step: 0.03,
            range: (0.35, 0.8),
            size: (0.18, 0.92),
            stops: vec![
                (0.0, Color::rgba(0.10, 0.18, 0.30, 0.55)),
                (0.5, Color::rgba(0.20, 0.75, 0.95, 1.0)),
                (1.0, Color::rgba(1.0, 0.95, 0.80, 1.0)),
            ],
        }
    }

    /// The grid this draws on: `cols` by 24.
    pub fn cells(&self) -> (u32, u32) {
        (self.cols, MEL_BANDS as u32)
    }

    fn energy(&self, v: f32) -> f32 {
        let (lo, hi) = self.range;
        ((v - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0)
    }
}

impl DotSource for MelField {
    fn dots(&self, t: f64, out: &mut Vec<Dot>) {
        let signals = self.cell.get();
        let newest = (t / self.step).floor();
        let fade = signals.presence(t);
        for c in 0..self.cols {
            let at = (newest - c as f64) * self.step;
            let x = (self.cols - 1 - c) as f32;
            for b in 0..MEL_BANDS {
                let e = self.energy(signals.sample(Sig::Band(b as u8), at)) * fade.max(0.0);
                let mut color = ramp(&self.stops, e);
                color.a *= 0.35 + 0.65 * e;
                out.push(Dot {
                    x,
                    y: (MEL_BANDS - 1 - b) as f32,
                    size: self.size.0 + (self.size.1 - self.size.0) * e,
                    color,
                    snap: 1.0,
                });
            }
        }
    }

    fn moving(&self, t: f64) -> bool {
        // The last column scrolls off `cols` steps after the last frame.
        self.cell.get().active_until(self.cols as f64 * self.step).is_some_and(|until| t < until)
    }
}

/// How a pitch contour looks.
#[derive(Clone, Debug)]
pub struct ContourLook {
    /// Seconds of history across the box.
    pub secs: f64,
    pub style: StrokeStyle,
    /// The normalised pitch (`Sig::Pitch`, 0 at 60 Hz, 1 at 1000 Hz) at the bottom and top
    /// of the box. `(0.2, 0.6)`, about 105 to 310 Hz, suits most voices.
    pub range: (f32, f32),
}

impl ContourLook {
    pub fn new(secs: f64, style: StrokeStyle) -> ContourLook {
        ContourLook { secs, style, range: (0.2, 0.6) }
    }
}

/// A pitch contour across the box: the last `look.secs` seconds of voiced pitch, newest on the
/// right, low pitch at the bottom (log scale, 60 to 1000 Hz). Unvoiced stretches leave gaps.
/// The element wakes the scene while frames arrive, so the host must also hand the scene the
/// store (`Scene::set_signals`).
pub fn pitch_contour<M: 'static>(key: impl Into<Key>, cell: SignalsCell, look: ContourLook) -> Element<M> {
    let tail = look.secs;
    Element::canvas(key, 0.0, move |t, rect, painter| draw_contour(&cell, &look, t, rect, painter))
        .wake_on_signals(tail as f32)
}

/// The points of each voiced run at `t`, in the box.
pub fn contour_runs(cell: &SignalsCell, look: &ContourLook, t: f64, rect: Rect) -> Vec<Vec<Point>> {
    let secs = look.secs;
    let (lo, hi) = look.range;
    let signals = cell.get();
    let step = FRAME_SECS;
    let n = (secs / step).round().max(1.0) as i64;
    let newest = (t / step).floor() as i64;
    let mut runs = Vec::new();
    let mut run: Vec<Point> = Vec::new();
    for i in 0..=n {
        let at = (newest - n + i) as f64 * step;
        if signals.sample(Sig::Voiced, at) >= 0.5 {
            let p = ((signals.sample(Sig::Pitch, at) - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
            run.push(Point { x: rect.x + rect.w * i as f32 / n as f32, y: rect.y + rect.h * (1.0 - p) });
        } else if !run.is_empty() {
            runs.push(std::mem::take(&mut run));
        }
    }
    if !run.is_empty() {
        runs.push(run);
    }
    runs.retain(|r| r.len() >= 2);
    runs
}

fn draw_contour(cell: &SignalsCell, look: &ContourLook, t: f64, rect: Rect, painter: &mut Painter) {
    let fade = cell.get().presence(t);
    if fade <= 0.0 {
        return;
    }
    let mut style = look.style;
    style.c0.a *= fade;
    style.c1.a *= fade;
    for run in contour_runs(cell, look, t, rect) {
        painter.stroke(&Contour::polyline(&run, false), &style);
    }
}

/// A sample panel that shows the signal bus at work: a voice orb whose scale, hop and colour
/// are bound to level, onsets and centroid; a mel spectrogram as dots; and the pitch contour.
/// `size` is the panel's logical size; the example and the golden use 600 by 300.
pub fn signals_panel<M: 'static>(cell: SignalsCell, size: (f32, f32)) -> Element<M> {
    use vo_core::signals::Bind;
    use vo_scene::display::ShapeKind;
    use vo_scene::{Param, Prop};

    let (w, h) = size;
    let pad = 16.0;
    let pitch = 5.0;
    let orb = (h - 2.0 * pad) * 0.42;
    let cols = (((w - 3.0 * pad - orb * 1.6) / pitch).floor() as u32).max(8);
    let mel = MelField::new(cell.clone(), cols);
    let cyan = Color::hex(0x5ad1ff, 1.0);
    let warm = Color::hex(0xffb35a, 1.0);
    let orb_el = Element::shape("orb", ShapeKind::Circle)
        .size(orb, orb)
        .fill(cyan)
        .bind(Param::Scale, Bind::new(Sig::Level).input(0.2, 0.9).output(0.8, 1.25).smooth(0.015, 0.18))
        .bind(Param::OffsetY, Bind::new(Sig::OnsetPulse { tau: 0.12 }).output(0.0, -6.0).add())
        .bind_color(Prop::Fill, warm, Bind::new(Sig::Centroid).input(0.45, 0.8).smooth(0.05, 0.3));
    let orb_box = Element::column("orb-box")
        .size(orb * 1.6, MEL_BANDS as f32 * pitch)
        .align_items(vo_scene::taffy::prelude::AlignItems::CENTER)
        .pad_edges(((MEL_BANDS as f32 * pitch - orb) / 2.0).max(0.0), 0.0, 0.0, 0.0)
        .child(orb_el);
    let field = Element::dots("mel", vo_scene::dots::Dots::new(std::sync::Arc::new(mel), pitch))
        .size(cols as f32 * pitch, MEL_BANDS as f32 * pitch);
    let look = ContourLook::new(3.0, StrokeStyle::solid(2.0, cyan).glow(6.0));
    let contour =
        pitch_contour("pitch", cell.clone(), look).size(w - 2.0 * pad, h - 3.0 * pad - MEL_BANDS as f32 * pitch);
    Element::panel("signals")
        .signals(cell)
        .size(w, h)
        .pad(pad)
        .gap(pad)
        .fill(Color::hex(0x08090c, 1.0))
        .radius(18.0)
        .child(Element::row("top").gap(pad).child(orb_box).child(field))
        .child(contour)
}

/// Seeded speech-like feature frames for demos and goldens: `secs` seconds at 100 Hz from
/// clock 0. Words of two to four syllables, pauses between phrases, a falling pitch line per
/// phrase, two formant peaks that move each syllable, and an `s`-like hiss now and then (unvoiced, high
/// bands, flat spectrum). Onsets mark each syllable's start.
pub fn sample_speech(seed: u32, secs: f64) -> Vec<SignalFrame> {
    let n = (secs / FRAME_SECS).round() as usize;
    let mut frames: Vec<SignalFrame> = (0..n).map(|i| SignalFrame::silent(i as f64 * FRAME_SECS)).collect();
    // Room noise.
    for (i, f) in frames.iter_mut().enumerate() {
        let floor = -62.0 + 4.0 * rand_f32(seed, i as u32, 9, 0);
        f.rms_dbfs = floor;
        f.peak_dbfs = floor + 6.0;
        f.flatness = 0.8;
        f.centroid_hz = 2500.0;
        for (b, m) in f.mel.iter_mut().enumerate() {
            *m = ((floor - 8.0 + 3.0 * rand_f32(seed, i as u32, b as u32, 1)) + 100.0) / 100.0;
        }
    }
    let r = |k: u32, j: u32| rand_f32(seed, k, j, 2);
    let (mut at, mut k, mut phrase_start) = (0.25, 0u32, 0.25);
    while at < secs - 0.6 {
        // A word: two to four syllables back to back, the level dipping between them.
        let syllables = 2 + (3.0 * r(k, 8)) as u32;
        for syl in 0..syllables {
            let len = 0.11 + 0.09 * r(k, 0) as f64;
            let amp = 0.6 + 0.4 * r(k, 1);
            let hiss = r(k, 2) < 0.12;
            let f1 = 3.0 + 4.0 * r(k, 3);
            let f2 = 9.0 + 6.0 * r(k, 4);
            let fall = 1.0 - 0.3 * ((at - phrase_start) / 2.5).min(1.0) as f32;
            let base = (190.0 + 30.0 * r(k, 5)) * fall;
            let first = (at / FRAME_SECS).round() as usize;
            let last = (((at + len) / FRAME_SECS).round() as usize).min(n);
            for (j, f) in frames[first..last].iter_mut().enumerate() {
                let x = j as f32 / (last - first).max(1) as f32;
                let mut env = (std::f32::consts::PI * x).sin().powf(0.6);
                // Inside the word the level only dips at a syllable join.
                if (syl > 0 && x < 0.5) || (syl + 1 < syllables && x >= 0.5) {
                    env = env.max(0.45);
                }
                let env = amp * env;
                let db = -60.0 + 54.0 * env;
                if db <= f.rms_dbfs {
                    continue;
                }
                f.rms_dbfs = db;
                f.peak_dbfs = (db + 7.0).min(0.0);
                f.onset = if j == 0 { 0.5 + 0.5 * amp } else { 0.0 };
                if hiss {
                    f.voiced = false;
                    f.flatness = 0.7;
                    f.centroid_hz = 5500.0;
                } else {
                    f.voiced = env > 0.12;
                    f.f0_hz = if f.voiced { base * (1.0 + 0.1 * (std::f32::consts::PI * x).sin()) } else { 0.0 };
                    f.flatness = 0.1;
                    f.centroid_hz = 500.0 + 120.0 * f2;
                }
                for (b, m) in f.mel.iter_mut().enumerate() {
                    let b = b as f32;
                    let shape = if hiss {
                        -30.0 * ((22.0 - b) / 8.0).max(0.0).powi(2).min(1.5)
                    } else {
                        let peak = |c: f32, w: f32| (-((b - c) / w).powi(2)).exp();
                        -36.0 + 36.0 * (0.9 * peak(1.5, 1.2) + peak(f1, 1.5) + 0.7 * peak(f2, 2.0)).min(1.0)
                    };
                    *m = m.max(((db + shape) + 100.0) / 100.0);
                }
            }
            at += len;
            k += 1;
        }
        at += 0.06 + 0.08 * r(k, 6) as f64;
        if r(k, 7) < 0.2 {
            at += 0.35;
            phrase_start = at;
        }
    }
    frames
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use vo_core::signals::Signals;

    use super::*;

    /// One second: band 3 loud, the rest quiet; voiced from 0.2 to 0.6 s at 220 Hz.
    fn cell() -> SignalsCell {
        let frames = (0..100).map(|i| {
            let mut mel = [0.2; MEL_BANDS];
            mel[3] = 0.8;
            let voiced = (20..60).contains(&i);
            SignalFrame {
                mel,
                voiced,
                f0_hz: if voiced { 220.0 } else { 0.0 },
                ..SignalFrame::silent(i as f64 * FRAME_SECS)
            }
        });
        let c = SignalsCell::default();
        c.set(Arc::new(Signals::recorded(frames, 0.0).with_playout(0.0)));
        c
    }

    #[test]
    fn mel_field_lights_the_loud_band_and_stops_after_the_tail() {
        let f = MelField::new(cell(), 10);
        let mut dots = Vec::new();
        f.dots(0.5, &mut dots);
        assert_eq!(dots.len(), 10 * MEL_BANDS);
        let row = |b: usize| dots.iter().find(|d| d.x == 9.0 && d.y == (MEL_BANDS - 1 - b) as f32).unwrap();
        assert_eq!(row(3).size, f.size.1);
        assert_eq!(row(0).size, f.size.0);
        assert!(f.moving(0.5));
        assert!(!f.moving(5.0));
        // The same t gives the same dots.
        let mut again = Vec::new();
        f.dots(0.5, &mut again);
        assert_eq!(dots, again);
    }

    #[test]
    fn contour_has_one_run_at_the_voiced_pitch() {
        let rect = Rect { x: 0.0, y: 0.0, w: 100.0, h: 50.0 };
        let look = ContourLook { range: (0.0, 1.0), ..ContourLook::new(1.0, StrokeStyle::solid(1.0, Color::WHITE)) };
        let runs = contour_runs(&cell(), &look, 0.99, rect);
        assert_eq!(runs.len(), 1);
        let p = (220f32 / 60.0).ln() / (1000f32 / 60.0).ln();
        assert!(runs[0].iter().all(|q| (q.y - 50.0 * (1.0 - p)).abs() < 1e-3));
        // 0.2 to 0.59 s of a 1 s window ending at 0.99 s.
        assert!((runs[0][0].x - 21.0).abs() < 1.01, "{}", runs[0][0].x);
    }
}
