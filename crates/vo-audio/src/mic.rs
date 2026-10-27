//! A mic health meter (FX-118): the recent level as bars on a dBFS scale, a target zone, the
//! noise floor as a dashed line, a peak hold tick and a clip latch; and [`MicStats`] with the
//! numbers an app shows next to it (noise floor, speech level, SNR, a verdict).
//!
//! The meter is a canvas element that reads the signal bus, so it redraws while frames arrive
//! and stays still after. Labels (the dB scale, "SNR 28 dB") are ordinary label elements the
//! app lays out from [`MicStats`].

use vo_core::signals::{Field, Sig, Signals, SignalsCell, FRAME_SECS};
use vo_scene::canvas::Painter;
use vo_scene::display::{Paint, ShapeInstance};
use vo_scene::geom::{Color, Point, RRect, Rect};
use vo_scene::path::{Contour, StrokeStyle};
use vo_scene::{Element, Key};

/// A peak at or above this counts as clipping.
pub const CLIP_DBFS: f32 = -1.0;

/// How the meter looks.
#[derive(Clone, Debug)]
pub struct MicLook {
    /// Seconds of history across the bars.
    pub secs: f64,
    pub bars: u32,
    /// Bar width as a fraction of its slot.
    pub bar_fill: f32,
    /// The dBFS scale, bottom and top.
    pub scale: (f32, f32),
    /// The level speech should sit in.
    pub target: (f32, f32),
    pub idle: Color,
    pub good: Color,
    pub loud: Color,
    pub clip: Color,
    pub zone: Color,
    pub floor: Color,
    /// Seconds a peak holds before the tick falls, and how long a clip stays lit.
    pub hold: f64,
}

impl Default for MicLook {
    fn default() -> MicLook {
        MicLook {
            secs: 4.0,
            bars: 48,
            bar_fill: 0.62,
            scale: (-72.0, 0.0),
            target: (-45.0, -18.0),
            idle: Color::hex(0x5a5f69, 0.9),
            good: Color::hex(0xffb340, 1.0),
            loud: Color::hex(0xff8a3d, 1.0),
            clip: Color::hex(0xff453a, 1.0),
            zone: Color::hex(0xffb340, 0.08),
            floor: Color::hex(0xffb340, 0.7),
            hold: 1.5,
        }
    }
}

/// What the level says about the mic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Not enough audio in the window yet.
    Unknown,
    /// Speech below the target zone.
    Quiet,
    Good,
    /// Speech above the target zone.
    Loud,
    /// A peak reached the clip level within the hold time.
    Clipping,
    /// Speech less than 10 dB over the noise floor.
    Noisy,
}

/// The mic's numbers over the last few seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MicStats {
    /// 10th percentile of the RMS level, dBFS.
    pub noise_floor: f32,
    /// 90th percentile of the RMS level, dBFS.
    pub speech: f32,
    pub snr: f32,
    /// The highest peak in the hold time, dBFS.
    pub peak_hold: f32,
    pub clipped: bool,
    pub verdict: Verdict,
}

impl MicStats {
    /// The numbers at `t` over the last `window` seconds of frames, judged against `look`.
    pub fn at(signals: &Signals, t: f64, window: f64, look: &MicLook) -> MicStats {
        let s = t - signals.playout();
        let recent: Vec<_> = signals.frames().filter(|(at, _)| *at <= s && *at > s - window).collect();
        let mut rms: Vec<f32> = recent.iter().map(|(_, f)| f.rms_dbfs).collect();
        rms.sort_by(f32::total_cmp);
        let pct = |p: f32| rms.get(((rms.len() as f32 - 1.0) * p).round() as usize).copied().unwrap_or(-100.0);
        let (noise_floor, speech) = (pct(0.1), pct(0.9));
        let held = recent.iter().filter(|(at, _)| *at > s - look.hold);
        let peak_hold = held.clone().map(|(_, f)| f.peak_dbfs).fold(-100.0, f32::max);
        let clipped = peak_hold >= CLIP_DBFS;
        let snr = speech - noise_floor;
        let verdict = if rms.len() < 50 {
            Verdict::Unknown
        } else if clipped {
            Verdict::Clipping
        } else if snr < 10.0 {
            Verdict::Noisy
        } else if speech < look.target.0 {
            Verdict::Quiet
        } else if speech > look.target.1 {
            Verdict::Loud
        } else {
            Verdict::Good
        };
        MicStats { noise_floor, speech, snr, peak_hold, clipped, verdict }
    }
}

/// The meter across the element's box. Give it a size.
pub fn mic_meter<M: 'static>(key: impl Into<Key>, cell: SignalsCell, look: MicLook) -> Element<M> {
    let tail = look.secs.max(look.hold) as f32;
    Element::canvas(key, 0.0, move |t, rect, painter| draw(&cell, &look, t, rect, painter)).wake_on_signals(tail)
}

/// The loudest RMS level in each of `bars` slices of the last `secs`, oldest first. Slices sit
/// on a fixed grid of whole frames, so a bar keeps its value as the history scrolls; the newest
/// bar fills as its frames arrive.
fn bar_levels(signals: &Signals, t: f64, secs: f64, bars: usize) -> Vec<f32> {
    let per = ((secs / FRAME_SECS) / bars as f64).round().max(1.0) as usize;
    let (hist, newest_at) = signals.history(Sig::Raw(Field::RmsDb), t, 1.0 / FRAME_SECS, (bars + 1) * per);
    let newest = ((newest_at - signals.playout()) / FRAME_SECS).round() as i64;
    let partial = match (newest + 1).rem_euclid(per as i64) as usize {
        0 => per,
        k => k,
    };
    let mut end = hist.len();
    let mut out: Vec<f32> = (0..bars)
        .map(|b| {
            let start = end - if b == 0 { partial } else { per };
            let v = hist[start..end].iter().copied().fold(-100.0, f32::max);
            end = start;
            v
        })
        .collect();
    out.reverse();
    out
}

fn draw(cell: &SignalsCell, look: &MicLook, t: f64, rect: Rect, p: &mut Painter) {
    let signals = cell.get();
    let (lo, hi) = look.scale;
    let y = |db: f32| rect.y + rect.h * (1.0 - ((db - lo) / (hi - lo)).clamp(0.0, 1.0));
    let fill = |r: Rect, c: Color| ShapeInstance::rrect(RRect::new(r, 0.0), Paint::solid(c));
    // Target zone.
    let (z0, z1) = (y(look.target.1), y(look.target.0));
    p.shape(fill(Rect::new(rect.x, z0, rect.w, z1 - z0), look.zone));
    // Bars, the last slot kept for the clip latch.
    let n = look.bars.max(2) as usize;
    let slot = rect.w / n as f32;
    let levels = bar_levels(&signals, t, look.secs, n - 1);
    for (i, &db) in levels.iter().enumerate() {
        let colour = if db > look.target.1 {
            look.loud
        } else if db >= look.target.0 {
            look.good
        } else {
            look.idle
        };
        let w = slot * look.bar_fill;
        let top = y(db.max(lo));
        if rect.y + rect.h - top >= 0.5 {
            p.shape(fill(
                Rect::new(rect.x + i as f32 * slot + 0.5 * (slot - w), top, w, rect.y + rect.h - top),
                colour,
            ));
        }
    }
    let stats = MicStats::at(&signals, t, look.secs, look);
    // Clip latch.
    if stats.clipped {
        let w = slot * look.bar_fill;
        let x = rect.x + (n - 1) as f32 * slot + 0.5 * (slot - w);
        p.shape(fill(Rect::new(x, rect.y, w, rect.h), look.clip));
    }
    // Noise floor.
    if stats.verdict != Verdict::Unknown {
        let fy = y(stats.noise_floor);
        let line = Contour::polyline(&[Point::new(rect.x, fy), Point::new(rect.x + rect.w, fy)], false);
        for dash in line.dash(&[4.0, 3.0], 0.0) {
            p.stroke(&dash, &StrokeStyle::solid(1.0, look.floor));
        }
    }
    // Peak hold over the newest three bars.
    if stats.peak_hold > lo {
        let py = y(stats.peak_hold);
        let x1 = rect.x + (n - 1) as f32 * slot;
        let tick = Contour::polyline(&[Point::new(x1 - 3.0 * slot, py), Point::new(x1, py)], false);
        p.stroke(&tick, &StrokeStyle::solid(1.5, Color::hex(0xe8e8ea, 0.9)));
    }
}

#[cfg(test)]
mod tests {
    use vo_core::signals::SignalFrame;

    use super::*;

    fn signals(db: impl Fn(usize) -> f32, peak: f32) -> Signals {
        let frames = (0..400).map(|i| SignalFrame {
            rms_dbfs: db(i),
            peak_dbfs: if i == 390 { peak } else { db(i) + 6.0 },
            ..SignalFrame::silent(i as f64 * FRAME_SECS)
        });
        Signals::recorded(frames, 0.0).with_playout(0.0)
    }

    #[test]
    fn floor_speech_and_snr() {
        // A third quiet room, the rest speech at -20 dBFS.
        let s = signals(|i| if i % 3 == 0 { -62.0 } else { -20.0 }, -8.0);
        let m = MicStats::at(&s, 3.99, 4.0, &MicLook::default());
        assert_eq!((m.noise_floor, m.speech), (-62.0, -20.0));
        assert_eq!(m.snr, 42.0);
        assert_eq!(m.verdict, Verdict::Good);
        assert!(!m.clipped);
    }

    #[test]
    fn verdicts() {
        let look = MicLook::default();
        let at = |s: &Signals| MicStats::at(s, 3.99, 4.0, &look).verdict;
        assert_eq!(at(&signals(|i| if i % 3 == 0 { -70.0 } else { -50.0 }, -40.0)), Verdict::Quiet);
        assert_eq!(at(&signals(|i| if i % 3 == 0 { -62.0 } else { -9.0 }, -4.0)), Verdict::Loud);
        assert_eq!(at(&signals(|_| -45.0, -0.5)), Verdict::Clipping);
        assert_eq!(at(&signals(|i| if i % 3 == 0 { -40.0 } else { -33.0 }, -30.0)), Verdict::Noisy);
        let short = Signals::recorded((0..10).map(|i| SignalFrame::silent(i as f64 * FRAME_SECS)), 0.0);
        assert_eq!(MicStats::at(&short, 0.1, 4.0, &look).verdict, Verdict::Unknown);
    }

    #[test]
    fn bars_keep_their_value_as_the_history_scrolls() {
        // The level rises 0.1 dB per frame.
        let s = signals(|i| -80.0 + 0.1 * i as f32, -30.0);
        let a = bar_levels(&s, 3.0, 1.0, 10);
        let b = bar_levels(&s, 3.1, 1.0, 10);
        assert_eq!(a.len(), 10);
        // Slices are 10 frames on a fixed grid: frames 290 to 299 are bar 8 at 3.0 s and bar 7
        // one slice later.
        assert_eq!(a[8], b[7], "{a:?} {b:?}");
        assert!(a.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn clip_shows_from_the_peak_on() {
        let s = signals(|_| -20.0, 0.0);
        let look = MicLook::default();
        assert!(MicStats::at(&s, 3.95, 4.0, &look).clipped);
        assert!(!MicStats::at(&s, 3.89, 4.0, &look).clipped);
    }
}
