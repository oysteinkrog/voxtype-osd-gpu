//! 3D elements in a vo scene: `Element::gpu(key, vo_3d::item(source))` reserves the box, and
//! [`Painter3d`], added to the 2D renderer with [`install`], draws the source's frame there.
//!
//! ```ignore
//! let ring = vo_3d::item(Ring::new());            // keep this Arc across views
//! Element::gpu("ring", ring.clone()).size(320.0, 200.0)
//! // once, where the app's renderer is made (vo: `Options::renderer_setup`):
//! vo_3d::install(&mut renderer);
//! ```

use std::any::Any;
use std::sync::Arc;

use vo_render::{GpuPainter, PaintCx, Renderer};
use vo_scene::geom::Size;
use vo_scene::gpu::GpuSource;

use crate::item::{ItemPainter, ItemPlace, Source3d, StageAt};

/// A [`Source3d`] as a GPU element's source. Make it once and keep the `Arc`: the element and
/// the painter know it by address.
#[derive(Debug)]
pub struct Item3d(Box<dyn Source3d>);

impl GpuSource for Item3d {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn until(&self) -> f64 {
        self.0.until()
    }
}

/// `source` as the source of `Element::gpu`.
pub fn item(source: impl Source3d + 'static) -> Arc<dyn GpuSource> {
    Arc::new(Item3d(Box::new(source)))
}

/// Draws [`Item3d`] elements. Each item in a frame gets its own [`ItemPainter`], since a painter
/// writes its buffers once per submit; each one holds its own 3D pipelines and meshes, so keep
/// to one or two 3D elements per frame.
#[derive(Default)]
pub struct Painter3d {
    painters: Vec<ItemPainter>,
    next: usize,
}

impl GpuPainter for Painter3d {
    fn begin_frame(&mut self) {
        self.next = 0;
    }

    fn paint(&mut self, cx: &mut PaintCx<'_>) -> bool {
        let Some(item) = cx.source.as_any().downcast_ref::<Item3d>() else { return false };
        if self.next == self.painters.len() {
            self.painters.push(ItemPainter::new(cx.device, cx.queue));
        }
        let painter = &mut self.painters[self.next];
        self.next += 1;
        let [x0, y0, x1, y1] = cx.rect_px();
        let at = ItemPlace {
            rect: [x0, y0, x1 - x0, y1 - y0],
            surface: cx.size,
            opacity: cx.opacity,
            size: Size::new(cx.rect.w, cx.rect.h),
            scissor: Some(cx.scissor),
            stage: Some(StageAt {
                origin: cx.stage.origin,
                perspective: cx.stage.perspective,
                centre: {
                    let c = vo_scene::geom::Point::new(0.5 * (x0 + x1), 0.5 * (y0 + y1));
                    let c = cx.warp.map_or(c, |w| w.apply(c));
                    [c.x, c.y]
                },
            }),
        };
        painter.paint(cx.enc, cx.view, cx.format, item.0.as_ref(), cx.t, &at);
        true
    }
}

/// Adds a [`Painter3d`] and a [`crate::HeroPainter`] to `renderer`, so it draws the 3D and
/// hero elements of the lists it renders.
pub fn install(renderer: &mut Renderer) {
    renderer.add_painter(Box::new(Painter3d::default()));
    renderer.add_painter(Box::new(crate::HeroPainter::default()));
}
