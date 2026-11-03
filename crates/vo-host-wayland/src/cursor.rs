//! Cursor shapes. With `wp_cursor_shape_v1` (plan stage F6) the compositor draws its own
//! theme's cursor for a shape name. Without it, the host draws the shape from the XCursor
//! theme (`XCURSOR_THEME`, `XCURSOR_SIZE`, default 24 px) on a cursor surface of its own and
//! sets that surface on each pointer.
//!
//! `VO_NO_CURSOR_SHAPE=1` ignores the shape global, so a nested test can check the fallback on
//! a compositor that has it.

use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use vo_host::{CursorShape, SurfaceId};
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy, QueueHandle};
use wayland_cursor::CursorTheme;

use crate::State;

#[derive(Default)]
pub(crate) struct Cursors {
    manager: Option<CursorShapeManager>,
    devices: Vec<WpCursorShapeDeviceV1>,
    /// Set when there is no shape manager but there is `wl_shm`.
    themed: Option<Themed>,
    /// The surface the pointer is over, the serial of its enter, and that surface's integer
    /// scale (rounded up), which the themed cursor is drawn at.
    over: Option<(SurfaceId, u32, u32)>,
}

/// The fallback: cursor images from the XCursor theme on a surface of our own.
struct Themed {
    conn: Connection,
    shm: WlShm,
    surface: WlSurface,
    pointers: Vec<WlPointer>,
    /// The theme loaded for this scale. Loaded on the first cursor shown, not at connect.
    theme: Option<(u32, CursorTheme)>,
    /// Shapes already named in the diagnostics notes, so the notes stay short.
    noted: Vec<CursorShape>,
}

impl Cursors {
    pub(crate) fn bind(
        globals: &GlobalList,
        qh: &QueueHandle<State>,
        conn: &Connection,
        compositor: &CompositorState,
        shm: Option<&WlShm>,
    ) -> Cursors {
        let hidden = std::env::var("VO_NO_CURSOR_SHAPE").is_ok_and(|v| v == "1");
        let manager = if hidden { None } else { CursorShapeManager::bind(globals, qh).ok() };
        let themed = match (&manager, shm) {
            (None, Some(shm)) => Some(Themed {
                conn: conn.clone(),
                shm: shm.clone(),
                surface: compositor.create_surface(qh),
                pointers: Vec::new(),
                theme: None,
                noted: Vec::new(),
            }),
            _ => None,
        };
        Cursors { manager, themed, ..Cursors::default() }
    }

    /// A new pointer: give it a shape device, or remember it for the themed cursor.
    pub(crate) fn pointer(&mut self, qh: &QueueHandle<State>, pointer: &WlPointer) {
        if let Some(m) = &self.manager {
            self.devices.push(m.get_shape_device(pointer, qh));
        } else if let Some(t) = &mut self.themed {
            t.pointers.push(pointer.clone());
        }
    }

    /// The pointers went away.
    pub(crate) fn pointers_gone(&mut self) {
        for d in self.devices.drain(..) {
            d.destroy();
        }
        if let Some(t) = &mut self.themed {
            t.pointers.clear();
        }
        self.over = None;
    }

    /// The pointer entered surface `id` (integer scale `scale`) with `serial`: show `shape`.
    pub(crate) fn enter(
        &mut self,
        id: SurfaceId,
        serial: u32,
        scale: u32,
        shape: CursorShape,
        notes: &mut Vec<String>,
    ) {
        self.over = Some((id, serial, scale.max(1)));
        self.show(shape, notes);
    }

    pub(crate) fn leave(&mut self) {
        self.over = None;
    }

    /// Surface `id` wants `shape`: shown at once when the pointer is over it.
    pub(crate) fn set(&mut self, id: SurfaceId, shape: CursorShape, notes: &mut Vec<String>) {
        if self.over.is_some_and(|(over, _, _)| over == id) {
            self.show(shape, notes);
        }
    }

    fn show(&mut self, shape: CursorShape, notes: &mut Vec<String>) {
        let Some((_, serial, scale)) = self.over else { return };
        if let Some(t) = &mut self.themed {
            t.show(serial, scale, shape, notes);
            return;
        }
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

/// XCursor names for a shape: the CSS name first, then the older X11 name.
fn names(shape: CursorShape) -> &'static [&'static str] {
    match shape {
        CursorShape::Default => &["default", "left_ptr"],
        CursorShape::Text => &["text", "xterm"],
        CursorShape::Pointer => &["pointer", "hand2", "hand1"],
    }
}

impl Themed {
    fn show(&mut self, serial: u32, scale: u32, shape: CursorShape, notes: &mut Vec<String>) {
        if self.theme.as_ref().is_none_or(|(k, _)| *k != scale) {
            let size = std::env::var("XCURSOR_SIZE").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(24);
            match CursorTheme::load(&self.conn, self.shm.clone(), size * scale) {
                Ok(theme) => self.theme = Some((scale, theme)),
                Err(e) => {
                    notes.push(format!("cursor: no cursor theme ({e})"));
                    return;
                }
            }
        }
        let Some((_, theme)) = &mut self.theme else { return };
        // Two steps: a loop that returns the borrowed cursor does not pass the borrow checker.
        let name = names(shape).iter().copied().find(|n| theme.get_cursor(n).is_some());
        let note = !self.noted.contains(&shape);
        let Some((name, cursor)) = name.and_then(|n| Some((n, theme.get_cursor(n)?))) else {
            if note {
                self.noted.push(shape);
                notes.push(format!("cursor: the cursor theme has no {:?} cursor", names(shape)));
            }
            return;
        };
        let image = &cursor[0];
        let (w, h) = image.dimensions();
        let (hx, hy) = image.hotspot();
        let k = scale as i32;
        self.surface.set_buffer_scale(k);
        self.surface.attach(Some(image), 0, 0);
        if self.surface.version() >= 4 {
            self.surface.damage_buffer(0, 0, w as i32, h as i32);
        } else {
            self.surface.damage(0, 0, w as i32 / k, h as i32 / k);
        }
        self.surface.commit();
        for p in &self.pointers {
            p.set_cursor(serial, Some(&self.surface), hx as i32 / k, hy as i32 / k);
        }
        if note {
            self.noted.push(shape);
            notes.push(format!("cursor: no wp_cursor_shape_v1, {shape:?} drawn from the cursor theme as \"{name}\" ({w}x{h} at scale {scale})"));
        }
    }
}
