//! The stage (`vo_scene::stage`): 2D layers turned and pushed back in one perspective
//! camera, as in a hologram popup (concept 20), spatial glass (08) and a tilted ring (09).
//! Two panels pushed back and turned toward the middle, a line of text tilted back with a
//! glow, and two rings laid back under it.
//!
//!   cargo run --release -p vo --example stage_demo -- [--out DIR]
//!   cargo run --release -p vo --example stage_demo -- --bench
//!   cargo run --release -p vo --example stage_demo -- --panes [--out DIR]
//!
//! The default writes `DIR/stage_demo.png` (default DIR `/var/tmp/claude/fw-stage`): the
//! same view flat, half turned and fully turned, and the turn springing in over time.
//! `--bench` prints GPU time per frame (timestamp queries) for the flat and the turned view,
//! filling a 600x300 and a 1920x1080 frame, and for the 20 panes below.
//!
//! `--panes` writes `DIR/stage_panes.png`: 20 receding word frames (EG-33), each its own turned
//! layer, flat, half turned and turned, and the turn springing in. A turned layer of shapes and
//! glyphs takes no texture: the run prints the frame report's offscreen layer count, 0.

use std::path::PathBuf;

use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::taffy::{AlignItems, JustifyContent};
use vo::scene::{DisplayList, Scene};
use vo::{FontRegistry, ShapedLayout};
use vo_render::offscreen::{write_png, FORMAT};
use vo_render::profile::SurfaceKind;
use vo_render::{Offscreen, Renderer, Target};

const CYAN: u32 = 0x6fe8ff;

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.set_default_fallbacks(&[fonts.asset("sans").unwrap()]);
    fonts.asset("condensed").unwrap_or_else(|e| panic!("{e}"));
    fonts
}

/// The view at a frame of `size`; `tilt` scales every turn and push, so 0 is flat.
fn view(fonts: &FontRegistry, size: Size, tilt: f32) -> Element<()> {
    let k = size.h / 300.0;
    let font = fonts.asset("condensed").unwrap();
    let pane = |key: &str, turn: f32| {
        Element::layer(key.to_string()).translate_z(-260.0 * k * tilt).rotate_y(turn * tilt).child(
            Element::panel(format!("{key}-glass"))
                .size(150.0 * k, 96.0 * k)
                .radius(10.0 * k)
                .fill(Color::hex(0x1d5f7a, 0.35))
                .stroke(Color::hex(CYAN, 0.7), 1.5 * k),
        )
    };
    let panes = Element::row("panes")
        .gap(250.0 * k)
        .margin_edges(0.0, 0.0, -70.0 * k, 0.0)
        .child(pane("left", -1.0))
        .child(pane("right", 1.0));
    let words = Element::layer("words").rotate_x(0.7 * tilt).child(
        Element::label("line", "so the plan for tomorrow")
            .text_style(TextStyle::new(font, 44.0 * k, 50.0 * k))
            .color(Color::hex(0xdffaff, 1.0))
            .glow(Color::hex(CYAN, 1.0), 10.0 * k, 0.8),
    );
    // Two rings, one inside the other, laid back under the words.
    let ring = |key: &str, d: f32, w: f32, a: f32| {
        Element::panel(key.to_string())
            .size(d * k, d * k)
            .radius(0.5 * d * k)
            .stroke(Color::hex(CYAN, a), w * k)
            .align_items(AlignItems::CENTER)
            .justify(JustifyContent::CENTER)
    };
    let rings = Element::layer("rings").rotate_x(1.3 * tilt).margin_edges(-170.0 * k, 0.0, -120.0 * k, 0.0).child(
        ring("outer", 260.0, 3.0, 0.9).glow(Color::hex(CYAN, 1.0), 6.0 * k, 0.6).child(ring("inner", 180.0, 1.5, 0.5)),
    );
    Element::panel("root")
        .size(size.w, size.h)
        .fill(Color::hex(0x05080e, 1.0))
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
        .child(Element::column("stack").align_items(AlignItems::CENTER).child(panes).child(words).child(rings))
}

const WORDS: [&str; 20] = [
    "so", "the", "plan", "for", "tomorrow", "is", "to", "open", "the", "file", "and", "set", "the", "theme", "then",
    "read", "the", "live", "words", "back",
];

/// 20 word frames in four rows of five (EG-33), each a layer of its own turned toward the
/// middle and pushed further back than the one before it.
fn panes_view(fonts: &FontRegistry, size: Size, tilt: f32) -> Element<()> {
    let k = size.h / 300.0;
    let font = fonts.asset("condensed").unwrap();
    let pane = |i: usize| {
        let (col, row) = ((i % 5) as f32, (i / 5) as f32);
        Element::layer(format!("pane{i}"))
            .translate_z(-40.0 * k * i as f32 * tilt)
            .rotate_y(0.35 * (col - 2.0) * tilt)
            .rotate_x(0.25 * (row - 1.5) * tilt)
            .child(
                Element::panel(format!("frame{i}"))
                    .size(100.0 * k, 46.0 * k)
                    .radius(9.0 * k)
                    .fill(Color::hex(0x1d5f7a, 0.45))
                    .stroke(Color::hex(CYAN, 0.9), 1.5 * k)
                    .align_items(AlignItems::CENTER)
                    .justify(JustifyContent::CENTER)
                    .child(
                        Element::label(format!("word{i}"), WORDS[i])
                            .text_style(TextStyle::new(font, 24.0 * k, 28.0 * k))
                            .color(Color::hex(0xdffaff, 1.0)),
                    ),
            )
    };
    let row = |r: usize| Element::row(format!("row{r}")).gap(12.0 * k).children((0..5).map(move |c| pane(r * 5 + c)));
    Element::panel("root")
        .size(size.w, size.h)
        .fill(Color::hex(0x05080e, 1.0))
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
        .child(Element::column("grid").gap(14.0 * k).align_items(AlignItems::CENTER).children((0..4).map(row)))
}

type View = fn(&FontRegistry, Size, f32) -> Element<()>;

/// The view applied flat at 0 and turned to `tilt` at 1 s, sampled at `t`.
fn sample(fonts: &FontRegistry, size: Size, tilt: f32, t: f64) -> DisplayList {
    sample_view(view, fonts, size, tilt, t)
}

fn sample_view(view: View, fonts: &FontRegistry, size: Size, tilt: f32, t: f64) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(view(fonts, size, 0.0), 0.0);
    scene.apply(view(fonts, size, tilt), 1.0);
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
        label: Some("stage-demo"),
        required_features: wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
        ..Default::default()
    }))
    .expect("a GPU device")
}

fn bench(fonts: FontRegistry) {
    let (device, queue) = gpu();
    let period = queue.get_timestamp_period() as f64;
    let mut renderer = Renderer::with_fonts(&device, &queue, fonts.clone());
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let buf = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
    let resolve = buf(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buf(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    let cases: [(&str, View, f32); 4] =
        [("flat", view, 0.0), ("turned", view, 1.0), ("panes-0", panes_view, 0.0), ("panes-20", panes_view, 1.0)];
    for (name, v, tilt) in cases {
        let mut line = format!("stage_demo bench: {name:8}");
        for (w, h) in [(600u32, 300u32), (1920, 1080)] {
            let list = sample_view(v, &fonts, Size::new(w as f32, h as f32), tilt, 10.0);
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
        let list = sample_view(v, &fonts, Size::new(600.0, 300.0), tilt, 10.0);
        let layers = list.layers.iter().filter(|l| l.warp.is_some()).count();
        renderer.render(&list, &mut Target::new(&device, 600, 300, FORMAT));
        println!("{line} {layers} turned layers, {} offscreen", offscreen_layers(&renderer));
    }
}

/// Layers the last frame drew into textures of their own.
fn offscreen_layers(r: &Renderer) -> usize {
    r.frame_report().surfaces.iter().filter(|s| s.kind == SurfaceKind::Layer).count()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("/var/tmp/claude/fw-stage");
    let fonts = fonts();
    let (mut v, mut name): (View, &str) = (view, "stage_demo");
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--bench" => return bench(fonts),
            "--panes" => (v, name) = (panes_view, "stage_panes"),
            _ => panic!("unknown option {a}"),
        }
    }
    // Row 1: flat, half turned, turned, all settled. Row 2: the turn springing in.
    let (w, h) = (600u32, 300u32);
    let size = Size::new(w as f32, h as f32);
    let frames = [(0.0, 10.0), (0.5, 10.0), (1.0, 10.0), (1.0, 1.05), (1.0, 1.15), (1.0, 1.4)];
    let (device, queue) = gpu();
    let mut off = Offscreen::with_fonts(&device, &queue, fonts.clone());
    let (cols, gap) = (3u32, 8u32);
    let (sw, sh) = (cols * w + (cols + 1) * gap, 2 * h + 3 * gap);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    for (i, &(tilt, t)) in frames.iter().enumerate() {
        let rgba = off.render_to_rgba(&sample_view(v, &fonts, size, tilt, t), [w, h], 1.0);
        if i == 2 {
            println!("{name}: turned, {} offscreen layers", offscreen_layers(&off.renderer));
        }
        let (x0, y0) = (gap + (i as u32 % cols) * (w + gap), gap + (i as u32 / cols) * (h + gap));
        for y in 0..h {
            let src = (y * w * 4) as usize;
            let dst = (((y0 + y) * sw + x0) * 4) as usize;
            sheet[dst..dst + (w * 4) as usize].copy_from_slice(&rgba[src..src + (w * 4) as usize]);
        }
    }
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join(format!("{name}.png"));
    write_png(&path, sw, sh, &sheet).unwrap();
    println!("{name}: wrote {}", path.display());
}
