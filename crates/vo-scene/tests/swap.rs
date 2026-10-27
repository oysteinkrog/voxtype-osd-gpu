//! Word swap (T22, T25, L02): shared words keep their node and slide, dropped words leave
//! upward and give up their space at once, new words rise in, and a badge after the row
//! slides with it.

use vo_core::spring::Spring;
use vo_scene::element::{Element, TextStyle};
use vo_scene::geom::Color;
use vo_scene::swap::{word_swap, Swap};
use vo_scene::{NodeId, Presence, Prop, Scene};

const STYLE: TextStyle = TextStyle::new(0, 20.0, 24.0);

fn view(text: &str) -> Element<()> {
    Element::row("root")
        .gap(12.0)
        .child(word_swap("s", text, STYLE, Color::WHITE, Swap::new(&STYLE)))
        .child(Element::panel("badge").size(40.0, 20.0).motion(Prop::Position, Spring::critical_to_98(0.2)))
}

fn word(scene: &Scene, w: &str) -> NodeId {
    scene.find(&["root", "s", &format!("s-{w}")]).unwrap_or_else(|| panic!("no word {w}"))
}

fn x(scene: &Scene, id: NodeId, t: f64) -> f32 {
    scene.sampled(id, t).expect("sampled").transform.e
}

fn y(scene: &Scene, id: NodeId, t: f64) -> f32 {
    scene.sampled(id, t).expect("sampled").transform.f
}

#[test]
fn a_dropped_word_leaves_and_the_rest_slide_into_its_place() {
    let mut scene = Scene::new();
    scene.apply(view("LIVE TRANSCRIPT"), 0.0);
    let (live, transcript) = (word(&scene, "LIVE-1"), word(&scene, "TRANSCRIPT-1"));
    let badge = scene.find(&["root", "badge"]).unwrap();
    let (x_live, x_tr, x_badge, y0) =
        (x(&scene, live, 2.0), x(&scene, transcript, 2.0), x(&scene, badge, 2.0), y(&scene, live, 2.0));
    assert!(x_tr > x_live + 10.0);

    scene.apply(view("TRANSCRIPT"), 3.0);
    assert_eq!(word(&scene, "TRANSCRIPT-1"), transcript, "a shared word keeps its node");
    // It starts where it was and slides left to where LIVE was.
    assert!((x(&scene, transcript, 3.0) - x_tr).abs() < 0.01);
    let mid = x(&scene, transcript, 3.06);
    assert!(mid < x_tr - 1.0 && mid > x_live, "TRANSCRIPT at {mid}");
    assert!((x(&scene, transcript, 4.0) - x_live).abs() < 0.01);
    // LIVE leaves upward and fades out.
    assert!(matches!(scene.presence(live, 3.05), Presence::Leaving { .. }));
    assert!(y(&scene, live, 3.06) < y0 - 1.0);
    assert!(scene.sampled(live, 3.06).unwrap().opacity < 0.9);
    // The badge slides left with the row.
    let b = x(&scene, badge, 3.06);
    assert!(b < x_badge - 1.0 && b > x(&scene, badge, 4.0), "badge at {b}");
    assert!(x(&scene, badge, 4.0) < x_badge - 20.0);
}

#[test]
fn shared_tokens_stay_and_new_words_rise_in() {
    let mut scene = Scene::new();
    scene.apply(view("Finishing · 4 s left"), 0.0);
    let dot = word(&scene, "·-1");
    let y_dot = y(&scene, dot, 2.0);
    scene.apply(view("Done · Esc to dismiss"), 3.0);
    assert_eq!(word(&scene, "·-1"), dot);
    assert!((y(&scene, dot, 3.05) - y_dot).abs() < 0.01, "the shared dot does not move up or down");
    let esc = word(&scene, "Esc-1");
    assert!(y(&scene, esc, 3.0) > y_dot + 5.0, "a new word starts below");
    assert!((y(&scene, esc, 4.0) - y_dot).abs() < 0.01);
    assert!(scene.sampled(esc, 3.0).unwrap().opacity < 0.01);
    // A repeated word gets a key per copy.
    scene.apply(view("to to"), 5.0);
    assert!(scene.find(&["root", "s", "s-to-2"]).is_some());
}
