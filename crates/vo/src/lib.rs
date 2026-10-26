//! The framework facade.
//!
//! - [`run_overlay`]: runs an [`App`] as a live overlay on the platform host (Wayland in M1).
//! - [`render_offline`]: runs the same app through a scripted [`Scenario`] and writes frames
//!   as PNGs.
//! - [`Diagnostics`]: startup stages since exec, memory, frames and idle checks.
//!
//! An app is Elm-shaped: `update(msg, cx)` changes state, `view(&self, &Tokens)` describes it.
//! Every message is applied at its own timestamp from the host clock, and the scene samples
//! each frame at the time it will reach the screen.

mod app;
mod diag;
#[cfg(feature = "wayland")]
mod live;
mod offline;

pub use app::{channel, App, Cx, Inbox, Proxy, TimerId, Tokens};
pub use diag::{smaps_kb, Diagnostics, HeapFn, LogDiagnostics};
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
        }
    }
}

impl<M> Options<M> {
    /// The scene with the text layout these options ask for.
    pub(crate) fn scene(&mut self) -> vo_scene::Scene<M> {
        let scene = vo_scene::Scene::new();
        match self.text.take() {
            Some(text) => scene.with_text(text),
            None if !self.fonts.is_empty() => scene.with_text(Box::new(ShapedLayout::new(self.fonts.clone()))),
            None => scene,
        }
    }
}

/// Runs `app` as a live overlay with default options.
#[cfg(feature = "wayland")]
pub fn run_overlay<A: App>(app: A, spec: OverlaySpec) -> Result<(), HostError> {
    run_overlay_with(app, spec, Options::default())
}
