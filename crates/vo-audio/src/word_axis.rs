//! Words on a time axis (FX-121): each word's label over a bracket as wide as the word lasted,
//! the gaps between words labelled in seconds, a mirrored waveform of the level below, an
//! optional ruler, and a playhead at the live edge. Rows wrap like lines of text: a row holds
//! [`AxisLook::px_per_sec`] seconds per pixel of width and breaks before the first word that
//! does not fit, so a word never splits across rows.
//!
//! [`word_axis`] builds the element tree from the words known now. Rebuild it when the words
//! change; between rebuilds the canvas follows the signal bus, so the waveform and playhead
//! move every frame. Word labels and gap labels are keyed by word id, so a word that wraps to
//! the next row moves there.

use std::ops::Range;

use vo_core::signals::{SignalFrame, SignalsCell};
use vo_core::words::{WordId, WordModel};
use vo_scene::canvas::Painter;
use vo_scene::display::{Paint, ShapeInstance};
use vo_scene::element::TextStyle;
use vo_scene::geom::{Color, Point, RRect, Rect, Size};
use vo_scene::path::{Contour, StrokeStyle};
use vo_scene::{Element, Key};

/// A word with its time in the recording, in seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct TimedWord {
    pub id: WordId,
    pub text: String,
    pub start: f32,
    pub end: f32,
}

impl TimedWord {
    /// The model's words that have a start and an end, in order.
    pub fn from_model(model: &WordModel) -> Vec<TimedWord> {
        model
            .words()
            .iter()
            .filter_map(|w| {
                let info = model.info(w.id)?;
                let (start, end) = (info.start?, info.end?);
                Some(TimedWord { id: w.id, text: w.text.clone(), start, end: end.max(start) })
            })
            .collect()
    }
}

/// How the waveform is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WaveStyle {
    /// Rounded bars mirrored about a solid centre line.
    Bars,
    /// Columns of dots mirrored about a dotted centre line, as on a dot-matrix display.
    Dots,
}

/// The mark on top of the playhead.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cap {
    None,
    Dot,
    /// A small triangle pointing down.
    Triangle,
}

/// How the axis looks.
#[derive(Clone, Debug)]
pub struct AxisLook {
    pub px_per_sec: f32,
    /// Most rows shown; older rows drop off the top.
    pub max_rows: usize,
    pub label: TextStyle,
    pub label_color: Color,
    /// Gap labels use the label style at this size.
    pub gap_size: f32,
    pub gap_color: Color,
    /// Gaps shorter than this get no label.
    pub gap_min: f32,
    pub bracket: Color,
    pub bracket_width: f32,
    /// Length of the end ticks; the centre tick is half of it, pointing up. 0 for none.
    pub tick: f32,
    pub centre_tick: bool,
    pub wave: WaveStyle,
    pub wave_height: f32,
    pub wave_color: Color,
    /// The centre line in silence.
    pub line_color: Color,
    /// Bar width and the space after it; for dots, the dot pitch is the bar width plus the gap.
    pub bar: (f32, f32),
    /// RMS dBFS at the centre line and at full height.
    pub db_range: (f32, f32),
    /// Major and minor ruler steps in seconds; a major step of 0 draws no ruler.
    pub ruler: (f32, f32),
    pub ruler_color: Color,
    pub playhead: Color,
    pub cap: Cap,
    /// Space between rows.
    pub row_gap: f32,
}

impl Default for AxisLook {
    /// Light grey bars and labels with an amber playhead, as in the Swiss mock-up.
    fn default() -> AxisLook {
        AxisLook {
            px_per_sec: 150.0,
            max_rows: 3,
            label: TextStyle::new(0, 12.0, 15.0),
            label_color: Color::hex(0xe8e8ea, 0.95),
            gap_size: 10.0,
            gap_color: Color::hex(0x9a9ea8, 0.9),
            gap_min: 0.3,
            bracket: Color::hex(0xc8cad0, 0.8),
            bracket_width: 1.0,
            tick: 4.0,
            centre_tick: false,
            wave: WaveStyle::Bars,
            wave_height: 36.0,
            wave_color: Color::hex(0xdcdde2, 0.9),
            line_color: Color::hex(0xdcdde2, 0.35),
            bar: (2.0, 3.0),
            db_range: (-62.0, -8.0),
            ruler: (0.0, 0.0),
            ruler_color: Color::hex(0x9a9ea8, 0.7),
            playhead: Color::hex(0xffb340, 1.0),
            cap: Cap::Triangle,
            row_gap: 12.0,
        }
    }
}

impl AxisLook {
    /// White dot columns, centre ticks on the brackets and an amber dot on the playhead, as in
    /// the dot-matrix mock-up.
    pub fn dots() -> AxisLook {
        AxisLook {
            wave: WaveStyle::Dots,
            centre_tick: true,
            bar: (2.0, 1.0),
            wave_color: Color::hex(0xf2f2f2, 0.95),
            line_color: Color::hex(0xf2f2f2, 0.45),
            cap: Cap::Dot,
            ..AxisLook::default()
        }
    }

    /// Cyan bars over a ruler with tenths of a second, as in the vector HUD mock-up.
    pub fn hud() -> AxisLook {
        let cyan = Color::hex(0x7fd8ff, 1.0);
        AxisLook {
            label_color: Color::hex(0xd8f4ff, 0.95),
            gap_color: Color::hex(0xb8e8ff, 0.9),
            bracket: Color::hex(0x7fd8ff, 0.8),
            centre_tick: true,
            wave_color: cyan,
            line_color: Color::hex(0x7fd8ff, 0.4),
            ruler: (0.8, 0.2),
            ruler_color: Color::hex(0x7fd8ff, 0.6),
            playhead: cyan,
            ..AxisLook::default()
        }
    }

    fn ruler_height(&self) -> f32 {
        if self.ruler.0 > 0.0 {
            8.0 + self.gap_size + 4.0
        } else {
            0.0
        }
    }

    /// Height of one row.
    pub fn row_height(&self) -> f32 {
        self.label.line_height + BRACKET_ZONE + self.wave_height + self.ruler_height()
    }
}

/// Space between the label and the waveform, holding the bracket.
const BRACKET_ZONE: f32 = 12.0;

/// One row of the axis: the seconds it covers and the words on it.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub from: f32,
    pub to: f32,
    pub words: Range<usize>,
}

/// Breaks `words` into rows of `span` seconds. A row ends before the first word that does not
/// fit; the next row starts a little before that word, at most a quarter second and never
/// before the previous word ended. Rows keep coming every `span` while the live edge `until`
/// runs on in silence.
pub fn rows(words: &[TimedWord], until: f32, span: f32) -> Vec<Row> {
    let span = span.max(0.1);
    let mut rows = vec![Row { from: 0.0, to: span, words: 0..0 }];
    let mut prev_end = 0.0f32;
    for (i, w) in words.iter().enumerate() {
        let last = rows.last_mut().expect("one row");
        if w.end <= last.from + span || (last.words.is_empty() && w.start < last.from + span) {
            last.words.end = i + 1;
        } else {
            let from = (w.start - (0.5 * (w.start - prev_end)).clamp(0.0, 0.25)).max(prev_end).max(last.from);
            rows.push(Row { from, to: from + span, words: i..i + 1 });
        }
        prev_end = prev_end.max(w.end);
    }
    while until > rows.last().expect("one row").from + span && rows.len() < 10_000 {
        let (from, at) = {
            let last = rows.last().expect("one row");
            (last.from + span, last.words.end)
        };
        rows.push(Row { from, to: from + span, words: at..at });
    }
    let n = rows.len();
    for i in 0..n.saturating_sub(1) {
        rows[i].to = rows[i + 1].from;
    }
    rows
}

/// Where a word sits: its row, and its left and right edge in pixels from the row start.
pub fn word_span(row: &Row, w: &TimedWord, px_per_sec: f32) -> (f32, f32) {
    ((w.start - row.from) * px_per_sec, (w.end - row.from) * px_per_sec)
}

/// The recording time of the live edge at scene time `t`: the audio clock heard then minus
/// `origin`, the clock at recording time 0.
fn edge(cell: &SignalsCell, t: f64, origin: f64) -> Option<f32> {
    cell.get().clock_at(t).map(|c| (c - origin) as f32)
}

/// What the axis shows.
#[derive(Clone, Debug)]
pub struct Axis {
    pub words: Vec<TimedWord>,
    /// The signal bus clock, in seconds, at recording time 0.
    pub origin: f64,
    /// While live, the waveform and playhead follow the live edge; after, the axis shows
    /// everything heard and no playhead.
    pub live: bool,
    /// The recording time the rows are laid out up to, normally the live edge when built.
    pub until: f32,
}

/// The axis `width` wide, as tall as the rows it shows.
pub fn word_axis<M: 'static>(
    key: impl Into<Key>,
    cell: SignalsCell,
    axis: &Axis,
    look: &AxisLook,
    width: f32,
) -> Element<M> {
    let span = width / look.px_per_sec.max(1.0);
    let all = rows(&axis.words, axis.until, span);
    let shown: Vec<Row> = all[all.len().saturating_sub(look.max_rows.max(1))..].to_vec();
    let row_h = look.row_height() + look.row_gap;
    let height = shown.len() as f32 * row_h - look.row_gap;
    let words: Vec<TimedWord> = axis.words.clone();
    let canvas = {
        let (shown, look, words, origin, live) = (shown.clone(), look.clone(), words.clone(), axis.origin, axis.live);
        let cell = cell.clone();
        Element::canvas("canvas", 0.0, move |t, rect, p| draw(&cell, &shown, &words, &look, origin, live, t, rect, p))
            .wake_on_signals(0.25)
    };
    let mut root =
        Element::column(key).size(width, height).child(place(canvas, Point::new(0.0, 0.0), Size::new(width, height)));
    let lh = look.label.line_height;
    for (r, row) in shown.iter().enumerate() {
        let top = r as f32 * row_h;
        let mut prev: Option<&TimedWord> = None;
        // Right edge of the previous label: a label centres over its bracket but moves right
        // rather than overlap its neighbour.
        let mut right = f32::MIN;
        for w in &words[row.words.clone()] {
            let (x0, x1) = word_span(row, w, look.px_per_sec);
            let text_w = w.text.chars().count() as f32 * look.label.size * 0.62 + 4.0;
            let x = (0.5 * (x0 + x1) - 0.5 * text_w).max(right + 2.0).min(width - text_w).max(0.0);
            right = x + text_w;
            let label = Element::label(format!("w{}", w.id), w.text.clone())
                .text_style(look.label.clamp_lines(1).centered())
                .color(look.label_color);
            // The box is wider than the estimate so a wide word is not cut; the text centres in it.
            let box_w = 1.4 * text_w + 8.0;
            let bx = (x + 0.5 * text_w - 0.5 * box_w).clamp(0.0, (width - box_w).max(0.0));
            root = root.child(place(label, Point::new(bx, top), Size::new(box_w, lh)));
            if let Some(p) = prev {
                let gap = w.start - p.end;
                if gap >= look.gap_min {
                    let (_, g0) = word_span(row, p, look.px_per_sec);
                    let text = format!("{gap:.1} s");
                    let gw = text.chars().count() as f32 * look.gap_size * 0.62 + 6.0;
                    if x0 - g0 >= gw {
                        let style = TextStyle { size: look.gap_size, line_height: look.gap_size + 3.0, ..look.label }
                            .centered();
                        let gl = Element::label(format!("g{}", p.id), text)
                            .text_style(style.clamp_lines(1))
                            .color(look.gap_color);
                        let y = top + lh + 0.5 * BRACKET_ZONE - 0.5 * style.line_height;
                        root = root.child(place(
                            gl,
                            Point::new(0.5 * (g0 + x0) - 0.5 * gw, y),
                            Size::new(gw, style.line_height),
                        ));
                    }
                }
            }
            prev = Some(w);
        }
        if look.ruler.0 > 0.0 {
            let step = look.ruler.0;
            let first = (row.from / step).ceil() as i64;
            let last = ((row.from + span) / step).floor() as i64;
            let y = top + lh + BRACKET_ZONE + look.wave_height + 8.0;
            let lw = 4.0 * look.gap_size;
            for k in first..=last {
                let v = k as f32 * step;
                let x = (v - row.from) * look.px_per_sec - 0.5 * lw;
                if x < -0.5 * lw || x + 0.5 * lw > width + 0.5 * lw {
                    continue;
                }
                let style =
                    TextStyle { size: look.gap_size, line_height: look.gap_size + 3.0, ..look.label }.centered();
                let l = Element::label(format!("t{k}r{}", (row.from * 1000.0) as i64), format!("{v:.1}"))
                    .text_style(style)
                    .color(look.ruler_color);
                root = root.child(place(
                    l,
                    Point::new(x.clamp(0.0, (width - lw).max(0.0)), y),
                    Size::new(lw, style.line_height),
                ));
            }
        }
    }
    root
}

/// `e` out of flow at `at` with `size`, in its parent's box.
fn place<M>(e: Element<M>, at: Point, size: Size) -> Element<M> {
    use vo_scene::taffy::style::{LengthPercentageAuto, Position};
    e.size(size.w, size.h).layout(|s| {
        s.position = Position::Absolute;
        s.inset.left = LengthPercentageAuto::length(at.x);
        s.inset.top = LengthPercentageAuto::length(at.y);
    })
}

/// The loudest RMS level of the frames with clocks in `[c0, c1)`; `frames` is sorted by clock.
fn loudest(frames: &[(f64, f32)], c0: f64, c1: f64) -> Option<f32> {
    let a = frames.partition_point(|f| f.0 < c0);
    let b = frames.partition_point(|f| f.0 < c1);
    frames[a..b].iter().map(|f| f.1).reduce(f32::max)
}

#[allow(clippy::too_many_arguments)]
fn draw(
    cell: &SignalsCell,
    rows: &[Row],
    words: &[TimedWord],
    look: &AxisLook,
    origin: f64,
    live: bool,
    t: f64,
    rect: Rect,
    p: &mut Painter,
) {
    let signals = cell.get();
    let frames: Vec<(f64, f32)> = signals.frames().map(|(_, f)| (f.clock, f.rms_dbfs)).collect();
    let newest = frames.last().map(|f| (f.0 - origin) as f32 + 0.01);
    let edge = if live { edge(cell, t, origin) } else { newest };
    let Some(edge) = edge else { return };
    let pps = look.px_per_sec;
    let row_h = look.row_height() + look.row_gap;
    let lh = look.label.line_height;
    let (lo, hi) = look.db_range;
    let solid = |c: Color| Paint::solid(c);
    for (r, row) in rows.iter().enumerate() {
        if row.from >= edge {
            break;
        }
        let top = rect.y + r as f32 * row_h;
        let x = |s: f32| rect.x + (s - row.from) * pps;
        let end = row.to.min(edge);
        // Brackets.
        let by = top + lh + 3.0;
        let bs = StrokeStyle::solid(look.bracket_width, look.bracket);
        for w in &words[row.words.clone()] {
            if w.start >= edge {
                continue;
            }
            let (x0, x1) = (x(w.start) + 1.0, (x(w.end.min(edge)) - 1.0).max(x(w.start) + 3.0));
            let mut pts = Vec::with_capacity(4);
            if look.tick > 0.0 {
                pts.push(Point::new(x0, by + look.tick));
            }
            pts.push(Point::new(x0, by));
            pts.push(Point::new(x1, by));
            if look.tick > 0.0 {
                pts.push(Point::new(x1, by + look.tick));
            }
            p.stroke(&Contour::polyline(&pts, false), &bs);
            if look.centre_tick && look.tick > 0.0 {
                let cx = 0.5 * (x0 + x1);
                p.shape(ShapeInstance::line(
                    Point::new(cx, by),
                    Point::new(cx, by - 0.5 * look.tick),
                    look.bracket_width,
                    solid(look.bracket),
                ));
            }
        }
        // Waveform.
        let mid = top + lh + BRACKET_ZONE + 0.5 * look.wave_height;
        let half = 0.5 * look.wave_height;
        let (bw, bg) = look.bar;
        let pitch = (bw + bg).max(1.0);
        match look.wave {
            WaveStyle::Bars => p.shape(ShapeInstance::line(
                Point::new(x(row.from), mid),
                Point::new(x(end), mid),
                1.0,
                solid(look.line_color),
            )),
            WaveStyle::Dots => {
                let mut cx = x(row.from) + 0.5 * pitch;
                while cx < x(end) {
                    p.shape(ShapeInstance::circle(Point::new(cx, mid), 0.35 * bw, solid(look.line_color)));
                    cx += 2.0 * pitch;
                }
            }
        }
        let bars = ((end - row.from) * pps / pitch).floor().max(0.0) as usize;
        let dt = (pitch / pps) as f64;
        for k in 0..bars {
            let c0 = origin + row.from as f64 + k as f64 * dt;
            let Some(db) = loudest(&frames, c0, c0 + dt) else { continue };
            let amp = ((db - lo) / (hi - lo)).clamp(0.0, 1.0).powf(1.3);
            let h = amp * half;
            let cx = x(row.from) + k as f32 * pitch + 0.5 * bw;
            match look.wave {
                WaveStyle::Bars => {
                    if h >= 0.75 {
                        let r = Rect::new(cx - 0.5 * bw, mid - h, bw, 2.0 * h);
                        p.shape(ShapeInstance::rrect(RRect::new(r, 0.5 * bw), solid(look.wave_color)));
                    }
                }
                WaveStyle::Dots => {
                    let n = (h / pitch).round() as i32;
                    for j in -n..=n {
                        if n > 0 {
                            let y = mid + j as f32 * pitch;
                            p.shape(ShapeInstance::circle(Point::new(cx, y), 0.5 * bw, solid(look.wave_color)));
                        }
                    }
                }
            }
        }
        // Ruler.
        if look.ruler.0 > 0.0 {
            let y = mid + half + 4.0;
            let rs = |c: Color| solid(c);
            p.shape(ShapeInstance::line(
                Point::new(x(row.from), y),
                Point::new(x(row.from) + rect.w, y),
                1.0,
                rs(look.ruler_color),
            ));
            for (step, len) in [(look.ruler.1, 3.0), (look.ruler.0, 6.0)] {
                if step <= 0.0 {
                    continue;
                }
                let first = (row.from / step).ceil() as i64;
                let last = ((row.from + rect.w / pps) / step).floor() as i64;
                for k in first..=last {
                    let tx = x(k as f32 * step);
                    p.shape(ShapeInstance::line(Point::new(tx, y), Point::new(tx, y + len), 1.0, rs(look.ruler_color)));
                }
            }
        }
        // Playhead on the row that holds the live edge.
        if live && edge < row.to + 1e-4 {
            let px = x(edge);
            let y0 = top + lh;
            let y1 = mid + half;
            p.shape(ShapeInstance::line(Point::new(px, y0), Point::new(px, y1), 1.5, solid(look.playhead)));
            match look.cap {
                Cap::None => {}
                Cap::Dot => p.shape(ShapeInstance::circle(Point::new(px, y0), 3.0, solid(look.playhead))),
                Cap::Triangle => {
                    let tri = Contour::polyline(
                        &[Point::new(px - 4.0, y0 - 5.0), Point::new(px + 4.0, y0 - 5.0), Point::new(px, y0)],
                        true,
                    );
                    p.fill([&tri], &StrokeStyle::solid(1.0, look.playhead));
                }
            }
        }
    }
}

/// Spans of the recording where the level stays above `threshold_db`, with dips shorter than
/// `join` seconds bridged, as `(start, end)` clock seconds. Stand-in word times for an engine
/// that gives none, and for tests.
pub fn level_spans(frames: &[SignalFrame], threshold_db: f32, join: f64) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    for f in frames.iter().filter(|f| f.rms_dbfs > threshold_db) {
        let end = f.clock + vo_core::signals::FRAME_SECS;
        match out.last_mut() {
            Some(last) if f.clock - last.1 <= join => last.1 = end,
            _ => out.push((f.clock, end)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use vo_core::signals::Signals;
    use vo_scene::Scene;

    use super::*;

    fn word(id: u32, start: f32, end: f32) -> TimedWord {
        TimedWord { id, text: format!("w{id}"), start, end }
    }

    #[test]
    fn rows_break_before_the_word_that_does_not_fit() {
        // Words 0.4 s long every 0.5 s; rows of 2 s.
        let words: Vec<_> = (0..10).map(|i| word(i, 0.1 + 0.5 * i as f32, 0.5 + 0.5 * i as f32)).collect();
        let rows = rows(&words, 5.0, 2.0);
        for r in &rows {
            for w in &words[r.words.clone()] {
                assert!(w.start >= r.from && w.end <= r.from + 2.0, "{r:?} {w:?}");
            }
        }
        // Every word is on exactly one row, in order.
        let on: Vec<usize> = rows.iter().flat_map(|r| r.words.clone()).collect();
        assert_eq!(on, (0..10).collect::<Vec<_>>());
        // Rows start in the gap before their first word, a quarter of the 0.1 s gap at most.
        assert!((rows[1].from - 2.05).abs() < 1e-5, "{rows:?}");
        assert!(rows.windows(2).all(|w| w[0].to == w[1].from && w[0].from < w[1].from));
    }

    #[test]
    fn silence_after_the_last_word_adds_rows() {
        let words = vec![word(0, 0.2, 0.6)];
        let rows = rows(&words, 4.5, 2.0);
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[1].from, rows[2].from), (2.0, 4.0));
        assert!(rows[1].words.is_empty());
    }

    #[test]
    fn a_word_longer_than_a_row_still_gets_one() {
        let words = vec![word(0, 0.1, 0.3), word(1, 0.5, 3.5), word(2, 3.6, 3.8)];
        let rows = rows(&words, 3.8, 2.0);
        let on: Vec<Vec<usize>> = rows.iter().map(|r| r.words.clone().collect()).collect();
        assert_eq!(on, vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn level_spans_bridge_short_dips() {
        let frames: Vec<SignalFrame> = (0..100)
            .map(|i| SignalFrame {
                rms_dbfs: if (10..30).contains(&i) || (33..50).contains(&i) || (70..80).contains(&i) {
                    -20.0
                } else {
                    -70.0
                },
                ..SignalFrame::silent(i as f64 * 0.01)
            })
            .collect();
        let spans = level_spans(&frames, -50.0, 0.05);
        assert_eq!(spans.len(), 2);
        assert!((spans[0].0 - 0.1).abs() < 1e-9 && (spans[0].1 - 0.5).abs() < 1e-9, "{spans:?}");
    }

    #[test]
    fn labels_for_words_and_long_gaps_only() {
        let words = vec![word(0, 0.2, 0.6), word(1, 0.7, 1.1), word(2, 1.8, 2.3)];
        let cell = SignalsCell::new(Signals::recorded((0..300).map(|i| SignalFrame::silent(i as f64 * 0.01)), 0.0));
        let axis = Axis { words, origin: 0.0, live: true, until: 2.5 };
        let look = AxisLook::default();
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(400.0, 200.0), 1.0);
        scene.apply(word_axis("axis", cell, &axis, &look, 400.0), 0.0);
        assert!(scene.find(&["axis", "w0"]).is_some());
        assert!(scene.find(&["axis", "w2"]).is_some());
        // 0.1 s between the first two words: no label; 0.7 s before the third: "0.7 s".
        assert!(scene.find(&["axis", "g0"]).is_none());
        assert!(scene.find(&["axis", "g1"]).is_some());
        let list = scene.sample(1.0);
        assert!(!list.shapes.is_empty());
    }
}
