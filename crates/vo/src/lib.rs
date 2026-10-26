//! The framework facade.
//!
//! - [`run_overlay`]: runs an [`App`] as a live overlay on the platform host (Wayland in M1).
//! - [`render_offline`]: runs the same app through a scripted [`Scenario`] and writes frames
//!   as PNGs.
//! - `export` (feature `export`): the same render at a fixed frame count, to PNGs and to a video
//!   file through ffmpeg.
//! - [`Diagnostics`]: startup stages since exec, memory, frames and idle checks.
//! - `preview` (feature `preview`): the preview and scrub tool.
//!
//! An app is Elm-shaped: `update(msg, cx)` changes state, `view(&self, &Tokens)` describes it.
//! Every message is applied at its own timestamp from the host clock, and the scene samples
//! each frame at the time it will reach the screen.

mod app;
mod diag;
#[cfg(feature = "export")]
mod export;
#[cfg(feature = "wayland")]
mod live;
mod offline;
#[cfg(feature = "preview")]
pub mod preview;

pub use app::{channel, App, Cx, Inbox, Proxy, TimerId, Tokens};
pub use diag::{smaps_kb, Diagnostics, HeapFn, LogDiagnostics};
#[cfg(feature = "export")]
pub use export::{export, export_with, frame_hash, Export, ExportError, ExportReport, Video};
#[cfg(feature = "wayland")]
pub use live::run_overlay_with;
pub use offline::{render_offline, render_offline_with, Input, OfflineError, Scenario};

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
        }
    }
}

impl<M> Options<M> {
    /// Applies the quality tier and light mode to a renderer.
    pub(crate) fn configure(&self, r: &mut vo_render::Renderer) {
        r.set_quality(self.quality);
        r.set_light(self.light);
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
