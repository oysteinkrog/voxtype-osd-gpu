//! Ambient loops and ripples in a scene: they ask for frames only while they run or settle,
//! they settle to their rest when the view drops them, and they sample the same at any frame
//! rate.

use std::f64::consts::TAU;

use vo_scene::display::ShapeKind;
use vo_scene::element::Element;
use vo_scene::geom::Color;
use vo_scene::{Loop, LoopProp, Ripples, Scene};

type El = Element<()>;

fn ring(spin: bool) -> El {
    let mut dial = Element::shape("dial", ShapeKind::Circle).size(40.0, 40.0).fill(Color::WHITE);
    if spin {
        dial = dial.ambient(LoopProp::Rotation, Loop::spin(1.0));
    }
    Element::column("root").size(100.0, 100.0).child(dial)
}

fn angle(scene: &Scene, t: f64) -> f64 {
    let s = scene.sampled(scene.find(&["root", "dial"]).unwrap(), t).unwrap();
    (s.transform.b as f64).atan2(s.transform.a as f64)
}

/// Follows `next_wake` from `t` like a host would, up to `until`. Returns the frame times.
fn frames(scene: &Scene, mut t: f64, until: f64) -> Vec<f64> {
    let mut out = Vec::new();
    while let Some(w) = scene.next_wake(t) {
        let w = if w <= t { t + 1.0 / 240.0 } else { w };
        if w > until {
            break;
        }
        out.push(w);
        t = w;
    }
    out
}

#[test]
fn spin_runs_at_30_fps_then_coasts_to_a_whole_turn_and_sleeps() {
    let mut scene = Scene::new();
    scene.apply(ring(true), 0.0);
    // Running: one wake per 30 fps step.
    let running = frames(&scene, 0.0, 1.0);
    assert_eq!(running.len(), 30, "{running:?}");
    assert!((angle(&scene, 0.25) - 0.25 * TAU).abs() < 0.03 * TAU);
    // The view drops the loop at 1.31 s: the dial coasts on and stops on a whole turn.
    scene.apply(ring(false), 1.31);
    let settling = frames(&scene, 1.31, 10.0);
    let end = *settling.last().expect("frames while it coasts");
    assert!(end < 1.31 + 2.0 * 1.0, "coasts for at most two laps' time: {end}");
    assert_eq!(scene.next_wake(end + 0.01), None, "idle after the coast");
    assert!(angle(&scene, end + 0.01).abs() < 1e-4, "rests on a whole turn: {}", angle(&scene, end + 0.01));
    // No jump where it stopped (1.31 s is inside a 30 fps step, so only the coast moves it).
    let (a, b) = (angle(&scene, 1.31 - 1e-4), angle(&scene, 1.31 + 1e-4));
    assert!((a - b).abs() < 1e-3, "{a} {b}");
}

#[test]
fn same_events_same_frames_at_any_rate() {
    let events = [(0.0, true), (0.73, false), (1.41, true), (2.6, false)];
    let run = |hz: f64| {
        let mut scene = Scene::new();
        let mut next = 0;
        for f in 0..=(5.0 * hz) as u64 {
            let t = f as f64 / hz;
            while next < events.len() && events[next].0 <= t {
                scene.apply(ring(events[next].1), events[next].0);
                next += 1;
            }
        }
        scene
    };
    let (a, b, c) = (run(144.0), run(60.0), run(30.0));
    for i in 0..500 {
        let t = i as f64 * 0.01;
        let l = a.sample(t);
        assert_eq!(l, b.sample(t), "t {t}");
        assert_eq!(l, c.sample(t), "t {t}");
    }
}

#[test]
fn a_leaving_node_settles_its_loop_and_goes() {
    let spinning = Element::column("root").size(100.0, 100.0).child(
        Element::shape("dial", ShapeKind::Circle)
            .size(40.0, 40.0)
            .fill(Color::WHITE)
            .presence(vo_scene::Transition::fade(), vo_scene::Transition::fade())
            .ambient(LoopProp::Scale, Loop::pulse(0.1, 0.8)),
    );
    let mut scene = Scene::new();
    scene.apply(spinning, 0.0);
    assert!(scene.next_wake(5.0).is_some(), "runs while present");
    scene.apply(Element::column("root").size(100.0, 100.0), 5.0);
    let end = *frames(&scene, 5.0, 20.0).last().unwrap();
    assert!(end < 6.0, "{end}");
    assert_eq!(scene.next_wake(end + 0.01), None);
    // Drawing a frame after the exit removes it on the next apply.
    scene.apply(Element::column("root").size(100.0, 100.0), end + 0.1);
    assert!(scene.find(&["root", "dial"]).is_none());
}

fn dots(on: bool) -> El {
    Element::column("root").child(dot_row(on))
}

fn dot_row(on: bool) -> El {
    let mut row = Element::row("dots").gap(4.0);
    for i in 0..3 {
        let mut d = Element::shape(format!("d{i}"), ShapeKind::Circle).size(6.0, 6.0).fill(Color::WHITE).opacity(0.0);
        if on {
            d = d.ambient(LoopProp::Opacity, Loop::step_on(i, 3, 1.2));
        }
        row = row.child(d);
    }
    row
}

fn lit(scene: &Scene, t: f64) -> Vec<f32> {
    (0..3)
        .map(|i| scene.sampled(scene.find(&["root", "dots", &format!("d{i}")]).unwrap(), t).unwrap().opacity)
        .collect()
}

#[test]
fn listening_dots_light_one_at_a_time_and_end_all_lit() {
    let mut scene = Scene::new();
    scene.apply(dots(true), 0.0);
    assert_eq!(lit(&scene, 0.1), [0.0, 0.0, 0.0]);
    assert_eq!(lit(&scene, 0.4), [1.0, 0.0, 0.0]);
    assert_eq!(lit(&scene, 0.7), [1.0, 1.0, 0.0]);
    assert_eq!(lit(&scene, 1.0), [1.0, 1.0, 1.0]);
    assert_eq!(lit(&scene, 1.25), [0.0, 0.0, 0.0], "a new cycle");
    // Stopped part way through the second cycle: plays it out, ends with all lit.
    scene.apply(dots(false), 1.5);
    assert_eq!(lit(&scene, 1.6), [1.0, 0.0, 0.0]);
    assert_eq!(lit(&scene, 2.39), [1.0, 1.0, 1.0]);
    assert_eq!(lit(&scene, 3.0), [1.0, 1.0, 1.0]);
    let end = *frames(&scene, 1.5, 10.0).last().unwrap();
    assert!((end - 2.4).abs() < 1e-9, "{end}");
}

#[test]
fn reduced_motion_holds_loops_at_rest_and_draws_no_ripples() {
    let mut scene = Scene::new().with_reduced_motion(true);
    let view = Element::column("root")
        .size(200.0, 200.0)
        .child(dot_row(true))
        .child(Element::ripples("rip", Ripples::new(Color::WHITE)).size(100.0, 100.0));
    scene.apply(view, 0.0);
    assert_eq!(scene.next_wake(0.5), None);
    assert_eq!(lit(&scene, 0.1), [1.0, 1.0, 1.0]);
    assert!(scene.sample(0.5).shapes.iter().all(|s| s.stroke.c0.a == 0.0), "no rings");
}

fn rip(active: bool) -> El {
    Element::column("root").size(200.0, 200.0).child(
        Element::ripples("rip", Ripples::new(Color::WHITE).radii(10.0, 90.0).timing(0.6, 1.5).active(active))
            .size(200.0, 200.0),
    )
}

fn rings(scene: &Scene, t: f64) -> usize {
    scene.sample(t).shapes.iter().filter(|s| s.stroke.c0.a > 0.0).count()
}

#[test]
fn ripples_repeat_while_active_and_finish_when_off() {
    let mut scene = Scene::new();
    scene.apply(rip(true), 0.0);
    // Births at 0, 0.6 and 1.2 s; all three alive at 1.3 s.
    assert_eq!(rings(&scene, 1.3), 3);
    assert_eq!(scene.next_wake(1.3).map(|w| (w * 30.0).round()), Some(40.0));
    scene.apply(rip(false), 2.0);
    // Births at 1.8 is the last; it lives until 3.3 s.
    assert!(rings(&scene, 3.2) >= 1);
    assert_eq!(rings(&scene, 3.35), 0);
    let end = *frames(&scene, 2.0, 10.0).last().unwrap();
    assert!(end <= 3.34, "{end}");
    assert_eq!(scene.next_wake(3.34), None);
    // On again: a ring starts at once.
    scene.apply(rip(true), 4.0);
    assert_eq!(rings(&scene, 4.0), 1);
}
