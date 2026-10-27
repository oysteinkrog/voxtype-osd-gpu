//! The display list: one frame, flat, ready for the renderer.
//!
//! `Scene::sample(t)` produces it and `vo-render` consumes it. The renderer never sees the
//! node tree. The list is a set of typed instance arenas plus layers that point into them:
//!
//! - `layers[0]` is the root. Each layer lists its content in paint order as `items`, a range
//!   into `DisplayList::items`. An item is a run of instances of one kind, or one child layer.
//!   Layers are stored in pre-order, so a child always comes after its parent.
//! - Positions are logical units in the layer's own space. `Layer::transform` maps that space
//!   to the surface. `DisplayList::scale` maps logical units to physical pixels.
//!
//! Portability rule (plan, M0): nothing here needs a feature that DX12 or Metal are known to
//! lack. Blend modes are the ones fixed-function blending can do with premultiplied alpha
//! (no dual-source blending, no programmable blend). Instance fields are `f32` and `u32`
//! only, with no 64-bit or 16-bit types, so every backend can read them as plain vertex or
//! storage data.

use std::ops::Range;

use serde::Serialize;

use crate::geom::{Affine, Color, Point, RRect, Rect, Size};
use crate::glass::GlassInstance;
use crate::gpu::GpuItem;
use crate::path::{VectorInstance, VectorSeg};

/// Index of a layer in `DisplayList::layers`.
pub type LayerIndex = u32;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DisplayList {
    /// Surface size in logical units.
    pub size: Size,
    /// Physical pixels per logical unit.
    pub scale: f32,
    pub layers: Vec<Layer>,
    pub items: Vec<Item>,
    pub shapes: Vec<ShapeInstance>,
    pub glyphs: Vec<GlyphInstance>,
    pub emitters: Vec<EmitterInstance>,
    /// The sources that `emitters` point into. Not serialized.
    #[serde(skip)]
    pub emitter_sources: Vec<SourceRef>,
    /// Live-text words that form from particles, with the time each last changed. `None`
    /// when no live text uses particles.
    pub particle_words: Option<ParticleWords>,
    /// Round dots, for dot grids (`crate::dots`).
    pub dots: Vec<DotInstance>,
    /// Path strokes and blobs (`crate::path`).
    pub vectors: Vec<VectorInstance>,
    /// The segments that `vectors` point into.
    pub vector_segs: Vec<VectorSeg>,
    /// Glass panels (`crate::glass`), each drawn under its panel's content.
    pub glass: Vec<GlassInstance>,
    /// Content drawn by app GPU code (`crate::gpu`).
    pub gpu: Vec<GpuItem>,
    /// Hit shapes in paint order, for `input_region`. Not drawn.
    pub hits: Vec<HitShape>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Layer {
    /// Enclosing layer, or `None` for the root.
    pub parent: Option<LayerIndex>,
    /// Layer space to surface space (logical units), already composed with every parent.
    pub transform: Affine,
    /// The layer's own group opacity, relative to its parent layer: the product of the
    /// opacities of the nodes from the parent layer down to this one. It is not multiplied
    /// by any parent layer's opacity; the renderer multiplies it down the tree, so the
    /// opacity a layer ends up drawn with is the product along its parent chain.
    ///
    /// On an offscreen layer it applies once, when the layer is composited. On a direct
    /// layer the renderer multiplies it into each instance, so overlapping instances inside
    /// the layer show through each other.
    pub opacity: f32,
    pub blend: BlendMode,
    /// Content outside this shape is not drawn. In layer space.
    pub clip: Option<RRect>,
    /// Edge fades of the clip (FX04): content fades out over this many layer units toward
    /// the clip's left, top, right and bottom edge. Zero for a hard edge.
    pub clip_fade: [f32; 4],
    pub target: LayerTarget,
    /// Motion blur (FX12): the distance the content moves while the shutter is open, in layer
    /// units. The renderer averages the content along this segment, centred on its position.
    /// Zero for none.
    pub motion_blur: Point,
    /// A feedback trail (FX11): the layer keeps what it drew in earlier frames, faded, under
    /// what it draws now. Only on offscreen layers.
    pub trail: Option<Trail>,
    /// Range into `DisplayList::items`, in paint order.
    pub items: Range<u32>,
}

impl Layer {
    pub fn root() -> Layer {
        Layer {
            parent: None,
            transform: Affine::IDENTITY,
            opacity: 1.0,
            blend: BlendMode::Normal,
            clip: None,
            clip_fade: [0.0; 4],
            target: LayerTarget::Direct,
            motion_blur: Point::ZERO,
            trail: None,
            items: 0..0,
        }
    }
}

/// How a layer's content is combined with what is below it. All work with premultiplied
/// alpha and fixed-function blending on every backend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[repr(u32)]
pub enum BlendMode {
    /// Source over.
    #[default]
    Normal,
    /// Adds light: glows and particles.
    Add,
    Multiply,
    Screen,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub enum LayerTarget {
    /// Drawn straight into the parent's target with the layer's transform and clip.
    #[default]
    Direct,
    /// Drawn into its own texture, then composited. Needed for exact group opacity and for
    /// effects. A layer with `bloom` puts the whole frame on the renderer's effect path.
    Offscreen { bloom: Option<Bloom> },
}

/// Bloom on an offscreen layer: the layer's content is the light source, and a blurred copy
/// of it is added on top when the layer is composited.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Bloom {
    /// How much of the blurred copy is added: 0 is none, 1 adds it at full strength.
    /// Above 1 is allowed.
    pub strength: f32,
    /// Spread in logical units, about the blur's standard deviation.
    pub radius: f32,
}

/// A feedback trail on an offscreen layer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Trail {
    /// Which trail this is from frame to frame, so the renderer finds last frame's pixels:
    /// the scene uses the node id.
    pub key: u64,
    /// Seconds for the trail to fade to half. Frame rate independent.
    pub half_life: f32,
    /// The time this frame shows, in seconds: the trail fades by the time between frames.
    pub t: f64,
}

/// A run of content in a layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Item {
    pub kind: ItemKind,
    /// Index of the first instance in the arena for `kind`, or the child layer's index.
    pub start: u32,
    /// Number of instances; 1 for a layer.
    pub len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[repr(u32)]
pub enum ItemKind {
    Shapes,
    Glyphs,
    Emitters,
    Layer,
    Dots,
    Vectors,
    Glass,
    /// GPU elements, one painter call each.
    Gpu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[repr(u32)]
pub enum ShapeKind {
    /// `geom` is the rect `x, y, w, h`; `radii` are the corners (top-left, top-right,
    /// bottom-right, bottom-left).
    RRect,
    /// `geom` is `cx, cy, r, 0`.
    Circle,
    /// `geom` is the rect `x, y, w, h`; the short side is fully round.
    Capsule,
    /// `geom` is `x0, y0, x1, y1`. Stroked only, with round caps.
    Line,
    /// A vertical strip mirrored about a centre line, for filled waveforms. `geom` is
    /// `x0, y, x1, 0`: the strip runs from `x0` to `x1` around the centre line at `y`.
    /// `radii[0]` and `radii[1]` are the half heights at `x0` and `x1`; the top and bottom
    /// edges are straight between them. Only those edges are antialiased: a pixel belongs to
    /// the band when its centre is in `[x0, x1)` measured in pixels along x, so bands that share
    /// an end tile with no seam. Meant for layers that map x to pixel x.
    Band,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[repr(u32)]
pub enum PaintKind {
    /// Nothing is drawn.
    #[default]
    None,
    /// `c0` everywhere.
    Solid,
    /// `c0` at `p0` to `c1` at `p1`, clamped outside.
    Linear,
    /// A glow around the shape, from its distance: `c0` fading out as `exp(-d / p0.x)`,
    /// times `p0.y` (the strength, which may exceed 1). Used as a fill on an instance of
    /// its own, drawn under the shape. See [`Paint::glow`].
    Glow,
    /// A soft drop shadow, a Gaussian blur of the shape: `c0`, moved by `p0`, with
    /// `p1.x` the blur's standard deviation and `p1.y` a spread that grows the shape. Used
    /// as a fill on an instance of its own, drawn under the shape. See [`Paint::shadow`].
    Shadow,
    /// A conic band (FX08): `c0` in a soft band around the direction `p1.x` (in turns,
    /// clockwise from +x) seen from `p0`, `p1.y` turns wide each side, `c1` elsewhere. On a
    /// stroke with the angle moving over time it is a rotating rim. See [`Paint::conic`].
    Conic,
    /// A straight band (FX08): `c0` on the line through `p0` square to `p1`, fading to `c1`
    /// at `p1` either side of it. Moving `p0` across a shape sweeps a sheen over it. See
    /// [`Paint::sheen`].
    Sheen,
}

/// Fill or stroke paint. Gradient points are in layer space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[repr(C)]
pub struct Paint {
    pub kind: PaintKind,
    pub c0: Color,
    pub c1: Color,
    pub p0: Point,
    pub p1: Point,
}

impl Paint {
    pub const NONE: Paint = Paint {
        kind: PaintKind::None,
        c0: Color::TRANSPARENT,
        c1: Color::TRANSPARENT,
        p0: Point::ZERO,
        p1: Point::ZERO,
    };

    pub fn solid(c: Color) -> Paint {
        Paint { kind: PaintKind::Solid, c0: c, c1: c, p0: Point::ZERO, p1: Point::ZERO }
    }

    pub fn linear(p0: Point, c0: Color, p1: Point, c1: Color) -> Paint {
        Paint { kind: PaintKind::Linear, c0, c1, p0, p1 }
    }

    /// A glow paint, `radius` in logical units. Put it in the fill of a copy of the shape
    /// drawn under it. A stroke width on that copy makes it the glow of the stroke: it falls
    /// off on both sides of the stroke, so an outline glows inside as well.
    pub fn glow(c: Color, radius: f32, strength: f32) -> Paint {
        Paint {
            kind: PaintKind::Glow,
            c0: c,
            c1: c,
            p0: Point::new(radius.max(0.0), strength.max(0.0)),
            p1: Point::ZERO,
        }
    }

    /// A drop shadow paint: moved by `offset`, blurred with standard deviation `blur` and
    /// grown by `spread`, all in logical units. Put it in the fill of a copy of the shape
    /// drawn under it.
    pub fn shadow(c: Color, offset: Point, blur: f32, spread: f32) -> Paint {
        Paint { kind: PaintKind::Shadow, c0: c, c1: c, p0: offset, p1: Point::new(blur.max(0.0), spread) }
    }

    /// A conic band paint: `c0` around the direction `angle` (turns, clockwise from +x in
    /// layer space) seen from `centre`, fading over `width` turns either side to `c1`.
    pub fn conic(centre: Point, c0: Color, c1: Color, angle: f32, width: f32) -> Paint {
        Paint { kind: PaintKind::Conic, c0, c1, p0: centre, p1: Point::new(angle, width.max(1e-4)) }
    }

    /// A straight band paint: `c0` on the line through `at`, square to `half`, fading to `c1`
    /// at `at + half` and `at - half`.
    pub fn sheen(at: Point, half: Point, c0: Color, c1: Color) -> Paint {
        Paint { kind: PaintKind::Sheen, c0, c1, p0: at, p1: half }
    }

    /// A glow or shadow paint.
    pub fn is_halo(&self) -> bool {
        matches!(self.kind, PaintKind::Glow | PaintKind::Shadow)
    }

    /// The same paint with alpha multiplied by `k`.
    pub fn fade(self, k: f32) -> Paint {
        Paint { c0: self.c0.fade(k), c1: self.c1.fade(k), ..self }
    }

    pub fn is_visible(&self) -> bool {
        match self.kind {
            PaintKind::None => false,
            PaintKind::Glow => self.c0.a > 0.0 && self.p0.y > 0.0,
            _ => self.c0.a > 0.0 || self.c1.a > 0.0,
        }
    }
}

/// `ShapeInstance::flags` bit: the stroke is one physical pixel wide at any scale, and
/// `stroke_width` is ignored.
pub const SHAPE_HAIRLINE: u32 = 1;

/// One SDF shape, in layer space.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[repr(C)]
pub struct ShapeInstance {
    pub kind: ShapeKind,
    pub flags: u32,
    pub geom: [f32; 4],
    pub radii: [f32; 4],
    pub fill: Paint,
    pub stroke: Paint,
    /// Logical units, centred on the outline. 0 draws no stroke unless `SHAPE_HAIRLINE`.
    pub stroke_width: f32,
}

impl ShapeInstance {
    pub fn rrect(r: RRect, fill: Paint) -> ShapeInstance {
        ShapeInstance {
            kind: ShapeKind::RRect,
            flags: 0,
            geom: [r.rect.x, r.rect.y, r.rect.w, r.rect.h],
            radii: r.radii,
            fill,
            stroke: Paint::NONE,
            stroke_width: 0.0,
        }
    }

    pub fn circle(centre: Point, radius: f32, fill: Paint) -> ShapeInstance {
        ShapeInstance {
            kind: ShapeKind::Circle,
            flags: 0,
            geom: [centre.x, centre.y, radius, 0.0],
            radii: [radius; 4],
            fill,
            stroke: Paint::NONE,
            stroke_width: 0.0,
        }
    }

    pub fn capsule(r: Rect, fill: Paint) -> ShapeInstance {
        let radius = 0.5 * r.w.min(r.h);
        ShapeInstance {
            kind: ShapeKind::Capsule,
            flags: 0,
            geom: [r.x, r.y, r.w, r.h],
            radii: [radius; 4],
            fill,
            stroke: Paint::NONE,
            stroke_width: 0.0,
        }
    }

    pub fn line(p0: Point, p1: Point, width: f32, stroke: Paint) -> ShapeInstance {
        ShapeInstance {
            kind: ShapeKind::Line,
            flags: 0,
            geom: [p0.x, p0.y, p1.x, p1.y],
            radii: [0.0; 4],
            fill: Paint::NONE,
            stroke,
            stroke_width: width,
        }
    }

    /// A band from `x0` to `x1` around the centre line at `y`, half height `h0` at `x0` and
    /// `h1` at `x1`.
    pub fn band(x0: f32, x1: f32, y: f32, h0: f32, h1: f32, fill: Paint) -> ShapeInstance {
        ShapeInstance {
            kind: ShapeKind::Band,
            flags: 0,
            geom: [x0, y, x1, 0.0],
            radii: [h0.max(0.0), h1.max(0.0), 0.0, 0.0],
            fill,
            stroke: Paint::NONE,
            stroke_width: 0.0,
        }
    }

    pub fn with_stroke(mut self, stroke: Paint, width: f32) -> ShapeInstance {
        self.stroke = stroke;
        self.stroke_width = width;
        self
    }

    pub fn with_hairline(mut self, stroke: Paint) -> ShapeInstance {
        self.stroke = stroke;
        self.flags |= SHAPE_HAIRLINE;
        self
    }
}

/// One glyph. The renderer looks up the outline by `font` and `glyph` (Slug encoding is
/// `vo-render`'s job). `pos` is the pen position on the baseline, in layer space.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[repr(C)]
pub struct GlyphInstance {
    pub pos: Point,
    /// Em size in logical units.
    pub size: f32,
    pub font: u32,
    pub glyph: u32,
    /// The `vo-core` word id this glyph belongs to, or `NO_WORD`. Per-word effects and
    /// particles key on it.
    pub word: u32,
    pub color: Color,
}

/// `GlyphInstance::word` for glyphs outside live text.
pub const NO_WORD: u32 = u32::MAX;

/// One round dot, in layer space. Many thousands per frame are fine: the renderer draws them
/// with a small instance of their own, not as shapes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[repr(C)]
pub struct DotInstance {
    pub centre: Point,
    pub radius: f32,
    /// How far the renderer moves the centre toward the nearest physical pixel centre, 0 to
    /// 1, when the layer maps axes to axes. 1 for dots at rest, 0 for dots in flight.
    pub snap: f32,
    pub color: Color,
}

/// Particle emitters (`crate::emitter`).
pub use crate::emitter::{EmitterInstance, ParticleWords, SourceRef};

/// A hit target's shape as sampled for this frame, for the input region.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct HitShape {
    /// The node's id as `NodeId::to_bits`.
    pub node: u64,
    /// Node space to surface space.
    pub transform: Affine,
    /// The node's box in node space.
    pub shape: RRect,
}

/// An integer rectangle in surface logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// The input region handed to the host: the union of these rectangles takes pointer input,
/// everything else is click-through. Empty means fully click-through.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct InputRegion {
    pub rects: Vec<IRect>,
}

/// Most bands per rounded corner when a hit shape is turned into rectangles. One band per
/// 4 logical pixels of radius up to this, which covers at most about a third more than the
/// corner needs.
const MAX_CORNER_BANDS: usize = 6;

impl DisplayList {
    /// Clears every arena but keeps the allocations, so a host can sample into the same
    /// list every frame without allocating.
    pub fn clear(&mut self) {
        self.layers.clear();
        self.items.clear();
        self.shapes.clear();
        self.glyphs.clear();
        self.emitters.clear();
        self.emitter_sources.clear();
        self.particle_words = None;
        self.dots.clear();
        self.vectors.clear();
        self.vector_segs.clear();
        self.glass.clear();
        self.gpu.clear();
        self.hits.clear();
    }

    pub fn root(&self) -> &Layer {
        &self.layers[0]
    }

    /// The items of one layer, in paint order.
    pub fn layer_items(&self, layer: LayerIndex) -> &[Item] {
        let r = &self.layers[layer as usize].items;
        &self.items[r.start as usize..r.end as usize]
    }

    /// The union of the hit shapes as integer rectangles, rounded outward so the region
    /// never misses a pixel of a target. Rounded corners become a few stepped bands. A
    /// rotated or skewed target uses its bounding box.
    pub fn input_region(&self) -> InputRegion {
        let mut rects = Vec::new();
        for hit in &self.hits {
            hit_rects(hit, &mut rects);
        }
        rects.sort();
        rects.dedup();
        InputRegion { rects }
    }
}

fn outward(x0: f32, y0: f32, x1: f32, y1: f32) -> Option<IRect> {
    let (x0, y0, x1, y1) = (x0.floor() as i32, y0.floor() as i32, x1.ceil() as i32, y1.ceil() as i32);
    (x1 > x0 && y1 > y0).then_some(IRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
}

fn hit_rects(hit: &HitShape, out: &mut Vec<IRect>) {
    let m = hit.transform;
    let r = hit.shape.rect;
    if !m.is_axis_aligned() {
        let corners = [(r.x, r.y), (r.right(), r.y), (r.right(), r.bottom()), (r.x, r.bottom())]
            .map(|(x, y)| m.apply(Point { x, y }));
        let min_x = corners.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
        let min_y = corners.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        let max_x = corners.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
        let max_y = corners.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
        out.extend(outward(min_x, min_y, max_x, max_y));
        return;
    }
    // Axis aligned: map the box, scale the radius. A negative scale flips the box.
    let p0 = m.apply(Point::new(r.x, r.y));
    let p1 = m.apply(Point::new(r.right(), r.bottom()));
    let (x0, x1) = (p0.x.min(p1.x), p0.x.max(p1.x));
    let (y0, y1) = (p0.y.min(p1.y), p0.y.max(p1.y));
    let k = m.a.abs().min(m.d.abs());
    let max_r = 0.5 * (x1 - x0).min(y1 - y0);
    let radius = (hit.shape.radii.iter().copied().fold(0.0, f32::max) * k).clamp(0.0, max_r);
    if radius < 0.5 {
        out.extend(outward(x0, y0, x1, y1));
        return;
    }
    // Middle band at full width, then `bands` bands at the top and at the bottom. Each band
    // is as wide as the shape at the band's inner edge, the widest point inside it.
    out.extend(outward(x0, y0 + radius, x1, y1 - radius));
    let bands = ((radius / 4.0).ceil() as usize).clamp(1, MAX_CORNER_BANDS);
    let step = radius / bands as f32;
    for i in 0..bands {
        let inner = (i + 1) as f32 * step;
        let dy = radius - inner;
        let inset = radius - (radius * radius - dy * dy).max(0.0).sqrt();
        let (a, b) = (i as f32 * step, inner);
        out.extend(outward(x0 + inset, y0 + a, x1 - inset, y0 + b));
        out.extend(outward(x0 + inset, y1 - b, x1 - inset, y1 - a));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(transform: Affine, rect: Rect, radius: f32) -> HitShape {
        HitShape { node: 1, transform, shape: RRect::new(rect, radius) }
    }

    #[test]
    fn square_box_is_one_rect_rounded_outward() {
        let list = DisplayList {
            hits: vec![hit(Affine::translate(0.5, 0.25), Rect::new(10.0, 10.0, 20.0, 5.0), 0.0)],
            ..Default::default()
        };
        assert_eq!(list.input_region().rects, vec![IRect { x: 10, y: 10, w: 21, h: 6 }]);
    }

    #[test]
    fn rounded_box_covers_every_inside_point() {
        let shape = RRect::new(Rect::new(3.3, 7.1, 120.0, 40.0), 20.0);
        let list =
            DisplayList { hits: vec![HitShape { node: 1, transform: Affine::IDENTITY, shape }], ..Default::default() };
        let region = list.input_region();
        assert!(region.rects.len() > 1);
        let covered = |p: Point| {
            region
                .rects
                .iter()
                .any(|r| p.x >= r.x as f32 && p.y >= r.y as f32 && p.x < (r.x + r.w) as f32 && p.y < (r.y + r.h) as f32)
        };
        let (mut corner_miss, mut corners) = (0, 0);
        for i in 0..400 {
            for j in 0..200 {
                let p = Point::new(i as f32 * 0.31, j as f32 * 0.29);
                if shape.contains(p) {
                    assert!(covered(p), "{p:?}");
                } else if p.x > 3.3 && p.x < 123.3 && p.y > 7.1 && p.y < 47.1 {
                    corners += 1;
                    corner_miss += covered(p) as u32;
                }
            }
        }
        // The far corners are cut, not covered as a full box.
        assert!(corner_miss * 4 < corners * 3, "{corner_miss} of {corners}");
    }

    #[test]
    fn rotated_box_uses_its_bounds() {
        let m = Affine::rotate(std::f32::consts::FRAC_PI_4).then(&Affine::translate(50.0, 50.0));
        let list = DisplayList { hits: vec![hit(m, Rect::new(-10.0, -10.0, 20.0, 20.0), 0.0)], ..Default::default() };
        assert_eq!(list.input_region().rects, vec![IRect { x: 35, y: 35, w: 30, h: 30 }]);
    }

    #[test]
    fn no_hits_is_click_through() {
        assert!(DisplayList::default().input_region().rects.is_empty());
    }
}
