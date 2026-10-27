//! The stage (`vo_scene::stage`): 2D layers turned and pushed back in one perspective camera.
//! A panel pushed back and turned about y, a line of text tilted back about x, and a ring
//! laid nearly flat into an ellipse under it, as in a hologram theme. Shapes, glyphs and
//! dots go through the layer's warp in the vertex stage; clips and antialiasing map fragments
//! back through its inverse.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::{DisplayList, ShapeKind};
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::taffy::{AlignItems, JustifyContent};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const W: f32 = 600.0;
const H: f32 = 300.0;
const RED: u32 = 0xff3040;

/// `tilt` scales every turn and push, so 0 is the flat layout.
fn view(tilt: f32) -> Element<()> {
    let back = Element::layer("back")
        .translate_z(-400.0 * tilt)
        .rotate_y(0.9 * tilt)
        .child(Element::panel("card").size(220.0, 70.0).radius(12.0).fill(Color::hex(RED, 1.0)));
    let words = Element::layer("words").rotate_x(0.6 * tilt).clip().child(
        Element::label("line", "so the plan for tomorrow")
            .text_style(TextStyle::new(0, 34.0, 40.0))
            .color(Color::hex(0x8fd8ff, 1.0)),
    );
    let ring = Element::layer("ring")
        .rotate_x(1.25 * tilt)
        .margin_edges(-70.0, 0.0, -70.0, 0.0)
        .child(Element::shape("disc", ShapeKind::Circle).size(200.0, 200.0).stroke(Color::hex(0x5f8dff, 1.0), 6.0));
    Element::panel("root")
        .size(W, H)
        .fill(Color::hex(0x0b0e16, 1.0))
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
        .child(back)
        .child(words)
        .child(ring)
}

fn sample(fonts: &FontRegistry, tilt: f32) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(view(tilt), 0.0);
    scene.sample(5.0)
}

/// Rows of the red card covered at column `x`.
fn red_height(img: &Image, x: u32) -> u32 {
    (0..img.h)
        .filter(|&y| {
            let i = ((y * img.w + x) * 4) as usize;
            img.px[i] > 200 && img.px[i + 1] < 120
        })
        .count() as u32
}

/// Columns `x0..x1` that hold red, as `(first, last)`.
fn red_span(img: &Image) -> (u32, u32) {
    let cols: Vec<u32> = (0..img.w).filter(|&x| red_height(img, x) > 0).collect();
    (*cols.first().unwrap(), *cols.last().unwrap())
}

#[test]
fn turned_layers_in_perspective() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let flat = sample(&fonts, 0.0);
    assert!(flat.layers.iter().all(|l| l.warp.is_none()), "nothing turned, nothing warped");
    let turned = sample(&fonts, 1.0);
    assert_eq!(turned.layers.iter().filter(|l| l.warp.is_some()).count(), 3);

    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let render = |off: &mut Offscreen, list: &DisplayList, name: &str, scale: f32| {
        let (w, h) = ((W * scale) as u32, (H * scale) as u32);
        Image { name: name.into(), w, h, channels: 4, px: off.render_to_rgba(list, [w, h], scale) }
    };
    let plain = render(&mut off, &flat, "stage-flat", 1.0);
    let img = render(&mut off, &turned, "stage-turned", 1.0);

    // The card pushed back is smaller: narrower than 220 and shorter than 70.
    let (a, b) = red_span(&plain);
    let (c, d) = red_span(&img);
    assert!((b - a) > 210 && (d - c) < b - a - 40, "flat {a}..{b}, turned {c}..{d}");
    // Turned about y with its right edge away: the right edge is shorter than the left.
    // Sampled past the 12 px corners.
    let (left, right) = (red_height(&img, c + 14), red_height(&img, d - 14));
    assert!(right + 2 < left, "left edge {left} rows, right edge {right}");

    // The ring laid back is an ellipse: much wider than tall.
    let blue = |img: &Image, x: u32, y: u32| {
        let i = ((y * img.w + x) * 4) as usize;
        img.px[i + 2] > 150 && img.px[i] < 140
    };
    let rows: Vec<u32> = (0..img.h).filter(|&y| (0..img.w).any(|x| blue(&img, x, y))).collect();
    let flat_rows: Vec<u32> = (0..plain.h).filter(|&y| (0..plain.w).any(|x| blue(&plain, x, y))).collect();
    assert!(rows.len() < flat_rows.len(), "the ring should be squashed: {} vs {} rows", rows.len(), flat_rows.len());

    let hi = render(&mut off, &turned, "stage-turned-2x", 2.0);
    let failures: Vec<String> =
        [&plain, &img, &hi].iter().filter_map(|i| support::check(i, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// A tilt is a spring like any other property: half way, the warp is part way.
#[test]
fn tilt_springs() {
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    let el = |a: f32| {
        Element::panel("root")
            .size(W, H)
            .child(Element::layer("l").rotate_x(a).child(Element::panel("p").size(100.0, 100.0).fill(Color::WHITE)))
    };
    scene.apply(el(0.0), 0.0);
    scene.apply(el(1.0), 1.0);
    assert!(scene.next_wake(1.05).is_some(), "the turn is moving");
    let mid = scene.sample(1.05);
    let end = scene.sample(10.0);
    let squash = |l: &DisplayList| {
        let w = l.layers.iter().find_map(|l| l.warp).expect("warped");
        let p = |y: f32| w.apply(vo_scene::geom::Point::new(50.0, y)).y;
        p(100.0) - p(0.0)
    };
    assert!(squash(&mid) > squash(&end), "mid {} end {}", squash(&mid), squash(&end));
    assert!(scene.next_wake(10.0).is_none());
}
