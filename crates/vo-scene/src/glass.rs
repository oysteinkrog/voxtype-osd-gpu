//! Glass panels (FX06, FX07, FX08): the material an element asks for with `Element::glass`,
//! and the display list item the scene emits for it. `vo-render` draws it; the live host
//! sends the compositor blur region from the items' rects.
//!
//! A client cannot read the desktop behind it. With compositor blur the desktop shows
//! blurred through the tint; without it the tint alone carries the look. Either way the
//! interior tint must keep text readable over any backdrop, so [`Glass::contrast`] checks
//! text against the tint composited over pure white and pure black, in linear and in sRGB
//! blending (compositors differ), and takes the worst (AX01).

use serde::Serialize;

use crate::display::{DisplayList, IRect, InputRegion};
use crate::frame::{Corner, Frame};
use crate::geom::{Affine, Color, Point, Size};
use crate::path::{Contour, VectorSeg};
use crate::stage::Warp;

/// The sheen crosses the panel once in this many seconds after it enters.
pub const SHEEN_SECS: f64 = 0.55;
/// One lap of the orbiting rim segment.
pub const ORBIT_SECS: f64 = 6.0;
/// Frame rate of the ambient orbit (FX08 caps ambient loops at 30 fps).
pub const ORBIT_FPS: f64 = 30.0;

/// The blur region holds a panel only from this opacity up. The protocol has no blur
/// strength, so a panel fading in would otherwise show full blur behind a faint tint; the
/// blur appears halfway through the fade instead, where the tint already reads as a panel.
pub const BLUR_MIN_ALPHA: f32 = 0.5;

/// Light or dark glass: which way the edge line, the grain and the orbit go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum GlassKind {
    Light,
    Dark,
}

/// The glass material. Lengths are logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Glass {
    pub kind: GlassKind,
    /// Interior tint, sRGB. Its alpha is the interior opacity the contrast gate checks.
    pub tint: Color,
    /// Tint alpha right at the edge, rising to the interior alpha over 12 logical pixels.
    pub edge_alpha: f32,
    /// Colour of the orbiting rim segment, sRGB.
    pub accent: Color,
    /// Bevel width.
    pub bevel: f32,
    /// Rim light, Fresnel and edge line strength, 0 to about 1.5.
    pub rim: f32,
    /// Grain amount, relative to the tint.
    pub grain: f32,
    /// Offset between colour channels in the rim light.
    pub dispersion: f32,
    /// Drop shadow alpha, blur and downward offset. Alpha 0 turns it off.
    pub shadow: [f32; 3],
    /// A sheen sweeps across once when the panel enters.
    pub sheen: bool,
    /// A bright segment orbits the border while the panel is shown, at 30 fps. Off by
    /// default: an ambient loop costs frames while nothing else moves.
    pub orbit: bool,
    /// Ask the compositor to blur the desktop behind the panel, where it can.
    pub blur_behind: bool,
    /// The outline the glass follows (EG-06). Tint, bevel, rim light, sheen and the orbiting
    /// rim all follow it. The default is the panel's rounded rect.
    pub shape: GlassShape,
    /// How the glass bends what is under it (EG-05). The default bends nothing.
    pub refract: Refract,
}

/// The surface profile a refracting glass bends with (EG-05). Its slope comes from the
/// outline's signed distance (EG-06), so it follows any [`GlassShape`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub enum LensProfile {
    /// No slope: only [`Refract::magnify`] bends.
    #[default]
    Flat,
    /// A lens: the slope rises over the bevel band towards the edge, pulling what is under
    /// the rim in towards the centre.
    Dome,
    /// A thick slab: flat on top, a steep bend only over the bevel band at its edge.
    Slab,
    /// A prism: one even tilt over the whole face, moving what is under it `bend` along the
    /// direction `angle` (radians, clockwise from +x on screen, y down).
    Prism { angle: f32 },
    /// A bubble: a sphere cap over the whole shape, bending most at the rim.
    Bubble,
}

impl LensProfile {
    /// The id the shader switches on.
    pub fn id(&self) -> u32 {
        match self {
            LensProfile::Flat => 0,
            LensProfile::Dome => 1,
            LensProfile::Slab => 2,
            LensProfile::Prism { .. } => 3,
            LensProfile::Bubble => 4,
        }
    }
}

/// Refraction (EG-05): glass that reads what the popup has drawn under it (its layer's
/// backdrop, EG-01, through the texture path of EG-02) at an offset from its surface normal.
///
/// It needs a backdrop in the surface the glass draws into: put the panel in a layer with
/// `Element::backdrop(BackdropRes::Full, Color::TRANSPARENT)`. Without one, or on a tier
/// without effects, the glass draws as plain glass. It never reads the desktop.
///
/// Where the backdrop is partly transparent, the refracted copy lands over the unmoved one,
/// so a half-transparent backdrop shows a little stronger under the glass. Over opaque
/// drawing it is exact.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Refract {
    pub profile: LensProfile,
    /// The most the profile moves what is under the glass, logical pixels, where the
    /// profile is steepest.
    pub bend: f32,
    /// Magnification about the centre of the glass's box: 1 is none, 1.5 shows what is
    /// under the centre half as large again.
    pub magnify: f32,
    /// Per-channel dispersion: red is moved by `1 - dispersion` of the offset and blue by
    /// `1 + dispersion`, green by the offset itself. 0 is none; 0.1 to 0.3 gives rainbow
    /// edges.
    pub dispersion: f32,
    /// Thin-film iridescence, 0 to 1: a soap-bubble colour play added as light, strongest at
    /// the rim.
    pub film: f32,
    /// An animated wobble of the bend, logical pixels; 0 is still. It and the film's drift
    /// stop under reduced motion; the bend itself stays (it is not motion).
    pub wobble: f32,
}

impl Default for Refract {
    fn default() -> Refract {
        Refract::NONE
    }
}

impl Refract {
    /// Bends nothing: plain glass.
    pub const NONE: Refract =
        Refract { profile: LensProfile::Flat, bend: 0.0, magnify: 1.0, dispersion: 0.0, film: 0.0, wobble: 0.0 };

    /// A lens disc magnifying `magnify` times at its centre.
    pub const fn lens(magnify: f32) -> Refract {
        Refract { profile: LensProfile::Dome, bend: 6.0, magnify, dispersion: 0.12, ..Refract::NONE }
    }

    /// A prism moving what is under it `bend` logical pixels along `angle`, with rainbow edges.
    pub const fn prism(angle: f32, bend: f32) -> Refract {
        Refract { profile: LensProfile::Prism { angle }, bend, dispersion: 0.2, ..Refract::NONE }
    }

    /// A thick slab: plain in the middle, bent at the edge.
    pub const fn slab() -> Refract {
        Refract { profile: LensProfile::Slab, bend: 10.0, dispersion: 0.15, ..Refract::NONE }
    }

    /// A soap bubble: a slight magnification, a strong bend at the rim and a thin-film sheen.
    pub const fn bubble() -> Refract {
        Refract { profile: LensProfile::Bubble, bend: 8.0, magnify: 1.08, dispersion: 0.1, film: 0.8, ..Refract::NONE }
    }

    /// Whether anything under the glass moves: the glass then reads its backdrop.
    pub fn bends(&self) -> bool {
        (self.magnify - 1.0).abs() > 1e-4
            || (self.profile != LensProfile::Flat && self.bend.abs() > 1e-4)
            || self.wobble.abs() > 1e-4
    }

    /// The same refraction without anything that moves over time (reduced motion).
    pub const fn still(mut self) -> Refract {
        self.wobble = 0.0;
        self
    }
}

/// The outline of a glass panel (EG-06), laid out in the panel's box. A display list item
/// can instead carry any filled path ([`GlassInstance::outline`]).
///
/// Any shape but [`GlassShape::Rounded`] is drawn from a polygon: curves are flattened to
/// within about a tenth of a pixel at 1x. The compositor blur region follows the outline
/// (EG-07, [`outline_blur_rects`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub enum GlassShape {
    /// The panel's rounded rect, with its corner radius.
    #[default]
    Rounded,
    /// Each corner square, chamfered, rounded or notched (as [`Frame`] corners), clockwise
    /// from the top left.
    Corners(#[serde(serialize_with = "debug_corners")] [Corner; 4]),
    /// The ellipse filling the box: a circle in a square box.
    Ellipse,
    /// A regular polygon with `sides` corners on the box's ellipse, the first at the top,
    /// turned clockwise by `turn` of a full turn. Three sides make a prism's face.
    Polygon { sides: u32, turn: f32 },
    /// The rounded rect with edges that wave in and out by `amplitude`, one wave about every
    /// `wavelength` (rounded so whole waves close the loop). The waves stay inside the box.
    Wavy { amplitude: f32, wavelength: f32 },
}

fn debug_corners<S: serde::Serializer>(c: &[Corner; 4], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&format!("{c:?}"))
}

impl GlassShape {
    /// Every corner cut at 45 degrees, `size` along each edge: an octagon.
    pub const fn chamfer(size: f32) -> GlassShape {
        GlassShape::Corners([Corner::Chamfer(size); 4])
    }

    /// The outline in `rect` (x, y, w, h) clockwise on screen, or `None` for the rounded rect,
    /// which the shader draws exactly. `radius` is the panel's corner radius, used by
    /// [`GlassShape::Wavy`].
    pub fn points(&self, rect: [f32; 4], radius: f32) -> Option<Vec<Point>> {
        let [x, y, w, h] = rect;
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        let at = |p: Point| Point::new(x + p.x, y + p.y);
        let oval = |n: usize, turn: f32| -> Vec<Point> {
            (0..n)
                .map(|i| {
                    let a = std::f32::consts::TAU * (turn + i as f32 / n as f32) - std::f32::consts::FRAC_PI_2;
                    Point::new(x + 0.5 * w * (1.0 + a.cos()), y + 0.5 * h * (1.0 + a.sin()))
                })
                .collect()
        };
        let pts = match *self {
            GlassShape::Rounded => return None,
            GlassShape::Corners(c) => Frame::new().corners(c).outline(Size::new(w, h)).into_iter().map(at).collect(),
            GlassShape::Ellipse => {
                // Enough points that the chord sits within 0.1 px of the curve at 1x.
                let r = 0.5 * w.max(h);
                let n = ((std::f32::consts::PI / (1.0 - 0.1 / r.max(0.2)).clamp(-1.0, 1.0).acos()).ceil() as usize)
                    .clamp(16, 160);
                oval(n, 0.0)
            }
            GlassShape::Polygon { sides, turn } => oval(sides.max(3) as usize, turn),
            GlassShape::Wavy { amplitude, wavelength } => wavy(rect, radius, amplitude.abs(), wavelength.max(2.0)),
        };
        (pts.len() >= 3).then(|| clockwise(pts))
    }
}

/// The rounded rect `rect` inset by `amp`, its edge moved along its normal by a sine.
fn wavy(rect: [f32; 4], radius: f32, amp: f32, wavelength: f32) -> Vec<Point> {
    let [x, y, w, h] = rect;
    let (iw, ih) = (w - 2.0 * amp, h - 2.0 * amp);
    if iw <= 0.0 || ih <= 0.0 {
        return Vec::new();
    }
    let r = (radius - amp).clamp(0.0, 0.5 * iw.min(ih));
    let base: Vec<Point> = Frame::new()
        .corners([Corner::Round(r); 4])
        .outline(Size::new(iw, ih))
        .into_iter()
        .map(|p| Point::new(x + amp + p.x, y + amp + p.y))
        .collect();
    let n = base.len();
    let mut cum = Vec::with_capacity(n + 1);
    cum.push(0.0f32);
    for i in 0..n {
        let (a, b) = (base[i], base[(i + 1) % n]);
        cum.push(cum[i] + (b.x - a.x).hypot(b.y - a.y));
    }
    let total = cum[n];
    let waves = (total / wavelength).round().max(1.0);
    let samples = (waves as usize * 16).clamp(32, 512);
    // Base point at arc length `s`.
    let at = |s: f32| -> Point {
        let s = s.rem_euclid(total);
        let i = cum.partition_point(|c| *c <= s).saturating_sub(1).min(n - 1);
        let (a, b) = (base[i], base[(i + 1) % n]);
        let t = ((s - cum[i]) / (cum[i + 1] - cum[i]).max(1e-6)).clamp(0.0, 1.0);
        Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
    };
    let step = total / samples as f32;
    (0..samples)
        .map(|i| {
            let s = i as f32 * step;
            let p = at(s);
            // The normal from a central difference, so it turns smoothly round the corners.
            let (p0, p1) = (at(s - 0.5 * step), at(s + 0.5 * step));
            let (tx, ty) = (p1.x - p0.x, p1.y - p0.y);
            let len = tx.hypot(ty).max(1e-6);
            // Clockwise on screen (y down): outward is the tangent turned left.
            let (nx, ny) = (ty / len, -tx / len);
            let k = amp * (std::f32::consts::TAU * waves * s / total).sin();
            Point::new(p.x + nx * k, p.y + ny * k)
        })
        .collect()
}

/// `pts` turned clockwise on screen (y down), if they run the other way.
fn clockwise(mut pts: Vec<Point>) -> Vec<Point> {
    let n = pts.len();
    let twice_area: f32 = (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    if twice_area < 0.0 {
        pts.reverse();
    }
    pts
}

impl Glass {
    /// Dark smoked glass for light text. The interior tint keeps white text at 4.5:1 or more
    /// over pure white.
    pub const fn dark() -> Glass {
        Glass {
            kind: GlassKind::Dark,
            tint: Color::rgba(0.055, 0.063, 0.082, 0.92),
            edge_alpha: 0.42,
            accent: Color::rgba(0.45, 0.70, 1.0, 1.0),
            bevel: 12.0,
            rim: 1.0,
            grain: 0.14,
            dispersion: 0.9,
            shadow: [0.30, 14.0, 6.0],
            sheen: true,
            orbit: false,
            blur_behind: true,
            shape: GlassShape::Rounded,
            refract: Refract::NONE,
        }
    }

    /// Light frosted glass for dark text.
    pub const fn light() -> Glass {
        Glass {
            kind: GlassKind::Light,
            tint: Color::rgba(0.965, 0.97, 0.98, 0.78),
            edge_alpha: 0.40,
            accent: Color::rgba(0.20, 0.50, 1.0, 1.0),
            bevel: 12.0,
            rim: 1.0,
            grain: 0.5,
            dispersion: 0.9,
            shadow: [0.22, 16.0, 7.0],
            sheen: true,
            orbit: false,
            blur_behind: true,
            shape: GlassShape::Rounded,
            refract: Refract::NONE,
        }
    }

    /// The same glass following `shape` (EG-06).
    pub const fn with_shape(mut self, shape: GlassShape) -> Glass {
        self.shape = shape;
        self
    }

    /// The same glass bending what is under it (EG-05).
    pub const fn with_refract(mut self, refract: Refract) -> Glass {
        self.refract = refract;
        self
    }

    /// The same glass with nothing that moves over time: no sheen, no orbit, no wobble. The
    /// scene draws reduced motion with it.
    pub const fn still(mut self) -> Glass {
        self.sheen = false;
        self.orbit = false;
        self.refract = self.refract.still();
        self
    }

    /// Dark or light glass of `kind`.
    pub const fn of_kind(kind: GlassKind) -> Glass {
        match kind {
            GlassKind::Dark => Glass::dark(),
            GlassKind::Light => Glass::light(),
        }
    }

    /// The text colours a preset of `kind` is checked against (AX01): body text and captions.
    pub const fn preset_texts(kind: GlassKind) -> [Color; 2] {
        match kind {
            GlassKind::Dark => [Color::rgba(0.96, 0.97, 0.98, 1.0), Color::rgba(0.82, 0.84, 0.88, 1.0)],
            GlassKind::Light => [Color::rgba(0.07, 0.08, 0.10, 1.0), Color::rgba(0.24, 0.26, 0.30, 1.0)],
        }
    }

    /// `self` with the lowest tint alpha that keeps [`Glass::preset_texts`] at 4.5:1 (AX01), so
    /// as much of the bent backdrop shows as text allows.
    fn clearest(self) -> Glass {
        let t = self.tint;
        self.tinted_for(Color::rgba(t.r, t.g, t.b, 1.0), &Glass::preset_texts(self.kind), 4.5).unwrap_or(self)
    }

    /// A lens disc (EG-05): a circle in a square box, magnifying 1.5 times at its centre.
    pub fn lens(kind: GlassKind) -> Glass {
        Glass { sheen: false, ..Glass::of_kind(kind) }
            .with_shape(GlassShape::Ellipse)
            .with_refract(Refract::lens(1.5))
            .clearest()
    }

    /// A prism (EG-05): a triangle face moving what is under it to the right, with rainbow edges.
    pub fn prism(kind: GlassKind) -> Glass {
        Glass { sheen: false, ..Glass::of_kind(kind) }
            .with_shape(GlassShape::Polygon { sides: 3, turn: 0.0 })
            .with_refract(Refract::prism(0.0, 12.0))
            .clearest()
    }

    /// A thick glass slab (EG-05): the panel's rounded rect, bent at its edge.
    pub fn slab(kind: GlassKind) -> Glass {
        Glass { bevel: 16.0, ..Glass::of_kind(kind) }.with_refract(Refract::slab()).clearest()
    }

    /// A bubble with thin-film iridescence (EG-05), slowly wobbling (still under reduced motion).
    pub fn bubble(kind: GlassKind) -> Glass {
        Glass { sheen: false, shadow: [0.0; 3], ..Glass::of_kind(kind) }
            .with_shape(GlassShape::Ellipse)
            .with_refract(Refract { wobble: 1.5, ..Refract::bubble() })
            .clearest()
    }

    /// The same glass with the orbiting rim on.
    pub const fn with_orbit(mut self, accent: Color) -> Glass {
        self.orbit = true;
        self.accent = accent;
        self
    }

    /// The same glass tinted `tint` (sRGB, alpha ignored), with the lowest interior alpha
    /// that keeps every colour in `texts` at `ratio` or more (AX01: 4.5) over pure white and
    /// pure black. Use it for theme colours (LY03), where the tint is not known ahead. `None`
    /// when no alpha reaches the ratio, for example light text on a light tint.
    pub fn tinted_for(self, tint: Color, texts: &[Color], ratio: f32) -> Option<Glass> {
        let with = |a: f32| Glass { tint: Color::rgba(tint.r, tint.g, tint.b, a), ..self };
        let ok = |a: f32| texts.iter().all(|t| with(a).contrast(*t, 0.02) >= ratio);
        if !ok(1.0) {
            return None;
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..20 {
            let mid = 0.5 * (lo + hi);
            if ok(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        // Round up to the next 1/256 so 8-bit storage cannot drop below the bar.
        let a = ((hi * 256.0).ceil() / 256.0).min(1.0);
        Some(Glass { edge_alpha: self.edge_alpha.min(a), ..with(a) })
    }

    /// The lowest contrast ratio of `text` against the panel interior, over pure white and
    /// pure black backdrops, blended in linear light and in sRGB. `light_margin` is the most
    /// luminance the glass's own light (rim, Fresnel, grain) adds in the interior.
    pub fn contrast(&self, text: Color, light_margin: f32) -> f32 {
        let text_l = luminance([text.r, text.g, text.b].map(srgb_to_linear));
        let tint = [self.tint.r, self.tint.g, self.tint.b];
        let a = self.tint.a;
        let mut worst = f32::INFINITY;
        for back in [0.0f32, 1.0] {
            let linear = tint.map(|c| srgb_to_linear(c) * a + back * (1.0 - a));
            let srgb = tint.map(|c| srgb_to_linear(c * a + back * (1.0 - a)));
            for panel in [linear, srgb] {
                let l = luminance(panel);
                // The margin moves the panel towards the text's own luminance: the worse way.
                let l = if text_l > l { l + light_margin } else { (l - light_margin).max(0.0) };
                worst = worst.min(ratio(text_l, l));
            }
        }
        worst
    }

    /// What moves at `t` for a panel that entered at `since`: sheen progress, orbit head and
    /// orbit strength.
    pub fn motion(&self, since: f64, t: f64) -> (Option<f32>, Option<f32>, f32) {
        let age = (t - since).max(0.0);
        let sheen = (self.sheen && age < SHEEN_SECS).then(|| (age / SHEEN_SECS) as f32);
        let orbit_t = age - if self.sheen { SHEEN_SECS } else { 0.0 };
        let orbit = (self.orbit && orbit_t >= 0.0).then(|| (orbit_t / ORBIT_SECS).fract() as f32);
        (sheen, orbit, (orbit_t / 0.4).clamp(0.0, 1.0) as f32 * 1.1)
    }

    /// When the panel next needs a frame, or `None` once it is still: 60 fps during the
    /// sheen, then 30 fps for the orbit or a refraction wobble (EG-05).
    pub fn next_frame(&self, since: f64, t: f64) -> Option<f64> {
        let age = t - since;
        if self.sheen && age < SHEEN_SECS {
            Some(t + 1.0 / 60.0)
        } else if self.orbit || self.refract.wobble != 0.0 {
            Some(t + 1.0 / ORBIT_FPS)
        } else {
            None
        }
    }
}

/// One glass panel in the display list, drawn under the panel's content.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct GlassInstance {
    /// The panel in its layer's space, logical units: x, y, w, h.
    pub rect: [f32; 4],
    pub radius: f32,
    pub glass: Glass,
    /// Opacity, the node's own times its layer's chain is applied by the renderer.
    pub alpha: f32,
    /// Seconds since the panel entered.
    pub age: f32,
    pub sheen: Option<f32>,
    pub orbit: Option<f32>,
    pub orbit_strength: f32,
    /// The panel's layer space to surface space, for the blur region.
    pub to_surface: Affine,
    /// The panel's opacity on the surface: `alpha` times every enclosing layer's opacity.
    pub surface_alpha: f32,
    /// Any filled path as the outline (EG-06): a range `[first, count]` into
    /// `DisplayList::vector_segs` holding one closed contour in the layer's space, made by
    /// [`push_outline`]. It wins over `glass.shape`. `rect` must hold it: it bounds the draw
    /// and lays out the sheen. `None` uses `glass.shape`.
    pub outline: Option<[u32; 2]>,
}

impl GlassInstance {
    /// Whether the panel is something other than its rounded rect.
    pub fn is_shaped(&self) -> bool {
        self.outline.is_some() || self.glass.shape != GlassShape::Rounded
    }

    /// The outline as a polygon in the layer's space, clockwise on screen, or `None` for the
    /// rounded rect. `segs` is the list's `vector_segs`.
    pub fn outline_points(&self, segs: &[VectorSeg]) -> Option<Vec<Point>> {
        let Some([first, count]) = self.outline else {
            return self.glass.shape.points(self.rect, self.radius);
        };
        let segs = segs.get(first as usize..(first + count) as usize)?;
        let mut pts = Vec::with_capacity(segs.len());
        for s in segs {
            // A straight segment has its control point on the chord; a curved one is cut in
            // pieces, one per 2 layer units of bend.
            let mid = Point::new(0.5 * (s.p0.x + s.p2.x), 0.5 * (s.p0.y + s.p2.y));
            let bend = (s.p1.x - mid.x).hypot(s.p1.y - mid.y);
            let n = if bend < 0.05 { 1 } else { ((bend / 2.0).sqrt().ceil() as usize * 4).clamp(4, 32) };
            for i in 0..n {
                let t = i as f32 / n as f32;
                let u = 1.0 - t;
                let q = |a: f32, b: f32, c: f32| u * u * a + 2.0 * u * t * b + t * t * c;
                pts.push(Point::new(q(s.p0.x, s.p1.x, s.p2.x), q(s.p0.y, s.p1.y, s.p2.y)));
            }
        }
        (pts.len() >= 3).then(|| clockwise(pts))
    }
}

/// Appends `contour` (closed by a straight line if open) to `list.vector_segs` as a glass
/// outline. Returns the range for [`GlassInstance::outline`] and the contour's bounding rect
/// (x, y, w, h) for [`GlassInstance::rect`].
pub fn push_outline(list: &mut DisplayList, contour: &Contour) -> ([u32; 2], [f32; 4]) {
    let first = list.vector_segs.len() as u32;
    list.vector_segs.extend_from_slice(&contour.segs);
    let (mut x0, mut y0, mut x1, mut y1) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    for s in &contour.segs {
        // A quadratic stays inside its control triangle.
        for p in [s.p0, s.p1, s.p2] {
            (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
        }
    }
    let count = list.vector_segs.len() as u32 - first;
    if count == 0 {
        return ([first, 0], [0.0; 4]);
    }
    ([first, count], [x0, y0, x1 - x0, y1 - y0])
}

/// Rectangles covering a rounded rect, one band per pixel row of the corner radius, each
/// rounded outward to whole logical pixels, for the compositor blur region. Coarser bands
/// leave visible blur steps outside the corner (the stair-step test in the `blur_probe`
/// example of `vo-host-wayland`).
pub fn blur_rects(rect: [f32; 4], radius: f32, out: &mut Vec<IRect>) {
    blur_rects_in(rect, radius, None, out);
}

/// Coordinates past this many logical pixels are cut to it before they become integers, so
/// widths and heights cannot overflow an `i32`.
const FAR: f32 = 1.0e8;

/// [`blur_rects`] cut to `clip` (RV-01): only the corner rows inside it are walked, and every
/// rect is cut to it. With `clip` `None`, or a rect inside it, the output is [`blur_rects`]'s.
fn blur_rects_in(rect: [f32; 4], radius: f32, clip: Option<IRect>, out: &mut Vec<IRect>) {
    let far = |v: f32| v.clamp(-FAR, FAR);
    let (x0, y0) = (far(rect[0]).floor() as i32, far(rect[1]).floor() as i32);
    let (x1, y1) = (far(rect[0] + rect[2]).ceil() as i32, far(rect[1] + rect[3]).ceil() as i32);
    let r = radius.min(rect[2] / 2.0).min(rect[3] / 2.0).clamp(0.0, FAR);
    let ri = r.ceil() as i32;
    let mut push = |rect: IRect| match clip {
        None => out.push(rect),
        Some(c) => {
            let (l, t) = (rect.x.max(c.x), rect.y.max(c.y));
            let (rr, b) = ((rect.x + rect.w).min(c.x + c.w), (rect.y + rect.h).min(c.y + c.h));
            if rr > l && b > t {
                out.push(IRect { x: l, y: t, w: rr - l, h: b - t });
            }
        }
    };
    // The corner rows that can show: those whose top or bottom row lies in the clip.
    let (lo, hi) = match clip {
        None => (0, ri),
        Some(c) => {
            let top = (c.y - y0).max(0)..(c.y + c.h - y0).min(ri);
            let bottom = (y1 - c.y - c.h).max(0)..(y1 - c.y).min(ri);
            match (top.is_empty(), bottom.is_empty()) {
                (true, true) => (0, 0),
                (false, true) => (top.start, top.end),
                (true, false) => (bottom.start, bottom.end),
                (false, false) => (top.start.min(bottom.start), top.end.max(bottom.end)),
            }
        }
    };
    for row in lo..hi {
        // The row's edge nearest the straight side has the smallest inset in the row.
        let dy = (r - (row + 1) as f32).max(0.0);
        let inset = (r - (r * r - dy * dy).sqrt()).floor() as i32;
        let w = x1 - x0 - 2 * inset;
        push(IRect { x: x0 + inset, y: y0 + row, w, h: 1 });
        push(IRect { x: x0 + inset, y: y1 - row - 1, w, h: 1 });
    }
    push(IRect { x: x0, y: y0 + ri, w: x1 - x0, h: (y1 - y0 - 2 * ri).max(0) });
}

/// Most rectangles one outline's blur region may hold (EG-07). KWin keeps the region as a
/// list of rectangles and walks it on every repaint of what is behind the surface, so a
/// region costs more the more rectangles it has. An outline that needs more rows than this
/// is cut in bands of 2, 4, 8 ... rows until it fits; each band takes only what lies inside
/// the outline over its whole height, so a coarser band covers less but never spills.
pub const MAX_BLUR_RECTS: usize = 256;

/// A non-glass element that asks the compositor to blur the desktop behind its outline
/// (EG-07, `Element::blur_behind`): a translucent fill or frame that wants frosted glass
/// under it without the glass material.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BlurBehind {
    /// The element's box in its layer's space, logical units: x, y, w, h.
    pub rect: [f32; 4],
    /// The element's corner radius, for [`GlassShape::Rounded`].
    pub radius: f32,
    /// The outline in the box.
    pub shape: GlassShape,
    /// The element's layer, for its transform and perspective warp.
    pub layer: u32,
    /// The element's opacity on the surface, every enclosing layer's included.
    pub surface_alpha: f32,
}

/// Rectangles covering the polygon `pts` (surface logical pixels, either winding, even-odd
/// fill) for the compositor blur region, at `scale` physical pixels per logical pixel.
///
/// The region is one band per logical pixel row, the finest a `wl_region` can say: each row
/// keeps the spans that lie inside the outline over the whole row, widened by less than one
/// physical pixel diagonally and rounded to whole logical pixels. So no blur shows more than
/// one physical pixel past a slanted, curved or rotated edge, and the rows step along it as
/// [`blur_rects`] steps along a rounded corner. Rows with the same spans merge into one
/// rectangle. At most [`MAX_BLUR_RECTS`] rectangles.
pub fn outline_blur_rects(pts: &[Point], scale: f32, out: &mut Vec<IRect>) {
    outline_blur_rects_in(pts, scale, None, out);
}

/// [`outline_blur_rects`] cut to `clip` (RV-01): only the rows inside it are walked and every
/// rect is cut to it, so a point far off the surface (a layer tilted towards the eye) costs
/// no more than the surface's height. With `clip` `None`, or an outline inside it, the
/// output is [`outline_blur_rects`]'s.
pub fn outline_blur_rects_in(pts: &[Point], scale: f32, clip: Option<IRect>, out: &mut Vec<IRect>) {
    if pts.len() < 3 || pts.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
        return;
    }
    let scale = if scale > 0.0 { scale } else { 1.0 };
    // A rect corner sits within `tol` across and `tol` down of an inside point: under one
    // physical pixel away along the diagonal.
    let tol = 0.7 / scale;
    let (ymin, ymax) = pts.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), p| (a.min(p.y), b.max(p.y)));
    let (mut y0, mut y1) = (ymin.clamp(-FAR, FAR).floor() as i32, ymax.clamp(-FAR, FAR).ceil() as i32);
    let mut xs = [-(FAR as i32), FAR as i32];
    if let Some(c) = clip {
        (y0, y1) = (y0.max(c.y), y1.min(c.y + c.h));
        xs = [c.x, c.x + c.w];
    }
    if y1 <= y0 {
        return;
    }
    let edges = Edges::new(pts);
    let mut band = 1;
    loop {
        let start = out.len();
        rows(&edges, [y0, y1], xs, band, tol, out);
        if out.len() - start <= MAX_BLUR_RECTS || band >= 1 << 16 {
            return;
        }
        out.truncate(start);
        band *= 2;
    }
}

/// The polygon's edges for a sweep down the rows (RV-01): each band looks only at the edges
/// that reach into it and the vertices inside it, not at every point, so a pass costs about
/// the rows plus the points rather than rows times points.
struct Edges {
    /// Non-horizontal edges `(top y, bottom y, from, to)`, `from` to `to` in outline order,
    /// sorted by top y.
    edges: Vec<(f32, f32, Point, Point)>,
    /// Every vertex height, sorted, without repeats.
    ys: Vec<f32>,
}

impl Edges {
    fn new(pts: &[Point]) -> Edges {
        let n = pts.len();
        let mut edges: Vec<(f32, f32, Point, Point)> = (0..n)
            .map(|i| (pts[i], pts[(i + 1) % n]))
            .filter(|(a, b)| a.y != b.y)
            .map(|(a, b)| (a.y.min(b.y), a.y.max(b.y), a, b))
            .collect();
        edges.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut ys: Vec<f32> = pts.iter().map(|p| p.y).collect();
        ys.sort_by(f32::total_cmp);
        ys.dedup();
        Edges { edges, ys }
    }
}

/// One pass of [`outline_blur_rects`] over rows `y0..y1` with bands `band` rows tall, rects
/// cut to columns `cx0..cx1`.
fn rows(edges: &Edges, [y0, y1]: [i32; 2], [cx0, cx1]: [i32; 2], band: i32, tol: f32, out: &mut Vec<IRect>) {
    let ty = tol.min(0.45);
    let mut open: Vec<usize> = Vec::new();
    let mut spans: Vec<[f32; 2]> = Vec::new();
    let mut at: Vec<[f32; 2]> = Vec::new();
    let mut both: Vec<[f32; 2]> = Vec::new();
    let mut xs: Vec<f32> = Vec::new();
    // The edges that reach into the band: added by top y, dropped once wholly above it.
    let mut active: Vec<usize> = Vec::new();
    let mut next_edge = 0;
    let mut y = y0;
    while y < y1 {
        let yb = (y + band).min(y1);
        let (lo, hi) = (y as f32 + ty, yb as f32 - ty);
        while next_edge < edges.edges.len() && edges.edges[next_edge].0 <= hi {
            active.push(next_edge);
            next_edge += 1;
        }
        active.retain(|&i| edges.edges[i].1 > lo);
        // The spans inside over the whole band: the crossings move linearly between vertex
        // heights, so the band's ends and both sides of each vertex inside it are enough.
        cross(edges, &active, lo, &mut xs, &mut spans);
        let mut probe = |s: f32, spans: &mut Vec<[f32; 2]>| {
            cross(edges, &active, s, &mut xs, &mut at);
            intersect(spans, &at, &mut both);
            std::mem::swap(spans, &mut both);
        };
        probe(hi, &mut spans);
        let first = edges.ys.partition_point(|&v| v <= lo);
        for &v in edges.ys[first..].iter().take_while(|&&v| v < hi) {
            probe((v - 1e-3).max(lo), &mut spans);
            probe((v + 1e-3).min(hi), &mut spans);
        }
        let mut next = Vec::with_capacity(spans.len());
        for [a, b] in &spans {
            let l = ((a - tol).clamp(-FAR, FAR).ceil() as i32).max(cx0);
            let r = ((b + tol).clamp(-FAR, FAR).floor() as i32).min(cx1);
            if r <= l {
                continue;
            }
            // A span the band above had too grows that rect down.
            if let Some(&i) = open.iter().find(|&&i| out[i].x == l && out[i].w == r - l) {
                out[i].h += yb - y;
                next.push(i);
            } else {
                out.push(IRect { x: l, y, w: r - l, h: yb - y });
                next.push(out.len() - 1);
            }
        }
        // Every rect in `open` ends where the next band starts.
        open = next;
        y = yb;
    }
}

/// The spans of `y` inside the polygon (even-odd), sorted, into `out`. Only the `active`
/// edges can cross `y`. `xs` is scratch.
fn cross(edges: &Edges, active: &[usize], y: f32, xs: &mut Vec<f32>, out: &mut Vec<[f32; 2]>) {
    xs.clear();
    for &i in active {
        let (_, _, a, b) = edges.edges[i];
        if (a.y <= y) != (b.y <= y) {
            xs.push(a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y));
        }
    }
    xs.sort_by(f32::total_cmp);
    out.clear();
    out.extend(xs.as_chunks::<2>().0.iter().copied());
}

/// The intersection of two sorted span lists.
fn intersect(a: &[[f32; 2]], b: &[[f32; 2]], out: &mut Vec<[f32; 2]>) {
    out.clear();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (l, r) = (a[i][0].max(b[j][0]), a[i][1].min(b[j][1]));
        if r > l {
            out.push([l, r]);
        }
        if a[i][1] < b[j][1] {
            i += 1;
        } else {
            j += 1;
        }
    }
}

/// The outline of `shape` in `rect` with corner `radius` as a polygon, the rounded rect too.
fn shape_points(shape: GlassShape, rect: [f32; 4], radius: f32) -> Option<Vec<Point>> {
    if shape != GlassShape::Rounded {
        return shape.points(rect, radius);
    }
    let [x, y, w, h] = rect;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = radius.clamp(0.0, 0.5 * w.min(h));
    let pts = Frame::new().corners([Corner::Round(r); 4]).outline(Size::new(w, h));
    Some(pts.into_iter().map(|p| Point::new(x + p.x, y + p.y)).collect())
}

impl DisplayList {
    /// The compositor blur region in surface logical pixels: the union of the glass panels
    /// that ask for blur behind and the [`BlurBehind`] elements, each at least
    /// [`BLUR_MIN_ALPHA`] opaque. Empty when there are none.
    ///
    /// An upright rounded panel gets [`blur_rects`]. Any other outline (a shape, a path, a
    /// rotated or perspective-tilted layer) gets [`outline_blur_rects`], which follows it to
    /// within one physical pixel.
    ///
    /// Every rect is cut to the surface (RV-01) when the list has a size, so an outline
    /// with a point far off the surface costs no more than one on it. [`BlurCache`] builds
    /// the region only when what it depends on changed.
    pub fn blur_region(&self) -> InputRegion {
        let mut rects = Vec::new();
        let clip = self.blur_clip();
        let warps = self.glass_warps();
        let seen = |p: Point, m: &Affine, w: &Option<Warp>| {
            let p = m.apply(p);
            w.map_or(p, |w| w.apply(p))
        };
        for (g, warp) in self.glass.iter().zip(&warps) {
            if !g.glass.blur_behind || g.surface_alpha < BLUR_MIN_ALPHA {
                continue;
            }
            let m = g.to_surface;
            if warp.is_none() && m.is_axis_aligned() && !g.is_shaped() {
                let [x, y, w, h] = g.rect;
                let p0 = m.apply(Point { x, y });
                let p1 = m.apply(Point { x: x + w, y: y + h });
                let k = (m.a * m.a + m.b * m.b).sqrt();
                let r = [p0.x.min(p1.x), p0.y.min(p1.y), (p1.x - p0.x).abs(), (p1.y - p0.y).abs()];
                blur_rects_in(r, g.radius * k, clip, &mut rects);
                continue;
            }
            let pts = g.outline_points(&self.vector_segs).or_else(|| shape_points(GlassShape::Rounded, g.rect, g.radius));
            if let Some(pts) = pts {
                let pts: Vec<Point> = pts.into_iter().map(|p| seen(p, &m, warp)).collect();
                outline_blur_rects_in(&pts, self.scale, clip, &mut rects);
            }
        }
        for b in self.blur_behind.iter().filter(|b| b.surface_alpha >= BLUR_MIN_ALPHA) {
            let Some(layer) = self.layers.get(b.layer as usize) else { continue };
            let m = layer.transform;
            if layer.warp.is_none() && m.is_axis_aligned() && b.shape == GlassShape::Rounded {
                let [x, y, w, h] = b.rect;
                let p0 = m.apply(Point { x, y });
                let p1 = m.apply(Point { x: x + w, y: y + h });
                let k = (m.a * m.a + m.b * m.b).sqrt();
                let r = [p0.x.min(p1.x), p0.y.min(p1.y), (p1.x - p0.x).abs(), (p1.y - p0.y).abs()];
                blur_rects_in(r, b.radius * k, clip, &mut rects);
            } else if let Some(pts) = shape_points(b.shape, b.rect, b.radius) {
                let pts: Vec<Point> = pts.into_iter().map(|p| seen(p, &m, &layer.warp)).collect();
                outline_blur_rects_in(&pts, self.scale, clip, &mut rects);
            }
        }
        InputRegion { rects }
    }

    /// The surface in whole logical pixels, or `None` for a list with no size.
    fn blur_clip(&self) -> Option<IRect> {
        let (w, h) = (self.size.w.clamp(0.0, FAR).ceil() as i32, self.size.h.clamp(0.0, FAR).ceil() as i32);
        (w > 0 && h > 0).then_some(IRect { x: 0, y: 0, w, h })
    }

    /// The perspective warp each glass panel is seen through, by index into `glass`.
    fn glass_warps(&self) -> Vec<Option<Warp>> {
        let mut warps = vec![None; self.glass.len()];
        for layer in self.layers.iter().filter(|l| l.warp.is_some()) {
            let Some(items) = self.items.get(layer.items.start as usize..layer.items.end as usize) else { continue };
            for it in items {
                if it.kind == crate::display::ItemKind::Glass {
                    for i in it.start..it.start + it.len {
                        if let Some(w) = warps.get_mut(i as usize) {
                            *w = layer.warp;
                        }
                    }
                }
            }
        }
        warps
    }

    /// Everything [`DisplayList::blur_region`] reads, into `key`: two lists with equal keys
    /// have equal blur regions.
    fn blur_key(&self, key: &mut BlurKey) {
        key.scale = self.scale;
        key.clip = self.blur_clip();
        key.glass.clear();
        key.segs.clear();
        key.behind.clear();
        for (g, warp) in self.glass.iter().zip(self.glass_warps()) {
            if !g.glass.blur_behind || g.surface_alpha < BLUR_MIN_ALPHA {
                continue;
            }
            let segs = g.outline.and_then(|[first, count]| self.vector_segs.get(first as usize..(first + count) as usize));
            key.segs.extend_from_slice(segs.unwrap_or_default());
            key.glass.push(GlassKey {
                rect: g.rect,
                radius: g.radius,
                shape: g.glass.shape,
                outline: g.outline.map(|_| segs.map_or(0, |s| s.len())),
                to_surface: g.to_surface,
                warp,
            });
        }
        for b in self.blur_behind.iter().filter(|b| b.surface_alpha >= BLUR_MIN_ALPHA) {
            let layer = self.layers.get(b.layer as usize).map(|l| (l.transform, l.warp));
            key.behind.push((b.rect, b.radius, b.shape, layer));
        }
    }
}

/// What one glass panel adds to [`BlurKey`].
#[derive(Clone, Debug, PartialEq)]
struct GlassKey {
    rect: [f32; 4],
    radius: f32,
    shape: GlassShape,
    /// The number of outline segments in `BlurKey::segs`, `None` for no outline.
    outline: Option<usize>,
    to_surface: Affine,
    warp: Option<Warp>,
}

/// A layer's transform and warp, in [`BlurKey`].
type LayerKey = (Affine, Option<Warp>);

/// The inputs of a list's blur region (RV-01). A NaN anywhere makes it unequal to itself,
/// which only costs a rebuild.
#[derive(Clone, Debug, Default, PartialEq)]
struct BlurKey {
    scale: f32,
    clip: Option<IRect>,
    glass: Vec<GlassKey>,
    /// The outline segments of the panels in `glass`, in order.
    segs: Vec<VectorSeg>,
    /// Per [`BlurBehind`] that holds blur: its box, radius, shape and its layer's transform
    /// and warp.
    behind: Vec<([f32; 4], f32, GlassShape, Option<LayerKey>)>,
}

/// [`DisplayList::blur_region`] kept across samples (RV-01). The live host samples the scene
/// every frame; the region is built again only when an input to it changed: a panel's box,
/// radius, shape, outline, transform or warp, which panels hold blur, the scale or the
/// surface size. A 1000-point outline then costs a key compare per frame, not a rasterise.
#[derive(Clone, Debug, Default)]
pub struct BlurCache {
    key: Option<BlurKey>,
    spare: BlurKey,
    region: InputRegion,
    builds: u64,
}

impl BlurCache {
    /// The blur region of `list`, built only when its inputs differ from the last call's.
    pub fn region(&mut self, list: &DisplayList) -> &InputRegion {
        list.blur_key(&mut self.spare);
        if self.key.as_ref() != Some(&self.spare) {
            self.region = list.blur_region();
            self.builds += 1;
            let old = self.key.replace(std::mem::take(&mut self.spare));
            self.spare = old.unwrap_or_default();
        }
        &self.region
    }

    /// How many times [`BlurCache::region`] built the region.
    pub fn builds(&self) -> u64 {
        self.builds
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn luminance(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

fn ratio(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tinted_glass_finds_the_lowest_alpha_that_passes() {
        let white = Color::rgba(0.96, 0.97, 0.98, 1.0);
        let blue = Color::rgba(0.10, 0.20, 0.45, 1.0);
        let g = Glass::dark().tinted_for(blue, &[white], 4.5).expect("white on deep blue passes");
        assert!(g.contrast(white, 0.02) >= 4.5);
        let lower = Glass { tint: Color { a: g.tint.a - 2.0 / 256.0, ..g.tint }, ..g };
        assert!(lower.contrast(white, 0.02) < 4.5, "alpha {} is not the lowest", g.tint.a);
        // White text on a pale tint can never pass.
        assert_eq!(Glass::light().tinted_for(Color::rgba(0.9, 0.9, 0.9, 1.0), &[white], 4.5), None);
    }

    #[test]
    fn a_still_panel_stops_asking_for_frames() {
        let g = Glass::dark();
        assert!(g.next_frame(0.0, 0.1).is_some());
        assert_eq!(g.next_frame(0.0, SHEEN_SECS + 0.01), None);
        let o = g.with_orbit(Color::WHITE);
        let next = o.next_frame(0.0, 2.0).expect("orbit keeps going");
        assert!((next - 2.0 - 1.0 / ORBIT_FPS).abs() < 1e-9);
    }

    #[test]
    fn a_faint_panel_has_no_blur_yet() {
        use crate::element::Element;
        use crate::geom::Size;
        use crate::scene::Scene;
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(200.0, 100.0), 1.0);
        let panel = |o: f32| Element::panel("panel").size(120.0, 60.0).opacity(o).glass(Glass::dark());
        scene.apply(Element::column("root").child(panel(0.3)), 0.0);
        assert!(scene.sample(5.0).blur_region().rects.is_empty());
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(200.0, 100.0), 1.0);
        scene.apply(Element::column("root").child(panel(0.8)), 0.0);
        assert!(!scene.sample(5.0).blur_region().rects.is_empty());
    }

    #[test]
    fn reduced_motion_keeps_glass_still() {
        use crate::element::Element;
        use crate::geom::Size;
        use crate::scene::Scene;
        let mut scene: Scene<()> = Scene::new().with_reduced_motion(true);
        scene.set_viewport(0.0, Size::new(200.0, 100.0), 1.0);
        let glass = Glass::dark().with_orbit(Color::WHITE);
        scene.apply(Element::column("root").child(Element::panel("panel").size(120.0, 60.0).glass(glass)), 0.0);
        let list = scene.sample(0.1);
        assert_eq!((list.glass[0].sheen, list.glass[0].orbit), (None, None));
        assert_eq!(scene.next_wake(2.0), None);
    }

    #[test]
    fn a_glass_panel_draws_one_item_and_wakes_at_30_fps() {
        use crate::display::ItemKind;
        use crate::element::Element;
        use crate::geom::Size;
        use crate::scene::Scene;
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(200.0, 100.0), 1.0);
        let view = || {
            Element::column("root").child(
                Element::panel("panel").size(120.0, 60.0).radius(10.0).glass(Glass::dark().with_orbit(Color::WHITE)),
            )
        };
        scene.apply(view(), 0.0);
        let list = scene.sample(0.1);
        assert_eq!(list.glass.len(), 1);
        assert_eq!(list.items.iter().filter(|i| i.kind == ItemKind::Glass).count(), 1);
        assert_eq!(list.glass[0].rect, [0.0, 0.0, 120.0, 60.0]);
        assert!(list.glass[0].sheen.is_some());
        // During the sheen the scene wants frames now; after it, every 1/30 s.
        assert_eq!(scene.next_wake(0.1), Some(0.1 + 1.0 / 60.0));
        let t = SHEEN_SECS + 1.0;
        assert_eq!(scene.next_wake(t), Some(t + 1.0 / ORBIT_FPS));
        assert!(!list.blur_region().rects.is_empty());
        // Without the orbit the scene settles once the sheen is over.
        scene.apply(
            Element::column("root").child(Element::panel("panel").size(120.0, 60.0).radius(10.0).glass(Glass::dark())),
            t,
        );
        assert_eq!(scene.next_wake(t + 0.1), None);
    }

    fn inside_rect(pts: &[Point], rect: [f32; 4]) -> bool {
        let [x, y, w, h] = rect;
        pts.iter().all(|p| p.x >= x - 1e-3 && p.y >= y - 1e-3 && p.x <= x + w + 1e-3 && p.y <= y + h + 1e-3)
    }

    fn twice_area(pts: &[Point]) -> f32 {
        let n = pts.len();
        (0..n).map(|i| pts[i].x * pts[(i + 1) % n].y - pts[(i + 1) % n].x * pts[i].y).sum()
    }

    #[test]
    fn shapes_give_clockwise_outlines_inside_the_box() {
        let rect = [10.0, 20.0, 172.0, 110.0];
        assert_eq!(GlassShape::Rounded.points(rect, 12.0), None);
        let shapes = [
            GlassShape::chamfer(12.0),
            GlassShape::Corners([Corner::Chamfer(20.0), Corner::Round(10.0), Corner::Notch(8.0), Corner::Square]),
            GlassShape::Ellipse,
            GlassShape::Polygon { sides: 3, turn: 0.0 },
            GlassShape::Wavy { amplitude: 4.0, wavelength: 30.0 },
        ];
        for s in shapes {
            let pts = s.points(rect, 16.0).unwrap_or_else(|| panic!("{s:?} has no outline"));
            assert!(pts.len() >= 3, "{s:?}");
            assert!(inside_rect(&pts, rect), "{s:?} leaves its box");
            assert!(twice_area(&pts) > 0.0, "{s:?} is not clockwise");
        }
        // The octagon: eight points, its cut corners 12 along each edge.
        let oct = GlassShape::chamfer(12.0).points(rect, 0.0).unwrap();
        assert_eq!(oct.len(), 8);
        assert_eq!((oct[0], oct[1]), (Point::new(10.0, 32.0), Point::new(22.0, 20.0)));
        // A triangle's first corner is at the top centre.
        let tri = GlassShape::Polygon { sides: 3, turn: 0.0 }.points(rect, 0.0).unwrap();
        assert!((tri[0].x - 96.0).abs() < 1e-3 && (tri[0].y - 20.0).abs() < 1e-3, "{:?}", tri[0]);
    }

    #[test]
    fn a_path_outline_is_flattened_and_bounded() {
        use crate::path::Outline;
        let mut path = kurbo::BezPath::new();
        path.move_to((0.0, 0.0));
        path.line_to((100.0, 0.0));
        path.quad_to((140.0, 50.0), (100.0, 100.0));
        path.line_to((0.0, 100.0));
        path.close_path();
        let outline = Outline::new(&path, 0.1);
        let mut list = DisplayList::default();
        let (range, rect) = push_outline(&mut list, &outline.contours[0]);
        assert!(range[1] >= 4);
        assert_eq!([rect[0], rect[1], rect[3]], [0.0, 0.0, 100.0]);
        let g = GlassInstance {
            rect,
            radius: 0.0,
            glass: Glass::dark(),
            alpha: 1.0,
            age: 0.0,
            sheen: None,
            orbit: None,
            orbit_strength: 0.0,
            to_surface: Affine::IDENTITY,
            surface_alpha: 1.0,
            outline: Some(range),
        };
        let pts = g.outline_points(&list.vector_segs).unwrap();
        // The curve is cut into pieces, and none of them leaves the bounding rect.
        assert!(pts.len() > 8, "{} points", pts.len());
        assert!(inside_rect(&pts, rect));
        assert!(pts.iter().any(|p| p.x > 115.0), "the bulge is kept");
        // A shaped panel's blur region follows the outline (EG-07), not its bounding box.
        list.scale = 1.0;
        list.glass.push(g);
        let region = list.blur_region();
        assert!(region.rects.len() > 1);
        check_region(&pts, &region.rects, 1.0, &format!("path outline, {} rects", region.rects.len()));
    }

    /// Whether `p` lies inside the polygon `pts` (even-odd).
    fn inside(pts: &[Point], p: Point) -> bool {
        let n = pts.len();
        let mut odd = false;
        for i in 0..n {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            if (a.y <= p.y) != (b.y <= p.y) && p.x < a.x + (p.y - a.y) * (b.x - a.x) / (b.y - a.y) {
                odd = !odd;
            }
        }
        odd
    }

    /// Distance from `p` to the polygon's edge.
    fn edge_dist(pts: &[Point], p: Point) -> f32 {
        let n = pts.len();
        (0..n)
            .map(|i| {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / (dx * dx + dy * dy).max(1e-12)).clamp(0.0, 1.0);
                (p.x - a.x - t * dx).hypot(p.y - a.y - t * dy)
            })
            .fold(f32::INFINITY, f32::min)
    }

    /// The EG-07 checks for a blur region of the outline `pts` (surface logical pixels) at
    /// `scale`: no rect spills more than one physical pixel past the outline, the rects cover
    /// at least 97 % of it, they do not overlap, and there are at most `MAX_BLUR_RECTS`.
    /// Returns the coverage.
    fn check_region(pts: &[Point], rects: &[IRect], scale: f32, what: &str) -> f32 {
        assert!(!rects.is_empty() && rects.len() <= MAX_BLUR_RECTS, "{what}: {} rects", rects.len());
        let step = 0.25;
        let mut spill = 0.0f32;
        for r in rects {
            assert!(r.w > 0 && r.h > 0, "{what}: empty rect {r:?}");
            let (x0, y0, x1, y1) = (r.x as f32, r.y as f32, (r.x + r.w) as f32, (r.y + r.h) as f32);
            // A rect spills furthest at its boundary: walk it.
            let mut edge = Vec::new();
            let (nx, ny) = (((x1 - x0) / step) as i32, ((y1 - y0) / step) as i32);
            for i in 0..=nx {
                let x = x0 + i as f32 * step;
                edge.extend([Point::new(x, y0), Point::new(x, y1)]);
            }
            for j in 0..=ny {
                let y = y0 + j as f32 * step;
                edge.extend([Point::new(x0, y), Point::new(x1, y)]);
            }
            for p in edge {
                if !inside(pts, p) {
                    spill = spill.max(edge_dist(pts, p));
                }
            }
        }
        assert!(spill * scale <= 1.0, "{what}: spills {:.2} physical px", spill * scale);
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let apart = a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
                assert!(apart, "{what}: {a:?} overlaps {b:?}");
            }
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
        for p in pts {
            (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
        }
        let (mut total, mut covered) = (0u32, 0u32);
        let mut y = y0 + 0.5 * step;
        while y < y1 {
            let mut x = x0 + 0.5 * step;
            while x < x1 {
                if inside(pts, Point::new(x, y)) {
                    total += 1;
                    let hit = rects.iter().any(|r| {
                        x >= r.x as f32 && x < (r.x + r.w) as f32 && y >= r.y as f32 && y < (r.y + r.h) as f32
                    });
                    covered += hit as u32;
                }
                x += step;
            }
            y += step;
        }
        let cover = covered as f32 / total as f32;
        assert!(cover >= 0.97, "{what}: covers {:.2} %", 100.0 * cover);
        cover
    }

    /// A list holding one glass panel, laid out at `rect` in a root layer seen through
    /// `to_surface`, at `scale`.
    fn one_panel(rect: [f32; 4], radius: f32, glass: Glass, to_surface: Affine, scale: f32) -> DisplayList {
        use crate::display::{Item, ItemKind, Layer};
        let mut root = Layer::root();
        root.transform = to_surface;
        root.items = 0..1;
        let g = GlassInstance {
            rect,
            radius,
            glass,
            alpha: 1.0,
            age: 1.0,
            sheen: None,
            orbit: None,
            orbit_strength: 0.0,
            to_surface,
            surface_alpha: 1.0,
            outline: None,
        };
        DisplayList {
            scale,
            layers: vec![root],
            items: vec![Item { kind: ItemKind::Glass, start: 0, len: 1 }],
            glass: vec![g],
            ..Default::default()
        }
    }

    #[test]
    fn an_octagon_blur_region_follows_its_chamfers() {
        let rect = [20.3, 14.6, 320.0, 120.0];
        let oct = GlassShape::chamfer(12.0).points(rect, 0.0).unwrap();
        for scale in [1.0, 1.25] {
            let region = one_panel(rect, 0.0, Glass::dark().with_shape(GlassShape::chamfer(12.0)), Affine::IDENTITY, scale)
                .blur_region();
            let cover = check_region(&oct, &region.rects, scale, &format!("octagon at {scale}x"));
            // Two rows per chamfer pixel row, top and bottom, plus the middle.
            assert!(region.rects.len() <= 2 * 14 + 1, "octagon at {scale}x: {} rects", region.rects.len());
            eprintln!("octagon at {scale}x: {} rects, {:.2} % covered", region.rects.len(), 100.0 * cover);
        }
    }

    #[test]
    fn a_rotated_card_blur_region_follows_its_edges() {
        let (rect, radius) = ([0.0, 0.0, 300.0, 120.0], 16.0);
        let turn = Affine::translate(-150.0, -60.0).then(&Affine::rotate(15f32.to_radians())).then(&Affine::translate(210.0, 130.0));
        let card = shape_points(GlassShape::Rounded, rect, radius).unwrap();
        let seen: Vec<Point> = card.iter().map(|p| turn.apply(*p)).collect();
        for scale in [1.0, 1.25] {
            let region = one_panel(rect, radius, Glass::dark(), turn, scale).blur_region();
            let cover = check_region(&seen, &region.rects, scale, &format!("15 degree card at {scale}x"));
            eprintln!("15 degree card at {scale}x: {} rects, {:.2} % covered", region.rects.len(), 100.0 * cover);
        }
        // A chamfered card turned the same way.
        let oct: Vec<Point> = GlassShape::chamfer(12.0).points(rect, 0.0).unwrap().iter().map(|p| turn.apply(*p)).collect();
        let region = one_panel(rect, 0.0, Glass::dark().with_shape(GlassShape::chamfer(12.0)), turn, 1.25).blur_region();
        check_region(&oct, &region.rects, 1.25, "15 degree octagon at 1.25x");
    }

    #[test]
    fn an_upright_rounded_panel_keeps_todays_blur_rects() {
        for (rect, radius, scale) in [([10.0, 8.0, 200.0, 80.0], 14.0, 1.0), ([3.5, 2.25, 151.0, 60.5], 9.5, 1.25)] {
            let region = one_panel(rect, radius, Glass::dark(), Affine::IDENTITY, scale).blur_region();
            let mut want = Vec::new();
            blur_rects(rect, radius, &mut want);
            assert_eq!(region.rects, want);
        }
    }

    #[test]
    fn a_tall_outline_is_capped_by_coarser_bands() {
        // A 600-row turned card needs about 640 rows: the cap halves the bands until it fits.
        let rect = [0.0, 0.0, 200.0, 600.0];
        let turn = Affine::rotate(10f32.to_radians()).then(&Affine::translate(120.0, 10.0));
        let seen: Vec<Point> = shape_points(GlassShape::Rounded, rect, 0.0).unwrap().iter().map(|p| turn.apply(*p)).collect();
        let mut rects = Vec::new();
        outline_blur_rects(&seen, 1.0, &mut rects);
        assert!(rects.len() <= MAX_BLUR_RECTS && rects.len() > MAX_BLUR_RECTS / 2, "{} rects", rects.len());
        assert!(rects.iter().any(|r| r.h >= 2), "the bands got coarser");
        check_region(&seen, &rects, 1.0, "capped tall card");
    }

    #[test]
    fn a_tilted_layer_blur_region_follows_the_warp() {
        use crate::geom::Rect;
        use crate::stage::Stage;
        let rect = [40.0, 30.0, 240.0, 120.0];
        let warp = Stage::default()
            .tilt(Size::new(320.0, 180.0), Rect::new(40.0, 30.0, 240.0, 120.0), 0.0, 0.5, 0.0)
            .expect("the layer turns");
        let mut list = one_panel(rect, 12.0, Glass::dark(), Affine::IDENTITY, 1.0);
        list.layers[0].warp = Some(warp);
        let seen: Vec<Point> = shape_points(GlassShape::Rounded, rect, 12.0).unwrap().iter().map(|p| warp.apply(*p)).collect();
        check_region(&seen, &list.blur_region().rects, 1.0, "tilted card");
    }

    /// The EG-07 rasteriser as it was, scanning every edge for every probe: the reference
    /// the edge sweep must match rect for rect (RV-01).
    fn full_scan_rects(pts: &[Point], scale: f32) -> Vec<IRect> {
        fn cross_all(pts: &[Point], y: f32, out: &mut Vec<[f32; 2]>) {
            let n = pts.len();
            let mut xs: Vec<f32> = (0..n)
                .map(|i| (pts[i], pts[(i + 1) % n]))
                .filter(|(a, b)| (a.y <= y) != (b.y <= y))
                .map(|(a, b)| a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y))
                .collect();
            xs.sort_by(f32::total_cmp);
            out.clear();
            out.extend(xs.as_chunks::<2>().0.iter().copied());
        }
        let tol = 0.7 / scale;
        let (ymin, ymax) = pts.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), p| (a.min(p.y), b.max(p.y)));
        let mut band = 1;
        loop {
            let mut out: Vec<IRect> = Vec::new();
            let (y0, y1) = (ymin.floor() as i32, ymax.ceil() as i32);
            let ty = tol.min(0.45);
            let mut open: Vec<usize> = Vec::new();
            let (mut spans, mut at, mut both) = (Vec::new(), Vec::new(), Vec::new());
            let mut y = y0;
            while y < y1 {
                let yb = (y + band).min(y1);
                let (lo, hi) = (y as f32 + ty, yb as f32 - ty);
                cross_all(pts, lo, &mut spans);
                let mut probe = |s: f32, spans: &mut Vec<[f32; 2]>| {
                    cross_all(pts, s, &mut at);
                    intersect(spans, &at, &mut both);
                    std::mem::swap(spans, &mut both);
                };
                probe(hi, &mut spans);
                for p in pts {
                    if p.y > lo && p.y < hi {
                        probe((p.y - 1e-3).max(lo), &mut spans);
                        probe((p.y + 1e-3).min(hi), &mut spans);
                    }
                }
                let mut next = Vec::new();
                for [a, b] in &spans {
                    let (l, r) = ((a - tol).ceil() as i32, (b + tol).floor() as i32);
                    if r <= l {
                        continue;
                    }
                    if let Some(&i) = open.iter().find(|&&i| out[i].x == l && out[i].w == r - l) {
                        out[i].h += yb - y;
                        next.push(i);
                    } else {
                        out.push(IRect { x: l, y, w: r - l, h: yb - y });
                        next.push(out.len() - 1);
                    }
                }
                open = next;
                y = yb;
            }
            if out.len() <= MAX_BLUR_RECTS || band >= 1 << 16 {
                return out;
            }
            band *= 2;
        }
    }

    /// A closed outline of `n` points round an ellipse whose edge waves in and out, as a
    /// glass path outline.
    fn wavy_contour(n: usize, c: Point, r: [f32; 2]) -> Contour {
        let pt = |i: usize| {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            let k = 1.0 + 0.04 * (40.0 * a).sin();
            Point::new(c.x + r[0] * k * a.cos(), c.y + r[1] * k * a.sin())
        };
        let segs = (0..n)
            .map(|i| {
                let (p0, p2) = (pt(i), pt(i + 1));
                let p1 = Point::new(0.5 * (p0.x + p2.x), 0.5 * (p0.y + p2.y));
                VectorSeg { p0, p1, p2, s0: 0.0, s1: 0.0, w: [0.0; 2] }
            })
            .collect();
        Contour { segs, closed: true }
    }

    #[test]
    fn the_edge_sweep_matches_the_full_scan() {
        let turn = Affine::translate(-150.0, -60.0).then(&Affine::rotate(15f32.to_radians())).then(&Affine::translate(210.0, 130.0));
        let mut shapes: Vec<Vec<Point>> = vec![
            shape_points(GlassShape::Rounded, [0.0, 0.0, 300.0, 120.0], 16.0).unwrap(),
            GlassShape::chamfer(12.0).points([20.3, 14.6, 320.0, 120.0], 0.0).unwrap(),
            GlassShape::Wavy { amplitude: 4.0, wavelength: 9.0 }.points([0.0, 0.0, 300.0, 120.0], 20.0).unwrap(),
            GlassShape::Polygon { sides: 3, turn: 0.1 }.points([5.0, 5.0, 90.0, 80.0], 0.0).unwrap(),
            shape_points(GlassShape::Rounded, [0.0, 0.0, 200.0, 600.0], 0.0).unwrap(),
        ];
        shapes.push(shapes[0].iter().map(|p| turn.apply(*p)).collect());
        shapes.push(shapes[2].iter().map(|p| turn.apply(*p)).collect());
        let mut list = DisplayList::default();
        let (range, _) = push_outline(&mut list, &wavy_contour(1000, Point::new(400.0, 300.0), [300.0, 200.0]));
        let g = GlassInstance { outline: Some(range), ..one_panel([0.0; 4], 0.0, Glass::dark(), Affine::IDENTITY, 1.0).glass[0] };
        shapes.push(g.outline_points(&list.vector_segs).unwrap());
        for (i, pts) in shapes.iter().enumerate() {
            for scale in [1.0, 1.25, 2.0] {
                let mut got = Vec::new();
                outline_blur_rects(pts, scale, &mut got);
                assert_eq!(got, full_scan_rects(pts, scale), "shape {i} at {scale}x");
            }
        }
    }

    #[test]
    fn a_point_far_past_the_surface_is_clipped_and_quick() {
        // A layer tilted until its far edge nears the eye plane: the divisor falls towards 0
        // at the bottom and the bottom corners land millions of pixels down.
        let rect = [40.0, 30.0, 240.0, 169.999];
        let warp = Warp([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -0.005, 1.0]]);
        let mut list = one_panel(rect, 12.0, Glass::dark(), Affine::IDENTITY, 1.25);
        list.size = Size::new(320.0, 180.0);
        list.layers[0].warp = Some(warp);
        let seen: Vec<Point> = shape_points(GlassShape::Rounded, rect, 12.0).unwrap().iter().map(|p| warp.apply(*p)).collect();
        let far = seen.iter().fold(0.0f32, |m, p| m.max(p.y));
        assert!(far > 1.0e6, "the outline reaches y = {far}");
        // The same outline handed in directly, with a point at y = 1e7.
        let mut tall = GlassShape::chamfer(12.0).points([40.0, 30.0, 240.0, 120.0], 0.0).unwrap();
        tall[3].y = 1.0e7;
        let surface = IRect { x: 0, y: 0, w: 320, h: 180 };
        let t = std::time::Instant::now();
        let region = list.blur_region();
        let mut direct = Vec::new();
        outline_blur_rects_in(&tall, 1.0, Some(surface), &mut direct);
        let took = t.elapsed();
        assert!(took.as_secs_f64() < 0.005, "took {took:?}");
        for (what, rects) in [("warped", &region.rects), ("direct", &direct)] {
            assert!(!rects.is_empty() && rects.len() <= MAX_BLUR_RECTS, "{what}: {} rects", rects.len());
            for r in rects.iter() {
                assert!(r.w > 0 && r.h > 0 && r.x >= 0 && r.y >= 0 && r.x + r.w <= 320 && r.y + r.h <= 180, "{what}: {r:?}");
            }
        }
        // An upright panel half off the surface is cut to it as well.
        let mut half = one_panel([200.0, 100.0, 300.0, 200.0], 16.0, Glass::dark(), Affine::IDENTITY, 1.0);
        half.size = Size::new(320.0, 180.0);
        let rects = half.blur_region().rects;
        assert!(rects.iter().all(|r| r.x >= 200 && r.y >= 100 && r.x + r.w <= 320 && r.y + r.h <= 180), "{rects:?}");
        assert!(rects.iter().map(|r| r.w * r.h).sum::<i32>() >= 110 * 70, "{rects:?}");
    }

    #[test]
    fn a_panel_inside_the_surface_keeps_its_region_when_clipped() {
        let turn = Affine::translate(-150.0, -60.0).then(&Affine::rotate(15f32.to_radians())).then(&Affine::translate(210.0, 130.0));
        for (shape, m) in [(GlassShape::Rounded, Affine::IDENTITY), (GlassShape::chamfer(12.0), turn), (GlassShape::Rounded, turn)] {
            let mut list = one_panel([0.0, 0.0, 300.0, 120.0], 16.0, Glass::dark().with_shape(shape), m, 1.25);
            let open = list.blur_region();
            list.size = Size::new(420.0, 260.0);
            assert_eq!(list.blur_region(), open, "{shape:?}");
        }
    }

    #[test]
    fn a_still_outline_builds_its_region_once() {
        let mut list = one_panel([0.0; 4], 0.0, Glass::dark(), Affine::rotate(0.2), 1.25);
        list.size = Size::new(800.0, 600.0);
        let (range, rect) = push_outline(&mut list, &wavy_contour(1000, Point::new(400.0, 300.0), [300.0, 200.0]));
        list.glass[0].outline = Some(range);
        list.glass[0].rect = rect;
        assert!(list.glass[0].outline_points(&list.vector_segs).unwrap().len() >= 1000);
        let mut cache = BlurCache::default();
        let want = list.blur_region();
        assert!(!want.rects.is_empty());
        for _ in 0..60 {
            assert_eq!(cache.region(&list), &want);
        }
        assert_eq!(cache.builds(), 1, "60 unchanged samples build the region once");
        // Any input that moves the region builds it again.
        list.glass[0].to_surface = Affine::rotate(0.25);
        assert_ne!(cache.region(&list), &want);
        assert_eq!(cache.builds(), 2);
        list.vector_segs[range[0] as usize].p0.y += 5.0;
        cache.region(&list);
        assert_eq!(cache.builds(), 3, "an outline point moved");
        list.glass[0].surface_alpha = 0.2;
        assert!(cache.region(&list).rects.is_empty());
        assert_eq!(cache.builds(), 4, "the panel let go of blur");
        cache.region(&list);
        list.size = Size::new(640.0, 480.0);
        list.glass[0].surface_alpha = 1.0;
        cache.region(&list);
        assert_eq!(cache.builds(), 5);
    }

    #[test]
    fn a_non_glass_element_can_ask_for_blur_behind_its_outline() {
        use crate::element::Element;
        use crate::scene::Scene;
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(240.0, 120.0), 1.0);
        let frame = |o: f32| {
            Element::column("root")
                .child(Element::panel("frame").size(160.0, 80.0).opacity(o).blur_behind(GlassShape::chamfer(10.0)))
        };
        scene.apply(frame(1.0), 0.0);
        let list = scene.sample(5.0);
        assert_eq!(list.glass.len(), 0, "no glass is drawn");
        let oct = GlassShape::chamfer(10.0).points([0.0, 0.0, 160.0, 80.0], 0.0).unwrap();
        check_region(&oct, &list.blur_region().rects, 1.0, "blur-behind frame");
        // A faint element holds no blur yet, as glass.
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(240.0, 120.0), 1.0);
        scene.apply(frame(0.3), 0.0);
        assert!(scene.sample(5.0).blur_region().rects.is_empty());
    }
}
