//! Glass panels (FX06, FX08) through the renderer's display list pass: dark and light glass
//! over coloured bars, during the sheen and with the orbiting rim. Goldens at 1x and 2x and a
//! determinism check.
//!
//! GPU test: `just test-gpu` runs it. Goldens hold the premultiplied bytes as read back.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::{DisplayList, Item, ItemKind, Layer, Paint, ShapeInstance};
use vo_scene::geom::{Affine, Color, RRect, Rect, Size};
use vo_scene::glass::{Glass, GlassInstance};

const W: f32 = 420.0;
const H: f32 = 200.0;

fn hex(rgb: u32) -> Color {
    Color::hex(rgb, 1.0)
}

/// Coloured bars and thin white lines behind two panels, dark on the left, light on the right,
/// `t` seconds after they entered.
fn scene(t: f64) -> DisplayList {
    let mut list = DisplayList { size: Size::new(W, H), scale: 1.0, ..Default::default() };
    list.layers.push(Layer::root());
    let bars = [0xe62832, 0xfac81e, 0x28b450, 0x1e78e6, 0x9632c8, 0xf0f0f0];
    for (i, c) in bars.iter().enumerate() {
        let x = i as f32 * W / bars.len() as f32;
        list.shapes.push(ShapeInstance::rrect(
            RRect::new(Rect::new(x, 0.0, W / bars.len() as f32, H), 0.0),
            Paint::solid(hex(*c)),
        ));
    }
    for y in (0..H as u32).step_by(18) {
        list.shapes
            .push(ShapeInstance::rrect(RRect::new(Rect::new(0.0, y as f32, W, 2.0), 0.0), Paint::solid(hex(0xffffff))));
    }
    list.items.push(Item { kind: ItemKind::Shapes, start: 0, len: list.shapes.len() as u32 });
    let accent = hex(0x4c9bff);
    for (i, g) in [Glass::dark().with_orbit(accent), Glass::light().with_orbit(accent)].into_iter().enumerate() {
        let (sheen, orbit, orbit_strength) = g.motion(0.0, t);
        list.glass.push(GlassInstance {
            rect: [24.0 + i as f32 * 200.0, 40.0, 172.0, 110.0],
            radius: 16.0,
            glass: g,
            alpha: 1.0,
            age: t as f32,
            sheen,
            orbit,
            orbit_strength,
            to_surface: Affine::IDENTITY,
        });
    }
    list.items.push(Item { kind: ItemKind::Glass, start: 0, len: 2 });
    list.layers[0].items = 0..list.items.len() as u32;
    list
}

fn render(off: &mut Offscreen, name: &str, list: &DisplayList, scale: f32) -> Image {
    let (w, h) = ((list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32);
    let px = off.render_to_rgba(list, [w, h], scale);
    Image { name: name.into(), w, h, channels: 4, px }
}

#[test]
fn glass_goldens() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let cases = [("glass-sheen-1x", 0.25, 1.0), ("glass-orbit-1x", 2.0, 1.0), ("glass-orbit-2x", 2.0, 2.0)];
    let mut failures = Vec::new();
    for (name, t, scale) in cases {
        let img = render(&mut off, name, &scene(t), scale);
        if let Err(e) = support::check(&img, Tolerance::default()) {
            failures.push(e);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn glass_is_deterministic() {
    let gpu = support::gpu();
    let mut a = Offscreen::new(&gpu.device, &gpu.queue);
    let mut b = Offscreen::new(&gpu.device, &gpu.queue);
    let list = scene(2.0);
    assert_eq!(render(&mut a, "a", &list, 1.0).px, render(&mut b, "b", &list, 1.0).px);
}

/// The glass's own pixels in the panel interior, over the bars, keep the interior tint's
/// coverage: the backdrop can never show through more than the contrast gate allows.
#[test]
fn interior_keeps_the_tint_alpha() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let mut list = scene(2.0);
    // Glass alone, no bars.
    list.items.remove(0);
    list.layers[0].items = 0..1;
    let img = render(&mut off, "glass-alone", &list, 1.0);
    for (i, g) in [Glass::dark(), Glass::light()].iter().enumerate() {
        let (x, y) = (24 + i * 200 + 86, 40 + 55);
        let a = img.px[(y * W as usize + x) * 4 + 3] as f32 / 255.0;
        assert!(a + 1.0 / 255.0 >= g.tint.a, "panel {i}: alpha {a} under the tint alpha {}", g.tint.a);
    }
}
