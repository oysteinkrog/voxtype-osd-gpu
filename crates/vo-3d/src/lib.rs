//! A small 3D layer for the overlay framework (CAPABILITIES section 1.7).
//!
//! - `math`, `camera`: `Vec3`, `Mat4`, an orbit camera with perspective or orthographic
//!   projection.
//! - `mesh`, `text3d`: meshes (torus, box, extruded paths) and extruded 3D text from glyph
//!   outlines (D306), and [`TurningWord`], a word whose letters turn in on springs.
//! - `frame`: [`Frame3d`], the value an app builds from `t`: camera, light, mesh draws, point
//!   clouds (D302), bokeh orbs (FX16), a reflecting floor and light cones (D305).
//! - `render`: [`Renderer3d`], one MSAA pass with a depth buffer that resolves into any
//!   colour view (D301).
//! - `item`: [`Source3d`] and [`ItemPainter`], 3D as an item at its place in a 2D scene,
//!   whole or as the half behind or in front of the text plane ([`Part`], EG-20).
//! - `painter`: [`Painter3d`] and [`install`], 3D elements (`Element::gpu`) in a vo scene.
//! - `sdf`: [`hero`], one raymarched hero object (chrome blob, aurora orb, jellyfish bell)
//!   on the stage camera.
//! - `env`: [`Env`], the chrome hero reflecting the popup's own layers through an environment
//!   texture built from its layer's backdrop (EG-17).
//! - `grid`: [`grid_floor`], a perspective grid floor (FX-152) with a horizon glow and a pool
//!   of light.
//! - `field`: [`TextField`], the text distance field (TY19) by jump flooding.
//! - `ribbon3d`: [`Ribbon3d`], silk ribbons and cloth strips that twist to show their back
//!   and swell with the voice, in closed form, rewritten each frame (EG-13).
//! - `surface_text`: [`SurfaceText`], text drawn into a mipmapped texture for a textured mesh
//!   ([`Material::texture`]), and two surfaces to print it on: a [`Scroll`] that unrolls and
//!   a [`PageTurn`] (EG-14).
//! - `shadow`: [`Shadows`], soft shadows from the key light: a shadow map fit to the meshes
//!   each frame, read with a Poisson disc (EG-18).
//! - `stack`: [`Stack`], the fixed depth order of D304, with Slug text planes in the scene.
//! - `sculpture`: [`Sculpture`], extruded letters behind flat text whose depth follows
//!   loudness (MO-88).
//! - `timer`: GPU time per stage from timestamp queries.
//! - `glyph_sdf`, `glyph_fx`: distance fields of single glyphs, and the shader effects built
//!   on them: ink diffusion, light through letters and liquid morph (MO-82, MO-84, MO-87).
//!
//! Everything is a pure function of `t` and seeds: point clouds are evaluated in closed form
//! in the vertex shader from the point index, so there is no simulation state to replay.

pub mod camera;
pub mod env;
pub mod field;
pub mod frame;
pub mod glyph_fx;
pub mod glyph_sdf;
pub mod grid;
pub mod item;
pub mod math;
pub mod mesh;
pub mod painter;
mod quad;
pub mod readback;
pub mod render;
pub mod ribbon3d;
pub mod sculpture;
pub mod sdf;
pub mod shadow;
pub mod stack;
pub mod surface_text;
pub mod text3d;
pub mod timer;

pub use camera::{Camera, Projection};
pub use env::Env;
pub use field::TextField;
pub use frame::{
    Background, CloudShape, Floor, Focus, Frame3d, Light, LightCone, Material, MeshDraw, MeshId, OnStage, Orb,
    PointCloud, Pool, TextureId, MAX_POOLS,
};
pub use grid::{grid_floor, GridFrame, GridItem, GridPainter, GridPool, GridSource};
pub use item::{ItemPainter, ItemPlace, Part, Source3d, StageAt};
pub use math::{Mat4, Vec3};
pub use mesh::{Mesh, Vertex};
pub use painter::{install, item, Item3d, Painter3d};
pub use render::{ExtraDraw, FieldBinding, Output, PassInfo, Renderer3d};
pub use ribbon3d::{Ribbon3d, RibbonItem, RibbonLook};
pub use sculpture::{Letter, SculptLook, Sculpture};
pub use shadow::Shadows;
pub use sdf::{hero, HeroFrame, HeroItem, HeroLook, HeroPainter, HeroShape, HeroSource};
pub use stack::{Layers, Stack, TextPlane};
pub use surface_text::{PageTurn, Scroll, SurfaceText};
pub use text3d::{extrude_glyph, extrude_text, GlyphMesh, TurningWord};
pub use timer::GpuTimer;
