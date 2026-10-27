//! Shared by the vo-audio examples: a GPU with timestamps, frames from a file, a contact sheet
//! and the per-frame cost table.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use vo_core::signals::SignalFrame;
use vo_render::offscreen::write_png;
use vo_render::{Renderer, Target};
use vo_scene::geom::Size;
use vo_scene::Scene;

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    timestamps: bool,
}

pub fn gpu() -> Gpu {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("no Vulkan adapter");
    let want = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    let timestamps = adapter.features().contains(want);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("vo-audio-example"),
        required_features: if timestamps { want } else { wgpu::Features::empty() },
        ..Default::default()
    }))
    .expect("no device");
    Gpu { device, queue, timestamps }
}

/// `--out DIR` and `--frames FILE` from the command line.
pub fn args(default_out: &str) -> (PathBuf, Option<PathBuf>) {
    let mut args = std::env::args().skip(1);
    let (mut out, mut frames) = (PathBuf::from(default_out), None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--frames" => frames = Some(PathBuf::from(args.next().expect("--frames needs a file"))),
            _ => panic!("unknown option {a}"),
        }
    }
    std::fs::create_dir_all(&out).expect("create the output directory");
    (out, frames)
}

/// `{"frames": [SignalFrame, ...], "seams": [seconds, ...]}`, or seeded speech without seams.
pub fn load(path: Option<&PathBuf>) -> (Vec<SignalFrame>, Vec<f64>) {
    let Some(path) = path else { return (vo_dots::sample_speech(7, 9.0), Vec::new()) };
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).expect("read --frames")).expect("json");
    let frames = serde_json::from_value(v["frames"].clone()).expect("frames");
    let seams = serde_json::from_value(v["seams"].clone()).unwrap_or_default();
    (frames, seams)
}

/// The `"words"` of the same file as `[text, start, end]`, or stand-in words from the level
/// when there are none.
pub fn load_words(path: Option<&PathBuf>, frames: &[SignalFrame]) -> Vec<vo_audio::TimedWord> {
    let listed: Vec<(String, f32, f32)> = path
        .and_then(|p| {
            let v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).ok()?).ok()?;
            serde_json::from_value(v["words"].clone()).ok()
        })
        .unwrap_or_default();
    if !listed.is_empty() {
        return listed
            .into_iter()
            .enumerate()
            .map(|(i, (text, start, end))| vo_audio::TimedWord { id: i as u32, text, start, end })
            .collect();
    }
    const SAY: [&str; 12] =
        ["Book", "the", "train", "to", "Bergen", "for", "Thursday", "morning,", "and", "pack", "the", "folder."];
    vo_audio::level_spans(frames, -45.0, 0.06)
        .into_iter()
        .enumerate()
        .map(|(i, (s, e))| vo_audio::TimedWord {
            id: i as u32,
            text: SAY[i % SAY.len()].into(),
            start: s as f32,
            end: e as f32,
        })
        .collect()
}

/// Premultiplied frames of one size stacked in a column over a mid grey.
pub fn sheet(path: &Path, frames: &[Vec<u8>], w: u32, h: u32) {
    let gap = 8u32;
    let (sw, sh) = (w + 2 * gap, frames.len() as u32 * (h + gap) + gap);
    let mut px = [0x3au8, 0x3a, 0x3a, 255].repeat((sw * sh) as usize);
    for (i, img) in frames.iter().enumerate() {
        let oy = gap + i as u32 * (h + gap);
        for y in 0..h {
            for x in 0..w {
                let s = ((y * w + x) * 4) as usize;
                let d = (((oy + y) * sw + gap + x) * 4) as usize;
                let a = img[s + 3] as u32;
                for c in 0..3 {
                    px[d + c] = (img[s + c] as u32 + 0x3a * (255 - a) / 255) as u8;
                }
            }
        }
    }
    write_png(path, sw, sh, &px).expect("write sheet");
}

fn stats(v: &mut [f64]) -> (f64, f64) {
    v.sort_by(f64::total_cmp);
    (v[v.len() / 2], v[(v.len() * 95 / 100).min(v.len() - 1)])
}

/// Prints sample, encode and GPU time per frame (p50 / p95 over 240 frames from `t0`) for the
/// scene `make` builds at 600 x 300 and at 1920 x 1080.
pub fn bench(g: &Gpu, setup: impl Fn(&mut Renderer), make: impl Fn(Size) -> Scene, t0: f64) {
    bench_with_fonts(g, vo_scene::font::FontRegistry::new(), setup, make, t0)
}

/// [`bench`] with a renderer that shares the scene's fonts, for scenes with text.
pub fn bench_with_fonts(
    g: &Gpu,
    fonts: vo_scene::font::FontRegistry,
    setup: impl Fn(&mut Renderer),
    make: impl Fn(Size) -> Scene,
    t0: f64,
) {
    let mut renderer = Renderer::with_fonts(&g.device, &g.queue, fonts);
    setup(&mut renderer);
    let qs = g.timestamps.then(|| {
        g.device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 })
    });
    let buf = |usage| {
        g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
    let resolve = buf(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buf(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    let period = g.queue.get_timestamp_period() as f64;
    println!("| surface | sample ms (p50/p95) | encode ms (p50/p95) | GPU ms (p50/p95) |");
    println!("|---|---|---|---|");
    for (w, h) in [(600u32, 300u32), (1920, 1080)] {
        let target = Target::new(&g.device, w, h, wgpu::TextureFormat::Rgba8UnormSrgb);
        let s = make(Size::new(w as f32, h as f32));
        let (mut sample, mut record, mut gpu_ms) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..240 {
            let t = t0 + i as f64 / 60.0;
            let s0 = Instant::now();
            let list = s.sample(t);
            sample.push(s0.elapsed().as_secs_f64() * 1e3);
            let r0 = Instant::now();
            let mut enc = g.device.create_command_encoder(&Default::default());
            if let Some(q) = &qs {
                enc.write_timestamp(q, 0);
            }
            renderer.encode(&list, 1.0, &target, &mut enc);
            if let Some(q) = &qs {
                enc.write_timestamp(q, 1);
                enc.resolve_query_set(q, 0..2, &resolve, 0);
                enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, 16);
            }
            g.queue.submit([enc.finish()]);
            record.push(r0.elapsed().as_secs_f64() * 1e3);
            if qs.is_some() {
                read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
                g.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                {
                    let data = read.slice(..).get_mapped_range().expect("map");
                    let ts = |i: usize| u64::from_le_bytes(data[i * 8..i * 8 + 8].try_into().unwrap());
                    gpu_ms.push(ts(1).wrapping_sub(ts(0)) as f64 * period / 1e6);
                }
                read.unmap();
            }
        }
        let (s50, s95) = stats(&mut sample);
        let (r50, r95) = stats(&mut record);
        let gpu = if gpu_ms.is_empty() {
            "n/a".to_string()
        } else {
            let (g50, g95) = stats(&mut gpu_ms);
            format!("{g50:.3} / {g95:.3}")
        };
        println!("| {w}x{h} | {s50:.3} / {s95:.3} | {r50:.3} / {r95:.3} | {gpu} |");
    }
}
