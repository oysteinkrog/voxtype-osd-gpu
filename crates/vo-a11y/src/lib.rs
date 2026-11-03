//! What a screen reader sees of an overlay (plan stage F8), as an AccessKit tree.
//!
//! An app describes itself with an [`Accessible`]: a name, the text it shows, and, while the
//! text can be edited, an [`EditField`] with the text and its selection. [`tree`] turns that
//! into a full [`TreeUpdate`]:
//!
//! - a `Window` root named after the app,
//! - a `Label` with the shown text, marked as a polite live region, so a screen reader reads
//!   it out when it changes without cutting into what it is saying,
//! - while editing, a `MultilineTextInput` with the text as its value and focus, holding one
//!   `TextRun` per hard line, with character lengths by grapheme cluster and the caret and
//!   selection as a `TextSelection`.
//!
//! Live text changes many times a second while someone speaks. [`Announcer`] picks what the
//! label shows: a final text at once and only once, and live text at most every
//! [`LIVE_EVERY`] seconds, so a screen reader is not flooded with every revision.
//!
//! No platform code: `vo` connects this to AT-SPI (accesskit_unix) behind its `a11y` feature.

use accesskit::{Live, Node, NodeId, Role, TextPosition, TextSelection, Tree, TreeId, TreeUpdate};
use unicode_segmentation::UnicodeSegmentation;

pub use accesskit;

/// What an app shows, for a screen reader.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Accessible {
    /// The window's name, such as "Voxtype dictation".
    pub name: String,
    /// The text shown now.
    pub text: String,
    /// `text` is final: it will not change again for this piece of work (say, the delivered
    /// dictation). Announced at once; live text waits for [`LIVE_EVERY`].
    pub done: bool,
    /// The text box being edited, while there is one.
    pub edit: Option<EditField>,
}

/// A text box being edited: its text and selection, in bytes on char boundaries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditField {
    pub value: String,
    pub anchor: usize,
    pub focus: usize,
}

pub const WINDOW: NodeId = NodeId(1);
pub const TEXT: NodeId = NodeId(2);
pub const EDIT: NodeId = NodeId(3);
/// Text run `i` of the edit field is `NodeId(RUNS + i)`.
pub const RUNS: u64 = 100;

/// The full tree for `a`, with `text` as the label's text (from [`Announcer`]).
pub fn tree(a: &Accessible, text: &str) -> TreeUpdate {
    let mut window = Node::new(Role::Window);
    window.set_label(a.name.clone());
    let mut label = Node::new(Role::Label);
    label.set_label(text.to_string());
    label.set_live(Live::Polite);
    let mut children = vec![TEXT];
    let mut nodes = Vec::new();
    let mut focus = WINDOW;
    if let Some(e) = &a.edit {
        let (field, runs) = edit_nodes(e);
        children.push(EDIT);
        nodes.push((EDIT, field));
        nodes.extend(runs);
        focus = EDIT;
    }
    window.set_children(children);
    nodes.insert(0, (TEXT, label));
    nodes.insert(0, (WINDOW, window));
    TreeUpdate {
        nodes,
        tree: Some(Tree { toolkit_name: Some("vo".into()), ..Tree::new(WINDOW) }),
        tree_id: TreeId::ROOT,
        focus,
    }
}

/// One hard line of an edit field's text, with its line break at the end.
struct Run<'a> {
    start: usize,
    text: &'a str,
    /// Byte length of each character (grapheme cluster; split where one is over 255 bytes).
    lengths: Vec<u8>,
}

fn runs(value: &str) -> Vec<Run<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    for line in value.split_inclusive('\n').chain((value.is_empty() || value.ends_with('\n')).then_some("")) {
        let mut lengths = Vec::new();
        for g in line.graphemes(true) {
            if g.len() <= u8::MAX as usize {
                lengths.push(g.len() as u8);
            } else {
                lengths.extend(g.chars().map(|c| c.len_utf8() as u8));
            }
        }
        out.push(Run { start, text: line, lengths });
        start += line.len();
    }
    out
}

/// Where byte `at` falls: the run and the character in it. A position at the end of a line
/// that ends with a break is on the break; the end of the text is past the last character.
fn position(runs: &[Run<'_>], at: usize) -> TextPosition {
    let i = runs.iter().rposition(|r| r.start <= at).unwrap_or(0);
    let r = &runs[i];
    let mut b = r.start;
    let mut index = 0;
    for &l in &r.lengths {
        if b >= at {
            break;
        }
        b += l as usize;
        index += 1;
    }
    TextPosition { node: NodeId(RUNS + i as u64), character_index: index }
}

fn edit_nodes(e: &EditField) -> (Node, Vec<(NodeId, Node)>) {
    let runs = runs(&e.value);
    let mut field = Node::new(Role::MultilineTextInput);
    field.set_value(e.value.clone());
    field.set_children((0..runs.len()).map(|i| NodeId(RUNS + i as u64)).collect::<Vec<_>>());
    field.set_text_selection(TextSelection { anchor: position(&runs, e.anchor), focus: position(&runs, e.focus) });
    let nodes = runs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut n = Node::new(Role::TextRun);
            n.set_value(r.text.to_string());
            n.set_character_lengths(r.lengths.clone());
            (NodeId(RUNS + i as u64), n)
        })
        .collect();
    (field, nodes)
}

/// Seconds between two announcements of live text.
pub const LIVE_EVERY: f64 = 2.0;

/// Picks the text the label shows, so a screen reader hears live text now and then and a
/// final text once.
#[derive(Clone, Debug, Default)]
pub struct Announcer {
    shown: String,
    /// When live text was last shown.
    at: Option<f64>,
}

impl Announcer {
    /// The app shows `text` at `t`, final when `done`. Returns true when the label's text
    /// changes now; it is then [`Announcer::shown`]. Live text that changed less than
    /// `LIVE_EVERY` after the last one waits for a later call.
    pub fn take(&mut self, t: f64, text: &str, done: bool) -> bool {
        if text == self.shown {
            return false;
        }
        if !done && self.at.is_some_and(|at| t - at < LIVE_EVERY) {
            return false;
        }
        self.shown = text.to_string();
        self.at = (!done).then_some(t);
        true
    }

    pub fn shown(&self) -> &str {
        &self.shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(u: &TreeUpdate, id: NodeId) -> &Node {
        &u.nodes.iter().find(|(i, _)| *i == id).expect("node").1
    }

    #[test]
    fn a_dictation_is_a_window_with_a_polite_label() {
        let a = Accessible { name: "Dictation".into(), text: "plum pear".into(), ..Accessible::default() };
        let u = tree(&a, "plum pear");
        assert_eq!(u.tree.as_ref().unwrap().root, WINDOW);
        assert_eq!(u.focus, WINDOW);
        let w = node(&u, WINDOW);
        assert_eq!((w.role(), w.label()), (Role::Window, Some("Dictation")));
        assert_eq!(w.children(), &[TEXT]);
        let l = node(&u, TEXT);
        assert_eq!((l.role(), l.label(), l.live()), (Role::Label, Some("plum pear"), Some(Live::Polite)));
        assert_eq!(u.nodes.len(), 2);
    }

    #[test]
    fn edit_mode_has_a_focused_field_with_runs_caret_and_selection() {
        // Two hard lines; "é" as e plus a combining accent is one character of 3 bytes.
        let value = "plum ce\u{301}\nkiwi".to_string();
        let caret = value.find("kiwi").unwrap() + 2;
        let a = Accessible {
            name: "Dictation".into(),
            text: String::new(),
            done: false,
            edit: Some(EditField { value: value.clone(), anchor: 5, focus: caret }),
        };
        let u = tree(&a, "");
        assert_eq!(u.focus, EDIT);
        let f = node(&u, EDIT);
        assert_eq!((f.role(), f.value()), (Role::MultilineTextInput, Some(value.as_str())));
        assert_eq!(f.children(), &[NodeId(RUNS), NodeId(RUNS + 1)]);
        let r0 = node(&u, NodeId(RUNS));
        assert_eq!((r0.role(), r0.value()), (Role::TextRun, Some("plum ce\u{301}\n")));
        assert_eq!(r0.character_lengths(), &[1, 1, 1, 1, 1, 1, 3, 1]);
        assert_eq!(node(&u, NodeId(RUNS + 1)).value(), Some("kiwi"));
        let s = f.text_selection().unwrap();
        assert_eq!(s.anchor, TextPosition { node: NodeId(RUNS), character_index: 5 });
        assert_eq!(s.focus, TextPosition { node: NodeId(RUNS + 1), character_index: 2 });
    }

    #[test]
    fn positions_at_line_ends_and_the_text_end() {
        let rs = runs("ab\ncd");
        // At the break: on it. After the break: the next line's start.
        assert_eq!(position(&rs, 2), TextPosition { node: NodeId(RUNS), character_index: 2 });
        assert_eq!(position(&rs, 3), TextPosition { node: NodeId(RUNS + 1), character_index: 0 });
        assert_eq!(position(&rs, 5), TextPosition { node: NodeId(RUNS + 1), character_index: 2 });
        // A text ending in a break has an empty last line for the caret.
        let rs = runs("ab\n");
        assert_eq!(rs.len(), 2);
        assert_eq!(position(&rs, 3), TextPosition { node: NodeId(RUNS + 1), character_index: 0 });
        assert_eq!(runs("").len(), 1);
    }

    #[test]
    fn live_text_is_announced_now_and_then_and_a_final_text_once() {
        let mut a = Announcer::default();
        assert!(a.take(0.0, "plum", false));
        assert!(!a.take(0.5, "plum pear", false), "too soon after the last");
        assert!(!a.take(1.9, "plum pear fig", false));
        assert!(a.take(2.1, "plum pear fig", false));
        assert_eq!(a.shown(), "plum pear fig");
        // A final text goes at once, and only once.
        assert!(a.take(2.2, "Plum, pear, fig.", true));
        assert!(!a.take(2.3, "Plum, pear, fig.", true));
        // A new dictation's live text after a final one goes at once.
        assert!(a.take(2.4, "kiwi", false));
    }
}
