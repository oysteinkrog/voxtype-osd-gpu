//! A small 3D layer for the overlay framework (CAPABILITIES section 1.7).
//!
//! - `math`, `camera`: `Vec3`, `Mat4`, an orbit camera with perspective or orthographic
//!   projection.
//! - `mesh`, `text3d`: meshes (torus, box, extruded paths) and extruded 3D text from glyph
//!   outlines (D306), and [`TurningWord`], a word whose letters turn in on springs.
//! - `frame`: [`Frame3d`], the value an app builds from `t`: camera, light, mesh draws, point
//!   clouds (D302) and bokeh orbs (FX16).
//! - `render`: [`Renderer3d`], one MSAA pass with a depth buffer that resolves into any
//!   colour view (D301).
//! - `field`: [`TextField`], the text distance field (TY19) by jump flooding.
//! - `stack`: [`Stack`], the fixed depth order of D304, with Slug text planes in the scene.
//! - `timer`: GPU time per stage from timestamp queries.
//!
//! Everything is a pure function of `t` and seeds: point clouds are evaluated in closed form
//! in the vertex shader from the point index, so there is no simulation state to replay.

pub mod camera;
pub mod field;
pub mod frame;
pub mod math;
pub mod mesh;
pub mod readback;
pub mod render;
pub mod stack;
pub mod text3d;
pub mod timer;

pub use camera::{Camera, Projection};
pub use field::TextField;
pub use frame::{Background, CloudShape, Focus, Frame3d, Light, Material, MeshDraw, MeshId, Orb, PointCloud};
pub use math::{Mat4, Vec3};
pub use mesh::{Mesh, Vertex};
pub use render::{FieldBinding, Renderer3d};
pub use stack::{Layers, Stack, TextPlane};
pub use text3d::{extrude_text, GlyphMesh, TurningWord};
pub use timer::GpuTimer;
