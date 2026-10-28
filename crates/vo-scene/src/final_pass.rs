//! The final-pass sweep (T10): a reading pass across live text, placed by an outside value
//! such as the finishing fraction, not by time.
//!
//! The pass runs in reading order over all lines. A hairline one physical pixel wide marks
//! where it is. Each word brightens briefly when the pass reaches its start, and words the
//! pass reaches together start [`STAGGER`] apart. To make a revision (a casing fix, a comma)
//! play as the line crosses its word, give the live text its final words
//! (`Element::final_words`): each word takes its final text when the pass reaches its start.
//! `Scene::final_pass_reached` answers the same question for an app that holds text back
//! itself.

use crate::geom::Color;

/// The final pass on a live text (`Element::final_pass`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FinalPass {
    /// How far the pass has read, 0 to 1, in reading order over all lines.
    pub at: f32,
    /// The hairline's colour. Transparent draws none.
    pub line: Color,
    /// How far a word's text turns toward white at the top of its pulse, 0 to 1.
    pub lift: f32,
}

impl FinalPass {
    pub fn new(at: f32) -> FinalPass {
        FinalPass { at: at.clamp(0.0, 1.0), line: Color::rgba(1.0, 1.0, 1.0, 0.85), lift: 0.6 }
    }

    pub fn line(self, c: Color) -> FinalPass {
        FinalPass { line: c, ..self }
    }

    pub fn lift(self, k: f32) -> FinalPass {
        FinalPass { lift: k.clamp(0.0, 1.0), ..self }
    }
}

/// Words the pass reaches together start their pulses this far apart.
pub const STAGGER: f64 = 0.040;
/// The pulse rises over this long, then falls by `e` every `DECAY` seconds.
const RISE: f64 = 0.040;
const DECAY: f64 = 0.110;
/// A pulse is over this long after it starts.
pub const PULSE: f64 = RISE + 6.0 * DECAY;

/// How bright a word is `dt` seconds after the pass reached it, 0 to 1.
pub fn pulse(dt: f64) -> f32 {
    if !(0.0..PULSE).contains(&dt) {
        0.0
    } else if dt < RISE {
        (dt / RISE) as f32
    } else {
        (-(dt - RISE) / DECAY).exp() as f32
    }
}

/// One line in reading order: its baseline, its extent, and the reading length of the lines
/// before it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ReadLine {
    pub baseline: f32,
    pub lo: f32,
    pub hi: f32,
    pub start: f32,
}

/// The lines of word boxes `(id, x0, x1, baseline)` given in layout order.
pub(crate) fn read_lines(boxes: &[(u32, f32, f32, f32)]) -> Vec<ReadLine> {
    let mut lines: Vec<ReadLine> = Vec::new();
    for &(_, x0, x1, baseline) in boxes {
        match lines.iter_mut().find(|l| l.baseline == baseline) {
            Some(l) => (l.lo, l.hi) = (l.lo.min(x0), l.hi.max(x1)),
            None => lines.push(ReadLine { baseline, lo: x0, hi: x1, start: 0.0 }),
        }
    }
    let mut start = 0.0;
    for l in &mut lines {
        l.start = start;
        start += l.hi - l.lo;
    }
    lines
}

/// The total reading length.
pub(crate) fn total(lines: &[ReadLine]) -> f32 {
    lines.last().map_or(0.0, |l| l.start + l.hi - l.lo)
}

/// The reading position of `x` on the line at `baseline`.
pub(crate) fn reading_at(lines: &[ReadLine], baseline: f32, x: f32) -> f32 {
    lines.iter().find(|l| l.baseline == baseline).map_or(0.0, |l| l.start + (x - l.lo))
}

/// Where the pass is when it has read `at` (0 to 1): `(baseline, x)`.
pub(crate) fn locate(lines: &[ReadLine], at: f32) -> Option<(f32, f32)> {
    let pos = at.clamp(0.0, 1.0) * total(lines);
    let l = lines.iter().find(|l| pos <= l.start + l.hi - l.lo).or(lines.last())?;
    Some((l.baseline, (l.lo + pos - l.start).min(l.hi)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pulse_rises_then_falls_to_nothing() {
        assert_eq!(pulse(-0.01), 0.0);
        assert!((pulse(RISE) - 1.0).abs() < 1e-6);
        assert!(pulse(0.2) > 0.1 && pulse(0.2) < 0.5);
        assert!(pulse(PULSE - 1e-6) < 0.003);
        assert_eq!(pulse(PULSE), 0.0);
    }

    #[test]
    fn the_pass_reads_line_after_line() {
        let boxes = [(1, 0.0, 33.0, 16.0), (2, 39.0, 72.0, 16.0), (3, 0.0, 55.0, 42.0)];
        let lines = read_lines(&boxes);
        assert_eq!(lines.len(), 2);
        assert_eq!(total(&lines), 127.0);
        assert_eq!(locate(&lines, 0.0), Some((16.0, 0.0)));
        assert_eq!(locate(&lines, 0.5), Some((16.0, 63.5)));
        let (baseline, x) = locate(&lines, 0.8).unwrap();
        assert!(baseline == 42.0 && (x - 29.6).abs() < 1e-3, "{x}");
        assert_eq!(locate(&lines, 1.0), Some((42.0, 55.0)));
        assert_eq!(reading_at(&lines, 42.0, 10.0), 82.0);
    }
}
