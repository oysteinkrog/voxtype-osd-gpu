//! Materials (FX15) through elements: a neon pill that flickers on, a neon wavy line, chrome
//! type, a hologram card, a gradient across the newest word, a phosphor scope whose white
//! trace cools to green, an aurora mesh whose energy follows the scope's level, and a warm
//! haze. Offline it writes a contact sheet over time and measures GPU time
//! per frame at 600x300 and 1920x1080, with the materials on and off:
//!
//!   cargo run --release -p vo --example material_demo -- --offline /var/tmp/claude/material
//!
//! `--grid-words DIR` (EG-31) draws Breathing Grid instead: a grid whose contours brighten
//! and lift around each new word, from the text field and the word table
//! (`vo_render::text_field`). It writes a contact sheet, the luminance near the newest word
//! against elsewhere, and GPU time with the field's builds. `--reduced` shows the reduced
//! motion form: every word settled and no breathing.
//!
//!   cargo run -p vo --example material_demo -- --grid-words /var/tmp/claude/grid [--reduced]

use std::path::{Path, PathBuf};

use vo::scene::display::{DisplayList, ShapeKind};
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::material::Material;
use vo::scene::path::StrokeStyle;
use vo::scene::taffy::AlignItems;
use vo::scene::Scene;
use vo::{FontRegistry, ShapedLayout};
use vo_core::words::WordModel;
use vo_render::text_field::{TextField, WordTable};
use vo_render::{Renderer, Target};

/// The font id of Noto Sans Bold, the first one loaded.
const BOLD: u32 = 0;
const W: f32 = 600.0;
const H: f32 = 360.0;
const FPS: f64 = 30.0;

fn hex(rgb: u32) -> Color {
    Color::hex(rgb, 1.0)
}

/// The demo card at `t`. Without `materials` the same layout draws in plain paint.
fn view(t: f64, materials: bool) -> Element<()> {
    let on = |e: Element<()>, m: Material| if materials { e.material(m) } else { e };
    let style = |font: u32, px: f32| TextStyle::new(font, px, (px * 1.2).round());
    let pill = on(
        Element::shape("pill", ShapeKind::Capsule).size(170.0, 52.0).stroke(hex(0xff2fd0), 3.5),
        Material::neon(7.0).hue(0.08).flicker_on(0.9, 0.0).into(),
    );
    let wavy = on(
        Element::wavy_line("wavy", StrokeStyle::solid(3.0, hex(0x26e8ff)), 12.0, 72.0).width(330.0).height(40.0),
        Material::neon(6.0).core(0.4, 0.9).into(),
    );
    let top = Element::row("top").align_items(AlignItems::CENTER).gap(40.0).child(pill).child(wavy);
    let title = on(
        Element::label("title", "SYNTH").text_style(style(BOLD, 64.0)).color(Color::WHITE),
        Material::chrome_classic().into(),
    );
    let card = on(
        Element::panel("card")
            .size(230.0, 78.0)
            .radius(10.0)
            .fill(Color::hex(0x40e0ff, 0.45))
            .stroke(Color::hex(0x9ff4ff, 0.9), 1.5)
            .align_items(AlignItems::CENTER)
            .child(
                Element::label("rec", "REC 00:42")
                    .text_style(style(BOLD, 34.0))
                    .color(hex(0xc8fbff))
                    .margin_edges(16.0, 0.0, 0.0, 0.0),
            ),
        Material::hologram(Color::hex(0x40e0ff, 0.6), 3.0).into(),
    );
    let middle = Element::row("middle").align_items(AlignItems::CENTER).gap(60.0).child(title).child(card);
    let word = on(
        Element::label("word", "newest word").text_style(style(BOLD, 34.0)).color(Color::WHITE),
        Material::gradient_across(hex(0xffffff), 0.45, hex(0x38e8ff), hex(0x9b5cff)),
    );
    // The scope's beam: a white dot sweeping across, its height following the level.
    let x = (t as f32 * 150.0) % 222.0;
    let y = 24.0 - 20.0 * levels_at(t) * (x * 0.09).sin();
    let beam = Element::shape("beam", ShapeKind::Circle).size(8.0, 8.0).fill(Color::WHITE).offset(x, y);
    let mut scope = Element::layer("scope").size(230.0, 56.0).child(beam);
    if materials {
        scope = scope.phosphor(0.35, hex(0x33ff66)).bloom(0.6, 3.0);
    }
    let bottom = Element::row("bottom").align_items(AlignItems::CENTER).gap(60.0).child(word).child(scope);
    // The aurora's energy follows the level the scope shows now.
    let energy = levels_at(t);
    let aurora = on(
        Element::panel("aurora").size(300.0, 44.0).radius(12.0).fill(hex(0x101828)),
        Material::aurora(&[hex(0x18c8b0), hex(0x6a2cff), hex(0xff3d9a), hex(0x2a7bff)]).energy(energy).into(),
    );
    // The haze glows inside a plain warm panel.
    let mut haze = Element::panel("haze").size(220.0, 44.0).radius(12.0).fill(hex(0x1a1410));
    if materials {
        let glow = Element::panel("haze-glow").size(220.0, 44.0).radius(12.0).fill(Color::WHITE);
        haze = haze.child(glow.material(Material::from(Material::haze(Color::hex(0xffa860, 0.6)))));
    }
    let glow = Element::row("glow").gap(24.0).child(aurora).child(haze);
    Element::panel("root")
        .size(W, H)
        .fill(hex(0x07060f))
        .pad_edges(26.0, 28.0, 20.0, 28.0)
        .gap(22.0)
        .child(top)
        .child(middle)
        .child(bottom)
        .child(glow)
}

/// The scope's level history, sample `i` at 40 a second, fixed so frames compare.
fn level(i: usize) -> f32 {
    (0.5 + 0.45 * (i as f32 * 0.31).sin() * (i as f32 * 0.07).cos()).abs()
}

/// The level at `t` seconds, smoothed over a quarter second so the aurora breathes.
fn levels_at(t: f64) -> f32 {
    let n = (t * 40.0) as usize;
    let k = 10.min(n + 1);
    (n + 1 - k..=n).map(level).sum::<f32>() / k as f32
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let f = "/usr/share/fonts/noto/NotoSans-Bold.ttf";
    fonts.add_file(f.as_ref()).unwrap_or_else(|e| panic!("cannot load {f}: {e}"));
    fonts
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    timestamps: bool,
    adapter: String,
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
    .expect("adapter");
    let want = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    let features = adapter.features() & want;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("material-demo"),
        required_features: features,
        ..Default::default()
    }))
    .expect("device");
    Gpu { device, queue, timestamps: features == want, adapter: adapter.get_info().name }
}

/// One run of the card from t = 0, a frame every 1/30 s, at `size` logical units drawn at
/// `scale`. Phosphor trails need every frame in order. Returns the GPU time of each frame
/// after `settle` seconds in milliseconds, and the frames at `keep` times as RGBA.
fn run(
    g: &Gpu,
    fonts: &FontRegistry,
    size: Size,
    scale: f32,
    materials: bool,
    secs: f64,
    keep: &[f64],
) -> (Vec<f64>, Vec<(f64, Vec<u8>)>) {
    let mut renderer = Renderer::with_fonts(&g.device, &g.queue, fonts.clone());
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, size, 1.0);
    let (w, h) = ((size.w * scale).round() as u32, (size.h * scale).round() as u32);
    let target = Target::new(&g.device, w, h, vo_render::offscreen::FORMAT);
    let query = g.timestamps.then(|| {
        let set = g.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: None,
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let usage = wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC;
        let resolve =
            g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false });
        let read = g.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        (set, resolve, read)
    });
    let mut list = DisplayList::default();
    let (mut times, mut frames) = (Vec::new(), Vec::new());
    let n = (secs * FPS).round() as usize;
    for i in 0..=n {
        let t = i as f64 / FPS;
        scene.apply(view(t, materials), t);
        scene.sample_into(t, &mut list);
        let mut enc = g.device.create_command_encoder(&Default::default());
        if let Some((set, _, _)) = &query {
            enc.write_timestamp(set, 0);
        }
        renderer.encode(&list, scale, &target, &mut enc);
        if let Some((set, resolve, read)) = &query {
            enc.write_timestamp(set, 1);
            enc.resolve_query_set(set, 0..2, resolve, 0);
            enc.copy_buffer_to_buffer(resolve, 0, read, 0, 16);
        }
        let sub = g.queue.submit([enc.finish()]);
        g.device.poll(wgpu::PollType::Wait { submission_index: Some(sub), timeout: None }).expect("poll");
        if let Some((_, _, read)) = &query {
            read.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("map"));
            g.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
            let ticks: Vec<u64> = {
                let d = read.slice(..).get_mapped_range().expect("mapped");
                d.as_chunks::<8>().0.iter().map(|c| u64::from_le_bytes(*c)).collect()
            };
            read.unmap();
            if t >= 1.0 {
                times.push((ticks[1] - ticks[0]) as f64 * g.queue.get_timestamp_period() as f64 / 1e6);
            }
        }
        if keep.iter().any(|k| (k - t).abs() < 0.5 / FPS) {
            frames.push((t, read_rgba(g, &target)));
        }
    }
    (times, frames)
}

fn read_rgba(g: &Gpu, target: &Target) -> Vec<u8> {
    let (w, h) = (target.width(), target.height());
    let row = (w * 4).div_ceil(256) * 256;
    let buf = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("material-readback"),
        size: (row * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = g.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        target.texture().as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    let sub = g.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    g.device.poll(wgpu::PollType::Wait { submission_index: Some(sub), timeout: None }).expect("poll");
    let data = buf.slice(..).get_mapped_range().expect("mapped");
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let s = (y * row) as usize;
        out.extend_from_slice(&data[s..s + (w * 4) as usize]);
    }
    out
}

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Frames stacked into one column with a gap, as a contact sheet.
fn sheet(frames: &[(f64, Vec<u8>)], w: u32, h: u32, dir: &Path) -> PathBuf {
    let gap = 6;
    let sh = frames.len() as u32 * (h + gap) - gap;
    let mut px = vec![0u8; (w * sh * 4) as usize];
    for (k, (_, f)) in frames.iter().enumerate() {
        let y0 = k as u32 * (h + gap);
        for y in 0..h {
            let d = (((y0 + y) * w) * 4) as usize;
            let s = ((y * w) * 4) as usize;
            px[d..d + (w * 4) as usize].copy_from_slice(&f[s..s + (w * 4) as usize]);
        }
    }
    let path = dir.join("material-sheet.png");
    vo_render::offscreen::write_png(&path, w, sh, &px).expect("png");
    path
}

fn offline(dir: &Path) {
    std::fs::create_dir_all(dir).expect("output folder");
    let g = gpu();
    let fonts = fonts();
    println!("material_demo: adapter {}", g.adapter);
    let keep = [0.1, 0.4, 0.7, 1.2, 2.0, 3.0];
    let (times, frames) = run(&g, &fonts, Size::new(W, H), 1.0, true, 3.0, &keep);
    for (t, f) in &frames {
        vo_render::offscreen::write_png(&dir.join(format!("material-t{t:.1}.png")), W as u32, H as u32, f)
            .expect("png");
    }
    println!("material_demo: wrote {}", sheet(&frames, W as u32, H as u32, dir).display());
    if !g.timestamps {
        println!("material_demo: no timestamp queries on this adapter, no GPU times");
        return;
    }
    let (plain, _) = run(&g, &fonts, Size::new(W, H), 1.0, false, 3.0, &[]);
    println!(
        "material_demo: GPU ms per frame at 600x360: {:.3} with materials, {:.3} without",
        median(times),
        median(plain)
    );
    // 1920x1080: the same card at 3 times, on a viewport 640 units wide.
    let big = Size::new(640.0, H);
    let (on, frames) = run(&g, &fonts, big, 3.0, true, 3.0, &[3.0]);
    let (off, _) = run(&g, &fonts, big, 3.0, false, 3.0, &[]);
    println!(
        "material_demo: GPU ms per frame at 1920x1080: {:.3} with materials, {:.3} without",
        median(on),
        median(off)
    );
    if let Some((_, f)) = frames.first() {
        vo_render::offscreen::write_png(&dir.join("material-1080p.png"), 1920, 1080, f).expect("png");
    }
}

/// Breathing Grid's material id and WGSL (EG-31).
const GRID: u32 = vo::scene::material::CUSTOM_FIRST;
const GRID_WGSL: &str = include_str!("grid_words.wgsl");
const SENTENCE: &str = "words light the grid as they come in";
/// A new word every this many seconds.
const WORD_EVERY: f64 = 0.5;

/// The grid at `t`: one more word of the sentence every half second, over a panel whose
/// material reads the text.
fn grid_view(t: f64, reduced: bool) -> Element<()> {
    let n = ((t / WORD_EVERY) as usize + 1).min(SENTENCE.split(' ').count());
    let text: Vec<&str> = SENTENCE.split(' ').take(n).collect();
    let mut model = WordModel::new();
    model.update(&text.join(" "));
    let speed = if reduced { 0.0 } else { 1.3 };
    let grid = Material::custom(GRID).reads_text().color(0, hex(0x38e8ff)).param(1, [speed, 22.0, 0.0, 0.0]);
    // The grid is a sibling under the text, out of the flow: a material on a panel also
    // paints the panel's children.
    let back = Element::panel("grid").size(W, H).fill(Color::WHITE).material(grid).layout(|s| {
        s.position = vo::scene::taffy::style::Position::Absolute;
        s.inset.left = vo::scene::taffy::style::LengthPercentageAuto::length(0.0);
        s.inset.top = vo::scene::taffy::style::LengthPercentageAuto::length(0.0);
    });
    let words =
        Element::live_text("live", model.words()).text_style(TextStyle::new(BOLD, 40.0, 54.0)).color(Color::WHITE);
    Element::panel("root").size(W, H).fill(hex(0x02030a)).pad_edges(70.0, 40.0, 0.0, 40.0).child(back).child(words)
}

/// Relative luminance of an sRGB byte pixel.
fn luma(px: &[u8]) -> f64 {
    let lin = |v: u8| {
        let c = v as f64 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(px[0]) + 0.7152 * lin(px[1]) + 0.0722 * lin(px[2])
}

/// Mean luminance within 20 px of the newest word's box against the mean further out, both
/// outside every word's box so the ink itself does not count.
fn near_ratio(px: &[u8], w: u32, h: u32, table: &WordTable) -> Option<f64> {
    let newest = table.words().first()?.rect;
    let inside = |x: f32, y: f32, r: [f32; 4]| x >= r[0] && x <= r[2] && y >= r[1] && y <= r[3];
    let (mut near, mut far) = ((0.0, 0u64), (0.0, 0u64));
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            if table.words().iter().any(|wd| inside(fx, fy, wd.rect)) {
                continue;
            }
            let qx = (newest[0] - fx).max(fx - newest[2]).max(0.0);
            let qy = (newest[1] - fy).max(fy - newest[3]).max(0.0);
            let l = luma(&px[((y * w + x) * 4) as usize..]);
            let bucket = if qx.hypot(qy) <= 20.0 { &mut near } else { &mut far };
            bucket.0 += l;
            bucket.1 += 1;
        }
    }
    Some((near.0 / near.1.max(1) as f64) / (far.0 / far.1.max(1) as f64).max(1e-9))
}

/// One run of the grid from t = 0 at 30 fps. Returns the GPU ms of each frame after 0.2 s
/// (field build plus frame), the frames at `keep` with their near/far ratios, and the
/// field's build count and memory.
fn grid_run(g: &Gpu, fonts: &FontRegistry, reduced: bool, secs: f64, keep: &[f64]) -> GridRun {
    let mut renderer = Renderer::with_fonts(&g.device, &g.queue, fonts.clone());
    renderer.add_material(GRID, "mat_grid_words", GRID_WGSL);
    let mut field = TextField::new(&g.device, &g.queue);
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    let (w, h) = (W as u32, H as u32);
    let target = Target::new(&g.device, w, h, vo_render::offscreen::FORMAT);
    let query = g.timestamps.then(|| {
        let set = g.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: None,
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let usage = wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC;
        let resolve =
            g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false });
        let read = g.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        (set, resolve, read)
    });
    let mut list = DisplayList::default();
    let mut run = GridRun { times: Vec::new(), frames: Vec::new(), builds: 0, memory: 0 };
    for i in 0..=(secs * FPS).round() as usize {
        let t = i as f64 / FPS;
        scene.apply(grid_view(t, reduced), t);
        scene.sample_into(t, &mut list);
        let mut enc = g.device.create_command_encoder(&Default::default());
        if let Some((set, _, _)) = &query {
            enc.write_timestamp(set, 0);
        }
        field.update_words(&mut renderer, &list, [w, h], 1.0, t, &mut enc);
        g.queue.submit([enc.finish()]);
        let table = field.word_table(t, reduced);
        field.bind(&mut renderer, &table);
        let mut enc = g.device.create_command_encoder(&Default::default());
        renderer.encode(&list, 1.0, &target, &mut enc);
        if let Some((set, resolve, read)) = &query {
            enc.write_timestamp(set, 1);
            enc.resolve_query_set(set, 0..2, resolve, 0);
            enc.copy_buffer_to_buffer(resolve, 0, read, 0, 16);
        }
        let sub = g.queue.submit([enc.finish()]);
        g.device.poll(wgpu::PollType::Wait { submission_index: Some(sub), timeout: None }).expect("poll");
        if let Some((_, _, read)) = &query {
            read.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("map"));
            g.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
            let ticks: Vec<u64> = {
                let d = read.slice(..).get_mapped_range().expect("mapped");
                d.as_chunks::<8>().0.iter().map(|c| u64::from_le_bytes(*c)).collect()
            };
            read.unmap();
            if t >= 0.2 {
                run.times.push((ticks[1] - ticks[0]) as f64 * g.queue.get_timestamp_period() as f64 / 1e6);
            }
        }
        if keep.iter().any(|k| (k - t).abs() < 0.5 / FPS) {
            let px = read_rgba(g, &target);
            let ratio = near_ratio(&px, w, h, &table);
            run.frames.push((t, px, ratio));
        }
    }
    run.builds = field.builds();
    run.memory = field.memory_bytes();
    run
}

struct GridRun {
    times: Vec<f64>,
    frames: Vec<(f64, Vec<u8>, Option<f64>)>,
    builds: u64,
    memory: u64,
}

fn grid_offline(dir: &Path, reduced: bool) {
    std::fs::create_dir_all(dir).expect("output folder");
    let g = gpu();
    let fonts = fonts();
    println!("material_demo: grid-words on {}{}", g.adapter, if reduced { ", reduced motion" } else { "" });
    let keep = [0.1, 0.3, 0.6, 1.2, 2.1, 3.6];
    let run = grid_run(&g, &fonts, reduced, 3.6, &keep);
    for (t, _, ratio) in &run.frames {
        match ratio {
            Some(r) => println!("material_demo: t {t:.1} s: luminance near the newest word {r:.2}x elsewhere"),
            None => println!("material_demo: t {t:.1} s: no words yet"),
        }
    }
    let frames: Vec<(f64, Vec<u8>)> = run.frames.iter().map(|(t, px, _)| (*t, px.clone())).collect();
    let sheet = sheet(&frames, W as u32, H as u32, dir);
    let name = if reduced { "grid-words-reduced.png" } else { "grid-words-sheet.png" };
    std::fs::rename(&sheet, dir.join(name)).expect("rename sheet");
    println!("material_demo: wrote {}", dir.join(name).display());
    println!(
        "material_demo: the field was built {} times in {:.1} s ({} words); field memory {} KiB",
        run.builds,
        3.6,
        SENTENCE.split(' ').count(),
        run.memory / 1024
    );
    if !run.times.is_empty() {
        let mut v = run.times.clone();
        v.sort_by(f64::total_cmp);
        println!(
            "material_demo: GPU ms per frame at {W}x{H} with field builds: p50 {:.3}, p95 {:.3}, max {:.3}",
            v[v.len() / 2],
            v[((v.len() - 1) as f64 * 0.95).round() as usize],
            v[v.len() - 1]
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let reduced = args.iter().any(|a| a == "--reduced");
    if let Some(dir) = args.iter().position(|a| a == "--grid-words").and_then(|i| args.get(i + 1)) {
        grid_offline(&PathBuf::from(dir), reduced);
        return;
    }
    match args.iter().position(|a| a == "--offline").and_then(|i| args.get(i + 1)) {
        Some(dir) => offline(&PathBuf::from(dir)),
        None => {
            eprintln!("usage: material_demo --offline DIR | --grid-words DIR [--reduced]");
            std::process::exit(2);
        }
    }
}
