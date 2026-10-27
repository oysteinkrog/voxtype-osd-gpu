//! The signal bus at work: a voice orb whose scale, hop and colour follow level, onsets and
//! centroid, a mel spectrogram as dots and a pitch contour, fed from 6 s of seeded speech
//! features at 100 Hz.
//!
//!   cargo run --release -p vo-dots --example signals_demo -- [--out DIR]
//!
//! Writes `DIR/signals_demo.mp4` (600 x 300 at 2x, 60 fps) and `DIR/signals-sheet.png`, and
//! prints CPU and GPU time per frame with the panel at 600 x 300 and at 1920 x 1080. DIR
//! defaults to `/var/tmp/claude/m4-dots/sig`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

use vo_core::signals::{Signals, SignalsCell};
use vo_dots::sample_speech;
use vo_dots::signals::signals_panel;
use vo_render::offscreen::write_png;
use vo_render::{Offscreen, Renderer, Target};
use vo_scene::geom::Size;
use vo_scene::Scene;

const SECS: f64 = 6.0;
const FPS: f64 = 60.0;
const SHEET_AT: [f64; 9] = [0.4, 0.9, 1.5, 2.1, 2.7, 3.3, 4.2, 5.4, 6.4];

fn cell() -> SignalsCell {
    SignalsCell::new(Signals::recorded(sample_speech(7, SECS), 0.0))
}

fn scene(size: Size, scale: f32) -> Scene {
    let mut s: Scene = Scene::new();
    s.set_viewport(0.0, size, scale);
    s.apply(signals_panel(cell(), (size.w, size.h)), 0.0);
    s
}

/// Median and 95th percentile.
fn stats(v: &mut [f64]) -> (f64, f64) {
    v.sort_by(f64::total_cmp);
    (v[v.len() / 2], v[(v.len() * 95 / 100).min(v.len() - 1)])
}

/// Rows of 3, over a mid grey so the corners read.
fn sheet(frames: &[Vec<u8>], w: u32, h: u32) -> (u32, u32, Vec<u8>) {
    let (gap, cols) = (8u32, 3u32);
    let rows = (frames.len() as u32).div_ceil(cols);
    let (sw, sh) = (cols * w + (cols + 1) * gap, rows * h + (rows + 1) * gap);
    let mut out = [0x3au8, 0x3a, 0x3a, 255].repeat((sw * sh) as usize);
    for (i, f) in frames.iter().enumerate() {
        let (ox, oy) = (gap + (i as u32 % cols) * (w + gap), gap + (i as u32 / cols) * (h + gap));
        for y in 0..h {
            for x in 0..w {
                let s = ((y * w + x) * 4) as usize;
                let d = (((oy + y) * sw + ox + x) * 4) as usize;
                let a = f[s + 3] as u32;
                for c in 0..3 {
                    out[d + c] = (f[s + c] as u32 + 0x3a * (255 - a) / 255) as u8;
                }
            }
        }
    }
    (sw, sh, out)
}

fn video_and_sheet(out: &std::path::Path) {
    let gpu = gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let (size, scale) = (Size::new(600.0, 300.0), 2.0);
    let (w, h) = (1200u32, 600u32);
    let s = scene(size, scale);
    let mp4 = out.join("signals_demo.mp4");
    let mut ff = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{w}x{h}")])
        .args(["-r", "60", "-i", "-", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "16"])
        .arg(&mp4)
        .stdin(Stdio::piped())
        .spawn()
        .expect("ffmpeg");
    let stdin = ff.stdin.as_mut().unwrap();
    let frames = ((SECS + 0.8) * FPS) as u32;
    let mut thumbs = Vec::new();
    for i in 0..frames {
        let t = i as f64 / FPS;
        let px = off.render_to_rgba(&s.sample(t), [w, h], scale);
        // Over black, as the overlay sits on a dark desk in the video.
        stdin.write_all(&px).expect("ffmpeg write");
        if SHEET_AT.iter().any(|&at| (at * FPS).round() as u32 == i) {
            thumbs.push(off.render_to_rgba(&s.sample(t), [600, 300], 1.0));
        }
    }
    drop(ff.stdin.take());
    ff.wait().expect("ffmpeg");
    let (sw, sh, px) = sheet(&thumbs, 600, 300);
    let path = out.join("signals-sheet.png");
    write_png(&path, sw, sh, &px).expect("write sheet");
    println!("signals_demo: {} and {} (at {SHEET_AT:?} s)", mp4.display(), path.display());
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    timestamps: bool,
}

fn gpu() -> Gpu {
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
        label: Some("signals-demo"),
        required_features: if timestamps { want } else { wgpu::Features::empty() },
        ..Default::default()
    }))
    .expect("no device");
    Gpu { device, queue, timestamps }
}

/// CPU sample time, record time and GPU time per frame over 240 frames of speech.
fn bench() {
    let g = gpu();
    let mut renderer = Renderer::new(&g.device, &g.queue);
    let qs = g.timestamps.then(|| {
        g.device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 })
    });
    let buf = |usage| {
        g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
    let resolve = buf(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buf(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    let period = g.queue.get_timestamp_period() as f64;
    println!("| panel | dots | sample ms (p50/p95) | record ms (p50/p95) | GPU ms (p50/p95) |");
    println!("|---|---|---|---|---|");
    for (w, h) in [(600u32, 300u32), (1920, 1080)] {
        let target = Target::new(&g.device, w, h, wgpu::TextureFormat::Rgba8UnormSrgb);
        let s = scene(Size::new(w as f32, h as f32), 1.0);
        let (mut sample, mut record, mut gpu_ms) = (Vec::new(), Vec::new(), Vec::new());
        let mut dots = 0;
        for i in 0..240 {
            let t = 1.0 + i as f64 / 60.0;
            let s0 = Instant::now();
            let list = s.sample(t);
            sample.push(s0.elapsed().as_secs_f64() * 1e3);
            dots = list.dots.len();
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
        println!("| {w}x{h} | {dots} | {s50:.3} / {s95:.3} | {r50:.3} / {r95:.3} | {gpu} |");
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("/var/tmp/claude/m4-dots/sig");
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            _ => panic!("unknown option {a}"),
        }
    }
    std::fs::create_dir_all(&out).expect("create the output directory");
    video_and_sheet(&out);
    bench();
}
