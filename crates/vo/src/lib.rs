//! The framework facade.
//!
//! - [`run_overlay`]: runs an [`App`] as a live overlay on the platform host (Wayland in M1).
//! - [`render_offline`]: runs the same app through a scripted [`Scenario`] and writes frames
//!   as PNGs.
//! - `export` (feature `export`): the same render at a fixed frame count, to PNGs and to a video
//!   file through ffmpeg.
//! - [`envelope`]: the reading-line envelope and GPU budget checks for motion (MO-08).
//! - [`Diagnostics`]: startup stages since exec, memory, frames and idle checks.
//! - `preview` (feature `preview`): the preview and scrub tool.
//! - `record` (feature `record`, part of `preview`): record an app's messages and replay them
//!   as a scenario.
//!
//! An app is Elm-shaped: `update(msg, cx)` changes state, `view(&self, &Tokens)` describes it.
//! Every message is applied at its own timestamp from the host clock, and the scene samples
//! each frame at the time it will reach the screen.

mod app;
mod diag;
pub mod envelope;
#[cfg(feature = "export")]
mod export;
#[cfg(feature = "wayland")]
mod live;
mod offline;
#[cfg(feature = "preview")]
pub mod preview;
#[cfg(feature = "record")]
pub mod record;
mod surfaces;

pub use app::{channel, App, Cx, Inbox, Proxy, TimerId, Tokens};
pub use diag::{smaps_kb, Diagnostics, HeapFn, LogDiagnostics};
#[cfg(feature = "export")]
pub use export::{export, export_with, frame_hash, Export, ExportError, ExportReport, Video};
#[cfg(feature = "wayland")]
pub use live::run_overlay_with;
pub use offline::{render_offline, render_offline_with, Input, OfflineError, Scenario};
pub use surfaces::SurfaceView;

pub use vo_host::{Edges, HostError, InputMode, Keyboard, OverlaySpec, Placement, PointerKind, Stack, SurfaceKind};
pub use vo_scene as scene;
pub use vo_scene::font::FontRegistry;
pub use vo_scene::text::{ShapedLayout, TextLayout};

/// Optional parts of a run.
pub struct Options<M> {
    pub tokens: Tokens,
    /// The fonts that text styles name by id. The scene's text layout and the renderer share it.
    pub fonts: FontRegistry,
    /// Text layout for the scene. When `None`: shaped text over `fonts`, or the scene's width
    /// estimate when `fonts` is empty.
    pub text: Option<Box<dyn TextLayout>>,
    /// Messages from other threads (live only).
    pub inbox: Option<Inbox<M>>,
    /// Live only.
    pub diagnostics: Option<Box<dyn Diagnostics>>,
    /// Live only: draw the first frames on the CPU while the GPU device is made, when the
    /// host can (`HostCaps::cpu_first_frame`). Off by default.
    pub cpu_first_frame: bool,
    /// How properties move that no `.motion()` call names (`Scene::with_default_motion`).
    pub default_motion: vo_scene::Motion,
    /// Reduced motion (`Scene::with_reduced_motion`). Default: `VO_REDUCED_MOTION=1` in the
    /// environment.
    pub reduced_motion: bool,
    /// Effect quality tier (`vo_render::Quality`). Default: `VO_QUALITY`, else full.
    pub quality: vo_render::Quality,
    /// Light-mode shading (`Renderer::set_light`). Default: `VO_LIGHT=1` in the environment.
    pub light: bool,
    /// Runs on every renderer made for this app (live, offline, preview), after the quality
    /// and light settings: for example to add a painter for GPU elements, such as
    /// `vo_3d::install`.
    pub renderer_setup: Option<RendererSetup>,
    /// Live only: transparent room around the view for ink outside its box, so a glow, a
    /// shadow or particles are not cut at the surface edge. `Auto` by default.
    pub margin: SurfaceMargin,
    /// Offline only: GPU timestamps for every frame of every surface, handed here as each
    /// frame is drawn. The offline device then asks for `vo_render::profile::timestamp_features`
    /// where the adapter has them; without them the sink is never called.
    pub frame_timings: Option<FrameTimingsSink>,
}

/// How a live overlay sizes the transparent margin around its view.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum SurfaceMargin {
    /// From the first view: how far its glows, shadows, blooms, strokes, particles and
    /// motion reach outside the root box (`Scene::effect_reach`). A view that already keeps
    /// its effects inside its root gets no margin.
    #[default]
    Auto,
    /// `Auto`, and at least this on every side.
    AtLeast(vo_scene::Insets),
    /// Exactly this.
    Fixed(vo_scene::Insets),
    /// None: the surface is the size the spec asks for.
    None,
}

impl SurfaceMargin {
    /// The margin for `scene` laid out at `t`, or `None` for no margin.
    pub fn reach<M>(&self, scene: &vo_scene::Scene<M>, t: f64) -> Option<vo_scene::Insets> {
        let m = match *self {
            SurfaceMargin::Auto => scene.effect_reach(t),
            SurfaceMargin::AtLeast(m) => scene.effect_reach(t).max(m),
            SurfaceMargin::Fixed(m) => m,
            SurfaceMargin::None => return None,
        };
        (!m.ceil().is_zero()).then_some(m)
    }
}

/// See [`Options::renderer_setup`].
pub type RendererSetup = std::sync::Arc<dyn Fn(&mut vo_render::Renderer) + Send + Sync>;

/// See [`Options::frame_timings`].
pub type FrameTimingsSink = std::sync::Arc<dyn Fn(&FrameTiming) + Send + Sync>;

/// One offline frame's GPU time, for [`Options::frame_timings`].
#[derive(Clone, Debug)]
pub struct FrameTiming<'a> {
    /// The extra surface's key (`App::surfaces`), or `None` for the main surface.
    pub surface: Option<&'a str>,
    /// The surface's frame number, from 0.
    pub frame: u64,
    /// The time the frame was sampled at, in seconds.
    pub t: f64,
    /// The frame's size in physical pixels.
    pub size: [u32; 2],
    /// GPU time per section and in total.
    pub timings: &'a vo_render::profile::FrameTimings,
}

impl<M> Default for Options<M> {
    fn default() -> Self {
        Options {
            tokens: Tokens::default(),
            fonts: FontRegistry::new(),
            text: None,
            inbox: None,
            diagnostics: None,
            cpu_first_frame: false,
            default_motion: vo_scene::element::DEFAULT_MOTION,
            reduced_motion: env_flag("VO_REDUCED_MOTION"),
            quality: vo_render::Quality::from_env().unwrap_or_default(),
            light: env_flag("VO_LIGHT"),
            renderer_setup: None,
            margin: SurfaceMargin::default(),
            frame_timings: None,
        }
    }
}

impl<M> Options<M> {
    /// Applies the quality tier, light mode and `renderer_setup` to a renderer.
    pub(crate) fn configure(&self, r: &mut vo_render::Renderer) {
        r.set_quality(self.quality);
        r.set_light(self.light);
        if let Some(setup) = &self.renderer_setup {
            setup(r);
        }
    }

    /// Makes the scenes of extra surfaces (`App::surfaces`): shaped text over `fonts` and the
    /// same motion settings as the main scene. A custom `text` layout is the main scene's
    /// only, since it cannot be shared.
    pub(crate) fn scene_maker(&self) -> surfaces::MakeScene<M>
    where
        M: 'static,
    {
        let (fonts, motion, reduced) = (self.fonts.clone(), self.default_motion, self.reduced_motion);
        Box::new(move || {
            let scene = vo_scene::Scene::new().with_default_motion(motion).with_reduced_motion(reduced);
            if fonts.is_empty() {
                scene
            } else {
                scene.with_text(Box::new(ShapedLayout::new(fonts.clone())))
            }
        })
    }

    /// The scene with the text layout these options ask for.
    pub(crate) fn scene(&mut self) -> vo_scene::Scene<M> {
        let scene =
            vo_scene::Scene::new().with_default_motion(self.default_motion).with_reduced_motion(self.reduced_motion);
        match self.text.take() {
            Some(text) => scene.with_text(text),
            None if !self.fonts.is_empty() => scene.with_text(Box::new(ShapedLayout::new(self.fonts.clone()))),
            None => scene,
        }
    }
}

/// True when the variable is set to `1`, `true` or `yes`.
fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes"))
}

/// Runs `app` as a live overlay with default options.
#[cfg(feature = "wayland")]
pub fn run_overlay<A: App>(app: A, spec: OverlaySpec) -> Result<(), HostError> {
    run_overlay_with(app, spec, Options::default())
}
