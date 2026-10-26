//! Scene behaviour: reconcile, presence, determinism, settling, layout motion, input.

use vo_core::spring::Spring;
use vo_scene::display::{ItemKind, ShapeKind};
use vo_scene::element::Element;
use vo_scene::geom::{Color, Point, Size};
use vo_scene::input::{Button, PointerEvent, PointerKind};
use vo_scene::{DisplayList, Motion, Presence, Prop, Scene, Transition};

type El = Element<Msg>;

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Click,
    Enter,
    Leave,
}

fn dot(key: &'static str, x: f32) -> El {
    Element::shape(key, ShapeKind::Circle).size(10.0, 10.0).offset(x, 0.0).fill(Color::WHITE)
}

fn root(children: Vec<El>) -> El {
    Element::column("root").children(children)
}

#[test]
fn reconcile_keeps_channel_state_for_the_same_key() {
    let mut scene = Scene::new();
    scene.apply(root(vec![dot("a", 0.0)]), 0.0);
    let id = scene.find(&["root", "a"]).unwrap();
    scene.apply(root(vec![dot("a", 100.0)]), 0.1);
    // The same view again mid-flight: no reset.
    scene.apply(root(vec![dot("a", 100.0)]), 0.15);
    assert_eq!(scene.find(&["root", "a"]), Some(id));

    let mut reference = Scene::<Msg>::new();
    reference.apply(root(vec![dot("a", 0.0)]), 0.0);
    reference.apply(root(vec![dot("a", 100.0)]), 0.1);
    for t in [0.12, 0.15, 0.2, 0.3] {
        assert_eq!(scene.sample(t), reference.sample(t), "t {t}");
    }
    let x = scene.sampled(id, 0.15).unwrap().transform.e;
    assert!(x > 1.0 && x < 99.0, "{x}");
}

#[test]
fn apply_reports_whether_anything_changed() {
    let mut scene = Scene::new();
    assert!(scene.apply(root(vec![dot("a", 0.0)]), 0.0), "first view");
    assert!(!scene.apply(root(vec![dot("a", 0.0)]), 1.0), "same view");
    assert!(scene.apply(root(vec![dot("a", 50.0)]), 2.0), "retarget");
    assert!(!scene.apply(root(vec![dot("a", 50.0)]), 2.1), "same target mid-flight");
    assert!(scene.apply(root(vec![dot("a", 50.0), dot("b", 0.0)]), 5.0), "new node");
    assert!(scene.apply(root(vec![dot("b", 0.0), dot("a", 50.0)]), 6.0), "reorder");
    assert!(scene.apply(root(vec![dot("b", 0.0), dot("a", 50.0).on_click(Msg::Click)]), 7.0), "hit flag");
    assert!(!scene.apply(root(vec![dot("b", 0.0), dot("a", 50.0).on_click(Msg::Click)]), 8.0));
    assert!(scene.apply(root(vec![dot("b", 0.0)]), 9.0), "removed");
    assert!(scene.apply(root(vec![dot("b", 0.0).width(20.0)]), 10.0), "layout");
}

#[test]
fn key_moved_to_another_parent_is_a_new_node() {
    let mut scene = Scene::new();
    let view = |x_in_a: bool| {
        let x = || dot("x", 0.0);
        let a = Element::row("a").height(10.0);
        let b = Element::row("b").height(10.0);
        if x_in_a {
            root(vec![a.child(x()), b])
        } else {
            root(vec![a, b.child(x())])
        }
    };
    scene.apply(view(true), 0.0);
    let old = scene.find(&["root", "a", "x"]).unwrap();
    scene.apply(view(false), 0.5);
    let new = scene.find(&["root", "b", "x"]).unwrap();
    assert_ne!(old, new);
    assert!(!scene.contains(old));
    assert_eq!(scene.find(&["root", "a", "x"]), None);
    assert_eq!(scene.path(new), "root/b/x");
}

#[test]
fn removed_node_animates_out_then_is_dropped() {
    let fading = || dot("toast", 0.0).presence(Transition::fade().offset(0.0, 8.0), Transition::fade());
    let mut scene = Scene::new();
    scene.apply(root(vec![fading()]), 0.0);
    let id = scene.find(&["root", "toast"]).unwrap();
    assert!(matches!(scene.presence(id, 0.01), Presence::Entering { .. }));
    let o = scene.sampled(id, 0.05).unwrap().opacity;
    assert!(o > 0.0 && o < 1.0, "enter opacity {o}");
    assert_eq!(scene.presence(id, 2.0), Presence::Present);
    assert_eq!(scene.next_wake(2.0), None);

    scene.apply(root(vec![]), 2.0);
    assert!(matches!(scene.presence(id, 2.05), Presence::Leaving { .. }));
    let o = scene.sampled(id, 2.05).unwrap().opacity;
    assert!(o > 0.0 && o < 1.0, "exit opacity {o}");
    assert_eq!(scene.sample(2.05).shapes.len(), 1);
    assert!(scene.next_wake(2.05).is_some());

    let Presence::Leaving { until, .. } = scene.presence(id, 2.05) else { unreachable!() };
    assert!(until > 2.05 && until < 3.0, "{until}");
    assert_eq!(scene.sample(until).shapes.len(), 0);
    assert_eq!(scene.next_wake(until), None);
    // Still in the arena until the next apply, which drops it.
    assert!(scene.contains(id));
    scene.apply(root(vec![]), until + 0.1);
    assert!(!scene.contains(id));
    assert_eq!(scene.len(), 1);
}

#[test]
fn a_node_without_presence_goes_at_once() {
    let mut scene = Scene::new();
    scene.apply(root(vec![dot("a", 0.0)]), 0.0);
    let id = scene.find(&["root", "a"]).unwrap();
    scene.apply(root(vec![]), 0.5);
    assert!(!scene.contains(id));
    assert_eq!(scene.sample(0.5).shapes.len(), 0);
}

#[test]
fn a_leaving_node_that_comes_back_keeps_its_id() {
    let fading = || dot("toast", 0.0).presence(Transition::fade(), Transition::fade());
    let mut scene = Scene::new();
    scene.apply(root(vec![fading()]), 0.0);
    let id = scene.find(&["root", "toast"]).unwrap();
    scene.apply(root(vec![]), 1.0);
    scene.apply(root(vec![fading()]), 1.05);
    assert_eq!(scene.find(&["root", "toast"]), Some(id));
    assert_eq!(scene.presence(id, 1.06), Presence::Entering { since: 1.05 });
    let o1 = scene.sampled(id, 1.05).unwrap().opacity;
    assert!(o1 < 1.0 && o1 > 0.0);
    assert_eq!(scene.sampled(id, 3.0).unwrap().opacity, 1.0);
}

/// App state for the determinism script.
#[derive(Clone)]
struct State {
    items: usize,
    title: &'static str,
    shift: f32,
}

fn view(s: &State) -> El {
    let items = (0..s.items as u64).map(|i| {
        Element::panel(i)
            .height(12.0 + i as f32)
            .fill(Color::hex(0x336699, 1.0))
            .radius(4.0)
            .presence(Transition::fade().offset(0.0, -10.0).scale(0.9), Transition::fade().scale(0.8))
    });
    Element::panel("card")
        .pad(8.0)
        .gap(4.0)
        .width(240.0)
        .fill(Color::hex(0x101010, 0.9))
        .offset(s.shift, 0.0)
        .motion(Prop::Offset, Spring::POP)
        .child(Element::label("title", s.title).color(Color::WHITE))
        .children(items)
}

#[test]
fn same_script_same_display_lists_at_144_60_and_30_hz() {
    let script = [
        (0.0, State { items: 1, title: "one", shift: 0.0 }),
        (0.13, State { items: 3, title: "three items now", shift: 20.0 }),
        (0.41, State { items: 2, title: "two", shift: -15.0 }),
        (0.4100001, State { items: 4, title: "a much longer title that wraps", shift: 5.0 }),
        (0.77, State { items: 0, title: "none", shift: 0.0 }),
        (1.2, State { items: 5, title: "five", shift: 30.0 }),
    ];
    let span = 2.5;
    let run = |hz: f64| -> (Scene<Msg>, Vec<(f64, DisplayList)>) {
        let mut scene = Scene::new();
        let mut next = 0;
        let mut frames = Vec::new();
        for frame in 0..=(span * hz) as u64 {
            let t = frame as f64 / hz;
            while next < script.len() && script[next].0 <= t {
                scene.apply(view(&script[next].1), script[next].0);
                next += 1;
            }
            // Keep the frames all three rates share: every 1/6 s.
            if (frame * 6) % hz as u64 == 0 {
                frames.push((t, scene.sample(t)));
            }
        }
        (scene, frames)
    };
    let runs: Vec<_> = [144.0, 60.0, 30.0].into_iter().map(run).collect();
    let (reference, frames) = &runs[0];
    assert_eq!(frames.len(), (span * 6.0) as usize + 1);
    let mut moving = 0;
    for (scene, other) in &runs {
        assert_eq!(other.len(), frames.len());
        for ((t, a), (t2, b)) in frames.iter().zip(other) {
            assert_eq!(t, t2);
            assert_eq!(a, b, "t {t}");
            // A seek on a finished scene gives the frame the live run drew, from the last
            // event on. Earlier, nodes that left and were dropped are gone.
            if *t >= 1.2 {
                assert_eq!(&scene.sample(*t), a, "seek t {t}");
                assert_eq!(&reference.sample(*t), a, "cross seek t {t}");
            }
        }
    }
    for w in frames.windows(2) {
        moving += (w[0].1 != w[1].1) as u32;
    }
    assert!(moving > 5, "the script should move things: {moving}");
}

#[test]
fn next_wake_is_none_once_settled() {
    let mut scene = Scene::new();
    scene.apply(root(vec![dot("a", 0.0)]), 0.0);
    assert_eq!(scene.next_wake(0.0), None, "a first view starts at rest");
    scene.apply(root(vec![dot("a", 50.0)]), 1.0);
    assert_eq!(scene.next_wake(1.0), Some(1.0));
    assert_eq!(scene.next_wake(1.1), Some(1.1));
    assert_eq!(scene.next_wake(3.0), None);
    let settled = scene.sample(3.0);
    assert_eq!(scene.sample(10.0), settled);
    // Snap motion never wakes.
    scene.apply(root(vec![dot("a", 0.0).motion(Prop::Offset, Motion::Snap)]), 4.0);
    assert_eq!(scene.next_wake(4.0), None);
}

#[test]
fn layout_change_retargets_springs_instead_of_jumping() {
    let bar = |key: &'static str, h: f32| Element::<Msg>::panel(key).height(h).fill(Color::WHITE);
    let col = |bars: Vec<El>| Element::column("root").width(100.0).gap(5.0).children(bars);
    let mut scene = Scene::new();
    scene.apply(col(vec![bar("a", 20.0), bar("b", 20.0)]), 0.0);
    let b = scene.find(&["root", "b"]).unwrap();
    assert_eq!(scene.layout_target(b).y, 25.0);
    assert_eq!(scene.sampled(b, 0.0).unwrap().transform.f, 25.0);

    scene.apply(col(vec![bar("a", 20.0), bar("new", 30.0), bar("b", 20.0)]), 1.0);
    assert_eq!(scene.find(&["root", "b"]), Some(b));
    assert_eq!(scene.layout_target(b).y, 60.0);
    // The new node starts at its box; b moves there on a spring.
    let new = scene.find(&["root", "new"]).unwrap();
    assert_eq!(scene.sampled(new, 1.0).unwrap().transform.f, 25.0);
    assert_eq!(scene.sampled(b, 1.0).unwrap().transform.f, 25.0);
    let mid = scene.sampled(b, 1.05).unwrap().transform.f;
    assert!(mid > 25.0 && mid < 60.0, "{mid}");
    assert_eq!(scene.sampled(b, 3.0).unwrap().transform.f, 60.0);

    // A size change of the root also animates.
    scene.apply(col(vec![bar("a", 20.0), bar("new", 30.0), bar("b", 20.0)]).width(200.0), 4.0);
    let root = scene.root().unwrap();
    let w = scene.sampled(root, 4.05).unwrap().size.w;
    assert!(w > 100.0 && w < 200.0, "{w}");
}

#[test]
fn label_is_a_measure_leaf_and_wraps() {
    let mut scene = Scene::<Msg>::new();
    let view = |w: f32| Element::row("root").width(w).child(Element::label("l", "hello there world"));
    scene.apply(view(1000.0), 0.0);
    let l = scene.find(&["root", "l"]).unwrap();
    let one = scene.layout_target(l);
    assert_eq!(one.h, 18.0);
    let glyphs = scene.sample(0.0).glyphs.len();
    assert_eq!(glyphs, 15);
    scene.apply(view(60.0), 1.0);
    let wrapped = scene.layout_target(l);
    assert!(wrapped.h >= 36.0, "{wrapped:?}");
    assert!(wrapped.w <= 60.0, "{wrapped:?}");
}

#[test]
fn hit_test_follows_a_moving_spring() {
    let button =
        |x: f32| Element::<Msg>::shape("button", ShapeKind::RRect).size(40.0, 20.0).offset(x, 0.0).on_click(Msg::Click);
    let mut scene = Scene::new();
    scene.apply(root(vec![button(0.0)]), 0.0);
    scene.apply(root(vec![button(200.0)]), 1.0);
    let id = scene.find(&["root", "button"]).unwrap();
    for t in [1.02, 1.05, 1.1, 1.2] {
        let x = scene.sampled(id, t).unwrap().transform.e;
        assert!(x > 0.0 && x < 200.0, "{x}");
        assert_eq!(scene.hit_test(Point::new(x + 2.0, 10.0), t), Some(id), "t {t}");
        assert_eq!(scene.hit_test(Point::new(x + 38.0, 10.0), t), Some(id), "t {t}");
        assert_eq!(scene.hit_test(Point::new(x - 2.0, 10.0), t), None, "t {t}");
        assert_eq!(scene.hit_test(Point::new(x + 42.0, 10.0), t), None, "t {t}");
        // The region handed to the host follows the same box.
        let region = scene.sample(t).input_region();
        assert_eq!(region.rects.len(), 1);
        assert_eq!(region.rects[0].x, x.floor() as i32);
    }
    // At the final place once settled.
    assert_eq!(scene.hit_test(Point::new(210.0, 10.0), 5.0), Some(id));
    assert_eq!(scene.hit_test(Point::new(10.0, 10.0), 5.0), None);
}

#[test]
fn hit_test_skips_leaving_and_rotated_boxes_are_exact() {
    let button = Element::<Msg>::shape("b", ShapeKind::RRect)
        .size(40.0, 20.0)
        .rotate(std::f32::consts::FRAC_PI_2)
        .on_click(Msg::Click)
        .presence(Transition::NONE, Transition::fade());
    let mut scene = Scene::new();
    scene.apply(root(vec![button]), 0.0);
    let id = scene.find(&["root", "b"]).unwrap();
    // Rotated a quarter turn about its centre (20, 10): now 20 wide and 40 tall.
    assert_eq!(scene.hit_test(Point::new(20.0, -5.0), 0.0), Some(id));
    assert_eq!(scene.hit_test(Point::new(2.0, 10.0), 0.0), None);
    let list = scene.sample(0.0);
    assert_eq!(list.layers.len(), 2, "the rotation opens a layer");
    assert_eq!(list.layer_items(0)[0].kind, ItemKind::Layer);
    scene.apply(root(vec![]), 1.0);
    assert!(scene.sampled(id, 1.01).is_some());
    assert_eq!(scene.hit_test(Point::new(20.0, 10.0), 1.01), None);
    assert!(scene.sample(1.01).input_region().rects.is_empty());
}

#[test]
fn pointer_events_become_messages() {
    let mut scene = Scene::new();
    scene.set_viewport(0.0, Size::new(300.0, 100.0), 1.0);
    let view = root(vec![Element::shape("b", ShapeKind::Capsule)
        .size(40.0, 20.0)
        .on_click(Msg::Click)
        .on_hover(Msg::Enter, Msg::Leave)]);
    scene.apply(view, 0.0);
    let ev = |t: f64, x: f32, kind| PointerEvent { surface: 0, t, pos: Point::new(x, 10.0), kind };
    assert_eq!(scene.pointer(&ev(0.1, 200.0, PointerKind::Enter)), vec![]);
    assert_eq!(scene.pointer(&ev(0.2, 20.0, PointerKind::Motion)), vec![Msg::Enter]);
    assert_eq!(scene.pointer(&ev(0.3, 20.0, PointerKind::Down(Button::Primary))), vec![]);
    assert_eq!(scene.pointer(&ev(0.4, 21.0, PointerKind::Up(Button::Primary))), vec![Msg::Click]);
    // Press on the button, release outside: no click.
    scene.pointer(&ev(0.5, 20.0, PointerKind::Down(Button::Primary)));
    assert_eq!(scene.pointer(&ev(0.6, 200.0, PointerKind::Up(Button::Primary))), vec![Msg::Leave]);
    assert_eq!(scene.pointer(&ev(0.7, 200.0, PointerKind::Leave)), vec![]);
}

#[test]
#[should_panic(expected = "duplicate key 'a'")]
#[cfg(debug_assertions)]
fn duplicate_sibling_keys_panic_in_debug() {
    let mut scene = Scene::<Msg>::new();
    scene.apply(root(vec![dot("a", 0.0), dot("a", 1.0)]), 0.0);
}

#[test]
#[should_panic(expected = "time order")]
#[cfg(debug_assertions)]
fn events_out_of_order_panic_in_debug() {
    let mut scene = Scene::<Msg>::new();
    scene.apply(root(vec![]), 1.0);
    scene.apply(root(vec![]), 0.5);
}

#[test]
fn opacity_zero_draws_nothing_and_layers_carry_group_opacity() {
    let mut scene = Scene::<Msg>::new();
    let view = root(vec![
        dot("hidden", 0.0).opacity(0.0),
        Element::layer("group").opacity(0.5).size(20.0, 20.0).child(dot("inside", 0.0)),
    ]);
    scene.apply(view, 0.0);
    let list = scene.sample(0.0);
    assert_eq!(list.shapes.len(), 1);
    assert_eq!(list.layers.len(), 2);
    assert_eq!(list.layers[1].opacity, 0.5);
    // Inside the layer the colour keeps full alpha; the layer applies the group opacity.
    assert_eq!(list.shapes[0].fill.c0.a, 1.0);
}
