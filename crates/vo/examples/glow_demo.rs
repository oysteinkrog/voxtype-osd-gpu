//! Effects demo: a capsule with an analytic glow and a word in a bloom layer, both pulsing with
//! a spring-driven level, over a card with a soft drop shadow.
//!
//! The level is a made-up speech envelope, sampled 20 times a second by timers. Each sample
//! is a spring target, so glow, bloom and size move smoothly between them.
//!
//!   cargo run --release -p vo --example glow_demo -- [--out DIR] [--check]
//!   cargo run --release -p vo --example glow_demo -- --bench
//!   cargo run --release -p vo --example glow_demo -- --cold effect|direct
//!   cargo run --release -p vo --example glow_demo -- --live
//!
//! The default writes `DIR/glow_demo.mp4` and `DIR/contact-sheet.png` (default DIR
//! `/var/tmp/claude/m4-fx`). `--check` renders again and compares every frame's hash.
//! `--bench` prints GPU time per frame (timestamp queries) for the effect path at 600x300 and
//! 1920x1080, the direct path for the same scene without effects, and the pooled texture
//! memory. `--cold` prints the time from process start to the first frame read back, for one
//! scene, so it can be run once per fresh process. `--live` shows it as an overlay.

use std::path::PathBuf;
use std::time::Instant;

use vo::scene::display::ShapeKind;
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::taffy::prelude::{AlignItems, JustifyContent};
use vo::scene::{Prop, Scene};
use vo::{App, Cx, Export, FontRegistry, Options, Scenario, ShapedLayout, Video};
use vo_core::spring::Spring;
use vo_render::offscreen::{write_png, Frame};
use vo_render::{Offscreen, Renderer, Target};

const FPS: f64 = 60.0;
const DURATION: f64 = 6.0;
/// Level samples per second.
const RATE: f64 = 20.0;

fn accent() -> Color {
    Color::hex(0x4cc9f0, 1.0)
}

/// A speech-like envelope in 0..1: syllables at about 4 Hz under a slower phrase shape, with
/// pauses. Pure in `t`.
fn level(t: f64) -> f32 {
    let phrase = (0.5 + 0.5 * (t * 0.9).sin()).powf(0.7);
    let syllable = (0.5 + 0.5 * (t * 25.0).sin() * (t * 7.3 + 1.0).cos()).powi(2);
    let pause = if (t % 3.0) > 2.4 { 0.1 } else { 1.0 };
    (phrase * (0.25 + 0.75 * syllable) * pause).clamp(0.0, 1.0) as f32
}

struct Demo {
    size: Size,
    level: f32,
    /// Draw glow, shadow and bloom. Off for the direct-path comparison.
    effects: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Level(f32),
}

impl App for Demo {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        let n = (DURATION * RATE) as usize;
        for i in 0..n {
            let t = i as f64 / RATE;
            cx.after(t, Msg::Level(level(t)));
        }
    }

    fn update(&mut self, msg: Msg, _: &mut Cx<Msg>) {
        let Msg::Level(v) = msg;
        self.level = v;
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        view(self.size, self.level, self.effects)
    }
}

fn view(size: Size, level: f32, effects: bool) -> Element<Msg> {
    // Everything scales with the surface height, so the bench sizes show the same picture.
    let k = size.h / 400.0;
    let pulse = Spring::from_frequency(18.0, 0.8);
    let mut mic = Element::shape("mic", ShapeKind::Capsule)
        .size((64.0 + 90.0 * level) * k, 28.0 * k)
        .fill(accent())
        .motion(Prop::Size, pulse)
        .motion(Prop::Glow, pulse);
    let word_color = Color::rgba(0.85 + 0.15 * level, 0.95, 1.0, 1.0);
    let label = Element::label("text", "listening").text_style(TextStyle::new(0, 64.0 * k, 76.0 * k)).color(word_color);
    let mut word = Element::layer("word").child(label).scale(1.0 + 0.05 * level).motion(Prop::Scale, pulse);
    let mut card = Element::panel("card")
        .size(420.0 * k, 64.0 * k)
        .radius(16.0 * k)
        .fill(Color::hex(0x1d2233, 1.0))
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
        .child(
            Element::label("caption", "glow, bloom and a soft shadow")
                .text_style(TextStyle::new(0, 22.0 * k, 28.0 * k))
                .color(Color::hex(0x9aa3b8, 1.0)),
        );
    if effects {
        mic = mic.glow(accent(), (6.0 + 16.0 * level) * k, 0.5 + 2.5 * level);
        word = word.bloom(0.3 + 1.6 * level, (4.0 + 12.0 * level) * k).motion(Prop::Bloom, pulse);
        card = card.shadow(Color::rgba(0.0, 0.0, 0.0, 0.7), 0.0, 10.0 * k, 16.0 * k);
    }
    Element::panel("root")
        .size(size.w, size.h)
        .fill_gradient(Color::hex(0x0b0d14, 1.0), Color::hex(0x141826, 1.0))
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
        .gap(36.0 * k)
        .child(Element::row("stage").gap(40.0 * k).align_items(AlignItems::CENTER).child(mic).child(word))
        .child(card)
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let path = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
    fonts
}

/// Frames for the contact sheet.
const SHEET_AT: [f64; 8] = [0.4, 0.9, 1.4, 1.9, 2.3, 2.7, 3.6, 4.4];

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
    let size = Size::new(960.0, 400.0);
    let options = || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };
    let scenario = Scenario { size, scale: 1.0, duration: DURATION, inputs: Vec::new() };
    let mp4 = out.join("glow_demo.mp4");
    let spec = Export { fps: FPS, duration: DURATION, png_dir: None, video: Some(Video::h264(&mp4)) };
    let sheet_frames: Vec<u64> = SHEET_AT.iter().map(|t| (t * FPS).round() as u64).collect();
    let mut picked = Vec::new();
    let start = Instant::now();
    let app = || Demo { size, level: 0.0, effects: true };
    let report = vo::export_with(app(), &scenario, &spec, options(), |n, f| {
        if sheet_frames.contains(&n) {
            picked.push(f.clone());
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("glow_demo: {e}");
        std::process::exit(1);
    });
    let secs = start.elapsed().as_secs_f64();
    println!(
        "glow_demo: {} frames {}x{} in {:.2} s ({:.0} fps), {}",
        report.frames,
        report.width,
        report.height,
        secs,
        report.frames as f64 / secs,
        mp4.display()
    );
    let sheet = contact_sheet(&picked);
    let sheet_path = out.join("contact-sheet.png");
    write_png(&sheet_path, sheet.width, sheet.height, &sheet.rgba).expect("write contact sheet");
    println!("glow_demo: contact sheet {}", sheet_path.display());
    if check {
        let again = Export { video: None, ..spec.clone() };
        let second = vo::export(app(), &scenario, &again, options()).expect("second render");
        let differ = report.frame_hashes.iter().zip(&second.frame_hashes).filter(|(a, b)| a != b).count();
        println!("glow_demo: second render, {differ} of {} frames differ", report.frames);
        if differ > 0 {
            std::process::exit(1);
        }
    }
}

fn gpu(timestamps: bool) -> (wgpu::Device, wgpu::Queue) {
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
    let features = if timestamps {
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS
    } else {
        wgpu::Features::empty()
    };
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("glow-demo"),
        required_features: features,
        ..Default::default()
    }))
    .expect("a GPU device")
}

/// The scene's display list at `t`, after the level samples up to then.
fn sample(fonts: &FontRegistry, size: Size, effects: bool, t: f64) -> vo::scene::DisplayList {
    let mut scene: Scene<Msg> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, size, 1.0);
    let n = (t * RATE) as usize;
    for i in 0..=n {
        let ti = i as f64 / RATE;
        scene.apply(view(size, level(ti), effects), ti);
    }
    scene.sample(t)
}

fn bench() {
    let fonts = fonts();
    let (device, queue) = gpu(true);
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
        for effects in [true, false] {
            let size = Size::new(w as f32, h as f32);
            // A loud moment, so glow and bloom are wide.
            let list = sample(&fonts, size, effects, 1.9);
            let target = Target::new(&device, w, h, vo_render::offscreen::FORMAT);
            let mut times = Vec::new();
            for i in 0..220 {
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
            println!(
                "glow_demo bench: {w}x{h} {} path: GPU {median:.3} ms median, {p95:.3} ms p95; pooled textures {:.2} MB",
                if effects { "effect" } else { "direct" },
                renderer.pool_bytes() as f64 / 1048576.0
            );
        }
    }
}

/// From process start to the first frame read back, for one scene at 600x300.
fn cold(effects: bool, start: Instant) {
    let fonts = fonts();
    let size = Size::new(600.0, 300.0);
    let list = sample(&fonts, size, effects, 1.9);
    let (device, queue) = gpu(false);
    let mut off = Offscreen::with_fonts(&device, &queue, fonts);
    let rgba = off.render_to_rgba(&list, [600, 300], 1.0);
    assert_eq!(rgba.len(), 600 * 300 * 4);
    println!(
        "glow_demo cold: {} scene, first frame read back {:.1} ms after start",
        if effects { "effect" } else { "direct" },
        start.elapsed().as_secs_f64() * 1e3
    );
}

#[cfg(feature = "wayland")]
fn live() {
    let fonts = fonts();
    let size = Size::new(960.0, 400.0);
    let spec = vo::OverlaySpec {
        size,
        placement: vo::Placement::Anchored { edges: vo::Edges::TOP, margin: [40, 0, 0, 0] },
        app_id: "vo-glow-demo".into(),
        ..vo::OverlaySpec::default()
    };
    let options = Options::<Msg> { fonts, ..Options::default() };
    if let Err(e) = vo::run_overlay_with(Demo { size, level: 0.0, effects: true }, spec, options) {
        eprintln!("glow_demo: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(feature = "wayland"))]
fn live() {
    eprintln!("glow_demo: --live needs the wayland feature");
    std::process::exit(1);
}

fn main() {
    let start = Instant::now();
    let mut args = std::env::args().skip(1);
    let (mut out, mut check) = (PathBuf::from("/var/tmp/claude/m4-fx"), false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--check" => check = true,
            "--bench" => return bench(),
            "--cold" => return cold(args.next().as_deref() != Some("direct"), start),
            "--live" => return live(),
            // Read by the host: open on the real desktop on purpose.
            "--real-display" => {}
            _ => panic!("unknown option {a}"),
        }
    }
    export(&out, check);
}
