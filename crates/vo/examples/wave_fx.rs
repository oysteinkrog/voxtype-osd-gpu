//! Waveforms and meters (WP8, WP11): bars v2, the line trace, ribbon strands and segment
//! meters, over a made-up speech level.
//!
//!   cargo run --release -p vo --example wave_fx -- [--out DIR] [--t SECONDS]
//!   cargo run --release -p vo --example wave_fx -- --bench
//!
//! The default writes `DIR/wave_fx.png` (default DIR `/var/tmp/claude/fw-wave`): one panel
//! of each piece at time `--t` (default 3.2). `--bench` prints GPU time per frame (timestamp
//! queries) for each piece alone, filling a 600x300 and a 1920x1080 frame.

use std::path::PathBuf;
use std::sync::Arc;

use vo::scene::bars::{Anchor, BarColour, BarLayout, Bars, Core, Taper};
use vo::scene::element::Element;
use vo::scene::geom::{Color, Size};
use vo::scene::levels::{Levels, Stops};
use vo::scene::ribbon::Ribbon;
use vo::scene::segments::{SegmentLayout, Segments};
use vo::scene::trace::{Trace, TraceMode};
use vo::scene::{DisplayList, Scene};
use vo_render::offscreen::{write_png, FORMAT};
use vo_render::{Offscreen, Renderer, Target};

/// Speech-like levels at 100 a second: syllables in bursts with short pauses.
fn speech(n: usize) -> Arc<[f32]> {
    (0..n)
        .map(|i| {
            let t = i as f32 / 100.0;
            let syllable = (t * 21.0).sin().abs();
            let phrase = (0.5 + 0.5 * (t * 2.3).sin()).powf(0.7);
            let pause = if (t * 0.9).fract() > 0.82 { 0.05 } else { 1.0 };
            (syllable * phrase * pause * 0.95).clamp(0.0, 1.0)
        })
        .collect()
}

fn levels() -> Levels {
    Levels::new(speech(400), 4.0)
}

fn bars_of(l: &Levels) -> Bars {
    Bars { levels: l.levels.clone(), newest_at: l.newest_at, ..Bars::default() }
}

/// Concept 23: thin orange bars with a white-hot core, glow and dotted tails.
fn hot_bars() -> Bars {
    let orange = Color::hex(0xff5a00, 1.0);
    Bars {
        count: 72,
        slot: 0.03,
        width: 0.5,
        colour: BarColour::ByLevel(Stops::new(&[
            (0.0, Color::hex(0xff3d00, 1.0)),
            (0.6, orange),
            (1.0, Color::hex(0xffc04d, 1.0)),
        ])),
        core: Some(Core::default()),
        glow: Some((5.0, 0.8)),
        taper: Some(Taper::default()),
        layout: BarLayout::Mirror,
        ..bars_of(&levels())
    }
}

/// Concepts 07 and 12: hairlines on a baseline, accent in the middle.
fn hair_bars() -> Bars {
    Bars {
        count: 90,
        hairline: true,
        baseline: Some(Color::hex(0x6b7280, 0.6)),
        accent: Color::hex(0xf43f5e, 1.0),
        rest: Color::hex(0x9ca3af, 0.8),
        colour: BarColour::Centre { span: 0.25, soft: 0.15 },
        ..bars_of(&levels())
    }
}

/// Colour stops across, standing on a reflection.
fn mirror_bars() -> Bars {
    Bars {
        count: 40,
        anchor: Anchor::Bottom,
        reflection: 0.35,
        colour: BarColour::Across(Stops::new(&[
            (0.0, Color::hex(0x22d3ee, 1.0)),
            (0.5, Color::hex(0x6366f1, 1.0)),
            (1.0, Color::hex(0xd946ef, 1.0)),
        ])),
        ..bars_of(&levels())
    }
}

/// Concept 02: bars round a ring.
fn ring_bars() -> Bars {
    Bars {
        count: 64,
        slot: 0.03,
        layout: BarLayout::Radial { inner: 0.45 },
        anchor: Anchor::Bottom,
        accent: Color::hex(0x38bdf8, 1.0),
        glow: Some((3.0, 0.7)),
        core: Some(Core { color: Color::hex(0xe0f7ff, 1.0), ..Core::default() }),
        ..bars_of(&levels())
    }
}

/// Concept 16: a glowing trace with ghosts on a fading horizon.
fn live_trace() -> Trace {
    Trace { levels: levels(), ..Trace::default() }
}

/// Concept 18: a dense green oscilloscope over the history.
fn scope_trace() -> Trace {
    let green = Color::hex(0x22e57a, 1.0);
    Trace {
        levels: levels(),
        mode: TraceMode::History { window: 2.5 },
        cycles: 70.0,
        speed: 3.0,
        detail: 0.45,
        floor: 0.03,
        ghosts: 1,
        ghost_phase: 0.25,
        fade: 0.04,
        width: 1.4,
        glow: 4.0,
        color: green,
        core: None,
        horizon: Some(green.fade(0.5)),
        step: 1.0,
        ..Trace::default()
    }
}

fn ribbon() -> Ribbon {
    Ribbon { levels: levels(), ..Ribbon::default() }
}

fn meters() -> [Segments; 3] {
    let cyan = Color::hex(0x38bdf8, 1.0);
    let orange = Color::hex(0xff6a00, 1.0);
    let row = Segments { levels: levels(), cells: 20, ..Segments::default() };
    let column = Segments {
        cells: 10,
        layout: SegmentLayout::Column,
        on: Stops::two(orange.fade(0.8), orange),
        off: orange.fade(0.12),
        glow: Some((4.0, 0.6)),
        ..row.clone()
    };
    let arc = Segments {
        cells: 36,
        layout: SegmentLayout::Arc { start: 2.4, sweep: 1.2, inner: 0.82, width: 1.6 },
        on: Stops::two(cyan.fade(0.6), cyan),
        off: cyan.fade(0.12),
        clip: None,
        ..row.clone()
    };
    [row, column, arc]
}

/// One piece by name, filling `size`.
fn piece(name: &str) -> Element<()> {
    match name {
        "bars-hot" => Element::bars("p", hot_bars()),
        "bars-hairline" => Element::bars("p", hair_bars()),
        "bars-mirror" => Element::bars("p", mirror_bars()),
        "bars-ring" => Element::bars("p", ring_bars()),
        "trace-live" => Element::trace("p", live_trace()),
        "trace-scope" => Element::trace("p", scope_trace()),
        "ribbon" => Element::ribbon("p", ribbon()),
        "meter-row" => Element::segments("p", meters()[0].clone()),
        "meter-column" => Element::segments("p", meters()[1].clone()),
        "meter-arc" => Element::segments("p", meters()[2].clone()),
        _ => panic!("unknown piece {name}"),
    }
}

const PIECES: [&str; 10] = [
    "bars-hot",
    "bars-hairline",
    "bars-mirror",
    "bars-ring",
    "trace-live",
    "trace-scope",
    "ribbon",
    "meter-row",
    "meter-column",
    "meter-arc",
];

fn sample(root: Element<()>, size: Size, t: f64) -> DisplayList {
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(root, 0.0);
    scene.sample(t)
}

/// All pieces on one sheet, panels of 300 x 110, three to a row.
fn sheet(t: f64) -> (DisplayList, Size) {
    let (w, h, pad) = (300.0, 110.0, 12.0);
    let rows = PIECES.len().div_ceil(3);
    let size = Size::new(3.0 * w + 4.0 * pad, rows as f32 * h + (rows as f32 + 1.0) * pad);
    let mut root = Element::panel("sheet").pad(pad).gap(pad).fill(Color::hex(0x07080c, 1.0));
    for r in 0..rows {
        let mut row = Element::row(format!("r{r}")).gap(pad);
        for c in 0..3 {
            let Some(&name) = PIECES.get(r * 3 + c) else { continue };
            let (pw, ph) = if name == "meter-row" {
                (w - 40.0, 14.0)
            } else if name == "meter-column" {
                (16.0, h - 20.0)
            } else {
                (w - 20.0, h - 20.0)
            };
            let cell = Element::panel(format!("c{c}"))
                .fill(Color::hex(0x10131a, 1.0))
                .radius(8.0)
                .size(w, h)
                .align_items(vo::scene::taffy::prelude::AlignItems::CENTER)
                .justify(vo::scene::taffy::prelude::JustifyContent::CENTER)
                .child(piece(name).size(pw, ph));
            row = row.child(cell);
        }
        root = root.child(row);
    }
    (sample(root, size, t), size)
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
        label: Some("wave-fx"),
        required_features: wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
        ..Default::default()
    }))
    .expect("a GPU device")
}

fn bench() {
    let (device, queue) = gpu();
    let period = queue.get_timestamp_period() as f64;
    let mut renderer = Renderer::new(&device, &queue);
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let buf = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
    let resolve = buf(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buf(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    for name in PIECES {
        let mut line = format!("wave_fx bench: {name:14}");
        for (w, h) in [(600u32, 300u32), (1920, 1080)] {
            let size = Size::new(w as f32, h as f32);
            let (pw, ph) = match name {
                "meter-row" => (size.w * 0.8, size.h * 0.06),
                "meter-column" => (size.w * 0.03, size.h * 0.8),
                _ => (size.w, size.h),
            };
            let root = Element::column("root").size(size.w, size.h).child(piece(name).size(pw, ph));
            let list = sample(root, size, 3.2);
            let target = Target::new(&device, w, h, FORMAT);
            let mut times = Vec::new();
            for i in 0..120 {
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
                if i >= 20 {
                    times.push((ts[1] - ts[0]) as f64 * period / 1e6);
                }
            }
            times.sort_by(f64::total_cmp);
            let (median, p95) = (times[times.len() / 2], times[times.len() * 95 / 100]);
            line += &format!("  {w}x{h}: {median:.3} ms median, {p95:.3} ms p95;");
        }
        println!(
            "{line} {} shapes, {} strokes",
            {
                let s = Size::new(600.0, 300.0);
                sample(Element::column("root").child(piece(name).size(s.w, s.h)), s, 3.2).shapes.len()
            },
            {
                let s = Size::new(600.0, 300.0);
                sample(Element::column("root").child(piece(name).size(s.w, s.h)), s, 3.2).vectors.len()
            }
        );
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut t) = (PathBuf::from("/var/tmp/claude/fw-wave"), 3.2);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--t" => t = args.next().and_then(|s| s.parse().ok()).expect("--t needs seconds"),
            "--bench" => return bench(),
            _ => panic!("unknown option {a}"),
        }
    }
    let (list, size) = sheet(t);
    let (device, queue) = gpu();
    let mut off = Offscreen::new(&device, &queue);
    let [w, h] = [size.w.ceil() as u32, size.h.ceil() as u32];
    let rgba = off.render_to_rgba(&list, [w, h], 1.0);
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join("wave_fx.png");
    write_png(&path, w, h, &rgba).unwrap();
    println!("wave_fx: wrote {}", path.display());
}
