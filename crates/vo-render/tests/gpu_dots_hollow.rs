//! Hollow ring dots (FX-011): `Dot::ring` draws a dot as a ring, and a ring as wide as the
//! radius is a filled dot, so springing `ring` from 0.1 to 0.5 fills a dot in.
//!
//! `dots-hollow`: rings of width 0, 0.05, 0.1, 0.15, 0.2, 0.3, 0.4 and 0.5 of the diameter,
//! left to right, at three dot sizes, at 1x and 2x.
//!
//! GPU test: `just test-gpu dots_hollow`.

mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::dots::{Dot, DotSource, Dots};
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::Scene;

const RINGS: [f32; 8] = [0.0, 0.05, 0.1, 0.15, 0.2, 0.3, 0.4, 0.5];

#[derive(Debug)]
struct Rings;

impl DotSource for Rings {
    fn dots(&self, _: f64, out: &mut Vec<Dot>) {
        for (row, size) in [0.5f32, 0.75, 0.95].into_iter().enumerate() {
            for (col, ring) in RINGS.into_iter().enumerate() {
                let color = Color::hex(0xff8a1f, 1.0);
                out.push(Dot { x: col as f32, y: row as f32, size, color, snap: 1.0, ring });
            }
        }
    }

    fn moving(&self, _: f64) -> bool {
        false
    }
}

fn render(scale: f32) -> (Image, usize) {
    let size = Size::new(8.0 * 24.0 + 16.0, 3.0 * 24.0 + 16.0);
    let view = Element::panel("bg")
        .size(size.w, size.h)
        .pad(8.0)
        .fill(Color::hex(0x0b0806, 1.0))
        .child(Element::dots("dots", Dots::new(Arc::new(Rings), 24.0)).size(8.0 * 24.0, 3.0 * 24.0));
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, size, scale);
    scene.apply(view, 0.0);
    let list = scene.sample(1.0);
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let (w, h) = ((size.w * scale) as u32, (size.h * scale) as u32);
    let px = off.render_to_rgba(&list, [w, h], scale);
    let rings = list.dots.iter().filter(|d| d.ring > 0.0).count();
    (Image { name: String::new(), w, h, channels: 4, px }, rings)
}

#[test]
fn hollow_ring_dots() {
    for (name, scale) in [("dots-hollow", 1.0), ("dots-hollow-2x", 2.0)] {
        let (img, rings) = render(scale);
        assert_eq!(rings, 3 * 7, "every dot but the first column carries a ring");
        support::check(&Image { name: name.into(), ..img }, Tolerance::default()).unwrap();
    }
}
