//! Canvas elements: an app's own drawing code, run from the frame time.
//!
//! `Element::canvas(key, until, draw)` calls `draw(t, rect, painter)` every time the scene is
//! sampled. `rect` is the element's box in its layer's space. The function must be pure in `t`
//! (the same `t` gives the same drawing), like every other part of `sample`, so a live session
//! and an export draw the same frames. The scene keeps waking until `until`; after it the
//! drawing must stay still.
//!
//! This is the way to put vector strokes, trim, morph and blobs (`crate::path`) into a scene
//! before they have element kinds of their own.

use std::sync::Arc;

use crate::display::{DisplayList, GlyphInstance, Item, ItemKind, Layer, ShapeInstance};
use crate::geom::{Affine, Color, Point, Rect};
use crate::path::{push_blob, push_fill, push_stroke, Contour, Outline, StrokeStyle, WidthKnot};

/// The draw function of a canvas element.
pub type DrawFn = dyn Fn(f64, Rect, &mut Painter) + Send + Sync;

/// A canvas element's drawing: compared by identity, so a view that keeps the same `Arc`
/// leaves the node alone.
#[derive(Clone)]
pub struct CanvasDraw {
    pub(crate) draw: Arc<DrawFn>,
    /// Scene time after which the drawing no longer changes.
    pub until: f64,
}

impl CanvasDraw {
    pub fn new(until: f64, draw: Arc<DrawFn>) -> CanvasDraw {
        CanvasDraw { draw, until }
    }

    pub fn moving(&self, t: f64) -> bool {
        t < self.until
    }
}

impl PartialEq for CanvasDraw {
    fn eq(&self, other: &CanvasDraw) -> bool {
        Arc::ptr_eq(&self.draw, &other.draw) && self.until == other.until
    }
}

impl std::fmt::Debug for CanvasDraw {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasDraw").field("until", &self.until).finish()
    }
}

/// What a canvas draws with. Everything is in the element's layer space, and the node's
/// opacity is multiplied in.
pub struct Painter<'a> {
    pub(crate) list: &'a mut DisplayList,
    pub(crate) items: &'a mut Vec<Item>,
    pub(crate) alpha: f32,
    /// The layer being drawn into.
    pub(crate) layer: u32,
}

impl Painter<'_> {
    /// The node's opacity, already applied to everything drawn here.
    pub fn alpha(&self) -> f32 {
        self.alpha
    }

    fn push(&mut self, item: Item) {
        if let Some(last) = self.items.last_mut() {
            if last.kind == item.kind && last.start + last.len == item.start {
                last.len += item.len;
                return;
            }
        }
        self.items.push(item);
    }

    pub fn shape(&mut self, s: ShapeInstance) {
        let s = ShapeInstance { fill: s.fill.fade(self.alpha), stroke: s.stroke.fade(self.alpha), ..s };
        let item = Item { kind: ItemKind::Shapes, start: self.list.shapes.len() as u32, len: 1 };
        self.list.shapes.push(s);
        self.push(item);
    }

    pub fn glyph(&mut self, g: GlyphInstance) {
        let g = GlyphInstance { color: g.color.fade(self.alpha), ..g };
        let item = Item { kind: ItemKind::Glyphs, start: self.list.glyphs.len() as u32, len: 1 };
        self.list.glyphs.push(g);
        self.push(item);
    }

    pub fn stroke(&mut self, c: &Contour, style: &StrokeStyle) {
        let style = StrokeStyle { c0: style.c0.fade(self.alpha), c1: style.c1.fade(self.alpha), ..*style };
        if let Some(item) = push_stroke(self.list, c, &style) {
            self.push(item);
        }
    }

    /// Adds a width table for this frame and returns its id for `StrokeStyle::widths`: a
    /// stroke drawn with it changes width along its arc length (`Contour::widen`).
    pub fn widths(&mut self, knots: &[WidthKnot]) -> u32 {
        self.list.add_widths(knots)
    }

    /// The inside of `contours` by the nonzero rule (`path::push_fill`).
    pub fn fill<'c>(&mut self, contours: impl IntoIterator<Item = &'c Contour>, style: &StrokeStyle) {
        let style = StrokeStyle { c0: style.c0.fade(self.alpha), c1: style.c1.fade(self.alpha), ..*style };
        if let Some(item) = push_fill(self.list, contours, &style) {
            self.push(item);
        }
    }

    /// Every contour of `o`.
    pub fn outline(&mut self, o: &Outline, style: &StrokeStyle) {
        for c in &o.contours {
            self.stroke(c, style);
        }
    }

    /// Draws `f` in a direct child layer whose space maps into this one by `m`, with group
    /// opacity `opacity`. For rotated glyphs (text on a path) and anything else that is not
    /// axis aligned.
    pub fn layer(&mut self, m: Affine, opacity: f32, f: impl FnOnce(&mut Painter)) {
        let index = self.list.layers.len() as u32;
        self.items.push(Item { kind: ItemKind::Layer, start: index, len: 1 });
        let transform = m.then(&self.list.layers[self.layer as usize].transform);
        self.list.layers.push(Layer { parent: Some(self.layer), transform, opacity, ..Layer::root() });
        let mut own = Vec::new();
        f(&mut Painter { list: self.list, items: &mut own, alpha: self.alpha, layer: index });
        let start = self.list.items.len() as u32;
        self.list.items.extend(own);
        self.list.layers[index as usize].items = start..self.list.items.len() as u32;
    }

    pub fn blob(&mut self, a: (Point, f32), b: (Point, f32), k: f32, c0: Color, c1: Color) {
        let item = push_blob(self.list, a, b, k, c0.fade(self.alpha), c1.fade(self.alpha));
        self.push(item);
    }
}
