//! Water shading (EG-10): draws a [`Water`] surface as a pool in the popup.
//!
//! The scene side is `vo_scene::water::WaterPool`: a layer that captures its backdrop, a panel
//! with the water material, and a GPU element that carries the drops. This module is the GPU
//! side. [`install`] adds three things to a renderer:
//!
//! - the water material (`wgsl/water_material.wgsl`, id `vo_scene::water::WATER_MATERIAL`):
//!   the tint, the content above the pool mirrored at its top edge and bent by the wave
//!   normals, a bright edge that fades with depth, and specular glints;
//! - the surface's normal texture ([`Water::normal_view`]) in the pool's image slot;
//! - a painter ([`WaterPainter`]) for the pool's GPU element. It draws nothing: it moves the
//!   simulation's [`Timeline`] to the frame's time and calls [`Water::flush`], which submits the
//!   compute work before the frame's own command buffer, so the material reads this frame's
//!   normals.
//!
//! The drops come from the element, so the surface at `t` is a function of `t` and the drop
//! list: a seek back restores a snapshot and steps forward, and an export at any frame rate
//! draws the same frame at the same `t`. A drop list that only grows (new words) adds the new
//! drops; any other change replays from the start.
//!
//! Reduced motion (the pool's or the config's): the timeline stays at tick 0 with no drops, so
//! the water is flat and the reflection still, and no tick is dispatched.
//!
//! One pool per renderer: the painter owns one surface, sized by [`install`]'s config.
//!
//! Cost: one wave tick per 1/120 s of scene time (two per frame at 60 fps) plus one output pass,
//! both on a 128x64 grid at 600x300 (`Water::gpu_cost`), and the material's three texture reads
//! per pool pixel. `effect_cost` has the `water pool` case.

use vo_render::{GpuPainter, PaintCx, Renderer};
use vo_scene::water::{PoolDrop, WaterPool, WaterPoolSource, WATER_MATERIAL};

use crate::water::{Water, WaterConfig, WaterDrop};
use crate::Timeline;

/// The material's WGSL, `fn mat_water`.
pub const WATER_MATERIAL_WGSL: &str = include_str!("wgsl/water_material.wgsl");

/// Steps the pool's water to each frame's time. See the module docs.
pub struct WaterPainter {
    tl: Timeline<Water>,
    drops: Vec<PoolDrop>,
}

impl WaterPainter {
    pub fn new(water: Water) -> WaterPainter {
        WaterPainter { tl: Timeline::new(water), drops: Vec::new() }
    }

    pub fn timeline(&self) -> &Timeline<Water> {
        &self.tl
    }

    /// Makes the timeline's events match `drops`.
    fn sync(&mut self, drops: &[PoolDrop]) {
        if drops == self.drops.as_slice() {
            return;
        }
        if !drops.starts_with(&self.drops) {
            self.tl.clear_events();
            self.drops.clear();
        }
        for d in &drops[self.drops.len()..] {
            self.tl.push_event(d.t, WaterDrop { x: d.x, y: d.y, radius: d.radius, strength: d.strength });
        }
        self.drops = drops.to_vec();
    }

    /// Moves the water to time `t` with these drops and refreshes its textures.
    pub fn step_to(&mut self, source: &WaterPoolSource, t: f64) {
        if source.reduced_motion || self.tl.sim().config().reduced_motion {
            self.sync(&[]);
            self.tl.goto_tick(0);
        } else {
            self.sync(&source.drops);
            self.tl.state_at(t);
        }
        self.tl.sim().flush();
    }
}

impl GpuPainter for WaterPainter {
    fn paint(&mut self, cx: &mut PaintCx<'_>) -> bool {
        let Some(source) = cx.source.as_any().downcast_ref::<WaterPoolSource>() else {
            return false;
        };
        self.step_to(source, cx.t);
        true
    }
}

/// Adds the water material, the normal texture in image slot `slot` and the painter to `r`,
/// with a surface made from `cfg`.
pub fn install(r: &mut Renderer, cfg: WaterConfig, slot: u32) {
    let water = Water::new(r.device(), r.queue(), cfg);
    r.set_material_image(slot as usize, Some(water.normal_view().clone()));
    r.add_material(WATER_MATERIAL, "mat_water", WATER_MATERIAL_WGSL);
    r.add_painter(Box::new(WaterPainter::new(water)));
}

/// [`install`] for `pool`: a grid sized to the pool's box at `scale` physical pixels per unit
/// (about one cell per 4.7 px), its image slot and its reduced motion.
pub fn install_pool(r: &mut Renderer, pool: &WaterPool, scale: f32) {
    let cfg = WaterConfig {
        reduced_motion: pool.reduced_motion,
        ..WaterConfig::for_size(pool.pool.w * scale, pool.pool.h * scale)
    };
    install(r, cfg, pool.slot);
}
