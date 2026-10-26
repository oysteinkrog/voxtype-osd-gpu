//! The Wayland host (`docs/FRAMEWORK-PLAN.md` decision 9).
//!
//! - Surfaces: layer-shell surfaces where the compositor has `zwlr_layer_shell_v1`, else an
//!   undecorated `xdg_toplevel` window as a fallback. The fallback cannot stay on top or refuse
//!   focus, and it is not tested on GNOME yet.
//! - One wgpu device for every surface, made when the app first asks for it, after the first
//!   surface is committed, so the compositor's configure round trip overlaps device creation.
//! - Swapchain: an sRGB format, premultiplied alpha when the driver offers it (else the first
//!   mode it lists, the spike's rule), `Fifo`.
//! - Timing: a frame callback paces every frame. `wp_presentation` feedback gives the last
//!   present time and the refresh period, and `Frame::t` is the next vblank after now. Without
//!   feedback, `Frame::t` is the time the frame is drawn, which is right after the callback.
//! - Input: the input region from the app rides on the frame's own commit. Pointer events
//!   reach the app stamped with the host clock at receipt.
//! - A calloop ping wakes the loop from other threads. Nothing is drawn unless the app asks,
//!   so a settled app costs zero frames and the loop sleeps in `epoll`.

mod handlers;

use std::ptr::NonNull;
use std::time::Duration;

use raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::presentation_time::PresentationTimeState;
use smithay_client_toolkit::reexports::calloop::{self, EventLoop};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface};
use smithay_client_toolkit::shell::xdg::window::{Window, WindowDecorations};
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::WaylandSurface;
use vo_host::{
    ClickThrough, Frame, Gpu, Host, HostApp, HostCaps, HostCtl, HostDiag, HostError, HostEvent, InputMode, Keyboard,
    OverlaySpec, Placement, Size, Stack, SurfaceChange, SurfaceId, SurfaceKind, Waker,
};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_pointer::WlPointer, wl_surface::WlSurface};
use wayland_client::{Connection, Proxy, QueueHandle};

/// `CLOCK_MONOTONIC` in seconds: the clock Wayland present times use.
pub fn monotonic() -> f64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid timespec to write into.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}

/// Seconds from exec to now, from `/proc/self/stat` (clock-tick resolution) against
/// `CLOCK_BOOTTIME`.
pub fn since_exec() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let after = stat.rsplit_once(')').map(|(_, r)| r).unwrap_or("");
    let start_ticks: f64 = after.split_whitespace().nth(19).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    // SAFETY: sysconf and clock_gettime have no preconditions beyond a valid out pointer.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9 - start_ticks / hz
}

/// The first vblank after `now`, from the last present time and the refresh period. Without
/// both, `now`.
fn next_present(last: Option<f64>, refresh: f64, now: f64) -> f64 {
    match last {
        Some(p) if refresh > 0.0 && now >= p => p + ((now - p) / refresh).floor() * refresh + refresh,
        _ => now,
    }
}

pub(crate) enum Role {
    Layer(LayerSurface),
    Window(Window),
}

impl Role {
    fn wl(&self) -> &WlSurface {
        match self {
            Role::Layer(l) => l.wl_surface(),
            Role::Window(w) => w.wl_surface(),
        }
    }
}

pub(crate) struct Surf {
    pub(crate) id: SurfaceId,
    spec: OverlaySpec,
    // Field order matters: the wgpu surface must drop before the Wayland surface.
    wgpu: Option<wgpu::Surface<'static>>,
    pub(crate) role: Role,
    config: Option<wgpu::SurfaceConfiguration>,
    /// Logical size from the last configure.
    pub(crate) size: Size,
    pub(crate) scale: i32,
    pub(crate) configured: bool,
    pub(crate) dirty_config: bool,
    want_frame: bool,
    pub(crate) callback_pending: bool,
    frames: u64,
    /// Frame numbers with present feedback still to come, oldest first.
    pub(crate) feedback: std::collections::VecDeque<u64>,
    pub(crate) last_present: Option<f64>,
    pub(crate) refresh: f64,
    pending_input: Option<InputMode>,
}

struct GpuState {
    adapter: wgpu::Adapter,
    gpu: Gpu,
}

pub struct State {
    pub(crate) conn: Connection,
    pub(crate) qh: QueueHandle<State>,
    pub(crate) registry: RegistryState,
    pub(crate) outputs: OutputState,
    pub(crate) seats: SeatState,
    compositor: CompositorState,
    layer_shell: Option<LayerShell>,
    xdg: Option<XdgShell>,
    pub(crate) presentation: PresentationTimeState,
    caps: HostCaps,
    pub(crate) diag: HostDiag,
    origin: f64,
    instance: Option<wgpu::Instance>,
    gpu: Option<GpuState>,
    pub(crate) surfaces: Vec<Surf>,
    next_id: SurfaceId,
    pub(crate) pointers: Vec<WlPointer>,
    pub(crate) events: Vec<HostEvent>,
    timer: Option<f64>,
    exit: bool,
    waker: Waker,
    in_frame: Option<SurfaceId>,
}

/// The Wayland host. `connect` probes the compositor; `run` owns the thread.
pub struct WaylandHost {
    state: State,
    event_loop: EventLoop<'static, State>,
}

impl WaylandHost {
    /// Connects to `$WAYLAND_DISPLAY` and binds the globals the host uses.
    pub fn connect() -> Result<WaylandHost, HostError> {
        let origin = monotonic();
        let exec_to_origin = since_exec();
        let conn = Connection::connect_to_env().map_err(|e| HostError::Connect(e.to_string()))?;
        let (globals, queue) = registry_queue_init::<State>(&conn).map_err(|e| HostError::Connect(e.to_string()))?;
        let qh = queue.handle();
        let compositor = CompositorState::bind(&globals, &qh).map_err(|e| HostError::Connect(e.to_string()))?;
        let layer_shell = LayerShell::bind(&globals, &qh).ok();
        let xdg = XdgShell::bind(&globals, &qh).ok();
        if layer_shell.is_none() && xdg.is_none() {
            return Err(HostError::Connect("the compositor has neither a layer shell nor xdg_shell".into()));
        }
        let has = |name: &str| globals.contents().with_list(|l| l.iter().any(|g| g.interface == name));
        let caps = HostCaps {
            per_pixel_alpha: true,
            click_through: ClickThrough::Regions,
            no_activate: layer_shell.is_some(),
            layer_shell: layer_shell.is_some(),
            exclusive_zone: layer_shell.is_some(),
            // The blur protocols are bound by the spike, not yet by this host.
            backdrop_blur: false,
            fractional_scale: false,
            present_feedback: has("wp_presentation"),
            cpu_first_frame: false,
        };
        let presentation = PresentationTimeState::bind(&globals, &qh);

        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().map_err(|e| HostError::Connect(format!("event loop: {e}")))?;
        WaylandSource::new(conn.clone(), queue)
            .insert(event_loop.handle())
            .map_err(|e| HostError::Connect(format!("wayland source: {e}")))?;
        let (ping, ping_source) = calloop::ping::make_ping()?;
        event_loop
            .handle()
            .insert_source(ping_source, |_, _, state: &mut State| {
                let t = state.now();
                state.events.push(HostEvent::External { t });
            })
            .map_err(|e| HostError::Connect(format!("ping source: {e}")))?;

        let mut diag = HostDiag { exec_to_origin, ..HostDiag::default() };
        let t = monotonic() - origin;
        diag.stages.push(("connected", t));
        let state = State {
            registry: RegistryState::new(&globals),
            outputs: OutputState::new(&globals, &qh),
            seats: SeatState::new(&globals, &qh),
            conn,
            qh,
            compositor,
            layer_shell,
            xdg,
            presentation,
            caps,
            diag,
            origin,
            instance: None,
            gpu: None,
            surfaces: Vec::new(),
            next_id: 1,
            pointers: Vec::new(),
            events: Vec::new(),
            timer: None,
            exit: false,
            waker: Waker::new(move || ping.ping()),
            in_frame: None,
        };
        Ok(WaylandHost { state, event_loop })
    }
}

impl State {
    pub(crate) fn stage(&mut self, name: &'static str) {
        let t = self.now();
        self.diag.stages.push((name, t));
    }

    pub(crate) fn surface_index(&self, wl: &WlSurface) -> Option<usize> {
        self.surfaces.iter().position(|s| s.role.wl() == wl)
    }

    fn index(&self, id: SurfaceId) -> Result<usize, HostError> {
        self.surfaces.iter().position(|s| s.id == id).ok_or(HostError::UnknownSurface(id))
    }

    fn instance(&mut self) -> &wgpu::Instance {
        if self.instance.is_none() {
            let display = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                NonNull::new(self.conn.backend().display_ptr() as *mut std::ffi::c_void).expect("display pointer"),
            ));
            let mut desc = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(DisplayHandle(display)));
            desc.backends = wgpu::Backends::VULKAN;
            self.instance = Some(wgpu::Instance::new(desc));
            self.stage("instance");
        }
        self.instance.as_ref().unwrap()
    }

    fn apply_input(&self, wl: &WlSurface, mode: &InputMode) {
        match mode {
            InputMode::Whole => wl.set_input_region(None),
            InputMode::None | InputMode::Regions(_) => {
                let region = Region::new(&self.compositor).expect("wl_compositor makes regions");
                if let InputMode::Regions(r) = mode {
                    for rect in &r.rects {
                        region.add(rect.x, rect.y, rect.w, rect.h);
                    }
                }
                wl.set_input_region(Some(region.wl_region()));
            }
        }
    }

    pub(crate) fn remove(&mut self, i: usize) {
        let s = self.surfaces.remove(i);
        drop(s);
        let _ = self.conn.flush();
    }

    /// Sets up the swapchain for the surface's current size and scale.
    fn configure_swapchain(&mut self, i: usize) {
        let gpu = match self.gpu() {
            Ok(g) => g,
            Err(e) => panic!("{e}"),
        };
        let adapter = &self.gpu.as_ref().unwrap().adapter;
        let s = &mut self.surfaces[i];
        let surface = s.wgpu.as_ref().expect("open surfaces have a wgpu surface");
        let caps = surface.get_capabilities(adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Rgba8UnormSrgb))
            .unwrap_or(caps.formats[0]);
        let alpha = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else {
            caps.alpha_modes[0]
        };
        let w = (s.size.w.round() as u32).max(1) * s.scale as u32;
        let h = (s.size.h.round() as u32).max(1) * s.scale as u32;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: w,
            height: h,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: alpha,
            view_formats: vec![],
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&gpu.device, &config);
        if s.config.is_none() {
            let note = format!(
                "surface {}: swapchain {w}x{h} format={format:?} alpha={alpha:?} (offered {:?}) present=Fifo",
                s.id, caps.alpha_modes
            );
            self.diag.notes.push(note);
        }
        let s = &mut self.surfaces[i];
        s.config = Some(config);
        s.dirty_config = false;
    }

    /// The time the next frame is expected on screen: the next vblank after `now` when present
    /// feedback gave a phase and a period, else `now`.
    fn predict(&self, s: &Surf, now: f64) -> f64 {
        next_present(s.last_present, s.refresh, now)
    }

    fn ready(&self, s: &Surf) -> bool {
        s.want_frame && s.configured && !s.callback_pending
    }

    fn draw(&mut self, i: usize, app: &mut dyn HostApp) {
        if self.surfaces[i].dirty_config || self.surfaces[i].config.is_none() {
            self.configure_swapchain(i);
        }
        let tex = {
            let s = &self.surfaces[i];
            match s.wgpu.as_ref().unwrap().get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
                other => {
                    // Lost or outdated: set it up again and try on the next turn of the loop.
                    self.diag.notes.push(format!("surface {}: acquire failed ({other:?}), reconfiguring", s.id));
                    self.surfaces[i].dirty_config = true;
                    return;
                }
            }
        };
        let now = self.now();
        let (id, wl, frame, scale, t) = {
            let s = &self.surfaces[i];
            (s.id, s.role.wl().clone(), s.frames, s.scale as f32, self.predict(s, now))
        };
        wl.frame(&self.qh, wl.clone());
        if self.caps.present_feedback && self.presentation.feedback(&wl, &self.qh).is_ok() {
            self.surfaces[i].feedback.push_back(frame);
        }
        let size = [tex.texture.width(), tex.texture.height()];
        self.in_frame = Some(id);
        app.frame(self, &Frame { id, t, frame, size, scale, texture: &tex.texture });
        self.in_frame = None;
        // The app may have closed the surface during the frame.
        let Ok(i) = self.index(id) else { return };
        if let Some(mode) = self.surfaces[i].pending_input.take() {
            self.apply_input(&wl, &mode);
        }
        let queue = self.gpu.as_ref().unwrap().gpu.queue.clone();
        queue.present(tex);
        let s = &mut self.surfaces[i];
        s.callback_pending = true;
        s.want_frame = false;
        s.frames += 1;
        if s.frames == 1 {
            self.stage("first present call");
        }
    }

    fn timeout(&self) -> Option<Duration> {
        if self.surfaces.iter().any(|s| self.ready(s)) || !self.events.is_empty() {
            return Some(Duration::ZERO);
        }
        self.timer.map(|t| Duration::from_secs_f64((t - self.now()).max(0.0)))
    }
}

/// A display handle for `wgpu::Instance`, which wants an owned `HasDisplayHandle`.
#[derive(Debug)]
struct DisplayHandle(RawDisplayHandle);

// SAFETY: the pointer is the connection's `wl_display`, which outlives the instance (the
// host drops the instance before the connection), and libwayland's display is thread-safe.
unsafe impl Send for DisplayHandle {}
unsafe impl Sync for DisplayHandle {}

impl raw_window_handle::HasDisplayHandle for DisplayHandle {
    fn display_handle(&self) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        // SAFETY: see above.
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(self.0) })
    }
}

impl HostCtl for State {
    fn caps(&self) -> &HostCaps {
        &self.caps
    }

    fn diag(&self) -> &HostDiag {
        &self.diag
    }

    fn now(&self) -> f64 {
        monotonic() - self.origin
    }

    fn gpu(&mut self) -> Result<Gpu, HostError> {
        if let Some(g) = &self.gpu {
            return Ok(g.gpu.clone());
        }
        self.instance();
        let instance = self.instance.as_ref().unwrap();
        let surface = self.surfaces.first().and_then(|s| s.wgpu.as_ref());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: surface,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .map_err(|e| HostError::Gpu(e.to_string()))?;
        let (device, queue) = pollster::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor { label: Some("vo-host-wayland"), ..Default::default() }),
        )
        .map_err(|e| HostError::Gpu(e.to_string()))?;
        let name = adapter.get_info().name;
        self.diag.notes.push(format!("adapter: {name}"));
        self.gpu = Some(GpuState { adapter, gpu: Gpu { device, queue, adapter: name } });
        self.stage("adapter and device");
        Ok(self.gpu.as_ref().unwrap().gpu.clone())
    }

    fn open(&mut self, spec: &OverlaySpec) -> Result<SurfaceId, HostError> {
        if spec.keyboard != Keyboard::None {
            return Err(HostError::Unsupported("keyboard interactivity other than None (M1)".into()));
        }
        let wl = self.compositor.create_surface(&self.qh);
        let (w, h) = (spec.size.w.round().max(1.0) as u32, spec.size.h.round().max(1.0) as u32);
        let role = match (&self.layer_shell, &self.xdg, spec.kind) {
            (Some(shell), _, SurfaceKind::Overlay) => {
                let layer = match spec.layer {
                    Stack::Overlay => Layer::Overlay,
                    Stack::Top => Layer::Top,
                };
                let l = shell.create_layer_surface(&self.qh, wl, layer, Some(spec.app_id.clone()), None);
                l.set_size(w, h);
                if let Placement::Anchored { edges, margin } = spec.placement {
                    let mut a = Anchor::empty();
                    a.set(Anchor::TOP, edges.top);
                    a.set(Anchor::BOTTOM, edges.bottom);
                    a.set(Anchor::LEFT, edges.left);
                    a.set(Anchor::RIGHT, edges.right);
                    l.set_anchor(a);
                    l.set_margin(margin[0], margin[1], margin[2], margin[3]);
                }
                l.set_keyboard_interactivity(KeyboardInteractivity::None);
                Role::Layer(l)
            }
            (_, Some(xdg), _) => {
                let win = xdg.create_window(wl, WindowDecorations::RequestClient, &self.qh);
                win.set_title(spec.app_id.clone());
                win.set_app_id(spec.app_id.clone());
                if spec.kind == SurfaceKind::Overlay {
                    win.set_min_size(Some((w, h)));
                    win.set_max_size(Some((w, h)));
                }
                Role::Window(win)
            }
            (Some(_), None, SurfaceKind::Window) => {
                return Err(HostError::Unsupported("a window surface needs xdg_shell".into()));
            }
            (None, None, _) => unreachable!("connect checks for a shell"),
        };
        self.apply_input(role.wl(), &spec.input);
        role.wl().commit();
        let _ = self.conn.flush();
        if self.surfaces.is_empty() {
            self.stage("surface committed");
        }
        let window = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            NonNull::new(role.wl().id().as_ptr() as *mut std::ffi::c_void).expect("surface pointer"),
        ));
        let display = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            NonNull::new(self.conn.backend().display_ptr() as *mut std::ffi::c_void).expect("display pointer"),
        ));
        // SAFETY: the handles point at live Wayland objects. `Surf` drops the wgpu surface
        // before the Wayland surface (field order), and the host drops all surfaces before the
        // connection.
        let wgpu_surface = unsafe {
            self.instance().create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(display),
                raw_window_handle: window,
            })
        }
        .map_err(|e| HostError::Gpu(e.to_string()))?;
        let id = self.next_id;
        self.next_id += 1;
        self.surfaces.push(Surf {
            id,
            spec: spec.clone(),
            wgpu: Some(wgpu_surface),
            role,
            config: None,
            size: spec.size,
            scale: 1,
            configured: false,
            dirty_config: true,
            want_frame: false,
            callback_pending: false,
            frames: 0,
            feedback: Default::default(),
            last_present: None,
            refresh: 0.0,
            pending_input: None,
        });
        Ok(id)
    }

    fn update(&mut self, id: SurfaceId, change: SurfaceChange) -> Result<(), HostError> {
        let i = self.index(id)?;
        match change {
            SurfaceChange::Input(mode) => {
                if self.in_frame == Some(id) {
                    self.surfaces[i].pending_input = Some(mode);
                } else {
                    let wl = self.surfaces[i].role.wl().clone();
                    self.apply_input(&wl, &mode);
                    wl.commit();
                    let _ = self.conn.flush();
                }
            }
            SurfaceChange::Size(size) => {
                let s = &mut self.surfaces[i];
                s.spec.size = size;
                let (w, h) = (size.w.round().max(1.0) as u32, size.h.round().max(1.0) as u32);
                match &s.role {
                    Role::Layer(l) => l.set_size(w, h),
                    Role::Window(win) => {
                        win.set_min_size(Some((w, h)));
                        win.set_max_size(Some((w, h)));
                    }
                }
                s.role.wl().commit();
                let _ = self.conn.flush();
            }
        }
        Ok(())
    }

    fn close(&mut self, id: SurfaceId) {
        if let Ok(i) = self.index(id) {
            self.remove(i);
        }
    }

    fn request_frame(&mut self, id: SurfaceId) {
        if let Ok(i) = self.index(id) {
            self.surfaces[i].want_frame = true;
        }
    }

    fn wake_at(&mut self, t: Option<f64>) {
        self.timer = t;
    }

    fn waker(&self) -> Waker {
        self.waker.clone()
    }

    fn exit(&mut self) {
        self.exit = true;
    }
}

impl HostCtl for WaylandHost {
    fn caps(&self) -> &HostCaps {
        self.state.caps()
    }
    fn diag(&self) -> &HostDiag {
        self.state.diag()
    }
    fn now(&self) -> f64 {
        self.state.now()
    }
    fn gpu(&mut self) -> Result<Gpu, HostError> {
        self.state.gpu()
    }
    fn open(&mut self, spec: &OverlaySpec) -> Result<SurfaceId, HostError> {
        self.state.open(spec)
    }
    fn update(&mut self, id: SurfaceId, change: SurfaceChange) -> Result<(), HostError> {
        self.state.update(id, change)
    }
    fn close(&mut self, id: SurfaceId) {
        self.state.close(id)
    }
    fn request_frame(&mut self, id: SurfaceId) {
        self.state.request_frame(id)
    }
    fn wake_at(&mut self, t: Option<f64>) {
        self.state.wake_at(t)
    }
    fn waker(&self) -> Waker {
        self.state.waker()
    }
    fn exit(&mut self) {
        self.state.exit()
    }
}

impl Host for WaylandHost {
    fn run(mut self, app: &mut dyn HostApp) -> Result<(), HostError> {
        let state = &mut self.state;
        app.start(state);
        let mut events = Vec::new();
        while !state.exit {
            let _ = state.conn.flush();
            let timeout = state.timeout();
            state.diag.loop_wakeups += 1;
            self.event_loop.dispatch(timeout, state).map_err(|e| HostError::Connect(format!("event loop: {e}")))?;
            if state.timer.is_some_and(|t| state.now() >= t) {
                state.timer = None;
                let t = state.now();
                state.events.push(HostEvent::Timer { t });
            }
            std::mem::swap(&mut events, &mut state.events);
            for ev in events.drain(..) {
                app.event(state, ev);
                if state.exit {
                    break;
                }
            }
            let mut i = 0;
            while i < state.surfaces.len() && !state.exit {
                if state.ready(&state.surfaces[i]) {
                    state.draw(i, app);
                }
                i += 1;
            }
        }
        // Surfaces before the device and the instance, all before the connection.
        state.surfaces.clear();
        state.gpu = None;
        state.instance = None;
        let _ = state.conn.flush();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::next_present;

    #[test]
    fn predicts_the_next_vblank() {
        let r = 1.0 / 60.0;
        assert_eq!(next_present(None, r, 2.0), 2.0);
        assert_eq!(next_present(Some(1.0), 0.0, 2.0), 2.0);
        assert!((next_present(Some(1.0), r, 1.0) - (1.0 + r)).abs() < 1e-12);
        assert!((next_present(Some(1.0), r, 1.0 + 2.5 * r) - (1.0 + 3.0 * r)).abs() < 1e-12);
        // A stale present time far back still lands on its phase.
        let t = next_present(Some(1.0), r, 11.004);
        assert!(t > 11.004 && t - 11.004 <= r);
        assert!((((t - 1.0) / r).round() * r + 1.0 - t).abs() < 1e-9);
    }
}
