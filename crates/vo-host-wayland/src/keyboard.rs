//! Keyboard: `wl_keyboard` events through libxkbcommon into `HostEvent`s (plan stage F1,
//! `docs/INPUT-MODEL.md`).
//!
//! The host keeps its own xkb state and compose state rather than sctk's keyboard, because a
//! focus loss must cancel a half-typed dead key or compose sequence, and sctk keeps its
//! compose state out of reach. Rules:
//! - Text comes from the compositor's keymap and modifier state, after dead keys and compose
//!   (the compose table of the locale in `LC_ALL`, `LC_CTYPE` or `LANG`).
//! - Keys already held when focus arrives send nothing, not even their release. Only a key
//!   that sent `Down` sends `Up`.
//! - Focus loss forgets every held key and resets compose. No `Up` follows for those keys.
//! - The host never repeats keys: the compositor's rate and delay go to the app as
//!   `KeyRepeatInfo`, and the framework's driver repeats on its own clock.

use std::ffi::OsString;
use std::os::fd::OwnedFd;

use vo_host::{HostEvent, KeyEvent, KeyState, LogicalKey, Mods, NamedKey, RepeatInfo, SurfaceId};
use xkbcommon::xkb::{self, compose, Keysym};

/// The keymap, its state and the compose state.
pub(crate) struct Xkb {
    state: xkb::State,
    compose: Option<compose::State>,
}

impl Xkb {
    fn new(context: &xkb::Context, keymap: xkb::Keymap, locale: &OsString) -> Xkb {
        let compose = compose::Table::new_from_locale(context, locale, compose::COMPILE_NO_FLAGS)
            .ok()
            .map(|table| compose::State::new(&table, compose::STATE_NO_FLAGS));
        let state = xkb::State::new(&keymap);
        Xkb { state, compose }
    }

    /// The keymap the compositor sent.
    fn from_fd(fd: OwnedFd, size: u32) -> Option<Xkb> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        // SAFETY: the fd comes from the compositor's keymap event, which promises a readable
        // keymap of `size` bytes; xkbcommon maps it private and read-only.
        let keymap = unsafe {
            xkb::Keymap::new_from_fd(&context, fd, size as usize, xkb::KEYMAP_FORMAT_TEXT_V1, xkb::COMPILE_NO_FLAGS)
        };
        Some(Xkb::new(&context, keymap.ok()??, &locale()))
    }

    /// A keymap from layout names, with the compose table of `locale`. For tests.
    #[cfg(test)]
    fn from_names(layout: &str, variant: &str, locale: &str) -> Option<Xkb> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap =
            xkb::Keymap::new_from_names(&context, "evdev", "pc105", layout, variant, None, xkb::COMPILE_NO_FLAGS)?;
        Some(Xkb::new(&context, keymap, &locale.into()))
    }

    fn mods(&self) -> Mods {
        let on = |name: &str| self.state.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE);
        Mods {
            shift: on(xkb::MOD_NAME_SHIFT),
            ctrl: on(xkb::MOD_NAME_CTRL),
            alt: on(xkb::MOD_NAME_ALT),
            logo: on(xkb::MOD_NAME_LOGO),
            caps_lock: on(xkb::MOD_NAME_CAPS),
            num_lock: on(xkb::MOD_NAME_NUM),
        }
    }

    /// The key and the text a press of evdev `code` makes. Feeds compose.
    fn press(&mut self, code: u32) -> (LogicalKey, Option<String>) {
        let keycode = xkb::Keycode::new(code + 8);
        let sym = self.state.key_get_one_sym(keycode);
        let text = match self.compose.as_mut() {
            Some(c) => match c.feed(sym) {
                xkb::FeedResult::Ignored => Some(self.state.key_get_utf8(keycode)),
                xkb::FeedResult::Accepted => match c.status() {
                    compose::Status::Composed => {
                        let s = c.utf8();
                        c.reset();
                        s
                    }
                    compose::Status::Nothing => Some(self.state.key_get_utf8(keycode)),
                    // Composing waits for the next key; Cancelled drops the sequence.
                    _ => None,
                },
            },
            None => Some(self.state.key_get_utf8(keycode)),
        };
        // Control characters (Enter, Tab, Backspace, Escape, Ctrl+letter) are keys, not text.
        let text = text.filter(|s| !s.is_empty() && !s.chars().any(char::is_control));
        (logical(sym), text)
    }

    fn key(&self, code: u32) -> LogicalKey {
        logical(self.state.key_get_one_sym(xkb::Keycode::new(code + 8)))
    }

    fn reset_compose(&mut self) {
        if let Some(c) = self.compose.as_mut() {
            c.reset();
        }
    }
}

/// The locale for the compose table, as libc would pick it.
fn locale() -> OsString {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|v| !v.is_empty())
        .unwrap_or_else(|| "C".into())
}

fn logical(sym: Keysym) -> LogicalKey {
    let named = match sym {
        Keysym::Escape => NamedKey::Escape,
        Keysym::Return | Keysym::KP_Enter | Keysym::ISO_Enter => NamedKey::Enter,
        Keysym::Tab | Keysym::ISO_Left_Tab => NamedKey::Tab,
        Keysym::BackSpace => NamedKey::Backspace,
        Keysym::Delete | Keysym::KP_Delete => NamedKey::Delete,
        Keysym::Insert | Keysym::KP_Insert => NamedKey::Insert,
        Keysym::Left | Keysym::KP_Left => NamedKey::Left,
        Keysym::Right | Keysym::KP_Right => NamedKey::Right,
        Keysym::Up | Keysym::KP_Up => NamedKey::Up,
        Keysym::Down | Keysym::KP_Down => NamedKey::Down,
        Keysym::Home | Keysym::KP_Home => NamedKey::Home,
        Keysym::End | Keysym::KP_End => NamedKey::End,
        Keysym::Prior | Keysym::KP_Prior => NamedKey::PageUp,
        Keysym::Next | Keysym::KP_Next => NamedKey::PageDown,
        Keysym::Shift_L => NamedKey::ShiftLeft,
        Keysym::Shift_R => NamedKey::ShiftRight,
        Keysym::Control_L => NamedKey::ControlLeft,
        Keysym::Control_R => NamedKey::ControlRight,
        Keysym::Alt_L | Keysym::Meta_L => NamedKey::AltLeft,
        Keysym::Alt_R | Keysym::Meta_R => NamedKey::AltRight,
        Keysym::ISO_Level3_Shift | Keysym::Mode_switch => NamedKey::AltGr,
        Keysym::Super_L => NamedKey::SuperLeft,
        Keysym::Super_R => NamedKey::SuperRight,
        Keysym::Caps_Lock => NamedKey::CapsLock,
        Keysym::Num_Lock => NamedKey::NumLock,
        _ => {
            return match char::from_u32(xkb::keysym_to_utf32(sym)) {
                Some(c) if c != '\0' && !c.is_control() => LogicalKey::Char(c),
                _ => LogicalKey::Other(sym.raw()),
            }
        }
    };
    LogicalKey::Named(named)
}

/// Keyboard state for one seat: the keymap, which of our surfaces has focus and which keys
/// went down while it had it.
#[derive(Default)]
pub(crate) struct Keyboard {
    xkb: Option<Xkb>,
    focus: Option<SurfaceId>,
    down: Vec<u32>,
    mods: Mods,
}

impl Keyboard {
    pub(crate) fn keymap(&mut self, fd: OwnedFd, size: u32) {
        self.xkb = Xkb::from_fd(fd, size);
        self.down.clear();
    }

    /// Focus came to `id`. Keys already held are ignored until they are pressed again.
    pub(crate) fn enter(&mut self, id: SurfaceId, t: f64, events: &mut Vec<HostEvent>) {
        self.focus = Some(id);
        self.down.clear();
        if let Some(x) = self.xkb.as_mut() {
            x.reset_compose();
        }
        events.push(HostEvent::KeyboardFocus { id, t, gained: true, requested: false });
    }

    /// Focus left. Held keys are forgotten and compose is reset. Returns the surface that
    /// had it, without sending anything: the caller decides whether the loss was asked for.
    pub(crate) fn leave(&mut self) -> Option<SurfaceId> {
        self.down.clear();
        if let Some(x) = self.xkb.as_mut() {
            x.reset_compose();
        }
        self.focus.take()
    }

    /// As `leave`, only when `id` has focus. True when it had.
    pub(crate) fn leave_if(&mut self, id: SurfaceId) -> bool {
        self.focus == Some(id) && self.leave().is_some()
    }

    pub(crate) fn key(&mut self, t: f64, code: u32, pressed: bool, events: &mut Vec<HostEvent>) {
        let (Some(surface), Some(x)) = (self.focus, self.xkb.as_mut()) else { return };
        let mods = self.mods;
        if pressed {
            if !self.down.contains(&code) {
                self.down.push(code);
            }
            let (key, text) = x.press(code);
            events.push(HostEvent::Key(KeyEvent { surface, t, state: KeyState::Down, key, code, text, mods }));
        } else if let Some(i) = self.down.iter().position(|&c| c == code) {
            self.down.swap_remove(i);
            let key = x.key(code);
            events.push(HostEvent::Key(KeyEvent { surface, t, state: KeyState::Up, key, code, text: None, mods }));
        }
    }

    pub(crate) fn modifiers(
        &mut self,
        t: f64,
        (depressed, latched, locked, group): (u32, u32, u32, u32),
        events: &mut Vec<HostEvent>,
    ) {
        let Some(x) = self.xkb.as_mut() else { return };
        x.state.update_mask(depressed, latched, locked, 0, 0, group);
        let mods = x.mods();
        if mods != self.mods {
            self.mods = mods;
            if let Some(id) = self.focus {
                events.push(HostEvent::Modifiers { id, t, mods });
            }
        }
    }

    pub(crate) fn repeat_info(t: f64, rate: i32, delay: i32) -> HostEvent {
        let info = RepeatInfo { rate_hz: rate.max(0) as f32, delay_s: delay.max(0) as f32 / 1000.0 };
        HostEvent::KeyRepeatInfo { t, info }
    }

    /// Whether the keymap says `code` repeats (letters do, modifiers do not).
    #[cfg(test)]
    fn repeats(&self, code: u32) -> bool {
        self.xkb.as_ref().is_some_and(|x| x.state.get_keymap().key_repeats(xkb::Keycode::new(code + 8)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // evdev codes.
    const KEY_A: u32 = 30;
    const KEY_E: u32 = 18;
    const KEY_APOSTROPHE: u32 = 40;
    const KEY_L: u32 = 38;
    const KEY_W: u32 = 17;
    const KEY_5: u32 = 6;
    const KEY_LEFTSHIFT: u32 = 42;
    const KEY_RIGHTALT: u32 = 100;
    const KEY_ENTER: u32 = 28;
    const KEY_LEFTCTRL: u32 = 29;

    /// A host keyboard plus a stand-in compositor that turns key presses into the modifier
    /// masks a real one would send.
    struct Rig {
        kb: Keyboard,
        server: xkb::State,
        events: Vec<HostEvent>,
        t: f64,
    }

    impl Rig {
        fn new(layout: &str, variant: &str) -> Rig {
            let xkb = Xkb::from_names(layout, variant, "en_US.UTF-8").expect("xkeyboard-config is installed");
            assert!(xkb.compose.is_some(), "the en_US.UTF-8 compose table is installed");
            let server = xkb::State::new(&xkb.state.get_keymap());
            Rig { kb: Keyboard { xkb: Some(xkb), ..Keyboard::default() }, server, events: Vec::new(), t: 0.0 }
        }

        fn key(&mut self, code: u32, pressed: bool) {
            self.t += 0.01;
            self.kb.key(self.t, code, pressed, &mut self.events);
            let dir = if pressed { xkb::KeyDirection::Down } else { xkb::KeyDirection::Up };
            if self.server.update_key(xkb::Keycode::new(code + 8), dir) != 0 {
                let s = &self.server;
                let mask = (
                    s.serialize_mods(xkb::STATE_MODS_DEPRESSED),
                    s.serialize_mods(xkb::STATE_MODS_LATCHED),
                    s.serialize_mods(xkb::STATE_MODS_LOCKED),
                    s.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
                );
                self.kb.modifiers(self.t, mask, &mut self.events);
            }
        }

        fn tap(&mut self, code: u32) {
            self.key(code, true);
            self.key(code, false);
        }

        /// The text of every key press so far, joined.
        fn typed(&self) -> String {
            self.events
                .iter()
                .filter_map(|e| match e {
                    HostEvent::Key(k) if k.state == KeyState::Down => k.text.clone(),
                    _ => None,
                })
                .collect()
        }

        fn keys(&self) -> Vec<(KeyState, LogicalKey)> {
            self.events
                .iter()
                .filter_map(|e| match e {
                    HostEvent::Key(k) => Some((k.state, k.key)),
                    _ => None,
                })
                .collect()
        }
    }

    #[test]
    fn altgr_dead_acute_then_e_types_e_acute() {
        // us(altgr-intl), the maintainer's layout: AltGr+' is dead acute.
        let mut r = Rig::new("us", "altgr-intl");
        r.kb.enter(1, 0.0, &mut r.events);
        r.key(KEY_RIGHTALT, true);
        r.tap(KEY_APOSTROPHE);
        r.key(KEY_RIGHTALT, false);
        assert_eq!(r.typed(), "", "a dead key types nothing yet");
        r.tap(KEY_E);
        assert_eq!(r.typed(), "é");
        // AltGr letters: ø on AltGr+L, å on AltGr+W, € on AltGr+5.
        r.key(KEY_RIGHTALT, true);
        r.tap(KEY_L);
        r.tap(KEY_W);
        r.tap(KEY_5);
        r.key(KEY_RIGHTALT, false);
        assert_eq!(r.typed(), "éøå€");
        // The AltGr key itself is a named key with no text.
        assert!(r.keys().contains(&(KeyState::Down, LogicalKey::Named(NamedKey::AltGr))));
    }

    #[test]
    fn shift_and_controls() {
        let mut r = Rig::new("us", "altgr-intl");
        r.kb.enter(1, 0.0, &mut r.events);
        r.key(KEY_LEFTSHIFT, true);
        r.tap(KEY_A);
        r.key(KEY_LEFTSHIFT, false);
        r.tap(KEY_A);
        r.tap(KEY_ENTER);
        r.key(KEY_LEFTCTRL, true);
        r.tap(KEY_A);
        r.key(KEY_LEFTCTRL, false);
        assert_eq!(r.typed(), "Aa", "Enter and Ctrl+A type nothing");
        let ctrl_a = r.events.iter().rev().find_map(|e| match e {
            HostEvent::Key(k) if k.state == KeyState::Down && k.code == KEY_A => Some(k.clone()),
            _ => None,
        });
        let ctrl_a = ctrl_a.unwrap();
        assert_eq!(ctrl_a.key, LogicalKey::Char('a'));
        assert!(ctrl_a.mods.ctrl);
        assert!(r.keys().contains(&(KeyState::Down, LogicalKey::Named(NamedKey::Enter))));
        // Modifier changes reach the app.
        assert!(r.events.iter().any(|e| matches!(e, HostEvent::Modifiers { mods, .. } if mods.shift)));
    }

    #[test]
    fn keys_held_at_enter_send_nothing() {
        let mut r = Rig::new("us", "");
        // 'a' goes down before focus arrives (the server sees it, the surface does not).
        r.key(KEY_A, true);
        assert!(r.events.is_empty());
        r.kb.enter(1, 0.0, &mut r.events);
        r.key(KEY_A, false);
        assert_eq!(r.keys(), [], "no release for a key pressed before focus");
        r.tap(KEY_A);
        assert_eq!(r.keys(), [(KeyState::Down, LogicalKey::Char('a')), (KeyState::Up, LogicalKey::Char('a'))]);
    }

    #[test]
    fn leave_forgets_held_keys_and_compose() {
        let mut r = Rig::new("us", "altgr-intl");
        r.kb.enter(1, 0.0, &mut r.events);
        r.key(KEY_RIGHTALT, true);
        r.tap(KEY_APOSTROPHE);
        r.key(KEY_RIGHTALT, false);
        r.key(KEY_A, true);
        assert_eq!(r.kb.leave(), Some(1));
        // The release after the leave goes nowhere, and keys without focus send nothing.
        let before = r.events.len();
        r.key(KEY_A, false);
        r.tap(KEY_E);
        assert_eq!(r.events.len(), before);
        // Back in focus, the dead acute is gone: 'e' is a plain e.
        r.kb.enter(1, 1.0, &mut r.events);
        r.tap(KEY_E);
        assert!(r.typed().ends_with('e') && !r.typed().contains('é'), "{:?}", r.typed());
    }

    #[test]
    fn repeat_info_in_seconds() {
        let HostEvent::KeyRepeatInfo { info, .. } = Keyboard::repeat_info(0.0, 25, 600) else { panic!() };
        assert_eq!(info, RepeatInfo { rate_hz: 25.0, delay_s: 0.6 });
        let HostEvent::KeyRepeatInfo { info, .. } = Keyboard::repeat_info(0.0, 0, 600) else { panic!() };
        assert_eq!(info.nth(0.0, 0), None, "rate 0 means no repeat");
    }

    #[test]
    fn keymap_says_which_keys_repeat() {
        let r = Rig::new("us", "");
        assert!(r.kb.repeats(KEY_A));
        assert!(!r.kb.repeats(KEY_LEFTSHIFT));
    }
}
