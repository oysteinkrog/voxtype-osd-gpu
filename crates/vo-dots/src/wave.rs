//! Dot-matrix level visuals: a scrolling dot waveform, a dot level meter with peak hold, and
//! a scrolling dot ticker. All three are pure functions of the frame time, like
//! `vo_scene::timed::Wave`: the app hands over new levels or text with the time they apply
//! from, and every frame in between is computed, never stepped.

use std::sync::Arc;

use vo_scene::dots::{Dot, DotSource};
use vo_scene::geom::Color;

use crate::field::mix;
use crate::font::DotFont;

/// Asymmetric one-pole envelope over evenly spaced levels: rises with time constant `attack`,
/// falls with `release` (seconds).
pub fn envelope(levels: &[f32], rate: f32, attack: f32, release: f32) -> Vec<f32> {
    let dt = 1.0 / rate.max(1e-3);
    let ka = 1.0 - (-dt / attack.max(1e-4)).exp();
    let kr = 1.0 - (-dt / release.max(1e-4)).exp();
    let mut e = 0.0f32;
    levels
        .iter()
        .map(|&l| {
            let l = l.clamp(0.0, 1.0);
            e += (l - e) * if l > e { ka } else { kr };
            e
        })
        .collect()
}

/// Colour at `f` (0 to 1) along stops `(position, colour)`.
pub fn ramp(stops: &[(f32, Color)], f: f32) -> Color {
    let Some(&(p0, c0)) = stops.first() else { return Color::WHITE };
    if f <= p0 {
        return c0;
    }
    for w in stops.windows(2) {
        let ((pa, a), (pb, b)) = (w[0], w[1]);
        if f <= pb {
            return mix(a, b, if pb > pa { (f - pa) / (pb - pa) } else { 1.0 });
        }
    }
    stops.last().map_or(c0, |s| s.1)
}

/// Smoothstep from 0 to 1 over `[0, 1]`.
fn ease(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// A dot waveform (design 21): columns of dots symmetric about a centre row, one column per
/// `column_secs` of level history, scrolling right to left a whole column at a time. Silence
/// is the centre row alone, drawn small; speech grows lit columns. The top dot of a column
/// fades in by its fraction, so a height change never strobes.
#[derive(Clone, Debug)]
pub struct DotWave {
    /// The levels, oldest first, 0 to 1, and their envelope.
    raw: Arc<[f32]>,
    env: Arc<[f32]>,
    /// Levels per second.
    pub rate: f32,
    /// Scene time of the newest level. The wave scrolls until then and freezes after.
    pub newest_at: f64,
    pub cols: u32,
    /// Odd, so there is a centre row.
    pub rows: u32,
    /// Seconds of history per column.
    pub column_secs: f32,
    /// Colour by column, left (0) to right (1).
    pub stops: Vec<(f32, Color)>,
    /// Lit dot diameter, and the diameter of the resting centre-row dots, as pitch fractions.
    pub size: f32,
    pub rest_size: f32,
    /// Colour of the resting centre-row dots.
    pub rest_color: Color,
    /// The wave opens from the centre column outward from this time, `open_stagger` seconds
    /// per column, each column over `open_secs`.
    pub open_at: f64,
    pub open_stagger: f32,
    pub open_secs: f32,
    /// From this time the columns collapse to the centre row over `collapse_secs`.
    pub collapse_at: Option<f64>,
    pub collapse_secs: f32,
}

impl DotWave {
    /// A wave over `levels` (oldest first, 0 to 1, `rate` per second; the newest at
    /// `newest_at`), with the design 21 envelope: attack 15 ms, release 140 ms.
    pub fn new(levels: &[f32], rate: f32, newest_at: f64, cols: u32, rows: u32) -> DotWave {
        DotWave {
            raw: levels.iter().map(|l| l.clamp(0.0, 1.0)).collect(),
            env: envelope(levels, rate, 0.015, 0.14).into(),
            rate,
            newest_at,
            cols,
            rows: rows | 1,
            column_secs: 0.05,
            stops: vec![(0.0, Color::WHITE)],
            size: 0.62,
            rest_size: 0.34,
            rest_color: Color::hex(0xffffff, 0.55),
            open_at: f64::NEG_INFINITY,
            open_stagger: 0.012,
            open_secs: 0.18,
            collapse_at: None,
            collapse_secs: 0.25,
        }
    }

    /// The envelope at scene time `at`, linearly between levels; 0 before the first.
    fn env_at(&self, at: f64) -> f32 {
        let n = self.env.len();
        if n == 0 {
            return 0.0;
        }
        let i = (n - 1) as f64 - (self.newest_at - at) * self.rate as f64;
        if i < 0.0 {
            return 0.0;
        }
        let i0 = (i.floor() as usize).min(n - 1);
        let i1 = (i0 + 1).min(n - 1);
        let k = (i - i0 as f64) as f32;
        self.env[i0] + (self.env[i1] - self.env[i0]) * k
    }

    /// The loudest level in `[start, end)` of scene time, or the nearest one when no level
    /// falls inside.
    fn peak_in(&self, start: f64, end: f64) -> f32 {
        let n = self.raw.len();
        if n == 0 {
            return 0.0;
        }
        let index = |at: f64| (n - 1) as f64 - (self.newest_at - at) * self.rate as f64;
        let (lo, hi) = (index(start).ceil(), index(end).ceil() - 1.0);
        if hi < 0.0 {
            return 0.0;
        }
        let (lo, hi) = (lo.max(0.0) as usize, (hi as usize).min(n - 1));
        if lo > hi {
            return self.raw[(index(start).round().max(0.0) as usize).min(n - 1)];
        }
        self.raw[lo..=hi].iter().copied().fold(0.0, f32::max)
    }

    /// Half height in rows of column `c` at `t`, before the open and collapse factors.
    fn height(&self, c: u32, t: f64) -> f32 {
        let head = t.min(self.newest_at);
        let cs = self.column_secs.max(1e-3) as f64;
        // Columns step a whole column at a time. The newest column follows the live level
        // through the envelope; the others hold the peak of their slice of history, so speech
        // reads as separate spikes.
        let step = (head / cs).floor();
        let age = (self.cols - 1 - c) as f64;
        let level = if age == 0.0 {
            self.env_at(head)
        } else {
            let end = (step - age + 1.0) * cs;
            self.peak_in(end - cs, end)
        };
        level * (self.rows / 2) as f32
    }

    fn open(&self, c: u32, t: f64) -> f32 {
        let mid = (self.cols as f32 - 1.0) * 0.5;
        let start = self.open_at + ((c as f32 - mid).abs() * self.open_stagger) as f64;
        ease(((t - start) / self.open_secs.max(1e-3) as f64) as f32)
    }

    fn collapse(&self, t: f64) -> f32 {
        match self.collapse_at {
            Some(at) => 1.0 - ease(((t - at) / self.collapse_secs.max(1e-3) as f64) as f32),
            None => 1.0,
        }
    }
}

impl DotSource for DotWave {
    fn dots(&self, t: f64, out: &mut Vec<Dot>) {
        let centre = (self.rows / 2) as f32;
        let collapse = self.collapse(t);
        for c in 0..self.cols {
            let open = self.open(c, t);
            if open <= 0.0 {
                continue;
            }
            let color = ramp(&self.stops, c as f32 / (self.cols.max(2) - 1) as f32);
            let h = self.height(c, t) * collapse * open;
            let x = c as f32;
            // Centre row: a resting dot that grows into a lit one as the column rises.
            let lit = h.clamp(0.0, 1.0);
            out.push(Dot {
                x,
                y: centre,
                size: (self.rest_size + (self.size - self.rest_size) * lit) * open,
                color: mix(self.rest_color, color, lit),
                snap: 1.0,
                ring: 0.0,
            });
            for k in 1..=self.rows / 2 {
                let b = (h - k as f32 + 1.0).clamp(0.0, 1.0);
                if b <= 0.0 {
                    break;
                }
                let dot = Dot { x, y: centre - k as f32, size: self.size, color: color.fade(b), snap: 1.0, ring: 0.0 };
                out.push(dot);
                out.push(Dot { y: centre + k as f32, ..dot });
            }
        }
    }

    fn moving(&self, t: f64) -> bool {
        let opening = t < self.open_at + (self.cols as f32 * 0.5 * self.open_stagger + self.open_secs) as f64;
        let collapsing = self.collapse_at.is_some_and(|at| t < at + self.collapse_secs as f64);
        // The live column follows the envelope, which keeps falling for a while after the
        // newest level; the release is under 1 s to silence.
        t < self.newest_at + 1.0 || opening || collapsing
    }
}

/// A dot level meter: a row of `len` dots lit from the left by the level, a peak dot that
/// holds for `hold` seconds and then falls a dot every `fall` seconds, colour by index.
#[derive(Clone, Debug)]
pub struct DotMeter {
    env: Arc<[f32]>,
    pub rate: f32,
    pub newest_at: f64,
    pub len: u32,
    pub stops: Vec<(f32, Color)>,
    pub unlit: Color,
    pub size: f32,
    pub hold: f32,
    pub fall: f32,
    /// Lay the dots out top to bottom (lit from the bottom) instead of left to right.
    pub vertical: bool,
}

impl DotMeter {
    pub fn new(levels: &[f32], rate: f32, newest_at: f64, len: u32) -> DotMeter {
        DotMeter {
            env: envelope(levels, rate, 0.015, 0.14).into(),
            rate,
            newest_at,
            len,
            stops: vec![
                (0.0, Color::WHITE),
                (0.7, Color::WHITE),
                (0.85, Color::hex(0xffb020, 1.0)),
                (1.0, Color::hex(0xff4d4d, 1.0)),
            ],
            unlit: Color::hex(0xffffff, 0.12),
            size: 0.62,
            hold: 0.5,
            fall: 0.08,
            vertical: false,
        }
    }

    fn index_at(&self, at: f64) -> Option<f64> {
        let n = self.env.len();
        let i = (n as f64 - 1.0) - (self.newest_at - at) * self.rate as f64;
        (n > 0 && i >= 0.0).then_some(i.min(n as f64 - 1.0))
    }

    /// Lit dots (fractional) at `t`.
    pub fn level(&self, t: f64) -> f32 {
        let Some(i) = self.index_at(t.min(self.newest_at)) else { return 0.0 };
        let (i0, k) = (i.floor() as usize, (i - i.floor()) as f32);
        let i1 = (i0 + 1).min(self.env.len() - 1);
        (self.env[i0] + (self.env[i1] - self.env[i0]) * k) * self.len as f32
    }

    /// The peak dot index at `t`: the highest level of the last `hold` seconds, minus a dot
    /// per `fall` seconds after that.
    pub fn peak(&self, t: f64) -> f32 {
        let Some(last) = self.index_at(t.min(self.newest_at)) else { return 0.0 };
        let mut best = 0.0f32;
        let window = ((self.hold + self.len as f32 * self.fall) * self.rate) as usize + 1;
        let last = last.floor() as usize;
        for i in last.saturating_sub(window)..=last {
            let age = (t - (self.newest_at - (self.env.len() - 1 - i) as f64 / self.rate as f64)) as f32;
            let drop = ((age - self.hold).max(0.0) / self.fall.max(1e-3)).floor();
            best = best.max(self.env[i] * self.len as f32 - drop);
        }
        best
    }
}

impl DotSource for DotMeter {
    fn dots(&self, t: f64, out: &mut Vec<Dot>) {
        let level = self.level(t);
        let peak = self.peak(t).ceil() as i64 - 1;
        for i in 0..self.len {
            let lit = (level - i as f32).clamp(0.0, 1.0).max(if i as i64 == peak { 1.0 } else { 0.0 });
            let on = ramp(&self.stops, i as f32 / (self.len.max(2) - 1) as f32);
            let (x, y) = if self.vertical { (0.0, (self.len - 1 - i) as f32) } else { (i as f32, 0.0) };
            out.push(Dot { x, y, size: self.size, color: mix(self.unlit, on, lit), snap: 1.0, ring: 0.0 });
        }
    }

    fn moving(&self, t: f64) -> bool {
        t < self.newest_at + (self.hold + self.len as f32 * self.fall) as f64 + 1.0
    }
}

/// Text scrolling through a window of dots, right to left, one column every `1 / speed`
/// seconds from `start_at`, looping with a gap. Unlit cells show a faint ghost grid, as on an
/// LED sign.
#[derive(Clone, Debug)]
pub struct DotTicker {
    pub text: String,
    pub font: DotFont,
    pub tracking: i32,
    /// Window size in cells.
    pub cols: u32,
    pub start_at: f64,
    /// Columns per second.
    pub speed: f32,
    /// Blank columns between the end of the text and its next pass.
    pub gap: u32,
    pub color: Color,
    pub ghost: Color,
    pub size: f32,
    /// Move smoothly between columns instead of stepping. Off for the classic sign.
    pub smooth: bool,
    lit: Arc<[(i32, i32)]>,
    width: i32,
}

impl DotTicker {
    pub fn new(text: &str, font: DotFont, cols: u32, start_at: f64) -> DotTicker {
        let tracking = 1;
        let lit: Vec<(i32, i32)> = font.cells(text, tracking).iter().map(|c| (c.x, c.y)).collect();
        DotTicker {
            text: text.into(),
            font,
            tracking,
            cols,
            start_at,
            speed: 40.0,
            gap: 12,
            color: Color::hex(0xffb020, 1.0),
            ghost: Color::hex(0xffb020, 0.16),
            size: 0.7,
            smooth: false,
            lit: lit.into(),
            width: font.width(text, tracking) + 1,
        }
    }

    /// Rows of the window: the font's line height.
    pub fn rows(&self) -> u32 {
        self.font.line_height() as u32
    }
}

impl DotSource for DotTicker {
    fn dots(&self, t: f64, out: &mut Vec<Dot>) {
        let rows = self.rows() as i32;
        let period = self.width + self.gap as i32 + self.cols as i32;
        let pos = ((t - self.start_at).max(0.0) * self.speed as f64) as f32;
        let whole = pos.floor();
        let frac = if self.smooth { pos - whole } else { 0.0 };
        // Text column at window column 0.
        let shift = (whole as i64).rem_euclid(period as i64) as i32 - self.cols as i32;
        if self.ghost.a > 0.0 {
            for y in 0..rows {
                for x in 0..self.cols as i32 {
                    out.push(Dot {
                        x: x as f32,
                        y: y as f32,
                        size: self.size * 0.8,
                        color: self.ghost,
                        snap: 1.0,
                        ring: 0.0,
                    });
                }
            }
        }
        for &(cx, cy) in self.lit.iter() {
            let x = (cx - shift) as f32 - frac;
            if x < -0.5 || x > self.cols as f32 - 0.5 {
                continue;
            }
            out.push(Dot {
                x,
                y: cy as f32,
                size: self.size,
                color: self.color,
                snap: if self.smooth { 0.0 } else { 1.0 },
                ring: 0.0,
            });
        }
    }

    fn moving(&self, _: f64) -> bool {
        self.speed > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dots(s: &impl DotSource, t: f64) -> Vec<Dot> {
        let mut v = Vec::new();
        s.dots(t, &mut v);
        v
    }

    #[test]
    fn envelope_rises_fast_and_falls_slowly() {
        let mut l = vec![1.0; 10];
        l.extend([0.0; 10]);
        let e = envelope(&l, 100.0, 0.015, 0.14);
        assert!(e[2] > 0.8, "{}", e[2]);
        assert!(e[12] > 0.8, "{}", e[12]);
    }

    #[test]
    fn silence_is_one_row_and_speech_grows_columns() {
        let quiet = DotWave::new(&[0.0; 300], 100.0, 3.0, 40, 9);
        let d = dots(&quiet, 3.0);
        assert_eq!(d.len(), 40);
        assert!(d.iter().all(|d| d.y == 4.0 && d.size < 0.4));
        let loud = DotWave::new(&[0.8; 300], 100.0, 3.0, 40, 9);
        let d = dots(&loud, 3.0);
        assert!(d.len() > 40 * 5, "{}", d.len());
        // Symmetric about the centre row.
        let above = d.iter().filter(|d| d.y < 4.0).count();
        let below = d.iter().filter(|d| d.y > 4.0).count();
        assert_eq!(above, below);
    }

    #[test]
    fn wave_scrolls_by_whole_columns_and_freezes() {
        let mut levels = vec![0.0; 200];
        levels.extend([1.0; 5]);
        let w = DotWave::new(&levels, 100.0, 2.05, 20, 9);
        let lit_cols = |t: f64| {
            let mut c: Vec<i32> = dots(&w, t).iter().filter(|d| d.y < 4.0).map(|d| d.x as i32).collect();
            c.dedup();
            c
        };
        assert_eq!(lit_cols(2.05), lit_cols(3.0), "frozen after the newest level");
        for d in dots(&w, 2.03) {
            assert_eq!(d.x, d.x.round());
        }
    }

    #[test]
    fn collapse_flattens_to_the_centre_row() {
        let mut w = DotWave::new(&[0.8; 300], 100.0, 3.0, 20, 9);
        w.collapse_at = Some(3.0);
        assert!(dots(&w, 3.1).len() > 20);
        assert_eq!(dots(&w, 3.3).len(), 20);
    }

    #[test]
    fn meter_peak_holds_then_falls() {
        let mut l = vec![0.0; 100];
        l.extend([1.0; 10]);
        l.extend([0.0; 100]);
        let m = DotMeter::new(&l, 100.0, 2.1, 10);
        // The loud part ends at 1.1 s.
        assert!(m.peak(1.3) > 9.0);
        assert!(m.peak(1.3) - m.peak(1.9) >= 3.0, "{} {}", m.peak(1.3), m.peak(1.9));
        assert!(m.level(2.1) < 0.5);
    }

    #[test]
    fn ticker_steps_a_column_at_a_time_and_loops() {
        let t = DotTicker::new("HELLO", DotFont::SMALL, 30, 0.0);
        let lit = |time: f64| -> Vec<(i32, i32)> {
            dots(&t, time).iter().filter(|d| d.color == t.color).map(|d| (d.x as i32, d.y as i32)).collect()
        };
        assert_eq!(lit(0.0), Vec::<(i32, i32)>::new(), "starts off the right edge");
        let a = lit(0.5);
        assert!(!a.is_empty());
        let b = lit(0.5 + 1.0 / 40.0);
        assert_eq!(
            a.iter().map(|p| (p.0 - 1, p.1)).filter(|p| p.0 >= 0).collect::<Vec<_>>(),
            b.iter().copied().filter(|p| p.0 < 29).collect::<Vec<_>>()
        );
        let period = (t.width + t.gap as i32 + 30) as f64 / 40.0;
        assert_eq!(lit(0.5), lit(0.5 + period));
    }
}
