//! Dot grid cost at 2k, 10k and 50k dots: CPU time to sample the scene and record the frame,
//! GPU time per frame from timestamp queries, and memory.
//!
//! `just bench dots_bench` (release). Two cases per size: every dot in flight (each frame
//! samples every spring) and every dot at rest. Prints a table.

use std::sync::Arc;
use std::time::Instant;

use vo_core::hash::rand_f32;
use vo_dots::{DotField, Dots, Look, Springs};
use vo_render::{Renderer, Target};
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::Scene;

const FRAMES: u32 = 240;
const PITCH: f32 = 4.0;

fn field(cols: u32, rows: u32) -> DotField {
    let mut f = DotField::new(0.0, Springs::default());
    for y in 0..rows {
        for x in 0..cols {
            let r = |k: u32| rand_f32(3, x, y, k);
            let look = Look {
                x: x as f32,
                y: y as f32,
                size: 0.4 + 0.4 * r(0),
                bright: 0.4 + 0.6 * r(1),
                color: Color::rgba(r(2), 0.8, 1.0, 1.0),
            };
            let from = Look::new(x as f32, (rows / 2) as f32, 0.1, Color::WHITE);
            f.retarget(0.0, y * cols + x, look, 0.002 * x as f64, Some(from));
        }
    }
    f
}

fn rss_kb() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)?.parse().ok()).unwrap_or(0)
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
        label: Some("dots-bench"),
        required_features: if timestamps { want } else { wgpu::Features::empty() },
        ..Default::default()
    }))
    .expect("no device");
    eprintln!("adapter: {}", adapter.get_info().name);
    Gpu { device, queue, timestamps }
}

/// Median and 95th percentile.
fn stats(v: &mut [f64]) -> (f64, f64) {
    v.sort_by(f64::total_cmp);
    (v[v.len() / 2], v[(v.len() * 95 / 100).min(v.len() - 1)])
}

fn main() {
    let g = gpu();
    let (w, h) = (1920u32, 1080u32);
    let target = Target::new(&g.device, w, h, wgpu::TextureFormat::Rgba8UnormSrgb);
    let mut renderer = Renderer::new(&g.device, &g.queue);
    let qs = g.timestamps.then(|| {
        g.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: None,
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        })
    });
    let resolve = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let period = g.queue.get_timestamp_period() as f64;

    println!(
        "| dots | case | build ms | sample ms (p50/p95) | record ms (p50/p95) | GPU ms (p50/p95) | field MB | dot buffer MB | RSS MB |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");
    for (cols, rows) in [(100u32, 20u32), (200, 50), (400, 125)] {
        let n = cols * rows;
        let rss0 = rss_kb();
        let t0 = Instant::now();
        let f = Arc::new(field(cols, rows));
        let build = t0.elapsed().as_secs_f64() * 1e3;
        let rest = f.rest_at();
        let mut scene: Scene = Scene::new();
        scene.set_viewport(0.0, Size::new(w as f32, h as f32), 1.0);
        scene.apply(
            Element::column("root").pad(16.0).child(
                Element::dots("grid", Dots::new(f.clone(), PITCH)).size(cols as f32 * PITCH, rows as f32 * PITCH),
            ),
            0.0,
        );
        for (case, t_at) in [("in flight", 0.05f64), ("at rest", rest + 1.0)] {
            let (mut sample, mut record, mut gpu_ms) = (Vec::new(), Vec::new(), Vec::new());
            for i in 0..FRAMES {
                // Frames 1/120 s apart; in flight they all stay inside the motion.
                let t = t_at + i as f64 * if case == "in flight" { 0.0005 } else { 1.0 / 120.0 };
                let s0 = Instant::now();
                let list = scene.sample(t);
                sample.push(s0.elapsed().as_secs_f64() * 1e3);
                assert_eq!(list.dots.len(), n as usize);
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
                } else {
                    g.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
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
            // About 196 bytes of spring state per dot, plus the id index.
            let field_mb = n as f64 * (196.0 + 16.0) / 1e6;
            println!(
                "| {n} | {case} | {build:.1} | {s50:.3} / {s95:.3} | {r50:.3} / {r95:.3} | {gpu} | {field_mb:.1} | {:.2} | +{:.1} |",
                n as f64 * 16.0 / 1e6,
                (rss_kb().saturating_sub(rss0)) as f64 / 1024.0,
            );
        }
    }
}
