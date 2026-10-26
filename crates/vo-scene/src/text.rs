//! Text measurement and glyph placement, behind a trait.
//!
//! Layout asks a `TextLayout` how big a label is at a given width, and the display list
//! takes the placed glyphs from the same call.
//!
//! - `ShapedLayout` is the real one (plan decision 8): harfrust shapes the whole paragraph
//!   once, unicode-linebreak finds the break opportunities, and a greedy fitter fills the
//!   lines. Glyph ids and advances are the font's, from the shared `FontRegistry`, the same
//!   shaping the Slug encoder's HarfBuzz output assumes. Left to right only for now: no
//!   bidi, and right-to-left scripts are laid out left to right.
//! - `EstimateText` uses a fixed advance per character, and its glyph ids are the
//!   characters' code points. For tests that need no font.

use std::ops::Range;

use harfrust::{Direction, GlyphInfo, GlyphPosition, ShaperFont};
use skrifa::instance::{LocationRef, Size as FontSize};
use skrifa::MetadataProvider;
use unicode_linebreak::{linebreaks, BreakOpportunity};

use crate::element::TextStyle;
use crate::font::FontRegistry;
use crate::geom::{Point, Size};

/// One placed glyph. `pos` is the pen position on the baseline, relative to the text box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedGlyph {
    pub glyph: u32,
    pub pos: Point,
    /// Byte offset of the glyph's cluster in the source text, or `ELLIPSIS_CLUSTER`.
    pub cluster: u32,
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
    /// line breaks in the text) and places its glyphs. A word longer than the width gets a
    /// line of its own. `style.max_lines` and `style.ellipsis` cut the result.
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
                glyphs.push(PlacedGlyph { glyph: c as u32, pos: Point::new(x, baseline), cluster: (start + i) as u32 });
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
    /// Width of the whitespace at the end of the run.
    trailing: f32,
    /// The text must break after this run.
    hard: bool,
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
        let run = shape(&shaper, text);
        let (infos, pos) = (run.glyph_infos(), run.glyph_positions());
        let segments = segments(text, infos, pos, k, style.tracking);
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
            for (info, p) in infos[range.clone()].iter().zip(&pos[range]) {
                let advance = p.x_advance as f32 * k + style.tracking;
                if !is_space(text, info.cluster) {
                    let at = Point::new(x + p.x_offset as f32 * k, baseline - p.y_offset as f32 * k);
                    out.glyphs.push(PlacedGlyph { glyph: info.glyph_id, pos: at, cluster: info.cluster });
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
                for &(glyph, dx, dy) in dots.iter() {
                    let at = Point::new(x + dx, baseline - dy);
                    out.glyphs.push(PlacedGlyph { glyph, pos: at, cluster: ELLIPSIS_CLUSTER });
                }
                width = x + *dots_width;
            }
            widest = widest.max(width);
        }
        out.size = Size::new(widest, lines.len() as f32 * style.line_height);
        out
    }
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
fn segments(text: &str, infos: &[GlyphInfo], pos: &[GlyphPosition], k: f32, tracking: f32) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut g = 0;
    for (end, op) in linebreaks(text) {
        let start = g;
        let (mut width, mut trailing) = (0.0, 0.0);
        while g < infos.len() && (infos[g].cluster as usize) < end {
            let advance = pos[g].x_advance as f32 * k + tracking;
            width += advance;
            trailing = if is_space(text, infos[g].cluster) { trailing + advance } else { 0.0 };
            g += 1;
        }
        out.push(Segment { glyphs: start..g, width, trailing, hard: op == BreakOpportunity::Mandatory });
    }
    out
}

/// Greedy fill: the segment ranges that make each line.
fn fit(segments: &[Segment], max_width: Option<f32>) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let (mut start, mut x) = (0, 0.0f32);
    for (i, s) in segments.iter().enumerate() {
        if i > start && max_width.is_some_and(|max| x + s.width - s.trailing > max + FIT_TOLERANCE) {
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
        // A word wider than the width gets its own line and overflows it.
        let narrow = t.layout("one two three", &STYLE, Some(1.0));
        assert_eq!(narrow.size.h, 78.0);
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
