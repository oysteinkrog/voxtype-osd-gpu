//! Words written by pen strokes (MO-80): each letter drawn as the strokes a hand makes, in
//! their order, then left for the real typeface.
//!
//! The strokes come from a single-line pen font, Hershey Script Simplex
//! (`assets/hershey/scripts.jhf`, licence and acknowledgement in `assets/hershey/README.md`).
//! Its letters are open centre lines in drawing order, which an outline font cannot give.
//! [`Writing::new`] places each letter's strokes over the glyph the real font set there
//! ([`WordOutline`]), scaled by cap height and centred on the glyph's pen box, so the hand
//! writes where the type will stand. A letter the pen font lacks (anything past ASCII) is
//! written by tracing its outline instead.
//!
//! [`Writing::with_source`] picks where the strokes come from ([`StrokeSource`]): the
//! Hershey pen font (the default, and what [`Writing::new`] uses), or the real glyph's own
//! centre lines ([`crate::centreline`], EG-29), which follow the face that settles. Those are
//! analysed off the frame: [`StrokeSource::Font`] takes them from the cache and writes a
//! glyph that is not ready yet by tracing its outline, and [`Writing::pending`] says to
//! build the writing again a frame later; [`StrokeSource::FontNow`] analyses on the calling
//! thread, for offline renders and tests. Font strokes keep each glyph's pen order, with
//! its dots, crossbars and marks ([`WrittenStroke::delayed`]) held to the end of the word,
//! the way a hand dots its i's after the word (SW-03), and carry the ink's width along them
//! as a width table relative to the stem ([`Writing::stem`]), so a pen `stem` wide draws the
//! letter's own weight.
//!
//! [`Writing::draw`] draws the strokes up to a pen distance along the whole word, with a
//! bright pen head, and lays the real glyphs under them at an ink that rises from a floor to
//! full as the pen finishes. A look that keeps the reading-line envelope uses a floor of 0.7,
//! so the word is readable the frame it arrives and the writing is an overlay that lands on
//! it.
//!
//! [`PenSchedule`] (SW-01) times the writing when words arrive one after another: one pen,
//! a word at a time, at a speed in px/s clamped so a word takes 160 to 260 ms, faster (up
//! to 2x) the further the pen is behind, and a word it cannot start within 250 ms of its
//! arrival is swept in instead. [`Writing::pen_at`] turns the word's share of that time into
//! the pen's place, eased per stroke with a pause at each lift, and [`Writing::draw_from`]
//! draws the done strokes whole and trims only the live one.
//!
//! [`WetInk`] (SW-03) is the font strokes' own look: [`Writing::draw_wet`] lays each stroke on
//! its letter at the glyph's stem width in the text colour lifted toward white, with a soft
//! nib light in the accent at the pen, and lets each glyph's ink dry to nothing 180 ms after
//! the pen leaves it. [`WriteStyle`] with [`Writing::draw`] stays the Hershey script look.
//!
//! A [`Nib`] (SW-05) makes the wet ink a broad pen's: each stroke is thinner where it runs
//! along the nib's edge and full across it, and the word's voice presses it up to 15 %
//! wider or lighter ([`Writing::draw_wet_voiced`]). Small pools of ink sit at stroke ends
//! and at turns sharper than 60 degrees, and dry with their stroke.
//!
//! [`Pen`] (SW-08) names the pens a look picks from: `Ink` (the wet ink above), `Light` (a
//! round line with a glow that settles and a 40 ms trail of nib light), `Phosphor` (a beam
//! spot with a dim 0.3 s afterglow, leaving swept words to the look's own beam) and `Script`
//! (the Hershey strokes in wet ink with ink bleeding into larger pools).

use std::sync::{Arc, OnceLock};

use crate::canvas::Painter;
use crate::centreline::{self, GlyphCentre, Lookup};
use crate::font::FontRegistry;
use crate::geom::{Color, Point};
use crate::glyph_morph::WordOutline;
use crate::path::{Contour, StrokeStyle, VectorSeg, WidthKnot};

const SCRIPT: &str = include_str!("../../../assets/hershey/scripts.jhf");

/// Hershey units from the cap line to the baseline.
const HERSHEY_CAP: f32 = 21.0;
/// Hershey y of the baseline.
const HERSHEY_BASE: f32 = 9.0;

/// One letter of a single-line pen font, in its own units, y down, baseline at
/// `HERSHEY_BASE`.
#[derive(Clone, Debug, Default)]
pub struct PenGlyph {
    pub left: f32,
    pub right: f32,
    /// Strokes in drawing order, each a polyline the pen draws without lifting.
    pub strokes: Vec<Vec<Point>>,
}

/// A single-line pen font: one [`PenGlyph`] per ASCII character from space to `~`.
#[derive(Clone, Debug)]
pub struct PenFont {
    glyphs: Vec<PenGlyph>,
}

impl PenFont {
    /// Hershey Script Simplex, parsed once.
    pub fn script() -> &'static PenFont {
        static F: OnceLock<PenFont> = OnceLock::new();
        F.get_or_init(|| PenFont::parse(SCRIPT))
    }

    /// Reads James Hurt's format: per glyph a 5 column number, a 3 column vertex count, then
    /// pairs of letters, each `letter - 'R'` a coordinate; the first pair is the left and
    /// right bound, and ` R` lifts the pen.
    pub fn parse(src: &str) -> PenFont {
        let mut glyphs = Vec::new();
        for line in src.lines() {
            let b = line.as_bytes();
            if b.len() < 10 {
                continue;
            }
            let count: usize = line[5..8].trim().parse().unwrap_or(0);
            let pairs = &b[8..];
            let v = |c: u8| c as f32 - b'R' as f32;
            let mut g = PenGlyph { left: v(pairs[0]), right: v(pairs[1]), strokes: vec![Vec::new()] };
            for k in 1..count {
                let (Some(&x), Some(&y)) = (pairs.get(2 * k), pairs.get(2 * k + 1)) else { break };
                if x == b' ' && y == b'R' {
                    g.strokes.push(Vec::new());
                } else if let Some(s) = g.strokes.last_mut() {
                    s.push(Point::new(v(x), v(y)));
                }
            }
            g.strokes.retain(|s| s.len() > 1);
            glyphs.push(g);
        }
        PenFont { glyphs }
    }

    pub fn glyph(&self, c: char) -> Option<&PenGlyph> {
        let i = (c as u32).checked_sub(32)? as usize;
        self.glyphs.get(i).filter(|_| c.is_ascii())
    }
}

/// Where a [`Writing`]'s strokes come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StrokeSource {
    /// Hershey Script Simplex, placed over each glyph; outline traces past ASCII.
    #[default]
    Hershey,
    /// The glyph's own centre lines from the cache. A glyph not analysed yet is queued and
    /// written by tracing its outline; [`Writing::pending`] is then true.
    Font,
    /// The glyph's own centre lines, analysed on this thread when not cached.
    FontNow,
}

/// One stroke placed in layer units, with where it starts along the whole word's pen path.
#[derive(Clone, Debug)]
struct Stroke {
    contour: Contour,
    /// Pen distance at the stroke's start, pen lifts counted.
    start: f32,
    /// The glyph of the word it writes.
    glyph: usize,
    delayed: bool,
}

/// A stroke of a [`Writing`], for looks that order or style strokes themselves.
#[derive(Clone, Copy, Debug)]
pub struct WrittenStroke<'a> {
    /// The glyph of the word it writes.
    pub glyph: usize,
    /// In layer units. Font strokes carry a width table: factors of [`Writing::stem`].
    pub contour: &'a Contour,
    /// Pen distance at the stroke's start.
    pub start: f32,
    /// A dot, crossbar or mark (font strokes only), written after every base stroke of the
    /// word.
    pub delayed: bool,
}

/// A word to be written. See the module docs.
#[derive(Clone, Debug)]
pub struct Writing {
    pub word: WordOutline,
    strokes: Vec<Stroke>,
    /// Pen distance where each glyph's first stroke starts and its last base stroke ends (its
    /// last delayed stroke when it has only those): the type's ink rises over this span.
    glyph_begin: Vec<f32>,
    glyph_end: Vec<f32>,
    total: f32,
    /// Each glyph's centre-line analysis, for the font sources.
    centres: Vec<Option<Arc<GlyphCentre>>>,
    pending: bool,
    /// Word id the drawn glyphs carry (`GlyphInstance::word`).
    word_id: u32,
}

/// How [`Writing::draw`] paints.
#[derive(Clone, Copy, Debug)]
pub struct WriteStyle {
    pub pen: StrokeStyle,
    /// The pen head: a dot this wide in this colour; 0 for none.
    pub head: f32,
    pub head_color: Color,
    /// The real glyphs' colour.
    pub ink: Color,
    /// Ink of a glyph before the pen reaches it, rising to full as its last stroke ends.
    pub ink_floor: f32,
}

impl Writing {
    /// Places the pen strokes of `text` over `word`, which must be `text` shaped (glyph
    /// clusters index `text`). `lift` is the pen distance a lift between strokes costs, so
    /// strokes are not drawn back to back.
    pub fn new(fonts: &FontRegistry, text: &str, word: WordOutline, lift: f32) -> Writing {
        Writing::with_source(fonts, text, word, lift, StrokeSource::Hershey)
    }

    /// [`Writing::new`] with the strokes from `source`.
    pub fn with_source(fonts: &FontRegistry, text: &str, word: WordOutline, lift: f32, source: StrokeSource) -> Writing {
        let pen = PenFont::script();
        let mut placed_all: Vec<(usize, Contour, bool)> = Vec::new();
        let mut centres = Vec::with_capacity(word.glyphs.len());
        let mut pending = false;
        for (i, g) in word.glyphs.iter().enumerate() {
            let c = text.get(g.cluster as usize..).and_then(|s| s.chars().next()).unwrap_or(' ');
            let centre = match (source, g.has_ink(), fonts.get(g.font)) {
                (StrokeSource::Font, true, Some(face)) => match centreline::lookup(&face, g.glyph) {
                    Lookup::Ready(x) => Some(x),
                    Lookup::Pending => {
                        pending = true;
                        None
                    }
                },
                (StrokeSource::FontNow, true, Some(face)) => Some(centreline::get_now(&face, g.glyph)),
                _ => None,
            };
            let cap = cap_height(fonts, g.font, g.size);
            let k = cap / HERSHEY_CAP;
            let hershey = if source == StrokeSource::Hershey { pen.glyph(c).filter(|p| !p.strokes.is_empty()) } else { None };
            let placed: Vec<(Contour, bool)> = match (&centre, hershey) {
                (Some(cl), _) => cl
                    .strokes
                    .iter()
                    .map(|st| {
                        let mut pts: Vec<Point> =
                            st.points.iter().map(|q| Point::new(g.pos.x + q.x * g.size, g.pos.y + q.y * g.size)).collect();
                        let mut widths = st.widths.clone();
                        if st.closed && pts.len() > 1 {
                            // Round the loop back to its start.
                            pts.push(pts[0]);
                            widths.push(widths[0]);
                        }
                        (widen(Contour::polyline(&pts, false), &pts, &widths, cl.stem), st.delayed)
                    })
                    .collect(),
                (None, Some(p)) => {
                    // Centre the pen letter's box on the glyph's ink, baseline on baseline.
                    let mid_pen = 0.5 * (p.left + p.right);
                    let mid_ink = if g.has_ink() { g.ink.x + 0.5 * g.ink.w } else { g.pos.x + 0.5 * g.advance };
                    p.strokes
                        .iter()
                        .map(|s| {
                            let pts: Vec<Point> = s
                                .iter()
                                .map(|q| Point::new(mid_ink + (q.x - mid_pen) * k, g.pos.y + (q.y - HERSHEY_BASE) * k))
                                .collect();
                            (Contour::polyline(&smooth(&pts), false), false)
                        })
                        .collect()
                }
                // No pen letter, or its centre lines are not ready: trace the outline,
                // contour by contour.
                (None, None) => g.outline.contours.iter().map(|c| (c.clone(), false)).collect(),
            };
            placed_all.extend(placed.into_iter().filter(|(c, _)| c.length() > 0.0).map(|(c, d)| (i, c, d)));
            centres.push(centre);
        }
        // Base strokes glyph by glyph, then the delayed ones (dots, crossbars, marks) at the
        // end of the word, the way a hand dots its i's and crosses its t's (SW-03). Only font
        // strokes are ever delayed, so Hershey keeps its own order.
        let n = word.glyphs.len();
        let mut strokes = Vec::with_capacity(placed_all.len());
        let mut glyph_begin = vec![f32::NAN; n];
        let mut glyph_end = vec![f32::NAN; n];
        let mut at = 0.0f32;
        for late in [false, true] {
            for (i, c, delayed) in placed_all.iter().filter(|t| t.2 == late) {
                let len = c.length();
                if glyph_begin[*i].is_nan() {
                    glyph_begin[*i] = at;
                }
                strokes.push(Stroke { contour: c.clone(), start: at, glyph: *i, delayed: *delayed });
                at += len + lift;
                if !late || glyph_end[*i].is_nan() {
                    glyph_end[*i] = at;
                }
            }
            if !late {
                // A glyph with no strokes holds its place after the glyph before it.
                let mut last = 0.0f32;
                for i in 0..n {
                    if glyph_end[i].is_nan() && !placed_all.iter().any(|t| t.0 == i) {
                        (glyph_begin[i], glyph_end[i]) = (last, last);
                    }
                    if !glyph_end[i].is_nan() {
                        last = glyph_end[i];
                    }
                }
            }
        }
        Writing { word, strokes, glyph_begin, glyph_end, total: at, centres, pending, word_id: crate::display::NO_WORD }
    }

    /// The part of this writing that writes glyphs `range` (SW-06), for a revision that
    /// rewrites only the letters that changed: their strokes in the same order with the same
    /// lifts, from pen distance 0, and every other glyph counted as written before the pen
    /// starts.
    pub fn glyphs(&self, range: std::ops::Range<usize>) -> Writing {
        let n = self.word.glyphs.len();
        let mut strokes = Vec::new();
        let mut glyph_begin = vec![f32::NAN; n];
        let mut glyph_end = vec![f32::NAN; n];
        let mut base = vec![false; n];
        let mut at = 0.0f32;
        for (j, s) in self.strokes.iter().enumerate() {
            if !range.contains(&s.glyph) {
                continue;
            }
            let len = s.contour.length();
            let next = self.strokes.get(j + 1).map_or(self.total, |x| x.start);
            let lift = (next - s.start - len).max(0.0);
            let i = s.glyph;
            if glyph_begin[i].is_nan() {
                glyph_begin[i] = at;
            }
            strokes.push(Stroke { start: at, ..s.clone() });
            at += len + lift;
            // A glyph is done at the end of its last base stroke, or of its last delayed one
            // when it has only those.
            if !s.delayed || !base[i] {
                glyph_end[i] = at;
            }
            base[i] |= !s.delayed;
        }
        let mut last = 0.0f32;
        for i in 0..n {
            if !range.contains(&i) {
                // Written already: full from the first moment.
                (glyph_begin[i], glyph_end[i]) = (-1.0, -1.0);
            } else if glyph_end[i].is_nan() {
                // No strokes: done with the glyph before it.
                (glyph_begin[i], glyph_end[i]) = (last, last);
            } else {
                last = glyph_end[i];
            }
        }
        Writing {
            word: self.word.clone(),
            strokes,
            glyph_begin,
            glyph_end,
            total: at,
            centres: self.centres.clone(),
            pending: self.pending,
            word_id: self.word_id,
        }
    }

    /// The word id the drawn glyphs carry, so per-word tools (the reading-line envelope)
    /// follow them.
    pub fn word_id(mut self, id: u32) -> Writing {
        self.word_id = id;
        self
    }

    /// Whether some glyph's centre lines were still being analysed ([`StrokeSource::Font`]):
    /// it is written by its outline, and building the writing again later gives its strokes.
    pub fn pending(&self) -> bool {
        self.pending
    }

    /// Glyph `i`'s centre-line analysis (font sources): its stem, coverage and strokes in em.
    pub fn centre(&self, i: usize) -> Option<&Arc<GlyphCentre>> {
        self.centres.get(i).and_then(|c| c.as_ref())
    }

    /// Glyph `i`'s stem width in layer units, from its centre lines; `None` for a Hershey
    /// or outline-traced glyph.
    pub fn stem(&self, i: usize) -> Option<f32> {
        Some(self.centre(i)?.stem * self.word.glyphs.get(i)?.size)
    }

    /// Every stroke in pen order.
    pub fn strokes(&self) -> impl Iterator<Item = WrittenStroke<'_>> {
        self.strokes.iter().map(|t| WrittenStroke { glyph: t.glyph, contour: &t.contour, start: t.start, delayed: t.delayed })
    }

    /// The pen distance of the whole word.
    pub fn length(&self) -> f32 {
        self.total
    }

    /// Draws the word with the pen `s` along its path (0 nothing written, [`Writing::length`]
    /// all of it).
    pub fn draw(&self, p: &mut Painter, s: f32, style: &WriteStyle) {
        let (done, along) = self.split(s);
        // The raw distance sets the type's ink, so a pen resting in a lift reads as before.
        self.draw_at(p, done, along, s, style);
    }

    /// Where pen distance `s` falls: the index of the first stroke not finished, and how far
    /// along it the pen is (0 in a lift before it). [`Writing::draw`] is
    /// `draw_from(split(s))`.
    pub fn split(&self, s: f32) -> (usize, f32) {
        let done = self.strokes.partition_point(|t| t.start + t.contour.length() <= s);
        let along = self.strokes.get(done).map_or(0.0, |t| (s - t.start).max(0.0));
        (done, along)
    }

    /// The pen's place when the word is `u` (0 to 1) through its writing time: the time is
    /// shared out by pen distance, each stroke eases in and out, and the pen rests over each
    /// lift. Returns what [`Writing::draw_from`] takes.
    pub fn pen_at(&self, u: f32) -> (usize, f32) {
        let s = u.clamp(0.0, 1.0) * self.total;
        let i = self.strokes.partition_point(|t| t.start <= s);
        let Some(t) = i.checked_sub(1).and_then(|k| self.strokes.get(k)) else { return (0, 0.0) };
        let len = t.contour.length();
        if s >= t.start + len {
            // In the lift after it.
            return (i, 0.0);
        }
        let x = ((s - t.start) / len.max(1e-6)).clamp(0.0, 1.0);
        // Half linear, half smoothstep: slower at the ends of a stroke, never stopped.
        let e = 0.5 * x + 0.5 * x * x * (3.0 - 2.0 * x);
        (i - 1, e * len)
    }

    /// Draws the word with strokes before `done` whole and stroke `done` written `along`
    /// its length, the pen head there. The done strokes are drawn as built, so only the
    /// live stroke is trimmed each frame. `done` at the stroke count draws it all.
    pub fn draw_from(&self, p: &mut Painter, done: usize, along: f32, style: &WriteStyle) {
        let s = match self.strokes.get(done) {
            Some(t) => t.start + along.clamp(0.0, t.contour.length()),
            None => self.total,
        };
        self.draw_at(p, done, along, s, style);
    }

    /// [`Writing::draw_from`] with the type's ink taken at pen distance `s`.
    fn draw_at(&self, p: &mut Painter, done: usize, along: f32, s: f32, style: &WriteStyle) {
        // The type first, so the pen writes over it.
        if style.ink.a > 0.0 {
            for (i, g) in self.word.glyphs.iter().enumerate() {
                if !g.has_ink() {
                    continue;
                }
                let (begin, end) = (self.glyph_begin[i], self.glyph_end[i]);
                let k = ((s - begin) / (end - begin).max(1e-3)).clamp(0.0, 1.0);
                let ink = style.ink_floor + (1.0 - style.ink_floor) * k;
                p.glyph(crate::display::GlyphInstance { word: self.word_id, ..g.instance(style.ink.fade(ink)) });
            }
        }
        let done = done.min(self.strokes.len());
        for t in &self.strokes[..done] {
            p.stroke(&t.contour, &style.pen);
        }
        let Some(t) = self.strokes.get(done) else { return };
        let len = t.contour.length();
        let along = along.clamp(0.0, len);
        if along <= 0.0 {
            return;
        }
        if along >= len {
            p.stroke(&t.contour, &style.pen);
            return;
        }
        p.stroke(&t.contour.trim(0.0, along), &style.pen);
        if let (Some((at, _)), true) = (t.contour.at(along), style.head > 0.0) {
            p.stroke(
                &Contour::polyline(&[at, Point::new(at.x + 0.01, at.y)], false),
                &StrokeStyle::solid(style.head, style.head_color),
            );
        }
    }

    /// Draws the word swept in with no pen, `k` from 0 to 1: glyph by glyph left to right,
    /// each glyph's strokes and ink rising from nothing (ink from the floor) to full.
    pub fn draw_swept(&self, p: &mut Painter, k: f32, style: &WriteStyle) {
        let n = self.word.glyphs.len().max(1) as f32;
        // Glyph i rises over 3 of n + 2 equal steps, starting at step i.
        let glyph_k = |i: usize| ((k * (n + 2.0) - i as f32) / 3.0).clamp(0.0, 1.0);
        if style.ink.a > 0.0 {
            for (i, g) in self.word.glyphs.iter().enumerate() {
                if g.has_ink() {
                    let ink = style.ink_floor + (1.0 - style.ink_floor) * glyph_k(i);
                    p.glyph(crate::display::GlyphInstance { word: self.word_id, ..g.instance(style.ink.fade(ink)) });
                }
            }
        }
        for t in &self.strokes {
            let a = glyph_k(t.glyph);
            if a <= 0.0 {
                continue;
            }
            let pen = StrokeStyle { c0: style.pen.c0.fade(a), c1: style.pen.c1.fade(a), ..style.pen };
            p.stroke(&t.contour, &pen);
        }
    }
}

/// Wet ink (SW-03): the pen writes each glyph along its own centre lines at its stem width,
/// so the stroke lies on the letter's ink, in the text colour lifted toward white, and dries
/// back into the type. Only the nib light takes the accent. Draw with [`Writing::draw_wet`];
/// a look that wants the Hershey script draws with a [`WriteStyle`] instead.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WetInk {
    /// The wet stroke's colour.
    pub ink: Color,
    /// The nib light: a soft blob this many px across at the pen, 0 for none.
    pub nib: f32,
    pub nib_color: Color,
    /// A stroke dries to nothing over this many seconds once its glyph is done.
    pub dry_secs: f64,
    /// The wet shine: a line down the middle of the stroke, a third of its width, in
    /// `sheen_color` at this alpha, drying with the stroke; 0 for none.
    pub sheen: f32,
    pub sheen_color: Color,
    /// The real glyphs' colour under the ink, transparent when the look draws the type
    /// itself; their ink rises from `ink_floor` to full as the pen finishes each glyph.
    pub type_ink: Color,
    pub ink_floor: f32,
    /// A broad nib that sets the width by the stroke's direction and the voice (SW-05);
    /// `None` for a round pen at the glyph's stem width.
    pub pen: Option<Nib>,
    /// Ink pools at stroke ends and sharp turns: dots this many times the stroke's width
    /// there, drying with the stroke; 0 for none.
    pub pool: f32,
    /// A glow this many px around each wet stroke in `nib_color`, settling as the stroke
    /// dries; 0 for none.
    pub glow: f32,
    /// A light trail behind the nib: the last this-many seconds of the live stroke in
    /// `nib_color`; 0 for none.
    pub trail_secs: f64,
    /// Whether a swept word shows wet ink. A look that sweeps words in its own way (a
    /// beam) sets it false.
    pub swept_ink: bool,
}

/// The named pens a look picks from (SW-08): each a [`WetInk`] in the look's text and
/// accent colours, and where its strokes come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pen {
    /// Wet ink with the [`Nib::INK`] pen at 30 degrees ([`WetInk::new`]).
    #[default]
    Ink,
    /// A light pen: a round line in the text colour with a glow that settles, and a nib
    /// light in the accent trailing 40 ms behind it. No pools, no shine.
    Light,
    /// A beam: the spot as the nib, a dim round line with an afterglow that fades over
    /// 0.3 s, so the word never reads bold, no shine or pools. A swept word shows no ink, so the look's own beam (or the
    /// type) brings it in.
    Phosphor,
    /// Script: the Hershey script strokes over the type in wet ink, with ink that bleeds
    /// into larger pools at stroke ends and turns.
    Script,
}

impl Pen {
    /// This pen's wet ink for `text` with the nib light in `accent`.
    pub fn wet(self, text: Color, accent: Color) -> WetInk {
        let ink = WetInk::new(text, accent);
        match self {
            Pen::Ink => ink,
            Pen::Light => WetInk { nib: 7.0, sheen: 0.0, pen: None, pool: 0.0, glow: 4.0, trail_secs: 0.04, ..ink },
            Pen::Phosphor => WetInk {
                ink: text.fade(0.45),
                nib: 4.0,
                dry_secs: 0.3,
                sheen: 0.0,
                pen: None,
                pool: 0.0,
                glow: 1.5,
                swept_ink: false,
                ..ink
            },
            Pen::Script => WetInk { pool: 1.8, ..ink },
        }
    }

    /// Whether the strokes are the Hershey script's rather than the glyph's own centre
    /// lines.
    pub fn script(self) -> bool {
        self == Pen::Script
    }
}

/// A broad nib (SW-05). A stroke heading in direction `d` is `min + (1 - min) * |sin(d -
/// angle)|` of the pen's width: thinnest along the nib's edge, full across it. Directions
/// are angles on the page with y up, so 30 degrees is the edge rising to the right, as a
/// right-handed pen holds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Nib {
    /// The nib's edge, in radians.
    pub angle: f32,
    /// The width share of a stroke along the edge.
    pub min: f32,
    /// How far the voice moves the width either way: 0.15 is 15 % wider for the loudest
    /// voice and 15 % lighter for the quietest.
    pub pressure_gain: f32,
}

impl Nib {
    /// The wet-ink pen: its edge at 30 degrees, 55 % wide along it, 15 % pressure.
    pub const INK: Nib = Nib { angle: std::f32::consts::FRAC_PI_6, min: 0.55, pressure_gain: 0.15 };

    /// The width factor of a stroke heading `dir` radians (y up).
    pub fn width(&self, dir: f32) -> f32 {
        self.min + (1.0 - self.min) * (dir - self.angle).sin().abs()
    }

    /// The width factor of a `voice` from 0 (quiet) to 1 (loud); 0.5 presses neither way.
    pub fn pressure(&self, voice: f32) -> f32 {
        1.0 + self.pressure_gain * (2.0 * voice.clamp(0.0, 1.0) - 1.0)
    }
}

impl WetInk {
    /// The text colour lifted 15 % toward white, drying over 180 ms, with a shine lifted
    /// 60 % toward white and a 5 px nib light in `accent`, written with the [`Nib::INK`] pen
    /// and pools 1.3 times the stroke's width. The type is not drawn.
    pub fn new(text: Color, accent: Color) -> WetInk {
        let lift = |k: f32| {
            let l = |c: f32| c + k * (1.0 - c);
            Color { r: l(text.r), g: l(text.g), b: l(text.b), a: text.a }
        };
        WetInk {
            ink: lift(0.15),
            nib: 5.0,
            nib_color: accent,
            dry_secs: 0.18,
            sheen: 0.45,
            sheen_color: lift(0.6),
            type_ink: Color::TRANSPARENT,
            ink_floor: 0.7,
            pen: Some(Nib::INK),
            pool: 1.3,
            glow: 0.0,
            trail_secs: 0.0,
            swept_ink: true,
        }
    }

    /// The scene time after which a word in `slot` shows no wet ink.
    pub fn until(&self, slot: &PenSlot) -> f64 {
        slot.end + self.dry_secs
    }
}

impl Writing {
    /// Each stroke's wetness at scene time `t` for a word written in `slot`: 0 before the pen
    /// (or the sweep) reaches it, 1 from then until its glyph is done (its own end, for a
    /// delayed stroke), then falling to 0 over [`WetInk::dry_secs`].
    pub fn wetness(&self, slot: &PenSlot, t: f64, wet: &WetInk) -> Vec<f32> {
        let dry = |from: f64| -> f32 {
            let x = ((t - from) / wet.dry_secs.max(1e-6)).clamp(0.0, 1.0) as f32;
            1.0 - x * x * (3.0 - 2.0 * x)
        };
        if slot.swept && !wet.swept_ink {
            return vec![0.0; self.strokes.len()];
        }
        if slot.swept {
            let n = self.word.glyphs.len().max(1) as f32;
            let k = (((t - slot.start) / (slot.end - slot.start).max(1e-9)) as f32).clamp(0.0, 1.0);
            return self
                .strokes
                .iter()
                .map(|s| if t < slot.start { 0.0 } else { ((k * (n + 2.0) - s.glyph as f32) / 3.0).clamp(0.0, 1.0) * dry(slot.end) })
                .collect();
        }
        let dur = slot.end - slot.start;
        let total = self.total.max(1e-6) as f64;
        let when = |s: f32| slot.start + dur * (s as f64 / total).min(1.0);
        // Where each glyph's base strokes end.
        let mut base_end = vec![0.0f32; self.word.glyphs.len()];
        for s in self.strokes.iter().filter(|s| !s.delayed) {
            base_end[s.glyph] = base_end[s.glyph].max(s.start + s.contour.length());
        }
        self.strokes
            .iter()
            .map(|s| {
                let end = s.start + s.contour.length();
                if t < when(s.start) {
                    return 0.0;
                }
                dry(when(end.max(base_end[s.glyph])))
            })
            .collect()
    }

    /// Draws the word in wet ink at scene time `t`, written in `slot` (one word of a
    /// [`PenSchedule`]): written strokes at their wetness, the live one trimmed at the pen
    /// with the nib light there, a swept word's strokes rising glyph by glyph.
    pub fn draw_wet(&self, p: &mut Painter, t: f64, slot: &PenSlot, wet: &WetInk) {
        self.draw_wet_voiced(p, t, slot, wet, 0.5);
    }

    /// [`Writing::draw_wet`] with the word's `voice`, 0 (quiet) to 1 (loud), pressing the
    /// nib ([`Nib::pressure`]). Freeze it per word, so a word keeps its weight as it dries.
    pub fn draw_wet_voiced(&self, p: &mut Painter, t: f64, slot: &PenSlot, wet: &WetInk, voice: f32) {
        let phase = slot.phase(t);
        let press = wet.pen.map_or(1.0, |n| n.pressure(voice));
        if wet.type_ink.a > 0.0 {
            let glyph_k: Box<dyn Fn(usize) -> f32> = match phase {
                PenPhase::Waiting => Box::new(|_| 0.0),
                PenPhase::Done => Box::new(|_| 1.0),
                PenPhase::Sweeping(k) => {
                    let n = self.word.glyphs.len().max(1) as f32;
                    Box::new(move |i| ((k * (n + 2.0) - i as f32) / 3.0).clamp(0.0, 1.0))
                }
                PenPhase::Writing(u) => {
                    let (done, along) = self.pen_at(u);
                    let s = self.strokes.get(done).map_or(self.total, |t| t.start + along);
                    Box::new(move |i| {
                        let (b, e) = (self.glyph_begin[i], self.glyph_end[i]);
                        ((s - b) / (e - b).max(1e-3)).clamp(0.0, 1.0)
                    })
                }
            };
            for (i, g) in self.word.glyphs.iter().enumerate() {
                if g.has_ink() {
                    let ink = wet.ink_floor + (1.0 - wet.ink_floor) * glyph_k(i);
                    p.glyph(crate::display::GlyphInstance { word: self.word_id, ..g.instance(wet.type_ink.fade(ink)) });
                }
            }
        }
        let alpha = self.wetness(slot, t, wet);
        let live = match phase {
            PenPhase::Writing(u) => Some(self.pen_at(u)),
            _ => None,
        };
        let fallback = self.fallback_stem();
        for (j, s) in self.strokes.iter().enumerate() {
            let width = self.stem(s.glyph).unwrap_or(fallback);
            match live {
                Some((done, _)) if j > done => break,
                Some((done, along)) if j == done => {
                    let len = s.contour.length();
                    let along = along.clamp(0.0, len);
                    if along <= 0.0 {
                        break;
                    }
                    let c = nibbed(&s.contour, wet.pen, press);
                    let piece = if along >= len { c.clone() } else { c.trim(0.0, along) };
                    pools(p, &c, along, width, 1.0, wet);
                    self.wet_stroke(p, &piece, width, 1.0, wet);
                    if wet.trail_secs > 0.0 {
                        // The pen's distance over the trail's time, at its average speed.
                        let speed = self.total as f64 / (slot.end - slot.start).max(1e-6);
                        let from = (along - (speed * wet.trail_secs) as f32).max(0.0);
                        p.stroke(
                            &c.trim(from, along),
                            &StrokeStyle::solid(0.6 * width, wet.nib_color).glow(0.5 * wet.nib),
                        );
                    }
                    if let (Some((at, _)), true) = (s.contour.at(along), wet.nib > 0.0) {
                        p.stroke(
                            &Contour::polyline(&[at, Point::new(at.x + 0.01, at.y)], false),
                            &StrokeStyle::solid(0.5 * wet.nib, wet.nib_color).glow(0.25 * wet.nib),
                        );
                    }
                    break;
                }
                _ => {
                    if alpha[j] > 0.0 {
                        let c = nibbed(&s.contour, wet.pen, press);
                        pools(p, &c, f32::INFINITY, width, alpha[j], wet);
                        self.wet_stroke(p, &c, width, alpha[j], wet);
                    }
                }
            }
        }
    }

    /// One stroke of wet ink at `wetness`, with its shine.
    fn wet_stroke(&self, p: &mut Painter, c: &Contour, width: f32, wetness: f32, wet: &WetInk) {
        if wet.glow > 0.0 {
            p.stroke(c, &StrokeStyle::solid(width, wet.nib_color.fade(0.5 * wetness)).glow(wet.glow * wetness));
        }
        p.stroke(c, &StrokeStyle::solid(width, wet.ink.fade(wetness)));
        if wet.sheen > 0.0 {
            p.stroke(c, &StrokeStyle::solid(width / 3.0, wet.sheen_color.fade(wet.sheen * wetness)));
        }
    }

    /// The pen width for a glyph with no centre lines (Hershey, or traced while its
    /// analysis is pending): the word's widest stem, else a twelfth of the em.
    fn fallback_stem(&self) -> f32 {
        let stem = (0..self.word.glyphs.len()).filter_map(|i| self.stem(i)).fold(0.0, f32::max);
        if stem > 0.0 {
            stem
        } else {
            self.word.glyphs.first().map_or(1.0, |g| g.size / 12.0)
        }
    }
}

/// How a [`PenSchedule`] paces the pen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenParams {
    /// Pen speed in layer px per second, lifts counted ([`Writing::length`]).
    pub speed: f32,
    /// A word's writing time at speed is clamped to this range, in seconds.
    pub min_secs: f64,
    pub max_secs: f64,
    /// The pen runs up to this many times faster the further it is behind.
    pub max_boost: f64,
    /// A word the pen cannot start within this many seconds of its arrival is swept.
    pub sweep_after: f64,
    /// How long a sweep takes.
    pub sweep_secs: f64,
}

impl Default for PenParams {
    fn default() -> PenParams {
        PenParams { speed: 900.0, min_secs: 0.16, max_secs: 0.26, max_boost: 2.0, sweep_after: 0.25, sweep_secs: 0.08 }
    }
}

/// When one word is written. Times are scene seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenSlot {
    pub arrive: f64,
    /// The pen starts it, or the sweep does.
    pub start: f64,
    pub end: f64,
    /// Swept in with no pen ([`Writing::draw_swept`]).
    pub swept: bool,
}

/// Where a word is at a moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PenPhase {
    /// Arrived (or not yet), the pen not on it.
    Waiting,
    /// The pen is on it, this far (0 to 1) through its writing time: [`Writing::pen_at`].
    Writing(f32),
    /// Being swept in, this far (0 to 1): [`Writing::draw_swept`].
    Sweeping(f32),
    Done,
}

impl PenSlot {
    pub fn phase(&self, t: f64) -> PenPhase {
        if t < self.start {
            return PenPhase::Waiting;
        }
        if t >= self.end {
            return PenPhase::Done;
        }
        let k = ((t - self.start) / (self.end - self.start).max(1e-9)) as f32;
        if self.swept {
            PenPhase::Sweeping(k)
        } else {
            PenPhase::Writing(k)
        }
    }
}

/// One pen for a run of words (SW-01): a pure function of the words' arrival times and pen
/// lengths, in arrival order.
///
/// The pen takes each word when it is free and the word has arrived. A word's time is its
/// length over [`PenParams::speed`], clamped to `min_secs..max_secs`, divided by a boost
/// that rises from 1 with no wait to `max_boost` at a wait of `sweep_after`. A word that
/// would wait longer than `sweep_after` is swept instead, from its arrival, over
/// `sweep_secs`, and the pen goes on to the next. Which happens is known when the word
/// arrives, because the pen's time depends only on earlier words, so [`PenSchedule::push`]
/// adds words as they come and never changes a slot already given. With the defaults every
/// word is done by its arrival plus 0.51 s.
#[derive(Clone, Debug)]
pub struct PenSchedule {
    params: PenParams,
    slots: Vec<PenSlot>,
    /// When the pen is free.
    free: f64,
}

impl PenSchedule {
    pub fn new(params: PenParams) -> PenSchedule {
        PenSchedule { params, slots: Vec::new(), free: f64::NEG_INFINITY }
    }

    /// The schedule of `words` (arrival, pen length), in order.
    pub fn plan(params: PenParams, words: &[(f64, f32)]) -> PenSchedule {
        let mut s = PenSchedule::new(params);
        for &(arrive, length) in words {
            s.push(arrive, length);
        }
        s
    }

    pub fn params(&self) -> &PenParams {
        &self.params
    }

    /// Adds the next word and returns its index.
    pub fn push(&mut self, arrive: f64, length: f32) -> usize {
        let p = &self.params;
        let start = arrive.max(self.free);
        let wait = start - arrive;
        let slot = if wait > p.sweep_after {
            PenSlot { arrive, start: arrive, end: arrive + p.sweep_secs, swept: true }
        } else {
            let base = (length.max(0.0) as f64 / (p.speed.max(1e-3) as f64)).clamp(p.min_secs, p.max_secs);
            let boost = 1.0 + (p.max_boost - 1.0).max(0.0) * (wait / p.sweep_after.max(1e-9)).clamp(0.0, 1.0);
            let end = start + base / boost;
            self.free = end;
            PenSlot { arrive, start, end, swept: false }
        };
        self.slots.push(slot);
        self.slots.len() - 1
    }

    pub fn slot(&self, i: usize) -> Option<&PenSlot> {
        self.slots.get(i)
    }

    pub fn slots(&self) -> &[PenSlot] {
        &self.slots
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The scene time after which nothing changes.
    pub fn until(&self) -> f64 {
        self.slots.iter().map(|s| s.end).fold(f64::NEG_INFINITY, f64::max)
    }
}

/// The heading of a segment at its start and at its end, in radians with y up.
fn headings(s: &VectorSeg) -> (f32, f32) {
    let dir = |a: Point, b: Point| (a.y - b.y).atan2(b.x - a.x);
    let near = |a: Point, b: Point| (a.x - b.x).abs() + (a.y - b.y).abs() < 1e-4;
    let start = if near(s.p0, s.p1) { dir(s.p0, s.p2) } else { dir(s.p0, s.p1) };
    let end = if near(s.p1, s.p2) { dir(s.p0, s.p2) } else { dir(s.p1, s.p2) };
    (start, end)
}

/// Whether segment `b` carries on from `a` without a lift.
fn joined(a: &VectorSeg, b: &VectorSeg) -> bool {
    (a.p2.x - b.p0.x).abs() + (a.p2.y - b.p0.y).abs() < 1e-3
}

/// `c` with each segment end's width multiplied by `nib`'s factor for its heading and by
/// `press`. Where two segments join, both take the mean of their two factors, so the width
/// turns with the stroke instead of stepping. Without a nib, `c` as it is.
fn nibbed(c: &Contour, nib: Option<Nib>, press: f32) -> Contour {
    let Some(nib) = nib else { return c.clone() };
    let heads: Vec<(f32, f32)> = c.segs.iter().map(headings).collect();
    let mut out = c.clone();
    for i in 0..out.segs.len() {
        let mut f0 = nib.width(heads[i].0);
        let mut f1 = nib.width(heads[i].1);
        if i > 0 && joined(&c.segs[i - 1], &c.segs[i]) {
            f0 = 0.5 * (f0 + nib.width(heads[i - 1].1));
        }
        if i + 1 < c.segs.len() && joined(&c.segs[i], &c.segs[i + 1]) {
            f1 = 0.5 * (f1 + nib.width(heads[i + 1].0));
        }
        let s = &mut out.segs[i];
        s.w = [s.w[0] * f0 * press, s.w[1] * f1 * press];
    }
    out
}

/// A turn sharper than this between joined segments gets an ink pool.
const POOL_TURN: f32 = std::f32::consts::FRAC_PI_3;

/// Where stroke `c` pools ink up to pen distance `upto` (its arc length), with the width
/// factor there: at its start, at each turn sharper than 60 degrees the pen has passed, and
/// at its end once reached.
fn pool_points(c: &Contour, upto: f32) -> Vec<(Point, f32)> {
    let (Some(first), Some(last)) = (c.segs.first(), c.segs.last()) else { return Vec::new() };
    let mut out = vec![(first.p0, first.w[0])];
    for pair in c.segs.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if a.s1 > upto {
            break;
        }
        let turn = (headings(a).1 - headings(b).0).rem_euclid(std::f32::consts::TAU);
        let turn = turn.min(std::f32::consts::TAU - turn);
        if joined(a, b) && turn > POOL_TURN {
            out.push((a.p2, a.w[1].max(b.w[0])));
        }
    }
    if upto >= last.s1 {
        out.push((last.p2, last.w[1]));
    }
    out
}

/// Draws the ink pools of stroke `c` up to `upto` ([`pool_points`]), each `wet.pool` times
/// the stroke's width there (`stem` times the segment's factor), at `wetness`.
fn pools(p: &mut Painter, c: &Contour, upto: f32, stem: f32, wetness: f32, wet: &WetInk) {
    if wet.pool <= 0.0 || wetness <= 0.0 {
        return;
    }
    let ink = wet.ink.fade(wetness);
    for (at, w) in pool_points(c, upto) {
        let dot = Contour::polyline(&[at, Point::new(at.x + 0.01, at.y)], false);
        p.stroke(&dot, &StrokeStyle::solid(wet.pool * stem * w, ink));
    }
}

/// `c`, the polyline through `pts`, with the ink's `widths` at those points as factors of
/// `stem` (both in em).
fn widen(c: Contour, pts: &[Point], widths: &[f32], stem: f32) -> Contour {
    if stem <= 0.0 || widths.len() != pts.len() || pts.is_empty() {
        return c;
    }
    let mut knots = vec![WidthKnot::new(0.0, widths[0] / stem)];
    let mut s = 0.0;
    for i in 1..pts.len() {
        let d = ((pts[i].x - pts[i - 1].x).powi(2) + (pts[i].y - pts[i - 1].y).powi(2)).sqrt();
        if d <= 0.0 {
            continue;
        }
        s += d;
        knots.push(WidthKnot::new(s, widths[i] / stem));
    }
    c.widen(&knots)
}

/// The font's cap height at `size`, or 0.7 em when it does not say.
fn cap_height(fonts: &FontRegistry, font: u32, size: f32) -> f32 {
    use skrifa::instance::{LocationRef, Size};
    use skrifa::MetadataProvider;
    fonts
        .get(font)
        .and_then(|f| f.skrifa().metrics(Size::new(size), LocationRef::default()).cap_height)
        .filter(|c| *c > 0.0)
        .unwrap_or(0.7 * size)
}

/// Two rounds of Chaikin corner cutting, keeping the ends: the pen font's coarse points
/// become a smooth hand-drawn line.
fn smooth(pts: &[Point]) -> Vec<Point> {
    let mut cur = pts.to_vec();
    for _ in 0..2 {
        if cur.len() < 3 {
            return cur;
        }
        let mut next = Vec::with_capacity(2 * cur.len());
        next.push(cur[0]);
        for w in cur.windows(2) {
            let (a, b) = (w[0], w[1]);
            next.push(Point::new(0.75 * a.x + 0.25 * b.x, 0.75 * a.y + 0.25 * b.y));
            next.push(Point::new(0.25 * a.x + 0.75 * b.x, 0.25 * a.y + 0.75 * b.y));
        }
        next.push(*cur.last().unwrap());
        cur = next;
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::TextStyle;
    use crate::text::{ShapedLayout, TextLayout};

    #[test]
    fn script_font_parses_every_ascii_letter() {
        let f = PenFont::script();
        for c in ('a'..='z').chain('A'..='Z') {
            let g = f.glyph(c).unwrap_or_else(|| panic!("{c}"));
            assert!(!g.strokes.is_empty(), "{c} has strokes");
            assert!(g.right > g.left);
        }
        // 'i' is two strokes: the stem, then the dot after it.
        assert!(f.glyph('i').unwrap().strokes.len() >= 2);
        assert!(f.glyph('ø').is_none());
    }

    #[test]
    fn strokes_land_on_their_glyphs_in_order() {
        let fonts = FontRegistry::new();
        let id = fonts.add_file(&crate::font::test_font_path()).unwrap();
        let text = "sync på";
        let shaped = ShapedLayout::new(fonts.clone()).layout(text, &TextStyle::new(id, 40.0, 48.0), None);
        let word = WordOutline::new(&fonts, &shaped, 40.0, Point::new(10.0, 20.0));
        let w = Writing::new(&fonts, text, word, 4.0);
        assert!(w.length() > 0.0);
        // Pen distance only grows, glyph by glyph, left to right.
        for pair in w.strokes.windows(2) {
            assert!(pair[1].start > pair[0].start);
            assert!(pair[1].glyph >= pair[0].glyph);
        }
        // Each stroke stays near its glyph's pen box and between cap line and descender.
        for t in &w.strokes {
            let g = &w.word.glyphs[t.glyph];
            for q in t.contour.resample(20) {
                assert!(q.x > g.pos.x - 15.0 && q.x < g.pos.x + g.advance + 15.0, "x {q:?} for glyph {}", t.glyph);
                assert!(q.y > g.pos.y - 45.0 && q.y < g.pos.y + 20.0, "y {q:?}");
            }
        }
        // 'å' has no pen letter: its strokes are its outline contours.
        let a = w.word.glyphs.iter().position(|g| g.cluster as usize == text.find('å').unwrap()).unwrap();
        let n = w.strokes.iter().filter(|t| t.glyph == a).count();
        assert_eq!(n, w.word.glyphs[a].outline.contours.len());
    }

    fn shaped_word(fonts: &FontRegistry, id: u32, text: &str) -> WordOutline {
        let shaped = ShapedLayout::new(fonts.clone()).layout(text, &TextStyle::new(id, 40.0, 48.0), None);
        WordOutline::new(fonts, &shaped, 40.0, Point::new(10.0, 20.0))
    }

    #[test]
    fn each_pen_preset_keeps_its_own_ink() {
        let fonts = FontRegistry::new();
        let id = fonts.asset("grotesk").unwrap();
        let w = Writing::with_source(&fonts, "pen", shaped_word(&fonts, id, "pen"), 4.0, StrokeSource::FontNow);
        let (text, accent) = (Color::hex(0xffffff, 1.0), Color::hex(0x22d3ee, 1.0));
        let swept = PenSlot { arrive: 1.0, start: 1.0, end: 1.08, swept: true };
        let written = PenSlot { swept: false, end: 1.2, ..swept };
        let ink = Pen::Ink.wet(text, accent);
        assert_eq!(ink, WetInk::new(text, accent));
        // A swept word shows wet ink, except with the beam, which leaves the sweep to the look.
        assert!(w.wetness(&swept, 1.08, &ink).iter().any(|&a| a > 0.0));
        let beam = Pen::Phosphor.wet(text, accent);
        assert!(w.wetness(&swept, 1.08, &beam).iter().all(|&a| a == 0.0));
        // The beam's afterglow outlasts the ink's drying, dimmer than the type.
        assert!(beam.until(&written) > ink.until(&written) + 0.1);
        assert!(w.wetness(&written, 1.42, &beam).iter().any(|&a| a > 0.0));
        assert!(w.wetness(&written, 1.42, &ink).iter().all(|&a| a == 0.0));
        assert!(beam.ink.a < 0.5);
        // The light pen trails 40 ms and glows; neither ink pen does.
        let light = Pen::Light.wet(text, accent);
        assert!((light.trail_secs - 0.04).abs() < 1e-9 && light.glow > 0.0 && light.pen.is_none());
        assert!(ink.trail_secs == 0.0 && ink.glow == 0.0);
        // Script writes the Hershey strokes with bigger pools.
        assert!(Pen::Script.script() && !Pen::Ink.script());
        assert!(Pen::Script.wet(text, accent).pool > ink.pool);
    }

    #[test]
    fn font_strokes_follow_the_glyph_in_pen_order() {
        let fonts = FontRegistry::new();
        let id = fonts.asset("grotesk").unwrap();
        let text = "tiff";
        let w = Writing::with_source(&fonts, text, shaped_word(&fonts, id, text), 4.0, StrokeSource::FontNow);
        assert!(!w.pending());
        for (i, g) in w.word.glyphs.iter().enumerate() {
            let c = w.centre(i).expect("centre lines");
            assert_eq!(w.strokes().filter(|t| t.glyph == i).count(), c.strokes.len());
            let stem = w.stem(i).unwrap();
            assert!(stem > 0.04 * g.size && stem < 0.2 * g.size, "stem {stem}");
            // Each stroke stays inside the glyph's ink box, give or take the pen.
            for t in w.strokes().filter(|t| t.glyph == i) {
                for q in t.contour.resample(12) {
                    assert!(q.x > g.ink.x - stem && q.x < g.ink.x + g.ink.w + stem, "{q:?}");
                    assert!(q.y > g.ink.y - stem && q.y < g.ink.y + g.ink.h + stem, "{q:?}");
                }
                // The width table is the ink's width over the stem: about 1.
                assert!(t.contour.segs.iter().all(|s| s.w[0] > 0.2 && s.w[0] < 3.0));
            }
        }
        // The dot of the i and the bars of t and f are delayed; base strokes come first in
        // each glyph.
        let delayed = w.strokes().filter(|t| t.delayed).count();
        assert!(delayed >= 4, "{delayed} delayed strokes");
        for i in 0..w.word.glyphs.len() {
            let flags: Vec<bool> = w.strokes().filter(|t| t.glyph == i).map(|t| t.delayed).collect();
            assert!(flags.windows(2).all(|p| p[0] <= p[1]), "glyph {i}: {flags:?}");
        }
        let starts: Vec<f32> = w.strokes().map(|t| t.start).collect();
        assert!(starts.windows(2).all(|p| p[1] > p[0]));
    }

    #[test]
    fn font_source_never_waits_and_falls_back_to_outlines() {
        let fonts = FontRegistry::new();
        let id = fonts.asset("serif").unwrap();
        let text = "Qz";
        let w = Writing::with_source(&fonts, text, shaped_word(&fonts, id, text), 4.0, StrokeSource::Font);
        for (i, g) in w.word.glyphs.iter().enumerate() {
            let n = w.strokes().filter(|t| t.glyph == i).count();
            match w.centre(i) {
                Some(c) => assert_eq!(n, c.strokes.len()),
                None => {
                    assert!(w.pending());
                    assert_eq!(n, g.outline.contours.len());
                }
            }
        }
        // A later build finds them ready.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let w = Writing::with_source(&fonts, text, shaped_word(&fonts, id, text), 4.0, StrokeSource::Font);
            if !w.pending() {
                assert!(w.centre(0).is_some() && w.centre(1).is_some());
                break;
            }
            assert!(std::time::Instant::now() < deadline, "never ready");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn hershey_stays_the_default() {
        let fonts = FontRegistry::new();
        let id = fonts.add_file(&crate::font::test_font_path()).unwrap();
        let a = Writing::new(&fonts, "sync", shaped_word(&fonts, id, "sync"), 4.0);
        let b = Writing::with_source(&fonts, "sync", shaped_word(&fonts, id, "sync"), 4.0, StrokeSource::default());
        assert_eq!(a.length(), b.length());
        assert!(a.centre(0).is_none() && a.stem(0).is_none() && !a.pending());
        assert!(a.strokes().all(|t| !t.delayed));
    }
    #[test]
    fn pen_keeps_up_with_a_burst_then_steady_speech() {
        // 6 words within 0.3 s, then a word every 0.3 s for 10 s; lengths 60 to 330 px.
        let mut words: Vec<(f64, f32)> = (0..6).map(|i| (0.06 * i as f64, 60.0 + 45.0 * i as f32)).collect();
        let mut t = 0.3;
        let mut k = 0;
        while t <= 10.3 {
            t += 0.3;
            words.push((t, 80.0 + ((k * 37) % 250) as f32));
            k += 1;
        }
        let s = PenSchedule::plan(PenParams::default(), &words);
        assert_eq!(s.len(), words.len());
        let mut most = 0;
        let mut step = 0;
        loop {
            let t = step as f64 * 0.001;
            if t > s.until() + 0.01 {
                break;
            }
            let pen = s.slots().iter().filter(|x| matches!(x.phase(t), PenPhase::Writing(_))).count();
            most = most.max(pen);
            step += 1;
        }
        assert!(most <= 1, "{most} words in pen at once");
        for (i, x) in s.slots().iter().enumerate() {
            assert!(x.end <= x.arrive + 0.60, "word {i}: {x:?}");
            assert!(x.start >= x.arrive && x.start - x.arrive <= 0.25 + 1e-9, "word {i}: {x:?}");
            assert!(matches!(x.phase(x.arrive + 0.60), PenPhase::Done));
        }
        // The burst makes the pen hurry and sweep some; steady speech is all written.
        assert!(s.slots()[..6].iter().any(|x| x.swept));
        assert!(s.slots()[8..].iter().all(|x| !x.swept));
        let fast = s.slots()[1];
        assert!(fast.end - fast.start < 0.16, "{fast:?}");
        // Bit-identical across runs, and pushing one by one gives the plan.
        let again = PenSchedule::plan(PenParams::default(), &words);
        let bits = |s: &PenSchedule| -> Vec<(u64, u64, bool)> {
            s.slots().iter().map(|x| (x.start.to_bits(), x.end.to_bits(), x.swept)).collect()
        };
        assert_eq!(bits(&s), bits(&again));
        let mut live = PenSchedule::new(PenParams::default());
        for &(a, l) in &words {
            live.push(a, l);
        }
        assert_eq!(bits(&s), bits(&live));
    }

    #[test]
    fn a_word_alone_takes_160_to_260_ms() {
        for len in [1.0, 100.0, 180.0, 230.0, 5000.0] {
            let s = PenSchedule::plan(PenParams::default(), &[(1.0, len)]);
            let x = s.slots()[0];
            assert!(!x.swept && x.start == 1.0);
            let d = x.end - x.start;
            assert!((0.16 - 1e-9..=0.26 + 1e-9).contains(&d), "{len}: {d}");
        }
        let s = PenSchedule::plan(PenParams::default(), &[(0.0, 180.0)]);
        let x = s.slots()[0];
        assert_eq!(x.phase(-0.1), PenPhase::Waiting);
        assert_eq!(x.phase(0.1), PenPhase::Writing(0.5));
        assert_eq!(x.phase(0.2), PenPhase::Done);
    }

    #[test]
    fn pen_eases_per_stroke_and_rests_over_lifts() {
        let fonts = FontRegistry::new();
        let id = fonts.add_file(&crate::font::test_font_path()).unwrap();
        let w = Writing::new(&fonts, "tiff", shaped_word(&fonts, id, "tiff"), 6.0);
        let n = w.strokes().count();
        assert!(n >= 4);
        let mut last = (0usize, 0.0f32);
        let mut rested = 0;
        for i in 0..=1000 {
            let (done, along) = w.pen_at(i as f32 / 1000.0);
            // The pen only moves forward.
            assert!(done > last.0 || (done == last.0 && along >= last.1), "{i}: {last:?} -> {:?}", (done, along));
            if (done, along) == last && done > 0 {
                rested += 1;
            }
            let s = w.strokes().nth(done).map_or(w.length(), |t| t.start + along);
            assert_eq!(w.split(s).0, done.min(n), "{i}");
            last = (done, along);
        }
        assert_eq!(w.pen_at(0.0), (0, 0.0));
        assert_eq!(w.pen_at(1.0).0, n);
        // The pen holds still over the lifts.
        assert!(rested >= n - 1, "{rested}");
        // Easing: a stroke's first tenth of its time covers less than a tenth of it.
        let t = w.strokes().next().unwrap();
        let (d, a) = w.pen_at(0.1 * t.contour.length() / w.length());
        assert_eq!(d, 0);
        assert!(a < 0.1 * t.contour.length() && a > 0.0);
    }

    #[test]
    fn a_horizontal_stroke_at_a_30_degree_nib_is_thinner_than_a_vertical_one() {
        let nib = Nib::INK;
        let flat = Contour::polyline(&[Point::new(0.0, 0.0), Point::new(20.0, 0.0)], false);
        let down = Contour::polyline(&[Point::new(0.0, 0.0), Point::new(0.0, 20.0)], false);
        let (f, d) = (nibbed(&flat, Some(nib), 1.0), nibbed(&down, Some(nib), 1.0));
        let want = |deg: f32| 0.55 + 0.45 * (deg.to_radians() - std::f32::consts::FRAC_PI_6).sin().abs();
        let ratio = f.segs[0].w[0] / d.segs[0].w[0];
        let formula = want(0.0) / want(90.0);
        assert!(ratio < 1.0, "{ratio}");
        assert!((ratio - formula).abs() <= 0.02 * formula, "{ratio} against {formula}");
        // Pressure scales both ends, 15 % either way.
        let loud = nibbed(&flat, Some(nib), nib.pressure(1.0));
        assert!((loud.segs[0].w[0] / f.segs[0].w[0] - 1.15).abs() < 1e-5);
        assert!((nib.pressure(0.0) - 0.85).abs() < 1e-6 && nib.pressure(0.5) == 1.0);
        // No nib leaves the widths alone.
        assert_eq!(nibbed(&flat, None, 1.3), flat);
    }

    #[test]
    fn a_nibbed_stroke_turns_its_width_without_steps() {
        let pts = [Point::new(0.0, 0.0), Point::new(10.0, 0.0), Point::new(17.0, 7.0), Point::new(17.0, 17.0)];
        let c = nibbed(&Contour::polyline(&pts, false), Some(Nib::INK), 1.0);
        for pair in c.segs.windows(2) {
            assert!((pair[0].w[1] - pair[1].w[0]).abs() < 1e-6, "{:?}", c.segs);
        }
    }

    #[test]
    fn ink_pools_at_the_ends_and_at_sharp_turns_only() {
        let line = |pts: &[Point]| Contour::polyline(pts, false);
        let p = Point::new;
        let straight = line(&[p(0.0, 0.0), p(10.0, 0.0), p(20.0, 0.0)]);
        assert_eq!(pool_points(&straight, f32::INFINITY).len(), 2);
        // A 30 degree bend pools nowhere between; a right angle does.
        let gentle = line(&[p(0.0, 0.0), p(10.0, 0.0), p(18.66, 5.0)]);
        assert_eq!(pool_points(&gentle, f32::INFINITY).len(), 2);
        let corner = line(&[p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0)]);
        let all = pool_points(&corner, f32::INFINITY);
        assert_eq!(all.iter().map(|x| x.0).collect::<Vec<_>>(), vec![p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0)]);
        // The pen half way along the first leg has pooled only at the start.
        assert_eq!(pool_points(&corner, 5.0).len(), 1);
        // At the corner, not yet at the end.
        assert_eq!(pool_points(&corner, 12.0).len(), 2);
    }

    #[test]
    fn a_glyph_range_writes_only_those_glyphs_from_the_start() {
        let fonts = FontRegistry::new();
        let id = fonts.asset("grotesk").unwrap();
        let shaped = ShapedLayout::new(fonts.clone()).layout("swiss", &TextStyle::new(id, 40.0, 48.0), None);
        let word = WordOutline::new(&fonts, &shaped, 40.0, Point::ZERO);
        let whole = Writing::with_source(&fonts, "swiss", word, 6.0, StrokeSource::FontNow);
        let last = whole.glyphs(4..5);
        assert!(last.strokes().all(|s| s.glyph == 4));
        assert_eq!(last.strokes().count(), whole.strokes().filter(|s| s.glyph == 4).count());
        assert_eq!(last.strokes().next().map(|s| s.start), Some(0.0));
        let own: f32 = whole.strokes().filter(|s| s.glyph == 4).map(|s| s.contour.length()).sum();
        assert!(last.length() >= own && last.length() < 0.5 * whole.length(), "{} of {}", last.length(), whole.length());
        // The other glyphs count as written from the start; the last is not.
        assert!(last.glyph_end[..4].iter().all(|e| *e < 0.0));
        assert!(last.glyph_end[4] > 0.0);
        // The whole range is the whole writing.
        let all = whole.glyphs(0..5);
        assert_eq!(all.length(), whole.length());
        assert_eq!(all.glyph_end, whole.glyph_end);
    }
}
