//! Edit model: one editable text with a selection, the moves a text box needs, and the edits
//! they make (plan stage F3). Pure: no fonts, no geometry, no clock.
//!
//! Positions are byte offsets into the UTF-8 text and always sit on grapheme cluster
//! boundaries (UAX #29), so a caret never lands inside "é" written as e plus a combining
//! accent, a flag, or a ZWJ emoji sequence.
//!
//! Word moves and word deletes follow Qt's `QTextLayout` word rule, the one `QLineEdit` uses
//! (checked against a real `QLineEdit` in the tests):
//! - forward: from a separator, skip separators; else skip word characters (anything that is
//!   neither white space nor a separator). Then skip white space.
//! - backward: skip white space, then a run of separators if one comes next, else a run of
//!   word characters.
//!
//! Separators are ASCII punctuation such as `.` `,` `-` `/` and quotes ([`is_word_separator`]).
//!
//! Vertical moves and line-edge moves need the wrapped layout. The app passes it in as a
//! [`Lines`]; [`PlainLines`] is the layout with no soft wraps, one line per hard newline.

use std::ops::Range;

use unicode_segmentation::GraphemeCursor;

/// Which side of a soft line break a position belongs to. Only matters at a soft wrap,
/// where the end of one line and the start of the next are the same byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Affinity {
    /// The end of the earlier line.
    Upstream,
    /// The start of the later line.
    #[default]
    Downstream,
}

/// The selection: `anchor` stays put while Shift moves `focus`. Equal ends are a caret.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Selection {
    pub anchor: usize,
    pub focus: usize,
    pub affinity: Affinity,
}

impl Selection {
    pub fn caret(at: usize) -> Selection {
        Selection { anchor: at, focus: at, affinity: Affinity::Downstream }
    }

    pub fn is_caret(&self) -> bool {
        self.anchor == self.focus
    }

    /// The selected bytes, start first.
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.focus)..self.anchor.max(self.focus)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dir {
    Back,
    Forward,
}

/// How far a move or a delete goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unit {
    /// One grapheme cluster.
    Grapheme,
    /// To the next word stop, by the Qt rule.
    Word,
    /// To the start or end of the wrapped line (Home, End).
    Line,
    /// To the start or end of the text (Ctrl+Home, Ctrl+End).
    Text,
}

/// One thing the user does to the text.
#[derive(Clone, Debug, PartialEq)]
pub enum EditAction {
    /// Types `text` at the caret, replacing the selection.
    Insert(String),
    /// Deletes the selection, or with a caret, from the caret by `Unit` in `Dir`. A word
    /// delete with a selection extends the selection by a word first, as `QLineEdit` does.
    Delete(Dir, Unit),
    /// Moves the focus. Without `extend`, a grapheme move with a selection collapses it to
    /// the side it goes to.
    MoveBy {
        dir: Dir,
        unit: Unit,
        extend: bool,
    },
    /// Up or Down by one wrapped line, keeping the x the run of vertical moves started at.
    /// Up on the first line goes to the text start, Down on the last to the text end.
    MoveLine {
        down: bool,
        extend: bool,
    },
    /// Puts the focus at `to` (from a hit test), snapped to a grapheme boundary.
    MoveTo {
        to: usize,
        affinity: Affinity,
        extend: bool,
    },
    SelectAll,
}

/// The text change an action made: `range` of the old text was replaced by `inserted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditEffect {
    pub range: Range<usize>,
    pub removed: String,
    pub inserted: String,
}

/// The wrapped layout of the buffer's current text, for vertical and line-edge moves.
/// Implemented by the app over its shaped text; x is in any unit, as long as it is the same
/// one for every line.
pub trait Lines {
    fn line_count(&self) -> usize;
    /// The line that holds `byte`. At a soft wrap, `Upstream` is the earlier line.
    fn line_of(&self, byte: usize, affinity: Affinity) -> usize;
    /// The bytes of `line` without the hard newline that ends it, if any. A soft-wrapped line
    /// keeps its trailing spaces.
    fn line_range(&self, line: usize) -> Range<usize>;
    /// The caret x at `byte`.
    fn x_of(&self, byte: usize, affinity: Affinity) -> f32;
    /// The caret stop on `line` nearest to `x`.
    fn stop_at_x(&self, line: usize, x: f32) -> (usize, Affinity);
}

/// The text as one line per hard newline, with x counted in grapheme clusters. For tests and
/// for an app that does not wrap.
#[derive(Clone, Debug)]
pub struct PlainLines {
    text: String,
    lines: Vec<Range<usize>>,
}

impl PlainLines {
    pub fn new(text: &str) -> PlainLines {
        let mut lines = Vec::new();
        let mut start = 0;
        for (i, _) in text.match_indices('\n') {
            lines.push(start..i);
            start = i + 1;
        }
        lines.push(start..text.len());
        PlainLines { text: text.to_owned(), lines }
    }
}

impl Lines for PlainLines {
    fn line_count(&self) -> usize {
        self.lines.len()
    }

    fn line_of(&self, byte: usize, _: Affinity) -> usize {
        self.lines.iter().rposition(|r| r.start <= byte).unwrap_or(0)
    }

    fn line_range(&self, line: usize) -> Range<usize> {
        self.lines[line].clone()
    }

    fn x_of(&self, byte: usize, affinity: Affinity) -> f32 {
        let r = &self.lines[self.line_of(byte, affinity)];
        graphemes_between(&self.text, r.start, byte) as f32
    }

    fn stop_at_x(&self, line: usize, x: f32) -> (usize, Affinity) {
        let r = self.lines[line].clone();
        let mut at = r.start;
        let mut n = 0.0;
        while at < r.end && n + 0.5 <= x {
            at = next_grapheme(&self.text, at);
            n += 1.0;
        }
        (at, Affinity::Downstream)
    }
}

/// One editable text, its selection and its revision number.
#[derive(Clone, Debug, PartialEq)]
pub struct EditBuffer {
    text: String,
    sel: Selection,
    /// The x a run of Up and Down moves keeps to. Any other action forgets it.
    goal_x: Option<f32>,
    rev: u64,
}

impl EditBuffer {
    /// A buffer holding `text`, with the caret at the end.
    pub fn new(text: impl Into<String>) -> EditBuffer {
        let text = text.into();
        let sel = Selection::caret(text.len());
        EditBuffer { text, sel, goal_x: None, rev: 0 }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn selection(&self) -> Selection {
        self.sel
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.sel.range()]
    }

    /// Goes up by one on every change to the text, never on a move.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Sets the selection, clamped to the text and snapped to grapheme boundaries.
    pub fn set_selection(&mut self, sel: Selection) {
        self.sel = Selection {
            anchor: self.snap(sel.anchor, Dir::Back),
            focus: self.snap(sel.focus, Dir::Back),
            affinity: sel.affinity,
        };
        self.goal_x = None;
    }

    /// [`EditBuffer::apply`] with [`PlainLines`] for the current text.
    pub fn apply_plain(&mut self, action: EditAction) -> Option<EditEffect> {
        let lines = PlainLines::new(&self.text);
        self.apply(action, &lines)
    }

    /// Does `action`. `lines` is the layout of the text as it is before the action. Returns
    /// the text change, or `None` when only the selection moved (or nothing happened).
    pub fn apply(&mut self, action: EditAction, lines: &dyn Lines) -> Option<EditEffect> {
        if !matches!(action, EditAction::MoveLine { .. }) {
            self.goal_x = None;
        }
        match action {
            EditAction::Insert(s) => {
                if s.is_empty() && self.sel.is_caret() {
                    return None;
                }
                Some(self.replace(self.sel.range(), &s))
            }
            EditAction::Delete(dir, unit) => {
                let range = match unit {
                    Unit::Grapheme if !self.sel.is_caret() => self.sel.range(),
                    _ => {
                        let to = self.target(self.sel.focus, self.sel.affinity, dir, unit, lines).0;
                        self.sel.anchor.min(to)..self.sel.anchor.max(to)
                    }
                };
                if range.is_empty() {
                    return None;
                }
                Some(self.replace(range, ""))
            }
            EditAction::MoveBy { dir, unit, extend } => {
                let (to, affinity) = if unit == Unit::Grapheme && !extend && !self.sel.is_caret() {
                    let r = self.sel.range();
                    (if dir == Dir::Back { r.start } else { r.end }, Affinity::Downstream)
                } else {
                    self.target(self.sel.focus, self.sel.affinity, dir, unit, lines)
                };
                self.move_focus(to, affinity, extend);
                None
            }
            EditAction::MoveLine { down, extend } => {
                let line = lines.line_of(self.sel.focus, self.sel.affinity);
                let x = *self.goal_x.get_or_insert_with(|| lines.x_of(self.sel.focus, self.sel.affinity));
                let (to, affinity) = if !down && line == 0 {
                    (0, Affinity::Downstream)
                } else if down && line + 1 >= lines.line_count() {
                    (self.text.len(), Affinity::Downstream)
                } else {
                    let (b, a) = lines.stop_at_x(if down { line + 1 } else { line - 1 }, x);
                    (self.snap(b, Dir::Back), a)
                };
                self.move_focus(to, affinity, extend);
                None
            }
            EditAction::MoveTo { to, affinity, extend } => {
                let to = self.snap(to, Dir::Back);
                self.move_focus(to, affinity, extend);
                None
            }
            EditAction::SelectAll => {
                self.sel = Selection { anchor: 0, focus: self.text.len(), affinity: Affinity::Downstream };
                None
            }
        }
    }

    fn move_focus(&mut self, to: usize, affinity: Affinity, extend: bool) {
        self.sel = if extend {
            Selection { anchor: self.sel.anchor, focus: to, affinity }
        } else {
            Selection { anchor: to, focus: to, affinity }
        };
    }

    /// Where a move from `from` by `unit` in `dir` lands.
    fn target(&self, from: usize, affinity: Affinity, dir: Dir, unit: Unit, lines: &dyn Lines) -> (usize, Affinity) {
        let t = &self.text;
        match (unit, dir) {
            (Unit::Grapheme, Dir::Back) => (prev_grapheme(t, from), Affinity::Downstream),
            (Unit::Grapheme, Dir::Forward) => (next_grapheme(t, from), Affinity::Downstream),
            (Unit::Word, Dir::Back) => (self.snap(prev_word(t, from), Dir::Back), Affinity::Downstream),
            (Unit::Word, Dir::Forward) => (self.snap(next_word(t, from), Dir::Forward), Affinity::Downstream),
            (Unit::Line, Dir::Back) => (lines.line_range(lines.line_of(from, affinity)).start, Affinity::Downstream),
            (Unit::Line, Dir::Forward) => (lines.line_range(lines.line_of(from, affinity)).end, Affinity::Upstream),
            (Unit::Text, Dir::Back) => (0, Affinity::Downstream),
            (Unit::Text, Dir::Forward) => (t.len(), Affinity::Downstream),
        }
    }

    fn replace(&mut self, range: Range<usize>, with: &str) -> EditEffect {
        let removed = self.text[range.clone()].to_owned();
        self.text.replace_range(range.clone(), with);
        let caret = self.snap(range.start + with.len(), Dir::Forward);
        self.sel = Selection::caret(caret);
        self.rev += 1;
        EditEffect { range, removed, inserted: with.to_owned() }
    }

    /// `at` clamped to the text and moved to a grapheme boundary in `dir` if it is not on one.
    fn snap(&self, at: usize, dir: Dir) -> usize {
        let mut at = at.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            match dir {
                Dir::Back => at -= 1,
                Dir::Forward => at += 1,
            }
        }
        if is_grapheme_boundary(&self.text, at) {
            return at;
        }
        match dir {
            Dir::Back => prev_grapheme(&self.text, at),
            Dir::Forward => next_grapheme(&self.text, at),
        }
    }
}

/// True for the characters Qt's word moves treat as separators.
pub fn is_word_separator(c: char) -> bool {
    matches!(
        c,
        '.' | ','
            | '?'
            | '!'
            | '@'
            | '#'
            | '$'
            | ':'
            | ';'
            | '-'
            | '<'
            | '>'
            | '['
            | ']'
            | '('
            | ')'
            | '{'
            | '}'
            | '='
            | '/'
            | '+'
            | '%'
            | '&'
            | '^'
            | '*'
            | '\''
            | '"'
            | '`'
            | '~'
            | '|'
            | '\\'
    )
}

fn is_word_char(c: char) -> bool {
    !c.is_whitespace() && !is_word_separator(c)
}

/// The next word stop after `at`, by the Qt rule. May fall inside a grapheme cluster when a
/// separator is followed by a combining mark; [`EditBuffer`] snaps it.
pub fn next_word(text: &str, at: usize) -> usize {
    let mut chars = text[at..].char_indices().map(|(i, c)| (at + i, c)).peekable();
    let skip = |chars: &mut std::iter::Peekable<_>, f: &dyn Fn(char) -> bool| {
        while chars.next_if(|&(_, c): &(usize, char)| f(c)).is_some() {}
    };
    match chars.peek() {
        Some(&(_, c)) if is_word_separator(c) => skip(&mut chars, &is_word_separator),
        _ => skip(&mut chars, &is_word_char),
    }
    skip(&mut chars, &char::is_whitespace);
    chars.peek().map_or(text.len(), |&(i, _)| i)
}

/// The word stop before `at`, by the Qt rule.
pub fn prev_word(text: &str, at: usize) -> usize {
    let mut chars = text[..at].char_indices().rev().peekable();
    let mut pos = at;
    let mut skip = |chars: &mut std::iter::Peekable<_>, f: &dyn Fn(char) -> bool| {
        while let Some((i, _)) = chars.next_if(|&(_, c): &(usize, char)| f(c)) {
            pos = i;
        }
    };
    skip(&mut chars, &char::is_whitespace);
    match chars.peek() {
        Some(&(_, c)) if is_word_separator(c) => skip(&mut chars, &is_word_separator),
        _ => skip(&mut chars, &is_word_char),
    }
    pos
}

/// The grapheme boundary after `at`, or the text length.
pub fn next_grapheme(text: &str, at: usize) -> usize {
    GraphemeCursor::new(at, text.len(), true).next_boundary(text, 0).ok().flatten().unwrap_or(text.len())
}

/// The grapheme boundary before `at`, or 0.
pub fn prev_grapheme(text: &str, at: usize) -> usize {
    GraphemeCursor::new(at, text.len(), true).prev_boundary(text, 0).ok().flatten().unwrap_or(0)
}

pub fn is_grapheme_boundary(text: &str, at: usize) -> bool {
    text.is_char_boundary(at) && GraphemeCursor::new(at, text.len(), true).is_boundary(text, 0).unwrap_or(false)
}

fn graphemes_between(text: &str, from: usize, to: usize) -> usize {
    let mut n = 0;
    let mut at = from;
    while at < to {
        at = next_grapheme(text, at);
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret(text: &str, at: usize) -> EditBuffer {
        let mut b = EditBuffer::new(text);
        b.set_selection(Selection::caret(at));
        b
    }

    fn mv(dir: Dir, unit: Unit) -> EditAction {
        EditAction::MoveBy { dir, unit, extend: false }
    }

    /// Every grapheme stop going right, then going left.
    fn walk(text: &str) -> (Vec<usize>, Vec<usize>) {
        let mut b = caret(text, 0);
        let mut right = vec![0];
        while b.selection().focus < text.len() {
            b.apply_plain(mv(Dir::Forward, Unit::Grapheme));
            right.push(b.selection().focus);
        }
        let mut left = vec![text.len()];
        while b.selection().focus > 0 {
            b.apply_plain(mv(Dir::Back, Unit::Grapheme));
            left.push(b.selection().focus);
        }
        (right, left)
    }

    #[test]
    fn grapheme_moves_norwegian() {
        let (right, left) = walk("æøå");
        assert_eq!(right, [0, 2, 4, 6]);
        assert_eq!(left, [6, 4, 2, 0]);
    }

    #[test]
    fn grapheme_moves_combining_marks() {
        // "e" + U+0301 is one cluster of 3 bytes.
        let (right, left) = walk("e\u{301}te\u{301}");
        assert_eq!(right, [0, 3, 4, 7]);
        assert_eq!(left, [7, 4, 3, 0]);
    }

    #[test]
    fn grapheme_moves_emoji_and_zwj() {
        // Thumbs up with a skin tone (8 bytes), then a ZWJ family (18 bytes), then "x".
        let s = "👍🏽👨\u{200d}👩\u{200d}👧x";
        let (right, left) = walk(s);
        assert_eq!(right, [0, 8, 26, 27]);
        assert_eq!(left, [27, 26, 8, 0]);
        // Backspace removes the whole family, Delete the whole thumbs up.
        let mut b = caret(s, 26);
        let e = b.apply_plain(EditAction::Delete(Dir::Back, Unit::Grapheme)).unwrap();
        assert_eq!(e.removed, "👨\u{200d}👩\u{200d}👧");
        assert_eq!(b.text(), "👍🏽x");
        let mut b = caret(s, 0);
        b.apply_plain(EditAction::Delete(Dir::Forward, Unit::Grapheme));
        assert_eq!(b.text(), "👨\u{200d}👩\u{200d}👧x");
    }

    #[test]
    fn positions_snap_to_clusters() {
        let mut b = EditBuffer::new("e\u{301}x");
        b.set_selection(Selection::caret(1));
        assert_eq!(b.selection().focus, 0);
        b.apply_plain(EditAction::MoveTo { to: 2, affinity: Affinity::Downstream, extend: false });
        assert_eq!(b.selection().focus, 0);
        b.set_selection(Selection::caret(99));
        assert_eq!(b.selection().focus, 4);
    }

    /// Rows from a real `QLineEdit` (PySide6, Qt 6, offscreen platform):
    /// (text, caret byte, Ctrl+Right, Ctrl+Left, text after Ctrl+Backspace, text after
    /// Ctrl+Delete). Qt places the caret inside clusters too; those rows are skipped.
    #[rustfmt::skip]
    const QT: &[(&str, usize, usize, usize, &str, &str)] = &[
        ("hello world", 0, 6, 0, "ello world", "world"),
        ("hello world", 1, 6, 0, "ello world", "hworld"),
        ("hello world", 2, 6, 0, "llo world", "heworld"),
        ("hello world", 3, 6, 0, "lo world", "helworld"),
        ("hello world", 4, 6, 0, "o world", "hellworld"),
        ("hello world", 5, 6, 0, " world", "helloworld"),
        ("hello world", 6, 11, 0, "world", "hello "),
        ("hello world", 7, 11, 6, "hello orld", "hello w"),
        ("hello world", 8, 11, 6, "hello rld", "hello wo"),
        ("hello world", 9, 11, 6, "hello ld", "hello wor"),
        ("hello world", 10, 11, 6, "hello d", "hello worl"),
        ("hello world", 11, 11, 6, "hello ", "hello world"),
        ("foo.bar baz", 0, 3, 0, "oo.bar baz", ".bar baz"),
        ("foo.bar baz", 1, 3, 0, "oo.bar baz", "f.bar baz"),
        ("foo.bar baz", 2, 3, 0, "o.bar baz", "fo.bar baz"),
        ("foo.bar baz", 3, 4, 0, ".bar baz", "foobar baz"),
        ("foo.bar baz", 4, 8, 3, "foobar baz", "foo.baz"),
        ("foo.bar baz", 5, 8, 4, "foo.ar baz", "foo.bbaz"),
        ("foo.bar baz", 6, 8, 4, "foo.r baz", "foo.babaz"),
        ("foo.bar baz", 7, 8, 4, "foo. baz", "foo.barbaz"),
        ("foo.bar baz", 8, 11, 4, "foo.baz", "foo.bar "),
        ("foo.bar baz", 9, 11, 8, "foo.bar az", "foo.bar b"),
        ("foo.bar baz", 10, 11, 8, "foo.bar z", "foo.bar ba"),
        ("foo.bar baz", 11, 11, 8, "foo.bar ", "foo.bar baz"),
        ("a  --  b", 0, 3, 0, "  --  b", "--  b"),
        ("a  --  b", 1, 3, 0, "  --  b", "a--  b"),
        ("a  --  b", 2, 3, 0, " --  b", "a --  b"),
        ("a  --  b", 3, 7, 0, "--  b", "a  b"),
        ("a  --  b", 4, 7, 3, "a  -  b", "a  -b"),
        ("a  --  b", 5, 7, 3, "a    b", "a  --b"),
        ("a  --  b", 6, 7, 3, "a   b", "a  -- b"),
        ("a  --  b", 7, 8, 3, "a  b", "a  --  "),
        ("a  --  b", 8, 8, 7, "a  --  ", "a  --  b"),
        ("end.", 0, 3, 0, "nd.", "."),
        ("end.", 3, 4, 0, ".", "end"),
        ("end.", 4, 4, 3, "end", "end."),
        ("  lead", 0, 2, 0, " lead", "lead"),
        ("  lead", 1, 2, 0, " lead", " lead"),
        ("  lead", 2, 6, 0, "lead", "  "),
        ("  lead", 3, 6, 2, "  ead", "  l"),
        ("  lead", 6, 6, 2, "  ", "  lead"),
        ("trail  ", 0, 7, 0, "rail  ", ""),
        ("trail  ", 5, 7, 0, "  ", "trail"),
        ("trail  ", 6, 7, 0, " ", "trail "),
        ("trail  ", 7, 7, 0, "", "trail  "),
        ("x, y; z!", 0, 1, 0, ", y; z!", ", y; z!"),
        ("x, y; z!", 1, 3, 0, ", y; z!", "xy; z!"),
        ("x, y; z!", 2, 3, 1, "x y; z!", "x,y; z!"),
        ("x, y; z!", 3, 4, 1, "xy; z!", "x, ; z!"),
        ("x, y; z!", 4, 6, 3, "x, ; z!", "x, yz!"),
        ("x, y; z!", 5, 6, 4, "x, y z!", "x, y;z!"),
        ("x, y; z!", 6, 7, 4, "x, yz!", "x, y; !"),
        ("x, y; z!", 7, 8, 6, "x, y; !", "x, y; z"),
        ("x, y; z!", 8, 8, 7, "x, y; z", "x, y; z!"),
        ("it's done", 0, 2, 0, "t's done", "'s done"),
        ("it's done", 2, 3, 0, "'s done", "its done"),
        ("it's done", 3, 5, 2, "its done", "it'done"),
        ("it's done", 4, 5, 3, "it' done", "it'sdone"),
        ("it's done", 5, 9, 3, "it'done", "it's "),
        ("it's done", 9, 9, 5, "it's ", "it's done"),
        ("path/to/file.rs", 0, 4, 0, "ath/to/file.rs", "/to/file.rs"),
        ("path/to/file.rs", 4, 5, 0, "/to/file.rs", "pathto/file.rs"),
        ("path/to/file.rs", 5, 7, 4, "pathto/file.rs", "path//file.rs"),
        ("path/to/file.rs", 7, 8, 5, "path//file.rs", "path/tofile.rs"),
        ("path/to/file.rs", 8, 12, 7, "path/tofile.rs", "path/to/.rs"),
        ("path/to/file.rs", 12, 13, 8, "path/to/.rs", "path/to/filers"),
        ("path/to/file.rs", 13, 15, 12, "path/to/filers", "path/to/file."),
        ("path/to/file.rs", 15, 15, 13, "path/to/file.", "path/to/file.rs"),
        ("æøå ÆØÅ", 0, 7, 0, "øå ÆØÅ", "ÆØÅ"),
        ("æøå ÆØÅ", 2, 7, 0, "øå ÆØÅ", "æÆØÅ"),
        ("æøå ÆØÅ", 4, 7, 0, "å ÆØÅ", "æøÆØÅ"),
        ("æøå ÆØÅ", 6, 7, 0, " ÆØÅ", "æøåÆØÅ"),
        ("æøå ÆØÅ", 7, 13, 0, "ÆØÅ", "æøå "),
        ("æøå ÆØÅ", 9, 13, 7, "æøå ØÅ", "æøå Æ"),
        ("æøå ÆØÅ", 11, 13, 7, "æøå Å", "æøå ÆØ"),
        ("æøå ÆØÅ", 13, 13, 7, "æøå ", "æøå ÆØÅ"),
        ("café niño", 0, 6, 0, "afé niño", "niño"),
        ("café niño", 3, 6, 0, "é niño", "cafniño"),
        ("café niño", 5, 6, 0, " niño", "caféniño"),
        ("café niño", 6, 11, 0, "niño", "café "),
        ("café niño", 8, 11, 6, "café ño", "café ni"),
        ("café niño", 10, 11, 6, "café o", "café niñ"),
        ("café niño", 11, 11, 6, "café ", "café niño"),
        ("e\u{301}te\u{301} x", 0, 8, 0, "\u{301}te\u{301} x", "x"),
        ("e\u{301}te\u{301} x", 1, 8, 0, "\u{301}te\u{301} x", "ex"),
        ("e\u{301}te\u{301} x", 3, 8, 0, "te\u{301} x", "e\u{301}x"),
        ("e\u{301}te\u{301} x", 4, 8, 0, "e\u{301} x", "e\u{301}tx"),
        ("e\u{301}te\u{301} x", 7, 8, 0, " x", "e\u{301}te\u{301}x"),
        ("e\u{301}te\u{301} x", 8, 9, 0, "x", "e\u{301}te\u{301} "),
        ("e\u{301}te\u{301} x", 9, 9, 8, "e\u{301}te\u{301} ", "e\u{301}te\u{301} x"),
        ("👍🏽 ok", 0, 9, 0, " ok", "ok"),
        ("👍🏽 ok", 4, 9, 0, "🏽 ok", "👍ok"),
        ("👍🏽 ok", 8, 9, 0, " ok", "👍🏽ok"),
        ("👍🏽 ok", 9, 11, 0, "ok", "👍🏽 "),
        ("👍🏽 ok", 10, 11, 9, "👍🏽 k", "👍🏽 o"),
        ("👍🏽 ok", 11, 11, 9, "👍🏽 ", "👍🏽 ok"),
        ("👨\u{200d}👩\u{200d}👧 fam ily", 0, 19, 0, " fam ily", "fam ily"),
        ("👨\u{200d}👩\u{200d}👧 fam ily", 4, 19, 0, "\u{200d}👩\u{200d}👧 fam ily", "👨fam ily"),
        ("👨\u{200d}👩\u{200d}👧 fam ily", 18, 19, 0, " fam ily", "👨\u{200d}👩\u{200d}👧fam ily"),
        ("👨\u{200d}👩\u{200d}👧 fam ily", 19, 23, 0, "fam ily", "👨\u{200d}👩\u{200d}👧 ily"),
        ("👨\u{200d}👩\u{200d}👧 fam ily", 20, 23, 19, "👨\u{200d}👩\u{200d}👧 am ily", "👨\u{200d}👩\u{200d}👧 fily"),
        ("👨\u{200d}👩\u{200d}👧 fam ily", 23, 26, 19, "👨\u{200d}👩\u{200d}👧 ily", "👨\u{200d}👩\u{200d}👧 fam "),
        ("👨\u{200d}👩\u{200d}👧 fam ily", 26, 26, 23, "👨\u{200d}👩\u{200d}👧 fam ", "👨\u{200d}👩\u{200d}👧 fam ily"),
        ("(a+b)*c", 0, 1, 0, "a+b)*c", "a+b)*c"),
        ("(a+b)*c", 1, 2, 0, "a+b)*c", "(+b)*c"),
        ("(a+b)*c", 2, 3, 1, "(+b)*c", "(ab)*c"),
        ("(a+b)*c", 3, 4, 2, "(ab)*c", "(a+)*c"),
        ("(a+b)*c", 4, 6, 3, "(a+)*c", "(a+bc"),
        ("(a+b)*c", 5, 6, 4, "(a+b*c", "(a+b)c"),
        ("(a+b)*c", 6, 7, 4, "(a+bc", "(a+b)*"),
        ("(a+b)*c", 7, 7, 6, "(a+b)*", "(a+b)*c"),
        ("one\ttwo", 0, 4, 0, "ne\ttwo", "two"),
        ("one\ttwo", 3, 4, 0, "\ttwo", "onetwo"),
        ("one\ttwo", 4, 7, 0, "two", "one\t"),
        ("one\ttwo", 5, 7, 4, "one\two", "one\tt"),
        ("one\ttwo", 7, 7, 4, "one\t", "one\ttwo"),
        ("...", 0, 3, 0, "..", ""),
        ("...", 1, 3, 0, "..", "."),
        ("...", 3, 3, 0, "", "..."),
        ("a.b.c", 0, 1, 0, ".b.c", ".b.c"),
        ("a.b.c", 1, 2, 0, ".b.c", "ab.c"),
        ("a.b.c", 2, 3, 1, "ab.c", "a..c"),
        ("a.b.c", 3, 4, 2, "a..c", "a.bc"),
        ("a.b.c", 4, 5, 3, "a.bc", "a.b."),
        ("a.b.c", 5, 5, 4, "a.b.", "a.b.c"),
        ("x--y", 0, 1, 0, "--y", "--y"),
        ("x--y", 1, 3, 0, "--y", "xy"),
        ("x--y", 2, 3, 1, "x-y", "x-y"),
        ("x--y", 3, 4, 1, "xy", "x--"),
        ("x--y", 4, 4, 3, "x--", "x--y"),
    ];

    #[test]
    fn word_moves_match_qlineedit() {
        let mut checked = 0;
        for &(text, at, next, prev, after_back, after_fwd) in QT {
            if !is_grapheme_boundary(text, at) {
                continue;
            }
            checked += 1;
            let mut b = caret(text, at);
            b.apply_plain(mv(Dir::Forward, Unit::Word));
            assert_eq!(b.selection().focus, next, "Ctrl+Right in {text:?} from {at}");
            let mut b = caret(text, at);
            b.apply_plain(mv(Dir::Back, Unit::Word));
            assert_eq!(b.selection().focus, prev, "Ctrl+Left in {text:?} from {at}");

            // QLineEdit's Ctrl+Backspace is "extend a word back, then Delete", so where the word
            // move goes nowhere (the text start) it deletes the grapheme after the caret. We
            // delete nothing there instead; everywhere else the result must match.
            let mut b = caret(text, at);
            b.apply_plain(EditAction::Delete(Dir::Back, Unit::Word));
            let want = if prev == at { text } else { after_back };
            assert_eq!(b.text(), want, "Ctrl+Backspace in {text:?} at {at}");
            assert_eq!(b.selection().focus, prev);
            let mut b = caret(text, at);
            b.apply_plain(EditAction::Delete(Dir::Forward, Unit::Word));
            assert_eq!(b.text(), after_fwd, "Ctrl+Delete in {text:?} at {at}");
            assert_eq!(b.selection().focus, at);
        }
        assert!(checked >= 120, "{checked} rows checked");
    }

    #[test]
    fn word_delete_extends_a_selection() {
        // "hello |wor|ld": Ctrl+Backspace extends the selection back a word, then deletes it.
        let mut b = EditBuffer::new("hello world");
        b.set_selection(Selection { anchor: 9, focus: 6, affinity: Affinity::Downstream });
        b.apply_plain(EditAction::Delete(Dir::Back, Unit::Word));
        assert_eq!(b.text(), "ld");
    }

    #[test]
    fn grapheme_moves_collapse_a_selection() {
        let mut b = EditBuffer::new("abcdef");
        b.set_selection(Selection { anchor: 4, focus: 2, affinity: Affinity::Downstream });
        b.apply_plain(mv(Dir::Forward, Unit::Grapheme));
        assert_eq!(b.selection(), Selection::caret(4));
        b.set_selection(Selection { anchor: 4, focus: 2, affinity: Affinity::Downstream });
        b.apply_plain(mv(Dir::Back, Unit::Grapheme));
        assert_eq!(b.selection(), Selection::caret(2));
        // Shift extends from the focus instead.
        b.set_selection(Selection { anchor: 4, focus: 2, affinity: Affinity::Downstream });
        b.apply_plain(EditAction::MoveBy { dir: Dir::Back, unit: Unit::Grapheme, extend: true });
        assert_eq!(b.selection().range(), 1..4);
        assert_eq!(b.selected_text(), "bcd");
    }

    #[test]
    fn insert_replaces_the_selection() {
        let mut b = EditBuffer::new("their car");
        b.set_selection(Selection { anchor: 0, focus: 5, affinity: Affinity::Downstream });
        let e = b.apply_plain(EditAction::Insert("there".into())).unwrap();
        assert_eq!(e, EditEffect { range: 0..5, removed: "their".into(), inserted: "there".into() });
        assert_eq!(b.text(), "there car");
        assert_eq!(b.selection(), Selection::caret(5));
        assert_eq!(b.rev(), 1);
        // Moves do not change the revision; an empty insert with a caret does nothing.
        b.apply_plain(mv(Dir::Back, Unit::Text));
        assert_eq!(b.apply_plain(EditAction::Insert(String::new())), None);
        assert_eq!(b.rev(), 1);
        b.apply_plain(EditAction::SelectAll);
        assert_eq!(b.selected_text(), "there car");
        b.apply_plain(EditAction::Delete(Dir::Back, Unit::Grapheme));
        assert_eq!(b.text(), "");
        assert_eq!(b.rev(), 2);
    }

    #[test]
    fn home_end_and_text_edges() {
        let mut b = caret("one\ntwo three\nx", 7);
        b.apply_plain(mv(Dir::Back, Unit::Line));
        assert_eq!(b.selection().focus, 4);
        b.apply_plain(mv(Dir::Forward, Unit::Line));
        assert_eq!(b.selection().focus, 13);
        b.apply_plain(mv(Dir::Forward, Unit::Text));
        assert_eq!(b.selection().focus, 15);
        b.apply_plain(EditAction::MoveBy { dir: Dir::Back, unit: Unit::Text, extend: true });
        assert_eq!(b.selection().range(), 0..15);
    }

    /// A monospace soft wrap at `width` columns, breaking after spaces, as a Flow would.
    struct Wrap {
        text: String,
        lines: Vec<Range<usize>>,
    }

    impl Wrap {
        fn new(text: &str, width: usize) -> Wrap {
            let mut lines = Vec::new();
            let mut start = 0;
            let mut last_space = None;
            let mut at = 0;
            while at < text.len() {
                let next = next_grapheme(text, at);
                if &text[at..next] == " " {
                    last_space = Some(next);
                }
                if graphemes_between(text, start, next) > width {
                    if let Some(s) = last_space.filter(|&s| s > start) {
                        lines.push(start..s);
                        start = s;
                        last_space = None;
                        continue;
                    }
                }
                at = next;
            }
            lines.push(start..text.len());
            Wrap { text: text.into(), lines }
        }
    }

    impl Lines for Wrap {
        fn line_count(&self) -> usize {
            self.lines.len()
        }
        fn line_of(&self, byte: usize, affinity: Affinity) -> usize {
            let i = self.lines.iter().rposition(|r| r.start <= byte).unwrap_or(0);
            if affinity == Affinity::Upstream && i > 0 && self.lines[i].start == byte {
                i - 1
            } else {
                i
            }
        }
        fn line_range(&self, line: usize) -> Range<usize> {
            self.lines[line].clone()
        }
        fn x_of(&self, byte: usize, affinity: Affinity) -> f32 {
            let r = &self.lines[self.line_of(byte, affinity)];
            graphemes_between(&self.text, r.start, byte) as f32
        }
        fn stop_at_x(&self, line: usize, x: f32) -> (usize, Affinity) {
            let r = self.lines[line].clone();
            let (mut at, mut n) = (r.start, 0.0);
            while at < r.end && n + 0.5 <= x {
                at = next_grapheme(&self.text, at);
                n += 1.0;
            }
            let last = line + 1 < self.lines.len() && at == r.end;
            (at, if last { Affinity::Upstream } else { Affinity::Downstream })
        }
    }

    #[test]
    fn up_down_keep_a_sticky_x() {
        // Wrapped at 10: "the quick " / "brown fox " / "jumps over " / "a dog". A line's
        // trailing space may hang past the width.
        let text = "the quick brown fox jumps over a dog";
        let mut b = caret(text, 0);
        let step = |b: &mut EditBuffer, down: bool| {
            let w = Wrap::new(b.text(), 10);
            b.apply(EditAction::MoveLine { down, extend: false }, &w);
            b.selection().focus
        };
        let w = Wrap::new(text, 10);
        assert_eq!(w.lines, [0..10, 10..20, 20..31, 31..36]);
        // Start at column 8 of line 0 ("the quic|k").
        b.set_selection(Selection::caret(8));
        assert_eq!(step(&mut b, true), 18); // "brown fo|x"
        assert_eq!(step(&mut b, true), 28); // "jumps ov|er"
        assert_eq!(step(&mut b, true), 36); // "a dog" is shorter: its end
        assert_eq!(step(&mut b, true), 36); // Down on the last line: text end
        assert_eq!(step(&mut b, false), 28); // back to column 8, not 5
        assert_eq!(step(&mut b, false), 18);
        assert_eq!(step(&mut b, false), 8);
        assert_eq!(step(&mut b, false), 0); // Up on the first line: text start
        // A horizontal move forgets the goal x.
        b.set_selection(Selection::caret(8));
        step(&mut b, true);
        let w = Wrap::new(b.text(), 10);
        b.apply(mv(Dir::Back, Unit::Grapheme), &w);
        assert_eq!(b.selection().focus, 17);
        assert_eq!(step(&mut b, false), 7);
    }

    #[test]
    fn home_end_per_wrapped_line() {
        let text = "the quick brown fox";
        let w = Wrap::new(text, 10);
        let mut b = caret(text, 13);
        b.apply(mv(Dir::Back, Unit::Line), &w);
        assert_eq!(b.selection().focus, 10);
        b.apply(mv(Dir::Forward, Unit::Line), &w);
        assert_eq!(b.selection().focus, 19);
        // End on a soft-wrapped line stays on that line: upstream of the break.
        let mut b = caret(text, 2);
        b.apply(mv(Dir::Forward, Unit::Line), &w);
        assert_eq!(b.selection(), Selection { anchor: 10, focus: 10, affinity: Affinity::Upstream });
        assert_eq!(w.line_of(10, b.selection().affinity), 0);
        // Home from there goes to the start of line 0, not line 1.
        b.apply(mv(Dir::Back, Unit::Line), &w);
        assert_eq!(b.selection().focus, 0);
    }
}
