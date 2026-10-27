//! Ambient loops and ripples (FX-203, FX-101, FX-047): one cell with a spinner, a breathing
//! glow, "..." dots, a stripe marquee and ripples. The loops run from 0 s and the view drops
//! them at 2 s. The golden is a contact sheet of the cell at eight times: five while running,
//! then settling, then at rest.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::ShapeKind;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::taffy::prelude::{AlignItems, JustifyContent};
use vo_scene::{Loop, LoopProp, Ripples, Scene};

const W: f32 = 240.0;
const H: f32 = 120.0;
const STOP: f64 = 2.0;
const TIMES: [f64; 8] = [0.0, 0.3, 0.6, 0.9, 1.5, 2.15, 2.5, 4.0];

fn view(on: bool) -> Element<()> {
    let accent = Color::hex(0x7dd3fc, 1.0);
    let ink = Color::hex(0xe6ebf5, 1.0);
    let with = |e: Element<()>, prop: LoopProp, l: Loop| if on { e.ambient(prop, l) } else { e };

    let ripples =
        Element::ripples("rip", Ripples::new(accent).radii(12.0, 54.0).timing(0.5, 1.4).width(2.0).active(on))
            .size(H, H);
    let spinner =
        with(Element::shape("spin", ShapeKind::Capsule).size(44.0, 6.0).fill(ink), LoopProp::Rotation, Loop::spin(1.2));
    let breath = with(
        with(
            Element::shape("breath", ShapeKind::Circle).size(18.0, 18.0).fill(accent).glow(accent, 8.0, 0.4),
            LoopProp::GlowStrength,
            Loop::pulse(1.2, 1.6),
        ),
        LoopProp::Scale,
        Loop::pulse(0.15, 1.6),
    );
    let mut dots = Element::row("dots").gap(5.0);
    for i in 0..3 {
        let d = Element::shape(format!("d{i}"), ShapeKind::Circle).size(7.0, 7.0).fill(ink).opacity(0.0);
        dots = dots.child(with(d, LoopProp::Opacity, Loop::step_on(i, 3, 1.2)));
    }
    let mut stripes = Element::row("bars").gap(6.0);
    for i in 0..12 {
        stripes = stripes.child(Element::shape(format!("s{i}"), ShapeKind::RRect).size(6.0, 10.0).fill(accent));
    }
    let marquee = Element::layer("track").clip().size(96.0, 10.0).child(with(
        stripes,
        LoopProp::OffsetX,
        Loop::marquee(12.0, 0.8),
    ));
    let right = Element::column("right")
        .size(W - H, H)
        .gap(14.0)
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
        .child(Element::row("top").gap(18.0).align_items(AlignItems::CENTER).child(spinner).child(breath))
        .child(dots)
        .child(marquee);
    Element::panel("cell").size(W, H).direction_row().fill(Color::hex(0x111318, 1.0)).child(ripples).child(right)
}

fn scene() -> Scene<()> {
    let mut scene = Scene::new();
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(view(true), 0.0);
    scene.apply(view(false), STOP);
    scene
}

#[test]
fn ambient_golden() {
    let scene = scene();
    // At rest by 4 s: no frames asked for.
    assert_eq!(scene.next_wake(4.0), None);
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let (cols, gap) = (4u32, 6u32);
    let (cw, ch) = (W as u32, H as u32);
    let rows = (TIMES.len() as u32).div_ceil(cols);
    let (sw, sh) = (cols * cw + (cols + 1) * gap, rows * ch + (rows + 1) * gap);
    let mut px = vec![0u8; (sw * sh * 4) as usize];
    for p in px.as_chunks_mut::<4>().0 {
        *p = [40, 42, 48, 255];
    }
    for (i, &t) in TIMES.iter().enumerate() {
        let frame = off.render_to_rgba(&scene.sample(t), [cw, ch], 1.0);
        let (ox, oy) = (gap + (i as u32 % cols) * (cw + gap), gap + (i as u32 / cols) * (ch + gap));
        for y in 0..ch {
            let src = &frame[(y * cw * 4) as usize..((y + 1) * cw * 4) as usize];
            let at = (((oy + y) * sw + ox) * 4) as usize;
            px[at..at + src.len()].copy_from_slice(src);
        }
    }
    let img = Image { name: "ambient-loops".into(), w: sw, h: sh, channels: 4, px };
    support::check(&img, Tolerance::default()).unwrap();
}
