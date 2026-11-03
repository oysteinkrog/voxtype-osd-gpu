//! Input methods (IME) for a text box, as `zwp_text_input_v3` frames them (plan stage F7).
//!
//! The app tells the input method the text around the caret ([`TextInputState`]); the input
//! method sends back what to do in batches ([`TextInputEvent`]): text to delete around the
//! caret, text to commit, and a preedit string to show at the caret until it is committed.
//! The preedit never enters the text (plan R55): only a commit does.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::edit::EditAction;

/// The text an input method composes but has not committed yet, shown at the caret.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preedit {
    pub text: String,
    /// The cursor inside `text`, as a byte range (begin equals end for a plain cursor);
    /// `None` hides the cursor while composing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<(usize, usize)>,
}

/// One batch from the input method (a text-input-v3 `done`). Apply in this order: replace
/// any old preedit with nothing, delete `delete_before` bytes before the selection and
/// `delete_after` bytes after it, insert `commit` at the caret (replacing the selection),
/// then show `preedit` at the caret. A batch with no `preedit` ends the composing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextInputEvent {
    /// Host clock, seconds.
    pub t: f64,
    pub delete_before: u32,
    pub delete_after: u32,
    pub commit: Option<String>,
    pub preedit: Option<Preedit>,
}

/// What the app tells the input method while a text box has keyboard focus: the text
/// around the caret and the selection in it, in bytes of `surrounding`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextInputState {
    /// At most [`SURROUNDING_MAX`] bytes, cut on char boundaries around the caret
    /// ([`TextInputState::around`]).
    pub surrounding: String,
    pub cursor: usize,
    pub anchor: usize,
}

/// The most bytes of surrounding text sent: text-input-v3 limits a request to 4000.
pub const SURROUNDING_MAX: usize = 4000;

impl TextInputState {
    /// The state for `text` with the selection from `anchor` to `cursor` (bytes on char
    /// boundaries): the whole text when it fits, else a slice of at most `SURROUNDING_MAX`
    /// bytes centred on the cursor as far as the text allows. An anchor outside the slice
    /// is moved to its nearer end.
    pub fn around(text: &str, anchor: usize, cursor: usize) -> TextInputState {
        if text.len() <= SURROUNDING_MAX {
            return TextInputState { surrounding: text.to_string(), cursor, anchor };
        }
        let mut start = cursor.saturating_sub(SURROUNDING_MAX / 2).min(text.len() - SURROUNDING_MAX);
        let mut end = start + SURROUNDING_MAX;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        TextInputState {
            surrounding: text[start..end].to_string(),
            cursor: cursor - start,
            anchor: anchor.clamp(start, end) - start,
        }
    }
}

impl TextInputEvent {
    /// The edits this batch makes to a text whose selection is `sel` (bytes): one `Select`
    /// over the selection grown by the deletes, when they reach past it, and one `Insert` of
    /// the commit (empty when the batch only deletes). Nothing for a batch that only changes
    /// the preedit. Deletes stop at the text's ends and widen to char boundaries.
    pub fn actions(&self, text: &str, sel: Range<usize>) -> Vec<EditAction> {
        let mut lo = sel.start.saturating_sub(self.delete_before as usize);
        let mut hi = (sel.end + self.delete_after as usize).min(text.len());
        while !text.is_char_boundary(lo) {
            lo -= 1;
        }
        while !text.is_char_boundary(hi) {
            hi += 1;
        }
        let mut out = Vec::new();
        if (lo, hi) != (sel.start, sel.end) {
            out.push(EditAction::Select { anchor: lo, focus: hi });
        }
        match &self.commit {
            Some(c) => out.push(EditAction::Insert(c.clone())),
            None if lo != sel.start || hi != sel.end => out.push(EditAction::Insert(String::new())),
            None => {}
        }
        out
    }
}

/// The preedit a text box shows, from the input method's batches. It never enters the text:
/// a batch replaces it, a commit ends it (unless the same batch starts a new one), and a
/// focus loss or the end of editing drops it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composing {
    preedit: Option<Preedit>,
}

impl Composing {
    /// Takes in batch `ev`: the preedit becomes the batch's, or none. Returns true when the
    /// preedit changed.
    pub fn take(&mut self, ev: &TextInputEvent) -> bool {
        let next = ev.preedit.clone().filter(|p| !p.text.is_empty());
        std::mem::replace(&mut self.preedit, next) != self.preedit
    }

    /// Drops the preedit (focus lost, editing ended). Returns true when there was one.
    pub fn cancel(&mut self) -> bool {
        self.preedit.take().is_some()
    }

    pub fn preedit(&self) -> Option<&Preedit> {
        self.preedit.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_goes_whole() {
        let s = TextInputState::around("plum pear", 5, 9);
        assert_eq!((s.surrounding.as_str(), s.anchor, s.cursor), ("plum pear", 5, 9));
    }

    #[test]
    fn long_text_is_cut_around_the_caret_on_char_boundaries() {
        let text = "é".repeat(3000) + "CARET" + &"ø".repeat(3000);
        let caret = 6000 + 2;
        let s = TextInputState::around(&text, caret, caret);
        assert!(s.surrounding.len() <= SURROUNDING_MAX);
        assert!(s.surrounding.len() > SURROUNDING_MAX - 4);
        assert_eq!(&s.surrounding[s.cursor - 2..s.cursor + 3], "CARET");
        assert_eq!(s.anchor, s.cursor);
        // Near the end: the slice ends at the text's end.
        let end = text.len();
        let s = TextInputState::around(&text, end, end);
        assert!(text.ends_with(&s.surrounding));
        assert_eq!(s.cursor, s.surrounding.len());
    }

    #[test]
    fn a_long_selection_keeps_the_cursor_and_clamps_the_anchor() {
        let text = "a".repeat(9000);
        let s = TextInputState::around(&text, 100, 8900);
        assert_eq!(s.surrounding.len(), SURROUNDING_MAX);
        assert_eq!((s.anchor, s.cursor), (0, 8900 - 5000));
    }

    fn batch(before: u32, after: u32, commit: Option<&str>, preedit: Option<&str>) -> TextInputEvent {
        TextInputEvent {
            t: 0.0,
            delete_before: before,
            delete_after: after,
            commit: commit.map(str::to_owned),
            preedit: preedit.map(|p| Preedit { text: p.into(), cursor: Some((p.len(), p.len())) }),
        }
    }

    #[test]
    fn a_preedit_is_replaced_then_committed_and_never_enters_the_text() {
        use crate::edit::EditBuffer;
        let mut buf = EditBuffer::new("plum ");
        buf.apply_plain(EditAction::MoveTo { to: 5, affinity: crate::edit::Affinity::Downstream, extend: false }, 0.0);
        let mut c = Composing::default();
        let feed = |buf: &mut EditBuffer, c: &mut Composing, ev: TextInputEvent| {
            for a in ev.actions(buf.text(), buf.selection().range()) {
                buf.apply_plain(a, 0.0);
            }
            c.take(&ev)
        };
        assert!(feed(&mut buf, &mut c, batch(0, 0, None, Some("n"))));
        assert!(feed(&mut buf, &mut c, batch(0, 0, None, Some("ni"))));
        assert_eq!(c.preedit().unwrap().text, "ni");
        assert_eq!(buf.text(), "plum ", "a preedit is not in the text");
        assert!(!feed(&mut buf, &mut c, batch(0, 0, None, Some("ni"))), "same preedit, no change");
        // The commit ends composing and goes in at the caret.
        assert!(feed(&mut buf, &mut c, batch(0, 0, Some("你"), None)));
        assert_eq!((buf.text(), c.preedit()), ("plum 你", None));
        // A commit and a new preedit in one batch.
        feed(&mut buf, &mut c, batch(0, 0, Some("好"), Some("m")));
        assert_eq!((buf.text(), c.preedit().map(|p| p.text.as_str())), ("plum 你好", Some("m")));
        // Focus loss drops the preedit; the text keeps what was committed.
        assert!(c.cancel());
        assert!(!c.cancel());
        assert_eq!(buf.text(), "plum 你好");
    }

    #[test]
    fn deletes_reach_around_the_selection_and_snap_to_chars() {
        let text = "naïve pear";
        // Caret after "naï" (byte 4); delete 1 byte before lands inside "ï": widened.
        assert_eq!(
            batch(1, 0, Some("i"), None).actions(text, 4..4),
            vec![EditAction::Select { anchor: 2, focus: 4 }, EditAction::Insert("i".into())]
        );
        // Delete only, past the end: stops at the end.
        assert_eq!(
            batch(0, 99, None, None).actions(text, 6..6),
            vec![EditAction::Select { anchor: 6, focus: text.len() }, EditAction::Insert(String::new())]
        );
        // A commit over a selection replaces it; a preedit alone edits nothing.
        assert_eq!(batch(0, 0, Some("x"), None).actions(text, 0..5), vec![EditAction::Insert("x".into())]);
        assert!(batch(0, 0, None, Some("p")).actions(text, 0..0).is_empty());
    }
}
