//! Keyboard input types shared by the host, the driver and the edit model
//! (`docs/INPUT-MODEL.md`, plan stage F1).
//!
//! A host turns its platform keyboard into [`KeyEvent`]s: the key as the layout reads it,
//! the raw key code, the text the key types (after compose and dead keys) and the modifier
//! state. Hosts send only `Down` and `Up`; key repeat is made by the framework's driver on
//! the host clock, so a replay gives the same repeats at any frame rate.
//!
//! These types live here, below the scene and the host, so the pure edit model can read them.

/// The surface a keyboard event is for. Same id space as the host's surfaces.
pub type KeySurface = u32;

/// One key press, repeat or release.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyEvent {
    pub surface: KeySurface,
    /// Host clock, seconds.
    pub t: f64,
    pub state: KeyState,
    /// The key as the active layout and the current modifiers read it.
    pub key: LogicalKey,
    /// The raw key code: the Linux evdev code on Wayland (xkb keycode minus 8). Stays the
    /// same whatever the layout, so it pairs a release with its press.
    pub code: u32,
    /// The text the key types, after dead keys and compose. `None` for a key that types
    /// nothing (arrows, a bare modifier, a dead key waiting for its next key) and on `Up`.
    pub text: Option<String>,
    /// Modifiers in effect for this key.
    pub mods: Mods,
}

impl KeyEvent {
    /// True for `Down` and `Repeat`.
    pub fn is_press(&self) -> bool {
        matches!(self.state, KeyState::Down | KeyState::Repeat)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyState {
    Down,
    /// Made by the driver while a key is held, never by a host.
    Repeat,
    Up,
}

/// A key as the layout reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicalKey {
    Named(NamedKey),
    /// A key whose keysym is a character, with Shift and the layout applied ('a', 'A',
    /// 'ø'). With Ctrl held this is still the letter, so Ctrl+Z reads as 'z' or 'Z'.
    Char(char),
    /// Any other keysym, as its raw xkb value.
    Other(u32),
}

/// The non-character keys the framework knows by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NamedKey {
    Escape,
    Enter,
    Tab,
    Backspace,
    Delete,
    Insert,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    AltLeft,
    AltRight,
    /// ISO level 3 shift, the AltGr key on most layouts.
    AltGr,
    SuperLeft,
    SuperRight,
    CapsLock,
    NumLock,
}

impl NamedKey {
    /// A key that only changes modifiers and types nothing by itself.
    pub fn is_modifier(self) -> bool {
        matches!(
            self,
            NamedKey::ShiftLeft
                | NamedKey::ShiftRight
                | NamedKey::ControlLeft
                | NamedKey::ControlRight
                | NamedKey::AltLeft
                | NamedKey::AltRight
                | NamedKey::AltGr
                | NamedKey::SuperLeft
                | NamedKey::SuperRight
                | NamedKey::CapsLock
                | NamedKey::NumLock
        )
    }
}

/// Modifier state. Locks are the latched state, the others are held or latched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// The Super (Windows) key.
    pub logo: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
}

impl Mods {
    pub const NONE: Mods =
        Mods { shift: false, ctrl: false, alt: false, logo: false, caps_lock: false, num_lock: false };
    pub const SHIFT: Mods = Mods { shift: true, ..Mods::NONE };
    pub const CTRL: Mods = Mods { ctrl: true, ..Mods::NONE };

    /// The same modifiers with the locks left out, for comparing shortcuts.
    pub fn held(self) -> Mods {
        Mods { caps_lock: false, num_lock: false, ..self }
    }
}

/// Key repeat as the compositor asks for it. `rate_hz` 0 means no repeat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RepeatInfo {
    /// Repeats per second once repeat has started.
    pub rate_hz: f32,
    /// Seconds from the press to the first repeat.
    pub delay_s: f32,
}

impl RepeatInfo {
    /// The time of repeat `n` (0 is the first) for a key pressed at `down`, or `None` when
    /// repeat is off.
    pub fn nth(&self, down: f64, n: u32) -> Option<f64> {
        (self.rate_hz > 0.0).then(|| down + self.delay_s as f64 + n as f64 / self.rate_hz as f64)
    }
}

impl Default for RepeatInfo {
    /// 25 per second after 600 ms, the usual desktop default.
    fn default() -> RepeatInfo {
        RepeatInfo { rate_hz: 25.0, delay_s: 0.6 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_times() {
        let r = RepeatInfo { rate_hz: 20.0, delay_s: 0.5 };
        assert_eq!(r.nth(1.0, 0), Some(1.5));
        assert!((r.nth(1.0, 2).unwrap() - 1.6).abs() < 1e-9);
        assert_eq!(RepeatInfo { rate_hz: 0.0, delay_s: 0.5 }.nth(1.0, 0), None);
    }

    #[test]
    fn held_mods_drop_locks() {
        let m = Mods { ctrl: true, caps_lock: true, num_lock: true, ..Mods::NONE };
        assert_eq!(m.held(), Mods::CTRL);
    }

    #[test]
    fn modifier_keys() {
        assert!(NamedKey::ShiftRight.is_modifier());
        assert!(NamedKey::AltGr.is_modifier());
        assert!(!NamedKey::Enter.is_modifier());
    }
}
