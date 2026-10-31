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
use std::sync::Arc;

use harfrust::{Direction, Feature, ShaperFont};
use skrifa::instance::Size as FontSize;
use skrifa::MetadataProvider;
use unicode_linebreak::{linebreaks, BreakOpportunity};
use vo_core::edit::{is_grapheme_boundary, next_grapheme, Affinity, Lines};

use crate::element::TextStyle;
use crate::font::{Face, FontId, FontRegistry};
use crate::geom::{Point, Rect, Size};
use crate::inline::{OBJECT_CHAR, OBJECT_GLYPH, OBJECT_TEXT};

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
    /// The lines, top to bottom.
    pub lines: Vec<Line>,
    /// Where a caret can stand, line by line and left to right: one stop before every
    /// grapheme cluster a line holds, spaces included, and one at the line's end. At a soft
    /// wrap the break byte has two stops, the end of one line and the start of the next. Only
    /// built when `TextStyle::stops` is set; empty otherwise. Text cut by `max_lines` or an
    /// ellipsis has no stops.
    pub stops: Vec<CaretStop>,
}

/// A place a caret can stand: before the grapheme cluster at `byte` (or at the line's end),
/// on `line`, at `x` across the text box, after alignment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaretStop {
    pub byte: usize,
    pub line: usize,
    pub x: f32,
}

/// One laid-out line: its glyphs, where it sits across the box and its baseline.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line {
    /// Range in `ShapedText::glyphs`.
    pub glyphs: Range<usize>,
    /// Left end of the line's ink and its width, after alignment.
    pub x: f32,
    pub width: f32,
    pub baseline: f32,
    /// The source bytes the line holds, without the hard line break that ends it. A soft
    /// wrapped line keeps its trailing spaces. Only filled when `TextStyle::stops` is set;
    /// `0..0` otherwise.
    pub bytes: Range<usize>,
}

/// Where each line of a paragraph sits across the text box (FX-002).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    /// Lines start at the left edge.
    #[default]
    Start,
    /// Each line is centred.
    Center,
    /// Lines end at the right edge.
    End,
}

impl Align {
    /// Share of the free space that goes before the line.
    pub fn factor(self) -> f32 {
        match self {
            Align::Start => 0.0,
            Align::Center => 0.5,
            Align::End => 1.0,
        }
    }
}

impl ShapedText {
    /// Moves every glyph across by the alignment's share of `width` less the text's width,
    /// and widens the text to `width`. Layout calls this when the box is wider than the
    /// text; lines are already aligned to each other.
    pub fn align_in(&mut self, align: Align, width: f32) {
        let dx = align.factor() * (width - self.size.w);
        if dx > 0.0 {
            for g in &mut self.glyphs {
                g.pos.x += dx;
            }
            for l in &mut self.lines {
                l.x += dx;
            }
            for s in &mut self.stops {
                s.x += dx;
            }
            self.size.w = width;
        }
    }

    /// The height of one line: lines are evenly spaced.
    fn line_height(&self) -> f32 {
        if self.lines.is_empty() {
            0.0
        } else {
            self.size.h / self.lines.len() as f32
        }
    }

    /// The stops of `line`, left to right.
    fn line_stops(&self, line: usize) -> &[CaretStop] {
        let lo = self.stops.partition_point(|s| s.line < line);
        let hi = self.stops.partition_point(|s| s.line <= line);
        &self.stops[lo..hi]
    }

    /// The line that holds the caret at `byte`. At a soft wrap the break byte ends one line
    /// and starts the next: `Upstream` is the earlier line, `Downstream` the later. A byte
    /// past every line is on the last. Needs `TextStyle::stops`; 0 without it.
    pub fn line_of(&self, byte: usize, affinity: Affinity) -> usize {
        let mut found = None;
        for (i, l) in self.lines.iter().enumerate() {
            if l.bytes.start <= byte && byte <= l.bytes.end {
                if affinity == Affinity::Upstream {
                    return i;
                }
                found = Some(i);
            } else if l.bytes.start > byte {
                break;
            }
        }
        found.unwrap_or(self.lines.len().saturating_sub(1))
    }

    /// The caret stop for `byte` on the line `affinity` picks, or `None` when the byte is no
    /// caret position (inside a cluster) or the text has no stops.
    pub fn stop_at(&self, byte: usize, affinity: Affinity) -> Option<CaretStop> {
        let line = self.line_of(byte, affinity);
        self.line_stops(line).iter().find(|s| s.byte == byte).copied()
    }

    /// The caret position nearest to `p` (text box coordinates): the line under `p.y`
    /// (clamped to the first and last), then the stop on it nearest to `p.x`. The end of a
    /// soft-wrapped line comes back `Upstream`, so the caret stays on that line.
    pub fn hit(&self, p: Point) -> (usize, Affinity) {
        let lh = self.line_height();
        let line = if lh > 0.0 { (p.y / lh).floor().clamp(0.0, (self.lines.len() - 1) as f32) as usize } else { 0 };
        self.nearest_on(line, p.x)
    }

    fn nearest_on(&self, line: usize, x: f32) -> (usize, Affinity) {
        let stops = self.line_stops(line);
        let Some(best) = stops.iter().min_by(|a, b| (a.x - x).abs().total_cmp(&(b.x - x).abs())) else {
            return (0, Affinity::Downstream);
        };
        let wraps_on = self.lines.get(line + 1).is_some_and(|next| next.bytes.start == best.byte);
        (best.byte, if wraps_on { Affinity::Upstream } else { Affinity::Downstream })
    }

    /// The rectangles that show the selection of `range`, one per line it touches, a whole
    /// line tall, in text box coordinates. A selection that runs on past a line's end covers
    /// that line's trailing space too.
    pub fn selection_rects(&self, range: std::ops::Range<usize>) -> Vec<Rect> {
        let lh = self.line_height();
        let mut out = Vec::new();
        if range.is_empty() {
            return out;
        }
        for (i, l) in self.lines.iter().enumerate() {
            if range.end < l.bytes.start || range.start > l.bytes.end {
                continue;
            }
            let (lo, hi) = (range.start.max(l.bytes.start), range.end.min(l.bytes.end));
            let stops = self.line_stops(i);
            let x_at = |b: usize| stops.iter().find(|s| s.byte >= b).or(stops.last()).map_or(l.x, |s| s.x);
            let x0 = x_at(lo);
            let x1 = x_at(hi);
            if x1 > x0 {
                out.push(Rect::new(x0, i as f32 * lh, x1 - x0, lh));
            }
        }
        out
    }
}

/// The wrapped layout for the edit model's Up, Down, Home and End, in the text's own
/// coordinates. Needs `TextStyle::stops`.
impl Lines for ShapedText {
    fn line_count(&self) -> usize {
        self.lines.len().max(1)
    }

    fn line_of(&self, byte: usize, affinity: Affinity) -> usize {
        ShapedText::line_of(self, byte, affinity)
    }

    fn line_range(&self, line: usize) -> std::ops::Range<usize> {
        self.lines.get(line).map_or(0..0, |l| l.bytes.clone())
    }

    fn x_of(&self, byte: usize, affinity: Affinity) -> f32 {
        self.stop_at(byte, affinity).map_or(0.0, |s| s.x)
    }

    fn stop_at_x(&self, line: usize, x: f32) -> (usize, Affinity) {
        self.nearest_on(line, x)
    }
}

/// Shifts the glyphs of each line, given as `(first glyph, width, baseline)` in order, so
/// the lines sit by `align` across the widest one, and returns the lines.
fn align_lines(glyphs: &mut [PlacedGlyph], lines: &[(usize, f32, f32)], widest: f32, align: Align) -> Vec<Line> {
    let k = align.factor();
    let mut out = Vec::with_capacity(lines.len());
    for (i, &(start, width, baseline)) in lines.iter().enumerate() {
        let end = lines.get(i + 1).map_or(glyphs.len(), |l| l.0).min(glyphs.len());
        let dx = k * (widest - width);
        for g in &mut glyphs[start.min(end)..end] {
            g.pos.x += dx;
        }
        out.push(Line { glyphs: start.min(end)..end, x: dx, width, baseline, bytes: 0..0 });
    }
    out
}

/// Characters that end a line by themselves. A line's `bytes` stop before them.
fn is_hard_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// `end` moved back over the hard line break characters just before it.
fn trim_break(text: &str, start: usize, mut end: usize) -> usize {
    while end > start {
        match text[..end].chars().next_back() {
            Some(c) if is_hard_break(c) => end -= c.len_utf8(),
            _ => break,
        }
    }
    end
}

/// Pushes the stops of one line: for each span `(bytes, x0, x1)`, one stop per grapheme
/// boundary in it, spread evenly from `x0` to `x1` (a ligature's advance is shared by its
/// letters), then the end stop at `end`, `end_x`. Spans are in order and touch.
fn push_stops(
    text: &str,
    line: usize,
    spans: impl IntoIterator<Item = (Range<usize>, f32, f32)>,
    (end, end_x): (usize, f32),
    out: &mut Vec<CaretStop>,
) {
    let mut cuts = Vec::new();
    for (r, x0, x1) in spans {
        cuts.clear();
        let mut b = r.start;
        if !is_grapheme_boundary(text, b) {
            b = next_grapheme(text, b);
        }
        while b < r.end {
            cuts.push(b);
            b = next_grapheme(text, b);
        }
        let n = cuts.len() as f32;
        for (k, &b) in cuts.iter().enumerate() {
            out.push(CaretStop { byte: b, line, x: x0 + (x1 - x0) * k as f32 / n });
        }
    }
    out.push(CaretStop { byte: end, line, x: end_x });
}

/// Moves the stops of each line across by that line's alignment shift.
fn align_stops(stops: &mut [CaretStop], lines: &[Line]) {
    for s in stops {
        s.x += lines[s.line].x;
    }
}

/// `PlacedGlyph::cluster` of the glyphs of an ellipsis the layout added.
pub const ELLIPSIS_CLUSTER: u32 = u32::MAX;

pub trait TextLayout {
    /// Breaks `text` into lines no wider than `max_width` (one line for `None`, apart from
    /// line breaks in the text) and places its glyphs. A word longer than the width starts
    /// a new line and breaks between characters, as Pango's `WRAP_WORD_CHAR` does
    /// (`EstimateText` lets it overflow instead). `style.max_lines` and `style.ellipsis` cut
    /// the result. Lines sit by `style.align` across the widest line, and `style.balance`
    /// evens out their lengths (`ShapedLayout` only).
    fn layout(&self, text: &str, style: &TextStyle, max_width: Option<f32>) -> ShapedText;

    /// As `layout`, with each [`OBJECT_CHAR`] taking the next width of `objects`, in logical
    /// units, and placing an [`OBJECT_GLYPH`] there: room for an inline object
    /// (`crate::inline`). An object with no width left in `objects` takes none.
    fn layout_objects(&self, text: &str, style: &TextStyle, max_width: Option<f32>, objects: &[f32]) -> ShapedText {
        let _ = objects;
        self.layout(text, style, max_width)
    }

    /// As `layout_objects`, with the byte ranges in `fonts` set in their own font instead of
    /// `style.font` (and that font's fallbacks): a word in a variable font instance of its
    /// own (MO-81). Ranges are in order and do not overlap. Size and line height stay the
    /// style's.
    fn layout_fonts(
        &self,
        text: &str,
        style: &TextStyle,
        max_width: Option<f32>,
        objects: &[f32],
        fonts: &[(Range<usize>, FontId)],
    ) -> ShapedText {
        let _ = fonts;
        self.layout_objects(text, style, max_width, objects)
    }

    /// As `layout_fonts`, with each range in `fonts` keeping the advances `style.font` gives
    /// it: its own font changes how its glyphs are drawn, never where they or the words after
    /// them stand, nor where lines break. A range's advances are scaled to the width the
    /// style's font shapes it to. For a voice that breathes through a variable font (MO-81)
    /// without moving the text.
    fn layout_fonts_in_place(
        &self,
        text: &str,
        style: &TextStyle,
        max_width: Option<f32>,
        objects: &[f32],
        fonts: &[(Range<usize>, FontId)],
    ) -> ShapedText {
        let _ = fonts;
        self.layout_objects(text, style, max_width, objects)
    }
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
        self.layout_objects(text, style, max_width, &[])
    }

    fn layout_objects(&self, text: &str, style: &TextStyle, max_width: Option<f32>, objects: &[f32]) -> ShapedText {
        if style.vertical {
            return self.vertical(text, style);
        }
        let mut objects = objects.iter();
        let em = style.size;
        let space = self.space * em;
        let mut glyphs = Vec::new();
        let (mut x, mut line, mut widest) = (0.0f32, 0usize, 0.0f32);
        let mut any = false;
        let first_baseline = self.ascent * em;
        let mut starts: Vec<(usize, f32, f32)> = vec![(0, 0.0, first_baseline)];
        // With stops: each word as (line, bytes, x, width).
        let mut placed_words: Vec<(usize, Range<usize>, f32, f32)> = Vec::new();
        for (start, word) in words(text) {
            let object = (word == OBJECT_TEXT).then(|| objects.next().copied().unwrap_or(0.0));
            let w = object.unwrap_or(word.chars().count() as f32 * self.advance * em);
            if any {
                // The tolerance absorbs rounding in widths that went through layout and back.
                let fits = max_width.is_none_or(|max| x + space + w <= max + FIT_TOLERANCE);
                if fits {
                    x += space;
                } else {
                    widest = widest.max(x);
                    starts.last_mut().expect("a line").1 = x;
                    line += 1;
                    starts.push((glyphs.len(), 0.0, line as f32 * style.line_height + first_baseline));
                    x = 0.0;
                }
            }
            let baseline = line as f32 * style.line_height + self.ascent * em;
            if style.stops {
                placed_words.push((line, start..start + word.len(), x, w));
            }
            if let Some(w) = object {
                glyphs.push(PlacedGlyph {
                    glyph: OBJECT_GLYPH,
                    font: style.font,
                    pos: Point::new(x, baseline),
                    cluster: start as u32,
                    advance: w,
                });
                x += w;
                any = true;
                continue;
            }
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
        starts.last_mut().expect("a line").1 = x;
        let mut placed = if any { align_lines(&mut glyphs, &starts, widest, style.align) } else { Vec::new() };
        let mut lines = placed.len();
        let max_lines = style.max_lines as usize;
        if max_lines > 0 && lines > max_lines {
            lines = max_lines;
            placed.truncate(lines);
            glyphs.truncate(placed.last().map_or(0, |l| l.glyphs.end));
        }
        let mut out = ShapedText {
            size: Size::new(widest, lines as f32 * style.line_height),
            glyphs,
            lines: placed,
            stops: Vec::new(),
        };
        if style.stops {
            self.stops(text, style, &placed_words, &mut out);
        }
        out
    }
}

impl EstimateText {
    /// Fills `Line::bytes` and the caret stops from the placed words. The space between two
    /// words, however many characters it holds, is one `space` wide, as the layout draws it.
    fn stops(&self, text: &str, style: &TextStyle, words: &[(usize, Range<usize>, f32, f32)], out: &mut ShapedText) {
        let space = self.space * style.size;
        if out.lines.is_empty() {
            // An empty or all-space text: one empty line, so a caret has somewhere to stand.
            let end = trim_break(text, 0, text.len());
            out.lines.push(Line {
                glyphs: 0..0,
                x: 0.0,
                width: 0.0,
                baseline: self.ascent * style.size,
                bytes: 0..end,
            });
            out.size.h = style.line_height;
            push_stops(text, 0, [(0..end, 0.0, 0.0)], (end, 0.0), &mut out.stops);
            return;
        }
        for line in 0..out.lines.len() {
            let on = words.iter().filter(|w| w.0 == line);
            let start = if line == 0 { 0 } else { words.iter().find(|w| w.0 == line).map_or(0, |w| w.1.start) };
            let end = words.iter().find(|w| w.0 == line + 1).map_or(text.len(), |w| w.1.start);
            let end = trim_break(text, start, end);
            let mut spans = Vec::new();
            // The byte and x where the last word ended.
            let mut at = (start, 0.0f32);
            for (_, bytes, x, w) in on {
                spans.push((at.0..bytes.start, at.1, *x));
                spans.push((bytes.clone(), *x, x + w));
                at = (bytes.end, x + w);
            }
            let end_x = if at.0 < end { at.1 + space } else { at.1 };
            spans.push((at.0..end, at.1, end_x));
            push_stops(text, line, spans, (end, end_x), &mut out.stops);
            out.lines[line].bytes = start..end;
        }
        align_stops(&mut out.stops, &out.lines);
    }

    /// Vertical text as `ShapedLayout::vertical` sets it, with every character one em tall
    /// and `advance` ems wide, hanging `ascent` ems from its top, and a space `space` ems
    /// tall. `style.max_lines` caps the number of characters.
    pub fn vertical(&self, text: &str, style: &TextStyle) -> ShapedText {
        let em = style.size;
        let max = style.max_lines as usize;
        let mut out = ShapedText::default();
        let (mut y, mut any) = (0.0f32, false);
        for (i, c) in text.char_indices() {
            if c.is_whitespace() {
                y += self.space * em + style.tracking;
                any = true;
                continue;
            }
            if max > 0 && out.glyphs.len() == max {
                break;
            }
            let pos = Point::new(0.0, y + self.ascent * em);
            out.glyphs.push(PlacedGlyph {
                glyph: c as u32,
                font: style.font,
                pos,
                cluster: i as u32,
                advance: self.advance * em,
            });
            y += em + style.tracking;
            any = true;
        }
        let column = if out.glyphs.is_empty() { 0.0 } else { self.advance * em };
        out.size = Size::new(column, if any { (y - style.tracking).max(0.0) } else { 0.0 });
        out
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

/// Scales the advances (and offsets) of the glyphs in each range of `fonts` so the range is
/// as wide as `base`, the same text shaped in the style's font alone.
fn hold_advances(glyphs: &mut [Shaped], base: &[Shaped], fonts: &[(Range<usize>, FontId)]) {
    let within = |g: &Shaped, r: &Range<usize>| r.contains(&(g.cluster as usize));
    for (range, _) in fonts {
        let want: f32 = base.iter().filter(|g| within(g, range)).map(|g| g.advance).sum();
        let have: f32 = glyphs.iter().filter(|g| within(g, range)).map(|g| g.advance).sum();
        if have <= 0.0 {
            continue;
        }
        let k = want / have;
        for g in glyphs.iter_mut().filter(|g| within(g, range)) {
            g.advance *= k;
            g.dx *= k;
        }
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
        self.layout_objects(text, style, max_width, &[])
    }

    fn layout_objects(&self, text: &str, style: &TextStyle, max_width: Option<f32>, objects: &[f32]) -> ShapedText {
        self.layout_fonts(text, style, max_width, objects, &[])
    }

    fn layout_fonts(
        &self,
        text: &str,
        style: &TextStyle,
        max_width: Option<f32>,
        objects: &[f32],
        fonts: &[(Range<usize>, FontId)],
    ) -> ShapedText {
        self.lay(text, style, max_width, objects, fonts, false)
    }

    fn layout_fonts_in_place(
        &self,
        text: &str,
        style: &TextStyle,
        max_width: Option<f32>,
        objects: &[f32],
        fonts: &[(Range<usize>, FontId)],
    ) -> ShapedText {
        self.lay(text, style, max_width, objects, fonts, true)
    }
}

impl ShapedLayout {
    /// `layout_fonts`, or with `in_place` `layout_fonts_in_place`.
    fn lay(
        &self,
        text: &str,
        style: &TextStyle,
        max_width: Option<f32>,
        objects: &[f32],
        fonts: &[(Range<usize>, FontId)],
        in_place: bool,
    ) -> ShapedText {
        let Some(face) = self.fonts.get(style.font) else {
            return EstimateText::default().layout_objects(text, style, max_width, objects);
        };
        if style.vertical {
            return self.vertical(text, style);
        }
        if text.is_empty() {
            if !style.stops {
                return ShapedText::default();
            }
            // Editable and empty: one empty line, so a caret has somewhere to stand.
            let m = face.skrifa().metrics(FontSize::new(style.size), face.location());
            let baseline = 0.5 * (style.line_height - (m.ascent - m.descent)) + m.ascent;
            return ShapedText {
                size: Size::new(0.0, style.line_height),
                glyphs: Vec::new(),
                lines: vec![Line { glyphs: 0..0, x: 0.0, width: 0.0, baseline, bytes: 0..0 }],
                stops: vec![CaretStop { byte: 0, line: 0, x: 0.0 }],
            };
        }
        let upem = face.units_per_em();
        let k = style.size / upem as f32;
        let shaper = ShaperFont::new(face.shaping()).with_scale(upem as i32);
        let runs = self.runs_in(text, style.font, fonts);
        let mut glyphs = self.shape_these(text, style, (&face, &shaper), k, (&[], Direction::LeftToRight), runs);
        if in_place && !fonts.is_empty() {
            let plain = self.runs_one(text, style.font);
            let base = self.shape_these(text, style, (&face, &shaper), k, (&[], Direction::LeftToRight), plain);
            hold_advances(&mut glyphs, &base, fonts);
        }
        size_objects(text, &mut glyphs, objects, style.tracking);
        let mut segments = segments(text, &glyphs, style.tracking);
        if let Some(max) = max_width {
            segments = split_long_words(text, segments, &glyphs, style.tracking, max);
        }
        let mut lines = fit(&segments, max_width);
        if let (true, Some(max)) = (style.balance && lines.len() > 1, max_width) {
            lines = balance(&segments, lines, max);
        }
        let max_lines = style.max_lines as usize;
        let dropped = max_lines > 0 && lines.len() > max_lines;
        if dropped {
            lines.truncate(max_lines);
        }

        let m = face.skrifa().metrics(FontSize::new(style.size), face.location());
        let first_baseline = 0.5 * (style.line_height - (m.ascent - m.descent)) + m.ascent;
        let mut ellipsis: Option<Ellipsis> = None;
        let mut out = ShapedText::default();
        let mut ends: Vec<f32> = Vec::new();
        let mut widest = 0.0f32;
        let mut line_starts: Vec<(usize, f32, f32)> = Vec::with_capacity(lines.len());
        let mut line_bytes: Vec<Range<usize>> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let baseline = first_baseline + i as f32 * style.line_height;
            let range = segments[line.start].glyphs.start..segments[line.end - 1].glyphs.end;
            let start = out.glyphs.len();
            ends.clear();
            let (mut x, mut width) = (0.0f32, 0.0f32);
            // The first cluster an ellipsis dropped, if one did.
            let mut cut: Option<u32> = None;
            for g in &glyphs[range.clone()] {
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
                let (dots, dots_width) = ellipsis.get_or_insert_with(|| ellipsis_glyphs(&face, &shaper, k));
                let limit = max_width.map_or(f32::INFINITY, |max| max - *dots_width + FIT_TOLERANCE);
                // Drop whole clusters from the end until the ellipsis fits after them.
                while ends.last().is_some_and(|&end| end > limit) {
                    let cluster = out.glyphs[start + ends.len() - 1].cluster;
                    while !ends.is_empty() && out.glyphs[start + ends.len() - 1].cluster == cluster {
                        ends.pop();
                    }
                    cut = Some(cluster);
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
            line_starts.push((start, width, baseline));
            if style.stops {
                let bytes =
                    line_stops(text, &glyphs, (i, line, range), (&segments, style.tracking, cut), &mut out.stops);
                line_bytes.push(bytes);
            }
        }
        out.lines = align_lines(&mut out.glyphs, &line_starts, widest, style.align);
        out.size = Size::new(widest, lines.len() as f32 * style.line_height);
        if style.stops {
            for (l, bytes) in out.lines.iter_mut().zip(line_bytes) {
                l.bytes = bytes;
            }
            align_stops(&mut out.stops, &out.lines);
            // A text that ends in a line break has an empty last line for the caret.
            if !dropped && text.chars().next_back().is_some_and(is_hard_break) {
                let line = out.lines.len();
                let x = style.align.factor() * widest;
                let baseline = first_baseline + line as f32 * style.line_height;
                let n = out.glyphs.len();
                out.lines.push(Line { glyphs: n..n, x, width: 0.0, baseline, bytes: text.len()..text.len() });
                out.stops.push(CaretStop { byte: text.len(), line, x });
                out.size.h += style.line_height;
            }
        }
        out
    }
}

/// Pushes the caret stops of line `i` of a shaped layout, before alignment, and returns the
/// line's bytes. `line` is its range of segments and `range` its range of shaped glyphs; `cut`
/// is the first cluster an ellipsis dropped from it.
fn line_stops(
    text: &str,
    glyphs: &[Shaped],
    (i, line, range): (usize, &Range<usize>, Range<usize>),
    (segments, tracking, cut): (&[Segment], f32, Option<u32>),
    out: &mut Vec<CaretStop>,
) -> Range<usize> {
    let cluster_at = |g: usize| glyphs.get(g).map_or(text.len(), |g| g.cluster as usize);
    let start = if i == 0 { 0 } else { cluster_at(range.start) };
    let next = segments.get(line.end).map_or(text.len(), |s| cluster_at(s.glyphs.start));
    let mut end = trim_break(text, start, next);
    if let Some(c) = cut {
        end = end.min(c as usize);
    }
    let mut spans: Vec<(Range<usize>, f32, f32)> = Vec::new();
    let mut x = 0.0f32;
    for g in &glyphs[range] {
        let c = g.cluster as usize;
        if c >= end {
            break;
        }
        let advance = g.advance + tracking;
        match spans.last_mut() {
            Some(span) if span.0.start == c => span.2 += advance,
            last => {
                if let Some(span) = last {
                    span.0.end = c;
                }
                spans.push((c..end, x, x + advance));
            }
        }
        x += advance;
    }
    if let Some(first) = spans.first_mut() {
        first.0.start = start;
    }
    push_stops(text, i, spans, (end, x), out);
    start..end
}

/// One shaped glyph, scaled to logical units, before line fitting.
struct Shaped {
    id: u32,
    font: FontId,
    /// Byte offset of its cluster in the whole text.
    cluster: u32,
    /// Pen advance without tracking.
    advance: f32,
    /// Downward pen advance, for text shaped top to bottom; zero otherwise.
    v_advance: f32,
    dx: f32,
    dy: f32,
}

impl ShapedLayout {
    /// Shapes `text` run by run: each run is a stretch of characters the same face has. A
    /// character goes to the first face of `style.font` and its fallbacks that has it.
    /// Combining marks stay in the run they are in, so an accent stays with its letter, and
    /// so does whitespace the run's face has, so a space between two Japanese words does not
    /// split the run.
    fn shape_runs_with(
        &self,
        text: &str,
        style: &TextStyle,
        primary: (&Face, &ShaperFont<'_, '_>),
        k: f32,
        shaping: (&[Feature], Direction),
    ) -> Vec<Shaped> {
        let runs = self.runs(text, style.font);
        self.shape_these(text, style, primary, k, shaping, runs)
    }

    /// Shapes `text` in the given runs, with these features in this direction.
    fn shape_these(
        &self,
        text: &str,
        style: &TextStyle,
        primary: (&Face, &ShaperFont<'_, '_>),
        k: f32,
        (features, direction): (&[Feature], Direction),
        runs: Vec<(Range<usize>, FontId)>,
    ) -> Vec<Shaped> {
        let mut out = Vec::with_capacity(text.len());
        for (range, font) in runs {
            let face = if font == style.font { None } else { self.fonts.get(font) };
            let (buf, k) = match &face {
                None => (shape_with(primary.0, primary.1, &text[range.clone()], features, direction), k),
                Some(face) => {
                    let upem = face.units_per_em();
                    let shaper = ShaperFont::new(face.shaping()).with_scale(upem as i32);
                    (shape_with(face, &shaper, &text[range.clone()], features, direction), style.size / upem as f32)
                }
            };
            for (info, p) in buf.glyph_infos().iter().zip(buf.glyph_positions()) {
                out.push(Shaped {
                    id: info.glyph_id,
                    font,
                    cluster: info.cluster + range.start as u32,
                    advance: p.x_advance as f32 * k,
                    v_advance: -(p.y_advance as f32) * k,
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
        self.runs_in(text, primary, &[])
    }

    /// As `runs`, with the byte ranges in `fonts` in their own font and its fallbacks.
    fn runs_in(&self, text: &str, primary: FontId, fonts: &[(Range<usize>, FontId)]) -> Vec<(Range<usize>, FontId)> {
        if fonts.is_empty() {
            return self.runs_one(text, primary);
        }
        let mut runs: Vec<(Range<usize>, FontId)> = Vec::new();
        let mut at = 0;
        let push = |runs: &mut Vec<(Range<usize>, FontId)>, range: Range<usize>, font: FontId| {
            for (r, f) in self.runs_one(&text[range.clone()], font) {
                let r = r.start + range.start..r.end + range.start;
                match runs.last_mut() {
                    Some((last, lf)) if *lf == f && last.end == r.start => last.end = r.end,
                    _ => runs.push((r, f)),
                }
            }
        };
        for (range, font) in fonts {
            let range = range.start.max(at).min(text.len())..range.end.min(text.len());
            if range.start >= range.end {
                continue;
            }
            if at < range.start {
                push(&mut runs, at..range.start, primary);
            }
            push(&mut runs, range.clone(), *font);
            at = range.end;
        }
        if at < text.len() {
            push(&mut runs, at..text.len(), primary);
        }
        runs
    }

    fn runs_one(&self, text: &str, primary: FontId) -> Vec<(Range<usize>, FontId)> {
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

impl ShapedLayout {
    /// Vertical text (FX-005, EG-30): one column, top to bottom, no wrapping, for labels.
    /// The text is shaped top to bottom, so Japanese faces swap in their vertical forms
    /// (`vert`, `vrt2`): the long vowel mark and the brackets turn, and the small kana and
    /// punctuation sit in the corner they do in vertical type. Each glyph advances the pen
    /// by its vertical advance (`vmtx`) and hangs from its vertical origin (`VORG`), centred
    /// across the column; a face with no vertical metrics stacks by its line height
    /// (ascent plus descent) and hangs each glyph from its ascent, as HarfBuzz does. Latin
    /// letters and digits stand upright. The column is as wide as the widest glyph advance
    /// and as tall as the sum of the vertical advances, plus `style.tracking` between
    /// characters. A space advances the pen and places no glyph. `style.max_lines` caps the
    /// number of characters. Fallback runs work as in `layout`.
    pub fn vertical(&self, text: &str, style: &TextStyle) -> ShapedText {
        let Some(face) = self.fonts.get(style.font) else {
            return ShapedText::default();
        };
        let upem = face.units_per_em();
        let k = style.size / upem as f32;
        let shaper = ShaperFont::new(face.shaping()).with_scale(upem as i32);
        let glyphs = self.shape_runs_with(text, style, (&face, &shaper), k, (&[], Direction::TopToBottom));

        // Shaped top to bottom, a glyph has no horizontal advance; the column needs its width,
        // as `(logical units, font units)`.
        let mut widths: Vec<(FontId, Option<Arc<Face>>)> = Vec::new();
        let width = |font: FontId, id: u32, widths: &mut Vec<(FontId, Option<Arc<Face>>)>| {
            let face = match widths.iter().find(|(f, _)| *f == font) {
                Some((_, face)) => face.clone(),
                None => {
                    let face = self.fonts.get(font);
                    widths.push((font, face.clone()));
                    face
                }
            };
            face.and_then(|face| {
                let upem = face.units_per_em() as f32;
                let units = face
                    .skrifa()
                    .glyph_metrics(FontSize::unscaled(), face.location())
                    .advance_width(skrifa::GlyphId::new(id))?;
                Some((units * style.size / upem, units))
            })
            .unwrap_or((style.size, 0.0))
        };

        // Characters as (cluster, glyphs, advance down, widest glyph); joiners and marks
        // stay with the one before them, because they share its cluster after shaping.
        let mut cells: Vec<(u32, Range<usize>, f32, f32)> = Vec::new();
        for (i, g) in glyphs.iter().enumerate() {
            let w = width(g.font, g.id, &mut widths).0;
            match cells.last_mut() {
                Some((cluster, range, down, wide)) if *cluster == g.cluster => {
                    range.end = i + 1;
                    *down += g.v_advance;
                    *wide = wide.max(w);
                }
                _ => cells.push((g.cluster, i..i + 1, g.v_advance, w)),
            }
        }
        let max = style.max_lines as usize;
        let mut count = 0usize;
        let mut kept = Vec::with_capacity(cells.len());
        for cell in cells {
            if !is_space(text, cell.0) {
                if max > 0 && count == max {
                    break;
                }
                count += 1;
            }
            kept.push(cell);
        }
        let column = kept.iter().filter(|c| !is_space(text, c.0)).fold(0.0f32, |w, c| w.max(c.3));
        let centre = 0.5 * column;
        let mut y = 0.0f32;
        let mut out = ShapedText::default();
        for (cluster, range, down, _) in kept {
            if !is_space(text, cluster) {
                let mut pen = y;
                for g in &glyphs[range] {
                    // Offsets are y up and already hold the vertical origin. Its x is half
                    // the advance in whole font units, so it is put back and the glyph centred
                    // exactly.
                    let (advance, units) = width(g.font, g.id, &mut widths);
                    let origin = if units > 0.0 { (units as i32 / 2) as f32 * advance / units } else { 0.0 };
                    let pos = Point::new(centre - 0.5 * advance + g.dx + origin, pen - g.dy);
                    out.glyphs.push(PlacedGlyph { glyph: g.id, font: g.font, pos, cluster, advance });
                    pen += g.v_advance;
                }
            }
            y += down + style.tracking;
        }
        let height = if out.glyphs.is_empty() && column == 0.0 { 0.0 } else { (y - style.tracking).max(0.0) };
        out.size = Size::new(column, height);
        out
    }
}

/// Autofit (FX-049): the largest of the sizes `style.size`, `style.size - step`, and so on
/// down to `min_size`, at which `text` fits in `room` (no taller, and no line wider), with
/// the line height and tracking scaled with it. Steps keep the size from creeping by tiny
/// amounts as text grows. When nothing fits, the style at `min_size`, which then overflows
/// (or ellipsises, with `style.max_lines` and `style.ellipsis`).
pub fn fit_size(
    layout: &dyn TextLayout,
    text: &str,
    style: &TextStyle,
    room: Size,
    min_size: f32,
    step: f32,
) -> TextStyle {
    let min_size = min_size.min(style.size);
    let step = step.max(0.25);
    let at = |size: f32| {
        let k = size / style.size;
        TextStyle { size, line_height: style.line_height * k, tracking: style.tracking * k, ..*style }
    };
    let fits = |st: &TextStyle| {
        let s = layout.layout(text, st, Some(room.w));
        s.size.h <= room.h + FIT_TOLERANCE && s.size.w <= room.w + FIT_TOLERANCE
    };
    let n = ((style.size - min_size) / step).floor() as i32;
    (0..=n).map(|i| at(style.size - i as f32 * step)).find(fits).unwrap_or_else(|| at(min_size))
}

/// Characters that belong to the one before them and stay in its run: combining marks,
/// the kana voicing marks, joiners and variation selectors.
fn joins(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0x3099..=0x309A
        | 0x200C..=0x200D | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F | 0xE0100..=0xE01EF)
}

fn shape(face: &Face, shaper: &ShaperFont<'_, '_>, text: &str) -> harfrust::Buffer {
    shape_with(face, shaper, text, &[], Direction::LeftToRight)
}

/// Shapes `text` with the face's cached plan for its script and `features`
/// (`Face::plan`), so laying out text again does not compile a plan again.
fn shape_with(
    face: &Face,
    shaper: &ShaperFont<'_, '_>,
    text: &str,
    features: &[Feature],
    direction: Direction,
) -> harfrust::Buffer {
    let mut buf = harfrust::Buffer::new();
    buf.push_str(text);
    buf.guess_segment_properties();
    buf.set_direction(direction);
    let plan = face.plan(buf.direction(), buf.script(), buf.language(), features);
    let options = harfrust::ShapeOptions::new().plan(Some(&plan)).features(features);
    harfrust::shape(shaper, &mut buf, options).expect("shaping a text buffer");
    buf
}

/// Ellipsis glyphs as `(glyph, x offset, y offset)` from the pen start, and their width.
type Ellipsis = (Vec<(u32, f32, f32)>, f32);

/// The ellipsis, or three full stops when the font has no ellipsis character.
fn ellipsis_glyphs(face: &Face, shaper: &ShaperFont<'_, '_>, k: f32) -> Ellipsis {
    let mut buf = shape(face, shaper, "\u{2026}");
    if buf.glyph_infos().iter().any(|g| g.glyph_id == 0) {
        buf = shape(face, shaper, "...");
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

/// Gives each object character the next width of `objects` (tracking included) and the
/// placeholder glyph. Extra glyphs of an object's cluster take no room.
fn size_objects(text: &str, glyphs: &mut [Shaped], objects: &[f32], tracking: f32) {
    if !text.contains(OBJECT_CHAR) {
        return;
    }
    let mut objects = objects.iter();
    let mut last = None;
    for g in glyphs {
        if !text.get(g.cluster as usize..).is_some_and(|s| s.starts_with(OBJECT_CHAR)) {
            continue;
        }
        let first = last != Some(g.cluster);
        last = Some(g.cluster);
        let width = if first { objects.next().copied().unwrap_or(0.0) } else { 0.0 };
        (g.id, g.advance, g.dx, g.dy) = (OBJECT_GLYPH, width - tracking, 0.0, 0.0);
    }
}

/// Even line lengths (TY25): the narrowest width at which the greedy fill still takes no
/// more lines than it does at `max`, found by bisection. Like CSS `text-wrap: balance`, the
/// last line comes out close to the others instead of holding one stray word.
fn balance(segments: &[Segment], greedy: Vec<Range<usize>>, max: f32) -> Vec<Range<usize>> {
    let count = greedy.len();
    let total: f32 = segments.iter().map(|s| s.width).sum();
    let widest_piece = segments.iter().map(|s| s.fit_width).fold(0.0f32, f32::max);
    let (mut lo, mut hi) = ((total / count as f32).max(widest_piece).min(max), max);
    let tightest = fit(segments, Some(lo));
    if tightest.len() <= count {
        return tightest;
    }
    let mut best = greedy;
    // Half a logical unit is below what a reader can see.
    while hi - lo > 0.5 {
        let mid = 0.5 * (lo + hi);
        let lines = fit(segments, Some(mid));
        if lines.len() <= count {
            (hi, best) = (mid, lines);
        } else {
            lo = mid;
        }
    }
    best
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

    const STYLE: TextStyle = TextStyle::new(0, 20.0, 26.0);

    fn gid(t: &ShapedLayout, c: char) -> u32 {
        let face = t.fonts().get(0).unwrap();
        face.skrifa().charmap().map(c).unwrap().to_u32()
    }

    fn advance(t: &ShapedLayout, c: char, size: f32) -> f32 {
        let face = t.fonts().get(0).unwrap();
        let font = face.skrifa();
        font.glyph_metrics(FontSize::new(size), face.location()).advance_width(font.charmap().map(c).unwrap()).unwrap()
    }

    #[test]
    fn laying_out_text_again_reuses_its_shape_plan() {
        let t = shaped();
        let first = t.layout("Hello there", &STYLE, Some(60.0));
        let compiled = t.fonts().get(0).unwrap().plans_compiled();
        assert_eq!(compiled, 1);
        let again = t.layout("Hello there", &STYLE, Some(60.0));
        assert_eq!(again, first);
        // Other Latin text and another size share the plan too.
        t.layout("Other words", &TextStyle::new(0, 14.0, 18.0), None);
        assert_eq!(t.fonts().get(0).unwrap().plans_compiled(), 1);
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
    fn vertical_stacks_one_upright_character_per_line() {
        let (t, latin, jp) = with_japanese();
        let text = "音声 AI";
        let s = t.vertical(text, &STYLE);
        assert_eq!(s.glyphs.len(), 4);
        // One em wide, as wide as the ideographs. Each ideograph and the space after them
        // advance one em (the Japanese face's vmtx); the Latin face has no vertical metrics,
        // so its letters advance by its line height (ascent plus descent).
        assert!((s.size.w - 20.0).abs() < 1e-3, "{}", s.size.w);
        let ys: Vec<f32> = s.glyphs.iter().map(|g| g.pos.y).collect();
        assert!((ys[1] - ys[0] - 20.0).abs() < 1e-3, "{ys:?}");
        let latin_face = t.fonts().get(latin).unwrap();
        let m = latin_face.skrifa().metrics(FontSize::new(20.0), latin_face.location());
        let line = m.ascent - m.descent;
        assert!((s.size.h - (3.0 * 20.0 + 2.0 * line)).abs() < 0.05, "{} {line}", s.size.h);
        assert!((ys[3] - ys[2] - line).abs() < 0.05, "{ys:?}");
        // Each glyph is centred across the column: a narrow I sits further right than A.
        let (a, i) = (&s.glyphs[2], &s.glyphs[3]);
        assert!((a.pos.x + 0.5 * a.advance - 10.0).abs() < 1e-3, "{a:?}");
        assert!((i.pos.x + 0.5 * i.advance - 10.0).abs() < 1e-3);
        assert!(i.pos.x > a.pos.x);
        assert_eq!([s.glyphs[0].font, s.glyphs[3].font], [jp, latin]);
        // max_lines caps the characters.
        assert_eq!(t.vertical(text, &STYLE.clamp_lines(3)).glyphs.len(), 3);
    }

    #[test]
    fn vertical_uses_the_vertical_forms() {
        let fonts = FontRegistry::new();
        let jp = fonts.asset("jp-labels").unwrap();
        let t = ShapedLayout::new(fonts);
        let style = TextStyle { font: jp, ..STYLE };
        // The long vowel mark turns to a vertical bar in vertical text.
        let across = t.layout("ー", &style, None).glyphs[0].glyph;
        let down = t.vertical("ー", &style).glyphs[0].glyph;
        assert_ne!(across, down);
    }

    #[test]
    fn fit_size_steps_down_until_the_text_fits() {
        // Estimate: 0.55 em per character, 0.3 em per space.
        let t = EstimateText::default();
        let style = TextStyle::new(0, 40.0, 48.0);
        let room = Size::new(300.0, 60.0);
        // Short text fits at full size.
        assert_eq!(fit_size(&t, "short", &style, room, 20.0, 2.0).size, 40.0);
        // Four words of four letters take 9.7 em on one line, and two lines take 2.4 em of
        // height, so only one line fits: at most 300 / 9.7 = 30.9, the step 30.
        let fitted = fit_size(&t, "aaaa bbbb cccc dddd", &style, room, 20.0, 2.0);
        assert_eq!(fitted.size, 30.0);
        assert!((fitted.line_height - 36.0).abs() < 1e-3);
        let s = t.layout("aaaa bbbb cccc dddd", &fitted, Some(room.w));
        assert!(s.size.h <= room.h && s.size.w <= room.w);
        // Too much text: the minimum size.
        let long = "aaaa ".repeat(40);
        assert_eq!(fit_size(&t, &long, &style, room, 20.0, 2.0).size, 20.0);
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

    /// Each line's ink as `(left, right)`, from the glyphs that share its baseline.
    fn line_spans(s: &ShapedText) -> Vec<(f32, f32)> {
        let mut out: Vec<(f32, f32, f32)> = Vec::new();
        for g in &s.glyphs {
            match out.iter_mut().find(|l| (l.0 - g.pos.y).abs() < 0.5) {
                Some(l) => (l.1, l.2) = (l.1.min(g.pos.x), l.2.max(g.pos.x + g.advance)),
                None => out.push((g.pos.y, g.pos.x, g.pos.x + g.advance)),
            }
        }
        out.into_iter().map(|(_, a, b)| (a, b)).collect()
    }

    const PARAGRAPH: &str = "the quick brown fox jumps over the lazy dog";

    #[test]
    fn centred_lines_share_a_middle() {
        let t = shaped();
        let s = t.layout(PARAGRAPH, &STYLE.centered(), Some(150.0));
        let spans = line_spans(&s);
        assert!(spans.len() >= 3, "{spans:?}");
        let mid = 0.5 * s.size.w;
        for &(a, b) in &spans {
            assert!((0.5 * (a + b) - mid).abs() < 0.01, "line {a}..{b} against middle {mid}");
        }
        // The widest line still starts at 0, and the text is as wide as before.
        let left = t.layout(PARAGRAPH, &STYLE, Some(150.0));
        assert_eq!(s.size, left.size);
        assert!(spans.iter().any(|&(a, _)| a.abs() < 0.01));
        // Same glyphs, same lines: only x moves.
        assert_eq!(s.glyphs.len(), left.glyphs.len());
        assert!(s.glyphs.iter().zip(&left.glyphs).all(|(a, b)| a.pos.y == b.pos.y && a.pos.x >= b.pos.x));
    }

    #[test]
    fn end_aligned_lines_share_a_right_edge() {
        let t = shaped();
        let s = t.layout(PARAGRAPH, &STYLE.align(Align::End), Some(150.0));
        for (_, b) in line_spans(&s) {
            assert!((b - s.size.w).abs() < 0.01, "right edge {b} against {}", s.size.w);
        }
    }

    #[test]
    fn estimate_centres_lines_too() {
        let style = TextStyle::new(0, 10.0, 12.0).centered();
        let s = EstimateText::default().layout("abcd ef", &style, Some(30.0));
        // "abcd" is 22 wide, "ef" 11: "ef" starts 5.5 in.
        assert_eq!(s.size.w, 22.0);
        assert_eq!(s.glyphs[4].pos.x, 5.5);
    }

    #[test]
    fn balance_evens_lines_and_keeps_the_count() {
        let t = shaped();
        let text = "one two three four five six seven eight nine ten eleven";
        let width = t.layout("one two three four five six seven", &STYLE, None).size.w + 1.0;
        let greedy = line_spans(&t.layout(text, &STYLE, Some(width)));
        let even = line_spans(&t.layout(text, &STYLE.balanced(), Some(width)));
        assert_eq!(greedy.len(), even.len());
        let spread = |l: &[(f32, f32)]| {
            let w: Vec<f32> = l.iter().map(|(a, b)| b - a).collect();
            w.iter().cloned().fold(0.0, f32::max) - w.iter().cloned().fold(f32::INFINITY, f32::min)
        };
        assert!(spread(&even) < 0.5 * spread(&greedy), "{even:?} against {greedy:?}");
        assert!(even.iter().all(|(_, b)| *b <= width + FIT_TOLERANCE));
        // One line stays one line.
        assert_eq!(t.layout("one two", &STYLE.balanced(), Some(width)), t.layout("one two", &STYLE, Some(width)));
    }

    #[test]
    fn align_in_moves_the_block_into_a_wider_box() {
        let t = shaped();
        let mut s = t.layout("hello", &STYLE.centered(), None);
        let (w, x0) = (s.size.w, s.glyphs[0].pos.x);
        s.align_in(Align::Center, w + 40.0);
        assert!((s.glyphs[0].pos.x - x0 - 20.0).abs() < 1e-4);
        assert_eq!(s.size.w, w + 40.0);
        let mut left = t.layout("hello", &STYLE, None);
        left.align_in(Align::Start, w + 40.0);
        assert_eq!(left.glyphs[0].pos.x, x0);
    }

    #[test]
    fn objects_take_their_width_and_wrap_like_words() {
        let t = shaped();
        let text = format!("ab {OBJECT_TEXT} cd");
        let plain = t.layout("ab cd", &STYLE, None);
        let s = t.layout_objects(&text, &STYLE, None, &[30.0]);
        let obj = s.glyphs.iter().find(|g| g.glyph == OBJECT_GLYPH).expect("a placeholder");
        assert_eq!(obj.advance, 30.0);
        assert_eq!(obj.cluster, 3);
        let space = plain.glyphs[2].pos.x - plain.glyphs[1].pos.x - plain.glyphs[1].advance;
        assert!((s.size.w - plain.size.w - 30.0 - space).abs() < 0.01, "{} {}", s.size.w, plain.size.w);
        // A wide object goes to the next line like a word.
        let width = t.layout("ab", &STYLE, None).size.w + 5.0;
        let wrapped = t.layout_objects(&text, &STYLE, Some(width + 30.0), &[width + 20.0]);
        let obj = wrapped.glyphs.iter().find(|g| g.glyph == OBJECT_GLYPH).unwrap();
        assert_eq!(obj.pos.x, 0.0);
        assert!(obj.pos.y > wrapped.glyphs[0].pos.y);
        // The estimate does the same.
        let e = EstimateText::default().layout_objects(&text, &STYLE, None, &[30.0]);
        assert_eq!(e.glyphs.iter().find(|g| g.glyph == OBJECT_GLYPH).unwrap().advance, 30.0);
    }

    #[test]
    fn lines_report_their_glyphs_extent_and_baseline() {
        let t = shaped();
        let s = t.layout(PARAGRAPH, &STYLE.centered(), Some(150.0));
        assert!(s.lines.len() >= 3);
        assert_eq!(s.lines.first().unwrap().glyphs.start, 0);
        assert_eq!(s.lines.last().unwrap().glyphs.end, s.glyphs.len());
        for (l, (a, b)) in s.lines.iter().zip(line_spans(&s)) {
            assert!((l.x - a).abs() < 0.01 && (l.x + l.width - b).abs() < 0.01, "{l:?} {a} {b}");
            assert!(s.glyphs[l.glyphs.clone()].iter().all(|g| (g.pos.y - l.baseline).abs() < 0.5));
        }
        let mut wide = s.clone();
        wide.align_in(Align::Center, s.size.w + 20.0);
        assert!((wide.lines[0].x - s.lines[0].x - 10.0).abs() < 1e-4);
        let e = EstimateText::default().layout("ab cd", &TextStyle::new(0, 10.0, 12.0), Some(20.0));
        assert_eq!(e.lines.len(), 2);
        assert_eq!(e.lines[1].glyphs, 2..4);
        assert_eq!(e.lines[1].baseline, 12.0 + 8.0);
    }

    fn stops_of(t: &ShapedText) -> Vec<(usize, usize, f32)> {
        t.stops.iter().map(|s| (s.byte, s.line, s.x)).collect()
    }

    #[test]
    fn estimate_stops_include_spaces_and_line_ends() {
        let style = TextStyle::new(0, 10.0, 12.0).with_stops();
        let t = EstimateText::default();
        let one = t.layout("ab cd", &style, None);
        assert_eq!(stops_of(&one), [(0, 0, 0.0), (1, 0, 5.5), (2, 0, 11.0), (3, 0, 14.0), (4, 0, 19.5), (5, 0, 25.0)]);
        assert_eq!(one.lines[0].bytes, 0..5);
        // Wrapped: the break byte 3 ends line 0 after its space and starts line 1.
        let two = t.layout("ab cd", &style, Some(20.0));
        assert_eq!(
            stops_of(&two),
            [(0, 0, 0.0), (1, 0, 5.5), (2, 0, 11.0), (3, 0, 14.0), (3, 1, 0.0), (4, 1, 5.5), (5, 1, 11.0)]
        );
        assert_eq!(two.lines.iter().map(|l| l.bytes.clone()).collect::<Vec<_>>(), [0..3, 3..5]);
        // Empty text still has a line and a stop.
        let empty = t.layout("", &style, None);
        assert_eq!(stops_of(&empty), [(0, 0, 0.0)]);
        assert_eq!(empty.size.h, 12.0);
    }

    #[test]
    fn no_stops_unless_asked() {
        let t = EstimateText::default();
        let style = TextStyle::new(0, 10.0, 12.0);
        let plain = t.layout("ab cd", &style, Some(20.0));
        assert!(plain.stops.is_empty());
        assert!(plain.lines.iter().all(|l| l.bytes == (0..0)));
        let with = t.layout("ab cd", &style.with_stops(), Some(20.0));
        assert_eq!((&plain.glyphs, plain.size), (&with.glyphs, with.size));

        let s = shaped();
        for text in ["", "hello world", "one two three four five", "a\nb"] {
            let plain = s.layout(text, &STYLE, Some(90.0));
            assert!(plain.stops.is_empty(), "{text:?}");
            assert!(plain.lines.iter().all(|l| l.bytes == (0..0)), "{text:?}");
            if !text.is_empty() {
                let with = s.layout(text, &STYLE.with_stops(), Some(90.0));
                assert_eq!((&plain.glyphs, plain.size), (&with.glyphs, with.size), "{text:?}");
            }
        }
    }

    /// Every stop sits where the glyph of its cluster starts, or for a space or the line end,
    /// where the glyph before it ends.
    fn assert_stops_on_glyphs(text: &str, t: &ShapedText) {
        for s in &t.stops {
            let line = &t.lines[s.line];
            assert!(line.bytes.start <= s.byte && s.byte <= line.bytes.end, "{s:?} outside {line:?}");
            if let Some(g) = t.glyphs[line.glyphs.clone()].iter().find(|g| g.cluster as usize == s.byte) {
                assert!((g.pos.x - s.x).abs() < 1e-3, "{text:?}: {s:?} vs glyph at {}", g.pos.x);
            } else if let Some(g) = t.glyphs[line.glyphs.clone()].iter().rev().find(|g| (g.cluster as usize) < s.byte) {
                if s.byte == line.bytes.end || text[s.byte..].starts_with(char::is_whitespace) {
                    assert!(s.x + 1e-3 >= g.pos.x + g.advance, "{text:?}: {s:?} before the end of {g:?}");
                } else {
                    // Inside a ligature: its letters share its advance.
                    assert!(g.pos.x < s.x && s.x < g.pos.x + g.advance, "{text:?}: {s:?} outside {g:?}");
                }
            }
        }
        // Left to right on each line.
        for w in t.stops.windows(2) {
            if w[0].line == w[1].line {
                assert!(w[0].byte < w[1].byte && w[0].x <= w[1].x + 1e-3, "{text:?}: {w:?}");
            } else {
                assert_eq!(w[0].line + 1, w[1].line);
            }
        }
    }

    #[test]
    fn shaped_stops_on_one_line() {
        let s = shaped();
        let text = "hello  world";
        let t = s.layout(text, &STYLE.with_stops(), None);
        assert_eq!(t.stops.iter().map(|s| s.byte).collect::<Vec<_>>(), (0..=12).collect::<Vec<_>>());
        assert_eq!(t.lines[0].bytes, 0..12);
        assert_stops_on_glyphs(text, &t);
        // The end stop is after the last glyph.
        let last = t.glyphs.last().unwrap();
        assert!((t.stops.last().unwrap().x - (last.pos.x + last.advance)).abs() < 1e-3);
        // Two spaces take two stops, a space apart.
        let space = advance(&s, ' ', 20.0);
        assert!((t.stops[6].x - t.stops[5].x - space).abs() < 1e-3);
    }

    #[test]
    fn shaped_stops_at_a_soft_wrap() {
        let s = shaped();
        let text = "hello world";
        // Wide enough for either word, not both.
        let width = s.layout("world", &STYLE, None).size.w.max(s.layout("hello", &STYLE, None).size.w) + 1.0;
        let t = s.layout(text, &STYLE.with_stops(), Some(width));
        assert_eq!(t.lines.len(), 2);
        assert_eq!(t.lines.iter().map(|l| l.bytes.clone()).collect::<Vec<_>>(), [0..6, 6..11]);
        let at_break: Vec<_> = t.stops.iter().filter(|s| s.byte == 6).map(|s| (s.line, s.x)).collect();
        assert_eq!(at_break.len(), 2);
        assert_eq!(at_break[1], (1, 0.0));
        // The end of line 0 is after its trailing space.
        let hello_space = s.layout("hello", &STYLE, None).size.w + advance(&s, ' ', 20.0);
        assert!((at_break[0].1 - hello_space).abs() < 1e-3, "{at_break:?}");
        assert_stops_on_glyphs(text, &t);
    }

    #[test]
    fn shaped_stops_at_hard_breaks() {
        let s = shaped();
        let t = s.layout("ab\ncd", &STYLE.with_stops(), None);
        assert_eq!(t.lines.iter().map(|l| l.bytes.clone()).collect::<Vec<_>>(), [0..2, 3..5]);
        assert_eq!(
            t.stops.iter().map(|s| (s.byte, s.line)).collect::<Vec<_>>(),
            [(0, 0), (1, 0), (2, 0), (3, 1), (4, 1), (5, 1)]
        );
        assert_eq!(t.stops[3].x, 0.0);
        // A break at the end makes an empty last line for the caret.
        let t = s.layout("ab\n", &STYLE.with_stops(), None);
        assert_eq!(t.lines.len(), 2);
        assert_eq!(t.lines[1].bytes, 3..3);
        assert_eq!(t.stops.last().map(|s| (s.byte, s.line)), Some((3, 1)));
        assert_eq!(t.size.h, 2.0 * STYLE.line_height);
        // Empty text: one line, one stop.
        let t = s.layout("", &STYLE.with_stops(), None);
        assert_eq!(stops_of(&t), [(0, 0, 0.0)]);
        assert_eq!(t.size.h, STYLE.line_height);
    }

    #[test]
    fn shaped_stops_land_on_grapheme_clusters() {
        let s = shaped();
        // e + combining acute, a skin-tone emoji, a ZWJ family, then x.
        let text = "e\u{301}a 👍🏽 👨\u{200d}👩\u{200d}👧x";
        let t = s.layout(text, &STYLE.with_stops(), None);
        let bytes: Vec<usize> = t.stops.iter().map(|s| s.byte).collect();
        assert_eq!(bytes, [0, 3, 4, 5, 13, 14, 32, 33]);
        assert_stops_on_glyphs(text, &t);
    }

    #[test]
    fn shaped_stops_follow_alignment() {
        let s = shaped();
        let text = "one two three four five six";
        for align in [Align::Start, Align::Center, Align::End] {
            let style = STYLE.with_stops().align(align);
            let mut t = s.layout(text, &style, Some(120.0));
            assert!(t.lines.len() > 1);
            assert_stops_on_glyphs(text, &t);
            t.align_in(align, t.size.w + 40.0);
            assert_stops_on_glyphs(text, &t);
            for l in &t.lines {
                let first =
                    t.stops.iter().find(|s| s.byte == l.bytes.start && t.lines[s.line].bytes == l.bytes).unwrap();
                assert!((first.x - l.x).abs() < 1e-3, "{align:?}: {first:?} vs line at {}", l.x);
            }
        }
    }

    #[test]
    fn soft_wrap_affinity() {
        // "ab " / "cd": byte 3 ends line 0 (x 14, after the space) and starts line 1 (x 0).
        let t = EstimateText::default().layout("ab cd", &TextStyle::new(0, 10.0, 12.0).with_stops(), Some(20.0));
        assert_eq!(t.line_of(3, Affinity::Upstream), 0);
        assert_eq!(t.line_of(3, Affinity::Downstream), 1);
        assert_eq!(t.line_of(1, Affinity::Upstream), 0);
        assert_eq!(t.stop_at(3, Affinity::Upstream).map(|s| (s.line, s.x)), Some((0, 14.0)));
        assert_eq!(t.stop_at(3, Affinity::Downstream).map(|s| (s.line, s.x)), Some((1, 0.0)));
        // A click past the end of line 0 stays on line 0; one at the start of line 1 is there.
        assert_eq!(t.hit(Point::new(50.0, 5.0)), (3, Affinity::Upstream));
        assert_eq!(t.hit(Point::new(-5.0, 17.0)), (3, Affinity::Downstream));
        assert_eq!(t.hit(Point::new(6.0, 17.0)), (4, Affinity::Downstream));
        // Above and below the text clamp to the first and last line.
        assert_eq!(t.hit(Point::new(0.0, -40.0)), (0, Affinity::Downstream));
        assert_eq!(t.hit(Point::new(99.0, 99.0)), (5, Affinity::Downstream));
    }

    #[test]
    fn selection_rects_per_line() {
        let t = EstimateText::default().layout("ab cd", &TextStyle::new(0, 10.0, 12.0).with_stops(), Some(20.0));
        // "b cd": the end of line 0 with its space, and all of line 1.
        assert_eq!(t.selection_rects(1..5), [Rect::new(5.5, 0.0, 8.5, 12.0), Rect::new(0.0, 12.0, 11.0, 12.0)]);
        // Ending at the wrap draws nothing on line 1; starting there nothing on line 0.
        assert_eq!(t.selection_rects(0..3), [Rect::new(0.0, 0.0, 14.0, 12.0)]);
        assert_eq!(t.selection_rects(3..4), [Rect::new(0.0, 12.0, 5.5, 12.0)]);
        assert!(t.selection_rects(2..2).is_empty());
    }

    #[test]
    fn shaped_text_drives_the_edit_model() {
        use vo_core::edit::{EditAction, EditBuffer, Selection};
        let s = shaped();
        let text = "one two three four five six seven";
        let style = STYLE.with_stops();
        let layout = s.layout(text, &style, Some(120.0));
        assert!(layout.lines.len() >= 3);
        let mut b = EditBuffer::new(text);
        b.set_selection(Selection::caret(2));
        let x0 = layout.stop_at(2, Affinity::Downstream).unwrap().x;
        // Down keeps x: the caret lands on the stop nearest to it on each next line.
        for line in 1..layout.lines.len() {
            b.apply(EditAction::MoveLine { down: true, extend: false }, &layout, 0.0);
            let sel = b.selection();
            assert_eq!(layout.line_of(sel.focus, sel.affinity), line);
            let x = layout.stop_at(sel.focus, sel.affinity).unwrap().x;
            let best = layout.line_stops(line).iter().map(|s| (s.x - x0).abs()).fold(f32::MAX, f32::min);
            assert!(((x - x0).abs() - best).abs() < 1e-3, "line {line}: x {x} for {x0}");
        }
        // End on a wrapped line stays on it, upstream of the break.
        b.set_selection(Selection::caret(0));
        b.apply(
            EditAction::MoveBy { dir: vo_core::edit::Dir::Forward, unit: vo_core::edit::Unit::Line, extend: false },
            &layout,
            0.0,
        );
        let sel = b.selection();
        assert_eq!(sel.focus, layout.lines[0].bytes.end);
        assert_eq!(layout.line_of(sel.focus, sel.affinity), 0);
    }
}
