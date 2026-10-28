//! What a revision does around the word that changed: width escrow (MO-22), the
//! correction hinge (MO-21) and the revision address (MO-23). Each is opt-in through
//! [`RevisionMotion`] on the live text (`Element::revision_motion`).
//!
//! - **Width escrow (MO-22).** In a burst of revisions the words after a changed word
//!   hold their place while the changed word borrows up to half an em; after
//!   `ESCROW_QUIET` without another change they make one move. A change that wraps a word
//!   to another line, drops or inserts a word before the end, or moves a word by more
//!   than half an em moves everything at once, as without escrow.
//! - **Correction hinge (MO-21).** When one word becomes several, several become one, or
//!   a word break moves ("ice cream" to "I scream"), a thin bracket under the span draws
//!   in (60 ms), its joints move from the old word breaks to the new ones (90 ms), and it
//!   retracts (80 ms). Only the replaced span is bracketed.
//! - **Revision address (MO-23).** When a word on an older line changes, a small mark
//!   lights beside that line and a matching one beside the newest line (70 ms); the two
//!   then contract toward each other and go (170 ms). Changes on one line in one update
//!   share one pair of marks.
//!
//! Marks are drawn in the text colour. Under reduced motion the hinge shows its joints at
//! the new breaks and the address marks stay where they lit, each for the same time,
//! then they go; escrow still holds the words and then moves them in one jump.

use super::*;

/// What a revision does around the changed word. All off by default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RevisionMotion {
    /// MO-22: neighbours hold while a burst of revisions goes on.
    pub escrow: bool,
    /// MO-21: a bracket with moving joints when word breaks change.
    pub hinge: bool,
    /// MO-23: paired margin marks when an older line changes.
    pub address: bool,
}

impl RevisionMotion {
    /// All three on.
    pub const ALL: RevisionMotion = RevisionMotion { escrow: true, hinge: true, address: true };
}

/// Neighbours move this long after the last revision of a burst.
pub const ESCROW_QUIET: f64 = 0.100;
/// The most a changed word may borrow before its neighbours move at once, in ems.
pub const ESCROW_EMS: f32 = 0.5;
/// Hinge phases: the bracket draws in, the joints move, the bracket retracts (seconds).
pub const HINGE_IN: f64 = 0.060;
pub const HINGE_MOVE: f64 = 0.090;
pub const HINGE_OUT: f64 = 0.080;
/// Address phases: the marks light, then contract together and go.
pub const ADDRESS_IN: f64 = 0.070;
pub const ADDRESS_MEET: f64 = 0.170;

/// The bracket and joints sit this far below the baseline, in ems: under the descenders.
const HINGE_DROP: f32 = 0.30;
/// Joint and end tick heights, in ems.
const JOINT_EMS: f32 = 0.16;
const END_EMS: f32 = 0.24;
/// Mark opacity against the text.
const MARK_INK: f32 = 0.7;
/// Address marks: gap to the line, width and height in ems.
const ADDRESS_GAP: f32 = 6.0;
const ADDRESS_W: f32 = 2.0;
const ADDRESS_EMS: f32 = 0.6;

/// A filled mark to draw at `t`, in the text box's space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MarkShape {
    pub rect: Rect,
    pub alpha: f32,
}

/// One joint of a hinge: the old break it starts from, if any, and whether it ends at a
/// new break (the new break of the same index, read live). A joint with no start grows
/// at its new break; one with no end shrinks where it was.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Joint {
    from: Option<f32>,
    to: bool,
}

/// A running hinge.
#[derive(Clone, Debug)]
pub(crate) struct Hinge {
    start: f64,
    /// The new words of the span, in reading order; their live bounds place the bracket.
    ids: Vec<WordId>,
    /// The old span's ends, so the bracket covers what was replaced too.
    old_x: (f32, f32),
    joints: Vec<Joint>,
    reduced: bool,
}

/// A running pair of address marks.
#[derive(Clone, Debug)]
pub(crate) struct Address {
    start: f64,
    line: u32,
    reduced: bool,
}

/// Running hinges and addresses of one live text.
#[derive(Default)]
pub(crate) struct Marks {
    hinges: Vec<Hinge>,
    addresses: Vec<Address>,
}

impl Marks {
    /// When the last mark is gone.
    pub fn settled_at(&self) -> f64 {
        let h = self.hinges.iter().map(|h| h.start + HINGE_IN + HINGE_MOVE + HINGE_OUT);
        let a = self.addresses.iter().map(|a| a.start + ADDRESS_IN + ADDRESS_MEET);
        h.chain(a).fold(f64::NEG_INFINITY, f64::max)
    }

    fn prune(&mut self, t: f64) {
        self.hinges.retain(|h| t < h.start + HINGE_IN + HINGE_MOVE + HINGE_OUT);
        self.addresses.retain(|a| t < a.start + ADDRESS_IN + ADDRESS_MEET);
    }
}

/// An old word as it was before the update.
struct Before {
    id: WordId,
    text: String,
    x0: f32,
    x1: f32,
    line: u32,
}

/// Smooth 0 to 1 over `u` in 0 to 1.
fn smooth(u: f64) -> f32 {
    let u = u.clamp(0.0, 1.0);
    (u * u * (3.0 - 2.0 * u)) as f32
}

/// Whether width escrow holds the neighbours for this update: some kept word changed its
/// text, no kept word changes line, words only arrive after the last old word, none
/// leaves, and every changed word's width moves by at most `ESCROW_EMS`.
pub(super) fn escrow_holds(old: &[FlowWord], words: &[Word], placed: &[Option<PlacedWord>], size: f32) -> bool {
    let mut revised = false;
    if old.iter().any(|o| !words.iter().any(|w| w.id == o.id)) {
        return false;
    }
    let mut seen_new = false;
    for (w, p) in words.iter().zip(placed) {
        let Some(p) = p else { return false };
        match old.iter().find(|o| o.id == w.id) {
            None => seen_new = true,
            Some(o) => {
                if seen_new || o.line != p.line {
                    return false;
                }
                if o.text != w.text {
                    revised = true;
                    if (p.width - o.width.target() as f32).abs() > ESCROW_EMS * size {
                        return false;
                    }
                }
            }
        }
    }
    revised
}

/// Drops the moves still to come on `ch` after `t` (from an earlier escrow), keeping the
/// motion that runs at `t`, so a new hold replaces the old one instead of adding to it.
pub(super) fn cancel_pending(ch: &mut Channel, t: f64) {
    let segs = ch.segments();
    if !segs.last().is_some_and(|s| s.t0 > t) {
        return;
    }
    if let Some(now) = segs.iter().rev().find(|s| s.t0 <= t).copied() {
        let spring = ch.spring();
        ch.set_spring(now.spring);
        ch.retarget(t, now.target);
        ch.set_spring(spring);
    }
}

/// `move_to` from `at`, after dropping moves an earlier escrow left for later than `t`.
pub(super) fn move_after(ch: &mut Channel, target: f32, t: f64, at: f64, spring: Spring, reduced: bool) {
    cancel_pending(ch, t);
    move_to(ch, target, at, spring, reduced);
}

impl Flow {
    /// Starts the hinges and addresses this update calls for. `old` is the words before the
    /// update; `placed` the new layout.
    pub(super) fn start_marks(
        &mut self,
        t: f64,
        old: &[FlowWord],
        words: &[Word],
        placed: &[Option<PlacedWord>],
        motion: &LiveMotion,
        reduced: bool,
    ) {
        self.marks.prune(t);
        let rm = motion.revisions;
        if !rm.hinge && !rm.address {
            return;
        }
        let before: Vec<Before> = old
            .iter()
            .map(|o| {
                let x0 = o.x.value(t) as f32;
                Before { id: o.id, text: o.text.clone(), x0, x1: x0 + o.width.value(t) as f32, line: o.line }
            })
            .collect();
        if rm.hinge {
            for h in hinges(&before, words, placed, t, reduced) {
                self.marks.hinges.push(h);
            }
        }
        if rm.address {
            let newest = placed.iter().flatten().map(|p| p.line).max().unwrap_or(0);
            let mut lines: Vec<u32> = Vec::new();
            for (w, p) in words.iter().zip(placed) {
                let (Some(p), Some(b)) = (p, before.iter().find(|b| b.id == w.id)) else { continue };
                if b.text != w.text && p.line < newest && !lines.contains(&p.line) {
                    lines.push(p.line);
                }
            }
            for line in lines {
                self.marks.addresses.retain(|a| a.line != line);
                self.marks.addresses.push(Address { start: t, line, reduced });
            }
        }
    }

    /// The hinge and address marks at `t`, in the text box's space.
    pub(crate) fn marks(&self, t: f64) -> Vec<MarkShape> {
        let mut out = Vec::new();
        let size = self.size;
        for h in &self.marks.hinges {
            let u = t - h.start;
            if !(0.0..HINGE_IN + HINGE_MOVE + HINGE_OUT).contains(&u) {
                continue;
            }
            let bounds: Vec<Rect> = h.ids.iter().filter_map(|id| self.word_bounds(*id, t, size)).collect();
            let (Some(first), Some(last)) = (bounds.first(), bounds.last()) else { continue };
            let x0 = first.x.min(h.old_x.0);
            let x1 = (last.x + last.w).max(h.old_x.1);
            // word_bounds' box starts 0.78 em above the baseline.
            let y = first.y + 0.78 * size + HINGE_DROP * size;
            let mid = 0.5 * (x0 + x1);
            // Draw in from the middle, hold, retract to the middle.
            let open = if h.reduced {
                1.0
            } else if u < HINGE_IN {
                smooth(u / HINGE_IN)
            } else if u < HINGE_IN + HINGE_MOVE {
                1.0
            } else {
                1.0 - smooth((u - HINGE_IN - HINGE_MOVE) / HINGE_OUT)
            };
            let alpha = MARK_INK * if h.reduced && u > HINGE_IN + HINGE_MOVE { 0.0 } else { 1.0 };
            if open <= 0.0 || alpha <= 0.0 {
                continue;
            }
            let (l, r) = (mid - 0.5 * (x1 - x0) * open, mid + 0.5 * (x1 - x0) * open);
            out.push(MarkShape { rect: Rect::new(l, y, r - l, 1.0), alpha });
            let end = END_EMS * size * open;
            out.push(MarkShape { rect: Rect::new(l, y - end, 1.0, end), alpha });
            out.push(MarkShape { rect: Rect::new(r - 1.0, y - end, 1.0, end), alpha });
            // Joints: at the old breaks, moving to the new ones.
            let k = if h.reduced { 1.0 } else { smooth((u - HINGE_IN) / HINGE_MOVE) };
            let breaks: Vec<f32> = bounds.windows(2).map(|w| 0.5 * (w[0].x + w[0].w + w[1].x)).collect();
            for (i, j) in h.joints.iter().enumerate() {
                let to = j.to.then(|| breaks.get(i).copied().unwrap_or(mid));
                let (x, grow) = match (j.from, to) {
                    (Some(a), Some(b)) => (a + (b - a) * k, 1.0),
                    (Some(a), None) => (a, 1.0 - k),
                    (None, Some(b)) => (b, k),
                    (None, None) => continue,
                };
                let hgt = JOINT_EMS * size * grow * open;
                if hgt > 0.05 && x > l && x < r {
                    out.push(MarkShape { rect: Rect::new(x - 0.5, y - hgt, 1.0, hgt), alpha });
                }
            }
        }
        for a in &self.marks.addresses {
            let u = t - a.start;
            if !(0.0..ADDRESS_IN + ADDRESS_MEET).contains(&u) {
                continue;
            }
            let (Some(sp), Some(newest)) = (self.spans.get(a.line as usize), self.spans.last()) else { continue };
            let roll = self.words.last().map_or(0.0, |w| w.roll.value(t) as f32);
            let centre = |baseline: f32| baseline - 0.28 * size + roll;
            let (ya, yb) = (centre(sp.2), centre(newest.2));
            let x = sp.0.min(newest.0) - ADDRESS_GAP - ADDRESS_W;
            let (k, alpha) = if u < ADDRESS_IN {
                (0.0, smooth(u / ADDRESS_IN))
            } else if a.reduced {
                (0.0, 1.0)
            } else {
                let m = smooth((u - ADDRESS_IN) / ADDRESS_MEET);
                (m, 1.0 - m * m)
            };
            let mid = 0.5 * (ya + yb);
            let h = ADDRESS_EMS * size * (1.0 - 0.6 * k);
            for y in [ya + (mid - ya) * k, yb + (mid - yb) * k] {
                out.push(MarkShape { rect: Rect::new(x, y - 0.5 * h, ADDRESS_W, h), alpha: MARK_INK * alpha });
            }
        }
        out
    }
}

/// The hinges an update calls for: spans between unchanged words where words were
/// removed or inserted, with more than one word on one side, all on one line.
fn hinges(before: &[Before], words: &[Word], placed: &[Option<PlacedWord>], t: f64, reduced: bool) -> Vec<Hinge> {
    let anchor = |id: WordId, text: &str| before.iter().any(|b| b.id == id && b.text == text);
    let mut out = Vec::new();
    // Walk the new words; each run of non-anchors with the old words between the same
    // anchors is one span.
    let mut j = 0;
    let mut prev_anchor: Option<WordId> = None;
    while j <= words.len() {
        let is_anchor = |j: usize| words.get(j).is_some_and(|w| anchor(w.id, &w.text));
        if j < words.len() && is_anchor(j) {
            prev_anchor = Some(words[j].id);
            j += 1;
            continue;
        }
        let start = j;
        while j < words.len() && !is_anchor(j) {
            j += 1;
        }
        let next_anchor = words.get(j).map(|w| w.id);
        let new_ids: Vec<WordId> = words[start..j].iter().map(|w| w.id).collect();
        // Old words strictly between the two anchors.
        let from = prev_anchor.and_then(|a| before.iter().position(|b| b.id == a)).map_or(0, |i| i + 1);
        let to = next_anchor.and_then(|a| before.iter().position(|b| b.id == a)).unwrap_or(before.len());
        let old: Vec<&Before> = if from < to { before[from..to].iter().collect() } else { Vec::new() };
        j += 1;
        if let Some(a) = next_anchor {
            prev_anchor = Some(a);
        }
        if new_ids.is_empty() || old.is_empty() || new_ids.len() + old.len() < 3 {
            continue;
        }
        let same_ids = old.len() == new_ids.len() && old.iter().zip(&new_ids).all(|(b, id)| b.id == *id);
        if same_ids {
            // Every word kept its id: plain revisions, no break moved.
            continue;
        }
        let lines: Vec<u32> = words[start..j - 1]
            .iter()
            .zip(&placed[start..j - 1])
            .filter_map(|(_, p)| p.as_ref().map(|p| p.line))
            .chain(old.iter().map(|b| b.line))
            .collect();
        if lines.is_empty() || lines.iter().any(|l| *l != lines[0]) {
            continue;
        }
        let old_breaks: Vec<f32> = old.windows(2).map(|w| 0.5 * (w[0].x1 + w[1].x0)).collect();
        let new_count = new_ids.len() - 1;
        let mut joints = Vec::new();
        for i in 0..old_breaks.len().max(new_count) {
            joints.push(Joint { from: old_breaks.get(i).copied(), to: i < new_count });
        }
        let old_x = (old[0].x0, old[old.len() - 1].x1);
        out.push(Hinge { start: t, ids: new_ids, old_x, joints, reduced });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(id: WordId, text: &str, x0: f32, x1: f32) -> Before {
        Before { id, text: text.into(), x0, x1, line: 0 }
    }

    fn w(id: WordId, text: &str) -> Word {
        Word { id, text: text.into() }
    }

    fn on_line(n: usize) -> Vec<Option<PlacedWord>> {
        (0..n)
            .map(|i| Some(PlacedWord { glyphs: Vec::new(), x: i as f32 * 50.0, y: 20.0, width: 40.0, line: 0 }))
            .collect()
    }

    #[test]
    fn two_words_into_two_with_a_moved_break_make_a_hinge() {
        // "my ice cream" to "my I scream": "ice" goes, "I" is new, "cream" becomes "scream".
        let before = [b(1, "my", 0.0, 20.0), b(2, "ice", 26.0, 50.0), b(3, "cream", 56.0, 100.0)];
        let words = [w(1, "my"), w(4, "I"), w(3, "scream")];
        let h = hinges(&before, &words, &on_line(3), 1.0, false);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].ids, vec![4, 3]);
        assert_eq!(h[0].joints, vec![Joint { from: Some(53.0), to: true }]);
    }

    #[test]
    fn one_word_into_two_and_plain_revisions() {
        let before = [b(1, "into", 0.0, 30.0), b(2, "it", 36.0, 50.0)];
        let split = [w(3, "in"), w(4, "to"), w(2, "it")];
        let h = hinges(&before, &split, &on_line(3), 1.0, false);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].joints, vec![Joint { from: None, to: true }]);
        // A word whose text changed but kept its id is a plain revision: no hinge.
        let revised = [w(1, "onto"), w(2, "it")];
        assert!(hinges(&before, &revised, &on_line(2), 1.0, false).is_empty());
        // Words arriving at the end are not a hinge either.
        let more = [w(1, "into"), w(2, "it"), w(5, "now")];
        assert!(hinges(&before, &more, &on_line(3), 1.0, false).is_empty());
    }
}
