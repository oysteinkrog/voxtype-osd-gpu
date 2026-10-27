//! The raymarched hero (`vo_3d::hero`) over time: a liquid chrome blob (concept 05), an
//! aurora orb (01) and a jellyfish bell (15), each driven by a made-up voice level so it
//! swells and ripples while speech runs and settles in the pauses.
//!
//!   cargo run --release -p vo-3d --example hero_demo -- [--out DIR]
//!   cargo run --release -p vo-3d --example hero_demo -- --bench
//!
//! The default writes `DIR/hero_demo.png` (default DIR `/var/tmp/claude/fw-hero`): the three
//! heroes at six moments, one row each. `--bench` prints GPU time per frame (timestamp
//! queries) for each hero alone, in a box a third of the frame's height wide plus its halo,
//! in a 600x300 and a 1920x1080 frame, at 48 and at 24 march steps.

use std::path::PathBuf;
use std::sync::Arc;

use vo_3d::*;
use vo_render::offscreen::{write_png, FORMAT};
use vo_render::{Offscreen, Renderer, Target};
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::gpu::GpuSource;
use vo_scene::taffy::{AlignItems, JustifyContent};
use vo_scene::{DisplayList, Scene};

/// A speech-like level: syllables in bursts, with a pause every few seconds.
fn level(t: f64) -> f32 {
    let t = t as f32;
    let syllable = (t * 9.0).sin().abs();
    let phrase = (0.5 + 0.5 * (t * 1.7).sin()).powf(0.7);
    let pause = if (t * 0.3).fract() > 0.75 { 0.0 } else { 1.0 };
    (syllable * phrase * pause).clamp(0.0, 1.0)
}

/// The level smoothed the way a theme would ease it: a quick rise, a slow fall.
fn eased(t: f64) -> f32 {
    (0..12).map(|i| level(t - i as f64 * 0.03) * (1.0 - i as f32 / 12.0)).sum::<f32>() / 6.5
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Chrome,
    Aurora,
    Bell,
}

#[derive(Debug)]
struct Hero {
    kind: Kind,
    steps: u32,
}

impl HeroSource for Hero {
    fn frame(&self, t: f64, _size: Size) -> HeroFrame {
        let v = eased(t).min(1.0);
        let phase = t as f32;
        let f = match self.kind {
            Kind::Chrome => HeroFrame {
                shape: HeroShape::Blob { lobes: 5 },
                look: HeroLook::Chrome {
                    tint: Color::hex(0xdfe8ff, 1.0),
                    top: Color::hex(0x8ea4d8, 1.0),
                    bottom: Color::hex(0x06070c, 1.0),
                    light: Color::hex(0xffffff, 1.0),
                },
                stretch: [1.0, 0.5 + 0.12 * v, 0.8],
                wobble: 0.25 + 0.75 * v,
                phase,
                spin: 0.2 * phase,
                ..Default::default()
            },
            Kind::Aurora => HeroFrame {
                shape: HeroShape::Orb,
                look: HeroLook::Glow {
                    a: Color::hex(0x2b6cff, 1.0),
                    b: Color::hex(0x8a4dff, 1.0),
                    c: Color::hex(0x2de0c8, 1.0),
                    rim: Color::hex(0xd8c8ff, 1.0),
                    body: 0.85,
                    halo: 0.35 + 0.4 * v,
                    halo_width: 0.12,
                },
                stretch: [0.72 + 0.1 * v; 3],
                wobble: v,
                phase: 0.6 * phase,
                spin: 0.3 * phase,
                ..Default::default()
            },
            Kind::Bell => HeroFrame {
                shape: HeroShape::Bell,
                look: HeroLook::Glow {
                    a: Color::hex(0x1846ff, 1.0),
                    b: Color::hex(0x6a3cff, 1.0),
                    c: Color::hex(0x23c8ff, 1.0),
                    rim: Color::hex(0x9fe8ff, 1.0),
                    body: 0.35,
                    halo: 0.25 + 0.35 * v,
                    halo_width: 0.1,
                },
                stretch: [0.8; 3],
                wobble: 0.6 * v,
                phase,
                spin: 0.15 * phase,
                ..Default::default()
            },
        };
        HeroFrame { steps: self.steps, ..f }
    }
}

/// One hero centred in a frame of `size`, its box a third of the height wide.
fn view(h: &Arc<dyn GpuSource>, size: Size) -> Element<()> {
    let d = size.h * 0.55;
    Element::panel("root")
        .size(size.w, size.h)
        .fill(Color::hex(0x07090f, 1.0))
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
        .child(Element::gpu("hero", h.clone()).size(d, d))
}

fn sample(h: &Arc<dyn GpuSource>, size: Size, t: f64) -> DisplayList {
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(view(h, size), 0.0);
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
        label: Some("hero-demo"),
        required_features: wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
        ..Default::default()
    }))
    .expect("a GPU device")
}

const KINDS: [(&str, Kind); 3] = [("chrome", Kind::Chrome), ("aurora", Kind::Aurora), ("bell", Kind::Bell)];

fn bench() {
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
    for (name, kind) in KINDS {
        for steps in [48, 24] {
            let h = hero(Hero { kind, steps });
            let mut line = format!("hero_demo bench: {name:7} {steps:2} steps");
            for (w, ht) in [(600u32, 300u32), (1920, 1080)] {
                let size = Size::new(w as f32, ht as f32);
                let target = Target::new(&device, w, ht, FORMAT);
                let mut times = Vec::new();
                for i in 0..140 {
                    let list = sample(&h, size, 1.0 + i as f64 / 60.0);
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
                line += &format!("  {w}x{ht}: {median:.3} ms median, {p95:.3} ms p95;");
            }
            println!("{line}");
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("/var/tmp/claude/fw-hero");
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--bench" => return bench(),
            _ => panic!("unknown option {a}"),
        }
    }
    let (w, h) = (300u32, 200u32);
    let size = Size::new(w as f32, h as f32);
    let (device, queue) = gpu();
    let mut off = Offscreen::new(&device, &queue);
    install(&mut off.renderer);
    let (cols, gap) = (6u32, 6u32);
    let rows = KINDS.len() as u32;
    let (sw, sh) = (cols * w + (cols + 1) * gap, rows * h + (rows + 1) * gap);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    for (r, (_, kind)) in KINDS.iter().enumerate() {
        let src = hero(Hero { kind: *kind, steps: 48 });
        for c in 0..cols {
            let t = 0.4 + 0.35 * c as f64;
            let rgba = off.render_to_rgba(&sample(&src, size, t), [w, h], 1.0);
            let (x0, y0) = (gap + c * (w + gap), gap + r as u32 * (h + gap));
            for y in 0..h {
                let s = (y * w * 4) as usize;
                let d = (((y0 + y) * sw + x0) * 4) as usize;
                sheet[d..d + (w * 4) as usize].copy_from_slice(&rgba[s..s + (w * 4) as usize]);
            }
        }
    }
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join("hero_demo.png");
    write_png(&path, sw, sh, &sheet).unwrap();
    println!("hero_demo: wrote {}", path.display());
}
