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
use crate::material::Material;
use crate::path::{VectorInstance, VectorSeg, WidthKnot};
use crate::stage::{Stage, Warp};

/// Index of a layer in `DisplayList::layers`.
pub type LayerIndex = u32;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DisplayList {
    /// Surface size in logical units.
    pub size: Size,
    /// Physical pixels per logical unit.
    pub scale: f32,
    /// The camera turned and pushed layers are seen through (`crate::stage`).
    pub stage: Stage,
    pub layers: Vec<Layer>,
    pub items: Vec<Item>,
    pub shapes: Vec<ShapeInstance>,
    /// The members of shape groups (EG-22): a `ShapeKind::Group` instance points at a run of
    /// them (`ShapeInstance::group`).
    pub shape_prims: Vec<GroupPrim>,
    pub glyphs: Vec<GlyphInstance>,
    /// Glyph effects (TY17) for some glyphs, as `(index into glyphs, effects)`, sorted by
    /// index. A glyph not listed has none.
    pub glyph_fx: Vec<(u32, GlyphFx)>,
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
    /// The signal bus uniform for this frame (`vo_core::signals`), zero without a store.
    #[serde(skip)]
    pub signals: vo_core::signals::SignalBlock,
    /// Materials (FX15, `crate::material`) that shapes, glyphs and vectors point at by a
    /// 1-based id; 0 is no material.
    pub materials: Vec<Material>,
    /// Stroke width tables (`StrokeStyle::widths`), each a range into `width_knots`; the id
    /// is 1-based, 0 is none.
    pub width_tables: Vec<[u32; 2]>,
    pub width_knots: Vec<WidthKnot>,
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
    /// A look applied when the layer is composited (FX13): chromatic aberration, glitch
    /// slices, scanlines, CRT curvature, vignette and flicker. Only on offscreen layers.
    ///
    /// An offscreen layer composites in this order: its content (and trail), plus its bloom;
    /// then the style, which bends and darkens content and glow alike; then its mask; then
    /// its opacity and the clips above it; then its blend mode. The two nearest rounded
    /// clips cut round; clips further out cut along their bounding box.
    pub style: Option<LayerStyle>,
    /// The layer's perspective map in surface space (`crate::stage`), already composed with
    /// every parent's: content is laid out by `transform`, then seen through this. `None`
    /// for a flat layer under flat parents.
    pub warp: Option<Warp>,
    /// A backdrop (EG-01): before the layer draws, what is already drawn under `rect` in the
    /// surface the layer sits in is copied into a texture, and drawn back tinted. Effects
    /// that bend, blur or refract what is under an element read that texture. Never the
    /// desktop: only this surface's own drawing. `None` costs nothing.
    pub backdrop: Option<Backdrop>,
    /// A displacement of the backdrop (EG-03). Only with a full-resolution backdrop.
    pub displace: Option<Displace>,
    /// A blur of the layer's own content (EG-04, `Element::layer_blur`). The layer is drawn
    /// offscreen. `None` costs nothing.
    pub blur: Option<LayerBlur>,
    /// Frost (EG-04, `Element::frost`): the backdrop is drawn back blurred inside the layer's
    /// rounded box, in place of its tint. The blur's standard deviation in layer units, 0 for
    /// none. Only with a full-resolution backdrop.
    pub frost: f32,
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
            style: None,
            warp: None,
            backdrop: None,
            displace: None,
            blur: None,
            frost: 0.0,
            items: 0..0,
        }
    }
}

/// A Gaussian blur of a layer's content (EG-04).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct LayerBlur {
    /// The standard deviation in layer units, above 0. The blur reaches three times as far.
    pub radius: f32,
    /// The newest line of live text, in surface space (logical units), when it is drawn
    /// inside this layer: drawn sharp, as without the blur (the reading-line envelope). Found
    /// by the scene after the whole list is built.
    pub keep: Option<Rect>,
}

/// How finely a backdrop is captured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum BackdropRes {
    /// One texel per physical pixel.
    #[default]
    Full,
    /// One texel per 2 by 2 physical pixels (their average): a quarter of the memory, for
    /// blur and frost, which soften it anyway.
    Half,
}

/// A layer's backdrop request (EG-01).
///
/// The capture covers the bounding box of `rect` in physical pixels, at a whole-pixel origin,
/// so fractional scales (1.25, 1.5) sample without drift. It holds what the layer's surface
/// drew before this layer: the target, or the offscreen layer around it. Inside a mask, and
/// below the full quality tier, there is no backdrop. A frame with a backdrop takes the
/// effect path.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Backdrop {
    /// The area under the element, in layer space. The tinted draw is cut to its rounded
    /// shape.
    pub rect: RRect,
    pub res: BackdropRes,
    /// The backdrop is drawn back over itself multiplied by this colour, before the layer's
    /// content, with the layer's opacity and clips. Transparent draws nothing: the capture
    /// is then only for the layer's own effects.
    pub tint: Color,
}

/// Most ripple rings one displacement draws at once (EG-03). The scene keeps the youngest.
pub const DISPLACE_RINGS: usize = 8;

/// A displacement (EG-03, `crate::displace`): the layer's backdrop is drawn back with each
/// pixel taken from somewhere else, moved by `field`. It replaces the backdrop's tinted draw.
/// The scene emits one only when its strength is above 0 and motion is not reduced, so a
/// list without one costs nothing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Displace {
    pub field: DisplaceField,
    /// Multiplies every offset. Above 0.
    pub strength: f32,
    /// Keep the newest line of live text sharp and still (the reading-line envelope).
    pub keep_reading: bool,
    /// The box drawn as captured, in surface space (logical units): the reading line, found
    /// by the scene after the whole list is built. `None` when there is no live text or
    /// `keep_reading` is off.
    pub keep: Option<Rect>,
}

/// What moves each pixel, evaluated at the frame's time. Lengths are in layer units, places
/// in layer space. A pixel at `p` shows what the backdrop holds at `p + offset`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub enum DisplaceField {
    /// Rings that grow from where they started: `count` of `rings`, each a wave packet one
    /// `wavelength` long, `width` wide, pushing outward by up to its amplitude.
    Ripples { rings: [Ring; DISPLACE_RINGS], count: u32, wavelength: f32, width: f32 },
    /// Simplex noise, one cell `scale` wide, offsets up to `amount`. `turn` (radians) turns
    /// its lattice; a turn of 2 pi repeats.
    Noise { scale: f32, amount: f32, seed: u32, turn: f32 },
    /// The curl of simplex noise: a swirl with no sources or sinks.
    Curl { scale: f32, amount: f32, seed: u32, turn: f32 },
    /// A page lifting over a cylinder of `radius`: past the fold line through `line`, in the
    /// direction `dir` (unit), the page rolls up and its back shows, dimmed.
    PageCurl { line: Point, dir: Point, radius: f32 },
    /// Offsets from image slot `slot` (`Renderer::set_material_image`), read with uv 0 to 1
    /// over the layer's box: `(rg - bias) * amount`. Water heights or normals, flow maps.
    Texture { slot: u32, amount: [f32; 2], bias: [f32; 2] },
}

/// One ripple ring at the frame's time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Ring {
    pub centre: Point,
    pub radius: f32,
    /// How far the ring pushes at its crest, already faded by its age.
    pub amp: f32,
}

/// Composite-pass looks for an offscreen layer (FX13). Zero turns each one off. Lengths are
/// in layer units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct LayerStyle {
    /// The box the look is laid out on: its centre, its edges for the vignette and the
    /// curvature. In layer space.
    pub rect: Rect,
    /// Red and blue move apart by this much at the box's edge, outward from its centre.
    pub chroma: f32,
    /// Glitch, 0 to 1: how many of twelve horizontal slices shift sideways, and how far.
    pub glitch: f32,
    /// Which glitch pattern shows: changes at the glitch's step rate.
    pub glitch_step: u32,
    /// Scanline depth, 0 to 1, and the distance between two lines.
    pub scanlines: f32,
    pub scan_period: f32,
    /// CRT curvature, 0 to about 0.3: the content bows out as on a curved screen, and the
    /// corners outside it are empty.
    pub curvature: f32,
    /// Vignette, 0 to 1: how dark the corners get.
    pub vignette: f32,
    /// How far brightness dips in this flicker step, 0 to 0.2.
    pub flicker: f32,
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
    /// A mask (FX04) for its parent layer, which must be offscreen. The mask's content is not
    /// drawn: its coverage multiplies the parent where the parent is composited, after the
    /// parent's bloom and trail. Shapes, glyphs, vectors and dots count; particles, glass
    /// and GPU items do not. An offscreen layer inside a mask draws as a direct one, and a
    /// mask inside a mask is skipped.
    Mask(Mask),
}

/// How a mask layer masks its parent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Mask {
    /// Mask by the content's luminance (white shows, black hides) instead of its alpha.
    pub luma: bool,
    /// Show the parent where the mask is empty and hide it where the mask is full.
    pub invert: bool,
}

/// Bloom on an offscreen layer: the layer's content is the light source, and a blurred copy
/// of it is added on top when the layer is composited.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Bloom {
    /// How much of the blurred copy is added: 0 is none, 1 adds it at full strength.
    /// Above 1 is allowed.
    pub strength: f32,
    /// Spread in logical units, about the blur's standard deviation. With a `shape`, the
    /// widest band's spread.
    pub radius: f32,
    /// A shaped glow: an emissive threshold and three bands (core, mid, tail) of their own
    /// spread, weight, tint and saturation. `None` is the plain one-radius bloom.
    pub shape: Option<BloomShape>,
}

impl Bloom {
    /// A plain bloom.
    pub fn new(strength: f32, radius: f32) -> Bloom {
        Bloom { strength, radius, shape: None }
    }
}

/// The shape of a glow (bloom v2): what lights it and how its light falls off. The concepts'
/// neon is a tight hot core, a saturated middle and a wide faint tail, each a blur of its own
/// size, added together.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BloomShape {
    /// Only light brighter than this feeds the glow: linear luminance of the premultiplied
    /// colour, with a soft knee half as wide. A dim panel under bright lines then does not
    /// glow. 0 lets everything glow.
    pub threshold: f32,
    /// Core, mid and tail, from tight to wide.
    pub bands: [BloomBand; 3],
}

/// One blur in a shaped glow.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BloomBand {
    /// How much of this band is added, times `Bloom::strength`.
    pub weight: f32,
    /// Spread as a fraction of `Bloom::radius`.
    pub reach: f32,
    /// Saturation of the band's light: 1 keeps it, above 1 is more vivid, 0 is grey.
    pub saturation: f32,
    /// The band's light moves toward this colour by its alpha, keeping its brightness. White
    /// at 0.5 or more makes a hot, whitish core. Straight-alpha sRGB.
    pub tint: Color,
}

impl BloomBand {
    pub const fn new(weight: f32, reach: f32, saturation: f32, tint: Color) -> BloomBand {
        BloomBand { weight, reach, saturation, tint }
    }
}

impl BloomShape {
    /// Neon: a tight core pushed toward white, a vivid middle band and a faint wide tail.
    pub const NEON: BloomShape = BloomShape {
        threshold: 0.04,
        bands: [
            BloomBand::new(1.0, 0.12, 1.0, Color::rgba(1.0, 1.0, 1.0, 0.55)),
            BloomBand::new(0.55, 0.4, 1.5, Color::TRANSPARENT),
            BloomBand::new(0.22, 1.0, 1.1, Color::TRANSPARENT),
        ],
    };

    /// A soft halo with no hot core: for text and panels that should glow without looking lit.
    pub const SOFT: BloomShape = BloomShape {
        threshold: 0.0,
        bands: [
            BloomBand::new(0.35, 0.2, 1.0, Color::TRANSPARENT),
            BloomBand::new(0.35, 0.5, 1.0, Color::TRANSPARENT),
            BloomBand::new(0.15, 1.0, 1.0, Color::TRANSPARENT),
        ],
    };
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
    /// Half-life multiplier per channel (red, green, blue), 1 for an even fade. Phosphor
    /// persistence (FX15) keeps its own colour longest, so a white trace cools to it.
    pub decay: [f32; 3],
    /// How the afterglow moves while it fades (EG-34). `None` keeps it in place.
    pub drift: Option<TrailDrift>,
}

/// A feedback trail whose afterglow turns, grows and slides while it fades (EG-34,
/// `Element::trail_drift`). Each frame the faded pixels are resampled through the drift over
/// the time since the last frame, so 60 and 144 fps show the same motion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct TrailDrift {
    /// Turn about `pivot`, in degrees a second. Positive turns clockwise on screen.
    pub turn_deg_per_s: f32,
    /// Growth a second: 0.2 makes the afterglow 20% larger each second about `pivot`, -0.2
    /// 20% smaller. 0 for none.
    pub scale_per_s: f32,
    /// Slide in logical units a second, x then y.
    pub offset_px_per_s: [f32; 2],
    /// The point the afterglow turns and grows about. On an element it is in the element's
    /// own space; in the display list, in surface space (logical units).
    pub pivot: Point,
}

impl TrailDrift {
    /// Whether the drift moves nothing, so the trail takes the plain path.
    pub fn is_still(&self) -> bool {
        self.turn_deg_per_s == 0.0 && self.scale_per_s == 0.0 && self.offset_px_per_s == [0.0; 2]
    }

    /// The map from a pixel of this frame back to where its afterglow was `dt` seconds ago, in
    /// physical pixels at `ppu` pixels per logical unit: `[a, c, bx, by]`, so the source of
    /// `p` is `(a px - c py + bx, c px + a py + by)`. The drift turns by `turn * dt`, grows by
    /// `(1 + scale)^dt` and slides by `offset * dt`, so a frame gap of `dt` equals two gaps of
    /// `dt / 2` when nothing slides.
    pub fn source_map(&self, dt: f64, ppu: f32) -> [f32; 4] {
        let theta = (self.turn_deg_per_s as f64).to_radians() * dt;
        let s = (1.0 + self.scale_per_s as f64).max(0.01).powf(dt);
        // The inverse of p' = P + s R(theta) (p - P) + o.
        let (a, c) = (theta.cos() / s, -theta.sin() / s);
        let ppu = ppu as f64;
        let (px, py) = (self.pivot.x as f64 * ppu, self.pivot.y as f64 * ppu);
        let (ox, oy) = (self.offset_px_per_s[0] as f64 * ppu * dt, self.offset_px_per_s[1] as f64 * ppu * dt);
        let (qx, qy) = (px + ox, py + oy);
        let bx = px - (a * qx - c * qy);
        let by = py - (c * qx + a * qy);
        [a as f32, c as f32, bx as f32, by as f32]
    }
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
    /// A shape group (EG-22): up to [`MAX_GROUP_PRIMS`] members (`DisplayList::shape_prims`)
    /// melted together by a smooth union. `geom` is the box `x, y, w, h` that holds the
    /// result; `radii` is `start, count, k, 0`: the members' range and the union's radius `k`
    /// in layer units (0 is a plain union). Fill, stroke, glow and shadow all use the combined
    /// distance. Made by [`ShapeInstance::group`]. An element of this kind draws its box as a
    /// rounded rect: groups come from canvases.
    Group,
}

/// Most members in one shape group.
pub const MAX_GROUP_PRIMS: usize = 8;

/// The ripple amplitude cap in physical pixels: the renderer clamps `GroupPrim::ripple[0]`
/// to this.
pub const MAX_RIPPLE_PX: f32 = 2.0;

/// One member of a shape group (EG-22): a rounded rect, circle or capsule, with an optional
/// ripple along its edge.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct GroupPrim {
    /// `RRect`, `Circle` or `Capsule`; `geom` and `radii` as for a plain shape of that kind.
    pub kind: ShapeKind,
    pub geom: [f32; 4],
    pub radii: [f32; 4],
    /// Edge ripple: amplitude in layer units (the renderer caps it at [`MAX_RIPPLE_PX`]
    /// physical pixels), wavelength along the edge in layer units, and phase in turns. The
    /// edge moves out by `amp * sin(2 pi ((x + y) / wavelength + phase))`; the offset fades
    /// away from the edge as `exp(-2 pi |d| / wavelength)`, so a glow around it stays smooth.
    /// Amplitude 0 for none.
    pub ripple: [f32; 3],
}

impl GroupPrim {
    pub fn rrect(r: RRect) -> GroupPrim {
        GroupPrim {
            kind: ShapeKind::RRect,
            geom: [r.rect.x, r.rect.y, r.rect.w, r.rect.h],
            radii: r.radii,
            ripple: [0.0; 3],
        }
    }

    pub fn circle(centre: Point, radius: f32) -> GroupPrim {
        GroupPrim {
            kind: ShapeKind::Circle,
            geom: [centre.x, centre.y, radius, 0.0],
            radii: [radius; 4],
            ripple: [0.0; 3],
        }
    }

    pub fn capsule(r: Rect) -> GroupPrim {
        GroupPrim {
            kind: ShapeKind::Capsule,
            geom: [r.x, r.y, r.w, r.h],
            radii: [0.5 * r.w.min(r.h); 4],
            ripple: [0.0; 3],
        }
    }

    /// A bulge on the capsule `on`: a capsule `w` wide and `h` high centred at `cx` on its
    /// centre line. In a group with `on` and a smooth union it swells the capsule there.
    pub fn bulge(on: Rect, cx: f32, w: f32, h: f32) -> GroupPrim {
        let cy = on.y + 0.5 * on.h;
        GroupPrim::capsule(Rect::new(cx - 0.5 * w, cy - 0.5 * h, w.max(0.0), h.max(0.0)))
    }

    /// Ripples the edge by `amp` (capped at 2 physical pixels) with `wavelength` along it,
    /// shifted by `phase` turns. Move the phase with time for a travelling ripple.
    pub fn with_ripple(mut self, amp: f32, wavelength: f32, phase: f32) -> GroupPrim {
        self.ripple = [amp.max(0.0), wavelength, phase];
        self
    }

    /// The member's box, ripple included.
    pub fn bounds(&self) -> Rect {
        let g = self.geom;
        let r = match self.kind {
            ShapeKind::Circle => Rect::new(g[0] - g[2], g[1] - g[2], 2.0 * g[2], 2.0 * g[2]),
            _ => Rect::new(g[0], g[1], g[2], g[3]),
        };
        let a = self.ripple[0].max(0.0);
        Rect::new(r.x - a, r.y - a, r.w + 2.0 * a, r.h + 2.0 * a)
    }
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

/// `ShapeInstance::flags` and `VectorInstance::flags` hold the material id (FX15) from this
/// bit up.
pub const MATERIAL_SHIFT: u32 = 8;

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

    /// A shape group (EG-22): the first [`MAX_GROUP_PRIMS`] of `prims`, added to
    /// `list.shape_prims`, melted together by a smooth union of radius `k`. Where two members
    /// meet, the outline rounds into a neck up to `k / 4` outside their plain union. A copy of
    /// the instance with a glow or shadow fill shares the same members.
    pub fn group(list: &mut DisplayList, prims: &[GroupPrim], k: f32, fill: Paint) -> ShapeInstance {
        let prims = &prims[..prims.len().min(MAX_GROUP_PRIMS)];
        let start = list.shape_prims.len();
        list.shape_prims.extend_from_slice(prims);
        let k = k.max(0.0);
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for p in prims {
            let r = p.bounds();
            b = [b[0].min(r.x), b[1].min(r.y), b[2].max(r.right()), b[3].max(r.bottom())];
        }
        let geom = if prims.is_empty() {
            [0.0; 4]
        } else {
            let m = if prims.len() > 1 { 0.25 * k } else { 0.0 };
            [b[0] - m, b[1] - m, b[2] - b[0] + 2.0 * m, b[3] - b[1] + 2.0 * m]
        };
        ShapeInstance {
            kind: ShapeKind::Group,
            flags: 0,
            geom,
            radii: [start as f32, prims.len() as f32, k, 0.0],
            fill,
            stroke: Paint::NONE,
            stroke_width: 0.0,
        }
    }

    /// The members of a group instance in `list`; empty for other kinds.
    pub fn group_prims<'a>(&self, list: &'a DisplayList) -> &'a [GroupPrim] {
        if self.kind != ShapeKind::Group {
            return &[];
        }
        let start = self.radii[0] as usize;
        let n = (self.radii[1] as usize).min(MAX_GROUP_PRIMS);
        list.shape_prims.get(start..start + n).unwrap_or(&[])
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

    /// Draws the shape with material `id` from `DisplayList::add_material` (0 for none). The
    /// material reads the stroke's paint where there is a stroke, else the fill's.
    pub fn with_material(mut self, id: u32) -> ShapeInstance {
        self.flags = (self.flags & ((1 << MATERIAL_SHIFT) - 1)) | id << MATERIAL_SHIFT;
        self
    }

    /// The material id, 0 for none.
    pub fn material(&self) -> u32 {
        self.flags >> MATERIAL_SHIFT
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

/// Effects on one glyph (TY17), drawn from its coverage in the glyph shader with no extra
/// pass. Lengths are in layer units. The default is no effect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct GlyphFx {
    /// Blur: the glyph's edges soften over this width.
    pub blur: f32,
    /// Dissolve, 0 to 1: holes from a noise fixed to the glyph grow until it is gone at 1.
    pub dissolve: f32,
    /// Wipe, 0 to 1: the part hidden from the right of the glyph's ink box, with a soft front.
    pub wipe: f32,
    /// Chromatic offset: red shifts this far left and blue this far right.
    pub chroma: f32,
    /// Halo under the glyph: its colour (alpha is its strength) and how far it reaches.
    pub halo: Color,
    pub halo_radius: f32,
    /// How far the halo is moved, for a drop shadow (FX-013).
    pub halo_offset: Point,
    /// Outline width (FX-018): the glyph's edge is stroked this wide instead of filled. 0
    /// fills it.
    pub outline: f32,
    /// Material id (FX15) from `DisplayList::add_material`, 0 for none.
    pub material: u32,
    /// Weight (MO-03): each edge of the glyph grows this far, or shrinks for a negative
    /// value. The advance does not change.
    pub weight: f32,
}

impl GlyphFx {
    pub fn is_none(&self) -> bool {
        self.blur <= 0.0
            && self.dissolve <= 0.0
            && self.wipe <= 0.0
            && self.chroma == 0.0
            && (self.halo.a <= 0.0 || self.halo_radius <= 0.0)
            && self.outline <= 0.0
            && self.material == 0
            && self.weight == 0.0
    }

    /// The effects with the halo faded by `alpha`, as the glyph's colour is.
    pub fn faded(self, alpha: f32) -> GlyphFx {
        GlyphFx { halo: self.halo.fade(alpha), ..self }
    }
}

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
    /// Ring width in layer units; 0 (or the radius or more) draws a filled dot.
    pub ring: f32,
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
        self.shape_prims.clear();
        self.glyphs.clear();
        self.glyph_fx.clear();
        self.emitters.clear();
        self.emitter_sources.clear();
        self.particle_words = None;
        self.dots.clear();
        self.vectors.clear();
        self.vector_segs.clear();
        self.glass.clear();
        self.gpu.clear();
        self.signals = Default::default();
        self.materials.clear();
        self.width_tables.clear();
        self.width_knots.clear();
        self.hits.clear();
    }

    /// Adds `m` and returns its id for an instance's material. The same material as the last
    /// one added reuses its id, so the glyphs of one label share one block.
    pub fn add_material(&mut self, m: Material) -> u32 {
        if self.materials.last() != Some(&m) {
            self.materials.push(m);
        }
        self.materials.len() as u32
    }

    /// Adds a stroke width table (`path::WidthKnot`s sorted by arc length) and returns its id
    /// for `StrokeStyle::widths`.
    pub fn add_widths(&mut self, knots: &[WidthKnot]) -> u32 {
        let start = self.width_knots.len() as u32;
        self.width_knots.extend_from_slice(knots);
        self.width_tables.push([start, knots.len() as u32]);
        self.width_tables.len() as u32
    }

    /// The width table `id` points at, if any.
    pub fn widths(&self, id: u32) -> Option<&[WidthKnot]> {
        let [start, len] = *id.checked_sub(1).and_then(|i| self.width_tables.get(i as usize))?;
        self.width_knots.get(start as usize..(start + len) as usize)
    }

    /// The material an instance points at, if any.
    pub fn material(&self, id: u32) -> Option<&Material> {
        id.checked_sub(1).and_then(|i| self.materials.get(i as usize))
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

    #[test]
    fn a_group_packs_its_members_and_box() {
        let mut list = DisplayList::default();
        // Something already in the arena, so the range does not start at 0.
        list.shape_prims.push(GroupPrim::circle(Point::ZERO, 1.0));
        let pill = Rect::new(10.0, 20.0, 100.0, 24.0);
        let prims = [GroupPrim::capsule(pill), GroupPrim::bulge(pill, 60.0, 30.0, 36.0).with_ripple(1.5, 20.0, 0.25)];
        let g = ShapeInstance::group(&mut list, &prims, 8.0, Paint::solid(Color::WHITE));
        assert_eq!(g.kind, ShapeKind::Group);
        assert_eq!(g.radii, [1.0, 2.0, 8.0, 0.0]);
        assert_eq!(g.group_prims(&list), &prims);
        // The bulge is 36 high around y 32 with a 1.5 ripple, and the union may round out 2.
        let b = g.geom;
        assert_eq!(b[0], 10.0 - 2.0);
        assert_eq!(b[1], 32.0 - 18.0 - 1.5 - 2.0);
        assert_eq!(b[0] + b[2], 110.0 + 2.0);
        assert_eq!(b[1] + b[3], 32.0 + 18.0 + 1.5 + 2.0);
        assert_eq!(prims[1].ripple, [1.5, 20.0, 0.25]);
        // A plain shape has no members.
        assert!(ShapeInstance::circle(Point::ZERO, 2.0, Paint::NONE).group_prims(&list).is_empty());
    }

    #[test]
    fn a_group_keeps_at_most_eight_members() {
        let mut list = DisplayList::default();
        let prims: Vec<_> = (0..11).map(|i| GroupPrim::circle(Point::new(i as f32 * 10.0, 0.0), 4.0)).collect();
        let g = ShapeInstance::group(&mut list, &prims, 4.0, Paint::NONE);
        assert_eq!(g.radii[1] as usize, MAX_GROUP_PRIMS);
        assert_eq!(list.shape_prims.len(), MAX_GROUP_PRIMS);
        assert_eq!(g.group_prims(&list), &prims[..MAX_GROUP_PRIMS]);
        // One member: the box is the member's own, no union margin.
        let one = ShapeInstance::group(&mut list, &prims[..1], 10.0, Paint::NONE);
        assert_eq!(one.geom, [-4.0, -4.0, 8.0, 8.0]);
        // None: nothing to draw, an empty box.
        let none = ShapeInstance::group(&mut list, &[], 10.0, Paint::NONE);
        assert_eq!((none.radii[1], none.geom), (0.0, [0.0; 4]));
        // A negative radius is a plain union; a negative ripple is none.
        assert_eq!(ShapeInstance::group(&mut list, &prims[..2], -3.0, Paint::NONE).radii[2], 0.0);
        assert_eq!(prims[0].with_ripple(-1.0, 10.0, 0.0).ripple[0], 0.0);
        list.clear();
        assert!(list.shape_prims.is_empty());
    }

    /// Applies a `TrailDrift::source_map` result to `p`.
    fn src(m: [f32; 4], p: [f32; 2]) -> [f32; 2] {
        [m[0] * p[0] - m[1] * p[1] + m[2], m[1] * p[0] + m[0] * p[1] + m[3]]
    }

    #[test]
    fn trail_drift_map_scales_with_the_frame_gap() {
        use crate::geom::Point;
        let d = TrailDrift { turn_deg_per_s: 90.0, pivot: Point::new(100.0, 50.0), ..TrailDrift::default() };
        assert!(TrailDrift::default().is_still() && !d.is_still());
        // After 1 s at 90 degrees a second, a point right of the pivot sits below it (y down),
        // so this frame's pixel below the pivot reads the old pixel to its right.
        let m = d.source_map(1.0, 1.0);
        let [x, y] = src(m, [100.0, 70.0]);
        assert!((x - 120.0).abs() < 1e-3 && (y - 50.0).abs() < 1e-3, "{x} {y}");
        // At 1.5 pixels per unit the pivot scales with the pixels.
        let [x, y] = src(d.source_map(1.0, 1.5), [150.0, 105.0]);
        assert!((x - 180.0).abs() < 1e-3 && (y - 75.0).abs() < 1e-3, "{x} {y}");
        // Two gaps of 1/120 s equal one of 1/60 s, with growth too.
        let d = TrailDrift { scale_per_s: 0.3, ..d };
        let p = [40.0, 10.0];
        let one = src(d.source_map(1.0 / 60.0, 1.0), p);
        let half = d.source_map(1.0 / 120.0, 1.0);
        let two = src(half, src(half, p));
        assert!((one[0] - two[0]).abs() < 1e-3 && (one[1] - two[1]).abs() < 1e-3, "{one:?} {two:?}");
        // A gap of zero maps every pixel to itself; a slide reads from behind it.
        assert_eq!(src(d.source_map(0.0, 1.0), p), p);
        let slide = TrailDrift { offset_px_per_s: [60.0, 0.0], ..TrailDrift::default() };
        let [x, _] = src(slide.source_map(0.5, 2.0), p);
        assert!((x - (40.0 - 60.0)).abs() < 1e-3, "{x}");
    }
}
