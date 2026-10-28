//! Glass panels (FX06, FX08) through the renderer's display list pass: dark and light glass
//! over coloured bars, during the sheen and with the orbiting rim. Goldens at 1x and 2x and a
//! determinism check. Shaped glass (EG-06): a chamfered octagon and a card tilted about y,
//! with goldens at 1x and 1.25x and a readback check that the octagon stays in its outline.
//!
//! GPU test: `just test-gpu` runs it. Goldens hold the premultiplied bytes as read back.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::{DisplayList, Item, ItemKind, Layer, Paint, ShapeInstance};
use vo_scene::geom::{Affine, Color, Point, RRect, Rect, Size};
use vo_scene::glass::{Glass, GlassInstance, GlassShape};
use vo_scene::stage::Stage;

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
    bars(&mut list);
    let accent = hex(0x4c9bff);
    for (i, g) in [Glass::dark().with_orbit(accent), Glass::light().with_orbit(accent)].into_iter().enumerate() {
        list.glass.push(instance(g, [24.0 + i as f32 * 200.0, 40.0, 172.0, 110.0], 16.0, t));
    }
    list.items.push(Item { kind: ItemKind::Glass, start: 0, len: 2 });
    list.layers[0].items = 0..list.items.len() as u32;
    list
}

/// Coloured bars and thin white lines over the whole list, as one shapes item.
fn bars(list: &mut DisplayList) {
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
}

/// `g` as a display list item in `rect` with corner radius `radius`, `t` seconds after it
/// entered.
fn instance(g: Glass, rect: [f32; 4], radius: f32, t: f64) -> GlassInstance {
    let (sheen, orbit, orbit_strength) = g.motion(0.0, t);
    GlassInstance {
        rect,
        radius,
        glass: g,
        alpha: 1.0,
        age: t as f32,
        sheen,
        orbit,
        orbit_strength,
        to_surface: Affine::IDENTITY,
        surface_alpha: 1.0,
        outline: None,
    }
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

/// The octagon's box and chamfer.
const OCTAGON: [f32; 4] = [24.0, 40.0, 172.0, 110.0];
const CHAMFER: f32 = 12.0;
/// The card turned about y, and how far.
const CARD: [f32; 4] = [224.0, 40.0, 172.0, 110.0];
const TILT_DEG: f32 = 20.0;

/// Shaped glass (EG-06): a dark chamfered octagon with the orbiting rim on the left, and a
/// light rounded card turned 20 degrees about y (its right edge away) on the right, `t`
/// seconds after they entered, over the bars unless `bare`. `shadow` false turns the
/// octagon's drop shadow off.
fn shaped(t: f64, bare: bool, shadow: bool, card: bool) -> DisplayList {
    let mut list = DisplayList { size: Size::new(W, H), scale: 1.0, ..Default::default() };
    list.layers.push(Layer::root());
    if !bare {
        bars(&mut list);
    }
    let accent = hex(0x4c9bff);
    let mut octagon = Glass::dark().with_orbit(accent).with_shape(GlassShape::chamfer(CHAMFER));
    if !shadow {
        octagon.shadow[0] = 0.0;
    }
    list.glass.push(instance(octagon, OCTAGON, 0.0, t));
    list.items.push(Item { kind: ItemKind::Glass, start: 0, len: 1 });
    if card {
        list.items.push(Item { kind: ItemKind::Layer, start: 1, len: 1 });
    }
    list.layers[0].items = 0..list.items.len() as u32;
    if card {
        let [x, y, w, h] = CARD;
        let warp = Stage::default().tilt(Size::new(W, H), Rect::new(x, y, w, h), 0.0, TILT_DEG.to_radians(), 0.0);
        assert!(warp.is_some());
        list.layers.push(Layer { parent: Some(0), warp, ..Layer::root() });
        let start = list.items.len() as u32;
        list.glass.push(instance(Glass::light().with_orbit(accent), CARD, 16.0, t));
        list.items.push(Item { kind: ItemKind::Glass, start: 1, len: 1 });
        list.layers[1].items = start..list.items.len() as u32;
    }
    list
}

#[test]
fn shaped_glass_goldens() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let mut failures = Vec::new();
    for (name, t, scale) in [("glass-shapes-1x", 2.0, 1.0), ("glass-shapes-1.25x", 2.0, 1.25), ("glass-shapes-sheen-1x", 0.25, 1.0)] {
        let img = render(&mut off, name, &shaped(t, false, true, true), scale);
        if let Err(e) = support::check(&img, Tolerance::default()) {
            failures.push(e);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Signed distance from `p` to the octagon, in logical pixels, negative inside.
fn octagon_distance(p: Point) -> f32 {
    let [x, y, w, h] = OCTAGON;
    let pts = GlassShape::chamfer(CHAMFER).points(OCTAGON, 0.0).unwrap();
    assert_eq!(pts.len(), 8);
    let n = pts.len();
    let mut best = f32::INFINITY;
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        let (ex, ey) = (b.x - a.x, b.y - a.y);
        let t = (((p.x - a.x) * ex + (p.y - a.y) * ey) / (ex * ex + ey * ey)).clamp(0.0, 1.0);
        best = best.min((p.x - a.x - ex * t).hypot(p.y - a.y - ey * t));
    }
    // Convex: inside when inside the box and every chamfer's half-plane.
    let (cx, cy) = (x + 0.5 * w, y + 0.5 * h);
    let (u, v) = ((p.x - cx).abs(), (p.y - cy).abs());
    let inside = u <= 0.5 * w && v <= 0.5 * h && u + v <= 0.5 * (w + h) - CHAMFER;
    if inside {
        -best
    } else {
        best
    }
}

/// No pixel of the octagon's glass (alpha above 1/255) lies outside its outline grown by one
/// physical pixel, at 1x and 1.25x; inside, the tint alpha holds. The drop shadow is off: it
/// lies outside on purpose.
#[test]
fn octagon_stays_in_its_outline() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    for scale in [1.0f32, 1.25] {
        let img = render(&mut off, "octagon", &shaped(2.0, true, false, false), scale);
        let mut covered = 0;
        for py in 0..img.h {
            for px in 0..img.w {
                let a = img.px[((py * img.w + px) * 4 + 3) as usize];
                if a <= 1 {
                    continue;
                }
                covered += 1;
                let p = Point::new((px as f32 + 0.5) / scale, (py as f32 + 0.5) / scale);
                let d = octagon_distance(p) * scale;
                assert!(d <= 1.0, "scale {scale}: pixel ({px}, {py}) alpha {a} is {d:.2} px outside the octagon");
            }
        }
        assert!(covered > 15_000, "scale {scale}: only {covered} pixels drawn");
        // The cut corner is empty, the middle holds the tint.
        let at = |x: f32, y: f32| img.px[(((y * scale) as u32 * img.w + (x * scale) as u32) * 4 + 3) as usize];
        assert_eq!(at(OCTAGON[0] + 2.0, OCTAGON[1] + 2.0), 0, "scale {scale}: chamfered corner drawn");
        let mid = at(OCTAGON[0] + 86.0, OCTAGON[1] + 55.0) as f32 / 255.0;
        assert!(mid + 1.0 / 255.0 >= Glass::dark().tint.a, "scale {scale}: middle alpha {mid}");
    }
}

/// The tilted card is drawn through its layer's warp: its right edge, turned away, is
/// shorter than its left edge, and the panel stays inside the warped quad.
#[test]
fn tilted_card_follows_its_warp() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let mut list = shaped(2.0, true, false, true);
    // The card alone, without its shadow.
    list.glass[1].glass.shadow[0] = 0.0;
    list.layers[0].items = 1..2;
    let img = render(&mut off, "card", &list, 1.0);
    let height = |x: u32| (0..img.h).filter(|&y| img.px[((y * img.w + x) * 4 + 3) as usize] > 128).count();
    let cols: Vec<u32> = (0..img.w).filter(|&x| height(x) > 0).collect();
    let (x0, x1) = (*cols.first().unwrap(), *cols.last().unwrap());
 // Measured just inside the rounded corners: the near edge is about 113 px, the far one 107.
    let (left, right) = (height(x0 + 18), height(x1 - 18));
    assert!(left >= right + 4, "left edge {left} px, right edge {right} px: no perspective");
    let [cx, _, cw, _] = CARD;
    assert!((x1 - x0) as f32 + 2.0 < cw, "turned card spans {} px of {cw}", x1 - x0);
    assert!(x0 as f32 >= cx, "card starts at {x0}, its box at {cx}");
}
