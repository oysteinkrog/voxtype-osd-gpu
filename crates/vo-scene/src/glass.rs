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
use crate::geom::{Affine, Color, Point};

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
        }
    }

    /// The same glass with the orbiting rim on.
    pub const fn with_orbit(mut self, accent: Color) -> Glass {
        self.orbit = true;
        self.accent = accent;
        self
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
    /// sheen, then 30 fps for the orbit.
    pub fn next_frame(&self, since: f64, t: f64) -> Option<f64> {
        let age = t - since;
        if self.sheen && age < SHEEN_SECS {
            Some(t + 1.0 / 60.0)
        } else if self.orbit {
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
}

/// Rectangles covering a rounded rect, one band per pixel row of the corner radius, each
/// rounded outward to whole logical pixels, for the compositor blur region. Coarser bands
/// leave visible blur steps outside the corner (the stair-step test in the `blur_probe`
/// example of `vo-host-wayland`).
pub fn blur_rects(rect: [f32; 4], radius: f32, out: &mut Vec<IRect>) {
    let (x0, y0) = (rect[0].floor() as i32, rect[1].floor() as i32);
    let (x1, y1) = ((rect[0] + rect[2]).ceil() as i32, (rect[1] + rect[3]).ceil() as i32);
    let r = radius.min(rect[2] / 2.0).min(rect[3] / 2.0).max(0.0);
    let ri = r.ceil() as i32;
    for row in 0..ri {
        // The row's edge nearest the straight side has the smallest inset in the row.
        let dy = (r - (row + 1) as f32).max(0.0);
        let inset = (r - (r * r - dy * dy).sqrt()).floor() as i32;
        let w = x1 - x0 - 2 * inset;
        out.push(IRect { x: x0 + inset, y: y0 + row, w, h: 1 });
        out.push(IRect { x: x0 + inset, y: y1 - row - 1, w, h: 1 });
    }
    out.push(IRect { x: x0, y: y0 + ri, w: x1 - x0, h: (y1 - y0 - 2 * ri).max(0) });
}

impl DisplayList {
    /// The compositor blur region in surface logical pixels: the union of the glass panels
    /// that ask for blur behind and are at least [`BLUR_MIN_ALPHA`] opaque. Empty when there
    /// are none. A rotated layer gets its bounding box.
    pub fn blur_region(&self) -> InputRegion {
        let mut rects = Vec::new();
        for g in self.glass.iter().filter(|g| g.glass.blur_behind && g.surface_alpha >= BLUR_MIN_ALPHA) {
            let m = g.to_surface;
            let [x, y, w, h] = g.rect;
            let p0 = m.apply(Point { x, y });
            let p1 = m.apply(Point { x: x + w, y: y + h });
            let k = (m.a * m.a + m.b * m.b).sqrt();
            let (x0, y0) = (p0.x.min(p1.x), p0.y.min(p1.y));
            let r = [x0, y0, (p1.x - p0.x).abs(), (p1.y - p0.y).abs()];
            blur_rects(r, if m.is_axis_aligned() { g.radius * k } else { 0.0 }, &mut rects);
        }
        InputRegion { rects }
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
}
