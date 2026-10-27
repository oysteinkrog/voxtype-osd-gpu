//! 3D on the stage camera (`vo_3d::OnStage`), as in a hologram popup (concept 20): a line of
//! text tilted back over two glowing 3D rings that turn slowly, with two glass panels pushed
//! back behind them. The stage origin sweeps from side to side, so the panels, the text and
//! the rings move against each other (parallax) as they would under one camera.
//!
//!   cargo run --release -p vo-3d --example stage3d_demo -- [--out DIR]
//!   cargo run --release -p vo-3d --example stage3d_demo -- --bench
//!
//! The default writes `DIR/stage3d_demo.png` (default DIR `/var/tmp/claude/fw-stage3d`): six
//! frames across the sweep. `--bench` prints GPU time per frame (timestamp queries) for the
//! whole view, the 2D layers and the 3D rings together, at 600x300 and 1920x1080.

use std::f32::consts::FRAC_PI_2;
use std::path::PathBuf;
use std::sync::Arc;

use vo_3d::*;
use vo_render::offscreen::{write_png, FORMAT};
use vo_render::{Offscreen, Renderer, Target};
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::FontRegistry;
use vo_scene::geom::{Color, Point, Size};
use vo_scene::gpu::GpuSource;
use vo_scene::stage::Stage;
use vo_scene::taffy::{AlignItems, JustifyContent};
use vo_scene::text::ShapedLayout;
use vo_scene::{DisplayList, Scene};

const CYAN: u32 = 0x6fe8ff;
/// How far the rings lie back, in radians, as `Element::rotate_x` counts.
const LAY_BACK: f32 = 1.2;

/// Two rings in the item's plane, laid back and turning about their own axis. Sizes are
/// fractions of the item's width, so the rings scale with the popup.
#[derive(Debug)]
struct Rings;

impl Source3d for Rings {
    fn meshes(&self) -> Vec<Mesh> {
        vec![Mesh::torus(1.0, 0.012, 160, 10), Mesh::torus(1.0, 0.006, 128, 8)]
    }

    fn frame(&self, t: f64, size: Size) -> Frame3d {
        let r = 0.5 * size.w;
        let spin = 0.4 * t as f32;
        let ring = |mesh: u32, k: f32, turn: f32, emissive: f32| MeshDraw {
            mesh: MeshId(mesh),
            model: Mat4::uniform_scale(k * r).then(&Mat4::rotate_y(turn)).then(&Mat4::rotate_x(FRAC_PI_2 - LAY_BACK)),
            material: Material { color: Color::hex(CYAN, 1.0), emissive, ..Default::default() },
        };
        Frame3d {
            meshes: vec![ring(0, 1.0, spin, 1.0), ring(1, 0.7, -1.5 * spin, 0.7)],
            on_stage: Some(OnStage { overscan: 0.15 }),
            ..Default::default()
        }
    }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.set_default_fallbacks(&[fonts.asset("sans").unwrap()]);
    fonts.asset("condensed").unwrap_or_else(|e| panic!("{e}"));
    fonts
}

fn view(fonts: &FontRegistry, rings: &Arc<dyn GpuSource>, size: Size) -> Element<()> {
    let k = size.h / 300.0;
    let font = fonts.asset("condensed").unwrap();
    let pane = |key: &str, turn: f32| {
        Element::layer(key.to_string()).translate_z(-300.0 * k).rotate_y(turn).child(
            Element::panel(format!("{key}-glass"))
                .size(150.0 * k, 110.0 * k)
                .radius(10.0 * k)
                .fill(Color::hex(0x1d5f7a, 0.35))
                .stroke(Color::hex(CYAN, 0.7), 1.5 * k),
        )
    };
    let panes = Element::row("panes")
        .gap(260.0 * k)
        .margin_edges(0.0, 0.0, -90.0 * k, 0.0)
        .child(pane("left", -0.9))
        .child(pane("right", 0.9));
    let words = Element::layer("words").rotate_x(0.55).child(
        Element::label("line", "so the plan for tomorrow")
            .text_style(TextStyle::new(font, 44.0 * k, 50.0 * k))
            .color(Color::hex(0xdffaff, 1.0))
            .glow(Color::hex(CYAN, 1.0), 10.0 * k, 0.8),
    );
    let rings =
        Element::gpu("rings", rings.clone()).size(300.0 * k, 300.0 * k).margin_edges(-110.0 * k, 0.0, -190.0 * k, 0.0);
    Element::panel("root")
        .size(size.w, size.h)
        .fill(Color::hex(0x05080e, 1.0))
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
        .child(Element::column("stack").align_items(AlignItems::CENTER).child(panes).child(rings).child(words))
}

/// The view at `t` with the stage origin swept by `sweep` (-1 to 1) across the frame.
fn sample(fonts: &FontRegistry, rings: &Arc<dyn GpuSource>, size: Size, t: f64, sweep: f32) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, size, 1.0);
    let origin = Point::new(size.w * (0.5 + 0.45 * sweep), size.h * 0.4);
    scene.set_stage(Stage { perspective: 900.0 * size.h / 300.0, origin: Some(origin) });
    scene.apply(view(fonts, rings, size), 0.0);
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
        label: Some("stage3d-demo"),
        required_features: wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
        ..Default::default()
    }))
    .expect("a GPU device")
}

fn bench(fonts: FontRegistry, rings: Arc<dyn GpuSource>) {
    let (device, queue) = gpu();
    let period = queue.get_timestamp_period() as f64;
    let mut renderer = Renderer::with_fonts(&device, &queue, fonts.clone());
    install(&mut renderer);
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let buf = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
    let resolve = buf(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buf(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    let mut line = String::from("stage3d_demo bench:");
    for (w, h) in [(600u32, 300u32), (1920, 1080)] {
        let size = Size::new(w as f32, h as f32);
        let target = Target::new(&device, w, h, FORMAT);
        let mut times = Vec::new();
        for i in 0..140 {
            let list = sample(&fonts, &rings, size, i as f64 / 60.0, ((i as f32) / 40.0).sin());
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
    let mut out = PathBuf::from("/var/tmp/claude/fw-stage3d");
    let fonts = fonts();
    let rings = item(Rings);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--bench" => return bench(fonts, rings),
            _ => panic!("unknown option {a}"),
        }
    }
    let (w, h) = (600u32, 300u32);
    let size = Size::new(w as f32, h as f32);
    let (device, queue) = gpu();
    let mut off = Offscreen::with_fonts(&device, &queue, fonts.clone());
    install(&mut off.renderer);
    let (cols, rows, gap) = (3u32, 2u32, 8u32);
    let (sw, sh) = (cols * w + (cols + 1) * gap, rows * h + (rows + 1) * gap);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    for i in 0..cols * rows {
        let sweep = -1.0 + 2.0 * i as f32 / (cols * rows - 1) as f32;
        let rgba = off.render_to_rgba(&sample(&fonts, &rings, size, 0.5 * i as f64, sweep), [w, h], 1.0);
        let (x0, y0) = (gap + (i % cols) * (w + gap), gap + (i / cols) * (h + gap));
        for y in 0..h {
            let src = (y * w * 4) as usize;
            let dst = (((y0 + y) * sw + x0) * 4) as usize;
            sheet[dst..dst + (w * 4) as usize].copy_from_slice(&rgba[src..src + (w * 4) as usize]);
        }
    }
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join("stage3d_demo.png");
    write_png(&path, sw, sh, &sheet).unwrap();
    println!("stage3d_demo: wrote {}", path.display());
}
