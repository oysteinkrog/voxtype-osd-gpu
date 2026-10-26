//! Goldens for grid-snapped dot particles (PA09): a row of dots that flies up into a check,
//! and the check peeling off and flying away.
//!
//! GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_dots::{assemble, check_cells, peel_off, DotField, Dots, Look, Peel, Springs};
use vo_render::Offscreen;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::Scene;

const COLS: u32 = 60;
const ROWS: u32 = 16;
const PITCH: f32 = 4.0;

/// The field after a check assembles at 0 s from a resting centre row, and peels off at
/// `peel_at`.
fn field(peel_at: Option<f64>) -> DotField {
    let mut f = DotField::new(0.0, Springs::default());
    let row: Vec<Look> =
        (0..COLS).map(|x| Look::new(x as f32, (ROWS - 2) as f32, 0.34, Color::hex(0xffffff, 0.55))).collect();
    let check = check_cells(15, 9, 2);
    let look = Look::new(0.0, 0.0, 0.7, Color::WHITE);
    let ids = assemble(&mut f, 0.0, &row, &check, ((COLS - 15) as f32 * 0.5, 2.0), look, 0, 0.004, 0.18);
    if let Some(at) = peel_at {
        f.prune(at);
        peel_off(&mut f, at, &ids, &Peel { distance: 10.0, spread: 6.0, ..Peel::default() });
    }
    f
}

#[test]
fn check_assembles_and_peels_off() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let size = Size::new(COLS as f32 * PITCH, ROWS as f32 * PITCH);
    let assembling = Arc::new(field(None));
    let rest = assembling.rest_at();
    assert!(rest < 1.0, "the check rests at {rest}");
    let peeling = Arc::new(field(Some(1.5)));
    let mut strips = Vec::new();
    for (f, t) in [
        (&assembling, 0.0),
        (&assembling, 0.12),
        (&assembling, 0.25),
        (&assembling, rest),
        (&peeling, 1.6),
        (&peeling, 1.75),
    ] {
        let mut scene: Scene = Scene::new();
        scene.set_viewport(0.0, size, 1.0);
        scene.apply(
            Element::column("root")
                .fill(Color::hex(0x0b0b0d, 1.0))
                .child(Element::dots("d", Dots::new(f.clone(), PITCH)).size(size.w, size.h)),
            0.0,
        );
        let (w, h) = (size.w as u32, size.h as u32);
        strips.push(off.render_to_rgba(&scene.sample(t), [w, h], 1.0));
    }
    // One image, the frames stacked.
    let (w, h) = (size.w as u32, size.h as u32 * strips.len() as u32);
    let px = strips.concat();
    let img = Image { name: "dots-particles-check".into(), w, h, channels: 4, px };
    support::check(&img, Tolerance::default()).unwrap();
}
