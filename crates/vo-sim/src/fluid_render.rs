//! Ink and smoke shading (EG-12): draws a [`Fluid`] as dye in a vo scene.
//!
//! The scene side is `vo_scene::fluid::FluidLayer`: a GPU element that carries the splats, a
//! panel with the dye material, and an optional smear (a displacement layer that reads the
//! velocity texture). This module is the GPU side. [`install`] adds three things to a
//! renderer:
//!
//! - the dye material (`wgsl/fluid_material.wgsl`, id `vo_scene::fluid::FLUID_MATERIAL`): ink
//!   in water, luminous smoke or watercolour, darkening the paper in light mode;
//! - the dye texture ([`Fluid::dye_view`]) and the velocity texture ([`Fluid::velocity_view`])
//!   in the layer's two image slots;
//! - a painter ([`FluidPainter`]) for the layer's GPU element. It draws nothing: it moves the
//!   simulation's [`Timeline`] to the frame's time and calls [`Fluid::flush`], which submits
//!   the compute work before the frame's own command buffer, so the material and the smear
//!   read this frame's textures.
//!
//! The splats come from the element, so the fluid at `t` is a function of `t` and the splat
//! list: a seek back restores a snapshot and steps forward, and an export at any frame rate
//! draws the same frame at the same `t`. A list that only grows (new words) adds the new
//! splats; any other change replays from the start.
//!
//! Reduced motion: the fluid is made with `FluidConfig::reduced_motion` ([`install_layer`]
//! takes it from the layer), so pushes are dropped, no tick is dispatched and dye lands where
//! it is placed. A source that asks for reduced motion on a fluid made without it has its
//! pushes dropped here, so its dye stays where it lands too.
//!
//! One fluid per renderer: the painter owns one simulation, sized by [`install`]'s config.
//!
//! Cost: one fluid tick per 1/120 s of scene time (two per frame at 60 fps) plus one output
//! pass (`Fluid::gpu_cost`); the material reads the dye five times per pixel of the box, and
//! the smear is one displacement pass. `effect_cost` has the `ink`, `smoke` and `watercolour`
//! cases. Measured on the dev box's GPU at 600x300 with [`LAYER_PRESSURE_ITERATIONS`] sweeps:
//! two ticks and the output pass 0.32 ms; `ink_demo --cost` frames (sim plus render) p95
//! 0.39 ms ink, 0.46 ms smoke, 0.38 ms watercolour; `effect_cost` adds 0.33, 0.41 and 0.34 ms
//! over the plain card.

use vo_render::{GpuPainter, PaintCx, Renderer};
use vo_scene::fluid::{FluidLayer, FluidSource, InkSplat, FLUID_MATERIAL};

use crate::fluid::{Fluid, FluidConfig, FluidSplat};
use crate::Timeline;

/// The material's WGSL, `fn mat_fluid`.
pub const FLUID_MATERIAL_WGSL: &str = include_str!("wgsl/fluid_material.wgsl");

/// Pressure sweeps per tick for a fluid layer. Fewer than the sim's default 20: at the popup's
/// grid each sweep is two dispatches of mostly fixed overhead, and dye drawn soft hides the
/// small divergence left over. Keeps the whole frame inside the 1 ms budget at 600x300.
pub const LAYER_PRESSURE_ITERATIONS: u32 = 8;

/// Steps the layer's fluid to each frame's time. See the module docs.
pub struct FluidPainter {
    tl: Timeline<Fluid>,
    splats: Vec<InkSplat>,
    /// The splats were synced with their pushes dropped.
    still: bool,
}

impl FluidPainter {
    pub fn new(fluid: Fluid) -> FluidPainter {
        FluidPainter { tl: Timeline::new(fluid), splats: Vec::new(), still: false }
    }

    pub fn timeline(&self) -> &Timeline<Fluid> {
        &self.tl
    }

    /// Makes the timeline's events match `splats`, with the pushes dropped when `still`.
    fn sync(&mut self, splats: &[InkSplat], still: bool) {
        if splats == self.splats.as_slice() && still == self.still {
            return;
        }
        if !splats.starts_with(&self.splats) || still != self.still {
            self.tl.clear_events();
            self.splats.clear();
        }
        for s in &splats[self.splats.len()..] {
            let velocity = if still { [0.0; 2] } else { s.velocity };
            if velocity == [0.0; 2] && s.amount == 0.0 {
                // A push with its push dropped does nothing; keep the event list short.
                continue;
            }
            self.tl.push_event(
                s.t,
                FluidSplat { x: s.x, y: s.y, radius: s.radius, velocity, color: s.color, amount: s.amount },
            );
        }
        self.splats = splats.to_vec();
        self.still = still;
    }

    /// Moves the fluid to time `t` with these splats and refreshes its textures.
    pub fn step_to(&mut self, source: &FluidSource, t: f64) {
        self.sync(&source.splats, source.reduced_motion);
        self.tl.state_at(t);
        self.tl.sim().flush();
    }
}

impl GpuPainter for FluidPainter {
    fn paint(&mut self, cx: &mut PaintCx<'_>) -> bool {
        let Some(source) = cx.source.as_any().downcast_ref::<FluidSource>() else {
            return false;
        };
        self.step_to(source, cx.t);
        true
    }
}

/// Adds the dye material, the dye texture in image slot `dye_slot`, the velocity texture in
/// `velocity_slot` and the painter to `r`, with a fluid made from `cfg`.
pub fn install(r: &mut Renderer, cfg: FluidConfig, dye_slot: u32, velocity_slot: u32) {
    let fluid = Fluid::new(r.device(), r.queue(), cfg);
    r.set_material_image(dye_slot as usize, Some(fluid.dye_view().clone()));
    r.set_material_image(velocity_slot as usize, Some(fluid.velocity_view().clone()));
    r.add_material(FLUID_MATERIAL, "mat_fluid", FLUID_MATERIAL_WGSL);
    r.add_painter(Box::new(FluidPainter::new(fluid)));
}

/// The fluid config [`install_layer`] uses for `layer` at `scale` physical pixels per unit: a
/// grid of about one cell per 4.7 px over the layer's box, [`LAYER_PRESSURE_ITERATIONS`] sweeps
/// and the layer's reduced motion.
pub fn layer_config(layer: &FluidLayer, scale: f32) -> FluidConfig {
    FluidConfig {
        reduced_motion: layer.reduced_motion,
        pressure_iterations: LAYER_PRESSURE_ITERATIONS,
        ..FluidConfig::for_size(layer.area.w * scale, layer.area.h * scale)
    }
}

/// [`install`] for `layer` with [`layer_config`] and the layer's image slots.
pub fn install_layer(r: &mut Renderer, layer: &FluidLayer, scale: f32) {
    install(r, layer_config(layer, scale), layer.dye_slot, layer.velocity_slot);
}
