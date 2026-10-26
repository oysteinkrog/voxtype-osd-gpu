//! Compositor blur behind a surface through `ext-background-effect-v1` (FX07). KWin 6.8 blurs
//! behind layer-shell surfaces and takes a new region with every commit; see the
//! `blur_probe` example.

use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::reexports::protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::{self, ExtBackgroundEffectManagerV1},
    ext_background_effect_surface_v1::{self, ExtBackgroundEffectSurfaceV1},
};
use vo_host::InputRegion;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};

use crate::State;

/// The `blur` bit of the manager's capabilities.
const CAP_BLUR: u32 = 1;

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for State {
    fn event(
        state: &mut State,
        _: &ExtBackgroundEffectManagerV1,
        ev: ext_background_effect_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let ext_background_effect_manager_v1::Event::Capabilities { flags } = ev {
            let bits = match flags {
                WEnum::Value(f) => f.bits(),
                WEnum::Unknown(u) => u,
            };
            state.caps.backdrop_blur = bits & CAP_BLUR != 0;
        }
    }
}

impl Dispatch<ExtBackgroundEffectSurfaceV1, ()> for State {
    fn event(
        _: &mut State,
        _: &ExtBackgroundEffectSurfaceV1,
        _: ext_background_effect_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

/// Sets the blur region for the next commit; `None` turns the blur off. Rectangles are in
/// surface-local logical pixels.
pub(crate) fn set_region(
    compositor: &CompositorState,
    effect: &ExtBackgroundEffectSurfaceV1,
    region: Option<&InputRegion>,
) {
    match region {
        None => effect.set_blur_region(None),
        Some(r) => {
            let wl = Region::new(compositor).expect("wl_compositor makes regions");
            for rect in &r.rects {
                wl.add(rect.x, rect.y, rect.w, rect.h);
            }
            effect.set_blur_region(Some(wl.wl_region()));
        }
    }
}
