//! Trails that drift and turn (EG-34): the swirl of Breath Ripple (gallery card 22).
//!
//! Five dots ride a ring that breathes in and out. They sit on a feedback trail whose
//! afterglow turns 70 degrees a second and grows 18% a second about the ring's centre, so
//! each dot leaves a streak that keeps turning and spreads outward like a ripple. The canvas
//! is pure in `t`; the trail is replayed frame by frame at `--fps`.
//!
//! Reduced motion keeps the trail in place (no drift), so the dots leave short plain tails.
//!
//!   cargo run -p vo --example trail_swirl -- [--out DIR] [--reduced] [--scale S] [--fps F]
//!
//! Writes `DIR/trail-swirl.png` (default DIR `/var/tmp/claude/trail-swirl`): one row per
//! moment.

use std::path::PathBuf;

use vo::scene::canvas::Painter;
use vo::scene::display::{Paint, ShapeInstance, TrailDrift};
use vo::scene::element::Element;
use vo::scene::geom::{Color, Point, Rect, Size};
use vo::scene::Scene;
use vo::FontRegistry;
use vo_render::offscreen::write_png;
use vo_render::Offscreen;

/// The moments in the output, one row each.
const MOMENTS: [f64; 5] = [0.4, 1.0, 1.8, 2.6, 3.6];

const W: f32 = 600.0;
const H: f32 = 300.0;
const DOTS: usize = 5;

/// The ring's breath, 0 to 1, one breath every 3.2 s.
fn breath(t: f64) -> f32 {
    (0.5 - 0.5 * (t * std::f64::consts::TAU / 3.2).cos()) as f32
}

fn draw(t: f64, rect: Rect, p: &mut Painter) {
    let c = Point::new(rect.x + 0.5 * rect.w, rect.y + 0.5 * rect.h);
    let r = 40.0 + 45.0 * breath(t);
    for i in 0..DOTS {
        let a = i as f64 * std::f64::consts::TAU / DOTS as f64 + 0.6 * t;
        let at = Point::new(c.x + r * a.cos() as f32, c.y + r * a.sin() as f32);
        p.shape(ShapeInstance::circle(at, 4.0, Paint::glow(Color::hex(0x4cc9f0, 1.0), 6.0, 0.8)));
        p.shape(ShapeInstance::circle(at, 3.0, Paint::solid(Color::hex(0xe6f7ff, 1.0))));
    }
}

fn view() -> Element<()> {
    let drift = TrailDrift {
        turn_deg_per_s: 70.0,
        scale_per_s: 0.18,
        offset_px_per_s: [0.0; 2],
        pivot: Point::new(0.5 * W, 0.5 * H),
    };
    let canvas = Element::canvas("ring", f64::INFINITY, draw).size(W, H);
    let swirl = Element::layer("swirl").size(W, H).trail(0.45).trail_drift(drift).child(canvas);
    Element::panel("root").size(W, H).fill(Color::hex(0x0b0f16, 1.0)).child(swirl)
}

fn main() {
    let mut out = PathBuf::from("/var/tmp/claude/trail-swirl");
    let (mut reduced, mut scale, mut fps) = (false, 1.0f32, 60.0f64);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out takes a directory").into(),
            "--reduced" => reduced = true,
            "--scale" => scale = args.next().and_then(|v| v.parse().ok()).expect("--scale takes a number"),
            "--fps" => fps = args.next().and_then(|v| v.parse().ok()).expect("--fps takes a number"),
            other => panic!("unknown argument {other}"),
        }
    }
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a Vulkan adapter");
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("a GPU device");
    let mut off = Offscreen::with_fonts(&device, &queue, FontRegistry::new());
    let (w, h) = ((W * scale).ceil() as u32, (H * scale).ceil() as u32);
    let mut scene: Scene<()> = Scene::new().with_reduced_motion(reduced);
    scene.set_viewport(0.0, Size::new(W, H), scale);
    scene.apply(view(), 0.0);
    // The trail needs every frame up to each moment, in order.
    let mut sheet = Vec::with_capacity((w * h * 4) as usize * MOMENTS.len());
    let last = (MOMENTS[MOMENTS.len() - 1] * fps).round() as u64;
    let picks: Vec<u64> = MOMENTS.iter().map(|t| (t * fps).round() as u64).collect();
    for n in 0..=last {
        let t = n as f64 / fps;
        let px = off.render_to_rgba(&scene.sample(t), [w, h], scale);
        if picks.contains(&n) {
            sheet.extend(px);
        }
    }
    std::fs::create_dir_all(&out).expect("create the output directory");
    let file = out.join(if reduced { "trail-swirl-reduced.png" } else { "trail-swirl.png" });
    write_png(&file, w, h * MOMENTS.len() as u32, &sheet).expect("write the sheet");
    println!("trail_swirl: {}", file.display());
}
