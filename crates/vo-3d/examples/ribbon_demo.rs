//! Ribbons and cloth that twist and fold (`vo_3d::Ribbon3d`, EG-13), driven by a made-up
//! voice level so they swell while speech runs and settle in the pauses.
//!
//!   cargo run --release -p vo-3d --example ribbon_demo -- [--out DIR]
//!   cargo run --release -p vo-3d --example ribbon_demo -- --bench
//!
//! The default writes `DIR/ribbon_demo.png` (default DIR `/var/tmp/claude/fw-ribbon`): a silk
//! ribbon, a cloth strip, the ribbon under reduced motion and the ribbon on a light
//! background, at six moments, one row each. `--bench` prints the GPU time per frame
//! (timestamp queries) of one ribbon element filling a 600x300 frame; without timestamp
//! queries it says so and exits 0.

use std::path::PathBuf;
use std::sync::Arc;

use vo_3d::*;
use vo_render::offscreen::{write_png, FORMAT};
use vo_render::{Offscreen, Renderer, Target};
use vo_scene::element::Element;
use vo_scene::emitter::LevelTrack;
use vo_scene::geom::{Color, Size};
use vo_scene::gpu::GpuSource;
use vo_scene::{DisplayList, Scene};

/// A speech-like level, recorded as the targets a theme would push: syllables in bursts,
/// a pause every few seconds.
fn speech(until: f64) -> LevelTrack {
    let mut l = LevelTrack::default();
    let mut t = 0.0;
    while t < until {
        let syllable = (t as f32 * 9.0).sin().abs();
        let phrase = (0.5 + 0.5 * (t as f32 * 1.7).sin()).powf(0.7);
        let pause = if (t * 0.3).fract() > 0.75 { 0.0 } else { 1.0 };
        l.push(t, (syllable * phrase * pause).clamp(0.0, 1.0));
        t += 1.0 / 30.0;
    }
    l
}

fn rows() -> Vec<(&'static str, RibbonItem)> {
    let level = speech(20.0);
    let dark = RibbonLook { background: Background::Solid(Color::hex(0x07090f, 1.0)), ..RibbonLook::default() };
    let silk = RibbonItem { ribbon: Ribbon3d::default(), level: level.clone(), look: dark.clone() };
    vec![
        ("silk", silk.clone()),
        (
            "cloth",
            RibbonItem {
                ribbon: Ribbon3d::cloth(),
                look: RibbonLook {
                    front: Color::hex(0x9fe8ff, 1.0),
                    back: Color::hex(0x1f6a8a, 1.0),
                    fibres: 40.0,
                    fibre: 0.35,
                    ..dark.clone()
                },
                ..silk.clone()
            },
        ),
        ("reduced", RibbonItem { ribbon: Ribbon3d { reduced_motion: true, ..Ribbon3d::default() }, ..silk.clone() }),
        (
            "light",
            RibbonItem {
                look: RibbonLook {
                    front: Color::hex(0x2a4bd8, 1.0),
                    back: Color::hex(0xd84a7a, 1.0),
                    background: Background::Solid(Color::hex(0xf2efe8, 1.0)),
                    ..RibbonLook::default()
                },
                ..silk
            },
        ),
    ]
}

/// One ribbon element filling a frame of `size`.
fn view(src: &Arc<dyn GpuSource>, size: Size) -> Element<()> {
    Element::panel("root").size(size.w, size.h).child(Element::gpu("ribbon", src.clone()).size(size.w, size.h))
}

fn sample(src: &Arc<dyn GpuSource>, size: Size, t: f64) -> DisplayList {
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(view(src, size), 0.0);
    scene.sample(t)
}

fn gpu(timestamps: bool) -> Option<(wgpu::Device, wgpu::Queue)> {
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
    let features = if timestamps { vo_3d::timer::FEATURES } else { wgpu::Features::empty() };
    if !adapter.features().contains(features) {
        return None;
    }
    Some(
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("ribbon-demo"),
            required_features: features,
            ..Default::default()
        }))
        .expect("a GPU device"),
    )
}

fn bench() {
    let Some((device, queue)) = gpu(true) else {
        println!("ribbon_demo bench: this GPU has no timestamp queries; no timing");
        return;
    };
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
    let (w, h) = (600u32, 300u32);
    let size = Size::new(w as f32, h as f32);
    let target = Target::new(&device, w, h, FORMAT);
    for (name, item) in rows() {
        let src = item_source(item);
        let mut times = Vec::new();
        for i in 0..240 {
            let list = sample(&src, size, 1.0 + i as f64 / 60.0);
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
            if i >= 40 {
                times.push((ts[1] - ts[0]) as f64 * period / 1e6);
            }
        }
        times.sort_by(f64::total_cmp);
        let (median, p95) = (times[times.len() / 2], times[times.len() * 95 / 100]);
        println!("ribbon_demo bench: {name:7} {w}x{h}: {median:.3} ms median, {p95:.3} ms p95");
    }
}

fn item_source(item: RibbonItem) -> Arc<dyn GpuSource> {
    vo_3d::item(item)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("/var/tmp/claude/fw-ribbon");
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--bench" => return bench(),
            _ => panic!("unknown option {a}"),
        }
    }
    let (w, h) = (300u32, 150u32);
    let size = Size::new(w as f32, h as f32);
    let (device, queue) = gpu(false).expect("a GPU device");
    let mut off = Offscreen::new(&device, &queue);
    install(&mut off.renderer);
    let rows = rows();
    let (cols, gap) = (6u32, 6u32);
    let (sw, sh) = (cols * w + (cols + 1) * gap, rows.len() as u32 * h + (rows.len() as u32 + 1) * gap);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    for (r, (_, item)) in rows.into_iter().enumerate() {
        let src = item_source(item);
        for c in 0..cols {
            let t = 0.5 + 0.6 * c as f64;
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
    let path = out.join("ribbon_demo.png");
    write_png(&path, sw, sh, &sheet).unwrap();
    println!("ribbon_demo: wrote {}", path.display());
}
