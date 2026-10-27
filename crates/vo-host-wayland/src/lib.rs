//! The Wayland host (`docs/FRAMEWORK-PLAN.md` decision 9).
//!
//! - Surfaces: layer-shell surfaces where the compositor has `zwlr_layer_shell_v1`, else an
//!   undecorated `xdg_toplevel` window as a fallback. The fallback cannot stay on top or refuse
//!   focus, and it is not tested on GNOME yet.
//! - One wgpu device for every surface, made when the app first asks for it, after the first
//!   surface is committed, so the compositor's configure round trip overlaps device creation.
//! - Swapchain: an sRGB format, premultiplied alpha when the driver offers it (else the first
//!   mode it lists, the spike's rule), `Fifo`.
//! - Scale: with `wp_fractional_scale_v1` and `wp_viewporter`, buffers are drawn at the
//!   compositor's preferred scale (1.25 draws 1.25 times the logical size) with buffer scale
//!   1, and a viewport maps them back to the logical size. Without them, the integer scale
//!   from `wl_surface` and `buffer_scale`.
//! - Timing: a frame callback paces every frame. `wp_presentation` feedback gives the last
//!   present time and the refresh period, and `Frame::t` is the next vblank after now. Without
//!   feedback, `Frame::t` is the time the frame is drawn, which is right after the callback.
//! - Input: the input region from the app rides on the frame's own commit. Pointer events
//!   reach the app stamped with the host clock at receipt.
//! - A calloop ping wakes the loop from other threads. Nothing is drawn unless the app asks,
//!   so a settled app costs zero frames and the loop sleeps in `epoll`.
//! - CPU first frame (`HostCtl::gpu_in_background`): the device is made on a thread while
//!   the app draws CPU frames into `wl_shm` buffers on the role surface. The Vulkan WSI owns
//!   buffer attachment on its own surface, so the GPU draws into a sub-surface of the role
//!   surface, placed over it at (0, 0). The sub-surface starts synchronized: the first GPU
//!   frame's commit waits in its cache until the host commits the role surface with a clear
//!   buffer, so the switch from CPU to GPU pixels lands in one compositor update. Then the
//!   sub-surface goes desynchronized and runs on its own. Input regions live on the GPU
//!   surface, so the overlay is click-through until the first GPU frame.

mod blur;
pub mod guard;
mod handlers;

use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::Duration;

use raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::presentation_time::PresentationTimeState;
use smithay_client_toolkit::reexports::calloop::{self, EventLoop};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1,
    ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
};
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface};
use smithay_client_toolkit::shell::xdg::window::{Window, WindowDecorations};
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::Shm;
use smithay_client_toolkit::subcompositor::SubcompositorState;
use vo_host::{
    ClickThrough, CpuFrame, Frame, Gpu, Host, HostApp, HostCaps, HostCtl, HostDiag, HostError, HostEvent, InputMode,
    InputRegion, Keyboard, OverlaySpec, Placement, Size, Stack, SurfaceChange, SurfaceId, SurfaceKind, Waker,
};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_shm::Format;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::{wl_pointer::WlPointer, wl_surface::WlSurface};
use wayland_client::{Connection, Proxy, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use smithay_client_toolkit::reexports::protocols as wayland_protocols;

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

/// A logical length (whole units, at least 1) in physical pixels at scale `k`, rounded to
/// the nearest pixel as the fractional scale protocol asks.
fn to_px(logical: f32, k: f64) -> u32 {
    ((logical.round().max(1.0) as f64) * k).round().max(1.0) as u32
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

/// The GPU's own surface while the CPU first frame is in use: a sub-surface of the role
/// surface.
pub(crate) struct Child {
    sub: WlSubsurface,
    pub(crate) wl: WlSurface,
    viewport: Option<WpViewport>,
}

impl Drop for Child {
    fn drop(&mut self) {
        if let Some(v) = &self.viewport {
            v.destroy();
        }
        self.sub.destroy();
        self.wl.destroy();
    }
}

/// Fractional scale objects of one surface: the scale listener on the role surface and the
/// viewport that maps its buffers to the logical size.
pub(crate) struct Fractional {
    scale: WpFractionalScaleV1,
    viewport: WpViewport,
}

impl Drop for Fractional {
    fn drop(&mut self) {
        self.viewport.destroy();
        self.scale.destroy();
    }
}

/// CPU frames on the role surface, until the GPU takes over.
struct ShmStage {
    pool: SlotPool,
    /// The buffer on the role surface: the last CPU frame, then a clear one.
    buffer: Option<Buffer>,
    handed_over: bool,
}

pub(crate) struct Surf {
    pub(crate) id: SurfaceId,
    spec: OverlaySpec,
    // Field order matters: the wgpu surface must drop before the Wayland surfaces, and the
    // sub-surface before its parent.
    wgpu: Option<wgpu::Surface<'static>>,
    fractional: Option<Fractional>,
    pub(crate) child: Option<Child>,
    shm: Option<ShmStage>,
    pub(crate) role: Role,
    config: Option<wgpu::SurfaceConfiguration>,
    /// Logical size from the last configure.
    pub(crate) size: Size,
    /// Integer scale from `wl_surface`, used without fractional scale.
    pub(crate) scale: i32,
    /// The compositor's preferred fractional scale, once it sent one.
    pub(crate) frac: Option<f64>,
    pub(crate) configured: bool,
    pub(crate) dirty_config: bool,
    want_frame: bool,
    pub(crate) callback_pending: bool,
    /// The frame callback of the last CPU frame, on the role surface. Kept apart so the first
    /// GPU frame does not wait for it.
    pub(crate) cpu_callback_pending: bool,
    frames: u64,
    /// `(on the GPU's sub-surface, frame number)` with present feedback still to come, oldest
    /// first.
    pub(crate) feedback: std::collections::VecDeque<(bool, u64)>,
    /// The number of the first frame the GPU drew.
    pub(crate) first_gpu: Option<u64>,
    pub(crate) last_present: Option<f64>,
    pub(crate) refresh: f64,
    pending_input: Option<InputMode>,
    /// The compositor blur object, made on the first blur change, and a region waiting for
    /// the frame's commit (`Some(None)` turns the blur off).
    blur: Option<ExtBackgroundEffectSurfaceV1>,
    pending_blur: Option<Option<InputRegion>>,
}

impl Surf {
    /// The surface the GPU draws into, which also holds the input region.
    pub(crate) fn gpu_wl(&self) -> &WlSurface {
        self.child.as_ref().map_or(self.role.wl(), |c| &c.wl)
    }

    pub(crate) fn is_child(&self, wl: &WlSurface) -> bool {
        self.child.as_ref().is_some_and(|c| &c.wl == wl)
    }

    /// Physical pixels per logical unit.
    pub(crate) fn px_scale(&self) -> f64 {
        match &self.fractional {
            Some(_) => self.frac.unwrap_or(1.0),
            None => self.scale as f64,
        }
    }

    pub(crate) fn px_scale_is_fractional(&self) -> bool {
        self.fractional.is_some()
    }

    /// The buffer size in physical pixels.
    pub(crate) fn px_size(&self) -> (u32, u32) {
        let k = self.px_scale();
        (to_px(self.size.w, k), to_px(self.size.h, k))
    }

    /// With fractional scale: the viewports show the buffers at the logical size. Applies on
    /// each surface's next commit.
    pub(crate) fn set_viewports(&self) {
        let Some(f) = &self.fractional else { return };
        let (w, h) = (self.size.w.round().max(1.0) as i32, self.size.h.round().max(1.0) as i32);
        f.viewport.set_destination(w, h);
        if let Some(v) = self.child.as_ref().and_then(|c| c.viewport.as_ref()) {
            v.set_destination(w, h);
        }
    }
}

struct GpuState {
    adapter: wgpu::Adapter,
    gpu: Gpu,
}

/// An sRGB format, premultiplied alpha when offered (else the first mode), `Fifo`. Also returns
/// the alpha modes offered, for the setup note.
fn swapchain_config(
    surface: &wgpu::Surface<'_>,
    adapter: &wgpu::Adapter,
    w: u32,
    h: u32,
) -> (wgpu::SurfaceConfiguration, Vec<wgpu::CompositeAlphaMode>) {
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
    (config, caps.alpha_modes)
}

fn swapchain_note(id: SurfaceId, c: &wgpu::SurfaceConfiguration, offered: &[wgpu::CompositeAlphaMode]) -> String {
    format!(
        "surface {id}: swapchain {}x{} format={:?} alpha={:?} (offered {offered:?}) present=Fifo",
        c.width, c.height, c.format, c.alpha_mode
    )
}

/// The first GPU surface, handed to the device thread: the `wl_surface` pointer and the size
/// in physical pixels.
struct BgTarget(usize, u32, u32);

/// What the device thread sends back.
struct BgGpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// The first GPU surface with its swapchain set up, and the alpha modes it offered.
    surface: Option<(wgpu::Surface<'static>, wgpu::SurfaceConfiguration, Vec<wgpu::CompositeAlphaMode>)>,
    /// Host clock times of "instance" and "adapter and device".
    stages: [f64; 2],
}

/// Makes the instance, then waits for the first surface, then makes the adapter, the device
/// and the swapchain, all off the main thread. Swapchain setup asks the compositor for
/// buffer formats in several round trips, about 10 ms in a nested KWin, so it belongs here
/// too. The Vulkan WSI uses its own event queue, so this is safe while the main thread
/// dispatches.
fn make_gpu(display: DisplayHandle, origin: f64, target: mpsc::Receiver<BgTarget>) -> Result<BgGpu, String> {
    let raw_display = display.0;
    let mut desc = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(display));
    desc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(desc);
    let t_instance = monotonic() - origin;
    let surface = target.recv().ok().and_then(|BgTarget(ptr, w, h)| {
        let window = RawWindowHandle::Wayland(WaylandWindowHandle::new(NonNull::new(ptr as *mut std::ffi::c_void)?));
        // SAFETY: the pointer is a live `wl_surface`. The host waits for this thread before it
        // destroys any surface.
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(raw_display),
                raw_window_handle: window,
            })
        };
        surface.ok().map(|s| (s, w, h))
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: surface.as_ref().map(|s| &s.0),
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .map_err(|e| e.to_string())?;
    let (device, queue) = pollster::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor { label: Some("vo-host-wayland"), ..Default::default() }),
    )
    .map_err(|e| e.to_string())?;
    let t_device = monotonic() - origin;
    let surface = surface.map(|(s, w, h)| {
        let (config, offered) = swapchain_config(&s, &adapter, w, h);
        s.configure(&device, &config);
        (s, config, offered)
    });
    Ok(BgGpu { instance, adapter, device, queue, surface, stages: [t_instance, t_device] })
}

pub struct State {
    pub(crate) conn: Connection,
    pub(crate) qh: QueueHandle<State>,
    pub(crate) registry: RegistryState,
    pub(crate) outputs: OutputState,
    pub(crate) seats: SeatState,
    compositor: CompositorState,
    subcompositor: Option<SubcompositorState>,
    pub(crate) shm: Option<Shm>,
    layer_shell: Option<LayerShell>,
    xdg: Option<XdgShell>,
    /// Both or neither: fractional scale needs the viewport to map buffers back.
    fractional: Option<(WpFractionalScaleManagerV1, WpViewporter)>,
    /// `ext-background-effect-v1`; `caps.backdrop_blur` follows its capabilities event.
    blur: Option<ExtBackgroundEffectManagerV1>,
    pub(crate) presentation: PresentationTimeState,
    caps: HostCaps,
    pub(crate) diag: HostDiag,
    origin: f64,
    instance: Option<wgpu::Instance>,
    gpu: Option<GpuState>,
    /// The device thread's answer, while it is being made.
    bg: Option<mpsc::Receiver<Result<BgGpu, String>>>,
    /// Hands the first GPU surface to the device thread.
    bg_target: Option<mpsc::Sender<BgTarget>>,
    gpu_failed: Option<String>,
    loop_handle: calloop::LoopHandle<'static, State>,
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
        // Examples and the preview tool stay off the real desktop unless asked (`guard`).
        guard::check(&guard::DisplayFacts::current()).map_err(HostError::Connect)?;
        let origin = monotonic();
        let exec_to_origin = since_exec();
        let conn = Connection::connect_to_env().map_err(|e| HostError::Connect(e.to_string()))?;
        let (globals, queue) = registry_queue_init::<State>(&conn).map_err(|e| HostError::Connect(e.to_string()))?;
        let qh = queue.handle();
        let compositor = CompositorState::bind(&globals, &qh).map_err(|e| HostError::Connect(e.to_string()))?;
        let subcompositor = SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh).ok();
        let shm = Shm::bind(&globals, &qh).ok();
        let layer_shell = LayerShell::bind(&globals, &qh).ok();
        let xdg = XdgShell::bind(&globals, &qh).ok();
        if layer_shell.is_none() && xdg.is_none() {
            return Err(HostError::Connect("the compositor has neither a layer shell nor xdg_shell".into()));
        }
        let has = |name: &str| globals.contents().with_list(|l| l.iter().any(|g| g.interface == name));
        let fractional = match (
            globals.bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ()),
            globals.bind::<WpViewporter, _, _>(&qh, 1..=1, ()),
        ) {
            (Ok(m), Ok(v)) => Some((m, v)),
            _ => None,
        };
        let caps = HostCaps {
            per_pixel_alpha: true,
            click_through: ClickThrough::Regions,
            no_activate: layer_shell.is_some(),
            layer_shell: layer_shell.is_some(),
            exclusive_zone: layer_shell.is_some(),
            // Set by the manager's capabilities event, which the first roundtrip brings.
            backdrop_blur: false,
            fractional_scale: fractional.is_some(),
            present_feedback: has("wp_presentation"),
            cpu_first_frame: subcompositor.is_some() && shm.is_some(),
        };
        let presentation = PresentationTimeState::bind(&globals, &qh);
        let blur = globals.bind::<ExtBackgroundEffectManagerV1, _, _>(&qh, 1..=1, ()).ok();

        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().map_err(|e| HostError::Connect(format!("event loop: {e}")))?;
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
        let mut state = State {
            registry: RegistryState::new(&globals),
            outputs: OutputState::new(&globals, &qh),
            seats: SeatState::new(&globals, &qh),
            conn,
            qh,
            compositor,
            subcompositor,
            shm,
            layer_shell,
            xdg,
            fractional,
            blur,
            presentation,
            caps,
            diag,
            origin,
            instance: None,
            gpu: None,
            bg: None,
            bg_target: None,
            gpu_failed: None,
            loop_handle: event_loop.handle(),
            surfaces: Vec::new(),
            next_id: 1,
            pointers: Vec::new(),
            events: Vec::new(),
            timer: None,
            exit: false,
            waker: Waker::new(move || ping.ping()),
            in_frame: None,
        };
        // Output sizes arrive after the outputs are bound; `Placement::FromTop` needs them
        // before the first surface opens.
        let mut queue = queue;
        queue.roundtrip(&mut state).map_err(|e| HostError::Connect(format!("output roundtrip: {e}")))?;
        state.stage("outputs known");
        WaylandSource::new(state.conn.clone(), queue)
            .insert(event_loop.handle())
            .map_err(|e| HostError::Connect(format!("wayland source: {e}")))?;
        Ok(WaylandHost { state, event_loop })
    }
}

impl State {
    pub(crate) fn stage(&mut self, name: &'static str) {
        let t = self.now();
        self.diag.stages.push((name, t));
    }

    /// Logical height of the first output with a known size, else 1080. A layer surface with
    /// no output goes where the compositor puts it, which this does not know, so the first
    /// output stands in, as GTK's layer shell does.
    fn output_height(&self) -> i32 {
        self.outputs
            .outputs()
            .filter_map(|o| self.outputs.info(&o))
            .find_map(|info| {
                info.logical_size.map(|(_, h)| h).or_else(|| {
                    let mode = info.modes.iter().find(|m| m.current)?;
                    Some(mode.dimensions.1 / info.scale_factor.max(1))
                })
            })
            .filter(|h| *h > 0)
            .unwrap_or(1080)
    }

    /// The surface `wl` belongs to: its role surface or its GPU sub-surface.
    pub(crate) fn surface_index(&self, wl: &WlSurface) -> Option<usize> {
        self.surfaces.iter().position(|s| s.role.wl() == wl || s.is_child(wl))
    }

    fn display_handle(&self) -> DisplayHandle {
        DisplayHandle(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            NonNull::new(self.conn.backend().display_ptr() as *mut std::ffi::c_void).expect("display pointer"),
        )))
    }

    /// Takes the device thread's answer if it is there; with `wait`, waits for it.
    fn poll_gpu(&mut self, wait: bool) {
        let Some(rx) = &self.bg else { return };
        let answer = if wait {
            rx.recv().map_err(|_| "the GPU thread ended".to_string())
        } else {
            match rx.try_recv() {
                Ok(a) => Ok(a),
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => Err("the GPU thread ended".to_string()),
            }
        };
        self.bg = None;
        self.bg_target = None;
        match answer.and_then(|a| a) {
            Ok(mut b) => {
                self.diag.stages.push(("instance", b.stages[0]));
                self.diag.stages.push(("adapter and device", b.stages[1]));
                if let Some((surface, config, offered)) = b.surface.take() {
                    if let Some(s) = self.surfaces.iter_mut().find(|s| s.child.is_some() && s.wgpu.is_none()) {
                        self.diag.notes.push(swapchain_note(s.id, &config, &offered));
                        s.wgpu = Some(surface);
                        s.config = Some(config);
                    }
                }
                let name = b.adapter.get_info().name;
                self.diag.notes.push(format!("adapter: {name}"));
                self.instance = Some(b.instance);
                self.gpu =
                    Some(GpuState { adapter: b.adapter, gpu: Gpu { device: b.device, queue: b.queue, adapter: name } });
                for i in 0..self.surfaces.len() {
                    if self.surfaces[i].wgpu.is_some() {
                        continue;
                    }
                    let wl = self.surfaces[i].gpu_wl().clone();
                    match self.wgpu_surface(&wl) {
                        Ok(surface) => self.surfaces[i].wgpu = Some(surface),
                        Err(e) => self.gpu_failed = Some(e.to_string()),
                    }
                }
                self.stage("gpu ready");
            }
            Err(e) => self.gpu_failed = Some(e),
        }
        let t = self.now();
        self.events.push(HostEvent::GpuReady { t });
    }

    /// A wgpu surface on `wl`.
    fn wgpu_surface(&mut self, wl: &WlSurface) -> Result<wgpu::Surface<'static>, HostError> {
        let window = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            NonNull::new(wl.id().as_ptr() as *mut std::ffi::c_void).expect("surface pointer"),
        ));
        let display = self.display_handle().0;
        // SAFETY: the handles point at live Wayland objects. `Surf` drops the wgpu surface
        // before the Wayland surfaces (field order), and the host drops all surfaces before the
        // connection.
        unsafe {
            self.instance().create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(display),
                raw_window_handle: window,
            })
        }
        .map_err(|e| HostError::Gpu(e.to_string()))
    }

    fn index(&self, id: SurfaceId) -> Result<usize, HostError> {
        self.surfaces.iter().position(|s| s.id == id).ok_or(HostError::UnknownSurface(id))
    }

    fn instance(&mut self) -> &wgpu::Instance {
        if self.instance.is_none() {
            let mut desc = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(self.display_handle()));
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

    /// Sets the blur region on the surface the GPU draws into, for its next commit.
    fn apply_blur(&mut self, i: usize, region: Option<&InputRegion>) {
        let (Some(manager), true) = (&self.blur, self.caps.backdrop_blur) else { return };
        let s = &mut self.surfaces[i];
        let wl = s.gpu_wl().clone();
        let effect = s.blur.get_or_insert_with(|| manager.get_background_effect(&wl, &self.qh, ()));
        blur::set_region(&self.compositor, effect, region);
    }

    pub(crate) fn remove(&mut self, i: usize) {
        let s = self.surfaces.remove(i);
        if let Some(effect) = &s.blur {
            effect.destroy();
        }
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
        let (w, h) = s.px_size();
        // Set up already at this size (the device thread does the first one).
        if s.config.as_ref().is_some_and(|c| c.width == w && c.height == h) {
            s.dirty_config = false;
            return;
        }
        let surface = s.wgpu.as_ref().expect("open surfaces have a wgpu surface");
        let (config, offered) = swapchain_config(surface, adapter, w, h);
        surface.configure(&gpu.device, &config);
        if s.config.is_none() {
            let note = swapchain_note(s.id, &config, &offered);
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
        let gpu = s.wgpu.is_some() && !s.callback_pending;
        let cpu = s.wgpu.is_none() && !s.cpu_callback_pending && s.shm.as_ref().is_some_and(|m| !m.handed_over);
        s.want_frame && s.configured && (gpu || cpu)
    }

    /// A CPU frame into a `wl_shm` buffer on the role surface.
    fn draw_cpu(&mut self, i: usize, app: &mut dyn HostApp) {
        let Some(mut stage) = self.surfaces[i].shm.take() else { return };
        let s = &self.surfaces[i];
        let (id, frame, scale, role) = (s.id, s.frames, s.px_scale(), s.role.wl().clone());
        let (w, h) = s.px_size();
        let stride = w as usize * 4;
        let t = self.predict(s, self.now());
        let (buffer, canvas) = match stage.pool.create_buffer(w as i32, h as i32, stride as i32, Format::Argb8888) {
            Ok(b) => b,
            Err(e) => {
                self.diag.notes.push(format!("surface {id}: no shm buffer ({e}), waiting for the GPU"));
                let s = &mut self.surfaces[i];
                s.shm = Some(stage);
                s.want_frame = false;
                return;
            }
        };
        // Cleared before the app runs, so a request_frame made inside cpu_frame is kept.
        self.surfaces[i].want_frame = false;
        self.in_frame = Some(id);
        let mut cpu = CpuFrame { id, t, frame, size: [w, h], scale: scale as f32, stride, pixels: canvas };
        let drawn = app.cpu_frame(self, &mut cpu);
        self.in_frame = None;
        let Ok(i) = self.index(id) else { return };
        let pending = (self.surfaces[i].pending_input.take(), self.surfaces[i].pending_blur.take());
        if pending.0.is_some() || pending.1.is_some() {
            let gpu_wl = self.surfaces[i].gpu_wl().clone();
            if let Some(mode) = pending.0 {
                self.apply_input(&gpu_wl, &mode);
            }
            if let Some(region) = pending.1 {
                self.apply_blur(i, region.as_ref());
            }
            gpu_wl.commit();
        }
        if !drawn {
            self.surfaces[i].shm = Some(stage);
            return;
        }
        role.frame(&self.qh, role.clone());
        if self.caps.present_feedback && self.presentation.feedback(&role, &self.qh).is_ok() {
            self.surfaces[i].feedback.push_back((false, frame));
        }
        if buffer.attach_to(&role).is_err() {
            self.diag.notes.push(format!("surface {id}: shm buffer busy"));
        }
        role.damage_buffer(0, 0, w as i32, h as i32);
        role.commit();
        stage.buffer = Some(buffer);
        let s = &mut self.surfaces[i];
        s.shm = Some(stage);
        s.cpu_callback_pending = true;
        s.frames += 1;
        if s.frames == 1 {
            self.stage("first present call");
        }
    }

    /// After the first GPU frame: a clear buffer on the role surface, committed together with
    /// the GPU frame waiting in the synchronized sub-surface's cache.
    fn hand_over(&mut self, i: usize) {
        let s = &mut self.surfaces[i];
        let (w, h) = s.px_size();
        let (w, h) = (w as i32, h as i32);
        let (Some(stage), Some(child)) = (s.shm.as_mut(), s.child.as_ref()) else { return };
        if stage.handed_over {
            return;
        }
        stage.handed_over = true;
        let role = s.role.wl();
        if let Ok((clear, canvas)) = stage.pool.create_buffer(w, h, w * 4, Format::Argb8888) {
            canvas.fill(0);
            let _ = clear.attach_to(role);
            role.damage_buffer(0, 0, w, h);
            stage.buffer = Some(clear);
        }
        child.sub.set_desync();
        role.commit();
        let _ = self.conn.flush();
        self.stage("hand-over commit");
    }

    fn draw(&mut self, i: usize, app: &mut dyn HostApp) {
        if self.surfaces[i].wgpu.is_none() {
            self.draw_cpu(i, app);
            return;
        }
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
        let (id, wl, frame, scale, t, on_child) = {
            let s = &self.surfaces[i];
            (s.id, s.gpu_wl().clone(), s.frames, s.px_scale() as f32, self.predict(s, now), s.child.is_some())
        };
        wl.frame(&self.qh, wl.clone());
        if self.caps.present_feedback && self.presentation.feedback(&wl, &self.qh).is_ok() {
            self.surfaces[i].feedback.push_back((on_child, frame));
        }
        self.surfaces[i].first_gpu.get_or_insert(frame);
        let size = [tex.texture.width(), tex.texture.height()];
        // Cleared before the app runs, so a request_frame made inside frame is kept.
        self.surfaces[i].want_frame = false;
        self.in_frame = Some(id);
        app.frame(self, &Frame { id, t, frame, size, scale, texture: &tex.texture });
        self.in_frame = None;
        // The app may have closed the surface during the frame.
        let Ok(i) = self.index(id) else { return };
        if let Some(mode) = self.surfaces[i].pending_input.take() {
            self.apply_input(&wl, &mode);
        }
        if let Some(region) = self.surfaces[i].pending_blur.take() {
            self.apply_blur(i, region.as_ref());
        }
        let queue = self.gpu.as_ref().unwrap().gpu.queue.clone();
        queue.present(tex);
        self.hand_over(i);
        let s = &mut self.surfaces[i];
        s.callback_pending = true;
        s.frames += 1;
        let (first, first_gpu) = (s.frames == 1, s.first_gpu == Some(s.frames - 1));
        if first {
            self.stage("first present call");
        }
        if first_gpu {
            self.stage("first GPU present call");
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
        self.poll_gpu(true);
        if let Some(e) = &self.gpu_failed {
            return Err(HostError::Gpu(e.clone()));
        }
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

    fn gpu_in_background(&mut self) -> bool {
        if self.bg.is_some() {
            return true;
        }
        if !self.caps.cpu_first_frame || self.gpu.is_some() || self.instance.is_some() {
            return false;
        }
        let Ok((ping, source)) = calloop::ping::make_ping() else { return false };
        if self.loop_handle.insert_source(source, |_, _, state: &mut State| state.poll_gpu(false)).is_err() {
            return false;
        }
        let (tx, rx) = mpsc::channel();
        let (target_tx, target_rx) = mpsc::channel();
        let (display, origin) = (self.display_handle(), self.origin);
        let spawned = std::thread::Builder::new().name("vo-gpu".into()).spawn(move || {
            let _ = tx.send(make_gpu(display, origin, target_rx));
            ping.ping();
        });
        if spawned.is_err() {
            return false;
        }
        self.bg = Some(rx);
        self.bg_target = Some(target_tx);
        self.stage("gpu thread started");
        true
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
                match spec.placement {
                    // Size 0 on both axes: the compositor sizes it to the output.
                    Placement::Fullscreen => l.set_size(0, 0),
                    _ => l.set_size(w, h),
                }
                match spec.placement {
                    Placement::Fullscreen => {
                        l.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
                        // Under panels and docks too, not only in the area they leave free.
                        l.set_exclusive_zone(-1);
                    }
                    Placement::Anchored { edges, margin } => {
                        let mut a = Anchor::empty();
                        a.set(Anchor::TOP, edges.top);
                        a.set(Anchor::BOTTOM, edges.bottom);
                        a.set(Anchor::LEFT, edges.left);
                        a.set(Anchor::RIGHT, edges.right);
                        l.set_anchor(a);
                        l.set_margin(margin[0], margin[1], margin[2], margin[3]);
                    }
                    Placement::FromTop { fraction } => {
                        let top = from_top_margin(fraction, self.output_height(), h as i32);
                        l.set_anchor(Anchor::TOP);
                        l.set_margin(top, 0, 0, 0);
                    }
                    Placement::LineAt { fraction, y } => {
                        let top = line_at_margin(fraction, y, self.output_height(), h as i32);
                        l.set_anchor(Anchor::TOP);
                        l.set_margin(top, 0, 0, 0);
                    }
                    Placement::Centered => {}
                }
                l.set_keyboard_interactivity(KeyboardInteractivity::None);
                Role::Layer(l)
            }
            (_, Some(xdg), _) => {
                let win = xdg.create_window(wl, WindowDecorations::RequestClient, &self.qh);
                win.set_title(spec.app_id.clone());
                win.set_app_id(spec.app_id.clone());
                if spec.placement == Placement::Fullscreen {
                    win.set_fullscreen(None);
                } else if spec.kind == SurfaceKind::Overlay {
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
        // While the device thread runs, the GPU gets a sub-surface and the role surface shows
        // CPU frames.
        let (child, shm) = match (&self.bg, &self.subcompositor, &self.shm) {
            (Some(_), Some(sub), Some(shm)) => {
                let (sub, wl) = sub.create_subsurface(role.wl().clone(), &self.qh);
                sub.set_position(0, 0);
                let pool = SlotPool::new((w * h * 4) as usize * 2, shm)
                    .map_err(|e| HostError::Io(std::io::Error::other(e)))?;
                let viewport = self.fractional.as_ref().map(|(_, vp)| vp.get_viewport(&wl, &self.qh, ()));
                (Some(Child { sub, wl, viewport }), Some(ShmStage { pool, buffer: None, handed_over: false }))
            }
            _ => (None, None),
        };
        // The preferred scale comes with the first configure; it is sent for the role surface.
        let fractional = self.fractional.as_ref().map(|(m, vp)| Fractional {
            scale: m.get_fractional_scale(role.wl(), &self.qh, role.wl().clone()),
            viewport: vp.get_viewport(role.wl(), &self.qh, ()),
        });
        match &child {
            Some(c) => {
                self.apply_input(role.wl(), &InputMode::None);
                self.apply_input(&c.wl, &spec.input);
                c.wl.commit();
            }
            None => self.apply_input(role.wl(), &spec.input),
        }
        role.wl().commit();
        let _ = self.conn.flush();
        if self.surfaces.is_empty() {
            self.stage("surface committed");
        }
        let wgpu_surface = match &child {
            Some(c) => {
                if let Some(tx) = self.bg_target.take() {
                    let (pw, ph) = (w.max(1), h.max(1));
                    let _ = tx.send(BgTarget(c.wl.id().as_ptr() as usize, pw, ph));
                }
                None
            }
            None => Some(self.wgpu_surface(&role.wl().clone())?),
        };
        let id = self.next_id;
        self.next_id += 1;
        self.surfaces.push(Surf {
            id,
            spec: spec.clone(),
            wgpu: wgpu_surface,
            fractional,
            child,
            shm,
            role,
            config: None,
            size: spec.size,
            scale: 1,
            frac: None,
            configured: false,
            dirty_config: true,
            want_frame: false,
            callback_pending: false,
            cpu_callback_pending: false,
            frames: 0,
            feedback: Default::default(),
            first_gpu: None,
            last_present: None,
            refresh: 0.0,
            pending_input: None,
            blur: None,
            pending_blur: None,
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
                    let wl = self.surfaces[i].gpu_wl().clone();
                    self.apply_input(&wl, &mode);
                    wl.commit();
                    let _ = self.conn.flush();
                }
            }
            SurfaceChange::Blur(region) => {
                if self.in_frame == Some(id) {
                    self.surfaces[i].pending_blur = Some(region);
                } else {
                    self.apply_blur(i, region.as_ref());
                    self.surfaces[i].gpu_wl().commit();
                    let _ = self.conn.flush();
                }
            }
            SurfaceChange::Size(size) => {
                let s = &mut self.surfaces[i];
                if s.spec.placement == Placement::Fullscreen {
                    // The output decides its size.
                    return Ok(());
                }
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
        // The device thread may be using the surface.
        self.bg_target = None;
        self.poll_gpu(true);
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
    fn gpu_in_background(&mut self) -> bool {
        self.state.gpu_in_background()
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
        // A device thread still running holds the display and surface pointers: wait for it.
        state.bg_target = None;
        state.bg.take().map(|rx| rx.recv());
        // Surfaces before the device and the instance, all before the connection.
        state.surfaces.clear();
        state.gpu = None;
        state.instance = None;
        let _ = state.conn.flush();
        Ok(())
    }
}

/// The top margin for `Placement::FromTop`: `fraction` of the output's height, moved up
/// as far as needed to keep a surface `surface_h` tall on the output, but never above
/// its top edge.
fn from_top_margin(fraction: f32, output_h: i32, surface_h: i32) -> i32 {
    let top = (fraction.clamp(0.0, 1.0) * output_h as f32).round() as i32;
    top.min(output_h - surface_h).max(0)
}

/// The top margin for `Placement::LineAt`: the line `y` below the surface's top at
/// `fraction` of the output's height, kept on the output as in `from_top_margin`.
fn line_at_margin(fraction: f32, y: f32, output_h: i32, surface_h: i32) -> i32 {
    let top = (fraction.clamp(0.0, 1.0) * output_h as f32 - y).round() as i32;
    top.min(output_h - surface_h).max(0)
}

#[cfg(test)]
mod tests {
    use super::{from_top_margin, line_at_margin, next_present, to_px};

    #[test]
    fn line_at_puts_the_line_at_the_fraction() {
        // The line 250 px down sits at 600, half of 1200.
        assert_eq!(line_at_margin(0.5, 250.0, 1200, 320), 350);
        // Too high up: pinned to the top.
        assert_eq!(line_at_margin(0.1, 250.0, 1200, 320), 0);
        // Too far down: moved up to keep the surface on the output.
        assert_eq!(line_at_margin(1.0, 10.0, 1200, 320), 880);
    }

    #[test]
    fn from_top_keeps_the_surface_on_the_output() {
        // Fits: the fraction as given.
        assert_eq!(from_top_margin(0.45, 1200, 470), 540);
        // Would run off the bottom: moved up just enough.
        assert_eq!(from_top_margin(0.85, 1200, 470), 730);
        // Taller than the output: pinned to the top.
        assert_eq!(from_top_margin(0.85, 400, 470), 0);
    }

    #[test]
    fn buffer_sizes_at_fractional_scales() {
        assert_eq!(to_px(600.0, 1.0), 600);
        assert_eq!(to_px(600.0, 1.25), 750);
        assert_eq!(to_px(313.0, 1.25), 391);
        assert_eq!(to_px(187.0, 1.5), 281);
        assert_eq!(to_px(0.2, 1.25), 1);
        assert_eq!(to_px(100.0, 2.0), 200);
    }

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
