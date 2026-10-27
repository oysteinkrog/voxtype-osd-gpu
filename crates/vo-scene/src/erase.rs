//! Cancel erase (T27): a strike line draws through the text, then the words leave in reverse
//! order, last word first.
//!
//! [`Erase`] takes the word boxes in reading order and the time the cancel happened. The
//! strike runs across all the words in `strike` seconds, line after line. Then each word fades
//! and drops a little, `stagger` seconds after the word to its right; the part of the strike
//! over a word fades with it, so the line retracts from the right as the words go.
//!
//! It is a pure function of `t`. [`Erase::word`] gives each word's opacity and offset for
//! whatever draws the text, and [`Erase::paint_strike`] draws the strike through a
//! [`Painter`].

use crate::canvas::Painter;
use crate::geom::Rect;
use crate::path::{make, StrokeStyle};

/// See the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct Erase {
    /// When the cancel happened.
    pub start: f64,
    /// Word boxes in reading order: from the top of the line to the baseline and below, as
    /// the text draws them.
    pub words: Vec<Rect>,
    /// How long the strike takes across all the words.
    pub strike: f32,
    /// Delay between one word leaving and the one before it.
    pub stagger: f32,
    /// How long a word takes to leave.
    pub exit: f32,
    /// How far a leaving word drops, in logical units.
    pub drop: f32,
    /// Where the strike crosses a word, as a fraction of its box height from the top.
    pub strike_at: f32,
    pub style: StrokeStyle,
}

impl Default for Erase {
    fn default() -> Erase {
        Erase {
            start: 0.0,
            words: Vec::new(),
            strike: 0.150,
            stagger: 0.025,
            exit: 0.180,
            drop: 4.0,
            strike_at: 0.58,
            style: StrokeStyle::solid(1.5, crate::geom::Color::hex(0xef4444, 1.0)),
        }
    }
}

/// How a word looks at some time while it is erased.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordState {
    pub opacity: f32,
    pub dy: f32,
}

impl Erase {
    /// When word `i` starts to leave: after the strike, last word first.
    pub fn leaves_at(&self, i: usize) -> f64 {
        let from_end = self.words.len().saturating_sub(1 + i);
        self.start + self.strike as f64 + from_end as f64 * self.stagger as f64
    }

    /// When the last word (the first one) has gone.
    pub fn until(&self) -> f64 {
        self.leaves_at(0) + self.exit as f64
    }

    /// Word `i` at `t`.
    pub fn word(&self, i: usize, t: f64) -> WordState {
        let u = ((t - self.leaves_at(i)) / self.exit.max(1e-3) as f64).clamp(0.0, 1.0) as f32;
        // Fade out on ease in, so the word holds a moment and then goes.
        WordState { opacity: 1.0 - u * u, dy: self.drop * u }
    }

    /// Draws the strike at `t`: across the words in order over `strike` seconds, each word's
    /// part fading as the word leaves.
    pub fn paint_strike(&self, t: f64, p: &mut Painter) {
        let total: f32 = self.words.iter().map(|w| w.w).sum();
        if total <= 0.0 {
            return;
        }
        let u = ((t - self.start) / self.strike.max(1e-3) as f64).clamp(0.0, 1.0) as f32;
        let mut reach = total * (1.0 - (1.0 - u).powi(2));
        for (i, w) in self.words.iter().enumerate() {
            if reach <= 0.0 {
                break;
            }
            let len = reach.min(w.w);
            reach -= w.w;
            let s = self.word(i, t);
            if s.opacity <= 0.0 {
                continue;
            }
            // The strike runs on into the space before the next word on the same line, so it
            // reads as one line.
            let gap = match self.words.get(i + 1) {
                Some(n) if (n.y - w.y).abs() < 0.5 * w.h && n.x > w.right() && len >= w.w => n.x - w.right(),
                _ => 0.0,
            };
            let y = w.y + w.h * self.strike_at + s.dy;
            let style =
                StrokeStyle { c0: self.style.c0.fade(s.opacity), c1: self.style.c1.fade(s.opacity), ..self.style };
            p.stroke(&make::underline(w.x, w.x + len + gap, y), &style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn erase() -> Erase {
        let words = (0..5).map(|i| Rect::new(i as f32 * 50.0, 0.0, 40.0, 20.0)).collect();
        Erase { start: 1.0, words, ..Erase::default() }
    }

    #[test]
    fn words_leave_last_first_after_the_strike() {
        let e = erase();
        assert!((e.leaves_at(4) - 1.15).abs() < 1e-6);
        assert!((e.leaves_at(3) - 1.175).abs() < 1e-6);
        assert!((e.leaves_at(0) - 1.25).abs() < 1e-6);
        assert_eq!(e.word(0, 1.2).opacity, 1.0);
        assert!(e.word(4, 1.25).opacity < 1.0);
        assert_eq!(e.word(0, e.until()).opacity, 0.0);
        // At any time a word to the right is at least as far gone as the ones before it.
        for k in 0..60 {
            let t = 1.0 + k as f64 * 0.01;
            for i in 0..4 {
                assert!(e.word(i, t).opacity >= e.word(i + 1, t).opacity, "t={t} i={i}");
            }
        }
    }
}
