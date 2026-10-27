//! Ambient loops with a rest state (FX-203) and ripples (FX-101): a mic disc with ripples, a
//! spinner, a breathing glow, "Listening" with "..." dots (FX-047), a gentle bob and a stripe
//! marquee. The loops run from 0 s, stop at 3 s, run again from 4.5 s and stop at 6.5 s. Each
//! time they stop, every loop settles to its rest and the scene asks for no more frames.
//!
//!   cargo run --release -p vo --example ambient_demo -- [--out DIR] [--check]
//!   cargo run --release -p vo --example ambient_demo -- --bench
//!
//! The default writes `DIR/ambient_demo.mp4` and `DIR/ambient-sheet.png` (default DIR
//! `/var/tmp/claude/wp10`). `--check` renders again and compares every frame's hash.
//! `--bench` prints the GPU time per frame (timestamp queries) at 600x300 and 1920x1080 while
//! the loops run, and how many frames a host following `next_wake` draws in each phase.

use std::path::PathBuf;

use vo::scene::display::ShapeKind;
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::taffy::prelude::{auto, length, AlignItems, JustifyContent};
use vo::scene::{Loop, LoopProp, Ripples, Scene};
use vo::{App, Cx, Export, FontRegistry, Options, Scenario, ShapedLayout, Video};
use vo_render::offscreen::{write_png, Frame};
use vo_render::{Renderer, Target};

const FPS: f64 = 60.0;
const DURATION: f64 = 8.0;
/// When the loops start and stop: `(time, on)`.
const EVENTS: [(f64, bool); 4] = [(0.0, true), (3.0, false), (4.5, true), (6.5, false)];

fn accent() -> Color {
    Color::hex(0x7dd3fc, 1.0)
}

struct Demo {
    size: Size,
    on: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Set(bool),
}

impl App for Demo {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        for (t, on) in EVENTS {
            cx.after(t, Msg::Set(on));
        }
    }

    fn update(&mut self, msg: Msg, _: &mut Cx<Msg>) {
        let Msg::Set(on) = msg;
        self.on = on;
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        view(self.size, self.on)
    }
}

fn view(size: Size, on: bool) -> Element<Msg> {
    // Everything scales with the surface height, so the bench sizes show the same picture.
    let k = size.h / 300.0;
    let ink = Color::hex(0xe6ebf5, 1.0);
    let with = |e: Element<Msg>, prop: LoopProp, l: Loop| if on { e.ambient(prop, l) } else { e };

    // The mic: ripples behind a disc, both centred in one box.
    let mic_box = 200.0 * k;
    let disc = 44.0 * k;
    let centred = move |s: &mut vo::scene::taffy::Style| {
        s.position = vo::scene::taffy::Position::Absolute;
        let at = 0.5 * (mic_box - disc);
        s.inset = vo::scene::taffy::Rect { left: length(at), top: length(at), right: auto(), bottom: auto() };
    };
    let ripples = Ripples::new(accent()).radii(0.5 * disc, 0.5 * mic_box - 4.0 * k).timing(0.7, 2.0).width(2.0 * k);
    let mic = Element::panel("mic")
        .size(mic_box, mic_box)
        .child(Element::ripples("rip", ripples.active(on)).size(mic_box, mic_box))
        .child(with(
            Element::shape("disc", ShapeKind::Circle)
                .size(disc, disc)
                .fill(accent())
                .glow(accent(), 10.0 * k, 0.5)
                .layout(centred),
            LoopProp::GlowStrength,
            Loop::pulse(1.0, 1.6),
        ));

    let spinner = with(
        Element::shape("spin", ShapeKind::Capsule).size(56.0 * k, 7.0 * k).fill(ink),
        LoopProp::Rotation,
        Loop::spin(1.2),
    );
    let breath = with(
        Element::shape("breath", ShapeKind::Circle).size(22.0 * k, 22.0 * k).fill(accent()).glow(
            accent(),
            10.0 * k,
            0.4,
        ),
        LoopProp::Scale,
        Loop::pulse(0.18, 1.6),
    );
    let mut dots = Element::row("dots").gap(5.0 * k).align_items(AlignItems::FLEX_END).height(30.0 * k);
    for i in 0..3 {
        let d = Element::shape(format!("d{i}"), ShapeKind::Circle).size(6.0 * k, 6.0 * k).fill(ink).opacity(0.0);
        dots = dots.child(with(d, LoopProp::Opacity, Loop::step_on(i, 3, 1.2)));
    }
    let label = Element::label("listening", "Listening").text_style(TextStyle::new(0, 26.0 * k, 30.0 * k)).color(ink);
    let words = with(
        Element::row("words").gap(2.0 * k).align_items(AlignItems::FLEX_END).child(label).child(dots),
        LoopProp::OffsetY,
        Loop::bob(2.0 * k as f64, 2.4),
    );
    let mut stripes = Element::row("bars").gap(8.0 * k);
    for i in 0..24 {
        stripes =
            stripes.child(Element::shape(format!("s{i}"), ShapeKind::RRect).size(8.0 * k, 12.0 * k).fill(accent()));
    }
    let tile = 16.0 * k as f64;
    let marquee = Element::layer("track").clip().size(220.0 * k, 12.0 * k).child(with(
        stripes,
        LoopProp::OffsetX,
        Loop::marquee(tile, 0.8),
    ));

    let right = Element::column("right")
        .gap(22.0 * k)
        .align_items(AlignItems::FLEX_START)
        .child(words)
        .child(Element::row("busy").gap(28.0 * k).align_items(AlignItems::CENTER).child(spinner).child(breath))
        .child(marquee);
    Element::panel("root")
        .size(size.w, size.h)
        .direction_row()
        .fill_gradient(Color::hex(0x0b0d14, 1.0), Color::hex(0x141826, 1.0))
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
        .gap(40.0 * k)
        .child(mic)
        .child(right)
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let path = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
    fonts
}

/// A scene with every event applied, like the host does at each event's own time.
fn scene(fonts: &FontRegistry, size: Size) -> Scene<Msg> {
    let mut scene: Scene<Msg> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, size, 1.0);
    for (t, on) in EVENTS {
        scene.apply(view(size, on), t);
    }
    scene
}

/// Frames for the contact sheet: running, settling after the first stop, at rest, running
/// again, at rest at the end.
const SHEET_AT: [f64; 8] = [0.5, 1.0, 1.6, 3.1, 3.4, 4.2, 5.3, 7.9];

/// Thumbnails in a 4 by 2 grid with an 8 px gutter, at half size.
fn contact_sheet(frames: &[Frame]) -> Frame {
    let half = |f: &Frame| {
        let (w, h) = (f.width / 2, f.height / 2);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                for c in 0..4 {
                    let at = |dx: u32, dy: u32| f.rgba[(((2 * y + dy) * f.width + 2 * x + dx) * 4 + c) as usize] as u32;
                    rgba[((y * w + x) * 4 + c) as usize] = ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) / 4) as u8;
                }
            }
        }
        Frame { width: w, height: h, rgba }
    };
    let thumbs: Vec<Frame> = frames.iter().map(half).collect();
    let (tw, th, gap, cols) = (thumbs[0].width, thumbs[0].height, 8u32, 4u32);
    let rows = (thumbs.len() as u32).div_ceil(cols);
    let (w, h) = (cols * tw + (cols + 1) * gap, rows * th + (rows + 1) * gap);
    let mut rgba = vec![255u8; (w * h * 4) as usize];
    for (i, t) in thumbs.iter().enumerate() {
        let (ox, oy) = (gap + (i as u32 % cols) * (tw + gap), gap + (i as u32 / cols) * (th + gap));
        for y in 0..th {
            let src = &t.rgba[(y * tw * 4) as usize..((y + 1) * tw * 4) as usize];
            let at = (((oy + y) * w + ox) * 4) as usize;
            rgba[at..at + src.len()].copy_from_slice(src);
        }
    }
    Frame { width: w, height: h, rgba }
}

fn export(out: &std::path::Path, check: bool) {
    let fonts = fonts();
    let size = Size::new(720.0, 300.0);
    let options = || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };
    let scenario = Scenario { size, scale: 1.0, duration: DURATION, inputs: Vec::new() };
    let mp4 = out.join("ambient_demo.mp4");
    let spec = Export { fps: FPS, duration: DURATION, png_dir: None, video: Some(Video::h264(&mp4)) };
    let sheet_frames: Vec<u64> = SHEET_AT.iter().map(|t| (t * FPS).round() as u64).collect();
    let mut picked = Vec::new();
    let app = || Demo { size, on: false };
    let report = vo::export_with(app(), &scenario, &spec, options(), |n, f| {
        if sheet_frames.contains(&n) {
            picked.push(f.clone());
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("ambient_demo: {e}");
        std::process::exit(1);
    });
    println!("ambient_demo: {} frames, {}", report.frames, mp4.display());
    let sheet = contact_sheet(&picked);
    let sheet_path = out.join("ambient-sheet.png");
    write_png(&sheet_path, sheet.width, sheet.height, &sheet.rgba).expect("write contact sheet");
    println!("ambient_demo: contact sheet {}", sheet_path.display());
    if check {
        let again = Export { video: None, ..spec.clone() };
        let second = vo::export(app(), &scenario, &again, options()).expect("second render");
        let differ = report.frame_hashes.iter().zip(&second.frame_hashes).filter(|(a, b)| a != b).count();
        println!("ambient_demo: second render, {differ} of {} frames differ", report.frames);
        if differ > 0 {
            std::process::exit(1);
        }
    }
}

fn gpu() -> (wgpu::Device, wgpu::Queue) {
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
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ambient-demo"),
        required_features: wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
        ..Default::default()
    }))
    .expect("a GPU device")
}

/// Frames a host draws between `from` and `to` when it follows `next_wake`.
fn host_frames(scene: &Scene<Msg>, from: f64, to: f64) -> usize {
    let (mut t, mut n) = (from, 0);
    while let Some(w) = scene.next_wake(t) {
        // "Now" means the next display frame, at 60 Hz.
        let w = if w <= t { t + 1.0 / 60.0 } else { w };
        if w >= to {
            break;
        }
        n += 1;
        t = w;
    }
    n
}

fn bench() {
    let fonts = fonts();
    let s = scene(&fonts, Size::new(720.0, 300.0));
    for (name, from, to) in [("running", 0.0, 3.0), ("settling", 3.0, 4.5), ("running", 4.5, 6.5), ("idle", 8.5, 60.0)]
    {
        let n = host_frames(&s, from, to);
        println!(
            "ambient_demo bench: {name} {from:.1} to {to:.1} s: {n} frames ({:.1} a second)",
            n as f64 / (to - from)
        );
    }
    let (device, queue) = gpu();
    let period = queue.get_timestamp_period() as f64;
    let mut renderer = Renderer::with_fonts(&device, &queue, fonts.clone());
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    for (w, h) in [(600u32, 300u32), (1920, 1080)] {
        let size = Size::new(w as f32, h as f32);
        let scene = scene(&fonts, size);
        let target = Target::new(&device, w, h, vo_render::offscreen::FORMAT);
        let mut times = Vec::new();
        for i in 0..220 {
            // A different moment each frame while the loops run, 1/60 s apart.
            let list = scene.sample(0.2 + (i % 150) as f64 / 60.0);
            let mut enc = device.create_command_encoder(&Default::default());
            enc.write_timestamp(&qs, 0);
            renderer.encode(&list, 1.0, &target, &mut enc);
            enc.write_timestamp(&qs, 1);
            enc.resolve_query_set(&qs, 0..2, &resolve, 0);
            enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, 16);
            queue.submit([enc.finish()]);
            read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let ts: [u64; 2] = bytemuck::pod_read_unaligned(&read.slice(..).get_mapped_range().unwrap()[..16]);
            read.unmap();
            // The first frames compile pipelines and fill the pool.
            if i >= 20 {
                times.push((ts[1] - ts[0]) as f64 * period / 1e6);
            }
        }
        times.sort_by(f64::total_cmp);
        let median = times[times.len() / 2];
        let p95 = times[times.len() * 95 / 100];
        println!("ambient_demo bench: {w}x{h} running: GPU {median:.3} ms median, {p95:.3} ms p95");
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut check) = (PathBuf::from("/var/tmp/claude/wp10"), false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--check" => check = true,
            "--bench" => return bench(),
            // Read by the host: open on the real desktop on purpose.
            "--real-display" => {}
            _ => panic!("unknown option {a}"),
        }
    }
    export(&out, check);
}
