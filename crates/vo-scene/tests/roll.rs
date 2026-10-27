//! Digit roll: only the changed digit moves, a carry rolls in a new column from below, and
//! each digit sits in a clip with faded edges.

use vo_core::spring::Spring;
use vo_scene::element::{Element, TextStyle};
use vo_scene::geom::Color;
use vo_scene::roll::digit_roll;
use vo_scene::{NodeId, Scene};

const LH: f32 = 24.0;

fn view(text: &str) -> Element<()> {
    let style = TextStyle::new(0, 20.0, LH);
    Element::column("root").child(digit_roll("n", text, style, Color::WHITE, Spring::from_frequency(6.0, 1.0)))
}

/// The column of the digit `place` from the right, with `prefix` the text left of it.
fn column(scene: &Scene, place: usize, prefix: &str) -> NodeId {
    scene.find(&["root", "n", &format!("n-d{place}"), &format!("n-c{place}-{prefix}")]).expect("digit column")
}

fn y(scene: &Scene, id: NodeId, t: f64) -> f32 {
    scene.sampled(id, t).expect("sampled").transform.f
}

#[test]
fn only_the_changed_digit_rolls() {
    let mut scene = Scene::new();
    scene.apply(view("128"), 0.0);
    let (units, tens) = (column(&scene, 0, "12"), column(&scene, 1, "1"));
    let (u0, t0) = (y(&scene, units, 4.0), y(&scene, tens, 4.0));
    scene.apply(view("129"), 5.0);
    assert_eq!(column(&scene, 0, "12"), units, "same column within a decade");
    let mid = y(&scene, units, 5.08);
    assert!(mid < u0 - 1.0 && mid > u0 - LH, "units at {mid}, from {u0}");
    assert!((y(&scene, units, 9.0) - (u0 - LH)).abs() < 0.01);
    assert_eq!(y(&scene, tens, 5.08), t0, "the tens digit stays");
    // Every digit is a clipped layer whose top and bottom edges fade.
    let list = scene.sample(9.0);
    let faded = list.layers.iter().filter(|l| l.clip.is_some() && l.clip_fade[1] > 0.0 && l.clip_fade[3] > 0.0);
    assert_eq!(faded.count(), 3);
}

#[test]
fn a_carry_rolls_a_new_column_in_from_below() {
    let mut scene = Scene::new();
    scene.apply(view("19"), 0.0);
    let old = column(&scene, 0, "1");
    let window = scene.find(&["root", "n", "n-d0"]).unwrap();
    let top = y(&scene, window, 4.0);
    scene.apply(view("20"), 5.0);
    let new = column(&scene, 0, "2");
    assert_ne!(old, new);
    // The new column starts one line below, showing 0, and rises into place.
    assert!((y(&scene, new, 5.0) - (top + LH)).abs() < 0.01);
    assert!((y(&scene, new, 9.0) - top).abs() < 0.01);
    // The old one, showing 9, leaves upward by one more line.
    let leaving = y(&scene, old, 5.08);
    assert!(leaving < top - 9.0 * LH, "old column at {leaving}");
}

#[test]
fn the_shown_digit_sits_in_the_window() {
    for d in 0..10 {
        let mut scene = Scene::new();
        scene.apply(view(&d.to_string()), 0.0);
        let window = scene.find(&["root", "n", "n-d0"]).unwrap();
        let label = scene.find(&["root", "n", "n-d0", "n-c0-", &format!("n-c0--{d}")]).unwrap();
        let (w, l) = (scene.sampled(window, 4.0).unwrap(), scene.sampled(label, 4.0).unwrap());
        assert!(
            (w.transform.f - l.transform.f).abs() < 0.01,
            "digit {d}: window {}, label {}",
            w.transform.f,
            l.transform.f
        );
        assert_eq!((w.size.h, l.size.h), (LH, LH));
    }
}

#[test]
fn a_countdown_borrow_drops_the_new_column_in_from_above() {
    use vo_scene::roll::{digit_roll_with, Roll};
    let down = |text: &str| {
        let style = TextStyle::new(0, 20.0, LH);
        let roll = digit_roll_with("n", text, style, Color::WHITE, Spring::from_frequency(6.0, 1.0), Roll::Down);
        Element::column("root").child(roll)
    };
    let mut scene = Scene::new();
    scene.apply(down("1.0"), 0.0);
    let old = column(&scene, 0, "1.");
    let window = scene.find(&["root", "n", "n-d0"]).unwrap();
    let top = y(&scene, window, 4.0);
    scene.apply(down("0.9"), 5.0);
    let new = column(&scene, 0, "0.");
    // The new column starts one line above 9, so the 9 comes down into the window.
    assert!((y(&scene, new, 5.0) - (top - 10.0 * LH)).abs() < 0.01);
    assert!((y(&scene, new, 9.0) - (top - 9.0 * LH)).abs() < 0.01);
    // The old one, showing 0, leaves downward.
    assert!(y(&scene, old, 5.08) > top + 1.0, "old column at {}", y(&scene, old, 5.08));
    // Within a column a countdown rolls down: 9 to 8 moves the column down by a line.
    scene.apply(down("0.8"), 10.0);
    assert_eq!(column(&scene, 0, "0."), new);
    assert!((y(&scene, new, 14.0) - (top - 8.0 * LH)).abs() < 0.01);
}
