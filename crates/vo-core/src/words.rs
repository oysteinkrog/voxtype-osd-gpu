//! Word model: turns each new hypothesis into a word list aligned to the
//! previous one, so every word on screen keeps a stable id across preview
//! revisions and the final swap (capability TY06).
//!
//! Algorithm:
//! 1. Split on whitespace. Each word gets a match key: lower case, letters
//!    and digits only ("Hello," and "hello" share the key "hello").
//! 2. Myers diff on the keys (`similar`). Equal runs keep their ids.
//! 3. Each changed run (old words replaced by new ones) is aligned with a
//!    small dynamic program that pairs words by character similarity, in
//!    order. A pair at or above [`MIN_SIMILARITY`] keeps the old id
//!    ("there" to "their", "confi" to "config"). Unpaired old words are
//!    removed, unpaired new words get new ids.

use similar::{capture_diff_slices, Algorithm, DiffOp};

pub type WordId = u32;

/// Pairs below this similarity are a removal plus an insertion.
pub const MIN_SIMILARITY: f32 = 0.5;

#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub id: WordId,
    pub text: String,
}

/// What happened to one word between two hypotheses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    /// Same text.
    Kept,
    /// Same letters; case or punctuation changed ("hello" to "Hello,").
    Restyled,
    /// The old key is a prefix of the new one: a word still being spoken
    /// ("confi" to "config").
    Extended,
    /// A different but similar word that keeps the id ("there" to "their").
    Substituted,
    /// A new word with a new id.
    Inserted,
    /// An old word that is gone.
    Removed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    pub id: WordId,
    /// Index in the previous word list (None for `Inserted`).
    pub old_index: Option<usize>,
    /// Index in the new word list (None for `Removed`).
    pub new_index: Option<usize>,
}

/// The words on screen and the next free id.
#[derive(Default, Debug)]
pub struct WordModel {
    words: Vec<Word>,
    next_id: WordId,
}

/// Match key: lower-case letters and digits. A word with none (a lone
/// dash) keys on its own text.
pub fn key(word: &str) -> String {
    let k: String = word.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
    if k.is_empty() {
        word.to_string()
    } else {
        k
    }
}

/// Character similarity of two keys, 0 to 1: 1 minus the Levenshtein
/// distance over the longer length. A non-empty prefix counts as 1.
pub fn similarity(a: &str, b: &str) -> f32 {
    if a == b || (!a.is_empty() && b.starts_with(a)) {
        return 1.0;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let sub = prev[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    1.0 - prev[b.len()] as f32 / longest as f32
}

/// Pair old and new words of one changed run, in order, maximising the
/// summed similarity of pairs at or above [`MIN_SIMILARITY`].
/// Returns `(old, new)` index pairs relative to the run.
fn align(old: &[String], new: &[String]) -> Vec<(usize, usize)> {
    let (n, m) = (old.len(), new.len());
    let sim: Vec<Vec<f32>> = old.iter().map(|o| new.iter().map(|w| similarity(o, w)).collect()).collect();
    // best[i][j]: best score for old[i..] and new[j..].
    let mut best = vec![vec![0.0f32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let mut b = best[i + 1][j].max(best[i][j + 1]);
            if sim[i][j] >= MIN_SIMILARITY {
                b = b.max(sim[i][j] + best[i + 1][j + 1]);
            }
            best[i][j] = b;
        }
    }
    let (mut i, mut j, mut pairs) = (0, 0, Vec::new());
    while i < n && j < m {
        if sim[i][j] >= MIN_SIMILARITY && best[i][j] == sim[i][j] + best[i + 1][j + 1] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if best[i][j] == best[i + 1][j] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

impl WordModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn words(&self) -> &[Word] {
        &self.words
    }

    fn fresh_id(&mut self) -> WordId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Old words from `cur.0` up to `end.0` are removed; new words from
    /// `cur.1` up to `end.1` are inserted with new ids.
    fn gap(
        &mut self,
        cur: &mut (usize, usize),
        end: (usize, usize),
        new_texts: &[&str],
        changes: &mut Vec<Change>,
        words: &mut Vec<Word>,
    ) {
        for oi in cur.0..end.0 {
            let id = self.words[oi].id;
            changes.push(Change { kind: ChangeKind::Removed, id, old_index: Some(oi), new_index: None });
        }
        for (ni, text) in new_texts.iter().enumerate().take(end.1).skip(cur.1) {
            let id = self.fresh_id();
            changes.push(Change { kind: ChangeKind::Inserted, id, old_index: None, new_index: Some(ni) });
            words.push(Word { id, text: text.to_string() });
        }
        *cur = end;
    }

    /// Replace the hypothesis with `text` and return one change per old and
    /// new word: kept ones in new order, removed ones where they were.
    /// The final text goes through the same call, diffed against the whole
    /// paragraph.
    pub fn update(&mut self, text: &str) -> Vec<Change> {
        let new_texts: Vec<&str> = text.split_whitespace().collect();
        let old_keys: Vec<String> = self.words.iter().map(|w| key(&w.text)).collect();
        let new_keys: Vec<String> = new_texts.iter().map(|w| key(w)).collect();

        // Merge the diff into alternating equal and changed runs.
        let mut runs: Vec<(bool, std::ops::Range<usize>, std::ops::Range<usize>)> = Vec::new();
        for op in capture_diff_slices(Algorithm::Myers, &old_keys, &new_keys) {
            let (equal, o, n) = match op {
                DiffOp::Equal { old_index, new_index, len } => {
                    (true, old_index..old_index + len, new_index..new_index + len)
                }
                DiffOp::Delete { old_index, old_len, new_index } => {
                    (false, old_index..old_index + old_len, new_index..new_index)
                }
                DiffOp::Insert { old_index, new_index, new_len } => {
                    (false, old_index..old_index, new_index..new_index + new_len)
                }
                DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                    (false, old_index..old_index + old_len, new_index..new_index + new_len)
                }
            };
            match runs.last_mut() {
                Some((false, ro, rn)) if !equal => {
                    ro.end = o.end;
                    rn.end = n.end;
                }
                _ => runs.push((equal, o, n)),
            }
        }

        let mut changes = Vec::with_capacity(self.words.len().max(new_texts.len()));
        let mut words = Vec::with_capacity(new_texts.len());
        for (equal, o, n) in runs {
            if equal {
                for (oi, ni) in o.zip(n) {
                    let old = &self.words[oi];
                    let kind = if old.text == new_texts[ni] { ChangeKind::Kept } else { ChangeKind::Restyled };
                    changes.push(Change { kind, id: old.id, old_index: Some(oi), new_index: Some(ni) });
                    words.push(Word { id: old.id, text: new_texts[ni].to_string() });
                }
                continue;
            }
            let pairs = align(&old_keys[o.clone()], &new_keys[n.clone()]);
            let mut cur = (o.start, n.start);
            for (po, pn) in pairs {
                let (po, pn) = (o.start + po, n.start + pn);
                self.gap(&mut cur, (po, pn), &new_texts, &mut changes, &mut words);
                let old = &self.words[po];
                let kind = if old_keys[po] == new_keys[pn] {
                    ChangeKind::Restyled
                } else if new_keys[pn].starts_with(&old_keys[po]) {
                    ChangeKind::Extended
                } else {
                    ChangeKind::Substituted
                };
                changes.push(Change { kind, id: old.id, old_index: Some(po), new_index: Some(pn) });
                words.push(Word { id: old.id, text: new_texts[pn].to_string() });
                cur = (po + 1, pn + 1);
            }
            self.gap(&mut cur, (o.end, n.end), &new_texts, &mut changes, &mut words);
        }
        self.words = words;
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(changes: &[Change]) -> Vec<ChangeKind> {
        changes.iter().map(|c| c.kind).collect()
    }

    fn ids(m: &WordModel) -> Vec<WordId> {
        m.words().iter().map(|w| w.id).collect()
    }

    #[test]
    fn appending_keeps_ids_and_adds_new_ones() {
        let mut m = WordModel::new();
        m.update("the quick");
        let before = ids(&m);
        let c = m.update("the quick brown fox");
        assert_eq!(kinds(&c), [ChangeKind::Kept, ChangeKind::Kept, ChangeKind::Inserted, ChangeKind::Inserted]);
        assert_eq!(&ids(&m)[..2], &before[..]);
    }

    #[test]
    fn a_word_still_being_spoken_keeps_its_id() {
        let mut m = WordModel::new();
        m.update("open the confi");
        let id = m.words()[2].id;
        let c = m.update("open the configuration file");
        assert_eq!(c[2].kind, ChangeKind::Extended);
        assert_eq!(m.words()[2].id, id);
        assert_eq!(c[3].kind, ChangeKind::Inserted);
    }

    #[test]
    fn similar_word_is_substituted_in_place() {
        let mut m = WordModel::new();
        m.update("put it over there now");
        let id = m.words()[3].id;
        let c = m.update("put it over their now");
        assert_eq!(c[3].kind, ChangeKind::Substituted);
        assert_eq!(m.words()[3].id, id);
        assert!(c.iter().filter(|c| c.kind != ChangeKind::Substituted).all(|c| c.kind == ChangeKind::Kept));
    }

    #[test]
    fn final_swap_restyles_case_and_punctuation() {
        let mut m = WordModel::new();
        m.update("hello world how are you");
        let before = ids(&m);
        let c = m.update("Hello, world. How are you?");
        assert_eq!(ids(&m), before);
        assert_eq!(c.iter().filter(|c| c.kind == ChangeKind::Restyled).count(), 4);
    }

    #[test]
    fn spoken_dash_merges_into_one_word() {
        let mut m = WordModel::new();
        m.update("a well dash known name");
        let well = m.words()[1].id;
        let c = m.update("a well-known name");
        assert_eq!(m.words()[1].id, well);
        assert_eq!(c.iter().filter(|c| c.kind == ChangeKind::Removed).count(), 2);
        assert_eq!(c.iter().filter(|c| c.kind == ChangeKind::Inserted).count(), 0);
    }

    #[test]
    fn unrelated_word_gets_a_new_id() {
        let mut m = WordModel::new();
        m.update("send it today");
        let old = m.words()[2].id;
        let c = m.update("send it tomorrow");
        // "today" and "tomorrow" share "to" only: below MIN_SIMILARITY.
        assert!(c.iter().any(|c| c.kind == ChangeKind::Removed && c.id == old));
        assert_ne!(m.words()[2].id, old);
    }

    #[test]
    fn similarity_bounds() {
        assert_eq!(similarity("abc", "abc"), 1.0);
        assert_eq!(similarity("con", "config"), 1.0);
        assert!(similarity("there", "their") >= MIN_SIMILARITY);
        assert!(similarity("cat", "dog") < MIN_SIMILARITY);
    }
}
