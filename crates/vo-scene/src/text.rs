//! Text measurement and glyph placement, behind a trait.
//!
//! Layout asks a `TextLayout` how big a label is at a given width, and the display list
//! takes the placed glyphs from the same call. `EstimateText` is the stand-in until the
//! harfrust line fitter (plan decision 8) plugs in here: it uses a fixed advance per
//! character, and its glyph ids are the characters' code points, not real glyph ids.

use crate::element::TextStyle;
use crate::geom::{Point, Size};

/// One placed glyph. `pos` is the pen position on the baseline, relative to the text box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedGlyph {
    pub glyph: u32,
    pub pos: Point,
    /// Byte offset of the glyph's cluster in the source text.
    pub cluster: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShapedText {
    pub size: Size,
    pub glyphs: Vec<PlacedGlyph>,
}

pub trait TextLayout {
    /// Breaks `text` into lines no wider than `max_width` (one line for `None`) and places
    /// its glyphs. A word longer than the width gets a line of its own.
    fn layout(&self, text: &str, style: &TextStyle, max_width: Option<f32>) -> ShapedText;
}

/// Logical units a line may overshoot its width and still count as fitting.
const FIT_TOLERANCE: f32 = 1e-3;

/// Fixed advances: `advance` ems per character and `space` ems per space. Breaks only at
/// spaces.
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
        let lines = if any { line + 1 } else { 0 };
        ShapedText { size: Size::new(widest, lines as f32 * style.line_height), glyphs }
    }
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
        let style = TextStyle { font: 0, size: 10.0, line_height: 12.0 };
        let t = EstimateText::default();
        let one = t.layout("ab cd", &style, None);
        assert_eq!(one.size, Size::new(5.5 * 4.0 + 3.0, 12.0));
        let two = t.layout("ab cd", &style, Some(20.0));
        assert_eq!(two.size, Size::new(11.0, 24.0));
        assert_eq!(two.glyphs[2].pos, Point::new(0.0, 12.0 + 8.0));
        assert_eq!(two.glyphs[2].cluster, 3);
        assert_eq!(t.layout("", &style, None).size, Size::ZERO);
    }
}
