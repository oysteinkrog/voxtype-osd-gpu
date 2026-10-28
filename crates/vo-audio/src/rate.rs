//! Speech rate and rhythm (FX-119): syllable ticks, a smooth rate curve with a faint fill, and
//! [`RateStats`] with syllables per second and words per minute for the big numbers.
//!
//! Syllables come from the signal bus, not the engine: a syllable nucleus is a peak in the
//! 300 to 2500 Hz mel energy that stands at least 4 dB above the dips around it, at least
//! 90 ms from a louder one, in speech ([`nuclei`]). The rate is the articulation rate:
//! syllables per second of speech, so pauses do not pull it down.
//!
//! Both views scroll over the last [`RateLook::secs`] up to the audio heard now, and stay still
//! when the audio stops. Give the bus that much history plus a second
//! (`Signals::with_cap`).

use vo_core::signals::{SignalFrame, Signals, SignalsCell, FRAME_SECS};
use vo_scene::canvas::Painter;
use vo_scene::display::{Paint, ShapeInstance};
use vo_scene::geom::{Color, Point, RRect, Rect};
use vo_scene::path::{Contour, StrokeStyle};
use vo_scene::{Element, Key};

use crate::word_axis::TimedWord;

/// Mel bands 2 to 12 cover about 300 to 2300 Hz, where vowels carry their energy.
const VOWEL_BANDS: std::ops::RangeInclusive<usize> = 2..=12;
/// How far a nucleus must rise over the dips on both sides, dB.
const PROMINENCE: f32 = 4.0;
/// Closest two nuclei can be, in frames (90 ms).
const MIN_FRAMES: usize = 9;
/// A frame is speech when voiced or this far over the noise floor, dB.
const SPEECH_OVER_FLOOR: f32 = 12.0;
/// Width of the rate curve's smoothing, seconds.
const SIGMA: f64 = 0.45;

/// A syllable nucleus: when, and how loud its vowel band was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Nucleus {
    pub clock: f64,
    pub db: f32,
}

/// Energy in the vowel bands, dB.
fn vowel_db(f: &SignalFrame) -> f32 {
    let power: f32 = f.mel[VOWEL_BANDS].iter().map(|&m| 10f32.powf((m * 100.0 - 100.0) / 10.0)).sum();
    10.0 * power.max(1e-12).log10()
}

/// The 10th percentile of the RMS level, dBFS.
fn floor(frames: &[SignalFrame]) -> f32 {
    let mut v: Vec<f32> = frames.iter().map(|f| f.rms_dbfs).collect();
    if v.is_empty() {
        return -100.0;
    }
    v.sort_by(f32::total_cmp);
    v[v.len() / 10]
}

fn is_speech(f: &SignalFrame, floor: f32) -> bool {
    f.voiced || f.rms_dbfs > floor + SPEECH_OVER_FLOOR
}

/// The syllable nuclei in `frames` (consecutive, oldest first), oldest first.
pub fn nuclei(frames: &[SignalFrame]) -> Vec<Nucleus> {
    let n = frames.len();
    if n < 3 {
        return Vec::new();
    }
    let raw: Vec<f32> = frames.iter().map(vowel_db).collect();
    // Five-frame mean.
    let s: Vec<f32> = (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(2), (i + 3).min(n));
            raw[a..b].iter().sum::<f32>() / (b - a) as f32
        })
        .collect();
    let floor = floor(frames);
    let mut peaks: Vec<(usize, f32)> = (1..n - 1)
        .filter(|&i| s[i] > s[i - 1] && s[i] >= s[i + 1] && is_speech(&frames[i], floor))
        .filter(|&i| {
            // The lowest point between the peak and the nearest higher sample on each side.
            let left = (0..i).rev().take_while(|&j| s[j] <= s[i]).map(|j| s[j]).fold(s[i], f32::min);
            let right = (i + 1..n).take_while(|&j| s[j] <= s[i]).map(|j| s[j]).fold(s[i], f32::min);
            s[i] - left.max(right) >= PROMINENCE
        })
        .map(|i| (i, s[i]))
        .collect();
    // Loudest first, dropping any within MIN_FRAMES of one already kept.
    peaks.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut kept: Vec<(usize, f32)> = Vec::new();
    for p in peaks {
        if kept.iter().all(|k| k.0.abs_diff(p.0) >= MIN_FRAMES) {
            kept.push(p);
        }
    }
    kept.sort_by_key(|k| k.0);
    kept.into_iter().map(|(i, db)| Nucleus { clock: frames[i].clock, db }).collect()
}

/// Syllables per second of speech around `clock`, smoothed over about a second; None where
/// there is too little speech to say.
fn rate_at(nuclei: &[Nucleus], speech: &[f64], clock: f64) -> Option<f32> {
    let g = |d: f64| (-0.5 * (d / SIGMA).powi(2)).exp() / (SIGMA * (2.0 * std::f64::consts::PI).sqrt());
    let near = |c: f64| (c - clock).abs() < 3.0 * SIGMA;
    let n: f64 = nuclei.iter().filter(|k| near(k.clock)).map(|k| g(k.clock - clock)).sum();
    let d: f64 = speech.iter().filter(|&&c| near(c)).map(|&c| g(c - clock) * FRAME_SECS).sum();
    (d > 0.2).then(|| (n / d) as f32)
}

/// The frames with clocks in `[from, to]`, oldest first.
fn window(signals: &Signals, from: f64, to: f64) -> Vec<SignalFrame> {
    signals.frames().map(|(_, f)| *f).filter(|f| f.clock >= from && f.clock <= to).collect()
}

/// The rate numbers over the last few seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateStats {
    /// Syllables per second of speech.
    pub syllables_per_sec: f32,
    /// Words per minute from the first to the last word in the window; None without words.
    pub wpm: Option<f32>,
    pub syllables: usize,
    /// Seconds of speech in the window.
    pub speech_secs: f32,
}

impl RateStats {
    /// The last `secs` of audio heard at scene time `t`. `words` are in recording seconds,
    /// with `origin` the bus clock at recording time 0 (see [`crate::Axis`]).
    pub fn at(signals: &Signals, t: f64, secs: f64, words: &[TimedWord], origin: f64) -> RateStats {
        let none = RateStats { syllables_per_sec: 0.0, wpm: None, syllables: 0, speech_secs: 0.0 };
        let Some(edge) = signals.clock_at(t) else { return none };
        let frames = window(signals, edge - secs - 1.0, edge);
        let fl = floor(&frames);
        let inside = |c: f64| c >= edge - secs && c <= edge;
        let syllables = nuclei(&frames).iter().filter(|k| inside(k.clock)).count();
        let speech_secs =
            frames.iter().filter(|f| inside(f.clock) && is_speech(f, fl)).count() as f32 * FRAME_SECS as f32;
        let heard: Vec<&TimedWord> = words.iter().filter(|w| inside(origin + w.end as f64)).collect();
        let wpm = match (heard.first(), heard.last()) {
            (Some(a), Some(b)) if b.end - a.start > 0.5 => Some(heard.len() as f32 * 60.0 / (b.end - a.start)),
            _ => None,
        };
        RateStats {
            syllables_per_sec: if speech_secs > 0.3 { syllables as f32 / speech_secs } else { 0.0 },
            wpm,
            syllables,
            speech_secs,
        }
    }
}

/// How ticks and the curve are drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RateStyle {
    /// Hairline ticks and a solid curve.
    Lines,
    /// Ticks as dot columns and the curve as dots, as on a dot-matrix display.
    Dots,
}

/// How the rate views look.
#[derive(Clone, Debug)]
pub struct RateLook {
    /// Seconds across the view.
    pub secs: f64,
    pub style: RateStyle,
    pub tick: Color,
    /// Ticks within this many dB of the loudest in view are stressed and drawn in `accent`.
    pub stress_db: f32,
    pub accent: Color,
    pub tick_width: f32,
    pub baseline: Color,
    pub curve: Color,
    pub curve_width: f32,
    /// Alpha of the fill under the curve; 0 for none.
    pub fill_alpha: f32,
    /// Syllables per second at the bottom and top of the curve view.
    pub range: (f32, f32),
    /// Marks on the curve's peaks and troughs.
    pub extrema: Option<Color>,
}

impl Default for RateLook {
    /// Grey ticks with amber stressed ones and an amber curve, as in the Swiss mock-up.
    fn default() -> RateLook {
        let amber = Color::hex(0xffb340, 1.0);
        RateLook {
            secs: 6.0,
            style: RateStyle::Lines,
            tick: Color::hex(0x8a8e98, 0.9),
            stress_db: 5.0,
            accent: amber,
            tick_width: 1.5,
            baseline: Color::hex(0x8a8e98, 0.5),
            curve: amber,
            curve_width: 1.5,
            fill_alpha: 0.1,
            range: (2.0, 8.0),
            extrema: None,
        }
    }
}

impl RateLook {
    /// White dot columns, a dotted amber curve with its peaks and troughs marked.
    pub fn dots() -> RateLook {
        RateLook {
            style: RateStyle::Dots,
            tick: Color::hex(0xf2f2f2, 0.9),
            tick_width: 2.0,
            fill_alpha: 0.0,
            extrema: Some(Color::hex(0xffb340, 1.0)),
            ..RateLook::default()
        }
    }
}

/// Syllable ticks across the element's box, rising from a baseline at the bottom. Give it a
/// size.
pub fn rate_ticks<M: 'static>(key: impl Into<Key>, cell: SignalsCell, look: RateLook) -> Element<M> {
    Element::canvas(key, 0.0, move |t, rect, p| draw_ticks(&cell, &look, t, rect, p)).wake_on_signals(0.5)
}

/// The rate curve across the element's box. Give it a size.
pub fn rate_curve<M: 'static>(key: impl Into<Key>, cell: SignalsCell, look: RateLook) -> Element<M> {
    Element::canvas(key, 0.0, move |t, rect, p| draw_curve(&cell, &look, t, rect, p)).wake_on_signals(0.5)
}

fn draw_ticks(cell: &SignalsCell, look: &RateLook, t: f64, rect: Rect, p: &mut Painter) {
    let signals = cell.get();
    let Some(edge) = signals.clock_at(t) else { return };
    let from = edge - look.secs;
    let frames = window(&signals, from - 1.0, edge);
    let shown: Vec<Nucleus> = nuclei(&frames).into_iter().filter(|k| k.clock >= from).collect();
    let bottom = rect.y + rect.h;
    p.shape(ShapeInstance::line(
        Point::new(rect.x, bottom - 0.5),
        Point::new(rect.x + rect.w, bottom - 0.5),
        1.0,
        Paint::solid(look.baseline),
    ));
    let Some(top) = shown.iter().map(|k| k.db).reduce(f32::max) else { return };
    let x = |c: f64| rect.x + ((c - from) / look.secs) as f32 * rect.w;
    let usable = rect.h - 3.0;
    for k in &shown {
        let h = ((k.db - (top - 24.0)) / 24.0).clamp(0.15, 1.0) * usable;
        let colour = if k.db >= top - look.stress_db { look.accent } else { look.tick };
        let cx = x(k.clock);
        match look.style {
            RateStyle::Lines => {
                let r = Rect::new(cx - 0.5 * look.tick_width, bottom - 3.0 - h, look.tick_width, h);
                p.shape(ShapeInstance::rrect(RRect::new(r, 0.0), Paint::solid(colour)));
            }
            RateStyle::Dots => {
                let pitch = 2.0 * look.tick_width;
                let n = (h / pitch).round().max(1.0) as usize;
                for j in 0..n {
                    let c = if j + 1 == n { colour } else { look.tick };
                    let y = bottom - 3.0 - 0.5 * pitch - j as f32 * pitch;
                    p.shape(ShapeInstance::circle(Point::new(cx, y), 0.5 * look.tick_width, Paint::solid(c)));
                }
            }
        }
    }
}

/// Curve points across the view: x and rate, None where there is too little speech.
fn curve_points(signals: &Signals, t: f64, secs: f64, step: usize) -> Option<(f64, Vec<Option<f32>>)> {
    let edge = signals.clock_at(t)?;
    let from = edge - secs;
    let frames = window(signals, from - 2.0, edge);
    let fl = floor(&frames);
    let nuc = nuclei(&frames);
    let speech: Vec<f64> = frames.iter().filter(|f| is_speech(f, fl)).map(|f| f.clock).collect();
    // Points on a fixed grid of the clock, so a point keeps its value as the view scrolls.
    let dt = FRAME_SECS * step as f64;
    let first = (from / dt).floor() as i64;
    let last = (edge / dt).floor() as i64;
    Some((first as f64 * dt, (first..=last).map(|k| rate_at(&nuc, &speech, k as f64 * dt)).collect()))
}

fn draw_curve(cell: &SignalsCell, look: &RateLook, t: f64, rect: Rect, p: &mut Painter) {
    let signals = cell.get();
    let step = 4;
    let Some((start, rates)) = curve_points(&signals, t, look.secs, step) else { return };
    let Some(edge) = signals.clock_at(t) else { return };
    let from = edge - look.secs;
    let dt = FRAME_SECS * step as f64;
    let (lo, hi) = look.range;
    let x = |c: f64| rect.x + ((c - from) / look.secs) as f32 * rect.w;
    let y = |r: f32| rect.y + rect.h * (1.0 - ((r - lo) / (hi - lo)).clamp(0.0, 1.0));
    // Runs of known rate; a run holds through gaps shorter than half a second.
    let mut held: Option<f32> = None;
    let mut gap = 0usize;
    let mut runs: Vec<Vec<Point>> = vec![Vec::new()];
    for (i, r) in rates.iter().enumerate() {
        let c = start + i as f64 * dt;
        match (r, held) {
            (Some(r), _) => {
                held = Some(*r);
                gap = 0;
            }
            (None, Some(_)) if (gap as f64) * dt < 0.5 => gap += 1,
            _ => {
                held = None;
                if !runs.last().is_some_and(Vec::is_empty) {
                    runs.push(Vec::new());
                }
                continue;
            }
        }
        if c >= from {
            runs.last_mut().expect("one run").push(Point::new(x(c), y(held.expect("held"))));
        }
    }
    let bottom = rect.y + rect.h;
    for run in runs.iter().filter(|r| r.len() >= 2) {
        if look.fill_alpha > 0.0 {
            let mut poly = run.clone();
            poly.push(Point::new(run[run.len() - 1].x, bottom));
            poly.push(Point::new(run[0].x, bottom));
            let fill = look.curve.fade(look.fill_alpha);
            p.fill([&Contour::polyline(&poly, true)], &StrokeStyle::solid(1.0, fill));
        }
        match look.style {
            RateStyle::Lines => {
                p.stroke(&Contour::polyline(run, false), &StrokeStyle::solid(look.curve_width, look.curve))
            }
            RateStyle::Dots => {
                let mut along = 0.0f32;
                for w in run.windows(2) {
                    along += (w[1].x - w[0].x).hypot(w[1].y - w[0].y);
                    if along >= 4.0 {
                        along = 0.0;
                        p.shape(ShapeInstance::circle(w[1], 0.5 * look.curve_width.max(1.5), Paint::solid(look.curve)));
                    }
                }
            }
        }
        if let Some(mark) = look.extrema {
            for i in extrema(run) {
                p.shape(ShapeInstance::circle(run[i], 2.5, Paint::solid(mark)));
            }
        }
    }
}

/// Peaks and troughs of a run that stand at least 3 px above or below the turns around them.
fn extrema(run: &[Point]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut last_turn = run.first().map_or(0.0, |p| p.y);
    for i in 1..run.len().saturating_sub(1) {
        let (a, b, c) = (run[i - 1].y, run[i].y, run[i + 1].y);
        let turn = (b < a && b <= c) || (b > a && b >= c);
        if turn && (b - last_turn).abs() >= 3.0 {
            out.push(i);
            last_turn = b;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Speech at 10 ms frames: a syllable bump every `period` seconds from `start` for `count`
    /// syllables, room noise elsewhere.
    fn bumps(start: f64, period: f64, count: usize, secs: f64) -> Vec<SignalFrame> {
        let n = (secs / FRAME_SECS) as usize;
        (0..n)
            .map(|i| {
                let c = i as f64 * FRAME_SECS;
                let k = ((c - start) / period).floor();
                let x = (c - start) / period - k;
                let on = c >= start && (k as usize) < count;
                let env = if on { (std::f64::consts::PI * x).sin().powi(2) as f32 } else { 0.0 };
                let db = -62.0 + 50.0 * env;
                let mut f = SignalFrame::silent(c);
                f.rms_dbfs = db;
                f.voiced = env > 0.2;
                f.mel = [((db + 100.0) / 100.0).clamp(0.0, 1.0); 24];
                f
            })
            .collect()
    }

    #[test]
    fn one_nucleus_per_syllable() {
        let frames = bumps(0.5, 0.2, 12, 4.0);
        let n = nuclei(&frames);
        assert_eq!(n.len(), 12, "{n:?}");
        // Each sits near the middle of its bump.
        for (k, nu) in n.iter().enumerate() {
            assert!((nu.clock - (0.5 + 0.2 * k as f64 + 0.1)).abs() < 0.03, "{k} {nu:?}");
        }
    }

    #[test]
    fn noise_has_no_syllables_and_close_peaks_merge() {
        assert!(nuclei(&bumps(0.5, 0.2, 0, 3.0)).is_empty());
        // 60 ms apart is closer than a syllable can be.
        let n = nuclei(&bumps(0.5, 0.06, 20, 3.0));
        assert!(n.len() <= 14, "{}", n.len());
    }

    #[test]
    fn nuclei_stay_put_as_the_window_slides() {
        let frames = bumps(0.3, 0.23, 30, 8.0);
        let a = nuclei(&frames[0..500]);
        let b = nuclei(&frames[100..600]);
        let inner =
            |n: &[Nucleus]| n.iter().filter(|k| k.clock > 1.5 && k.clock < 4.5).map(|k| k.clock).collect::<Vec<_>>();
        assert_eq!(inner(&a), inner(&b));
    }

    #[test]
    fn rate_is_syllables_per_second_of_speech() {
        // Five syllables a second, then a pause; the pause does not pull the rate down.
        let frames = bumps(0.5, 0.2, 15, 6.0);
        let s = Signals::recorded(frames, 0.0).with_playout(0.0);
        let words: Vec<TimedWord> = (0..5)
            .map(|i| TimedWord { id: i, text: "w".into(), start: 0.5 + 0.6 * i as f32, end: 1.0 + 0.6 * i as f32 })
            .collect();
        let m = RateStats::at(&s, 5.99, 6.0, &words, 0.0);
        assert_eq!(m.syllables, 15);
        // Speech frames are the voiced part of each bump, so the rate is above 5.
        assert!(m.syllables_per_sec > 5.0 && m.syllables_per_sec < 12.0, "{m:?}");
        // Five words from 0.5 s to 3.4 s.
        assert!((m.wpm.unwrap() - 5.0 * 60.0 / 2.9).abs() < 0.1, "{m:?}");
        let (_, rates) = curve_points(&s, 5.99, 6.0, 4).unwrap();
        let known: Vec<f32> = rates.iter().flatten().copied().collect();
        assert!(!known.is_empty());
        assert!(rates.last().unwrap().is_none(), "no rate in the long pause");
    }

    #[test]
    fn extrema_mark_turns_only() {
        let run: Vec<Point> = (0..40).map(|i| Point::new(i as f32, 10.0 * (i as f32 * 0.3).sin())).collect();
        let e = extrema(&run);
        assert!(e.len() >= 3, "{e:?}");
        assert!(extrema(&(0..10).map(|i| Point::new(i as f32, 0.0)).collect::<Vec<_>>()).is_empty());
    }
}
