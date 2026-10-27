//! Text measurement and glyph placement, behind a trait.
//!
//! Layout asks a `TextLayout` how big a label is at a given width, and the display list
//! takes the placed glyphs from the same call.
//!
//! - `ShapedLayout` is the real one (plan decision 8): harfrust shapes the whole paragraph
//!   once, unicode-linebreak finds the break opportunities, and a greedy fitter fills the
//!   lines. Glyph ids and advances are the font's, from the shared `FontRegistry`, the same
//!   shaping the Slug encoder's HarfBuzz output assumes. Characters the style's font lacks
//!   are set in the first face of its fallback chain that has them (TY05): the text is cut
//!   into runs by face and each run is shaped on its own. Left to right only for now: no
//!   bidi, and right-to-left scripts are laid out left to right.
//! - `EstimateText` uses a fixed advance per character, and its glyph ids are the
//!   characters' code points. For tests that need no font.

use std::ops::Range;

use harfrust::{Direction, ShaperFont};
use skrifa::instance::{LocationRef, Size as FontSize};
use skrifa::MetadataProvider;
use unicode_linebreak::{linebreaks, BreakOpportunity};

use crate::element::TextStyle;
use crate::font::{FontId, FontRegistry};
use crate::geom::{Point, Size};

/// One placed glyph. `pos` is the pen position on the baseline, relative to the text box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedGlyph {
    pub glyph: u32,
    /// The face the glyph is in: the style's font, or a fallback for a character it lacks.
    pub font: FontId,
    pub pos: Point,
    /// Byte offset of the glyph's cluster in the source text, or `ELLIPSIS_CLUSTER`.
    pub cluster: u32,
    /// How far the pen moves after this glyph, in logical units.
    pub advance: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShapedText {
    pub size: Size,
    pub glyphs: Vec<PlacedGlyph>,
}

/// `PlacedGlyph::cluster` of the glyphs of an ellipsis the layout added.
pub const ELLIPSIS_CLUSTER: u32 = u32::MAX;

pub trait TextLayout {
    /// Breaks `text` into lines no wider than `max_width` (one line for `None`, apart from
    /// line breaks in the text) and places its glyphs. A word longer than the width starts
    /// a new line and breaks between characters, as Pango's `WRAP_WORD_CHAR` does
    /// (`EstimateText` lets it overflow instead). `style.max_lines` and `style.ellipsis` cut
    /// the result.
    fn layout(&self, text: &str, style: &TextStyle, max_width: Option<f32>) -> ShapedText;
}

/// Logical units a line may overshoot its width and still count as fitting.
const FIT_TOLERANCE: f32 = 1e-3;

/// Fixed advances: `advance` ems per character and `space` ems per space. Breaks only at
/// spaces. Honours `max_lines` but draws no ellipsis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EstimateText {
    pub advance: f32,
    pub space: f32,
    /// Baseline below the line top, in ems.
    pub ascent: f32,
}

impl Default for EstimateText {
    fn default() -> EstimateText {
        EstimateText { advance: 0.55, space: 0.3, ascent: 0.8 }
    }
}

impl TextLayout for EstimateText {
    fn layout(&self, text: &str, style: &TextStyle, max_width: Option<f32>) -> ShapedText {
        let em = style.size;
        let space = self.space * em;
        let mut glyphs = Vec::new();
        let (mut x, mut line, mut widest) = (0.0f32, 0usize, 0.0f32);
        let mut any = false;
        for (start, word) in words(text) {
            let w = word.chars().count() as f32 * self.advance * em;
            if any {
                // The tolerance absorbs rounding in widths that went through layout and back.
                let fits = max_width.is_none_or(|max| x + space + w <= max + FIT_TOLERANCE);
                if fits {
                    x += space;
                } else {
                    widest = widest.max(x);
                    line += 1;
                    x = 0.0;
                }
            }
            let baseline = line as f32 * style.line_height + self.ascent * em;
            for (i, c) in word.char_indices() {
                glyphs.push(PlacedGlyph {
                    glyph: c as u32,
                    font: style.font,
                    pos: Point::new(x, baseline),
                    cluster: (start + i) as u32,
                    advance: self.advance * em,
                });
                x += self.advance * em;
            }
            any = true;
        }
        widest = widest.max(x);
        let mut lines = if any { line + 1 } else { 0 };
        let max_lines = style.max_lines as usize;
        if max_lines > 0 && lines > max_lines {
            lines = max_lines;
            let bottom = lines as f32 * style.line_height;
            glyphs.retain(|g| g.pos.y < bottom);
        }
        ShapedText { size: Size::new(widest, lines as f32 * style.line_height), glyphs }
    }
}

/// Real text: harfrust shaping, skrifa metrics, unicode-linebreak break opportunities and a
/// greedy line fitter. Fonts come from the registry the renderer shares, so the glyph ids
/// it places are the ones the renderer draws. A style whose font is not in the registry
/// falls back to `EstimateText`.
///
/// Each line puts its baseline half the leading below the line top plus the font's ascent,
/// so the glyphs sit centred in `line_height` the way CSS centres them. Whitespace advances
/// the pen but places no glyph, and trailing whitespace does not count toward a line's
/// width.
#[derive(Clone, Debug)]
pub struct ShapedLayout {
    fonts: FontRegistry,
}

impl ShapedLayout {
    pub fn new(fonts: FontRegistry) -> ShapedLayout {
        ShapedLayout { fonts }
    }

    pub fn fonts(&self) -> &FontRegistry {
        &self.fonts
    }
}

/// A run between two break opportunities.
struct Segment {
    glyphs: Range<usize>,
    width: f32,
    /// The text must break after this run.
    hard: bool,
    /// The width that must fit after the line's earlier runs for this run to join them:
    /// its own width less its trailing whitespace, or for the first piece of a word split
    /// between characters, the whole word's, so that the word starts a new line.
    fit_width: f32,
}

impl TextLayout for ShapedLayout {
    fn layout(&self, text: &str, style: &TextStyle, max_width: Option<f32>) -> ShapedText {
        let Some(face) = self.fonts.get(style.font) else {
            return EstimateText::default().layout(text, style, max_width);
        };
        if text.is_empty() {
            return ShapedText::default();
        }
        let upem = face.units_per_em();
        let k = style.size / upem as f32;
        let shaper = ShaperFont::new(face.shaping()).with_scale(upem as i32);
        let glyphs = self.shape_runs(text, style, &shaper, k);
        let mut segments = segments(text, &glyphs, style.tracking);
        if let Some(max) = max_width {
            segments = split_long_words(text, segments, &glyphs, style.tracking, max);
        }
        let mut lines = fit(&segments, max_width);
        let max_lines = style.max_lines as usize;
        let dropped = max_lines > 0 && lines.len() > max_lines;
        if dropped {
            lines.truncate(max_lines);
        }

        let m = face.skrifa().metrics(FontSize::new(style.size), LocationRef::default());
        let first_baseline = 0.5 * (style.line_height - (m.ascent - m.descent)) + m.ascent;
        let mut ellipsis: Option<Ellipsis> = None;
        let mut out = ShapedText::default();
        let mut ends: Vec<f32> = Vec::new();
        let mut widest = 0.0f32;
        for (i, line) in lines.iter().enumerate() {
            let baseline = first_baseline + i as f32 * style.line_height;
            let range = segments[line.start].glyphs.start..segments[line.end - 1].glyphs.end;
            let start = out.glyphs.len();
            ends.clear();
            let (mut x, mut width) = (0.0f32, 0.0f32);
            for g in &glyphs[range] {
                let advance = g.advance + style.tracking;
                if !is_space(text, g.cluster) {
                    let at = Point::new(x + g.dx, baseline - g.dy);
                    out.glyphs.push(PlacedGlyph { glyph: g.id, font: g.font, pos: at, cluster: g.cluster, advance });
                    ends.push(x + advance);
                    width = x + advance;
                }
                x += advance;
            }
            let last = i + 1 == lines.len();
            let overflows = max_width.is_some_and(|max| width > max + FIT_TOLERANCE);
            if style.ellipsis && ((last && dropped) || overflows) {
                let (dots, dots_width) = ellipsis.get_or_insert_with(|| ellipsis_glyphs(&shaper, k));
                let limit = max_width.map_or(f32::INFINITY, |max| max - *dots_width + FIT_TOLERANCE);
                // Drop whole clusters from the end until the ellipsis fits after them.
                while ends.last().is_some_and(|&end| end > limit) {
                    let cluster = out.glyphs[start + ends.len() - 1].cluster;
                    while !ends.is_empty() && out.glyphs[start + ends.len() - 1].cluster == cluster {
                        ends.pop();
                    }
                    out.glyphs.truncate(start + ends.len());
                }
                let x = ends.last().copied().unwrap_or(0.0);
                for (i, &(glyph, dx, dy)) in dots.iter().enumerate() {
                    let at = Point::new(x + dx, baseline - dy);
                    let next = dots.get(i + 1).map_or(*dots_width, |d| d.1);
                    let advance = next - dx;
                    out.glyphs.push(PlacedGlyph {
                        glyph,
                        font: style.font,
                        pos: at,
                        cluster: ELLIPSIS_CLUSTER,
                        advance,
                    });
                }
                width = x + *dots_width;
            }
            widest = widest.max(width);
        }
        out.size = Size::new(widest, lines.len() as f32 * style.line_height);
        out
    }
}

/// One shaped glyph, scaled to logical units, before line fitting.
struct Shaped {
    id: u32,
    font: FontId,
    /// Byte offset of its cluster in the whole text.
    cluster: u32,
    /// Pen advance without tracking.
    advance: f32,
    dx: f32,
    dy: f32,
}

impl ShapedLayout {
    /// Shapes `text` run by run: each run is a stretch of characters the same face has. A
    /// character goes to the first face of `style.font` and its fallbacks that has it.
    /// Combining marks stay in the run they are in, so an accent stays with its letter, and
    /// so does whitespace the run's face has, so a space between two Japanese words does not
    /// split the run.
    fn shape_runs(&self, text: &str, style: &TextStyle, primary: &ShaperFont<'_, '_>, k: f32) -> Vec<Shaped> {
        let runs = self.runs(text, style.font);
        let mut out = Vec::with_capacity(text.len());
        for (range, font) in runs {
            let face = if font == style.font { None } else { self.fonts.get(font) };
            let (buf, k) = match &face {
                None => (shape(primary, &text[range.clone()]), k),
                Some(face) => {
                    let upem = face.units_per_em();
                    let shaper = ShaperFont::new(face.shaping()).with_scale(upem as i32);
                    (shape(&shaper, &text[range.clone()]), style.size / upem as f32)
                }
            };
            for (info, p) in buf.glyph_infos().iter().zip(buf.glyph_positions()) {
                out.push(Shaped {
                    id: info.glyph_id,
                    font,
                    cluster: info.cluster + range.start as u32,
                    advance: p.x_advance as f32 * k,
                    dx: p.x_offset as f32 * k,
                    dy: p.y_offset as f32 * k,
                });
            }
        }
        out
    }

    /// The text cut into runs by face. One run in the style's font when it has no fallbacks
    /// or has every character.
    fn runs(&self, text: &str, primary: FontId) -> Vec<(Range<usize>, FontId)> {
        let chain = self.fonts.fallbacks(primary);
        let face = self.fonts.get(primary);
        let whole = || vec![(0..text.len(), primary)];
        let Some(face) = face else { return whole() };
        if chain.is_empty() || text.chars().all(|c| c.is_whitespace() || face.has(c)) {
            return whole();
        }
        let mut runs: Vec<(Range<usize>, FontId)> = Vec::new();
        for (at, c) in text.char_indices() {
            let font = match runs.last() {
                Some(&(_, f)) if joins(c) => f,
                Some(&(_, f)) if c.is_whitespace() && self.fonts.get(f).is_some_and(|face| face.has(c)) => f,
                _ => self.fonts.face_for(primary, c),
            };
            match runs.last_mut() {
                Some((r, f)) if *f == font => r.end = at + c.len_utf8(),
                _ => runs.push((at..at + c.len_utf8(), font)),
            }
        }
        runs
    }
}

/// Characters that belong to the one before them and stay in its run: combining marks,
/// the kana voicing marks, joiners and variation selectors.
fn joins(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0x3099..=0x309A
        | 0x200C..=0x200D | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F | 0xE0100..=0xE01EF)
}

fn shape(shaper: &ShaperFont<'_, '_>, text: &str) -> harfrust::Buffer {
    let mut buf = harfrust::Buffer::new();
    buf.push_str(text);
    buf.guess_segment_properties();
    buf.set_direction(Direction::LeftToRight);
    harfrust::shape(shaper, &mut buf, harfrust::ShapeOptions::new()).expect("shaping a text buffer");
    buf
}

/// Ellipsis glyphs as `(glyph, x offset, y offset)` from the pen start, and their width.
type Ellipsis = (Vec<(u32, f32, f32)>, f32);

/// The ellipsis, or three full stops when the font has no ellipsis character.
fn ellipsis_glyphs(shaper: &ShaperFont<'_, '_>, k: f32) -> Ellipsis {
    let mut buf = shape(shaper, "\u{2026}");
    if buf.glyph_infos().iter().any(|g| g.glyph_id == 0) {
        buf = shape(shaper, "...");
    }
    let mut x = 0.0;
    let mut glyphs = Vec::new();
    for (info, p) in buf.glyph_infos().iter().zip(buf.glyph_positions()) {
        glyphs.push((info.glyph_id, x + p.x_offset as f32 * k, p.y_offset as f32 * k));
        x += p.x_advance as f32 * k;
    }
    (glyphs, x)
}

fn is_space(text: &str, cluster: u32) -> bool {
    text.get(cluster as usize..).and_then(|s| s.chars().next()).is_some_and(char::is_whitespace)
}

/// Splits the shaped glyphs at the text's break opportunities. Clusters rise through the
/// buffer because the text is shaped left to right.
fn segments(text: &str, glyphs: &[Shaped], tracking: f32) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut g = 0;
    for (end, op) in linebreaks(text) {
        let start = g;
        let (mut width, mut trailing) = (0.0, 0.0);
        while g < glyphs.len() && (glyphs[g].cluster as usize) < end {
            let advance = glyphs[g].advance + tracking;
            width += advance;
            trailing = if is_space(text, glyphs[g].cluster) { trailing + advance } else { 0.0 };
            g += 1;
        }
        let hard = op == BreakOpportunity::Mandatory;
        out.push(Segment { glyphs: start..g, width, hard, fit_width: width - trailing });
    }
    out
}

/// Splits each run too wide for a line on its own into one run per cluster, so the fitter
/// can break it between characters. Space pieces never start a line, and the last
/// piece keeps the run's hard break.
fn split_long_words(
    text: &str,
    segments: Vec<Segment>,
    glyphs: &[Shaped],
    tracking: f32,
    max_width: f32,
) -> Vec<Segment> {
    if segments.iter().all(|s| s.fit_width <= max_width + FIT_TOLERANCE) {
        return segments;
    }
    let mut out = Vec::with_capacity(segments.len());
    for s in segments {
        if s.fit_width <= max_width + FIT_TOLERANCE {
            out.push(s);
            continue;
        }
        let first = out.len();
        let mut g = s.glyphs.start;
        while g < s.glyphs.end {
            let start = g;
            let cluster = glyphs[g].cluster;
            let mut width = 0.0;
            while g < s.glyphs.end && glyphs[g].cluster == cluster {
                width += glyphs[g].advance + tracking;
                g += 1;
            }
            let piece = if is_space(text, cluster) {
                Segment { glyphs: start..g, width, hard: false, fit_width: 0.0 }
            } else {
                Segment { glyphs: start..g, width, hard: false, fit_width: width }
            };
            out.push(piece);
        }
        out[first].fit_width = s.fit_width;
        out.last_mut().expect("a run has glyphs").hard = s.hard;
    }
    out
}

/// Greedy fill: the segment ranges that make each line.
fn fit(segments: &[Segment], max_width: Option<f32>) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let (mut start, mut x) = (0, 0.0f32);
    for (i, s) in segments.iter().enumerate() {
        if i > start && s.fit_width > 0.0 && max_width.is_some_and(|max| x + s.fit_width > max + FIT_TOLERANCE) {
            lines.push(start..i);
            (start, x) = (i, 0.0);
        }
        x += s.width;
        if s.hard && i + 1 < segments.len() {
            lines.push(start..i + 1);
            (start, x) = (i + 1, 0.0);
        }
    }
    if start < segments.len() {
        lines.push(start..segments.len());
    }
    lines
}

/// Words split on ASCII spaces, with their byte offsets.
fn words(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split(' ')
        .scan(0usize, |offset, w| {
            let start = *offset;
            *offset += w.len() + 1;
            Some((start, w))
        })
        .filter(|(_, w)| !w.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_spaces() {
        let style = TextStyle::new(0, 10.0, 12.0);
        let t = EstimateText::default();
        let one = t.layout("ab cd", &style, None);
        assert_eq!(one.size, Size::new(5.5 * 4.0 + 3.0, 12.0));
        let two = t.layout("ab cd", &style, Some(20.0));
        assert_eq!(two.size, Size::new(11.0, 24.0));
        assert_eq!(two.glyphs[2].pos, Point::new(0.0, 12.0 + 8.0));
        assert_eq!(two.glyphs[2].cluster, 3);
        assert_eq!(t.layout("", &style, None).size, Size::ZERO);
    }

    fn shaped() -> ShapedLayout {
        let fonts = FontRegistry::new();
        fonts.add_file(&crate::font::test_font_path()).unwrap();
        ShapedLayout::new(fonts)
    }

    const STYLE: TextStyle =
        TextStyle { font: 0, size: 20.0, line_height: 26.0, max_lines: 0, ellipsis: false, tracking: 0.0 };

    fn gid(t: &ShapedLayout, c: char) -> u32 {
        let face = t.fonts().get(0).unwrap();
        face.skrifa().charmap().map(c).unwrap().to_u32()
    }

    fn advance(t: &ShapedLayout, c: char, size: f32) -> f32 {
        let face = t.fonts().get(0).unwrap();
        let font = face.skrifa();
        font.glyph_metrics(FontSize::new(size), LocationRef::default())
            .advance_width(font.charmap().map(c).unwrap())
            .unwrap()
    }

    #[test]
    fn real_glyph_ids_and_advances() {
        let t = shaped();
        // No kerning pairs in "Hil", so the pen moves by the plain advances.
        let s = t.layout("Hil", &STYLE, None);
        let ids: Vec<u32> = s.glyphs.iter().map(|g| g.glyph).collect();
        assert_eq!(ids, vec![gid(&t, 'H'), gid(&t, 'i'), gid(&t, 'l')]);
        assert_ne!(ids[0], 'H' as u32);
        let (h, e, x) = (advance(&t, 'H', 20.0), advance(&t, 'i', 20.0), advance(&t, 'l', 20.0));
        assert!((s.glyphs[1].pos.x - h).abs() < 1e-3);
        assert!((s.glyphs[2].pos.x - h - e).abs() < 1e-3);
        assert!((s.size.w - h - e - x).abs() < 1e-3);
        assert_eq!(s.size.h, 26.0);
        assert_eq!(s.glyphs.iter().map(|g| g.cluster).collect::<Vec<_>>(), vec![0, 1, 2]);
        // Baseline: centred in the line box by the font's ascent and descent.
        let y = s.glyphs[0].pos.y;
        assert!(y > 13.0 && y < 26.0, "baseline {y}");
    }

    #[test]
    fn kerning_comes_from_shaping() {
        let t = shaped();
        let s = t.layout("AV", &STYLE, None);
        let plain = advance(&t, 'A', 20.0) + advance(&t, 'V', 20.0);
        assert!(s.size.w < plain - 0.1, "AV {} against {plain}", s.size.w);
    }

    #[test]
    fn wraps_at_break_opportunities() {
        let t = shaped();
        let one = t.layout("one two three", &STYLE, None);
        let first_two = t.layout("one two", &STYLE, None).size.w;
        let two = t.layout("one two three", &STYLE, Some(first_two + 1.0));
        assert_eq!(two.size.h, 52.0);
        assert!((two.size.w - first_two).abs() < 1e-3);
        let t3 = two.glyphs.iter().find(|g| g.cluster == 8).unwrap();
        assert_eq!(t3.pos.x, 0.0);
        assert_eq!(t3.pos.y, one.glyphs[0].pos.y + 26.0);
        // Spaces place no glyph.
        assert_eq!(two.glyphs.len(), 11);
        // Words wider than the width break between characters: one per line here, with
        // the spaces at the ends of lines.
        let narrow = t.layout("one two three", &STYLE, Some(1.0));
        assert_eq!(narrow.size.h, 11.0 * 26.0);
    }

    #[test]
    fn a_long_word_starts_a_new_line_then_breaks_between_characters() {
        let t = shaped();
        let word = "abcdefghijklmnopqrstuvwxyz";
        let width = t.layout("abcdefghij", &STYLE, None).size.w + 1.0;
        let s = t.layout(&format!("hi {word} end"), &STYLE, Some(width));
        let line_of = |cluster: u32| {
            let g = s.glyphs.iter().find(|g| g.cluster == cluster).unwrap();
            ((g.pos.y - s.glyphs[0].pos.y) / 26.0).round() as u32
        };
        // "hi" alone, then the word from the start of the next line, ten letters a line.
        assert_eq!(line_of(0), 0);
        assert_eq!(line_of(3), 1);
        assert!(t.layout(word, &STYLE, None).size.w > width);
        assert_eq!(line_of(3 + 9), 1);
        assert_eq!(line_of(3 + 10), 2);
        assert_eq!(line_of(3 + 20), 3);
        assert_eq!(s.glyphs.iter().find(|g| g.cluster == 3 + 10).unwrap().pos.x, 0.0);
        assert!(s.size.w <= width + FIT_TOLERANCE, "{}", s.size.w);
    }

    #[test]
    fn trailing_space_and_hard_breaks() {
        let t = shaped();
        assert_eq!(t.layout("ab  ", &STYLE, None).size.w, t.layout("ab", &STYLE, None).size.w);
        let s = t.layout("ab\ncd", &STYLE, None);
        assert_eq!(s.size.h, 52.0);
        assert_eq!(s.glyphs.len(), 4);
        assert_eq!(s.glyphs[2].pos.x, 0.0);
        assert_eq!(t.layout("", &STYLE, None), ShapedText::default());
    }

    #[test]
    fn max_lines_with_and_without_ellipsis() {
        let t = shaped();
        let text = "the quick brown fox jumps over the lazy dog again and again";
        let width = 120.0;
        let all = t.layout(text, &STYLE, Some(width));
        assert!(all.size.h > 2.0 * 26.0);

        let clipped = t.layout(text, &TextStyle { max_lines: 2, ..STYLE }, Some(width));
        assert_eq!(clipped.size.h, 52.0);
        assert!(clipped.glyphs.iter().all(|g| g.cluster != ELLIPSIS_CLUSTER));
        assert!(clipped.glyphs.iter().all(|g| g.pos.y < 52.0));

        let dots = t.layout(text, &STYLE.clamp_lines(2), Some(width));
        assert_eq!(dots.size.h, 52.0);
        assert!(dots.size.w <= width + FIT_TOLERANCE);
        let last = dots.glyphs.last().unwrap();
        assert_eq!(last.cluster, ELLIPSIS_CLUSTER);
        assert_eq!(last.glyph, gid(&t, '\u{2026}'));
        assert!(last.pos.y > 26.0);

        // Text that fits gets no ellipsis.
        let short = t.layout("fits", &STYLE.clamp_lines(1), Some(width));
        assert!(short.glyphs.iter().all(|g| g.cluster != ELLIPSIS_CLUSTER));
    }

    #[test]
    fn ellipsis_cuts_a_word_too_wide_for_its_line() {
        let t = shaped();
        let s = t.layout("Supercalifragilistic", &STYLE.clamp_lines(1), Some(80.0));
        assert_eq!(s.size.h, 26.0);
        assert!(s.size.w <= 80.0 + FIT_TOLERANCE, "{}", s.size.w);
        assert_eq!(s.glyphs.last().unwrap().cluster, ELLIPSIS_CLUSTER);
        assert!(s.glyphs.len() > 2);
    }

    #[test]
    fn unknown_font_falls_back_to_the_estimate() {
        let t = shaped();
        let style = TextStyle { font: 7, ..STYLE };
        assert_eq!(t.layout("ab", &style, None), EstimateText::default().layout("ab", &style, None));
    }

    fn with_japanese() -> (ShapedLayout, FontId, FontId) {
        let fonts = FontRegistry::new();
        let latin = fonts.asset("sans").unwrap();
        let jp = fonts.asset("jp-labels").unwrap();
        fonts.set_fallbacks(latin, &[jp]);
        (ShapedLayout::new(fonts), latin, jp)
    }

    #[test]
    fn missing_characters_fall_back_run_by_run() {
        let (t, latin, jp) = with_japanese();
        let text = "VOICE 音声入力 ok";
        let s = t.layout(text, &STYLE, None);
        let fonts: Vec<FontId> = s.glyphs.iter().map(|g| g.font).collect();
        assert_eq!(fonts, [vec![latin; 5], vec![jp; 4], vec![latin; 2]].concat());
        assert!(s.glyphs.iter().all(|g| g.glyph != 0), "no missing-glyph boxes");
        // Clusters are byte offsets into the whole text, not the run.
        assert_eq!(s.glyphs[5].cluster, 6);
        assert_eq!(s.glyphs[9].cluster as usize, text.find("ok").unwrap());
        // The pen runs on across runs: the first ideograph sits where any glyph after
        // "VOICE " would, and the last run starts one space after the fourth ideograph. That
        // space stays in the Japanese run, so it has the Japanese face's width.
        assert!((s.glyphs[5].pos.x - t.layout("VOICE x", &STYLE, None).glyphs[5].pos.x).abs() < 1e-3);
        let jp_style = TextStyle { font: jp, ..STYLE };
        let space = t.layout("入 x", &jp_style, None).glyphs[1].pos.x - t.layout("入", &jp_style, None).size.w;
        let end = s.glyphs[8].pos.x + s.glyphs[8].advance;
        assert!((s.glyphs[9].pos.x - end - space).abs() < 1e-3);
        let jp_face = t.fonts().get(jp).unwrap();
        let ideograph = jp_face.skrifa().charmap().map('音').unwrap().to_u32();
        assert_eq!(s.glyphs[5].glyph, ideograph);
        // A full-width ideograph is one em wide.
        assert!((s.glyphs[5].advance - 20.0).abs() < 0.5, "{}", s.glyphs[5].advance);
    }

    #[test]
    fn marks_and_spaces_stay_in_their_run() {
        let (t, latin, jp) = with_japanese();
        // A voicing mark and a space after kana stay with the kana; the Latin after them
        // goes back to the primary.
        let text = "カ\u{3099} 入 ab";
        let runs = t.runs(text, latin);
        let split = text.find("ab").unwrap();
        assert_eq!(runs, vec![(0..split, jp), (split..text.len(), latin)]);
    }

    #[test]
    fn fallback_text_wraps_between_ideographs() {
        let (t, _, jp) = with_japanese();
        let s = t.layout("音声入力中", &STYLE, Some(45.0));
        assert_eq!(s.size.h, 3.0 * 26.0);
        assert!(s.glyphs.iter().all(|g| g.font == jp));
    }

    #[test]
    fn no_fallback_keeps_the_primary_and_its_missing_glyph() {
        let t = shaped();
        let s = t.layout("a音", &STYLE, None);
        assert!(s.glyphs.iter().all(|g| g.font == 0));
        assert_eq!(s.glyphs[1].glyph, 0);
        // With fallbacks set, text the primary covers is shaped exactly as before.
        let (f, _, _) = with_japanese();
        assert_eq!(f.layout("the quick fox", &STYLE, Some(60.0)), t.layout("the quick fox", &STYLE, Some(60.0)));
    }

    #[test]
    fn estimate_honours_max_lines() {
        let style = TextStyle { max_lines: 1, ..TextStyle::new(0, 10.0, 12.0) };
        let s = EstimateText::default().layout("ab cd", &style, Some(20.0));
        assert_eq!(s.size.h, 12.0);
        assert_eq!(s.glyphs.len(), 2);
    }

    #[test]
    fn tracking_adds_space_after_every_character() {
        let t = shaped();
        let plain = t.layout("ab cd", &STYLE, None);
        let wide = t.layout("ab cd", &STYLE.tracking(2.0), None);
        assert!((wide.size.w - plain.size.w - 10.0).abs() < 1e-3, "{} vs {}", wide.size.w, plain.size.w);
        assert!((wide.glyphs[3].pos.x - plain.glyphs[3].pos.x - 8.0).abs() < 1e-3);
    }
}
