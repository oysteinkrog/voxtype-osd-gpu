//! Materials through elements (FX15): a material reaches the element's own shapes, glyphs
//! and strokes and its children's, a child's own material wins, glows and shadows stay
//! plain, materials that flicker or scroll pace the scene's frames, and phosphor sets a
//! trail whose channels fade at their own rates.

use vo_scene::display::{DisplayList, ShapeKind};
use vo_scene::element::{Element, TextStyle};
use vo_scene::geom::{Color, Size};
use vo_scene::material::{func, Material};
use vo_scene::Scene;

fn holo() -> Material {
    Material::hologram(Color::hex(0x40e0ff, 0.7), 3.0).into()
}

fn sample(view: Element<()>, t: f64, reduced: bool) -> (Scene<()>, DisplayList) {
    let mut scene: Scene<()> = Scene::new().with_reduced_motion(reduced);
    scene.set_viewport(0.0, Size::new(400.0, 200.0), 1.0);
    scene.apply(view, 0.0);
    let list = scene.sample(t);
    (scene, list)
}

fn panel(child_material: Option<Material>) -> Element<()> {
    let mut label = Element::label("text", "neon").text_style(TextStyle::new(0, 20.0, 26.0)).color(Color::WHITE);
    if let Some(m) = child_material {
        label = label.material(m);
    }
    Element::column("root")
        .material(holo())
        .child(Element::shape("box", ShapeKind::RRect).size(120.0, 40.0).fill(Color::hex(0x204060, 1.0)).shadow(
            Color::BLACK,
            0.0,
            4.0,
            8.0,
        ))
        .child(label)
}

#[test]
fn a_container_material_reaches_its_children() {
    let (_, list) = sample(panel(None), 0.5, false);
    assert_eq!(list.materials.len(), 1);
    assert_eq!(list.materials[0].func, func::HOLOGRAM);
    assert!((list.materials[0].time - 0.5).abs() < 1e-6);
    let filled: Vec<_> = list.shapes.iter().filter(|s| !s.fill.is_halo()).collect();
    assert!(!filled.is_empty() && filled.iter().all(|s| s.material() == 1));
    // The shadow stays a plain shadow.
    assert!(list.shapes.iter().filter(|s| s.fill.is_halo()).all(|s| s.material() == 0));
    assert_eq!(list.glyph_fx.len(), list.glyphs.len());
    assert!(list.glyph_fx.iter().enumerate().all(|(i, (g, fx))| *g == i as u32 && fx.material == 1));
}

#[test]
fn a_childs_own_material_wins() {
    let chrome: Material = Material::chrome_classic().into();
    let (_, list) = sample(panel(Some(chrome)), 0.5, false);
    assert_eq!(list.materials.len(), 2);
    let id = |f: u32| list.materials.iter().position(|m| m.func == f).unwrap() as u32 + 1;
    assert!(list.glyph_fx.iter().all(|(_, fx)| fx.material == id(func::CHROME)));
    assert!(list.shapes.iter().filter(|s| !s.fill.is_halo()).all(|s| s.material() == id(func::HOLOGRAM)));
}

#[test]
fn a_hologram_wakes_at_30_fps_and_reduced_motion_holds_it_still() {
    let (scene, _) = sample(panel(None), 0.5, false);
    let wake = scene.next_wake(2.0).expect("a scrolling hologram moves");
    assert!((wake - (2.0 + 1.0 / 30.0)).abs() < 1e-9, "{wake}");
    let (still, a) = sample(panel(None), 0.5, true);
    assert_eq!(still.next_wake(2.0), None);
    assert_eq!(a.materials[0].time, still.sample(7.0).materials[0].time);
}

#[test]
fn chrome_and_gradients_are_still() {
    let view = Element::column("root")
        .material(Material::chrome_classic())
        .child(Element::label("text", "chrome").text_style(TextStyle::new(0, 20.0, 26.0)));
    let (scene, _) = sample(view, 0.5, false);
    assert_eq!(scene.next_wake(2.0), None);
}

#[test]
fn phosphor_keeps_its_own_colour_longest() {
    let view = Element::column("root").child(
        Element::layer("scope")
            .phosphor(0.2, Color::hex(0x33ff66, 1.0))
            .child(Element::shape("dot", ShapeKind::Circle).size(10.0, 10.0).fill(Color::WHITE)),
    );
    let (_, list) = sample(view, 0.5, false);
    let trail = list.layers.iter().find_map(|l| l.trail).expect("a phosphor layer has a trail");
    assert_eq!(trail.half_life, 0.2);
    assert_eq!(trail.decay[1], 1.0);
    assert!(trail.decay[0] < 0.35 && trail.decay[2] < 0.45, "{:?}", trail.decay);
    // A plain trail fades evenly.
    let plain = Element::column("root").child(Element::layer("l").trail(0.2).child(Element::column("c")));
    let (_, list) = sample(plain, 0.5, false);
    assert_eq!(list.layers.iter().find_map(|l| l.trail).unwrap().decay, [1.0; 3]);
}
