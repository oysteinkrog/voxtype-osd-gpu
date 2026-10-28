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
        };
        let s = self.stretch.iter().fold(0.0f32, |m, v| m.max(*v));
        let halo = match self.look {
            HeroLook::Glow { halo, halo_width, .. } if halo > 0.0 => 5.0 * halo_width,
            _ => 0.0,
        };
        (reach + 0.07) * s + halo
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
        assert_eq!(std::mem::size_of::<HeroUniform>(), 11 * 16);
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
}
