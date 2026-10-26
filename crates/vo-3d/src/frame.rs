//! What one 3D frame contains. A [`Frame3d`] is a plain value the app builds from `t`
//! (usually by sampling `vo_core` channels and tracks), so sampling the same `t` twice gives
//! the same frame, at any frame rate and in any order.
//!
//! Pixel sizes (blur, minimum sprite size, text field reach and push) are logical pixels;
//! the renderer multiplies them by the target's scale.
//!
//! Colours are `vo_scene::geom::Color`: straight alpha, sRGB encoded. The renderer
//! linearises and premultiplies them.

use vo_scene::geom::Color;

use crate::camera::Camera;
use crate::math::{Mat4, Vec3};

/// A mesh uploaded with [`crate::Renderer3d::add_mesh`]. In a [`crate::Source3d`] frame,
/// `MeshId(i)` names the `i`th mesh of the source's own list instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MeshId(pub u32);

#[derive(Clone, Debug, Default)]
pub struct Frame3d {
    pub camera: Camera,
    pub background: Background,
    pub light: Light,
    pub meshes: Vec<MeshDraw>,
    pub clouds: Vec<PointCloud>,
    pub orbs: Vec<Orb>,
    pub focus: Focus,
    /// Points and orbs fade to nothing between these view distances, `(start, end)`.
    pub fog: Option<(f32, f32)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Background {
    /// Cleared to transparent: the 3D layer sits on whatever the overlay is over.
    #[default]
    Transparent,
    Solid(Color),
    /// A vertical gradient, `top` to `bottom`, mixed in linear light.
    Vertical {
        top: Color,
        bottom: Color,
    },
}

/// One directional key light, an ambient term and a rim term.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    /// Direction the light travels, in world space. It need not be normalised.
    pub dir: Vec3,
    pub color: Color,
    pub intensity: f32,
    pub ambient: Color,
    /// Strength of the Fresnel rim, 0 for none.
    pub rim: f32,
}

impl Default for Light {
    fn default() -> Light {
        Light {
            dir: Vec3::new(-0.4, -0.8, -0.5),
            color: Color::WHITE,
            intensity: 1.0,
            ambient: Color::rgba(0.25, 0.27, 0.32, 1.0),
            rim: 0.6,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    pub color: Color,
    /// Added on top of the lit colour, as a share of `color`.
    pub emissive: f32,
    pub specular: f32,
    pub shininess: f32,
}

impl Default for Material {
    fn default() -> Material {
        Material { color: Color::WHITE, emissive: 0.0, specular: 0.4, shininess: 48.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshDraw {
    pub mesh: MeshId,
    pub model: Mat4,
    pub material: Material,
}

/// The generator a point cloud evaluates per point, in closed form from the point index,
/// the seed and `t`. No state, so any `t` can be drawn in any order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CloudShape {
    /// Points in a torus around the local y axis. `minor` is the tube radius; points fill
    /// the tube with a soft falloff toward its centre line.
    Torus { major: f32, minor: f32 },
    /// Points in a ball of `radius`, denser at the centre.
    Ball { radius: f32 },
}

/// A cloud of round sprites drawn additively, depth tested against the meshes but never
/// writing depth (D304).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointCloud {
    pub shape: CloudShape,
    pub model: Mat4,
    pub count: u32,
    pub seed: u32,
    pub color: Color,
    /// A second colour mixed in by a per-point random amount.
    pub color2: Color,
    /// Sprite diameter in world units. On screen it shrinks with distance.
    pub size: f32,
    /// Sprites never get smaller than this many pixels across.
    pub min_px: f32,
    /// Seconds of the cloud's own clock.
    pub t: f32,
    /// Radians per second each point travels around the ring, scaled per point by 0.5 to 1.5.
    pub speed: f32,
    /// The level signal, 0 to 1 (voice level for the ring).
    pub level: f32,
    /// How far, in units of `minor` (or `radius`), the level pushes points outward.
    pub level_amp: f32,
    /// Text field reaction (TY19): within `reach_px` of the words, points are pushed away by
    /// up to `push_px` and brightened by up to `glow`. Zero `reach_px` turns it off.
    pub reach_px: f32,
    pub push_px: f32,
    pub glow: f32,
    /// Sprite size and blur follow the depth of field in [`Frame3d::focus`].
    pub defocus: bool,
}

impl Default for PointCloud {
    fn default() -> PointCloud {
        PointCloud {
            shape: CloudShape::Torus { major: 1.0, minor: 0.12 },
            model: Mat4::IDENTITY,
            count: 4000,
            seed: 1,
            color: Color::WHITE,
            color2: Color::WHITE,
            size: 0.02,
            min_px: 1.5,
            t: 0.0,
            speed: 0.2,
            level: 0.0,
            level_amp: 1.0,
            reach_px: 0.0,
            push_px: 0.0,
            glow: 0.0,
            defocus: false,
        }
    }
}

/// An out-of-focus light (FX16): a soft disc whose size and blur grow with its distance
/// from the focus plane. Brightness falls with the disc's area, so energy stays about the same.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Orb {
    pub pos: Vec3,
    /// Size of the light itself in world units, before defocus.
    pub radius: f32,
    pub color: Color,
    pub intensity: f32,
}

/// Depth of field for orbs and for clouds with `defocus`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Focus {
    /// View distance that is sharp.
    pub distance: f32,
    /// View distance from the focus plane at which the blur reaches `max_blur_px`.
    pub range: f32,
    /// Largest blur disc, in pixels across.
    pub max_blur_px: f32,
}

impl Default for Focus {
    fn default() -> Focus {
        Focus { distance: 4.0, range: 3.0, max_blur_px: 48.0 }
    }
}
