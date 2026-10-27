//! GPU elements: content an app draws with its own GPU code, such as a 3D scene.
//!
//! `Element::gpu(key, source)` reserves a box. Each sample puts a [`GpuItem`] in the display
//! list with the box in its layer's space and the frame time. The renderer breaks its pass
//! there, hands the item to a painter that knows the source's type (`vo_render::GpuPainter`),
//! and resumes on top of what the painter drew. The scene knows nothing about the drawing; it
//! only keeps waking until `GpuSource::until`.
//!
//! Like a canvas, the drawing must be a pure function of the frame time, so a live session and
//! an export draw the same frames.

use std::any::Any;
use std::fmt::Debug;
use std::sync::Arc;

use serde::Serialize;

use crate::geom::Rect;

/// What a GPU element draws. A painter finds its own sources with `as_any().downcast_ref`.
pub trait GpuSource: Debug + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    /// Scene time after which the drawing no longer changes. `f64::INFINITY` keeps the scene
    /// awake for as long as the element is shown.
    fn until(&self) -> f64;
}

/// A source handle: compared by identity, so a view that keeps the same `Arc` leaves the node
/// alone.
#[derive(Clone, Debug)]
pub struct GpuRef(pub Arc<dyn GpuSource>);

impl PartialEq for GpuRef {
    fn eq(&self, o: &GpuRef) -> bool {
        Arc::ptr_eq(&self.0, &o.0)
    }
}

/// One GPU element in a frame.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GpuItem {
    #[serde(skip)]
    pub source: GpuRef,
    /// The element's box in its layer's space.
    pub rect: Rect,
    /// The time this frame shows.
    pub t: f64,
    /// Inherited opacity, which the painter multiplies in.
    pub opacity: f32,
}
