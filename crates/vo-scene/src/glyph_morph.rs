//! Glyph outlines as paths (MO-06): a run of text as vector contours in layer units, and a
//! morph from a simple shape into those outlines and back.
//!
//! [`WordOutline`] takes shaped text (`ShapedLayout`) and turns every placed glyph into an
//! [`Outline`] at its place, keeping the glyph id so the real glyph can be drawn on top.
//! [`GlyphMorph`] pairs each glyph with a piece of a source shape:
//!
//! - [`MorphFrom::Segment`]: the stretch of a line under the glyph, so the letters rise out
//!   of a baseline (MO-10).
//! - [`MorphFrom::Frame`]: a piece of a rounded frame, cut into one piece per glyph by arc
//!   length and handed out from left to right.
//! - [`MorphFrom::Blob`]: a piece of a circle, the same way, so the word grows out of a dot.
//!
//! `t` runs from 0 (the source shape) to 1 (the glyph outlines); running it back is the
//! morph back. [`GlyphMorph::draw`] strokes the morphing contours, fills them as they close,
//! and lays the real Slug glyph over the end, so the settled word is exactly the typeface.
//! The morph itself is a stroke overlay: a look that keeps the reading-line envelope draws
//! the solid glyph at its arrival ink from the first frame ([`MorphStyle::ink_floor`]) and
//! lets the outline play over it.
//!
//! Build the outlines and the morph once per word and keep them in an `Arc` in the canvas
//! closure. Sampling costs a few microseconds per glyph, so every frame stays a pure
//! function of `t`.

use crate::canvas::Painter;
use crate::display::{GlyphInstance, NO_WORD};
use crate::font::{FontId, FontRegistry};
use crate::geom::{Affine, Color, Point, Rect};
use crate::path::{glyph_path, make, Contour, Morph, MorphPart, Outline, StrokeStyle};
use crate::text::ShapedText;

/// Flattening tolerance for glyph outlines, in layer units.
const TOLERANCE: f64 = 0.05;

/// One glyph as a path, placed in layer units.
#[derive(Clone, Debug)]
pub struct GlyphOutline {
    pub font: FontId,
    pub glyph: u32,
    /// Pen position on the baseline.
    pub pos: Point,
    /// Em size in layer units.
    pub size: f32,
    pub outline: Outline,
    /// Ink box of the outline; for a glyph with no ink, a zero-height box at the pen.
    pub ink: Rect,
    /// Where the pen box runs: from `pos.x` by the glyph's advance.
    pub advance: f32,
    /// Byte offset of the glyph's cluster in the shaped text.
    pub cluster: u32,
}

impl GlyphOutline {
    /// The real glyph, to draw over (or instead of) the outline.
    pub fn instance(&self, color: Color) -> GlyphInstance {
        GlyphInstance { pos: self.pos, size: self.size, font: self.font, glyph: self.glyph, word: NO_WORD, color }
    }

    /// Whether the glyph has any outline (a space has none).
    pub fn has_ink(&self) -> bool {
        !self.outline.contours.is_empty()
    }
}

/// A run of shaped text as glyph outlines.
#[derive(Clone, Debug, Default)]
pub struct WordOutline {
    pub glyphs: Vec<GlyphOutline>,
    /// Union of the glyphs' ink boxes.
    pub ink: Rect,
}

impl WordOutline {
    /// The glyphs of `shaped` at `size`, with the text box's top left at `origin`.
    pub fn new(fonts: &FontRegistry, shaped: &ShapedText, size: f32, origin: Point) -> WordOutline {
        let mut glyphs = Vec::with_capacity(shaped.glyphs.len());
        for g in &shaped.glyphs {
            let Some(face) = fonts.get(g.font) else { continue };
            let pos = Point::new(origin.x + g.pos.x, origin.y + g.pos.y);
            let path = glyph_path(&face, g.glyph, size);
            let outline = Outline::new(&path, TOLERANCE).transform(&Affine::translate(pos.x, pos.y));
            let ink = bounds(&outline).unwrap_or(Rect::new(pos.x, pos.y, 0.0, 0.0));
            glyphs.push(GlyphOutline {
                font: g.font,
                glyph: g.glyph,
                pos,
                size,
                outline,
                ink,
                advance: g.advance,
                cluster: g.cluster,
            });
        }
        let ink = glyphs.iter().filter(|g| g.has_ink()).map(|g| g.ink).reduce(union).unwrap_or_default();
        WordOutline { glyphs, ink }
    }

    /// Every glyph as a real glyph instance.
    pub fn instances(&self, color: Color) -> impl Iterator<Item = GlyphInstance> + '_ {
        self.glyphs.iter().filter(|g| g.has_ink()).map(move |g| g.instance(color))
    }

    /// Total outline length, for draw-on timing.
    pub fn length(&self) -> f32 {
        self.glyphs.iter().map(|g| g.outline.length()).sum()
    }
}

/// The shape the glyphs come out of. Coordinates are in the same layer space as the word.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MorphFrom {
    /// A straight line from `a` to `b`. Each glyph takes the stretch whose x range matches
    /// its own across the word.
    Segment { a: Point, b: Point },
    /// A rounded rectangle, cut into one piece per glyph.
    Frame { rect: Rect, radius: f32 },
    /// A circle, cut into one piece per glyph.
    Blob { centre: Point, radius: f32 },
}

impl MorphFrom {
    /// The source contour as one closed or open contour.
    pub fn contour(&self) -> Contour {
        match *self {
            MorphFrom::Segment { a, b } => Contour::polyline(&[a, b], false),
            MorphFrom::Frame { rect, radius } => make::panel(rect.x, rect.y, rect.w, rect.h, radius),
            MorphFrom::Blob { centre, radius } => make::ring(centre, radius),
        }
    }
}

/// How [`GlyphMorph::draw`] paints.
#[derive(Clone, Copy, Debug)]
pub struct MorphStyle {
    /// The morphing contours. Their alpha fades out over `outline_out` near `t = 1`.
    pub stroke: StrokeStyle,
    /// The text colour: the vector fill and the real glyph.
    pub fill: Color,
    /// `t` from which the morphing contours fill in, reaching full fill at 1.
    pub fill_from: f32,
    /// Ink of the real glyph at `t = 0`; it rises to full at `t = 1`. 0 hides the glyph until
    /// the morph lands; 0.7 keeps the reading-line envelope.
    pub ink_floor: f32,
    /// Fraction of the run at the end over which the outline stroke fades out.
    pub outline_out: f32,
}

impl MorphStyle {
    pub fn new(stroke: StrokeStyle, fill: Color) -> MorphStyle {
        MorphStyle { stroke, fill, fill_from: 0.6, ink_floor: 0.0, outline_out: 0.25 }
    }

    pub fn ink_floor(mut self, k: f32) -> MorphStyle {
        self.ink_floor = k.clamp(0.0, 1.0);
        self
    }

    pub fn fill_from(mut self, t: f32) -> MorphStyle {
        self.fill_from = t;
        self
    }

    pub fn outline_out(mut self, k: f32) -> MorphStyle {
        self.outline_out = k;
        self
    }
}

/// A morph between a [`MorphFrom`] shape and a word's glyph outlines. See the module docs.
#[derive(Clone, Debug)]
pub struct GlyphMorph {
    pub word: WordOutline,
    /// One per glyph of `word`; `None` for a glyph with no ink.
    morphs: Vec<Option<Morph>>,
}

impl GlyphMorph {
    /// Pairs every inked glyph of `word` with its piece of `from`, resampled to `n` points per
    /// contour.
    pub fn new(word: WordOutline, from: MorphFrom, n: usize) -> GlyphMorph {
        let inked: Vec<usize> = (0..word.glyphs.len()).filter(|&i| word.glyphs[i].has_ink()).collect();
        let pieces = pieces(&word, &inked, from);
        let mut morphs = vec![None; word.glyphs.len()];
        for (&i, piece) in inked.iter().zip(pieces) {
            let src = Outline { contours: vec![piece] };
            morphs[i] = Some(Morph::new(&src, &word.glyphs[i].outline, n));
        }
        GlyphMorph { word, morphs }
    }

    /// The contours of glyph `i` at `t`, or nothing for a glyph with no ink.
    pub fn sample(&self, i: usize, t: f32) -> Vec<MorphPart> {
        self.morphs.get(i).and_then(Option::as_ref).map_or_else(Vec::new, |m| m.sample(t))
    }

    /// Draws the word at `t` (0 the source shape, 1 the glyphs).
    pub fn draw(&self, p: &mut Painter, t: f32, style: &MorphStyle) {
        self.draw_glyphs(p, style, |_| t);
    }

    /// As [`GlyphMorph::draw`], with each glyph at its own `t` from `at(glyph index)`, for a
    /// stagger across the word.
    pub fn draw_glyphs(&self, p: &mut Painter, style: &MorphStyle, at: impl Fn(usize) -> f32) {
        for (i, g) in self.word.glyphs.iter().enumerate() {
            if !g.has_ink() {
                continue;
            }
            let t = at(i);
            let ink = style.ink_floor + (1.0 - style.ink_floor) * t.clamp(0.0, 1.0);
            if t < 1.0 {
                let parts = self.sample(i, t);
                let fill = ((t - style.fill_from) / (1.0 - style.fill_from).max(1e-3)).clamp(0.0, 1.0);
                if fill > 0.0 {
                    let closing: Vec<&Contour> = parts.iter().filter(|q| q.weight > 0.01).map(|q| &q.contour).collect();
                    // The real glyph carries `ink`; the fill of the shape on its way adds what
                    // is still missing.
                    let k = fill * (1.0 - ink);
                    if k > 0.0 {
                        p.fill(closing, &StrokeStyle::solid(0.0, style.fill.fade(k)));
                    }
                }
                let out = if style.outline_out > 0.0 { ((1.0 - t) / style.outline_out).clamp(0.0, 1.0) } else { 1.0 };
                for q in &parts {
                    if q.weight <= 0.01 {
                        continue;
                    }
                    let s = StrokeStyle {
                        width: style.stroke.width * q.weight,
                        c0: style.stroke.c0.fade(out * q.weight.sqrt()),
                        c1: style.stroke.c1.fade(out * q.weight.sqrt()),
                        ..style.stroke
                    };
                    p.stroke(&q.contour, &s);
                }
            }
            if ink > 0.0 && style.fill.a > 0.0 {
                p.glyph(g.instance(style.fill.fade(ink)));
            }
        }
    }
}

/// Cuts `from` into one piece per inked glyph.
fn pieces(word: &WordOutline, inked: &[usize], from: MorphFrom) -> Vec<Contour> {
    let n = inked.len();
    if n == 0 {
        return Vec::new();
    }
    match from {
        MorphFrom::Segment { a, b } => {
            let (x0, w) = (word.ink.x, word.ink.w.max(1e-3));
            inked
                .iter()
                .map(|&i| {
                    let g = &word.glyphs[i].ink;
                    let (u0, u1) = (((g.x - x0) / w).clamp(0.0, 1.0), ((g.right() - x0) / w).clamp(0.0, 1.0));
                    let lerp = |u: f32| Point::new(a.x + (b.x - a.x) * u, a.y + (b.y - a.y) * u);
                    Contour::polyline(&[lerp(u0), lerp(u1.max(u0 + 1e-3))], false)
                })
                .collect()
        }
        MorphFrom::Frame { .. } | MorphFrom::Blob { .. } => {
            let ring = from.contour();
            let len = ring.length();
            let mut cut: Vec<Contour> = (0..n)
                .map(|k| rebase(&ring.trim(len * k as f32 / n as f32, len * (k + 1) as f32 / n as f32)))
                .collect();
            // Hand the pieces out left to right by their middles, so glyphs travel short ways.
            cut.sort_by(|p, q| mid_x(p).total_cmp(&mid_x(q)));
            cut
        }
    }
}

/// A trimmed piece with arc lengths from 0, which `Contour::resample` needs.
fn rebase(c: &Contour) -> Contour {
    let Some(first) = c.segs.first() else { return Contour::default() };
    let s0 = first.s0;
    let mut out = c.clone();
    for s in &mut out.segs {
        s.s0 -= s0;
        s.s1 -= s0;
    }
    out
}

fn mid_x(c: &Contour) -> f32 {
    let len = c.length();
    let s0 = c.segs.first().map_or(0.0, |s| s.s0);
    c.at(0.5 * (s0 + len)).map_or(0.0, |p| p.0.x)
}

/// Bounding box of the control points of every segment.
fn bounds(o: &Outline) -> Option<Rect> {
    let mut b = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
    for c in &o.contours {
        for s in &c.segs {
            for p in [s.p0, s.p1, s.p2] {
                b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
            }
        }
    }
    (b[0] <= b[2]).then(|| Rect::new(b[0], b[1], b[2] - b[0], b[3] - b[1]))
}

fn union(a: Rect, b: Rect) -> Rect {
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (x1, y1) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::TextStyle;
    use crate::text::{ShapedLayout, TextLayout};

    fn word(text: &str) -> WordOutline {
        let fonts = FontRegistry::new();
        let id = fonts.add_file(&crate::font::test_font_path()).unwrap();
        let shaped = ShapedLayout::new(fonts.clone()).layout(text, &TextStyle::new(id, 40.0, 48.0), None);
        WordOutline::new(&fonts, &shaped, 40.0, Point::new(10.0, 20.0))
    }

    fn max_dist(a: &Contour, b: &[Point]) -> f32 {
        // Every point of `b` lies on `a` within the returned distance.
        let pts = a.resample(400);
        b.iter().map(|q| pts.iter().map(|p| (p.x - q.x).hypot(p.y - q.y)).fold(f32::MAX, f32::min)).fold(0.0, f32::max)
    }

    #[test]
    fn outlines_sit_where_the_glyphs_are_placed() {
        let w = word("Hello");
        assert_eq!(w.glyphs.len(), 5);
        // Pens move right; every ink box sits above the baseline (y down) and near its pen.
        for pair in w.glyphs.windows(2) {
            assert!(pair[1].pos.x > pair[0].pos.x);
        }
        for g in &w.glyphs {
            assert!(g.has_ink());
            assert!(g.ink.y < g.pos.y, "ink rises above the baseline");
            assert!(g.ink.x >= g.pos.x - 5.0 && g.ink.right() <= g.pos.x + g.advance + 5.0);
        }
        // 'H' has one contour, 'e' and 'o' two (counter).
        assert_eq!(w.glyphs[0].outline.contours.len(), 1);
        assert_eq!(w.glyphs[4].outline.contours.len(), 2);
    }

    #[test]
    fn morph_ends_on_the_source_and_on_the_glyph() {
        let w = word("Hello");
        let base = w.glyphs[0].pos.y;
        let from = MorphFrom::Segment { a: Point::new(0.0, base), b: Point::new(300.0, base) };
        let m = GlyphMorph::new(w, from, 96);
        for (i, g) in m.word.glyphs.iter().enumerate() {
            // At 0, every surviving point lies on the baseline.
            for part in m.sample(i, 0.0) {
                if part.weight > 0.5 {
                    for s in &part.contour.segs {
                        assert!((s.p0.y - base).abs() < 1e-3);
                    }
                }
            }
            // At 1, each contour with weight 1 traces a glyph contour.
            let parts: Vec<_> = m.sample(i, 1.0).into_iter().filter(|p| p.weight > 0.99).collect();
            assert_eq!(parts.len(), g.outline.contours.len());
            for part in parts {
                let pts = part.contour.resample(50);
                let d = g.outline.contours.iter().map(|c| max_dist(c, &pts)).fold(f32::MAX, f32::min);
                assert!(d < 0.6, "glyph {i} contour off by {d}");
            }
        }
    }

    #[test]
    fn frame_and_blob_pieces_go_left_to_right() {
        for from in [
            MorphFrom::Frame { rect: Rect::new(0.0, 0.0, 200.0, 60.0), radius: 8.0 },
            MorphFrom::Blob { centre: Point::new(80.0, 30.0), radius: 12.0 },
        ] {
            let w = word("word");
            let inked: Vec<usize> = (0..w.glyphs.len()).collect();
            let cut = pieces(&w, &inked, from);
            assert_eq!(cut.len(), 4);
            let total: f32 = cut.iter().map(Contour::length).sum();
            assert!((total - from.contour().length()).abs() < 0.5, "pieces cover the source");
            for pair in cut.windows(2) {
                assert!(mid_x(&pair[0]) <= mid_x(&pair[1]));
            }
            // A morph from it ends on the glyphs.
            let m = GlyphMorph::new(w, from, 64);
            assert!(m.sample(0, 1.0).iter().any(|p| p.weight > 0.99));
        }
    }
}
