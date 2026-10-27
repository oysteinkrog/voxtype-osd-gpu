//! HUD kit: the building blocks for radial, angular and audio-driven overlay designs.
//!
//! - **reach**: effect reach and the surface margin. A glowing toast on a surface only as big
//!   as its box, then on the surface `surface_for` sizes from `effect_reach`.
//!
//! `cargo run -p vo --example hud_kit -- [--out DIR] [--bench]` writes `contact-sheet.png`
//! (rows of frames per board) to DIR, or with `--bench` prints the GPU time per frame of
//! each board at 600 x 300 (scale 1) and at 1920 x 1080 (scale 3.2), from timestamp queries.

use std::path::PathBuf;
use std::time::Instant;

use vo::scene::element::Element;
use vo::scene::geom::{Color, Size};
use vo::scene::{surface_for, Scene};
use vo_render::offscreen::write_png;
use vo_render::{Offscreen, Renderer, Target};

/// One board: a view of a `Size` at a time, the times its sheet row shows, and its name.
struct Board {
    name: &'static str,
    view: fn(Size, f64) -> Element<()>,
    times: &'static [f64],
}

const BG: u32 = 0x0b0d12;

fn boards() -> Vec<Board> {
    vec![
        Board { name: "reach: tight surface", view: reach_tight, times: &[0.0] },
        Board { name: "reach: computed margin", view: reach_fitted, times: &[0.0] },
    ]
}

// Reach.

fn toast() -> Element<()> {
    Element::panel("toast")
        .size(240.0, 72.0)
        .radius(14.0)
        .fill(Color::hex(0x1b1d24, 1.0))
        .stroke(Color::hex(0x7ad7ff, 0.9), 2.0)
        .glow(Color::hex(0x2fb4ff, 1.0), 6.0, 1.4)
        .shadow(Color::hex(0x000000, 0.6), 0.0, 10.0, 8.0)
}

/// The toast centred on a surface exactly its own size, inside the board.
fn reach_tight(size: Size, _t: f64) -> Element<()> {
    centred(size, Element::layer("cut").clip().size(240.0, 72.0).child(toast()))
}

/// The toast on the surface `surface_for` sizes from its effect reach; the surface outline
/// shows where the transparent margin ends.
fn reach_fitted(size: Size, _t: f64) -> Element<()> {
    let content = Size::new(240.0, 72.0);
    let fit = surface_for(content, toast().effect_reach(0.0));
    let (l, t) = (fit.origin.x, fit.origin.y);
    let (r, b) = (fit.size.w - content.w - l, fit.size.h - content.h - t);
    let surface = Element::panel("surface")
        .size(fit.size.w, fit.size.h)
        .border(Color::hex(0x5a6070, 1.0), 1.0)
        .pad_edges(t, r, b, l)
        .child(toast());
    centred(size, Element::layer("cut").clip().size(fit.size.w, fit.size.h).child(surface))
}

/// `child` centred on a board of `size`.
fn centred(size: Size, child: Element<()>) -> Element<()> {
    use vo::scene::taffy::{AlignItems, JustifyContent};
    Element::column("board")
        .size(size.w, size.h)
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
        .child(child)
}

// Harness.

fn board_list(b: &Board, size: Size, scale: f32, t: f64) -> vo::scene::DisplayList {
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, size, scale);
    let root = Element::panel("bg").size(size.w, size.h).fill(Color::hex(BG, 1.0)).child((b.view)(size, t));
    scene.apply(root, 0.0);
    scene.sample(t)
}

fn device(timestamps: bool) -> (wgpu::Device, wgpu::Queue) {
    let mut idesc = wgpu::InstanceDescriptor::new_without_display_handle();
    idesc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(idesc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a Vulkan adapter");
    let required_features = if timestamps {
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS
    } else {
        wgpu::Features::empty()
    };
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("hud-kit"),
        required_features,
        ..Default::default()
    }))
    .expect("a GPU device")
}

/// Rows of frames, one row per board, over a dark sheet.
fn sheet(out: &std::path::Path) {
    let (device, queue) = device(false);
    let mut off = Offscreen::new(&device, &queue);
    let cell = Size::new(600.0, 300.0);
    let rows: Vec<Vec<Vec<u8>>> = boards()
        .iter()
        .map(|b| {
            b.times
                .iter()
                .map(|&t| off.render_to_rgba(&board_list(b, cell, 1.0, t), [600, 300], 1.0))
                .collect()
        })
        .collect();
    let cols = rows.iter().map(Vec::len).max().unwrap_or(1);
    let (cw, ch, gap) = (600usize, 300usize, 8usize);
    let (sw, sh) = (cols * cw + (cols + 1) * gap, rows.len() * ch + (rows.len() + 1) * gap);
    let mut px = vec![0u8; sw * sh * 4];
    for p in px.as_chunks_mut::<4>().0 {
        *p = [40, 40, 44, 255];
    }
    for (r, row) in rows.iter().enumerate() {
        for (c, img) in row.iter().enumerate() {
            let (ox, oy) = (gap + c * (cw + gap), gap + r * (ch + gap));
            for y in 0..ch {
                let d = ((oy + y) * sw + ox) * 4;
                px[d..d + cw * 4].copy_from_slice(&img[y * cw * 4..(y + 1) * cw * 4]);
            }
        }
    }
    std::fs::create_dir_all(out).expect("create the output directory");
    let path = out.join("contact-sheet.png");
    write_png(&path, sw as u32, sh as u32, &px).expect("write the contact sheet");
    println!("contact sheet: {}", path.display());
    for b in boards() {
        println!("  row: {}", b.name);
    }
}

/// GPU time per frame of each board, from timestamp queries around its passes.
fn bench() {
    let (device, queue) = device(true);
    let mut r = Renderer::new(&device, &queue);
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let buf = |usage| device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false });
    let resolve = buf(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buf(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    let period = queue.get_timestamp_period() as f64;
    let wait = |d: &wgpu::Device| d.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
    for (w, h, scale) in [(600u32, 300u32, 1.0f32), (1920, 1080, 3.2)] {
        let size = Size::new(w as f32 / scale, h as f32 / scale);
        let target = Target::new(&device, w, h, vo_render::offscreen::FORMAT);
        for b in boards() {
            let mut gpu = Vec::new();
            let mut cpu = Vec::new();
            for i in 0..120 {
                let t = b.times[i % b.times.len()] + (i / b.times.len()) as f64 * 1e-3;
                let c0 = Instant::now();
                let list = board_list(&b, size, scale, t);
                cpu.push(c0.elapsed().as_secs_f64() * 1e3);
                let mut enc = device.create_command_encoder(&Default::default());
                enc.write_timestamp(&qs, 0);
                r.encode(&list, scale, &target, &mut enc);
                enc.write_timestamp(&qs, 1);
                enc.resolve_query_set(&qs, 0..2, &resolve, 0);
                enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, 16);
                queue.submit([enc.finish()]);
                read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
                wait(&device);
                let ts = {
                    let b = read.slice(..).get_mapped_range().expect("mapped");
                    [u64::from_le_bytes(b[0..8].try_into().unwrap()), u64::from_le_bytes(b[8..16].try_into().unwrap())]
                };
                read.unmap();
                // The first frames build pipelines and caches.
                if i >= 20 {
                    gpu.push(ts[1].wrapping_sub(ts[0]) as f64 * period / 1e6);
                }
            }
            gpu.sort_by(f64::total_cmp);
            cpu.sort_by(f64::total_cmp);
            let median = |v: &[f64]| v[v.len() / 2];
            let p95 = |v: &[f64]| v[v.len() * 95 / 100];
            println!(
                "{w}x{h} {:<28} GPU median {:.3} ms, p95 {:.3} ms; CPU build {:.3} ms",
                b.name,
                median(&gpu),
                p95(&gpu),
                median(&cpu)
            );
        }
    }
}

fn main() {
    let mut out = PathBuf::from("/var/tmp/claude/hud-kit");
    let mut bench_only = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out DIR").into(),
            "--bench" => bench_only = true,
            other => panic!("unknown argument {other}"),
        }
    }
    if bench_only {
        bench();
    } else {
        sheet(&out);
    }
}
