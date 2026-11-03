//! Cursor shapes through `wp_cursor_shape_v1` (plan stage F6): the compositor draws its own
//! theme's cursor for a shape name, so the host needs no cursor theme. A compositor without
//! the protocol keeps whatever cursor it shows.

use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use vo_host::{CursorShape, SurfaceId};
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::QueueHandle;

use crate::State;

#[derive(Default)]
pub(crate) struct Cursors {
    manager: Option<CursorShapeManager>,
    devices: Vec<WpCursorShapeDeviceV1>,
    /// The surface the pointer is over, and the serial of its enter.
    over: Option<(SurfaceId, u32)>,
}

impl Cursors {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<State>) -> Cursors {
        Cursors { manager: CursorShapeManager::bind(globals, qh).ok(), ..Cursors::default() }
    }

    /// A new pointer: give it a shape device.
    pub(crate) fn pointer(&mut self, qh: &QueueHandle<State>, pointer: &WlPointer) {
        if let Some(m) = &self.manager {
            self.devices.push(m.get_shape_device(pointer, qh));
        }
    }

    /// The pointers went away.
    pub(crate) fn pointers_gone(&mut self) {
        for d in self.devices.drain(..) {
            d.destroy();
        }
        self.over = None;
    }

    /// The pointer entered surface `id` with `serial`: show `shape` there.
    pub(crate) fn enter(&mut self, id: SurfaceId, serial: u32, shape: CursorShape) {
        self.over = Some((id, serial));
        self.show(shape);
    }

    pub(crate) fn leave(&mut self) {
        self.over = None;
    }

    /// Surface `id` wants `shape`: shown at once when the pointer is over it.
    pub(crate) fn set(&mut self, id: SurfaceId, shape: CursorShape) {
        if self.over.is_some_and(|(over, _)| over == id) {
            self.show(shape);
        }
    }

    fn show(&self, shape: CursorShape) {
        let Some((_, serial)) = self.over else { return };
        let shape = match shape {
            CursorShape::Default => Shape::Default,
            CursorShape::Text => Shape::Text,
            CursorShape::Pointer => Shape::Pointer,
        };
        for d in &self.devices {
            d.set_shape(serial, shape);
        }
    }
}
