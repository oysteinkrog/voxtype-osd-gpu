//! sctk handlers: they turn Wayland events into `HostEvent`s on `State::events`, and keep the
//! per-surface frame and present bookkeeping.

use smithay_client_toolkit::compositor::CompositorHandler;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::presentation_time::{PresentTime, PresentationTimeHandler, PresentationTimeState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{
    PointerEvent as SctkPointer, PointerEventKind, PointerHandler, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{LayerShellHandler, LayerSurface, LayerSurfaceConfigure};
use smithay_client_toolkit::shell::xdg::window::{Window, WindowConfigure, WindowHandler};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_presentation_time,
    delegate_registry, delegate_seat, delegate_xdg_shell, delegate_xdg_window, registry_handlers,
};
use vo_host::{Button, HostCtl, HostEvent, Point, PointerEvent, PointerKind, Size};
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::{wl_output, wl_seat, wl_surface};
use wayland_client::{Connection, QueueHandle, WEnum};
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::{Kind, WpPresentationFeedback};

use smithay_client_toolkit::reexports::protocols as wayland_protocols;

use crate::{Role, State};

impl State {
    fn configured(&mut self, wl: &wl_surface::WlSurface, size: Option<(u32, u32)>) {
        let Some(i) = self.surface_index(wl) else { return };
        let first = !self.surfaces.iter().any(|s| s.configured);
        let s = &mut self.surfaces[i];
        if let Some((w, h)) = size.filter(|(w, h)| *w > 0 && *h > 0) {
            s.size = Size::new(w as f32, h as f32);
        }
        s.configured = true;
        s.dirty_config = true;
        let (id, size, scale) = (s.id, s.size, s.scale as f32);
        if first {
            self.stage("configured");
        }
        let t = self.now();
        self.events.push(HostEvent::Configured { id, t, size, scale });
    }

    fn closed(&mut self, wl: &wl_surface::WlSurface) {
        if let Some(i) = self.surface_index(wl) {
            let id = self.surfaces[i].id;
            self.remove(i);
            let t = self.now();
            self.events.push(HostEvent::Closed { id, t });
        }
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, wl: &wl_surface::WlSurface, factor: i32) {
        let Some(i) = self.surface_index(wl) else { return };
        let s = &mut self.surfaces[i];
        if s.scale == factor || factor < 1 {
            return;
        }
        s.scale = factor;
        // Integer scale only: fractional-scale-v1 is not bound yet (HostCaps::fractional_scale).
        let _ = match &s.role {
            Role::Layer(l) => l.set_buffer_scale(factor as u32),
            Role::Window(w) => w.set_buffer_scale(factor as u32),
        };
        if s.configured {
            self.configured(&wl.clone(), None);
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, wl: &wl_surface::WlSurface, _time: u32) {
        if let Some(i) = self.surface_index(wl) {
            self.surfaces[i].callback_pending = false;
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        State::closed(self, &layer.wl_surface().clone());
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        c: LayerSurfaceConfigure,
        _: u32,
    ) {
        self.configured(&layer.wl_surface().clone(), Some(c.new_size));
    }
}

impl WindowHandler for State {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window) {
        State::closed(self, &window.wl_surface().clone());
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window, c: WindowConfigure, _: u32) {
        let size = match c.new_size {
            (Some(w), Some(h)) => Some((w.get(), h.get())),
            _ => None,
        };
        self.configured(&window.wl_surface().clone(), size);
    }
}

impl PresentationTimeHandler for State {
    fn presentation_time_state(&mut self) -> &mut PresentationTimeState {
        &mut self.presentation
    }

    #[allow(clippy::too_many_arguments)]
    fn presented(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WpPresentationFeedback,
        wl: &wl_surface::WlSurface,
        _: Vec<wl_output::WlOutput>,
        time: PresentTime,
        refresh: u32,
        _: u64,
        _: WEnum<Kind>,
    ) {
        let Some(i) = self.surface_index(wl) else { return };
        let frame = self.surfaces[i].feedback.pop_front().unwrap_or(0);
        // Only a monotonic present clock can be compared with the host clock.
        if time.clk_id != libc::CLOCK_MONOTONIC as u32 {
            return;
        }
        let t = time.tv_sec as f64 + time.tv_nsec as f64 * 1e-9 - self.origin;
        let refresh = refresh as f64 * 1e-9;
        let s = &mut self.surfaces[i];
        s.last_present = Some(t);
        s.refresh = refresh;
        let id = s.id;
        if frame == 0 {
            self.diag.stages.push(("first frame on screen", t));
        }
        self.events.push(HostEvent::Presented { id, t, frame, refresh });
    }

    fn discarded(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WpPresentationFeedback,
        wl: &wl_surface::WlSurface,
    ) {
        if let Some(i) = self.surface_index(wl) {
            self.surfaces[i].feedback.pop_front();
        }
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer {
            if let Ok(p) = self.seats.get_pointer(qh, &seat) {
                self.pointers.push(p);
            }
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer {
            for p in self.pointers.drain(..) {
                p.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

fn button(code: u32) -> Option<Button> {
    match code {
        BTN_LEFT => Some(Button::Primary),
        BTN_RIGHT => Some(Button::Secondary),
        BTN_MIDDLE => Some(Button::Middle),
        _ => None,
    }
}

impl PointerHandler for State {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlPointer, events: &[SctkPointer]) {
        // One timestamp per pointer frame: the events in it happened together.
        let t = self.now();
        for e in events {
            let Some(i) = self.surface_index(&e.surface) else { continue };
            let kind = match e.kind {
                PointerEventKind::Enter { .. } => PointerKind::Enter,
                PointerEventKind::Leave { .. } => PointerKind::Leave,
                PointerEventKind::Motion { .. } => PointerKind::Motion,
                PointerEventKind::Press { button: b, .. } => match button(b) {
                    Some(b) => PointerKind::Down(b),
                    None => continue,
                },
                PointerEventKind::Release { button: b, .. } => match button(b) {
                    Some(b) => PointerKind::Up(b),
                    None => continue,
                },
                PointerEventKind::Axis { .. } => continue,
            };
            let pos = Point::new(e.position.0 as f32, e.position.1 as f32);
            self.events.push(HostEvent::Pointer(PointerEvent { surface: self.surfaces[i].id, t, pos, kind }));
        }
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(State);
delegate_output!(State);
delegate_layer!(State);
delegate_xdg_shell!(State);
delegate_xdg_window!(State);
delegate_seat!(State);
delegate_pointer!(State);
delegate_presentation_time!(State);
delegate_registry!(State);
