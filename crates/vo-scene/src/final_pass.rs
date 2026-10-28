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
//!
//! The pass sets the type (MO-40). A soft front `front` wide moves with it. Ahead of the
//! front words are spoken ink: `ink` of their opacity and tracked `loose` em wider per glyph.
//! Behind it they are set type. A word whose final text differs changes as the front reaches
//! it, nods `nod` px down and back, and gets a hairline bracket under it; its new
//! punctuation pops in and a new capital grows from 82 % (not on the newest line, where the
//! reading-line envelope allows no scale). Final words with new ids (a split) appear when the
//! word before them is reached, and live words the final text drops (a merge) leave when the
//! pass reaches them. Reduced motion keeps the ink, the swap and the bracket, without the
//! nod, the pop or the growth.

use vo_core::words::Word;

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
    /// Width of the soft front between spoken ink and set type, in layout units.
    pub front: f32,
    /// Opacity of spoken ink ahead of the front, 0 to 1. At least 0.7 keeps the reading-line
    /// envelope; 1 turns spoken ink off.
    pub ink: f32,
    /// Extra tracking of spoken ink, in em per glyph. At most 0.025 keeps the envelope.
    pub loose: f32,
    /// How far a changed word dips when the front reaches it, in layout units.
    pub nod: f32,
    /// The bracket under a changed word. Transparent draws none.
    pub bracket: Color,
    /// Reduced motion for this pass alone, as the scene's own: no nod, pop or growth.
    pub still: bool,
}

impl FinalPass {
    pub fn new(at: f32) -> FinalPass {
        FinalPass {
            at: at.clamp(0.0, 1.0),
            line: Color::rgba(1.0, 1.0, 1.0, 0.85),
            lift: 0.6,
            front: 40.0,
            ink: 0.72,
            loose: 0.025,
            nod: 2.0,
            bracket: Color::rgba(1.0, 1.0, 1.0, 0.45),
            still: false,
        }
    }

    /// Spoken ink ahead of the front: its opacity and extra tracking in em per glyph.
    pub fn spoken(self, ink: f32, loose: f32) -> FinalPass {
        FinalPass { ink: ink.clamp(0.0, 1.0), loose: loose.max(0.0), ..self }
    }

    /// The soft front's width.
    pub fn front(self, w: f32) -> FinalPass {
        FinalPass { front: w.max(1.0), ..self }
    }

    pub fn nod(self, px: f32) -> FinalPass {
        FinalPass { nod: px.clamp(0.0, 3.0), ..self }
    }

    pub fn bracket(self, c: Color) -> FinalPass {
        FinalPass { bracket: c, ..self }
    }

    pub fn still(self, still: bool) -> FinalPass {
        FinalPass { still, ..self }
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

/// The nod lasts this long.
pub const NOD: f64 = 0.220;
/// Punctuation pops in over this long; a capital grows over `GROW`.
pub const POP: f64 = 0.090;
pub const GROW: f64 = 0.160;
/// The bracket fades in over this long.
pub const BRACKET_IN: f64 = 0.120;
/// Spoken ink comes in over this long when a pass starts, so the text does not dim in a step.
pub const SPOKEN_IN: f64 = 0.200;

/// How far down a changed word is `dt` seconds after it changed, 0 to 1 and back.
pub fn nod(dt: f64) -> f32 {
    if !(0.0..NOD).contains(&dt) {
        0.0
    } else {
        (std::f64::consts::PI * dt / NOD).sin() as f32
    }
}

/// 0 to 1 over `secs`, eased out.
pub fn ease_in(dt: f64, secs: f64) -> f32 {
    let u = (dt / secs).clamp(0.0, 1.0);
    (1.0 - (1.0 - u) * (1.0 - u)) as f32
}

/// How far a point at reading position `r` is ahead of the pass at `pos`: 0 behind the soft
/// front, 1 ahead of it, smooth across its `front` width.
pub fn ahead(r: f32, pos: f32, front: f32) -> f32 {
    let u = ((r - pos) / front.max(1.0) + 0.5).clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

/// A word the pass changed: when, and its text before.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Changed {
    pub id: u32,
    pub at: f64,
    pub old: String,
}

/// The words to show: the live words, each taking its final text once `reached` says the
/// pass reached it. Final words with ids the live text lacks appear once the final word
/// before them is reached (or at once when it is the first); live words the final text lacks
/// leave once reached. Also returns the words that took a different text, with their old
/// text (an added word's old text is empty).
pub(crate) fn merge(live: &[Word], finals: &[Word], reached: impl Fn(u32) -> bool) -> (Vec<Word>, Vec<(u32, String)>) {
    let in_finals = |id: u32| finals.iter().any(|f| f.id == id);
    let mut out = Vec::with_capacity(live.len().max(finals.len()));
    let mut changed = Vec::new();
    let mut li = 0;
    let mut prev_reached = true;
    for f in finals {
        match live.iter().position(|w| w.id == f.id) {
            Some(at) => {
                // Live words before this one that the final text drops.
                while li < at {
                    let w = &live[li];
                    if !in_finals(w.id) && !reached(w.id) {
                        out.push(w.clone());
                    }
                    li += 1;
                }
                li = li.max(at + 1);
                let w = &live[at];
                prev_reached = reached(f.id);
                if prev_reached {
                    if f.text != w.text {
                        changed.push((f.id, w.text.clone()));
                    }
                    out.push(f.clone());
                } else {
                    out.push(w.clone());
                }
            }
            None if prev_reached => {
                changed.push((f.id, String::new()));
                out.push(f.clone());
            }
            None => {}
        }
    }
    for w in &live[li.min(live.len())..] {
        if !in_finals(w.id) && !reached(w.id) {
            out.push(w.clone());
        }
    }
    (out, changed)
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

    fn words(w: &[(u32, &str)]) -> Vec<Word> {
        w.iter().map(|&(id, t)| Word { id, text: t.into() }).collect()
    }

    fn texts(w: &[Word]) -> Vec<&str> {
        w.iter().map(|w| w.text.as_str()).collect()
    }

    #[test]
    fn merge_swaps_reached_words_and_handles_splits_and_merges() {
        let live = words(&[(1, "set"), (2, "the"), (3, "pop"), (4, "up"), (5, "swish")]);
        // "pop up" merges into "popup" (4 dropped); "swish" becomes "swiss." and a new word 9.
        let finals = words(&[(1, "Set"), (2, "the"), (3, "popup"), (5, "swiss,"), (9, "so.")]);
        let (out, changed) = merge(&live, &finals, |id| id <= 2);
        assert_eq!(texts(&out), ["Set", "the", "pop", "up", "swish"]);
        assert_eq!(changed, vec![(1, "set".to_string())]);
        let (out, _) = merge(&live, &finals, |id| id <= 4);
        assert_eq!(texts(&out), ["Set", "the", "popup", "swish"], "the dropped word left once reached");
        let (out, changed) = merge(&live, &finals, |_| true);
        assert_eq!(texts(&out), ["Set", "the", "popup", "swiss,", "so."]);
        assert!(changed.contains(&(9, String::new())), "an added word counts as changed");
        let (out, changed) = merge(&live, &finals, |_| false);
        assert_eq!(texts(&out), texts(&live));
        assert!(changed.is_empty());
    }

    #[test]
    fn spoken_ink_is_ahead_of_the_soft_front() {
        assert_eq!(ahead(0.0, 100.0, 40.0), 0.0);
        assert_eq!(ahead(200.0, 100.0, 40.0), 1.0);
        assert!((ahead(100.0, 100.0, 40.0) - 0.5).abs() < 1e-6);
        assert_eq!(nod(0.0), 0.0);
        assert!((nod(NOD / 2.0) - 1.0).abs() < 1e-6);
        assert_eq!(nod(NOD), 0.0);
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
