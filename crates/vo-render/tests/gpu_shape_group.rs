//! Shape groups (EG-22): members melted by a smooth union, bulges and edge ripples.
//!
//! A group of one member draws the same bytes as the plain shape, two touching circles melt
//! with no seam, goldens for a bulge, a ripple, a glow and a shadow on groups at 1x, 1.5x and
//! in light mode, and the CPU first frame matches the GPU.
//!
//! GPU test: `just test-gpu` runs it. Goldens hold the premultiplied bytes as read back.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::{DisplayList, GroupPrim, Item, ItemKind, Layer, Paint, ShapeInstance};
use vo_scene::geom::{Color, Point, RRect, Rect, Size};

fn hex(rgb: u32) -> Color {
    Color::hex(rgb, 1.0)
}

/// A list of `w` x `h` whose root layer draws the shapes `build` returns, in order.
fn list(w: f32, h: f32, build: impl FnOnce(&mut DisplayList) -> Vec<ShapeInstance>) -> DisplayList {
    let mut list = DisplayList { size: Size::new(w, h), scale: 1.0, ..Default::default() };
    list.layers.push(Layer::root());
    let shapes = build(&mut list);
    list.items.push(Item { kind: ItemKind::Shapes, start: 0, len: shapes.len() as u32 });
    list.shapes = shapes;
    list.layers[0].items = 0..1;
    list
}

fn render(off: &mut Offscreen, name: &str, list: &DisplayList, scale: f32) -> Image {
    let (w, h) = ((list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32);
    let px = off.render_to_rgba(list, [w, h], scale);
    Image { name: name.into(), w, h, channels: 4, px }
}

fn with_fill(s: ShapeInstance, fill: Paint) -> ShapeInstance {
    ShapeInstance { fill, stroke: Paint::NONE, stroke_width: 0.0, flags: 0, ..s }
}

/// Each plain shape kind with a glow, a shadow, a gradient fill and a stroke, as a list of
/// plain shapes or of one-member groups.
fn one_member_scene(grouped: bool) -> DisplayList {
    let blue = hex(0x4cc9f0);
    let plain = [
        (
            ShapeInstance::capsule(Rect::new(16.0, 20.0, 80.0, 24.0), Paint::NONE),
            GroupPrim::capsule(Rect::new(16.0, 20.0, 80.0, 24.0)),
        ),
        (
            ShapeInstance::circle(Point::new(140.0, 32.0), 14.0, Paint::NONE),
            GroupPrim::circle(Point::new(140.0, 32.0), 14.0),
        ),
        (
            ShapeInstance::rrect(
                RRect { rect: Rect::new(20.0, 70.0, 70.0, 40.0), radii: [2.0, 12.0, 6.0, 18.0] },
                Paint::NONE,
            ),
            GroupPrim::rrect(RRect { rect: Rect::new(20.0, 70.0, 70.0, 40.0), radii: [2.0, 12.0, 6.0, 18.0] }),
        ),
    ];
    list(240.0, 124.0, |l| {
        let mut out = Vec::new();
        for (i, (s, prim)) in plain.iter().enumerate() {
            let s = if grouped { ShapeInstance::group(l, &[*prim], 6.0, Paint::NONE) } else { *s };
            let fill = Paint::linear(Point::new(0.0, 20.0), blue, Point::new(0.0, 110.0), hex(0xf72585));
            let halo = if i == 1 {
                Paint::glow(blue, 6.0, 1.0)
            } else {
                Paint::shadow(Color::rgba(0.0, 0.0, 0.0, 0.6), Point::new(2.0, 4.0), 4.0, 1.0)
            };
            out.push(with_fill(s, halo));
            out.push(with_fill(s, fill).with_stroke(Paint::solid(hex(0xffd166)), 2.0));
        }
        // A plain union (k = 0) of one member with a stroke.
        let r = Rect::new(130.0, 70.0, 80.0, 30.0);
        let s = if grouped {
            ShapeInstance::group(l, &[GroupPrim::rrect(RRect::new(r, 6.0))], 0.0, Paint::NONE)
        } else {
            ShapeInstance::rrect(RRect::new(r, 6.0), Paint::NONE)
        };
        out.push(with_fill(s, Paint::solid(hex(0x2a3550))).with_stroke(Paint::solid(hex(0xffffff)), 1.5));
        out
    })
}

#[test]
fn a_group_of_one_draws_the_plain_shape() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    for scale in [1.0, 1.5] {
        let plain = render(&mut off, "plain", &one_member_scene(false), scale);
        let group = render(&mut off, "group", &one_member_scene(true), scale);
        let diff = plain.px.iter().zip(&group.px).filter(|(a, b)| a != b).count();
        assert_eq!(diff, 0, "at {scale}x {diff} bytes differ");
    }
}

/// Two circles of radius 20 that touch at (60, 40), as a group with union radius `k`.
fn touching(k: f32) -> DisplayList {
    list(120.0, 80.0, |l| {
        let prims = [GroupPrim::circle(Point::new(40.0, 40.0), 20.0), GroupPrim::circle(Point::new(80.0, 40.0), 20.0)];
        vec![ShapeInstance::group(l, &prims, k, Paint::solid(hex(0xffffff)))]
    })
}

#[test]
fn touching_circles_melt_with_no_seam() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let alpha = |img: &Image, x: u32, y: u32| img.px[((y * img.w + x) * 4 + 3) as usize];
    let melted = render(&mut off, "melted", &touching(10.0), 1.0);
    // Every pixel along the joint is fully covered: the neck is at least 2.5 units each side.
    for y in 35..=44 {
        for x in [59, 60] {
            assert_eq!(alpha(&melted, x, y), 255, "joint pixel ({x}, {y})");
        }
    }
    // A plain union pinches there: 4.5 units above and below the contact point the pixels
    // either side of it are only half covered.
    let plain = render(&mut off, "plain", &touching(0.0), 1.0);
    for (x, y) in [(59, 35), (60, 44)] {
        assert!(alpha(&plain, x, y) < 160, "plain union at ({x}, {y}): {}", alpha(&plain, x, y));
    }
}

/// The 03 capsule bulging around a word: the pill, a bulge at its middle, a smaller one to the
/// right, with fill, stroke, glow and a rippling panel edge below.
fn bulge_scene() -> DisplayList {
    let blue = hex(0x4cc9f0);
    list(240.0, 124.0, |l| {
        let pill = Rect::new(20.0, 22.0, 200.0, 26.0);
        let prims = [
            GroupPrim::capsule(pill),
            GroupPrim::bulge(pill, 100.0, 56.0, 40.0),
            GroupPrim::bulge(pill, 170.0, 30.0, 32.0),
        ];
        let g = ShapeInstance::group(l, &prims, 12.0, Paint::NONE);
        let panel = [
            GroupPrim::rrect(RRect::new(Rect::new(20.0, 70.0, 200.0, 40.0), 10.0)).with_ripple(2.0, 24.0, 0.1),
            GroupPrim::circle(Point::new(70.0, 70.0), 9.0),
            GroupPrim::circle(Point::new(150.0, 112.0), 7.0),
        ];
        let p = ShapeInstance::group(l, &panel, 8.0, Paint::NONE);
        vec![
            with_fill(g, Paint::glow(blue, 8.0, 1.0)),
            with_fill(g, Paint::solid(hex(0x10203a))).with_stroke(Paint::solid(blue), 2.0),
            with_fill(p, Paint::shadow(Color::rgba(0.0, 0.0, 0.0, 0.6), Point::new(0.0, 4.0), 5.0, 0.0)),
            with_fill(p, Paint::linear(Point::new(0.0, 60.0), hex(0x3a4a6a), Point::new(0.0, 120.0), hex(0x1a2235)))
                .with_hairline(Paint::solid(hex(0xc8d3e0))),
        ]
    })
}

/// The same groups over a light card, for light mode.
fn light_scene() -> DisplayList {
    let mut l = bulge_scene();
    let bg = ShapeInstance::rrect(RRect::new(Rect::new(0.0, 0.0, 240.0, 124.0), 0.0), Paint::solid(hex(0xf3f4f6)));
    l.shapes.insert(0, bg);
    l.items[0].len += 1;
    l
}

#[test]
fn shape_group_goldens() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let scene = bulge_scene();
    let mut images =
        vec![render(&mut off, "group-bulge-1x", &scene, 1.0), render(&mut off, "group-bulge-1.5x", &scene, 1.5)];
    off.renderer.set_light(true);
    images.push(render(&mut off, "group-bulge-light-1x", &light_scene(), 1.0));
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
    // The bulge swells the pill: covered 12 units above the pill's top at the bulge, not
    // between the bulges.
    let img = &images[0];
    let alpha = |x: u32, y: u32| img.px[((y * img.w + x) * 4 + 3) as usize];
    assert_eq!(alpha(100, 15), 255);
    assert!(alpha(135, 15) < 255);
}

/// The ripple moves the panel's edge by at most 2 physical pixels at any scale.
#[test]
fn ripple_is_capped_at_two_pixels() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let edge = |amp: f32, scale: f32, off: &mut Offscreen| {
        let l = list(120.0, 60.0, |l| {
            // Only the top edge is in view: the sides and the bottom are past the image.
            let r = GroupPrim::rrect(RRect::new(Rect::new(-20.0, 20.0, 160.0, 80.0), 0.0)).with_ripple(amp, 40.0, 0.0);
            vec![ShapeInstance::group(l, &[r], 0.0, Paint::solid(hex(0xffffff)))]
        });
        let img = render(off, "ripple", &l, scale);
        // The top edge's height in pixels per column, from coverage.
        (0..img.w)
            .map(|x| (0..img.h).map(|y| img.px[((y * img.w + x) * 4 + 3) as usize] as f32 / 255.0).sum::<f32>())
            .collect::<Vec<_>>()
    };
    for scale in [1.0, 1.5] {
        let flat = edge(0.0, scale, &mut off);
        let wild = edge(10.0, scale, &mut off);
        // A ripple asked for 10 units is the one at the cap, 2 physical pixels.
        assert_eq!(wild, edge(2.0 / scale, scale, &mut off), "at {scale}x");
        // The edge moves by less than the cap, since the ripple fades away from the edge.
        let most = flat.iter().zip(&wild).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(most > 1.0 && most <= 2.0, "at {scale}x the edge moved by up to {most} px");
    }
}

/// The first frame's CPU renderer draws groups as the shader does: within 3 of the GPU for
/// melted and rippled groups, and a group of one draws the same bytes as the plain shape on
/// the CPU too. (The plain one-member scene itself is 5 off on the GPU, from its offset
/// shadows, before groups come into it.)
#[cfg(feature = "cpu")]
#[test]
fn groups_on_the_cpu_match_the_gpu() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let cpu_draw = |list: &DisplayList, w: u32, h: u32| {
        let mut cpu = vo_render::cpu::CpuRenderer::new(vo_scene::font::FontRegistry::new());
        let mut got = vec![0u8; (w * h * 4) as usize];
        cpu.draw_bgra(list, 1.0, w, h, w as usize * 4, &mut got);
        for px in got.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        got
    };
    for (name, list) in [("bulge", bulge_scene()), ("touching", touching(10.0))] {
        let img = render(&mut off, name, &list, 1.0);
        let got = cpu_draw(&list, img.w, img.h);
        let max = img.px.iter().zip(&got).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(max <= 3, "{name}: CPU differs from GPU by {max}");
    }
    let (plain, one) = (cpu_draw(&one_member_scene(false), 240, 124), cpu_draw(&one_member_scene(true), 240, 124));
    assert!(plain == one, "a group of one differs from its plain shape on the CPU");
}
