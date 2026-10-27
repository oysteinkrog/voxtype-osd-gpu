//! Painters for GPU elements (`vo_scene::gpu`): app GPU code inside a frame.
//!
//! Where a layer has a `GpuItem`, the renderer ends the surface's render pass, calls the
//! painters with the encoder and the surface's view, then begins a new pass with
//! `LoadOp::Load` and carries on. The painter draws in its own passes, with its own depth
//! buffer or multisampling if it needs them, and must leave everything outside the item's
//! scissor as it found it.
//!
//! What the painter draws into:
//! - **Effect path** (a frame with bloom or a trail): the surface is `Rgba16Float` with linear,
//!   premultiplied colour. Write linear colour, no encoding.
//! - **Direct path**: the surface has the target's format. On an `*Srgb` format the hardware
//!   encodes. On a plain UNORM format `encode_srgb` is set and the painter must encode sRGB
//!   itself, as the renderer's own shaders do.
//!
//! Blend is premultiplied over. The layer's blend mode is not applied.
//!
//! What GPU items do not get, unlike the renderer's own content:
//! - The layer clip only as a scissor: no rounded corners and no edge fades (FX04).
//! - No motion blur smear (FX12).
//! - Nothing on the CPU first frame (`cpu`): the item appears with the first GPU frame.
//!
//! Queue writes made inside `paint` run before the whole command buffer, not at the point
//! the painter is called, so a painter that draws several items in one frame must give each
//! its own buffers.

use vo_scene::geom::{Affine, Rect};
use vo_scene::gpu::GpuSource;

/// Draws GPU items whose source it knows. Add one with `Renderer::add_painter`.
pub trait GpuPainter {
    /// Called once per frame that has GPU items, before the first `paint`.
    fn begin_frame(&mut self) {}
    /// Draws the item if this painter knows `cx.source`, and returns whether it did. The
    /// renderer asks each painter in the order they were added and stops at the first yes.
    fn paint(&mut self, cx: &mut PaintCx<'_>) -> bool;
}

/// One GPU item to draw, and where.
pub struct PaintCx<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub enc: &'a mut wgpu::CommandEncoder,
    /// The surface: the target, the effect path's scene texture or a layer's texture.
    pub view: &'a wgpu::TextureView,
    pub format: wgpu::TextureFormat,
    /// The surface's size in pixels.
    pub size: [u32; 2],
    pub source: &'a dyn GpuSource,
    /// The element's box in its layer's space.
    pub rect: Rect,
    /// The time the frame shows.
    pub t: f64,
    /// The layer's space to surface pixels.
    pub to_px: Affine,
    /// Inherited opacity, to multiply in.
    pub opacity: f32,
    /// The layer's clip as a scissor in surface pixels: `x, y, w, h`, never empty.
    pub scissor: [u32; 4],
    /// The painter must encode sRGB itself (a plain UNORM surface).
    pub encode_srgb: bool,
}

impl PaintCx<'_> {
    /// The element's box in surface pixels, `x0, y0, x1, y1`, when the layer only scales and
    /// moves it.
    pub fn rect_px(&self) -> [f32; 4] {
        let r = self.rect;
        let a = self.to_px.apply(vo_scene::geom::Point::new(r.x, r.y));
        let b = self.to_px.apply(vo_scene::geom::Point::new(r.x + r.w, r.y + r.h));
        [a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y)]
    }
}
