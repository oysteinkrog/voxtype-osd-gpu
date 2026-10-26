//! Dot-matrix text (TY16): a string drawn as dots of a [`DotField`], which assembles,
//! dissolves and morphs into the next string.
//!
//! [`DotText`] owns a range of dot ids in a field, so several texts, a waveform and
//! particles can share one field and one element. Each [`DotText::set`] diffs the new string
//! against the old one character by character: unchanged characters keep their dots and do
//! not move. Changed characters go one of three ways ([`Change`]):
//!
//! - `Morph`: the old dots travel to the new letters' cells. Extra dots split off the nearest
//!   old dot; dots left over merge into the nearest new cell and fade. Letters start left to
//!   right, `stagger` apart.
//! - `Dissolve`: the old letters crumble (each dot drifts a little and fades), and the new ones
//!   assemble from dots scattered around their cells.
//! - `Cut`: the new letters are there at once.

use vo_core::hash::{rand_f32, stagger_fit};
use vo_scene::geom::Color;

use crate::field::{DotField, Look};
use crate::font::{Cell, DotFont};

/// How changed characters change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    Morph,
    Dissolve,
    Cut,
}

/// Look and timing of a dot text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub font: DotFont,
    /// Extra columns between characters.
    pub tracking: i32,
    /// Dot diameter as a fraction of the pitch.
    pub size: f32,
    pub color: Color,
    /// Seconds between one changed letter and the next.
    pub stagger: f64,
    /// The last changed letter starts no later than this after the change.
    pub stagger_cap: f64,
}

impl TextStyle {
    pub fn new(font: DotFont, color: Color) -> TextStyle {
        TextStyle { font, tracking: 1, size: 0.62, color, stagger: 0.012, stagger_cap: 0.18 }
    }
}

/// One dot of the text: its id and the cell it sits in, relative to the text's origin.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Lit {
    id: u32,
    cell: Cell,
}

/// A string of dot letters in a [`DotField`]. See the module notes.
#[derive(Clone, Debug)]
pub struct DotText {
    style: TextStyle,
    /// Cell of the text's top-left corner in the field.
    origin: (f32, f32),
    text: Vec<char>,
    lit: Vec<Lit>,
    /// Ids are `base + n` for a counter `n` below `span`.
    base: u32,
    span: u32,
    next: u32,
    seed: u32,
}

impl DotText {
    /// An empty text that uses ids `base..base + span` of its field, with its top-left cell
    /// at `origin`.
    pub fn new(style: TextStyle, origin: (f32, f32), base: u32, span: u32) -> DotText {
        DotText { style, origin, text: Vec::new(), lit: Vec::new(), base, span, next: 0, seed: base }
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn style(&self) -> &TextStyle {
        &self.style
    }

    /// Width in cells of the current text.
    pub fn width(&self) -> i32 {
        self.style.font.width(&self.text(), self.style.tracking)
    }

    /// Ids of the dots that draw the current text.
    pub fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.lit.iter().map(|l| l.id)
    }

    fn new_id(&mut self) -> u32 {
        let id = self.base + self.next % self.span;
        self.next += 1;
        id
    }

    fn look(&self, c: Cell) -> Look {
        Look::new(self.origin.0 + c.x as f32, self.origin.1 + c.y as f32, self.style.size, self.style.color)
    }

    /// Changes the colour of every dot to `color`, from `t`, with the letters' stagger.
    pub fn recolor(&mut self, field: &mut DotField, t: f64, color: Color) {
        self.style.color = color;
        let n = self.text.len().max(1);
        for l in &self.lit {
            let delay = stagger_fit(l.cell.char as usize, n, self.style.stagger, self.style.stagger_cap);
            field.retarget(t, l.id, self.look(l.cell), delay, None);
        }
    }

    /// Shows `text` from `t`, changing the characters that differ by `change`.
    pub fn set(&mut self, field: &mut DotField, t: f64, text: &str, change: Change) {
        let new: Vec<char> = text.chars().collect();
        if new == self.text {
            return;
        }
        // Old and new characters at each index; only differing indices change.
        let old = std::mem::take(&mut self.text);
        let differs = |i: usize| old.get(i) != new.get(i);
        let n = old.len().max(new.len());
        let changed: Vec<usize> = (0..n).filter(|&i| differs(i)).collect();
        let order = |i: u32| changed.iter().position(|&c| c == i as usize).unwrap_or(0);
        let (gap, cap) = (self.style.stagger, self.style.stagger_cap);
        let delay = |i: u32| stagger_fit(order(i), changed.len(), gap, cap);

        let cells = self.style.font.cells(text, self.style.tracking);
        let (keep, gone): (Vec<Lit>, Vec<Lit>) =
            std::mem::take(&mut self.lit).into_iter().partition(|l| !differs(l.cell.char as usize));
        let mut arriving: Vec<Cell> = cells.iter().copied().filter(|c| differs(c.char as usize)).collect();
        self.lit = keep;
        self.seed = self.seed.wrapping_add(1);
        let seed = self.seed;

        match change {
            Change::Cut => {
                for l in &gone {
                    field.remove(l.id);
                }
                for c in arriving {
                    let id = self.new_id();
                    field.place(t, id, self.look(c));
                    self.lit.push(Lit { id, cell: c });
                }
            }
            Change::Dissolve => {
                for (k, l) in gone.iter().enumerate() {
                    let r = |j| rand_f32(seed, l.id, k as u32, j) - 0.5;
                    let at = self.look(l.cell);
                    let drift = at.at(at.x + 1.2 * r(0), at.y + 1.6 * r(1) + 0.6).hidden();
                    field.retarget(t, l.id, drift, delay(l.cell.char) + 0.03 * (r(2) + 0.5) as f64, None);
                }
                for c in arriving {
                    let id = self.new_id();
                    let r = |j| rand_f32(seed, id, 7, j) - 0.5;
                    let to = self.look(c);
                    let from = to.at(to.x + 5.0 * r(0), to.y + 4.0 * r(1)).hidden();
                    field.retarget(t, id, to, 0.06 + delay(c.char) + 0.04 * (r(2) + 0.5) as f64, Some(from));
                    self.lit.push(Lit { id, cell: c });
                }
            }
            Change::Morph => {
                // Pair old dots and new cells in reading order by column, so dots travel
                // mostly sideways and never cross far.
                let key = |c: &Cell| (c.x, c.y);
                let mut old = gone;
                old.sort_by_key(|l| key(&l.cell));
                arriving.sort_by_key(key);
                let (no, nn) = (old.len(), arriving.len());
                let mut used = vec![false; no];
                for (j, c) in arriving.iter().enumerate() {
                    let to = self.look(*c);
                    let d = delay(c.char);
                    if no == 0 {
                        let id = self.new_id();
                        field.retarget(t, id, to, d, Some(to.at(to.x, to.y + 2.0).hidden()));
                        self.lit.push(Lit { id, cell: *c });
                        continue;
                    }
                    let i = (j * no / nn.max(1)).min(no - 1);
                    if !used[i] {
                        used[i] = true;
                        field.retarget(t, old[i].id, to, d, None);
                        self.lit.push(Lit { id: old[i].id, cell: *c });
                    } else {
                        // Split: a new dot starts where the old one is now.
                        let id = self.new_id();
                        let from = field.look(t, old[i].id).unwrap_or(to);
                        field.retarget(t, id, to, d, Some(from));
                        self.lit.push(Lit { id, cell: *c });
                    }
                }
                // Left over: merge into the cell a neighbour went to, and fade.
                for (i, l) in old.iter().enumerate() {
                    if used[i] {
                        continue;
                    }
                    let to = if nn == 0 {
                        let at = self.look(l.cell);
                        at.at(at.x, at.y + 2.0).hidden()
                    } else {
                        let j = (i * nn / no).min(nn - 1);
                        let at = self.look(arriving[j]);
                        Look { bright: 0.0, ..at }
                    };
                    field.retarget(t, l.id, to, delay(l.cell.char), None);
                }
            }
        }
        self.text = new;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::Springs;

    fn setup(text: &str) -> (DotField, DotText) {
        let mut f = DotField::new(0.0, Springs::default());
        let mut t = DotText::new(TextStyle::new(DotFont::SMALL, Color::WHITE), (2.0, 1.0), 1000, 10_000);
        t.set(&mut f, 0.0, text, Change::Cut);
        (f, t)
    }

    /// Lit cells at rest, from the field.
    fn picture(f: &DotField, t: f64) -> Vec<(i32, i32)> {
        let mut v = Vec::new();
        f.sample(t, &mut v);
        let mut cells: Vec<(i32, i32)> =
            v.iter().filter(|d| d.color.a > 0.5).map(|d| (d.x.round() as i32, d.y.round() as i32)).collect();
        cells.sort();
        cells.dedup();
        cells
    }

    fn expected(text: &str) -> Vec<(i32, i32)> {
        let mut c: Vec<(i32, i32)> = DotFont::SMALL.cells(text, 1).iter().map(|c| (c.x + 2, c.y + 1)).collect();
        c.sort();
        c
    }

    #[test]
    fn every_change_ends_on_the_new_string() {
        for change in [Change::Morph, Change::Dissolve, Change::Cut] {
            let (mut f, mut t) = setup("Listening");
            t.set(&mut f, 1.0, "Finishing", change);
            let end = f.rest_at().max(1.0);
            assert!(end < 2.0, "{change:?} rests at {end}");
            f.prune(end);
            assert_eq!(picture(&f, end), expected("Finishing"), "{change:?}");
            assert_eq!(f.len(), t.ids().count(), "{change:?}: no stray dots after prune");
        }
    }

    #[test]
    fn unchanged_characters_do_not_move() {
        let (mut f, mut t) = setup("00:27");
        let before: Vec<u32> = t.ids().collect();
        t.set(&mut f, 1.0, "00:28", Change::Morph);
        let kept: Vec<u32> = t.ids().filter(|id| before.contains(id)).collect();
        // The dots of "00:2" are still there and not moving.
        let stay = DotFont::SMALL.cells("00:2", 1).len();
        assert!(kept.len() >= stay);
        for id in &kept[..stay] {
            assert_eq!(f.look(1.05, *id), f.target(*id));
        }
    }

    #[test]
    fn letters_start_left_to_right() {
        let (mut f, mut t) = setup("AAAA");
        t.set(&mut f, 1.0, "BBBB", Change::Dissolve);
        // Shortly after the change the first letter has moved more than the last.
        let moved = |ch: u32| {
            let cells = DotFont::SMALL.cells("BBBB", 1);
            t.lit_for_test(ch)
                .iter()
                .map(|id| {
                    let now = f.look(1.08, *id).unwrap();
                    let to = f.target(*id).unwrap();
                    (now.x - to.x).abs() + (now.y - to.y).abs() + (1.0 - now.bright)
                })
                .sum::<f32>()
                / cells.iter().filter(|c| c.char == ch).count() as f32
        };
        assert!(moved(0) < moved(3), "{} {}", moved(0), moved(3));
    }

    impl DotText {
        fn lit_for_test(&self, ch: u32) -> Vec<u32> {
            self.lit.iter().filter(|l| l.cell.char == ch).map(|l| l.id).collect()
        }
    }
}
