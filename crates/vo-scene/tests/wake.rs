//! `next_wake` answers for any moment of a scene's history, not only for "now": a tool that
//! replays a whole event list first and then asks about an earlier moment gets the same
//! answer the live scene gave at that moment.

use vo_scene::display::ShapeKind;
use vo_scene::element::Element;
use vo_scene::geom::Color;
use vo_scene::Scene;

fn dot(opacity: f32) -> Element<()> {
    Element::column("root")
        .size(100.0, 100.0)
        .child(Element::shape("dot", ShapeKind::Circle).size(10.0, 10.0).fill(Color::WHITE).opacity(opacity))
}

#[test]
fn a_replayed_scene_wakes_like_the_live_one() {
    let mut live = Scene::new();
    live.apply(dot(1.0), 0.0);
    live.apply(dot(0.2), 1.0);
    let at_rest = live.next_wake(3.0);

    let mut replayed = Scene::new();
    replayed.apply(dot(1.0), 0.0);
    replayed.apply(dot(0.2), 1.0);
    replayed.apply(dot(0.9), 5.0);
    // Live, the scene was idle at 3 s; replayed, it wakes for the event at 5 s, not now.
    assert_eq!(at_rest, None);
    assert_eq!(replayed.next_wake(3.0), Some(5.0));
    // While a fade runs it wakes at once, in both.
    assert_eq!(live.next_wake(1.05), Some(1.05));
    assert_eq!(replayed.next_wake(1.05), Some(1.05));
    assert_eq!(replayed.next_wake(5.05), Some(5.05));
    // After the last fade has settled, nothing.
    assert_eq!(replayed.next_wake(9.0), None);
}
