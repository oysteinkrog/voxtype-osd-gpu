//! Goldens for the dot grid (VE05): a scene with `Element::dots` over a spring-driven
//! `DotField`, sampled in flight and at rest, drawn by the dot pipeline.
//!
//! GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_core::hash::rand_f32;
use vo_dots::{DotField, Dots, Look, Springs};
use vo_render::Offscreen;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::Scene;

const COLS: u32 = 48;
const ROWS: u32 = 12;

/// A grid where every dot springs from the centre row to its cell, with its own size,
/// brightness and colour from a seeded hash, staggered from the centre column outward.
fn grid() -> DotField {
    let mut f = DotField::new(0.0, Springs::default());
    let blue = Color::hex(0x5ab0ff, 1.0);
    let red = Color::hex(0xff4d4d, 1.0);
    for y in 0..ROWS {
        for x in 0..COLS {
            let id = y * COLS + x;
            let r = |k: u32| rand_f32(7, x, y, k);
            let color = if r(0) < 0.15 {
                red
            } else if r(1) < 0.3 {
                blue
            } else {
                Color::WHITE
            };
            let look = Look { x: x as f32, y: y as f32, size: 0.35 + 0.45 * r(2), bright: 0.3 + 0.7 * r(3), color };
            let from = Look::new(x as f32, (ROWS / 2) as f32, 0.2, Color::WHITE);
            let delay = 0.012 * (x as f64 - COLS as f64 / 2.0).abs();
            f.retarget(0.0, id, look, delay, Some(from));
        }
    }
    f
}

fn view(field: &Arc<DotField>) -> Element {
    Element::column("root")
        .pad(6.0)
        .fill(Color::hex(0x0b0b0d, 1.0))
        .child(Element::dots("grid", Dots::new(field.clone(), 5.0)).size(COLS as f32 * 5.0, ROWS as f32 * 5.0))
}

#[test]
fn dot_grid_goldens() {
    let field = Arc::new(grid());
    let rest = field.rest_at();
    assert!(rest > 0.3 && rest < 1.5, "rest at {rest}");
    let mut scene: Scene = Scene::new();
    let size = Size::new(COLS as f32 * 5.0 + 12.0, ROWS as f32 * 5.0 + 12.0);
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(view(&field), 0.0);
    assert!(scene.next_wake(0.1).is_some());
    assert!(scene.next_wake(rest).is_none(), "the grid rests at {rest}");

    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let mut images = Vec::new();
    for (name, t, scale) in
        [("dots-grid-flight", 0.12, 1.0), ("dots-grid-rest", rest, 1.0), ("dots-grid-rest-1.5x", rest, 1.5)]
    {
        scene.set_viewport(t, size, scale);
        let list = scene.sample(t);
        assert_eq!(list.dots.len(), (COLS * ROWS) as usize, "{name}");
        let (w, h) = ((size.w * scale).ceil() as u32, (size.h * scale).ceil() as u32);
        let px = off.render_to_rgba(&list, [w, h], scale);
        images.push(Image { name: name.into(), w, h, channels: 4, px });
    }
    let errors: Vec<String> = images.iter().filter_map(|i| support::check(i, Tolerance::default()).err()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// At rest every dot of one size covers its pixels the same way, wherever it is: the pitch
/// is whole pixels and the centres are snapped.
#[test]
fn resting_dots_are_identical_on_the_pixel_grid() {
    let mut f = DotField::new(0.0, Springs::default());
    for i in 0..20u32 {
        f.place(0.0, i, Look::new(i as f32, 0.0, 0.6, Color::WHITE));
    }
    let field = Arc::new(f);
    let mut scene: Scene = Scene::new();
    // A pitch and an offset that are not whole pixels at 1.5x.
    scene.set_viewport(0.0, Size::new(140.0, 12.0), 1.5);
    scene
        .apply(Element::column("root").pad(1.3).child(Element::dots("d", Dots::new(field, 4.3)).size(130.0, 8.0)), 0.0);
    let list = scene.sample(0.0);
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let (w, h) = (210, 18);
    let px = off.render_to_rgba(&list, [w, h], 1.5);
    // Alpha of the 5x5 pixel block around each dot's snapped centre.
    let pitch_px = (4.3f32 * 1.5).round() as usize;
    let alpha = |x: usize, y: usize| px[(y * w as usize + x) * 4 + 3];
    let first = list.dots[0].centre;
    let (cx, cy) = ((first.x * 1.5).floor() as usize, (first.y * 1.5).floor() as usize);
    let block = |i: usize| {
        let mut v = Vec::new();
        for dy in 0..5 {
            for dx in 0..5 {
                v.push(alpha(cx + i * pitch_px + dx - 2, cy + dy - 2));
            }
        }
        v
    };
    let b0 = block(0);
    assert!(b0.iter().any(|&a| a > 200), "{b0:?}");
    for i in 1..20 {
        assert_eq!(block(i), b0, "dot {i}");
    }
}

/// A field holding one dot text: `from` at rest, then changed to `to` by `change` at 1 s.
fn text_field(font: vo_dots::DotFont, from: &str, to: &str, change: vo_dots::Change) -> DotField {
    use vo_dots::{Change, DotText, TextStyle};
    let mut f = DotField::new(0.0, Springs::default());
    let mut text = DotText::new(TextStyle::new(font, Color::hex(0xe8e8ea, 1.0)), (1.0, 1.0), 0, 1 << 20);
    text.set(&mut f, 0.0, from, Change::Cut);
    text.set(&mut f, 1.0, to, change);
    f
}

/// TY16: the 5x7 and 7x9 dot fonts, and a string morphing, dissolving into another, sampled
/// before, during and after.
#[test]
fn dot_text_goldens() {
    use vo_dots::{Change, DotFont};
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let mut images = Vec::new();
    let cases = [
        ("dots-text-5x7-morph", DotFont::SMALL, Change::Morph, [0.9, 1.05, 1.12, 1.6]),
        ("dots-text-5x7-dissolve", DotFont::SMALL, Change::Dissolve, [0.9, 1.08, 1.18, 1.6]),
        ("dots-text-7x9-morph", DotFont::LARGE, Change::Morph, [0.9, 1.05, 1.12, 1.6]),
    ];
    for (name, font, change, times) in cases {
        let field = Arc::new(text_field(font, "Listening ÆØÅ", "Finishing æøå", change));
        let pitch = 3.0;
        let (cols, rows) = (font.width("Listening ÆØÅ", 1) as f32 + 2.0, font.line_height() as f32 + 2.0);
        let (bw, bh) = (cols * pitch, rows * pitch);
        // One frame per time, stacked.
        let view = Element::column("root").fill(Color::hex(0x0b0b0d, 1.0)).children(
            times
                .iter()
                .enumerate()
                .map(|(i, _)| Element::dots(format!("t{i}"), Dots::new(field.clone(), pitch)).size(bw, bh)),
        );
        let size = Size::new(bw, bh * times.len() as f32);
        let (w, h) = (size.w.ceil() as u32, size.h.ceil() as u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for (i, &t) in times.iter().enumerate() {
            let mut scene: Scene = Scene::new();
            scene.set_viewport(0.0, size, 1.0);
            scene.apply(view.clone(), 0.0);
            let px = off.render_to_rgba(&scene.sample(t), [w, h], 1.0);
            // Keep band i of the frame sampled at t.
            let (y0, y1) = ((i as f32 * bh) as u32, (((i + 1) as f32 * bh) as u32).min(h));
            let (a, b) = ((y0 * w * 4) as usize, (y1 * w * 4) as usize);
            rgba[a..b].copy_from_slice(&px[a..b]);
        }
        images.push(Image { name: name.into(), w, h, channels: 4, px: rgba });
    }
    let errors: Vec<String> = images.iter().filter_map(|i| support::check(i, Tolerance::default()).err()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
