//! The grid floor (`vo_3d::grid_floor`, FX-152) as in a synthwave popup (concept 04): the
//! floor scrolls toward the viewer under a waveform of neon bars sitting on the horizon, with
//! a pool of light that swells with the voice level.
//!
//!   cargo run --release -p vo-3d --example grid_demo -- [--out DIR]
//!   cargo run --release -p vo-3d --example grid_demo -- --bench
//!
//! The default writes `DIR/grid_demo.png` (default DIR `/var/tmp/claude/fw-grid`): six frames
//! over two seconds. `--bench` prints GPU time per frame (timestamp queries) for the floor
//! alone, filling a 600x300 and a 1920x1080 frame.

use std::path::PathBuf;
use std::sync::Arc;

use vo_3d::*;
use vo_render::offscreen::{write_png, FORMAT};
use vo_render::{Offscreen, Renderer, Target};
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::FontRegistry;
use vo_scene::geom::{Color, Size};
use vo_scene::gpu::GpuSource;
use vo_scene::taffy::{AlignItems, JustifyContent};
use vo_scene::text::ShapedLayout;
use vo_scene::{DisplayList, Scene};

const MAGENTA: u32 = 0xff3ad7;
const CYAN: u32 = 0x35c8ff;

/// A speech-like level.
fn level(t: f64) -> f32 {
    let t = t as f32;
    ((t * 9.0).sin().abs() * (0.5 + 0.5 * (t * 1.7).sin()).powf(0.7)).clamp(0.0, 1.0)
}

#[derive(Debug)]
struct Floor;

impl GridSource for Floor {
    fn frame(&self, t: f64, _size: Size) -> GridFrame {
        let v = level(t);
        GridFrame {
            horizon: 0.42,
            scroll: 90.0 * t as f32,
            pool: GridPool { x: 0.0, depth: 700.0, radius: 300.0, strength: 0.4 + 0.8 * v },
            horizon_glow: (12.0, 0.6 + 0.4 * v),
            ..Default::default()
        }
    }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.set_default_fallbacks(&[fonts.asset("sans").unwrap()]);
    fonts
}

/// Neon bars standing on the horizon, magenta to cyan, heights from the level.
fn bars(t: f64, k: f32) -> Element<()> {
    let n = 41;
    let mut row = Element::row("bars").gap(5.0 * k).align_items(AlignItems::END);
    for i in 0..n {
        let x = i as f32 / (n - 1) as f32;
        let wave = level(t - 0.02 * (i as f64 - 20.0).abs());
        let h = (4.0 + 70.0 * wave * (1.0 - (2.0 * x - 1.0).abs()).powf(0.6)) * k;
        let (a, b) = (Color::hex(MAGENTA, 1.0), Color::hex(CYAN, 1.0));
        let c = Color::rgba(a.r + (b.r - a.r) * x, a.g + (b.g - a.g) * x, a.b + (b.b - a.b) * x, 1.0);
        row = row.child(Element::panel(format!("b{i}")).size(3.0 * k, h).radius(1.5 * k).fill(c).glow(c, 6.0 * k, 0.7));
    }
    row
}

fn view(floor: &Arc<dyn GpuSource>, size: Size, t: f64) -> Element<()> {
    let k = size.h / 300.0;
    Element::panel("root")
        .size(size.w, size.h)
        .fill(Color::hex(0x07041a, 1.0))
        .child(Element::gpu("floor", floor.clone()).size(size.w, size.h))
        .child(
            Element::column("over")
                .size(size.w, size.h)
                .margin_edges(-size.h, 0.0, 0.0, 0.0)
                .align_items(AlignItems::CENTER)
                .justify(JustifyContent::START)
                .child(Element::panel("gap").size(1.0, 0.42 * size.h - 78.0 * k))
                .child(bars(t, k))
                .child(
                    Element::label("words", "so the plan for tomorrow")
                        .text_style(TextStyle::new(0, 30.0 * k, 36.0 * k))
                        .color(Color::hex(0xffffff, 1.0))
                        .glow(Color::hex(0xb070ff, 1.0), 8.0 * k, 0.6)
                        .margin_edges(60.0 * k, 0.0, 0.0, 0.0),
                ),
        )
}

fn sample(fonts: &FontRegistry, floor: &Arc<dyn GpuSource>, size: Size, t: f64) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(view(floor, size, t), 0.0);
    scene.sample(t)
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
        label: Some("grid-demo"),
        required_features: wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
        ..Default::default()
    }))
    .expect("a GPU device")
}

fn bench(floor: Arc<dyn GpuSource>) {
    let (device, queue) = gpu();
    let period = queue.get_timestamp_period() as f64;
    let mut renderer = Renderer::new(&device, &queue);
    install(&mut renderer);
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let buf = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
    let resolve = buf(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buf(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    let mut line = String::from("grid_demo bench: floor alone");
    for (w, h) in [(600u32, 300u32), (1920, 1080)] {
        let size = Size::new(w as f32, h as f32);
        let target = Target::new(&device, w, h, FORMAT);
        let mut times = Vec::new();
        for i in 0..140 {
            let root = Element::panel("root")
                .size(size.w, size.h)
                .child(Element::gpu("floor", floor.clone()).size(size.w, size.h));
            let mut scene: Scene<()> = Scene::new();
            scene.set_viewport(0.0, size, 1.0);
            scene.apply(root, 0.0);
            let list = scene.sample(i as f64 / 60.0);
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
    println!("{line}");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("/var/tmp/claude/fw-grid");
    let floor = grid_floor(Floor);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--bench" => return bench(floor),
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = fonts();
    let (w, h) = (600u32, 300u32);
    let size = Size::new(w as f32, h as f32);
    let (device, queue) = gpu();
    let mut off = Offscreen::with_fonts(&device, &queue, fonts.clone());
    install(&mut off.renderer);
    let (cols, rows, gap) = (3u32, 2u32, 8u32);
    let (sw, sh) = (cols * w + (cols + 1) * gap, rows * h + (rows + 1) * gap);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    for i in 0..cols * rows {
        let t = 0.3 + 0.35 * i as f64;
        let rgba = off.render_to_rgba(&sample(&fonts, &floor, size, t), [w, h], 1.0);
        let (x0, y0) = (gap + (i % cols) * (w + gap), gap + (i / cols) * (h + gap));
        for y in 0..h {
            let s = (y * w * 4) as usize;
            let d = (((y0 + y) * sw + x0) * 4) as usize;
            sheet[d..d + (w * 4) as usize].copy_from_slice(&rgba[s..s + (w * 4) as usize]);
        }
    }
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join("grid_demo.png");
    write_png(&path, sw, sh, &sheet).unwrap();
    println!("grid_demo: wrote {}", path.display());
}
