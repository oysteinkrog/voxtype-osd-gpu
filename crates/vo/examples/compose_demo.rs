//! Effect composition demo: the plain bloom next to the shaped neon bloom, one layer with bloom,
//! a style and a mask inside nested rounded clips, the blend modes on both paths, and a slow
//! gradient with and without the moving blue-noise dither.
//!
//!   cargo run --release -p vo --example compose_demo -- [--out DIR]
//!   cargo run --release -p vo --example compose_demo -- --bench
//!
//! The default writes one PNG per scene to DIR (default `/var/tmp/claude/m1r-compose`), plus
//! `dither-frames.png`: four frames of the gradient with dither motion on, so the grain moves.
//! `--bench` prints GPU time per frame (timestamp queries) for each scene at 600x300 and at
//! 1920x1080, the scene scaled up to fill the frame.

use std::path::{Path, PathBuf};

use vo::scene::display::{BlendMode, BloomShape, DisplayList, ShapeKind};
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::taffy;
use vo::scene::Scene;
use vo::{FontRegistry, ShapedLayout};
use vo_render::offscreen::write_png;
use vo_render::{Offscreen, Renderer, Target};

type El = Element<()>;

const FONT: &str = "/usr/share/fonts/noto/NotoSans-Bold.ttf";

fn at(e: El, x: f32, y: f32) -> El {
    e.layout(|s| {
        s.position = taffy::style::Position::Absolute;
        s.inset.left = taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = taffy::style::LengthPercentageAuto::length(y);
    })
}

fn row(e: El) -> El {
    e.layout(|s| s.flex_direction = taffy::prelude::FlexDirection::Row)
}

/// Thin neon lines and a word.
fn neon(key: &'static str) -> El {
    let pink = Color::hex(0xff2bd6, 1.0);
    let cyan = Color::hex(0x22e4ff, 1.0);
    Element::panel(key)
        .size(200.0, 110.0)
        .child(at(Element::panel("tube").size(180.0, 60.0).radius(30.0).stroke(pink, 2.0), 10.0, 10.0))
        .child(at(Element::shape("ring", ShapeKind::Circle).size(36.0, 36.0).stroke(cyan, 1.5), 150.0, 64.0))
        .child(at(Element::panel("rule").size(120.0, 1.5).fill(cyan), 16.0, 94.0))
        .child(at(
            Element::label("word", "NEON").text_style(TextStyle::new(0, 26.0, 30.0)).color(Color::hex(0xffe8fb, 1.0)),
            62.0,
            24.0,
        ))
}

fn dark(key: &'static str, w: f32, h: f32) -> El {
    Element::panel(key).size(w, h).fill(Color::hex(0x07080c, 1.0)).pad(10.0).gap(20.0)
}

fn spark() -> El {
    Element::layer("spark")
        .size(6.0, 6.0)
        .bloom(0.5, 2.0)
        .child(Element::panel("dot").size(6.0, 6.0).fill(Color::WHITE))
}

struct Demo {
    name: &'static str,
    size: Size,
    view: El,
}

fn demos() -> Vec<Demo> {
    let bloom = row(dark("root", 460.0, 150.0)).children([
        Element::layer("plain").size(220.0, 130.0).pad(10.0).bloom(1.0, 16.0).child(neon("n1")),
        Element::layer("shaped")
            .size(220.0, 130.0)
            .pad(10.0)
            .bloom(1.0, 16.0)
            .bloom_shape(BloomShape::NEON)
            .child(neon("n2")),
    ]);
    let styled = Element::layer("styled")
        .size(220.0, 130.0)
        .pad(10.0)
        .bloom(1.0, 12.0)
        .bloom_shape(BloomShape::NEON)
        .scanlines(0.5, 3.0)
        .crt(0.12)
        .chromatic(2.0)
        .child(neon("n"))
        .child(Element::mask("m").child(at(
            Element::panel("hole").size(200.0, 110.0).radius(40.0).fill(Color::WHITE),
            10.0,
            10.0,
        )));
    let inner = Element::layer("inner")
        .size(240.0, 150.0)
        .radius(6.0)
        .clip()
        .offset(-10.0, -10.0)
        .child(Element::panel("bright").size(240.0, 150.0).fill(Color::hex(0x3a86ff, 1.0)));
    let stack = row(dark("root", 480.0, 150.0))
        .children([styled, Element::layer("outer").size(220.0, 130.0).radius(36.0).clip().child(inner)]);
    let blend = |effects: bool| {
        let stripes = Element::row("s").children(
            [0xe63946u32, 0xf1c453, 0x2a9d8f, 0x457b9d, 0xf4f1de]
                .map(|c| Element::panel(c as u64).size(24.0, 70.0).fill(Color::hex(c, 1.0))),
        );
        let cell = |key: &'static str, mode: BlendMode| {
            let discs = Element::layer("d")
                .size(120.0, 120.0)
                .blend(mode)
                .child(at(
                    Element::shape("a", ShapeKind::Circle).size(70.0, 70.0).fill(Color::hex(0x6a4c93, 0.9)),
                    0.0,
                    0.0,
                ))
                .child(at(
                    Element::shape("b", ShapeKind::Circle).size(70.0, 70.0).fill(Color::hex(0x8ac926, 0.9)),
                    40.0,
                    40.0,
                ));
            Element::panel(key).size(150.0, 130.0).child(at(stripes.clone(), 0.0, 0.0)).child(at(discs, 20.0, 10.0))
        };
        let mut root = row(Element::panel("root").size(470.0, 150.0).pad(10.0).gap(10.0)).children([
            cell("add", BlendMode::Add),
            cell("screen", BlendMode::Screen),
            cell("mul", BlendMode::Multiply),
        ]);
        if effects {
            root = root.child(at(spark(), 460.0, 140.0));
        }
        root
    };
    let gradient = Element::panel("root")
        .size(160.0, 300.0)
        .fill_gradient(Color::hex(0x10141c, 1.0), Color::hex(0x1e2433, 1.0))
        .child(at(spark(), 150.0, 290.0));
    vec![
        Demo { name: "bloom", size: Size::new(460.0, 150.0), view: bloom },
        Demo { name: "stack", size: Size::new(480.0, 150.0), view: stack },
        Demo { name: "blend-direct", size: Size::new(470.0, 150.0), view: blend(false) },
        Demo { name: "blend-effect", size: Size::new(470.0, 150.0), view: blend(true) },
        Demo { name: "gradient", size: Size::new(160.0, 300.0), view: gradient },
    ]
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(FONT.as_ref()).unwrap_or_else(|e| panic!("cannot load {FONT}: {e}"));
    fonts
}

fn sample(fonts: &FontRegistry, d: &Demo) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, d.size, 1.0);
    scene.apply(d.view.clone(), 0.0);
    scene.sample(0.5)
}

fn gpu(timestamps: bool) -> (wgpu::Device, wgpu::Queue) {
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
    let features = if timestamps {
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS
    } else {
        wgpu::Features::empty()
    };
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("compose-demo"),
        required_features: features,
        ..Default::default()
    }))
    .expect("a GPU device")
}

fn render(out: &Path) {
    std::fs::create_dir_all(out).expect("create the output directory");
    let fonts = fonts();
    let (device, queue) = gpu(false);
    let mut off = Offscreen::new(&device, &queue);
    off.renderer.add_font(std::fs::read(FONT).unwrap()).unwrap();
    for d in demos() {
        let list = sample(&fonts, &d);
        let (w, h) = (d.size.w as u32, d.size.h as u32);
        let px = off.render_to_rgba(&list, [w, h], 1.0);
        let path = out.join(format!("{}.png", d.name));
        write_png(&path, w, h, &px).expect("write png");
        println!("compose_demo: {}", path.display());
    }
    // Four frames of the gradient with the dither moving, side by side.
    let d = demos().pop().unwrap();
    let list = sample(&fonts, &d);
    off.renderer.set_dither_motion(true);
    let (w, h) = (d.size.w as u32, d.size.h as u32);
    let frames: Vec<Vec<u8>> = (0..4).map(|_| off.render_to_rgba(&list, [w, h], 1.0)).collect();
    let sw = 4 * w;
    let mut sheet = vec![0u8; (sw * h * 4) as usize];
    for (i, f) in frames.iter().enumerate() {
        for y in 0..h {
            let src = &f[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
            let at = ((y * sw + i as u32 * w) * 4) as usize;
            sheet[at..at + src.len()].copy_from_slice(src);
        }
    }
    let path = out.join("dither-frames.png");
    write_png(&path, sw, h, &sheet).expect("write png");
    println!("compose_demo: {}", path.display());
}

fn bench() {
    let fonts = fonts();
    let (device, queue) = gpu(true);
    let period = queue.get_timestamp_period() as f64;
    let mut renderer = Renderer::with_fonts(&device, &queue, fonts.clone());
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    for d in demos() {
        let list = sample(&fonts, &d);
        for (w, h) in [(600u32, 300u32), (1920, 1080)] {
            // Scale the scene up until it fills the frame on its tighter side.
            let scale = (w as f32 / d.size.w).min(h as f32 / d.size.h);
            let target = Target::new(&device, w, h, vo_render::offscreen::FORMAT);
            let mut times = Vec::new();
            for i in 0..220 {
                let mut enc = device.create_command_encoder(&Default::default());
                enc.write_timestamp(&qs, 0);
                renderer.encode(&list, scale, &target, &mut enc);
                enc.write_timestamp(&qs, 1);
                enc.resolve_query_set(&qs, 0..2, &resolve, 0);
                enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, 16);
                queue.submit([enc.finish()]);
                read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                let ts: [u64; 2] = bytemuck::pod_read_unaligned(&read.slice(..).get_mapped_range().unwrap()[..16]);
                read.unmap();
                // The first frames compile pipelines and fill the pool.
                if i >= 20 {
                    times.push((ts[1] - ts[0]) as f64 * period / 1e6);
                }
            }
            times.sort_by(f64::total_cmp);
            println!(
                "compose_demo bench: {:<13} {w}x{h} (scale {scale:.2}): GPU {:.3} ms median, {:.3} ms p95",
                d.name,
                times[times.len() / 2],
                times[times.len() * 95 / 100]
            );
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("/var/tmp/claude/m1r-compose");
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--bench" => return bench(),
            _ => panic!("unknown option {a}"),
        }
    }
    render(&out);
}
