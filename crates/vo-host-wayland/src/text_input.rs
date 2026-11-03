//! Input methods (plan stage F7) through `zwp_text_input_v3` on the seat the keyboard comes
//! from.
//!
//! - The compositor enters the text input on the surface with keyboard focus. The host enables
//!   it only while that surface is a focus proxy the app turned it on for
//!   (`HostCtl::set_text_input`), and disables it when the app turns it off. A leave disables
//!   it on the compositor's side, so the next enter enables it again.
//! - The state goes as one commit: surrounding text, content type, change cause and the caret
//!   box. Later changes send only what changed, then commit again.
//! - The caret box is in the proxy's coordinates. The proxy sits at the output's top left, so
//!   that is the caret's place on the output: the popup's top left (from its placement) plus
//!   the caret's place on it.
//! - Events gather until `done` and go to the app as one `HostEvent::TextInput`.

use smithay_client_toolkit::reexports::protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use smithay_client_toolkit::reexports::protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    self, ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3,
};
use vo_host::{HostCtl, HostEvent, Preedit, SurfaceId, TextInputEvent, TextInputState};
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle};

use crate::State;

/// What the app turned the input method on with.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Want {
    pub(crate) proxy: SurfaceId,
    pub(crate) surface: WlSurface,
    pub(crate) state: TextInputState,
    /// The caret box in the proxy's coordinates: x, y, width, height.
    pub(crate) caret: Option<[i32; 4]>,
}

#[derive(Default)]
pub(crate) struct TextInput {
    manager: Option<ZwpTextInputManagerV3>,
    input: Option<ZwpTextInputV3>,
    /// The surface the text input is entered on, while it is.
    entered: Option<WlSurface>,
    want: Option<Want>,
    /// What the compositor has from the last commit since the enable, or `None` while
    /// disabled.
    sent: Option<(TextInputState, Option<[i32; 4]>)>,
    /// The last batch changed the text, so the next state is the input method's own change.
    ime_changed: bool,
    pending: TextInputEvent,
}

impl TextInput {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<State>) -> TextInput {
        TextInput { manager: globals.bind(qh, 1..=1, ()).ok(), ..TextInput::default() }
    }

    /// The seat the keyboard comes from is known: get its text input.
    pub(crate) fn seat(&mut self, qh: &QueueHandle<State>, seat: &WlSeat) {
        if let (Some(m), None) = (&self.manager, &self.input) {
            self.input = Some(m.get_text_input(seat, qh, ()));
        }
    }

    /// The seat lost its keyboard.
    pub(crate) fn seat_gone(&mut self) {
        if let Some(i) = self.input.take() {
            i.destroy();
        }
        self.entered = None;
        self.sent = None;
    }

    /// The app turned the input method on or changed its state (`Some`), or turned it off.
    pub(crate) fn set(&mut self, want: Option<Want>) {
        if self.want != want {
            self.want = want;
            self.sync();
        }
    }

    /// The proxy `id` closed: the input method goes off with it.
    pub(crate) fn proxy_closed(&mut self, id: SurfaceId) {
        if self.want.as_ref().is_some_and(|w| w.proxy == id) {
            self.set(None);
        }
    }

    /// Enables, updates or disables the text input to match what the app wants and where
    /// the compositor entered it.
    fn sync(&mut self) {
        let Some(input) = &self.input else { return };
        let active = self.want.as_ref().filter(|w| self.entered.as_ref() == Some(&w.surface));
        match (active, &self.sent) {
            (Some(w), sent) => {
                if sent.is_none() {
                    input.enable();
                    input.set_content_type(ContentHint::Multiline, ContentPurpose::Normal);
                }
                let (old_state, old_caret) = match sent {
                    Some((s, c)) => (Some(s), *c),
                    None => (None, None),
                };
                if old_state != Some(&w.state) {
                    let s = &w.state;
                    input.set_surrounding_text(s.surrounding.clone(), s.cursor as i32, s.anchor as i32);
                    let cause = if self.ime_changed { ChangeCause::InputMethod } else { ChangeCause::Other };
                    input.set_text_change_cause(cause);
                }
                if let Some([x, y, w_, h]) = w.caret.filter(|c| sent.is_none() || Some(*c) != old_caret) {
                    input.set_cursor_rectangle(x, y, w_, h);
                }
                if sent.is_none() || old_state != Some(&w.state) || w.caret != old_caret {
                    input.commit();
                    self.ime_changed = false;
                }
                self.sent = Some((w.state.clone(), w.caret));
            }
            (None, Some(_)) => {
                if self.entered.is_some() {
                    input.disable();
                    input.commit();
                }
                self.sent = None;
                self.pending = TextInputEvent::default();
            }
            (None, None) => {}
        }
    }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for State {
    fn event(
        _: &mut State,
        _: &ZwpTextInputManagerV3,
        _: <ZwpTextInputManagerV3 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<ZwpTextInputV3, ()> for State {
    fn event(
        state: &mut State,
        _: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let ti = &mut state.text_input;
        match event {
            zwp_text_input_v3::Event::Enter { surface } => {
                ti.entered = Some(surface);
                ti.sent = None;
                ti.sync();
            }
            zwp_text_input_v3::Event::Leave { .. } => {
                // The compositor disables it on a leave; the next enter enables it again.
                ti.entered = None;
                ti.sent = None;
                ti.pending = TextInputEvent::default();
            }
            zwp_text_input_v3::Event::PreeditString { text, cursor_begin, cursor_end } => {
                ti.pending.preedit = text.map(|text| {
                    let ok = |c: i32| usize::try_from(c).ok().filter(|&c| c <= text.len() && text.is_char_boundary(c));
                    let cursor = ok(cursor_begin).zip(ok(cursor_end));
                    Preedit { text, cursor }
                });
            }
            zwp_text_input_v3::Event::CommitString { text } => ti.pending.commit = text,
            zwp_text_input_v3::Event::DeleteSurroundingText { before_length, after_length } => {
                ti.pending.delete_before = before_length;
                ti.pending.delete_after = after_length;
            }
            zwp_text_input_v3::Event::Done { .. } => {
                let mut ev = std::mem::take(&mut ti.pending);
                if ti.sent.is_none() {
                    // Not enabled for a proxy the app wants it on: nothing to apply.
                    return;
                }
                ti.ime_changed |= ev.commit.is_some() || ev.delete_before > 0 || ev.delete_after > 0;
                ev.t = state.now();
                state.events.push(HostEvent::TextInput(ev));
            }
            _ => {}
        }
        let _ = state.conn.flush();
    }
}
