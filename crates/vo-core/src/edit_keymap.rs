//! The text box keymap: which key does what to an [`EditBuffer`](crate::edit::EditBuffer)
//! (plan section 2.3, the Settling and Editing column).
//!
//! | Key | Command |
//! |---|---|
//! | Esc | collapse the selection if there is one, else resume (on press) |
//! | Enter | finish (on release) |
//! | Shift+Enter | insert a line break |
//! | Left, Right | one grapheme, or collapse the selection to that side |
//! | Ctrl+Left, Ctrl+Right | one word (Qt rule) |
//! | Up, Down | one wrapped line, keeping x |
//! | Home, End; Ctrl+Home, Ctrl+End | wrapped line start or end; text start or end |
//! | Shift + any move | extend the selection |
//! | Ctrl+A | select all |
//! | Backspace, Delete (Ctrl: by word) | delete the selection, or a grapheme or word |
//! | Ctrl+Z; Ctrl+Shift+Z, Ctrl+Y | undo; redo |
//! | a key that types text (AltGr and dead keys included) | insert it |
//! | Tab, Ctrl+C, Ctrl+X, Ctrl+V, a bare modifier, Alt+anything | nothing |
//!
//! Up and Down do nothing while both Ctrl keys are held: Left Ctrl + Right Ctrl + Up/Down
//! steps the look, and the daemon owns that chord.

use crate::edit::{Dir, EditAction, Unit};
use crate::key::{KeyEvent, KeyState, LogicalKey, NamedKey};

/// What a key asks of the text box.
#[derive(Clone, Debug, PartialEq)]
pub enum EditCmd {
    /// Change the text or the selection.
    Edit(EditAction),
    /// Collapse the selection to its focus.
    Collapse,
    /// Leave editing and listen again (Esc with no selection).
    Resume,
    /// Finish and send (Enter, on release).
    Finish,
}

/// Turns key events into text box commands. Keeps the little state it needs: which Ctrl keys
/// are held, for the look chord.
#[derive(Clone, Debug, Default)]
pub struct TextBoxKeys {
    ctrl_left: bool,
    ctrl_right: bool,
}

impl TextBoxKeys {
    /// Forgets held keys, for a focus loss.
    pub fn reset(&mut self) {
        *self = TextBoxKeys::default();
    }

    /// The command for `ev`, or `None` for a key the text box does not use. `selection` is
    /// whether the buffer has a non-empty selection.
    pub fn map(&mut self, ev: &KeyEvent, selection: bool) -> Option<EditCmd> {
        let down = ev.state != KeyState::Up;
        match ev.key {
            LogicalKey::Named(NamedKey::ControlLeft) => self.ctrl_left = down,
            LogicalKey::Named(NamedKey::ControlRight) => self.ctrl_right = down,
            _ => {}
        }
        let m = ev.mods.held();
        let (shift, ctrl) = (m.shift, m.ctrl);
        if ev.state == KeyState::Up {
            // Enter finishes on release, so its release cannot reach the target app.
            return (ev.key == LogicalKey::Named(NamedKey::Enter) && !shift && !ctrl && !m.alt)
                .then_some(EditCmd::Finish);
        }
        let mv = |dir, unit| Some(EditCmd::Edit(EditAction::MoveBy { dir, unit, extend: shift }));
        let by_word = |ctrl: bool| if ctrl { Unit::Word } else { Unit::Grapheme };
        match ev.key {
            LogicalKey::Named(named) => match named {
                NamedKey::Escape if ev.state == KeyState::Down => {
                    Some(if selection { EditCmd::Collapse } else { EditCmd::Resume })
                }
                NamedKey::Enter if shift && !ctrl => Some(EditCmd::Edit(EditAction::Insert("\n".into()))),
                NamedKey::Left => mv(Dir::Back, by_word(ctrl)),
                NamedKey::Right => mv(Dir::Forward, by_word(ctrl)),
                NamedKey::Up | NamedKey::Down if self.ctrl_left && self.ctrl_right => None,
                NamedKey::Up | NamedKey::Down => {
                    Some(EditCmd::Edit(EditAction::MoveLine { down: named == NamedKey::Down, extend: shift }))
                }
                NamedKey::Home => mv(Dir::Back, if ctrl { Unit::Text } else { Unit::Line }),
                NamedKey::End => mv(Dir::Forward, if ctrl { Unit::Text } else { Unit::Line }),
                NamedKey::Backspace => Some(EditCmd::Edit(EditAction::Delete(Dir::Back, by_word(ctrl)))),
                NamedKey::Delete => Some(EditCmd::Edit(EditAction::Delete(Dir::Forward, by_word(ctrl)))),
                _ => None,
            },
            LogicalKey::Char(c) if ctrl && !m.alt => match c.to_ascii_lowercase() {
                'a' => Some(EditCmd::Edit(EditAction::SelectAll)),
                'z' if shift => Some(EditCmd::Edit(EditAction::Redo)),
                'z' => Some(EditCmd::Edit(EditAction::Undo)),
                'y' => Some(EditCmd::Edit(EditAction::Redo)),
                _ => None,
            },
            _ if ctrl || m.alt || m.logo => None,
            _ => ev.text.as_ref().map(|t| EditCmd::Edit(EditAction::Insert(t.clone()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::Mods;

    const NONE: Mods = Mods::NONE;
    const SHIFT: Mods = Mods::SHIFT;
    const CTRL: Mods = Mods::CTRL;
    const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, ..Mods::NONE };
    const ALT: Mods = Mods { alt: true, ..Mods::NONE };

    fn ev(state: KeyState, key: LogicalKey, text: Option<&str>, mods: Mods) -> KeyEvent {
        KeyEvent { surface: 1, t: 0.0, state, key, code: 0, text: text.map(str::to_owned), mods }
    }

    fn press(key: LogicalKey, text: Option<&str>, mods: Mods) -> KeyEvent {
        ev(KeyState::Down, key, text, mods)
    }

    fn named(k: NamedKey) -> LogicalKey {
        LogicalKey::Named(k)
    }

    fn edit(a: EditAction) -> Option<EditCmd> {
        Some(EditCmd::Edit(a))
    }

    fn by(dir: Dir, unit: Unit, extend: bool) -> Option<EditCmd> {
        edit(EditAction::MoveBy { dir, unit, extend })
    }

    #[test]
    fn every_editing_row_of_table_2_3() {
        use NamedKey::*;
        let rows: Vec<(&str, KeyEvent, bool, Option<EditCmd>)> = vec![
            ("Esc, no selection: resume", press(named(Escape), None, NONE), false, Some(EditCmd::Resume)),
            ("Esc with a selection: collapse", press(named(Escape), None, NONE), true, Some(EditCmd::Collapse)),
            ("Enter press: nothing yet", press(named(Enter), None, NONE), false, None),
            ("Enter release: finish", ev(KeyState::Up, named(Enter), None, NONE), false, Some(EditCmd::Finish)),
            ("Shift+Enter: newline", press(named(Enter), None, SHIFT), false, edit(EditAction::Insert("\n".into()))),
            ("Shift+Enter release: nothing", ev(KeyState::Up, named(Enter), None, SHIFT), false, None),
            ("Left", press(named(Left), None, NONE), false, by(Dir::Back, Unit::Grapheme, false)),
            ("Right", press(named(Right), None, NONE), true, by(Dir::Forward, Unit::Grapheme, false)),
            ("Ctrl+Left", press(named(Left), None, CTRL), false, by(Dir::Back, Unit::Word, false)),
            ("Ctrl+Right", press(named(Right), None, CTRL), false, by(Dir::Forward, Unit::Word, false)),
            ("Up", press(named(Up), None, NONE), false, edit(EditAction::MoveLine { down: false, extend: false })),
            ("Down", press(named(Down), None, NONE), false, edit(EditAction::MoveLine { down: true, extend: false })),
            ("Home", press(named(Home), None, NONE), false, by(Dir::Back, Unit::Line, false)),
            ("End", press(named(End), None, NONE), false, by(Dir::Forward, Unit::Line, false)),
            ("Ctrl+Home", press(named(Home), None, CTRL), false, by(Dir::Back, Unit::Text, false)),
            ("Ctrl+End", press(named(End), None, CTRL), false, by(Dir::Forward, Unit::Text, false)),
            ("Shift+Left", press(named(Left), None, SHIFT), false, by(Dir::Back, Unit::Grapheme, true)),
            ("Ctrl+Shift+Right", press(named(Right), None, CTRL_SHIFT), false, by(Dir::Forward, Unit::Word, true)),
            (
                "Shift+Down",
                press(named(Down), None, SHIFT),
                false,
                edit(EditAction::MoveLine { down: true, extend: true }),
            ),
            ("Shift+End", press(named(End), None, SHIFT), false, by(Dir::Forward, Unit::Line, true)),
            ("Ctrl+A", press(LogicalKey::Char('a'), None, CTRL), false, edit(EditAction::SelectAll)),
            (
                "Backspace",
                press(named(Backspace), None, NONE),
                false,
                edit(EditAction::Delete(Dir::Back, Unit::Grapheme)),
            ),
            ("Delete", press(named(Delete), None, NONE), false, edit(EditAction::Delete(Dir::Forward, Unit::Grapheme))),
            (
                "Ctrl+Backspace",
                press(named(Backspace), None, CTRL),
                false,
                edit(EditAction::Delete(Dir::Back, Unit::Word)),
            ),
            (
                "Ctrl+Delete",
                press(named(Delete), None, CTRL),
                false,
                edit(EditAction::Delete(Dir::Forward, Unit::Word)),
            ),
            ("Ctrl+Z", press(LogicalKey::Char('z'), None, CTRL), false, edit(EditAction::Undo)),
            ("Ctrl+Shift+Z", press(LogicalKey::Char('Z'), None, CTRL_SHIFT), false, edit(EditAction::Redo)),
            ("Ctrl+Y", press(LogicalKey::Char('y'), None, CTRL), false, edit(EditAction::Redo)),
            ("a", press(LogicalKey::Char('a'), Some("a"), NONE), false, edit(EditAction::Insert("a".into()))),
            ("Shift+b", press(LogicalKey::Char('B'), Some("B"), SHIFT), true, edit(EditAction::Insert("B".into()))),
            ("AltGr+L", press(LogicalKey::Char('ø'), Some("ø"), NONE), false, edit(EditAction::Insert("ø".into()))),
            ("dead acute: nothing yet", press(LogicalKey::Other(0xfe51), None, NONE), false, None),
            (
                "dead acute then e",
                press(LogicalKey::Char('e'), Some("é"), NONE),
                false,
                edit(EditAction::Insert("é".into())),
            ),
            ("Tab: ignored", press(named(Tab), None, NONE), false, None),
            ("Ctrl+C: ignored", press(LogicalKey::Char('c'), None, CTRL), true, None),
            ("Ctrl+X: ignored", press(LogicalKey::Char('x'), None, CTRL), true, None),
            ("Ctrl+V: ignored", press(LogicalKey::Char('v'), None, CTRL), false, None),
            ("Alt+a: not text", press(LogicalKey::Char('a'), Some("a"), ALT), false, None),
            ("bare Right Shift", press(named(ShiftRight), None, NONE), false, None),
            ("bare Right Shift release", ev(KeyState::Up, named(ShiftRight), None, SHIFT), false, None),
            ("bare Left Shift", press(named(ShiftLeft), None, NONE), false, None),
            ("bare Right Ctrl", press(named(ControlRight), None, NONE), false, None),
            ("a release", ev(KeyState::Up, LogicalKey::Char('a'), None, NONE), false, None),
        ];
        for (name, e, selection, want) in rows {
            let mut keys = TextBoxKeys::default();
            assert_eq!(keys.map(&e, selection), want, "{name}");
        }
    }

    #[test]
    fn repeats_act_like_presses_except_esc() {
        let mut keys = TextBoxKeys::default();
        let rep = |k, mods| ev(KeyState::Repeat, k, None, mods);
        assert_eq!(keys.map(&rep(named(NamedKey::Left), NONE), false), by(Dir::Back, Unit::Grapheme, false));
        assert_eq!(
            keys.map(&ev(KeyState::Repeat, LogicalKey::Char('a'), Some("a"), NONE), false),
            edit(EditAction::Insert("a".into()))
        );
        assert_eq!(keys.map(&rep(named(NamedKey::Escape), NONE), false), None);
        assert_eq!(keys.map(&rep(named(NamedKey::Enter), NONE), false), None);
    }

    #[test]
    fn up_and_down_are_left_to_the_look_chord_while_both_ctrls_are_held() {
        let mut keys = TextBoxKeys::default();
        keys.map(&press(named(NamedKey::ControlLeft), None, NONE), false);
        keys.map(&press(named(NamedKey::ControlRight), None, CTRL), false);
        assert_eq!(keys.map(&press(named(NamedKey::Up), None, CTRL), false), None);
        assert_eq!(keys.map(&press(named(NamedKey::Down), None, CTRL), false), None);
        keys.map(&ev(KeyState::Up, named(NamedKey::ControlRight), None, CTRL), false);
        assert_eq!(
            keys.map(&press(named(NamedKey::Up), None, CTRL), false),
            edit(EditAction::MoveLine { down: false, extend: false })
        );
    }
}
