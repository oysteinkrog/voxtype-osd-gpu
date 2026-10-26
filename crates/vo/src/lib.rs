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

pub use vo_host::{
    Edges, HostError, InputMode, Keyboard, OverlaySpec, Placement, PointerKind, Stack, SurfaceKind,
};
pub use vo_scene as scene;
pub use vo_scene::text::TextLayout;

/// Optional parts of a run.
pub struct Options<M> {
    pub tokens: Tokens,
    /// Font files, registered in order: the first is font id 0.
    pub fonts: Vec<Vec<u8>>,
    /// Text measurer for the scene. The scene's width estimate when `None`.
    pub text: Option<Box<dyn TextLayout>>,
    /// Messages from other threads (live only).
    pub inbox: Option<Inbox<M>>,
    /// Live only.
    pub diagnostics: Option<Box<dyn Diagnostics>>,
}

impl<M> Default for Options<M> {
    fn default() -> Self {
        Options { tokens: Tokens::default(), fonts: Vec::new(), text: None, inbox: None, diagnostics: None }
    }
}

/// Runs `app` as a live overlay with default options.
#[cfg(feature = "wayland")]
pub fn run_overlay<A: App>(app: A, spec: OverlaySpec) -> Result<(), HostError> {
    run_overlay_with(app, spec, Options::default())
}
