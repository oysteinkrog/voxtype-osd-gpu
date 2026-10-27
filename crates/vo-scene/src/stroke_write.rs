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
//! [`Writing::draw`] draws the strokes up to a pen distance along the whole word, with a
//! bright pen head, and lays the real glyphs under them at an ink that rises from a floor to
//! full as the pen finishes. A look that keeps the reading-line envelope uses a floor of 0.7,
//! so the word is readable the frame it arrives and the writing is an overlay that lands on
//! it.

use std::sync::OnceLock;

use crate::canvas::Painter;
use crate::font::FontRegistry;
use crate::geom::{Color, Point};
use crate::glyph_morph::WordOutline;
use crate::path::{Contour, StrokeStyle};

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

/// One stroke placed in layer units, with where it starts along the whole word's pen path.
#[derive(Clone, Debug)]
struct Stroke {
    contour: Contour,
    /// Pen distance at the stroke's start, pen lifts counted.
    start: f32,
    /// The glyph of the word it writes.
    glyph: usize,
}

/// A word to be written. See the module docs.
#[derive(Clone, Debug)]
pub struct Writing {
    pub word: WordOutline,
    strokes: Vec<Stroke>,
    /// Pen distance at the end of each glyph's last stroke.
    glyph_end: Vec<f32>,
    total: f32,
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
        let pen = PenFont::script();
        let mut strokes = Vec::new();
        let mut glyph_end = Vec::with_capacity(word.glyphs.len());
        let mut at = 0.0f32;
        for (i, g) in word.glyphs.iter().enumerate() {
            let c = text.get(g.cluster as usize..).and_then(|s| s.chars().next()).unwrap_or(' ');
            let cap = cap_height(fonts, g.font, g.size);
            let k = cap / HERSHEY_CAP;
            let placed: Vec<Contour> = match pen.glyph(c).filter(|p| !p.strokes.is_empty()) {
                Some(p) => {
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
                            Contour::polyline(&smooth(&pts), false)
                        })
                        .collect()
                }
                // No pen letter: trace the outline, contour by contour.
                None => g.outline.contours.clone(),
            };
            for c in placed {
                let len = c.length();
                if len <= 0.0 {
                    continue;
                }
                strokes.push(Stroke { contour: c, start: at, glyph: i });
                at += len + lift;
            }
            glyph_end.push(at);
        }
        Writing { word, strokes, glyph_end, total: at }
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
            p.glyph(g.instance(style.ink.fade(ink)));
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
}
