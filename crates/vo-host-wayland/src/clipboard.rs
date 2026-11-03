//! Clipboard (plan stage F5): copy and paste plain text through `wl_data_device` on the seat
//! the keyboard comes from.
//!
//! - Only an explicit copy sets the clipboard. The host never sets the primary selection, so
//!   selecting text never changes what a middle click or Shift+Insert pastes (R18).
//! - Copy needs the serial of an input event the surface got: the host keeps the newest key
//!   press or keyboard enter. With no serial yet (no key since focus came) the copy fails.
//! - The compositor sends the clipboard only to the client with keyboard focus, so paste works
//!   while the focus proxy (or a keyboard-interactive surface) has focus.
//! - Pipes are read and written on short-lived threads, so a slow or stuck peer never blocks
//!   the event loop. A paste is answered with `HostEvent::Clipboard` once the reader is done.

use std::io::{Read, Write};
use std::sync::Arc;

use smithay_client_toolkit::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::{CopyPasteSource, DataSourceHandler};
use smithay_client_toolkit::data_device_manager::{DataDeviceManagerState, WritePipe};
use smithay_client_toolkit::delegate_data_device;
use smithay_client_toolkit::reexports::calloop;
use vo_host::{HostCtl, HostError, HostEvent};
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_data_device::WlDataDevice;
use wayland_client::protocol::wl_data_device_manager::DndAction;
use wayland_client::protocol::wl_data_source::WlDataSource;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, QueueHandle};

use crate::State;

/// The type the host reads and the first one it offers.
pub(crate) const MIME: &str = "text/plain;charset=utf-8";
/// What a copy offers: UTF-8 text under the names apps ask for.
const OFFERED: [&str; 3] = [MIME, "text/plain", "UTF8_STRING"];
/// Pasted text past this is cut off: the clipboard is for text, not files.
const MAX_PASTE: u64 = 4 << 20;

#[derive(Default)]
pub(crate) struct Clipboard {
    manager: Option<DataDeviceManagerState>,
    device: Option<DataDevice>,
    /// The selection this client offers, and its text.
    source: Option<(CopyPasteSource, Arc<str>)>,
    /// The serial of the newest key press or keyboard enter.
    pub(crate) serial: Option<u32>,
    /// Hands finished pastes back to the event loop.
    pasted: Option<calloop::channel::Sender<Option<String>>>,
}

impl Clipboard {
    /// Binds `wl_data_device_manager` and adds the paste channel to the loop. Without the
    /// global the clipboard stays off and copy and paste return `Unsupported`.
    pub(crate) fn bind(
        globals: &GlobalList,
        qh: &QueueHandle<State>,
        handle: &calloop::LoopHandle<'static, State>,
    ) -> Clipboard {
        let Ok(manager) = DataDeviceManagerState::bind(globals, qh) else { return Clipboard::default() };
        let (tx, rx) = calloop::channel::channel::<Option<String>>();
        let inserted = handle.insert_source(rx, |ev, _, state: &mut State| {
            if let calloop::channel::Event::Msg(text) = ev {
                let t = state.now();
                state.events.push(HostEvent::Clipboard { t, text });
            }
        });
        Clipboard { manager: Some(manager), pasted: inserted.is_ok().then_some(tx), ..Clipboard::default() }
    }

    /// The seat the keyboard comes from is known: get its data device.
    pub(crate) fn seat(&mut self, qh: &QueueHandle<State>, seat: &WlSeat) {
        if let (Some(m), None) = (&self.manager, &self.device) {
            self.device = Some(m.get_data_device(qh, seat));
        }
    }

    /// The seat lost its keyboard: no more copy or paste through it.
    pub(crate) fn seat_gone(&mut self) {
        self.device = None;
        self.source = None;
        self.serial = None;
    }

    /// Offers `text` as the clipboard.
    pub(crate) fn copy(&mut self, qh: &QueueHandle<State>, text: String) -> Result<(), HostError> {
        let (Some(m), Some(device)) = (&self.manager, &self.device) else {
            return Err(HostError::Unsupported("clipboard: no wl_data_device".into()));
        };
        let Some(serial) = self.serial else {
            return Err(HostError::Unsupported("clipboard: no key press to copy with".into()));
        };
        let source = m.create_copy_paste_source(qh, OFFERED);
        source.set_selection(device, serial);
        self.source = Some((source, text.into()));
        Ok(())
    }

    /// Reads the clipboard; `HostEvent::Clipboard` answers. `None` text when the clipboard
    /// is empty or holds no UTF-8 text.
    pub(crate) fn paste(&mut self) -> Result<(), HostError> {
        let (Some(device), Some(tx)) = (&self.device, &self.pasted) else {
            return Err(HostError::Unsupported("clipboard: no wl_data_device".into()));
        };
        let tx = tx.clone();
        let offer = device.data().selection_offer();
        let pipe = offer.and_then(|o| {
            let has = o.with_mime_types(|types| types.iter().any(|t| t == MIME));
            has.then(|| o.receive(MIME.into()).ok()).flatten()
        });
        let Some(pipe) = pipe else {
            let _ = tx.send(None);
            return Ok(());
        };
        let fd: std::os::fd::OwnedFd = pipe.into();
        std::thread::Builder::new()
            .name("vo-paste".into())
            .spawn(move || {
                let mut bytes = Vec::new();
                let read = std::fs::File::from(fd).take(MAX_PASTE).read_to_end(&mut bytes);
                let _ = tx.send(read.ok().and_then(|_| String::from_utf8(bytes).ok()));
            })
            .map_err(HostError::Io)?;
        Ok(())
    }
}

impl DataDeviceHandler for State {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64, _: &WlSurface) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    /// sctk keeps the newest selection offer on the device; paste reads it then.
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for State {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

impl DataSourceHandler for State {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}

    /// Someone pastes this client's copy: write the text on a thread and close the pipe.
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource, _: String, fd: WritePipe) {
        let Some((_, text)) = self.clipboard.source.as_ref().filter(|(s, _)| s.inner() == source) else { return };
        let text = text.clone();
        let fd: std::os::fd::OwnedFd = fd.into();
        let _ = std::thread::Builder::new().name("vo-copy".into()).spawn(move || {
            let _ = std::fs::File::from(fd).write_all(text.as_bytes());
        });
    }

    /// Another client took the clipboard.
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self.clipboard.source.as_ref().is_some_and(|(s, _)| s.inner() == source) {
            self.clipboard.source = None;
        }
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

delegate_data_device!(State);
