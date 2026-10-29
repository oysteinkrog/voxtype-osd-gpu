//! Trails that drift and turn (EG-34): the afterglow of a feedback trail turns about a pivot
//! by the time between frames, so the turn after one second is the same at any frame rate.
//! Checked at scale 1 and 1.5, on a dark and a light background.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use vo_render::Offscreen;
use vo_scene::display::{DisplayList, Item, ItemKind, Layer, LayerTarget, Paint, ShapeInstance, Trail, TrailDrift};
use vo_scene::geom::{Color, Point, RRect, Rect, Size};

const W: f32 = 200.0;
const H: f32 = 200.0;
const PIVOT: Point = Point::new(100.0, 100.0);

struct Look {
    background: Option<Color>,
    dot: Color,
}

const DARK: Look = Look { background: None, dot: Color::rgba(0.3, 0.8, 0.95, 1.0) };
const LIGHT: Look = Look { background: Some(Color::rgba(0.96, 0.95, 0.92, 1.0)), dot: Color::rgba(0.1, 0.12, 0.2, 1.0) };

/// A background on the root, then a trail layer with `dots` on it, at time `t`.
fn frame(look: &Look, dots: &[Point], t: f64, drift: Option<TrailDrift>, half_life: f32) -> DisplayList {
    let mut list = DisplayList { size: Size::new(W, H), scale: 1.0, ..Default::default() };
    list.layers.push(Layer::root());
    let mut root_items = Vec::new();
    if let Some(bg) = look.background {
        root_items.push(Item { kind: ItemKind::Shapes, start: 0, len: 1 });
        list.shapes.push(ShapeInstance::rrect(RRect::new(Rect::new(0.0, 0.0, W, H), 0.0), Paint::solid(bg)));
    }
    root_items.push(Item { kind: ItemKind::Layer, start: 1, len: 1 });
    list.items.extend(root_items);
    list.layers[0].items = 0..list.items.len() as u32;
    list.layers.push(Layer {
        parent: Some(0),
        target: LayerTarget::Offscreen { bloom: None },
        trail: Some(Trail { key: 3, half_life, t, decay: [1.0; 3], drift }),
        ..Layer::root()
    });
    let start = list.items.len() as u32;
    if !dots.is_empty() {
        let first = list.shapes.len() as u32;
        for &p in dots {
            list.shapes.push(ShapeInstance::circle(p, 5.0, Paint::solid(look.dot)));
        }
        list.items.push(Item { kind: ItemKind::Shapes, start: first, len: dots.len() as u32 });
    }
    list.layers[1].items = start..list.items.len() as u32;
    list
}

fn render(off: &mut Offscreen, list: &DisplayList, scale: f32) -> Vec<u8> {
    let (w, h) = ((list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32);
    off.render_to_rgba(list, [w, h], scale)
}

fn offscreen(gpu: &support::Gpu) -> Offscreen {
    Offscreen::new(&gpu.device, &gpu.queue)
}

/// The light's centroid in logical units: each pixel weighed by how far it is from the
/// background.
fn centroid(px: &[u8], bg: &[u8], scale: f32) -> (f32, f32) {
    let w = (W * scale).ceil() as usize;
    let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
    for (i, (p, b)) in px.as_chunks::<4>().0.iter().zip(bg.as_chunks::<4>().0).enumerate() {
        let d: f64 = (0..4).map(|c| (p[c] as f64 - b[c] as f64).abs()).sum();
        if d < 6.0 {
            continue;
        }
        let (x, y) = ((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
        (sx, sy, sw) = (sx + d * x, sy + d * y, sw + d);
    }
    assert!(sw > 0.0, "no light left");
    ((sx / sw) as f32 / scale, (sy / sw) as f32 / scale)
}

/// The angle of `p` about the pivot in degrees, clockwise on screen from +x.
fn angle(p: (f32, f32)) -> f32 {
    (p.1 - PIVOT.y).atan2(p.0 - PIVOT.x).to_degrees()
}

const TURN: TrailDrift = TrailDrift { turn_deg_per_s: 90.0, scale_per_s: 0.0, offset_px_per_s: [0.0; 2], pivot: PIVOT };

/// A dot 60 units right of the pivot, drawn in the first frame only, then one second of
/// frames at `fps` with nothing new: the afterglow is all that is left.
fn flash_then_turn(gpu: &support::Gpu, look: &Look, scale: f32, fps: u32) -> (Vec<u8>, Vec<u8>) {
    let mut off = offscreen(gpu);
    let dot = [Point::new(PIVOT.x + 60.0, PIVOT.y)];
    let mut last = Vec::new();
    for i in 0..=fps {
        let dots: &[Point] = if i == 0 { &dot } else { &[] };
        last = render(&mut off, &frame(look, dots, i as f64 / fps as f64, Some(TURN), 2.0), scale);
    }
    let bg = render(&mut offscreen(gpu), &frame(look, &[], 0.0, None, 2.0), scale);
    (last, bg)
}

#[test]
fn the_afterglow_turns_ninety_degrees_in_a_second() {
    let gpu = support::gpu();
    for (look, name) in [(&DARK, "dark"), (&LIGHT, "light")] {
        for scale in [1.0, 1.5] {
            let (img, bg) = flash_then_turn(&gpu, look, scale, 60);
            let c = centroid(&img, &bg, scale);
            let a = angle(c);
            eprintln!("trail drift: {name} at {scale}x, the light turned {a:.2} degrees in 1 s");
            assert!((a - 90.0).abs() <= 5.0, "{name} at {scale}x: the light sits at {a} degrees ({c:?})");
            // It stays on the circle: the radius is kept within a few units of blur.
            let r = ((c.0 - PIVOT.x).powi(2) + (c.1 - PIVOT.y).powi(2)).sqrt();
            assert!((r - 60.0).abs() < 4.0, "{name} at {scale}x: radius {r}");
        }
    }
}

#[test]
fn a_still_dot_leaves_a_curved_streak() {
    let gpu = support::gpu();
    let mut off = offscreen(&gpu);
    let dot = [Point::new(PIVOT.x + 60.0, PIVOT.y)];
    let mut img = Vec::new();
    for i in 0..=30 {
        img = render(&mut off, &frame(&DARK, &dot, i as f64 / 60.0, Some(TURN), 0.4), 1.0);
    }
    // After half a second the streak runs from the dot round to 45 degrees.
    let at = |deg: f32| {
        let r = deg.to_radians();
        let (x, y) = ((PIVOT.x + 60.0 * r.cos()) as usize, (PIVOT.y + 60.0 * r.sin()) as usize);
        img[(y * W as usize + x) * 4 + 3]
    };
    let arc: Vec<u8> = [0.0, 10.0, 20.0, 30.0, 40.0].iter().map(|&d| at(d)).collect();
    assert!(arc.iter().all(|&a| a > 20), "lit along the arc: {arc:?}");
    assert!(arc.windows(2).all(|w| w[0] >= w[1]), "fainter with age: {arc:?}");
    assert!(at(-20.0) < 3, "nothing on the other side: {}", at(-20.0));
}

/// A dot on a circle at `t`, so a moving trail has a swirl to draw.
fn swirl_frame(t: f64, look: &Look) -> DisplayList {
    let a = t * 5.0;
    let p = Point::new(100.0 + 50.0 * a.cos() as f32, 100.0 + 30.0 * a.sin() as f32);
    let drift = TrailDrift { turn_deg_per_s: -120.0, scale_per_s: 0.25, offset_px_per_s: [6.0, -4.0], pivot: PIVOT };
    frame(look, &[p], t, Some(drift), 0.3)
}

fn swirl_at(gpu: &support::Gpu, fps: u32, until: f64, scale: f32, look: &Look) -> Vec<u8> {
    let mut off = offscreen(gpu);
    let n = (until * fps as f64).round() as u32;
    (0..=n).map(|i| render(&mut off, &swirl_frame(i as f64 / fps as f64, look), scale)).last().unwrap()
}

#[test]
fn same_fps_is_byte_identical_and_60_matches_120() {
    let gpu = support::gpu();
    for (look, scale) in [(&DARK, 1.0), (&LIGHT, 1.5)] {
        let a = swirl_at(&gpu, 60, 0.75, scale, look);
        assert_eq!(a, swirl_at(&gpu, 60, 0.75, scale, look), "two renders at 60 fps differ");
        let b = swirl_at(&gpu, 120, 0.75, scale, look);
        let mean = a.iter().zip(&b).map(|(x, y)| (*x as f64 - *y as f64).abs()).sum::<f64>() / a.len() as f64;
        assert!(mean <= 4.0, "60 and 120 fps differ by a mean of {mean:.2}/255 at {scale}x");
        eprintln!("trail drift: 60 vs 120 fps mean difference {mean:.3}/255 at {scale}x");
    }
}

#[test]
fn a_still_drift_draws_as_a_plain_trail() {
    let gpu = support::gpu();
    let run = |drift: Option<TrailDrift>| {
        let mut off = offscreen(&gpu);
        (0..10)
            .map(|i| {
                let p = Point::new(30.0 + 14.0 * i as f32, 100.0);
                render(&mut off, &frame(&DARK, &[p], i as f64 / 60.0, drift, 0.05), 1.0)
            })
            .last()
            .unwrap()
    };
    assert_eq!(run(None), run(Some(TrailDrift::default())));
}
