//! `Element::default_motion`: a subtree's own default motion, inherited by its descendants.

use vo_core::spring::Spring;
use vo_scene::element::{Element, Motion};
use vo_scene::Scene;

/// Two cells side by side, each with a box whose opacity is `o`. `snap_cell` sets Snap on the
/// first cell; `inner_spring` gives the first cell's box its own spring default back.
fn wall(o: f32, snap_cell: bool, inner_spring: bool) -> Element<()> {
    let mut a_box = Element::panel("box").size(20.0, 20.0).opacity(o);
    if inner_spring {
        a_box = a_box.default_motion(Spring::SNAP);
    }
    let mut a = Element::panel("a").size(40.0, 40.0).child(a_box);
    if snap_cell {
        a = a.default_motion(Motion::Snap);
    }
    let b = Element::panel("b").size(40.0, 40.0).child(Element::panel("box").size(20.0, 20.0).opacity(o));
    Element::row("root").child(a).child(b)
}

fn opacity(scene: &Scene<()>, path: &[&str], t: f64) -> f32 {
    let id = scene.find(path).expect("node");
    scene.sampled(id, t).expect("drawn").opacity
}

#[test]
fn a_subtree_snaps_while_its_sibling_springs() {
    let mut scene = Scene::new().with_default_motion(Spring::SNAP.into());
    scene.apply(wall(1.0, true, false), 0.0);
    scene.apply(wall(0.2, true, false), 1.0);
    let just_after = 1.0 + 1.0 / 120.0;
    assert_eq!(opacity(&scene, &["root", "a", "box"], just_after), 0.2, "cell a snaps, its child inherits Snap");
    let b = opacity(&scene, &["root", "b", "box"], just_after);
    assert!(b > 0.5, "cell b keeps the scene's spring: {b}");
    assert!((opacity(&scene, &["root", "b", "box"], 4.0) - 0.2).abs() < 1e-3);
}

#[test]
fn a_descendant_default_wins_inside_its_subtree() {
    let mut scene = Scene::new().with_default_motion(Spring::SNAP.into());
    scene.apply(wall(1.0, true, true), 0.0);
    scene.apply(wall(0.2, true, true), 1.0);
    let a = opacity(&scene, &["root", "a", "box"], 1.0 + 1.0 / 120.0);
    assert!(a > 0.5, "the box's own spring beats the cell's Snap: {a}");
}

#[test]
fn a_changed_default_applies_to_the_next_retarget() {
    let mut scene = Scene::new().with_default_motion(Spring::SNAP.into());
    scene.apply(wall(1.0, false, false), 0.0);
    // The cell turns to Snap with the same targets, then the targets change.
    scene.apply(wall(1.0, true, false), 1.0);
    scene.apply(wall(0.2, true, false), 2.0);
    assert_eq!(opacity(&scene, &["root", "a", "box"], 2.0 + 1.0 / 120.0), 0.2);
}
