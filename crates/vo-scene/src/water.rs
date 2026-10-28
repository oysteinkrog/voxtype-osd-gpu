//! A water pool (EG-10): dark water with highlights and a wobbling reflection of the popup.
//!
//! [`WaterPool`] places a pool in a box of the popup, for example below the text. It builds
//! three things:
//!
//! - a layer the size of `area` that captures its backdrop (EG-01) without drawing it back, so
//!   the pool can read what the popup drew above it;
//! - a panel at the pool's box whose material ([`WATER_MATERIAL`]) draws the water: a tint, the
//!   content above the pool mirrored at the pool's top edge and bent by the wave normals, a
//!   bright line at the edge that fades with depth (a Fresnel-style edge), and specular
//!   highlights from the normals;
//! - a GPU element ([`WaterPoolSource`]) whose painter steps the water simulation to the frame's
//!   time before the pool is drawn. `vo_sim::water_render` holds the painter, the WGSL and the
//!   install call; the scene knows nothing about the simulation.
//!
//! The drops are recorded events ([`PoolDrop`]): a time, a place in the pool and a strength,
//! for example one per word onset with the voice level as its strength. The water at time `t`
//! is a function of `t` and the drop list only, so a seek or an export draws the same frame.
//!
//! The pool only reads: nothing above it moves, so the newest (reading) line is never
//! displaced, only its reflection. Keep the pool below the text it reflects.
//!
//! Reduced motion: the pool is drawn flat with a still reflection, and its source never wakes
//! the scene. Light mode (`VO_LIGHT=1` by default, [`WaterPool::light`]): a pale tint and a
//! fainter reflection, so the popup's dark text keeps its contrast.
//!
//! Parameter blocks: slot 0 is the pool box in layer units, slot 1 the reflection (strength,
//! squash, bend, image slot), slot 2 the shading (edge, specular, shininess, light flag),
//! slot 3 the highlight colour.

use std::any::Any;
use std::sync::Arc;

use crate::display::BackdropRes;
use crate::element::{Element, Key};
use crate::geom::{Color, Rect, Size};
use crate::gpu::GpuSource;
use crate::material::Material;

/// The material function id of the water. `vo_sim::water_render::install` registers it.
pub const WATER_MATERIAL: u32 = crate::material::CUSTOM_FIRST + 192;

/// Seconds the water keeps moving after its last drop (the default 0.8 s damping takes a
/// ring to under 1 % of its height in about 4 s).
pub const SETTLE_SECS: f64 = 5.0;

/// A drop on the pool, at a recorded time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PoolDrop {
    /// Scene time, in seconds.
    pub t: f64,
    /// Place across the pool, 0 (left) to 1 (right).
    pub x: f32,
    /// Place down the pool, 0 (its top edge) to 1 (its bottom).
    pub y: f32,
    /// Radius as a fraction of the pool's width.
    pub radius: f32,
    /// Peak height of the bump. Negative pushes the water down.
    pub strength: f32,
}

impl PoolDrop {
    /// A drop for a word that arrived at `t`, at `x` across the pool, with the voice `level`
    /// (0 to 1) as its size and strength. It falls near the top edge, under the text.
    pub fn word(t: f64, x: f32, level: f32) -> PoolDrop {
        let level = level.clamp(0.0, 1.0);
        PoolDrop { t, x, y: 0.3, radius: 0.015 + 0.02 * level, strength: -(0.3 + 0.5 * level) }
    }
}

/// What the pool's GPU element hands its painter: the drops and the motion setting.
#[derive(Clone, Debug, PartialEq)]
pub struct WaterPoolSource {
    pub drops: Vec<PoolDrop>,
    pub reduced_motion: bool,
}

impl GpuSource for WaterPoolSource {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn until(&self) -> f64 {
        if self.reduced_motion {
            return f64::NEG_INFINITY;
        }
        self.drops.iter().map(|d| d.t + SETTLE_SECS).fold(f64::NEG_INFINITY, f64::max)
    }
}

/// A water pool. See the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct WaterPool {
    /// The box the pool's layer covers, from its parent's top left: the pool plus the content
    /// it reflects.
    pub area: Size,
    /// The pool, in the layer's space (from the area's top left).
    pub pool: Rect,
    pub drops: Vec<PoolDrop>,
    /// The water's colour. Default: a deep blue-black, or a pale grey-blue in light mode.
    pub tint: Color,
    /// The highlight colour. Default: a cool white.
    pub highlight: Color,
    /// How much of the reflection shows at the top edge, 0 to 1. Default 0.55 (0.3 light).
    pub reflect: f32,
    /// How much the reflection is squashed: 1 is a true mirror, 1.6 shows content 1.6 units
    /// above the edge per unit of depth. Default 1.3.
    pub squash: f32,
    /// How far the waves bend the reflection, in layer units at a 45 degree slope. Default 6.
    pub bend: f32,
    /// The brightness of the line at the top edge, 0 to 1. Default 0.35.
    pub edge: f32,
    /// The strength of the specular highlights. Default 0.9.
    pub specular: f32,
    /// Corner radius of the pool. Default 0.
    pub radius: f32,
    /// The image slot (`Renderer::set_material_image`) the normal texture is in. Default 3.
    pub slot: u32,
    pub light: bool,
    pub reduced_motion: bool,
}

/// The default image slot of the normal texture.
pub const DEFAULT_SLOT: u32 = 3;

impl WaterPool {
    /// A pool at `pool` inside a layer of size `area`, placed at its parent's top left.
    /// Light mode and reduced motion default from `VO_LIGHT=1` and `VO_REDUCED_MOTION=1`.
    pub fn new(area: Size, pool: Rect) -> WaterPool {
        let env = |k: &str| std::env::var(k).is_ok_and(|v| v == "1");
        WaterPool {
            area,
            pool,
            drops: Vec::new(),
            tint: Color::TRANSPARENT,
            highlight: Color::hex(0xe8f4ff, 1.0),
            reflect: f32::NAN,
            squash: 1.3,
            bend: 6.0,
            edge: 0.35,
            specular: 0.9,
            radius: 0.0,
            slot: DEFAULT_SLOT,
            light: env("VO_LIGHT"),
            reduced_motion: env("VO_REDUCED_MOTION"),
        }
    }

    pub fn drops(mut self, drops: impl IntoIterator<Item = PoolDrop>) -> WaterPool {
        self.drops.extend(drops);
        self
    }

    pub fn drop_at(mut self, d: PoolDrop) -> WaterPool {
        self.drops.push(d);
        self
    }

    pub fn tint(self, tint: Color) -> WaterPool {
        WaterPool { tint, ..self }
    }

    pub fn highlight(self, highlight: Color) -> WaterPool {
        WaterPool { highlight, ..self }
    }

    pub fn reflect(self, reflect: f32) -> WaterPool {
        WaterPool { reflect: reflect.clamp(0.0, 1.0), ..self }
    }

    pub fn squash(self, squash: f32) -> WaterPool {
        WaterPool { squash: squash.max(0.05), ..self }
    }

    pub fn bend(self, bend: f32) -> WaterPool {
        WaterPool { bend, ..self }
    }

    pub fn edge(self, edge: f32) -> WaterPool {
        WaterPool { edge: edge.clamp(0.0, 1.0), ..self }
    }

    pub fn specular(self, specular: f32) -> WaterPool {
        WaterPool { specular: specular.max(0.0), ..self }
    }

    pub fn radius(self, radius: f32) -> WaterPool {
        WaterPool { radius: radius.max(0.0), ..self }
    }

    pub fn slot(self, slot: u32) -> WaterPool {
        WaterPool { slot, ..self }
    }

    pub fn light(self, light: bool) -> WaterPool {
        WaterPool { light, ..self }
    }

    pub fn reduced_motion(self, reduced_motion: bool) -> WaterPool {
        WaterPool { reduced_motion, ..self }
    }

    /// The tint in use: the one set, else the mode's default.
    pub fn tint_in_use(&self) -> Color {
        match (self.tint.a > 0.0, self.light) {
            (true, _) => self.tint,
            (false, false) => Color::hex(0x070d16, 0.94),
            (false, true) => Color::hex(0xd6e2ec, 0.94),
        }
    }

    /// The reflection strength in use.
    pub fn reflect_in_use(&self) -> f32 {
        match (self.reflect.is_nan(), self.light) {
            (false, _) => self.reflect,
            (true, false) => 0.55,
            (true, true) => 0.3,
        }
    }

    /// The pool's material.
    pub fn material(&self) -> Material {
        let p = self.pool;
        Material::custom(WATER_MATERIAL)
            .param(0, [p.x, p.y, p.w, p.h])
            .param(1, [self.reflect_in_use(), self.squash, self.bend, self.slot as f32])
            .param(2, [self.edge, self.specular, 90.0, if self.light { 1.0 } else { 0.0 }])
            .color(3, self.highlight)
            .reads_backdrop()
            .reads_images()
    }

    /// The pool's GPU source, for its painter.
    pub fn source(&self) -> WaterPoolSource {
        WaterPoolSource { drops: self.drops.clone(), reduced_motion: self.reduced_motion }
    }

    /// The pool as an element keyed `key`: a layer of size `area`, absolutely placed at its
    /// parent's top left, drawn over what comes before it.
    pub fn view<M>(&self, key: impl Into<Key>) -> Element<M> {
        let p = self.pool;
        let at = |e: Element<M>, x: f32, y: f32| {
            e.layout(|s| {
                s.position = taffy::style::Position::Absolute;
                s.inset.left = taffy::style::LengthPercentageAuto::length(x);
                s.inset.top = taffy::style::LengthPercentageAuto::length(y);
            })
        };
        let drive = Element::gpu("water-drive", Arc::new(self.source())).size(p.w, p.h);
        let water = Element::panel("water-pool")
            .size(p.w, p.h)
            .radius(self.radius)
            .fill(self.tint_in_use())
            .material(self.material());
        let layer = Element::layer("water-layer")
            .size(self.area.w, self.area.h)
            .backdrop(BackdropRes::Full, Color::TRANSPARENT)
            .child(at(water, p.x, p.y));
        at(Element::row(key).size(self.area.w, self.area.h), 0.0, 0.0)
            .child(at(drive, p.x, p.y))
            .child(at(layer, 0.0, 0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_settles_after_the_last_drop() {
        let pool = WaterPool::new(Size::new(600.0, 300.0), Rect::new(0.0, 200.0, 600.0, 100.0))
            .reduced_motion(false)
            .drops([PoolDrop::word(0.5, 0.2, 0.5), PoolDrop::word(1.5, 0.6, 1.0)]);
        assert_eq!(pool.source().until(), 1.5 + SETTLE_SECS);
        assert_eq!(pool.clone().reduced_motion(true).source().until(), f64::NEG_INFINITY);
        assert_eq!(WaterPool::new(pool.area, pool.pool).reduced_motion(false).source().until(), f64::NEG_INFINITY);
    }

    #[test]
    fn light_mode_has_a_pale_tint_and_a_fainter_reflection() {
        let dark = WaterPool::new(Size::new(600.0, 300.0), Rect::new(0.0, 200.0, 600.0, 100.0)).light(false);
        let light = dark.clone().light(true);
        let lum = |c: Color| c.r + c.g + c.b;
        assert!(lum(light.tint_in_use()) > 2.0 && lum(dark.tint_in_use()) < 0.3);
        assert!(light.reflect_in_use() < dark.reflect_in_use());
        assert_eq!(dark.clone().reflect(0.8).reflect_in_use(), 0.8);
        assert!(dark.material().reads_textures());
    }
}
