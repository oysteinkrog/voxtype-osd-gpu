//! The host interface: what a platform host does for the framework, and the types it shares
//! with the core (`docs/FRAMEWORK-PLAN.md` decision 9, `docs/INPUT-MODEL.md`).
//!
//! A host owns the main thread, the clock and the one wgpu device. It opens overlay surfaces,
//! tells the app when a frame is due and at which time it will reach the screen, and stamps
//! every event with its clock. The app runs in callbacks ([`HostApp`]) and drives the host
//! through [`HostCtl`].
//!
//! Times are seconds as `f64` on the host clock. Its origin is the host's choice; the Wayland
//! host uses `CLOCK_MONOTONIC` minus the time the host was created, so `t` starts near 0.

use std::fmt;
use std::sync::Arc;

pub use vo_scene::display::{IRect, InputRegion};
pub use vo_scene::geom::{Point, Size};
pub use vo_scene::input::{Button, PointerEvent, PointerKind, SurfaceId};

/// What this host can do on this system. Probed once, when the host connects.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostCaps {
    /// Surfaces can be translucent per pixel.
    pub per_pixel_alpha: bool,
    pub click_through: ClickThrough,
    /// Surfaces never take keyboard focus when shown.
    pub no_activate: bool,
    /// Overlay surfaces use a layer shell. When false they fall back to a plain window.
    pub layer_shell: bool,
    pub exclusive_zone: bool,
    pub backdrop_blur: bool,
    pub fractional_scale: bool,
    /// The host knows when a frame reached the screen, so `Frame::t` is a prediction from
    /// real present times rather than the time the frame was asked for.
    pub present_feedback: bool,
    /// The host can draw the first frames on the CPU while the GPU device is still being made
    /// ([`HostCtl::gpu_in_background`]).
    pub cpu_first_frame: bool,
}

/// How much of a surface can let pointer input through to what is below.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClickThrough {
    /// Every surface takes all pointer input over its whole area.
    #[default]
    None,
    /// A surface takes all input or none.
    Whole,
    /// Input goes only to a region of the surface (`InputMode::Regions`).
    Regions,
}

/// Which surface edges an anchored overlay sticks to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edges {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

impl Edges {
    pub const TOP: Edges = Edges { top: true, bottom: false, left: false, right: false };
    pub const BOTTOM: Edges = Edges { top: false, bottom: true, left: false, right: false };
    pub const TOP_RIGHT: Edges = Edges { top: true, bottom: false, left: false, right: true };
    pub const BOTTOM_RIGHT: Edges = Edges { top: false, bottom: true, left: false, right: true };
}

/// Where an overlay sits on its output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placement {
    /// Centred on the output.
    Centered,
    /// Stuck to `edges`, `margin` logical pixels in from each anchored edge
    /// (top, right, bottom, left).
    Anchored { edges: Edges, margin: [i32; 4] },
    /// Centred across the output, with the top edge `fraction` of the output's height down
    /// (0 at the top, 1 at the bottom). Where the host cannot learn the output's height it
    /// takes 1080.
    FromTop { fraction: f32 },
}

/// Which parts of a surface take pointer input. Everything else is click-through.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum InputMode {
    /// No pointer input at all: the surface is fully click-through.
    #[default]
    None,
    /// Only these rectangles take input. An empty region is the same as `None`.
    Regions(InputRegion),
    /// The whole surface takes input.
    Whole,
}

/// Keyboard focus, matching layer-shell's keyboard interactivity. M1 implements `None` only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Keyboard {
    #[default]
    None,
    OnDemand,
    Exclusive,
}

/// The stacking layer of an overlay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stack {
    /// Above everything, full-screen windows included.
    #[default]
    Overlay,
    /// Above normal windows.
    Top,
}

/// What kind of surface to open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SurfaceKind {
    /// An overlay: a layer surface where the compositor has a layer shell, else an undecorated
    /// window (`HostCaps::layer_shell` says which).
    #[default]
    Overlay,
    /// A normal window, for dev tools such as the preview and scrub tool.
    Window,
}

/// Everything needed to open a surface.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlaySpec {
    /// Logical pixels.
    pub size: Size,
    pub placement: Placement,
    pub input: InputMode,
    pub keyboard: Keyboard,
    pub layer: Stack,
    pub kind: SurfaceKind,
    /// Layer namespace or window app id.
    pub app_id: String,
}

impl Default for OverlaySpec {
    fn default() -> OverlaySpec {
        OverlaySpec {
            size: Size::new(400.0, 120.0),
            placement: Placement::Anchored { edges: Edges::BOTTOM, margin: [0, 0, 8, 0] },
            input: InputMode::None,
            keyboard: Keyboard::None,
            layer: Stack::Overlay,
            kind: SurfaceKind::Overlay,
            app_id: "vo".into(),
        }
    }
}

/// A change to an open surface.
#[derive(Clone, Debug, PartialEq)]
pub enum SurfaceChange {
    /// New logical size. A `Configured` event follows.
    Size(Size),
    /// New input region. Asked for while drawing a frame, it rides on that frame's commit.
    Input(InputMode),
}

/// Something the host tells the app. Every variant that happened at a time carries it on the
/// host clock.
#[derive(Clone, Debug, PartialEq)]
pub enum HostEvent {
    /// The surface has a size and can be drawn. `size` is logical, `scale` is physical pixels
    /// per logical pixel. Sent again when either changes.
    Configured {
        id: SurfaceId,
        t: f64,
        size: Size,
        scale: f32,
    },
    Pointer(PointerEvent),
    /// A frame reached the screen at `t`. `refresh` is the output's refresh period in seconds,
    /// 0 when unknown.
    Presented {
        id: SurfaceId,
        t: f64,
        frame: u64,
        refresh: f64,
    },
    /// The compositor closed the surface. The id is no longer valid.
    Closed {
        id: SurfaceId,
        t: f64,
    },
    /// A [`Waker`] was woken. The app drains whatever channel goes with it.
    External {
        t: f64,
    },
    /// The time set with [`HostCtl::wake_at`] was reached.
    Timer {
        t: f64,
    },
    /// The device started by [`HostCtl::gpu_in_background`] is ready: `HostCtl::gpu` returns
    /// at once, and frames go to `HostApp::frame` from now on.
    GpuReady {
        t: f64,
    },
}

/// A frame to draw. The app encodes and submits its work into `texture` before returning; the
/// host presents it afterwards.
pub struct Frame<'a> {
    pub id: SurfaceId,
    /// The time the frame is expected to reach the screen, on the host clock. Sample at this.
    pub t: f64,
    /// Frames drawn on this surface before this one.
    pub frame: u64,
    /// Physical pixels.
    pub size: [u32; 2],
    pub scale: f32,
    pub texture: &'a wgpu::Texture,
}

/// A frame to draw on the CPU, while the GPU device is still being made. The app writes every
/// pixel of `pixels`; the host attaches the buffer afterwards.
pub struct CpuFrame<'a> {
    pub id: SurfaceId,
    /// As in [`Frame::t`].
    pub t: f64,
    /// Frames drawn on this surface before this one, CPU and GPU together.
    pub frame: u64,
    /// Physical pixels.
    pub size: [u32; 2],
    pub scale: f32,
    /// Bytes from one row to the next.
    pub stride: usize,
    /// Premultiplied, sRGB encoded, 4 bytes per pixel in the order B, G, R, A.
    pub pixels: &'a mut [u8],
}

/// The device and queue every surface of the host renders with.
#[derive(Clone, Debug)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter: String,
}

/// Wakes the host's loop from another thread, for example after sending a message on a
/// channel. The app gets `HostEvent::External`.
#[derive(Clone)]
pub struct Waker(Arc<dyn Fn() + Send + Sync>);

impl Waker {
    pub fn new(f: impl Fn() + Send + Sync + 'static) -> Waker {
        Waker(Arc::new(f))
    }

    pub fn wake(&self) {
        (self.0)()
    }
}

impl fmt::Debug for Waker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Waker")
    }
}

/// Startup timing and setup notes, for the diagnostics hook.
#[derive(Clone, Debug, Default)]
pub struct HostDiag {
    /// Seconds from process exec to host clock zero. Add it to a host time to get the time
    /// since exec. Clock-tick resolution (usually 10 ms).
    pub exec_to_origin: f64,
    /// Named startup stages, host clock, in the order they happened.
    pub stages: Vec<(&'static str, f64)>,
    /// One-line facts about the setup, for example the swapchain format and alpha mode.
    pub notes: Vec<String>,
    /// Times the host's loop woke up. While the app is settled this should stay still.
    pub loop_wakeups: u64,
}

#[derive(Debug)]
pub enum HostError {
    /// No display server, or it lacks a protocol the host needs.
    Connect(String),
    /// No GPU adapter or device.
    Gpu(String),
    /// The spec asks for something this host cannot do.
    Unsupported(String),
    UnknownSurface(SurfaceId),
    Io(std::io::Error),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostError::Connect(s) => write!(f, "cannot connect to the display: {s}"),
            HostError::Gpu(s) => write!(f, "GPU: {s}"),
            HostError::Unsupported(s) => write!(f, "not supported by this host: {s}"),
            HostError::UnknownSurface(id) => write!(f, "no open surface with id {id}"),
            HostError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for HostError {}

impl From<std::io::Error> for HostError {
    fn from(e: std::io::Error) -> HostError {
        HostError::Io(e)
    }
}

/// What the app can ask of the host, inside and outside its callbacks.
pub trait HostCtl {
    fn caps(&self) -> &HostCaps;

    fn diag(&self) -> &HostDiag;

    /// The host clock, in seconds. The only clock the app should read.
    fn now(&self) -> f64;

    /// The shared GPU. Made on the first call, matched to the first open surface if there is
    /// one, so open the first surface before asking.
    fn gpu(&mut self) -> Result<Gpu, HostError>;

    /// Starts making the GPU device on another thread, so the first frames can be drawn on the
    /// CPU meanwhile: until `HostEvent::GpuReady` the host calls `HostApp::cpu_frame` instead
    /// of `HostApp::frame`. Call it before opening the first surface. Returns false when the
    /// host cannot do this (`HostCaps::cpu_first_frame`); then use `gpu` as usual.
    fn gpu_in_background(&mut self) -> bool {
        false
    }

    fn open(&mut self, spec: &OverlaySpec) -> Result<SurfaceId, HostError>;

    fn update(&mut self, id: SurfaceId, change: SurfaceChange) -> Result<(), HostError>;

    fn close(&mut self, id: SurfaceId);

    /// Asks for one frame on `id`. The host calls `HostApp::frame` when the compositor is ready
    /// for it. Asking again before then does nothing more. A host draws nothing that was not
    /// asked for, so a settled app costs no frames.
    fn request_frame(&mut self, id: SurfaceId);

    /// Sends `HostEvent::Timer` once the host clock reaches `t`. Replaces any earlier timer;
    /// `None` clears it.
    fn wake_at(&mut self, t: Option<f64>);

    /// A handle that wakes the loop from any thread.
    fn waker(&self) -> Waker;

    /// Ends `Host::run` after the current callback.
    fn exit(&mut self);
}

/// The app side of the host: the framework's driver implements this.
pub trait HostApp {
    /// Called once the host runs, before any event. Open surfaces here.
    fn start(&mut self, host: &mut dyn HostCtl);

    fn event(&mut self, host: &mut dyn HostCtl, event: HostEvent);

    /// Draw `frame`. Changes asked for with `HostCtl::update` during this call land on the
    /// same commit as the frame.
    fn frame(&mut self, host: &mut dyn HostCtl, frame: &Frame<'_>);

    /// Draw `frame` on the CPU, before the GPU is ready. Return false to skip it; the host
    /// then waits for the GPU. Changes asked for with `HostCtl::update` during this call land
    /// on the same commit.
    fn cpu_frame(&mut self, host: &mut dyn HostCtl, frame: &mut CpuFrame<'_>) -> bool {
        let _ = (host, frame);
        false
    }
}

/// A platform host. `run` owns the calling thread until the app exits or the display closes.
pub trait Host: HostCtl {
    fn run(self, app: &mut dyn HostApp) -> Result<(), HostError>
    where
        Self: Sized;
}
