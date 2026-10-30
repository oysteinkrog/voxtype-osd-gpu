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
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_presentation_time,
    delegate_registry, delegate_seat, delegate_shm, delegate_subcompositor, delegate_xdg_shell, delegate_xdg_window,
    registry_handlers,
};
use vo_host::{Button, HostCtl, HostEvent, Point, PointerEvent, PointerKind, Size};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::{wl_output, wl_seat, wl_surface};
use wayland_client::{Connection, QueueHandle, WEnum};
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::{Kind, WpPresentationFeedback};

use smithay_client_toolkit::reexports::protocols as wayland_protocols;

use wayland_client::Dispatch;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{self, WpFractionalScaleV1};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::{Role, State, Surf};

impl Surf {
    /// The oldest frame waiting for present feedback on `wl`.
    fn take_feedback(&mut self, wl: &wl_surface::WlSurface) -> Option<u64> {
        let on_child = self.is_child(wl);
        let i = self.feedback.iter().position(|f| f.0 == on_child)?;
        self.feedback.remove(i).map(|f| f.1)
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        self.shm.as_mut().expect("wl_shm events come only once it is bound")
    }
}

impl State {
    fn configured(&mut self, wl: &wl_surface::WlSurface, size: Option<(u32, u32)>) {
        let Some(i) = self.surface_index(wl) else { return };
        let first = !self.surfaces.iter().any(|s| s.configured);
        let s = &mut self.surfaces[i];
        if let Some((w, h)) = size.filter(|(w, h)| *w > 0 && *h > 0) {
            s.size = Size::new(w as f32, h as f32);
        }
        s.configured = true;
        s.resizing = None;
        s.dirty_config = true;
        s.set_viewports();
        let (id, size, scale) = (s.id, s.size, s.px_scale() as f32);
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
        // The role surface sets the scale for both. With fractional scale the buffer scale
        // stays 1 and the viewport does the mapping.
        if s.scale == factor || factor < 1 || s.is_child(wl) || s.px_scale_is_fractional() {
            return;
        }
        s.scale = factor;
        if let Some(c) = &s.child {
            c.wl.set_buffer_scale(factor);
        }
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
            let s = &mut self.surfaces[i];
            if s.child.is_some() && !s.is_child(wl) {
                s.cpu_callback_pending = false;
            } else {
                s.callback_pending = false;
            }
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
        if !self.proxy_closed(&layer.wl_surface().clone()) {
            State::closed(self, &layer.wl_surface().clone());
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        c: LayerSurfaceConfigure,
        _: u32,
    ) {
        if !self.proxy_configured(&layer.wl_surface().clone()) {
            self.configured(&layer.wl_surface().clone(), Some(c.new_size));
        }
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
        let frame = self.surfaces[i].take_feedback(wl).unwrap_or(0);
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
        // The first GPU frame itself may have been replaced before it showed.
        if self.surfaces[i].first_gpu.is_some_and(|f| frame >= f)
            && !self.diag.stages.iter().any(|s| s.0 == "first GPU frame on screen")
        {
            self.diag.stages.push(("first GPU frame on screen", t));
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
            self.surfaces[i].take_feedback(wl);
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
delegate_shm!(State);
delegate_subcompositor!(State);

impl Dispatch<WpFractionalScaleV1, wl_surface::WlSurface> for State {
    fn event(
        state: &mut State,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        wl: &wl_surface::WlSurface,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let wp_fractional_scale_v1::Event::PreferredScale { scale } = event else { return };
        let Some(i) = state.surface_index(wl) else { return };
        let s = &mut state.surfaces[i];
        // In 120ths, by the protocol.
        let k = scale as f64 / 120.0;
        if s.frac == Some(k) || k <= 0.0 {
            return;
        }
        s.frac = Some(k);
        if s.configured {
            state.configured(&wl.clone(), None);
        }
    }
}

/// The seat the keyboard comes from, bound at version 9 or lower: from version 10 KWin
/// repeats keys itself and sends a repeat rate of 0, and the driver needs the real rate to
/// repeat on its own clock (R38). Only its capabilities are read.
impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut State,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<State>,
    ) {
        let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event else { return };
        let has = caps.contains(wl_seat::Capability::Keyboard);
        if has && state.keyboards.is_empty() {
            state.keyboards.push(seat.get_keyboard(qh, ()));
        } else if !has && !state.keyboards.is_empty() {
            for k in state.keyboards.drain(..) {
                k.release();
            }
            if let Some(id) = state.keyboard.leave() {
                let t = state.now();
                state.events.push(HostEvent::KeyboardFocus { id, t, gained: false, requested: false });
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut State,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let t = state.now();
        match event {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                state.keyboard.keymap(fd, size);
            }
            wl_keyboard::Event::Enter { surface, .. } => {
                // Keys held at enter are ignored, so the key list is not read.
                if let Some(id) = state.focus_target(&surface) {
                    state.keyboard.enter(id, t, &mut state.events);
                }
            }
            wl_keyboard::Event::Leave { .. } => {
                if let Some(id) = state.keyboard.leave() {
                    state.events.push(HostEvent::KeyboardFocus { id, t, gained: false, requested: false });
                }
            }
            wl_keyboard::Event::Key { key, state: WEnum::Value(s), .. } => {
                let pressed = s == wl_keyboard::KeyState::Pressed;
                // Compositor repeats (wl_keyboard 10) are dropped: the driver repeats.
                if pressed || s == wl_keyboard::KeyState::Released {
                    state.keyboard.key(t, key, pressed, &mut state.events);
                }
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                state.keyboard.modifiers(t, (mods_depressed, mods_latched, mods_locked, group), &mut state.events);
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                state.events.push(crate::keyboard::Keyboard::repeat_info(t, rate, delay));
            }
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(State: ignore WpFractionalScaleManagerV1);
wayland_client::delegate_noop!(State: ignore WpViewporter);
wayland_client::delegate_noop!(State: ignore WpViewport);
