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
//! thread, for offline renders and tests. Font strokes keep the glyph's pen order (base
//! strokes, then dots, crossbars and marks, [`WrittenStroke::delayed`]) and carry the ink's
//! width along them as a width table relative to the stem ([`Writing::stem`]), so a pen
//! `stem` wide draws the letter's own weight.
//!
//! [`Writing::draw`] draws the strokes up to a pen distance along the whole word, with a
//! bright pen head, and lays the real glyphs under them at an ink that rises from a floor to
//! full as the pen finishes. A look that keeps the reading-line envelope uses a floor of 0.7,
//! so the word is readable the frame it arrives and the writing is an overlay that lands on
//! it.

use std::sync::{Arc, OnceLock};

use crate::canvas::Painter;
use crate::centreline::{self, GlyphCentre, Lookup};
use crate::font::FontRegistry;
use crate::geom::{Color, Point};
use crate::glyph_morph::WordOutline;
use crate::path::{Contour, StrokeStyle, WidthKnot};

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
    /// A dot, crossbar or mark (font strokes only).
    pub delayed: bool,
}

/// A word to be written. See the module docs.
#[derive(Clone, Debug)]
pub struct Writing {
    pub word: WordOutline,
    strokes: Vec<Stroke>,
    /// Pen distance at the end of each glyph's last stroke.
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
        let mut strokes = Vec::new();
        let mut glyph_end = Vec::with_capacity(word.glyphs.len());
        let mut centres = Vec::with_capacity(word.glyphs.len());
        let mut pending = false;
        let mut at = 0.0f32;
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
            for (c, delayed) in placed {
                let len = c.length();
                if len <= 0.0 {
                    continue;
                }
                strokes.push(Stroke { contour: c, start: at, glyph: i, delayed });
                at += len + lift;
            }
            glyph_end.push(at);
            centres.push(centre);
        }
        Writing { word, strokes, glyph_end, total: at, centres, pending, word_id: crate::display::NO_WORD }
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
        // The type first, so the pen writes over it.
        for (i, g) in self.word.glyphs.iter().enumerate() {
            if !g.has_ink() || style.ink.a <= 0.0 {
                continue;
            }
            let end = self.glyph_end[i];
            let begin = self.strokes.iter().find(|t| t.glyph == i).map_or(end, |t| t.start);
            let k = ((s - begin) / (end - begin).max(1e-3)).clamp(0.0, 1.0);
            let ink = style.ink_floor + (1.0 - style.ink_floor) * k;
            p.glyph(crate::display::GlyphInstance { word: self.word_id, ..g.instance(style.ink.fade(ink)) });
        }
        let mut head = None;
        for t in &self.strokes {
            if s <= t.start {
                break;
            }
            let len = t.contour.length();
            let done = (s - t.start).min(len);
            let piece = if done >= len { t.contour.clone() } else { t.contour.trim(0.0, done) };
            p.stroke(&piece, &style.pen);
            if done < len {
                head = t.contour.at(done).map(|x| x.0);
            }
        }
        if let (Some(at), true) = (head, style.head > 0.0) {
            p.stroke(
                &Contour::polyline(&[at, Point::new(at.x + 0.01, at.y)], false),
                &StrokeStyle::solid(style.head, style.head_color),
            );
        }
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
}
