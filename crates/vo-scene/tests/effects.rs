//! Glow, shadow and bloom from the element API into the display list.

use vo_scene::display::{LayerTarget, PaintKind, ShapeKind};
use vo_scene::element::Element;
use vo_scene::geom::Color;
use vo_scene::Scene;

type El = Element<()>;

fn root(children: Vec<El>) -> El {
    Element::column("root").children(children)
}

#[test]
fn shadow_and_glow_draw_under_the_shape_in_that_order() {
    let mut scene = Scene::new();
    let pill = Element::shape("pill", ShapeKind::Capsule)
        .size(80.0, 20.0)
        .fill(Color::WHITE)
        .shadow(Color::BLACK, 0.0, 4.0, 6.0)
        .glow(Color::hex(0x60a5fa, 1.0), 8.0, 2.0);
    scene.apply(root(vec![pill]), 0.0);
    // Glow and shadow start from zero and spring to their targets.
    let list = scene.sample(10.0);
    let kinds: Vec<_> = list.shapes.iter().map(|s| s.fill.kind).collect();
    assert_eq!(kinds, [PaintKind::Shadow, PaintKind::Glow, PaintKind::Solid]);
    let glow = &list.shapes[1];
    assert_eq!(glow.kind, ShapeKind::Capsule);
    assert!((glow.fill.p0.x - 8.0).abs() < 1e-3 && (glow.fill.p0.y - 2.0).abs() < 1e-3, "{:?}", glow.fill);
    let shadow = &list.shapes[0];
    assert!((shadow.fill.p0.y - 4.0).abs() < 1e-3 && (shadow.fill.p1.x - 6.0).abs() < 1e-3);
    assert_eq!(shadow.geom, list.shapes[2].geom);
}

#[test]
fn a_glow_at_zero_strength_draws_nothing() {
    let mut scene = Scene::new();
    let pill = Element::shape("pill", ShapeKind::Capsule).size(80.0, 20.0).fill(Color::WHITE).glow(
        Color::WHITE,
        8.0,
        0.0,
    );
    scene.apply(root(vec![pill]), 0.0);
    assert_eq!(scene.sample(10.0).shapes.len(), 1);
}

#[test]
fn bloom_makes_an_offscreen_layer_with_its_values() {
    let mut scene = Scene::new();
    let layer = Element::layer("glow").size(100.0, 40.0).bloom(1.5, 12.0).child(
        Element::shape("dot", ShapeKind::Circle).size(10.0, 10.0).fill(Color::WHITE),
    );
    scene.apply(root(vec![layer]), 0.0);
    let list = scene.sample(10.0);
    let bloom = list.layers.iter().find_map(|l| match l.target {
        LayerTarget::Offscreen { bloom } => bloom,
        LayerTarget::Direct => None,
    });
    let b = bloom.expect("a bloom layer");
    assert!((b.strength - 1.5).abs() < 1e-3 && (b.radius - 12.0).abs() < 1e-3, "{b:?}");
}
