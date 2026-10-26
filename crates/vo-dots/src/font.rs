//! Dot fonts: text as lit cells of a dot grid (TY16).
//!
//! Two fonts from the public-domain X11 misc-fixed set: [`DotFont::SMALL`] draws capitals 5
//! dots wide and 7 tall (the 6x10 cell, with descenders), [`DotFont::LARGE`] 7 by 9 (the 8x13
//! cell). Both are monospaced. A string becomes a list of cells; a [`crate::field::DotField`]
//! gives every cell a dot of its own, so letters can assemble, dissolve and morph.

use crate::font_data::{FONT_5X7, FONT_7X9};

/// Glyph bitmaps as generated into `font_data.rs`.
pub(crate) struct FontData {
    pub cell_w: u8,
    pub cell_h: u8,
    /// Rows from the top of the cell to the baseline.
    pub ascent: u8,
    pub rows: &'static [u8],
}

/// A monospaced dot font.
#[derive(Clone, Copy)]
pub struct DotFont {
    data: &'static FontData,
}

impl PartialEq for DotFont {
    fn eq(&self, other: &DotFont) -> bool {
        std::ptr::eq(self.data, other.data)
    }
}

impl std::fmt::Debug for DotFont {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DotFont({}x{})", self.data.cell_w, self.data.cell_h)
    }
}

/// One lit cell of a laid-out string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Column from the left of the string, and row from the top of the line box.
    pub x: i32,
    pub y: i32,
    /// Index of the character in the string, counted in `char`s.
    pub char: u32,
}

impl DotFont {
    /// Capitals 5 dots wide and 7 tall, in a 6 by 10 cell.
    pub const SMALL: DotFont = DotFont { data: &FONT_5X7 };
    /// Capitals 7 dots wide and 9 tall, in an 8 by 13 cell.
    pub const LARGE: DotFont = DotFont { data: &FONT_7X9 };

    /// Columns one character takes, its blank spacing column included.
    pub fn advance(&self) -> i32 {
        self.data.cell_w as i32
    }

    /// Rows of the line box: ascent plus descent.
    pub fn line_height(&self) -> i32 {
        self.data.cell_h as i32
    }

    /// Rows from the top of the line box to the baseline.
    pub fn ascent(&self) -> i32 {
        self.data.ascent as i32
    }

    /// The rows of `c`, top first, leftmost column in the top bit. Characters outside
    /// U+0020 to U+007E and U+00A0 to U+00FF draw as `?`.
    pub fn glyph(&self, c: char) -> &'static [u8] {
        let index = match c as u32 {
            v @ 0x20..=0x7e => v - 0x20,
            v @ 0xa0..=0xff => v - 0xa0 + 95,
            _ => '?' as u32 - 0x20,
        } as usize;
        let h = self.data.cell_h as usize;
        &self.data.rows[index * h..(index + 1) * h]
    }

    /// Width of `text` in columns, with `tracking` extra columns after every character but
    /// the last. The trailing spacing column of the last character is not counted.
    pub fn width(&self, text: &str, tracking: i32) -> i32 {
        let n = text.chars().count() as i32;
        if n == 0 {
            return 0;
        }
        n * self.advance() + (n - 1) * tracking - 1
    }

    /// The lit cells of `text` on one line, left to right, then top to bottom within each
    /// character.
    pub fn cells(&self, text: &str, tracking: i32) -> Vec<Cell> {
        let mut out = Vec::new();
        let step = self.advance() + tracking;
        for (i, c) in text.chars().enumerate() {
            let x0 = i as i32 * step;
            let rows = self.glyph(c);
            for col in 0..self.advance() {
                for (y, bits) in rows.iter().enumerate() {
                    if bits & (0x80 >> col) != 0 {
                        out.push(Cell { x: x0 + col, y: y as i32, char: i as u32 });
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(font: DotFont, text: &str) -> Vec<String> {
        let cells = font.cells(text, 0);
        let w = font.width(text, 0) as usize + 1;
        let mut rows = vec![vec!['.'; w]; font.line_height() as usize];
        for c in cells {
            rows[c.y as usize][c.x as usize] = '#';
        }
        rows.into_iter().map(|r| r.into_iter().collect()).collect()
    }

    #[test]
    fn small_a_is_five_by_seven() {
        let a = picture(DotFont::SMALL, "A");
        assert_eq!(
            a,
            ["......", "..#...", ".#.#..", "#...#.", "#...#.", "#####.", "#...#.", "#...#.", "......", "......"]
        );
        assert_eq!(DotFont::SMALL.ascent(), 8);
    }

    #[test]
    fn large_capitals_are_seven_by_nine() {
        let rows = picture(DotFont::LARGE, "M");
        let lit: Vec<usize> = rows.iter().enumerate().filter(|(_, r)| r.contains('#')).map(|(i, _)| i).collect();
        assert_eq!(lit.len(), 9, "{rows:#?}");
        let cols = (0..8).filter(|&x| rows.iter().any(|r| r.as_bytes()[x] == b'#')).count();
        assert_eq!(cols, 7, "{rows:#?}");
    }

    #[test]
    fn norwegian_letters_and_fallback() {
        for c in ['æ', 'ø', 'å', 'Æ', 'Ø', 'Å', '·'] {
            assert!(DotFont::SMALL.glyph(c).iter().any(|r| *r != 0), "{c}");
        }
        assert_eq!(DotFont::SMALL.glyph('\u{2026}'), DotFont::SMALL.glyph('?'));
    }

    #[test]
    fn tracking_moves_characters_apart() {
        let f = DotFont::SMALL;
        assert_eq!(f.width("AB", 0), 11);
        assert_eq!(f.width("AB", 2), 13);
        let b0 = f.cells("AB", 0).iter().filter(|c| c.char == 1).map(|c| c.x).min();
        let b2 = f.cells("AB", 2).iter().filter(|c| c.char == 1).map(|c| c.x).min();
        assert_eq!(b2.unwrap() - b0.unwrap(), 2);
    }
}
