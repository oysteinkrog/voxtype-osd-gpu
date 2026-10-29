//! One raymarched hero object in a 2D scene: a liquid chrome blob (concept 05), a glowing
//! aurora orb (01) or a jellyfish bell (15). `Element::gpu(key, vo_3d::hero(source))` reserves
//! the box; the object is centred in it with the box's half width as its radius, and is seen
//! through the scene's stage camera (`vo_scene::stage`), so it lines up with turned layers and
//! 3D items [`crate::OnStage`].
//!
//! It costs one quad over the object's bounds, drawn straight over the surface: no scratch
//! texture, no depth. Each pixel marches a signed distance field between the entry and exit
//! of a bounding sphere, at most [`HeroFrame::steps`] times, and pixels whose rays miss that
//! sphere stop at once.
//!
//! A hero can also be a small scene ([`HeroShape::Parts`]): up to [`MAX_PARTS`] primitives,
//! each in one of two layers. Skin parts melt into one outer surface drawn with the look;
//! core parts melt into an inner surface that glows. With [`HeroFrame::inside`] the skin
//! turns see-through: the core shows behind it, and an aurora of 3D noise can glow in the
//! skin's interior (at most [`MAX_FOG_STEPS`] samples per pixel). Everything stays a pure
//! function of the frame; a reduced-motion source passes phase 0, which also freezes the noise.

use std::any::Any;
use std::fmt::Debug;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use vo_render::color::linear_premul;
use vo_render::{GpuPainter, PaintCx};
use vo_scene::geom::{Color, Point, Size};
use vo_scene::gpu::GpuSource;

use crate::quad::{clip_to_scissor, QuadPass};

const HERO_WGSL: &str = include_str!("wgsl/sdf_hero.wgsl");

pub fn hero_shader_source() -> &'static str {
    HERO_WGSL
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HeroShape {
    Orb,
    /// `lobes` spheres circling the centre, melted into one.
    Blob {
        lobes: u32,
    },
    /// A dome open underneath, breathing slowly.
    Bell,
    /// Several primitives, built with [`HeroParts::new`].
    Parts(HeroParts),
}

/// Most primitives in one hero.
pub const MAX_PARTS: usize = 8;
/// Most aurora samples per pixel inside a see-through skin.
pub const MAX_FOG_STEPS: u32 = 16;
/// Uniform vectors per part.
const PART_VECS: usize = 4;

/// One primitive of a [`HeroShape::Parts`] scene. Lengths are in radii, y down, before the
/// frame's stretch and spin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PartKind {
    Sphere {
        radius: f32,
    },
    /// Half extents along x, y and z.
    Ellipsoid {
        radii: [f32; 3],
    },
    /// A rod from the part's position to `to` (relative to the position), round ends.
    Capsule {
        to: [f32; 3],
        radius: f32,
    },
    /// A dome open underneath with a swaying scalloped hem; `radii` are the dome's half
    /// extents. The hem sways with the frame's phase.
    Bell {
        radii: [f32; 3],
    },
}

/// Which surface a part belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PartLayer {
    /// The outer surface, drawn with the frame's look.
    #[default]
    Skin,
    /// The inner surface: it glows in [`HeroInside::core`] and shows through a see-through
    /// skin. Outside the skin it is drawn as it is (drip beads under a bell).
    Core,
}

/// Most copies of one part around the vertical axis.
pub const MAX_COPIES: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeroPart {
    pub kind: PartKind,
    /// Position in radii from the element's centre.
    pub at: [f32; 3],
    pub layer: PartLayer,
    /// Copies spread evenly around the vertical axis through the centre, the first at `at`
    /// (1 to [`MAX_COPIES`]): four membranes or six beads are one part each.
    pub copies: u32,
    /// Up and down travel in radii with the phase; copies move one after another, a wave
    /// running around the axis.
    pub bob: f32,
}

impl HeroPart {
    pub const fn new(kind: PartKind, at: [f32; 3], layer: PartLayer) -> HeroPart {
        HeroPart { kind, at, layer, copies: 1, bob: 0.0 }
    }

    /// `n` copies around the vertical axis.
    pub const fn around(self, n: u32) -> HeroPart {
        HeroPart { copies: n, ..self }
    }

    /// Bobbing `amount` radii up and down.
    pub const fn bobbing(self, amount: f32) -> HeroPart {
        HeroPart { bob: amount, ..self }
    }

    /// The radius, in radii, of a sphere about the centre that holds this part.
    fn reach(&self) -> f32 {
        let len = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        let max = |v: [f32; 3]| v[0].max(v[1]).max(v[2]);
        let at = len(self.at);
        (match self.kind {
            PartKind::Sphere { radius } => at + radius,
            PartKind::Ellipsoid { radii } => at + max(radii),
            PartKind::Capsule { to, radius } => {
                let end = [self.at[0] + to[0], self.at[1] + to[1], self.at[2] + to[2]];
                at.max(len(end)) + radius
            }
            PartKind::Bell { radii } => at + 1.1 * max(radii),
        }) + self.bob.abs()
    }
}

/// Too many parts for one hero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooManyParts(pub usize);

impl std::fmt::Display for TooManyParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a hero holds at most {MAX_PARTS} parts, got {}", self.0)
    }
}

impl std::error::Error for TooManyParts {}

/// Up to [`MAX_PARTS`] primitives melted together, layer by layer, with smooth union radius
/// `k` (in radii).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeroParts {
    parts: [HeroPart; MAX_PARTS],
    len: usize,
    k: f32,
}

impl HeroParts {
    /// `parts` melted with radius `k`. More than [`MAX_PARTS`] is an error.
    pub fn new(parts: &[HeroPart], k: f32) -> Result<HeroParts, TooManyParts> {
        if parts.len() > MAX_PARTS {
            return Err(TooManyParts(parts.len()));
        }
        let empty = HeroPart::new(PartKind::Sphere { radius: 0.0 }, [0.0; 3], PartLayer::Skin);
        let mut all = [empty; MAX_PARTS];
        all[..parts.len()].copy_from_slice(parts);
        Ok(HeroParts { parts: all, len: parts.len(), k: k.max(1e-4) })
    }

    pub fn parts(&self) -> &[HeroPart] {
        &self.parts[..self.len]
    }

    pub fn k(&self) -> f32 {
        self.k
    }

    fn reach(&self) -> f32 {
        self.parts().iter().fold(0.0f32, |m, p| m.max(p.reach())) + 0.25 * self.k
    }

    /// The parts as the shader reads them: per part, `at` and the kind; the sizes and the
    /// layer; the capsule's far end and the copies; the bob.
    fn pack(&self) -> [[f32; 4]; PART_VECS * MAX_PARTS] {
        let mut out = [[0.0; 4]; PART_VECS * MAX_PARTS];
        for (i, p) in self.parts().iter().enumerate() {
            let (kind, size, to) = match p.kind {
                PartKind::Sphere { radius } => (0.0, [radius; 3], [0.0; 3]),
                PartKind::Ellipsoid { radii } => (1.0, radii, [0.0; 3]),
                PartKind::Capsule { to, radius } => (2.0, [radius; 3], to),
                PartKind::Bell { radii } => (3.0, radii, [0.0; 3]),
            };
            let layer = match p.layer {
                PartLayer::Skin => 0.0,
                PartLayer::Core => 1.0,
            };
            let v = &mut out[PART_VECS * i..PART_VECS * (i + 1)];
            v[0] = [p.at[0], p.at[1], p.at[2], kind];
            v[1] = [size[0], size[1], size[2], layer];
            v[2] = [to[0], to[1], to[2], p.copies.clamp(1, MAX_COPIES) as f32];
            v[3] = [p.bob, 0.0, 0.0, 0.0];
        }
        out
    }
}

/// A see-through skin with things inside it: the core layer's parts and an aurora of 3D noise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeroInside {
    /// Opacity of the skin where it faces the viewer, 0 to 1; its edges go opaque (Fresnel).
    pub skin: f32,
    /// Colour of the core parts (light, drawn over nothing).
    pub core: Color,
    /// Brightness of the core's glow, 1 is the colour as given.
    pub core_glow: f32,
    /// The aurora's two colours; it drifts between them.
    pub fog_a: Color,
    pub fog_b: Color,
    /// Aurora brightness; 0 turns it off.
    pub fog: f32,
    /// Size of the aurora's folds: higher is finer.
    pub fog_scale: f32,
    /// Aurora samples per pixel, at most [`MAX_FOG_STEPS`].
    pub fog_steps: u32,
}

impl Default for HeroInside {
    fn default() -> HeroInside {
        HeroInside {
            skin: 0.3,
            core: Color::hex(0x9fe8ff, 1.0),
            core_glow: 1.0,
            fog_a: Color::hex(0x2de0c8, 1.0),
            fog_b: Color::hex(0x8a4dff, 1.0),
            fog: 0.0,
            fog_scale: 2.5,
            fog_steps: MAX_FOG_STEPS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HeroLook {
    /// A mirror in a studio: `tint` multiplies the reflection; the studio is a gradient from
    /// `bottom` to `top` with a bright horizon and two soft light strips in `light`.
    Chrome { tint: Color, top: Color, bottom: Color, light: Color },
    /// Light from inside: colours `a`, `b` and `c` drift over the surface, `rim` lights the
    /// edges. `body` is the opacity of the middle (the rim is opaque). `halo` is the strength
    /// of the soft light around it, `halo_width` its reach in radii.
    Glow { a: Color, b: Color, c: Color, rim: Color, body: f32, halo: f32, halo_width: f32 },
}

/// The object at one moment. Lengths are in radii, the radius being half the element's width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeroFrame {
    pub shape: HeroShape,
    pub look: HeroLook,
    /// Stretch along x, y and z: `[1.4, 0.6, 1.0]` is a wide, flat blob.
    pub stretch: [f32; 3],
    /// Ripples and spread, 0 to 1: feed it a voice level.
    pub wobble: f32,
    /// Animation time in seconds: lobes circle and ripples run with it.
    pub phase: f32,
    /// Turn about the vertical axis, in radians.
    pub spin: f32,
    /// Most march steps per pixel. 48 is plenty for these shapes; fewer cost less and blur
    /// the deepest folds.
    pub steps: u32,
    /// A see-through skin and what glows inside it. `None` keeps the skin solid (a glow
    /// look's `body` still sets its opacity).
    pub inside: Option<HeroInside>,
}

impl Default for HeroFrame {
    fn default() -> HeroFrame {
        HeroFrame {
            shape: HeroShape::Orb,
            look: HeroLook::Chrome {
                tint: Color::hex(0xe8eef8, 1.0),
                top: Color::hex(0x9aa8c0, 1.0),
                bottom: Color::hex(0x0a0c12, 1.0),
                light: Color::hex(0xffffff, 1.0),
            },
            stretch: [1.0; 3],
            wobble: 0.0,
            phase: 0.0,
            spin: 0.0,
            steps: 48,
            inside: None,
        }
    }
}

impl HeroFrame {
    /// The radius, in radii, of a sphere that holds the object and its halo.
    pub fn bound(&self) -> f32 {
        let reach = match self.shape {
            HeroShape::Orb => 1.0,
            HeroShape::Blob { .. } => 0.48 + 0.6 + 0.12,
            HeroShape::Bell => 1.2,
            HeroShape::Parts(p) => p.reach(),
        };
        let s = self.stretch.iter().fold(0.0f32, |m, v| m.max(*v));
        let halo = match self.look {
            HeroLook::Glow { halo, halo_width, .. } if halo > 0.0 => 5.0 * halo_width,
            _ => 0.0,
        };
        (reach + 0.07) * s + halo
    }
}

/// Ready-made scenes. Each is pure in `t` and `level` (a voice level, 0 to 1). Under reduced
/// motion the shapes rest and the aurora freezes as at `t = 0`; the level still brightens.
impl HeroFrame {
    /// A jellyfish (concept 15): a see-through bell, four glowing membranes inside it and six
    /// drip beads bobbing under the hem, all in three parts.
    pub fn jellyfish(t: f32, level: f32, reduced_motion: bool) -> HeroFrame {
        let v = level.clamp(0.0, 1.0);
        let ph = if reduced_motion { 0.0 } else { t };
        let wave = if reduced_motion { 0.0 } else { v };
        let breathe = 1.0 + 0.06 * (2.4 * ph).sin() + 0.08 * wave;
        let parts = [
            HeroPart::new(
                PartKind::Bell { radii: [0.9 * breathe, 0.75 / breathe, 0.9 * breathe] },
                [0.0, -0.2, 0.0],
                PartLayer::Skin,
            ),
            HeroPart::new(PartKind::Ellipsoid { radii: [0.15, 0.1, 0.09] }, [0.27, -0.34, 0.0], PartLayer::Core)
                .around(4),
            HeroPart::new(PartKind::Sphere { radius: 0.06 }, [0.5, 0.34, 0.0], PartLayer::Core).around(6).bobbing(0.06),
        ];
        HeroFrame {
            shape: HeroShape::Parts(HeroParts::new(&parts, 0.06).expect("three parts")),
            look: HeroLook::Glow {
                a: Color::hex(0x1846ff, 1.0),
                b: Color::hex(0x6a3cff, 1.0),
                c: Color::hex(0x23c8ff, 1.0),
                rim: Color::hex(0x9fe8ff, 1.0),
                body: 0.35,
                halo: 0.25 + 0.35 * v,
                halo_width: 0.1,
            },
            stretch: [0.85; 3],
            wobble: 0.4 * wave,
            phase: ph,
            spin: 0.15 * ph,
            inside: Some(HeroInside {
                skin: 0.3,
                core: Color::hex(0x7af0ff, 1.0),
                core_glow: 1.1 + 0.5 * v,
                fog_a: Color::hex(0x23c8ff, 1.0),
                fog_b: Color::hex(0x6a3cff, 1.0),
                fog: 0.6,
                fog_scale: 3.0,
                fog_steps: MAX_FOG_STEPS,
            }),
            ..HeroFrame::default()
        }
    }

    /// Liquid chrome with an aurora glowing inside (concept 05): three lobes melted into a
    /// see-through mirror, curtains of light folding within.
    pub fn aurora_chrome(t: f32, level: f32, reduced_motion: bool) -> HeroFrame {
        let v = level.clamp(0.0, 1.0);
        let ph = if reduced_motion { 0.0 } else { t };
        let wave = if reduced_motion { 0.0 } else { v };
        let lobe = |i: usize| {
            let a = i as f32 * std::f32::consts::TAU / 3.0 + ph * (0.5 + 0.1 * i as f32);
            let r = 0.3 + 0.08 * wave;
            HeroPart::new(
                PartKind::Sphere { radius: 0.55 },
                [r * a.cos(), 0.15 * (1.3 * a + ph).sin(), r * a.sin()],
                PartLayer::Skin,
            )
        };
        HeroFrame {
            shape: HeroShape::Parts(HeroParts::new(&[lobe(0), lobe(1), lobe(2)], 0.4).expect("three parts")),
            look: HeroLook::Chrome {
                tint: Color::hex(0xdfe8ff, 1.0),
                top: Color::hex(0x8ea4d8, 1.0),
                bottom: Color::hex(0x06070c, 1.0),
                light: Color::hex(0xffffff, 1.0),
            },
            stretch: [1.0, 0.62 + 0.1 * wave, 0.85],
            wobble: 0.25 + 0.5 * wave,
            phase: ph,
            spin: 0.2 * ph,
            inside: Some(HeroInside {
                skin: 0.25,
                core: Color::WHITE,
                core_glow: 0.0,
                fog_a: Color::hex(0x2de0c8, 1.0),
                fog_b: Color::hex(0x8a4dff, 1.0),
                fog: 2.2 + 1.5 * v,
                fog_scale: 2.6,
                fog_steps: MAX_FOG_STEPS,
            }),
            ..HeroFrame::default()
        }
    }
}

/// What a hero element shows. `frame` must be pure in `t`.
pub trait HeroSource: Debug + Send + Sync {
    /// The object at `t` for an element `size` logical pixels big.
    fn frame(&self, t: f64, size: Size) -> HeroFrame;

    /// The scene keeps waking for this element until `until`.
    fn until(&self) -> f64 {
        f64::INFINITY
    }
}

/// A [`HeroSource`] as the source of a GPU element.
#[derive(Debug)]
pub struct HeroItem(Box<dyn HeroSource>);

impl GpuSource for HeroItem {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn until(&self) -> f64 {
        self.0.until()
    }
}

/// `source` as the source of `Element::gpu`. Draw it with [`HeroPainter`] (added by
/// [`crate::install`]).
pub fn hero(source: impl HeroSource + 'static) -> Arc<dyn GpuSource> {
    Arc::new(HeroItem(Box::new(source)))
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct HeroUniform {
    rect: [f32; 4],
    viewport: [f32; 4],
    eye: [f32; 4],
    centre: [f32; 4],
    shape: [f32; 4],
    march: [f32; 4],
    stretch: [f32; 4],
    c: [[f32; 4]; 4],
    // Parts: count, smooth radius k, the inside flag, fog steps.
    parts: [f32; 4],
    // Skin opacity, core glow, fog brightness, fog scale.
    inside: [f32; 4],
    core: [f32; 4],
    fog_a: [f32; 4],
    fog_b: [f32; 4],
    part: [[f32; 4]; PART_VECS * MAX_PARTS],
}

/// Draws [`HeroItem`] elements.
pub struct HeroPainter {
    pass: QuadPass,
}

impl Default for HeroPainter {
    fn default() -> HeroPainter {
        HeroPainter { pass: QuadPass::new("vo3d-hero", HERO_WGSL, "vs_hero", "fs_hero") }
    }
}

/// Where the hero's quad goes: its bounding sphere seen from the eye, as `x0, y0, x1, y1` in
/// surface pixels. `None` when the eye is inside the sphere.
fn quad(eye: [f32; 3], centre: [f32; 2], radius: f32) -> Option<[f32; 4]> {
    // The sphere's silhouette on the plane z = 0: a circle about the projected centre, of
    // radius r * d / sqrt(dist^2 - r^2), widened by its offset from the eye's axis.
    let d = eye[2];
    let (dx, dy) = (centre[0] - eye[0], centre[1] - eye[1]);
    let dist2 = dx * dx + dy * dy + d * d;
    let r2 = radius * radius;
    if dist2 <= r2 * 1.0001 {
        return None;
    }
    let k = d / (dist2 - r2).sqrt();
    let off = (dx * dx + dy * dy).sqrt() / d;
    let h = radius * k * (1.0 + off) + 2.0;
    Some([centre[0] - h, centre[1] - h, centre[0] + h, centre[1] + h])
}

impl GpuPainter for HeroPainter {
    fn paint(&mut self, cx: &mut PaintCx<'_>) -> bool {
        let Some(item) = cx.source.as_any().downcast_ref::<HeroItem>() else { return false };
        let f = item.0.frame(cx.t, Size::new(cx.rect.w, cx.rect.h));
        let [x0, y0, x1, y1] = cx.rect_px();
        let centre = Point::new(0.5 * (x0 + x1), 0.5 * (y0 + y1));
        let centre = cx.warp.map_or(centre, |w| w.apply(centre));
        let radius = 0.5 * (x1 - x0);
        if radius <= 0.0 || cx.opacity <= 0.0 {
            return true;
        }
        let bound = f.bound();
        let eye = [cx.stage.origin[0], cx.stage.origin[1], cx.stage.perspective];
        let Some(q) = quad(eye, [centre.x, centre.y], bound * radius) else { return true };
        let Some(q) = clip_to_scissor(cx, q) else { return true };
        let lin = |c: Color| {
            let p = linear_premul(c, 1.0);
            [p[0], p[1], p[2], c.a]
        };
        let (material, c, halo_width) = match f.look {
            HeroLook::Chrome { tint, top, bottom, light } => (0.0, [lin(tint), lin(top), lin(bottom), lin(light)], 0.0),
            HeroLook::Glow { a, b, c, rim, body, halo, halo_width } => {
                let mut a = lin(a);
                let mut b = lin(b);
                a[3] = body;
                b[3] = halo;
                (1.0, [a, b, lin(c), lin(rim)], halo_width)
            }
        };
        let (shape, lobes) = match f.shape {
            HeroShape::Orb => (0.0, 0),
            HeroShape::Blob { lobes } => (1.0, lobes.clamp(1, 8)),
            HeroShape::Bell => (2.0, 0),
            HeroShape::Parts(_) => (3.0, 0),
        };
        let (parts, part) = match f.shape {
            HeroShape::Parts(p) => ([p.len as f32, p.k, 0.0, 0.0], p.pack()),
            _ => ([0.0; 4], [[0.0; 4]; PART_VECS * MAX_PARTS]),
        };
        let (parts, inside, core, fog_a, fog_b) = match f.inside {
            Some(i) => (
                [parts[0], parts[1], 1.0, i.fog_steps.min(MAX_FOG_STEPS) as f32],
                [i.skin.clamp(0.0, 1.0), i.core_glow.max(0.0), i.fog.max(0.0), i.fog_scale.max(0.0)],
                lin(i.core),
                lin(i.fog_a),
                lin(i.fog_b),
            ),
            None => (parts, [0.0; 4], [0.0; 4], [0.0; 4], [0.0; 4]),
        };
        let u = HeroUniform {
            rect: [q[0], q[1], q[2] - q[0], q[3] - q[1]],
            viewport: [cx.size[0] as f32, cx.size[1] as f32, cx.opacity, cx.encode_srgb as u32 as f32],
            eye: [eye[0], eye[1], eye[2], 0.0],
            centre: [centre.x, centre.y, 0.0, radius],
            shape: [shape, f.wobble.clamp(0.0, 1.0), f.phase, f.spin],
            march: [lobes as f32, f.steps.clamp(4, 256) as f32, bound, halo_width],
            stretch: [f.stretch[0], f.stretch[1], f.stretch[2], material],
            c,
            parts,
            inside,
            core,
            fog_a,
            fog_b,
            part,
        };
        self.pass.draw(cx, q, bytemuck::bytes_of(&u));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hero_shader_validates_in_naga() {
        let src = hero_shader_source();
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }

    #[test]
    fn uniform_matches_the_shader() {
        assert_eq!(std::mem::size_of::<HeroUniform>(), (16 + PART_VECS * MAX_PARTS) * 16);
    }

    #[test]
    fn quad_holds_the_sphere() {
        // On the eye's axis, a sphere at z = 0 of radius 100 seen from 1000: a little more
        // than 100 either way.
        let q = quad([300.0, 150.0, 1000.0], [300.0, 150.0], 100.0).unwrap();
        assert!(q[2] - 300.0 > 100.0 && q[2] - 300.0 < 110.0, "{q:?}");
        // Off the axis it widens, toward the far side too.
        let q = quad([300.0, 150.0, 1000.0], [700.0, 150.0], 100.0).unwrap();
        assert!(q[2] - 700.0 > 130.0, "{q:?}");
        // The eye inside: nothing to bound.
        assert!(quad([0.0, 0.0, 50.0], [0.0, 0.0], 100.0).is_none());
    }

    #[test]
    fn bound_grows_with_stretch_and_halo() {
        let orb = HeroFrame::default();
        let wide = HeroFrame { stretch: [1.6, 0.6, 1.0], ..orb };
        assert!(wide.bound() > 1.6 && orb.bound() < 1.2);
        let glow = HeroFrame {
            look: HeroLook::Glow {
                a: Color::WHITE,
                b: Color::WHITE,
                c: Color::WHITE,
                rim: Color::WHITE,
                body: 0.5,
                halo: 0.6,
                halo_width: 0.1,
            },
            ..orb
        };
        assert!(glow.bound() > orb.bound() + 0.4);
    }

    #[test]
    fn scenes_are_pure_and_rest_under_reduced_motion() {
        for scene in [HeroFrame::jellyfish, HeroFrame::aurora_chrome] {
            assert_eq!(scene(1.3, 0.5, false), scene(1.3, 0.5, false));
            assert_ne!(scene(1.3, 0.5, false), scene(2.1, 0.5, false));
            let (a, b) = (scene(1.3, 0.5, true), scene(7.9, 0.5, true));
            assert_eq!(a, b, "reduced motion moved");
            assert_eq!(a.phase, 0.0);
        }
    }

    fn ball(x: f32, layer: PartLayer) -> HeroPart {
        HeroPart::new(PartKind::Sphere { radius: 0.2 }, [x, 0.0, 0.0], layer)
    }

    #[test]
    fn eight_parts_fit_nine_do_not() {
        let eight: Vec<HeroPart> = (0..8).map(|i| ball(i as f32 * 0.1, PartLayer::Skin)).collect();
        let p = HeroParts::new(&eight, 0.1).unwrap();
        assert_eq!(p.parts().len(), 8);
        let nine: Vec<HeroPart> = (0..9).map(|i| ball(i as f32 * 0.1, PartLayer::Skin)).collect();
        let e = HeroParts::new(&nine, 0.1).unwrap_err();
        assert_eq!(e, TooManyParts(9));
        assert!(e.to_string().contains("at most 8"));
        assert!(HeroParts::new(&[], 0.1).unwrap().parts().is_empty());
    }

    #[test]
    fn parts_pack_kind_size_layer_and_end() {
        let p = HeroParts::new(
            &[
                ball(0.5, PartLayer::Core),
                HeroPart::new(PartKind::Ellipsoid { radii: [0.3, 0.1, 0.2] }, [0.0, -0.2, 0.1], PartLayer::Skin),
                HeroPart::new(PartKind::Capsule { to: [0.0, 0.4, 0.0], radius: 0.05 }, [0.1; 3], PartLayer::Core)
                    .around(6)
                    .bobbing(0.03),
                HeroPart::new(PartKind::Bell { radii: [0.9, 0.7, 0.9] }, [0.0; 3], PartLayer::Skin),
            ],
            0.15,
        )
        .unwrap();
        let k = p.pack();
        assert_eq!(k[0], [0.5, 0.0, 0.0, 0.0]);
        assert_eq!(k[1], [0.2, 0.2, 0.2, 1.0]);
        assert_eq!(k[2], [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(k[4], [0.0, -0.2, 0.1, 1.0]);
        assert_eq!(k[5], [0.3, 0.1, 0.2, 0.0]);
        assert_eq!(k[8], [0.1, 0.1, 0.1, 2.0]);
        assert_eq!(k[10], [0.0, 0.4, 0.0, 6.0]);
        assert_eq!(k[11], [0.03, 0.0, 0.0, 0.0]);
        assert_eq!(k[12][3], 3.0);
        // Unused slots stay zero.
        assert!(k[16..].iter().all(|v| *v == [0.0; 4]));
    }

    #[test]
    fn parts_bound_holds_the_farthest_part() {
        let near = HeroParts::new(&[ball(0.0, PartLayer::Skin)], 0.1).unwrap();
        let far = HeroParts::new(&[ball(0.0, PartLayer::Skin), ball(0.9, PartLayer::Core)], 0.1).unwrap();
        let f = |p| HeroFrame { shape: HeroShape::Parts(p), ..HeroFrame::default() };
        assert!(f(near).bound() < 0.4);
        assert!(f(far).bound() >= 1.1);
        let rod =
            HeroPart::new(PartKind::Capsule { to: [0.0, 1.0, 0.0], radius: 0.1 }, [0.0, 0.2, 0.0], PartLayer::Core);
        assert!(f(HeroParts::new(&[rod], 0.1).unwrap()).bound() >= 1.3);
        // Copies turn about the centre, so they reach no farther; the bob does.
        let ring = ball(0.9, PartLayer::Core).around(6);
        assert_eq!(f(HeroParts::new(&[ring], 0.1).unwrap()).bound(), f(far).bound());
        assert!(f(HeroParts::new(&[ring.bobbing(0.2)], 0.1).unwrap()).bound() > f(far).bound() + 0.15);
    }
}
