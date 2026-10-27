//! Type demo (WP7, FX-003, FX-004): built-in fonts by name and font fallback.
//!
//! - Top: a mecha-style header in Barlow Condensed Bold whose kanji fall back to the
//!   Japanese label face, and a body line with Norwegian letters.
//! - Below: one line in every built-in font.
//!
//!   cargo run --release -p vo --example type_demo -- [--out DIR] [--bench]
//!
//! Writes `DIR/type-demo.png` (default DIR `/var/tmp/claude/fw-fonts`). `--bench` times the
//! frame on the GPU at 600x300 and 1920x1080.

use std::path::PathBuf;

use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::{DisplayList, Scene};
use vo::{FontRegistry, ShapedLayout};
use vo_render::offscreen::write_png;
use vo_render::{Offscreen, Renderer, Target};

const SURFACE: Size = Size::new(600.0, 300.0);

const SPECIMEN: [(&str, &str); 9] = [
    ("sans", "sans: Noto Sans, the quick brown fox"),
    ("serif", "serif: Noto Serif, the quick brown fox"),
    ("mono", "mono: JetBrains Mono 0123456789"),
    ("grotesk", "grotesk: Inter, the quick brown fox"),
    ("condensed", "CONDENSED: BARLOW CONDENSED MEDIUM"),
    ("condensed-bold", "CONDENSED-BOLD: BARLOW CONDENSED BOLD"),
    ("thin", "thin: Barlow Thin, the quick brown fox"),
    ("jp-labels", "jp-labels: 音声入力 テキストに変換中"),
    ("jp-labels-bold", "jp-labels-bold: 作戦継続中 未来の都市"),
];

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.set_default_fallbacks(&[fonts.asset("sans").unwrap()]);
    for (name, _) in SPECIMEN {
        fonts.asset(name).unwrap_or_else(|e| panic!("{e}"));
    }
    let jp = fonts.asset("jp-labels-bold").unwrap();
    for name in ["condensed", "condensed-bold"] {
        fonts.set_fallbacks(fonts.asset(name).unwrap(), &[jp]);
    }
    fonts
}

fn view(fonts: &FontRegistry) -> Element<()> {
    let orange = Color::hex(0xff8a1f, 1.0);
    let style = |name: &str, size: f32| TextStyle::new(fonts.asset(name).unwrap(), size, size * 1.25);
    let mut col = Element::column("col")
        .pad(14.0)
        .gap(3.0)
        .child(Element::label("head", "音声入力 VOICE DICTATION").text_style(style("condensed-bold", 32.0)).color(orange))
        .child(
            Element::label("body", "録音中 LISTENING · Blåbærsyltetøy på Ærø")
                .text_style(style("condensed", 20.0))
                .color(orange),
        );
    for (name, line) in SPECIMEN {
        col = col.child(Element::label(name, line).text_style(style(name, 15.0)).color(Color::hex(0xe8ebf0, 1.0)));
    }
    Element::panel("bg").size(SURFACE.w, SURFACE.h).fill(Color::hex(0x0b0806, 1.0)).child(col)
}

fn sample(fonts: &FontRegistry, size: Size, scale: f32) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, size, scale);
    scene.apply(view(fonts), 0.0);
    scene.sample(10.0)
}

/// A headless Vulkan device with timestamp queries.
fn device() -> (wgpu::Device, wgpu::Queue) {
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
        label: Some("type-demo"),
        required_features: wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
        ..Default::default()
    }))
    .expect("a GPU device")
}

/// GPU time per frame at 600x300 and at 1920x1080 (the same layout at 3.2 times the scale).
fn bench() {
    let fonts = fonts();
    let (device, queue) = device();
    let period = queue.get_timestamp_period() as f64;
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let buffer = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
    let resolve = buffer(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buffer(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    for (w, h, scale) in [(600u32, 300u32, 1.0f32), (1920, 1080, 3.2)] {
        let mut renderer = Renderer::with_fonts(&device, &queue, fonts.clone());
        let target = Target::new(&device, w, h, vo_render::offscreen::FORMAT);
        let list = sample(&fonts, Size::new(w as f32 / scale, h as f32 / scale), scale);
        let mut times = Vec::new();
        for i in 0..150 {
            let mut start = device.create_command_encoder(&Default::default());
            start.write_timestamp(&qs, 0);
            queue.submit([start.finish()]);
            let mut enc = device.create_command_encoder(&Default::default());
            renderer.encode(&list, scale, &target, &mut enc);
            enc.write_timestamp(&qs, 1);
            enc.resolve_query_set(&qs, 0..2, &resolve, 0);
            enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, 16);
            queue.submit([enc.finish()]);
            read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let ts = {
                let bytes = read.slice(..).get_mapped_range().unwrap();
                let at = |i: usize| u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap());
                (at(0), at(1))
            };
            read.unmap();
            // The first frames fill the glyph atlas.
            if i >= 30 {
                times.push((ts.1 - ts.0) as f64 * period / 1e6);
            }
        }
        times.sort_by(f64::total_cmp);
        println!(
            "type_demo bench: {w}x{h}: {} glyphs, GPU {:.3} ms median, {:.3} ms p95 per frame",
            list.glyphs.len(),
            times[times.len() / 2],
            times[times.len() * 95 / 100]
        );
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("/var/tmp/claude/fw-fonts");
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--bench" => return bench(),
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = fonts();
    let scale = 2.0;
    let list = sample(&fonts, SURFACE, scale);
    let (device, queue) = device();
    let mut off = Offscreen::with_fonts(&device, &queue, fonts);
    let (w, h) = ((SURFACE.w * scale) as u32, (SURFACE.h * scale) as u32);
    let rgba = off.render_to_rgba(&list, [w, h], scale);
    std::fs::create_dir_all(&out).expect("create the output directory");
    let path = out.join("type-demo.png");
    write_png(&path, w, h, &rgba).expect("write the png");
    println!("{}", path.display());
}
