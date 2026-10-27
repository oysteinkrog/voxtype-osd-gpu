//! Inline objects in live text (FX-063): marks that take room in the line and wrap like
//! words.
//!
//! An inline object is a word in the live text's word list whose text is
//! [`OBJECT_TEXT`] (U+FFFC, the object replacement character). The element maps its id to
//! an [`Inline`]: a width and a mark. Layout gives the word exactly that width, so it wraps
//! with the words around it, and the flow springs it like any word: when its width changes
//! (a pause that grows), its box springs to the new width and the words after it slide.
//! It draws no glyph; its mark draws in the word's sprung box.
//!
//! - **Pause marker.** Dots, more of them the longer the pause (`Inline::pause`).
//! - **Gap slot.** A mini waveform of the speech that has no words yet (`Inline::gap`).
//! - **Brace.** A bracket before and after a phrase being rewritten (`Inline::brace_open`,
//!   `Inline::brace_close`).
//! - **Chip.** A rounded box around the words between a chip open and a chip close, one
//!   piece per line, such as an "end sentence?" chip (`Inline::chip_open`).
//! - **Dash** and **blank**: a rule, or plain room.
//!
//! Positions are in the text box's space. A word box from `Flow::word_bounds` has its top
//! at 0.78 em above the baseline and is one em tall.

use vo_core::words::{Word, WordId};

use crate::display::{Paint, ShapeInstance};
use crate::geom::{Color, Point, RRect, Rect};

/// The character an object word holds.
pub const OBJECT_CHAR: char = '\u{FFFC}';
/// The text of an object word.
pub const OBJECT_TEXT: &str = "\u{FFFC}";
/// `PlacedGlyph::glyph` of an object's placeholder: it takes room and is never drawn.
pub const OBJECT_GLYPH: u32 = u32::MAX - 1;

/// Pause width: this many ems at no length, plus `PAUSE_EMS_PER_SEC` per second, up to
/// `PAUSE_MAX_SECS`.
pub const PAUSE_MIN_EMS: f32 = 0.45;
pub const PAUSE_EMS_PER_SEC: f32 = 0.6;
pub const PAUSE_MAX_SECS: f32 = 3.0;
/// Gap slot width per second of wordless speech, in ems, and its least width.
pub const GAP_EMS_PER_SEC: f32 = 1.2;
pub const GAP_MIN_EMS: f32 = 0.8;
/// Distance between pause dots, in ems.
const DOT_STEP: f32 = 0.3;
/// Where marks sit: the middle of the x-height above the baseline, in ems.
const MID: f32 = 0.27;
/// An object word that has no `Inline` takes this room.
const DEFAULT_EMS: f32 = 0.5;

/// What an inline object draws in its box.
#[derive(Clone, Debug, PartialEq)]
pub enum InlineMark {
    /// Room only.
    Blank,
    /// Dots spread over the width; a wider pause shows more.
    Pause,
    /// A short rule.
    Dash,
    /// Bars of a mini waveform, levels 0 to 1, spread over the width.
    Wave(Vec<f32>),
    /// The left and right bracket of a phrase.
    BraceOpen,
    BraceClose,
    /// The two ends of a chip: a box drawn around the words between them.
    ChipOpen,
    ChipClose,
}

/// An inline object: its width in ems of the text size, its mark and its colour.
#[derive(Clone, Debug, PartialEq)]
pub struct Inline {
    pub width: f32,
    pub mark: InlineMark,
    /// `None` draws in the text colour at 60%.
    pub color: Option<Color>,
}

impl Inline {
    pub fn new(width: f32, mark: InlineMark) -> Inline {
        Inline { width, mark, color: None }
    }

    /// A pause marker for a pause of `secs`: wider and with more dots the longer it is.
    pub fn pause(secs: f32) -> Inline {
        Inline::new(PAUSE_MIN_EMS + PAUSE_EMS_PER_SEC * secs.clamp(0.0, PAUSE_MAX_SECS), InlineMark::Pause)
    }

    /// A slot for `secs` of speech that has no words yet, with a mini waveform of `levels`.
    pub fn gap(secs: f32, levels: Vec<f32>) -> Inline {
        Inline::new((GAP_EMS_PER_SEC * secs.max(0.0)).max(GAP_MIN_EMS), InlineMark::Wave(levels))
    }

    pub fn brace_open() -> Inline {
        Inline::new(0.45, InlineMark::BraceOpen)
    }

    pub fn brace_close() -> Inline {
        Inline::new(0.45, InlineMark::BraceClose)
    }

    /// The left end of a chip; its width is the chip's inner padding.
    pub fn chip_open() -> Inline {
        Inline::new(0.2, InlineMark::ChipOpen)
    }

    pub fn chip_close() -> Inline {
        Inline::new(0.2, InlineMark::ChipClose)
    }

    pub fn dash(width: f32) -> Inline {
        Inline::new(width, InlineMark::Dash)
    }

    pub fn color(self, c: Color) -> Inline {
        Inline { color: Some(c), ..self }
    }

    /// The word that holds this object's place in the word list.
    pub fn word(id: WordId) -> Word {
        Word { id, text: OBJECT_TEXT.to_string() }
    }
}

/// Whether a word is an inline object's placeholder.
pub fn is_object(word: &Word) -> bool {
    word.text == OBJECT_TEXT
}

/// The width of each object word in `words`, in order, in logical units at em size `size`.
pub(crate) fn widths(words: &[Word], inline: &[(WordId, Inline)], size: f32) -> Vec<f32> {
    words
        .iter()
        .filter(|w| is_object(w))
        .map(|w| size * inline.iter().find(|(id, _)| *id == w.id).map_or(DEFAULT_EMS, |(_, o)| o.width.max(0.0)))
        .collect()
}

/// Draws the marks of every object in `words`. `place` gives a word's box and opacity at
/// the frame being drawn, already in the space the shapes go to; `None` for a word not on
/// screen. `ink` is the text colour.
pub(crate) fn draw(
    words: &[Word],
    inline: &[(WordId, Inline)],
    size: f32,
    ink: Color,
    place: impl Fn(WordId) -> Option<(Rect, f32)>,
    out: &mut Vec<ShapeInstance>,
) {
    let look = |id: WordId| inline.iter().find(|(i, _)| *i == id).map(|(_, o)| o);
    let mut chip: Option<(Color, Vec<(Rect, f32)>)> = None;
    for w in words {
        let obj = if is_object(w) { look(w.id) } else { None };
        let Some((b, k)) = place(w.id) else { continue };
        if let Some((_, boxes)) = &mut chip {
            boxes.push((b, k));
        }
        let Some(obj) = obj else { continue };
        let color = obj.color.unwrap_or(ink.fade(0.6)).fade(k);
        match obj.mark {
            InlineMark::ChipOpen => chip = Some((color, vec![(b, k)])),
            InlineMark::ChipClose => {
                if let Some((c, boxes)) = chip.take() {
                    chip_shapes(&boxes, size, c, out);
                }
            }
            _ => mark_shapes(&obj.mark, b, size, color, out),
        }
    }
    // A chip still open at the end of the text runs to its last word.
    if let Some((c, boxes)) = chip {
        chip_shapes(&boxes, size, c, out);
    }
}

/// The shapes of one object's own mark in its box `b`.
pub fn mark_shapes(mark: &InlineMark, b: Rect, size: f32, color: Color, out: &mut Vec<ShapeInstance>) {
    if color.a <= 0.0 || b.w <= 0.0 {
        return;
    }
    let baseline = b.y + 0.78 * size;
    let mid = baseline - MID * size;
    let paint = Paint::solid(color);
    match mark {
        InlineMark::Blank | InlineMark::ChipOpen | InlineMark::ChipClose => {}
        InlineMark::Pause => {
            let r = 0.065 * size;
            let step = DOT_STEP * size;
            let room = b.w - 0.15 * size;
            let n = ((room / step).floor() as i32).max(1);
            let span = (n - 1) as f32 * step;
            let x0 = b.x + 0.5 * (b.w - span);
            for i in 0..n {
                // A dot whose room is still opening fades in with it.
                let k = ((room - (i as f32) * step) / step).clamp(0.0, 1.0);
                let k = if n == 1 { 1.0 } else { k };
                out.push(ShapeInstance::circle(Point::new(x0 + i as f32 * step, mid), r, Paint::solid(color.fade(k))));
            }
        }
        InlineMark::Dash => {
            let pad = (0.15 * size).min(0.25 * b.w);
            let h = (0.08 * size).max(1.0);
            out.push(ShapeInstance::capsule(Rect::new(b.x + pad, mid - 0.5 * h, b.w - 2.0 * pad, h), paint));
        }
        InlineMark::Wave(levels) => {
            let pad = 0.1 * size;
            let inner = b.w - 2.0 * pad;
            if inner <= 0.0 {
                return;
            }
            let n = levels.len().max(1);
            let pitch = inner / n as f32;
            let bar = (0.55 * pitch).max(1.0);
            let floor = 0.06 * size;
            for i in 0..n {
                let v = levels.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                let h = floor + v * (0.62 * size - floor);
                let x = b.x + pad + (i as f32 + 0.5) * pitch - 0.5 * bar;
                out.push(ShapeInstance::capsule(Rect::new(x, mid - 0.5 * h, bar, h), paint));
            }
        }
        InlineMark::BraceOpen | InlineMark::BraceClose => {
            let open = matches!(mark, InlineMark::BraceOpen);
            let stroke = (0.07 * size).max(1.0);
            let (top, bottom) = (baseline - 0.82 * size, baseline + 0.2 * size);
            let arm = 0.18 * size;
            let x = if open { b.x + 0.55 * b.w - 0.5 * arm } else { b.x + 0.45 * b.w + 0.5 * arm };
            let dir = if open { 1.0 } else { -1.0 };
            out.push(ShapeInstance::line(Point::new(x, top), Point::new(x, bottom), stroke, paint));
            out.push(ShapeInstance::line(Point::new(x, top), Point::new(x + dir * arm, top), stroke, paint));
            out.push(ShapeInstance::line(Point::new(x, bottom), Point::new(x + dir * arm, bottom), stroke, paint));
        }
    }
}

/// A chip around word boxes in reading order: one rounded box per line they cover.
pub fn chip_shapes(boxes: &[(Rect, f32)], size: f32, color: Color, out: &mut Vec<ShapeInstance>) {
    for piece in line_pieces(boxes) {
        let r = Rect::new(piece.x, piece.y - 0.08 * size, piece.w, piece.h + 0.22 * size);
        let fill = Paint::solid(color.fade(0.14));
        out.push(ShapeInstance::rrect(RRect::new(r, 0.5 * r.h.min(r.w)), fill).with_stroke(Paint::solid(color), 1.0));
    }
}

/// Word boxes in reading order joined into one box per line: a box starts a new line when
/// it sits below the last one by more than half its height.
pub fn line_pieces(boxes: &[(Rect, f32)]) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    for (b, _) in boxes {
        match out.last_mut() {
            Some(p) if (b.y - p.y).abs() < 0.5 * b.h.max(1.0) => {
                let right = (p.x + p.w).max(b.x + b.w);
                p.x = p.x.min(b.x);
                p.w = right - p.x;
            }
            _ => out.push(*b),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_grows_with_its_length_and_stops_growing() {
        assert!(Inline::pause(1.0).width > Inline::pause(0.2).width);
        assert_eq!(Inline::pause(10.0).width, Inline::pause(PAUSE_MAX_SECS).width);
        assert_eq!(Inline::pause(-1.0).width, PAUSE_MIN_EMS);
    }

    #[test]
    fn widths_follow_object_words_in_order() {
        let words = vec![
            Word { id: 1, text: "hello".into() },
            Inline::word(9),
            Word { id: 2, text: "world".into() },
            Inline::word(8),
        ];
        let inline = vec![(8, Inline::dash(2.0)), (9, Inline::pause(0.0))];
        assert_eq!(widths(&words, &inline, 10.0), vec![10.0 * PAUSE_MIN_EMS, 20.0]);
        // An object with no description takes the default room.
        assert_eq!(widths(&[Inline::word(5)], &[], 10.0), vec![10.0 * DEFAULT_EMS]);
    }

    #[test]
    fn more_dots_for_a_longer_pause() {
        let dots = |secs: f32| {
            let mut out = Vec::new();
            let w = Inline::pause(secs).width * 20.0;
            mark_shapes(&InlineMark::Pause, Rect::new(0.0, 0.0, w, 20.0), 20.0, Color::WHITE, &mut out);
            out.len()
        };
        assert_eq!(dots(0.0), 1);
        assert!(dots(2.0) > dots(0.5), "{} {}", dots(2.0), dots(0.5));
    }

    #[test]
    fn wave_draws_a_bar_per_level_inside_its_box() {
        let mut out = Vec::new();
        let b = Rect::new(10.0, 0.0, 60.0, 20.0);
        mark_shapes(&InlineMark::Wave(vec![0.0, 0.5, 1.0, 0.2]), b, 20.0, Color::WHITE, &mut out);
        assert_eq!(out.len(), 4);
        for s in &out {
            assert!(s.geom[0] >= b.x && s.geom[0] + s.geom[2] <= b.x + b.w);
        }
        // The loudest bar is the tallest.
        assert!(out[2].geom[3] > out[1].geom[3] && out[1].geom[3] > out[0].geom[3]);
    }

    #[test]
    fn chip_draws_one_box_per_line() {
        let boxes = [
            (Rect::new(50.0, 0.0, 10.0, 20.0), 1.0),
            (Rect::new(62.0, 0.0, 30.0, 20.0), 1.0),
            (Rect::new(0.0, 26.0, 40.0, 20.0), 1.0),
            (Rect::new(42.0, 26.0, 5.0, 20.0), 1.0),
        ];
        let pieces = line_pieces(&boxes);
        assert_eq!(pieces, vec![Rect::new(50.0, 0.0, 42.0, 20.0), Rect::new(0.0, 26.0, 47.0, 20.0)]);
        let mut out = Vec::new();
        chip_shapes(&boxes, 20.0, Color::WHITE, &mut out);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn draw_pairs_chip_ends_and_skips_words_off_screen() {
        let words = vec![
            Inline::word(10),
            Word { id: 1, text: "end".into() },
            Word { id: 2, text: "sentence?".into() },
            Inline::word(11),
            Inline::word(12),
        ];
        let inline = vec![(10, Inline::chip_open()), (11, Inline::chip_close()), (12, Inline::dash(1.0))];
        let place = |id: WordId| match id {
            10 => Some((Rect::new(0.0, 0.0, 4.0, 20.0), 1.0)),
            1 => Some((Rect::new(6.0, 0.0, 30.0, 20.0), 1.0)),
            2 => Some((Rect::new(40.0, 0.0, 60.0, 20.0), 1.0)),
            11 => Some((Rect::new(102.0, 0.0, 4.0, 20.0), 1.0)),
            _ => None,
        };
        let mut out = Vec::new();
        draw(&words, &inline, 20.0, Color::WHITE, place, &mut out);
        // One chip box from the open end to the close end; the dash is off screen.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].geom[0], 0.0);
        assert_eq!(out[0].geom[2], 106.0);
    }
}
