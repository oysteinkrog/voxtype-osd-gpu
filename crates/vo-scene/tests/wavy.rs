//! The wavy line (T06): it draws on from the left by its trim, its amplitude, wavelength and
//! phase spring, and a spring to amplitude 0 straightens it.

use vo_core::spring::Spring;
use vo_scene::display::DisplayList;
use vo_scene::element::Element;
use vo_scene::geom::Color;
use vo_scene::path::StrokeStyle;
use vo_scene::{Prop, Scene, Transition};

const DRAW_ON: Spring = Spring::critical_to_98(0.250);

fn line(amp: f32, wavelength: f32) -> Element<()> {
    Element::wavy_line("w", StrokeStyle::solid(2.0, Color::WHITE), amp, wavelength)
        .size(200.0, 20.0)
        .presence(Transition::NONE.trim(0.0, 0.0), Transition::fade())
        .motion(Prop::Trim, DRAW_ON)
        .motion(Prop::Wave, Spring::SOFT)
        .motion(Prop::Opacity, Spring::critical_to_98(0.200))
}

fn view(w: Element<()>) -> Element<()> {
    Element::column("root").child(w)
}

/// The drawn points' x extent and y extent, or `None` when nothing is drawn.
fn extent(list: &DisplayList) -> Option<((f32, f32), (f32, f32))> {
    let segs = &list.vector_segs;
    if list.vectors.is_empty() || segs.is_empty() {
        return None;
    }
    let pts = segs.iter().flat_map(|s| [s.p0, s.p2]);
    let fold = |(lo, hi): (f32, f32), v: f32| (lo.min(v), hi.max(v));
    let inf = (f32::INFINITY, f32::NEG_INFINITY);
    let (xs, ys) = pts.fold((inf, inf), |(x, y), p| (fold(x, p.x), fold(y, p.y)));
    Some((xs, ys))
}

#[test]
fn draws_on_from_the_left_in_about_250_ms() {
    let mut s = Scene::<()>::new();
    s.apply(view(line(3.0, 12.0)), 0.0);
    assert!(extent(&s.sample(0.0)).is_none(), "nothing is drawn before the trim opens");
    let ((x0, x1), _) = extent(&s.sample(0.08)).expect("part of the line at 80 ms");
    assert!(x0.abs() < 1e-3, "it starts at the left edge: {x0}");
    assert!(x1 > 20.0 && x1 < 190.0, "and has reached part of the way: {x1}");
    let ((_, x1), (y0, y1)) = extent(&s.sample(0.26)).expect("the whole line");
    assert!(x1 > 196.0, "drawn to the right edge by 260 ms: {x1}");
    assert!((y1 - y0 - 6.0).abs() < 0.1, "peak to peak is twice the amplitude: {}", y1 - y0);
    assert!(((y0 + y1) * 0.5 - 10.0).abs() < 0.1, "centred in the box");
}

#[test]
fn confirming_straightens_the_line_and_fades_it() {
    let mut s = Scene::<()>::new();
    s.apply(view(line(3.0, 12.0)), 0.0);
    // Confirmed: the amplitude springs to 0 and the line fades.
    s.apply(view(line(3.0, 12.0).wave_shape(0.0, 12.0).opacity(0.0)), 1.0);
    let list = s.sample(1.06);
    let (_, (y0, y1)) = extent(&list).expect("still drawn while it fades");
    assert!(y1 - y0 > 0.2 && y1 - y0 < 6.0, "part way straight: {}", y1 - y0);
    let a = list.vectors[0].c0.a;
    assert!(a > 0.05 && a < 1.0, "part way faded: {a}");
    let l = s.sample(1.5);
    assert!(l.vectors.iter().all(|v| v.c0.a < 1e-3), "faded out by 500 ms");
    let (_, (y0, y1)) = extent(&l).unwrap_or(((0.0, 0.0), (10.0, 10.0)));
    assert!(y1 - y0 < 1e-3, "and straight");
    assert_eq!(s.next_wake(1.6), None, "and settled 600 ms after the confirm");
}

#[test]
fn wavelength_and_phase_spring() {
    let mut s = Scene::<()>::new();
    s.apply(view(line(3.0, 10.0).motion(Prop::Trim, Spring::SNAP)), 0.0);
    // The first crest (the largest y) is a quarter wave in; doubling the wavelength moves
    // it out.
    let crest = |list: &DisplayList| {
        let p =
            list.vector_segs.iter().map(|s| s.p0).filter(|p| p.x < 20.0).fold(None, |best: Option<(f32, f32)>, p| {
                match best {
                    Some(b) if b.1 >= p.y => Some(b),
                    _ => Some((p.x, p.y)),
                }
            });
        p.expect("points near the start").0
    };
    let before = crest(&s.sample(1.0));
    assert!((before - 2.5).abs() < 0.7, "{before}");
    s.apply(view(line(3.0, 20.0).motion(Prop::Trim, Spring::SNAP)), 1.0);
    let mid = crest(&s.sample(1.05));
    let after = crest(&s.sample(2.0));
    assert!((after - 5.0).abs() < 0.7, "{after}");
    assert!(mid > before && mid < after + 0.7, "the crest moves out on a spring: {mid}");
    // A quarter-wave phase moves the crest a quarter wave to the right.
    s.apply(view(line(3.0, 20.0).motion(Prop::Trim, Spring::SNAP).wave_phase(0.25)), 2.0);
    let shifted = crest(&s.sample(3.0));
    assert!((shifted - 10.0).abs() < 0.7, "{shifted}");
}
