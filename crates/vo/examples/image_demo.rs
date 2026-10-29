//! Images and app icons (EG-24): a delivery receipt chip that shows the app's icon at text
//! height, and the same icon large, as a PNG and as a full-colour SVG.
//!
//! The chip slides up and fades in over 0.3 s; with `--reduced` it only fades. The icons are
//! made in code (a gradient terminal glyph as SVG, a four-colour PNG), or `--icon PATH` shows
//! any PNG or SVG file in their place.
//!
//!   cargo run -p vo --example image_demo -- [--out DIR] [--icon PATH] [--reduced] [--scale S]
//!       [--font PATH]
//!
//! Writes `DIR/image-demo.png` (default DIR `/var/tmp/claude/image-demo`): one row per moment.

use std::path::PathBuf;
use std::sync::Arc;

use vo::scene::display::ShapeKind;
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::image::ImageSource;
use vo::scene::{taffy, Scene};
use vo::{FontRegistry, ShapedLayout};
use vo_render::offscreen::write_png;
use vo_render::Offscreen;

const MOMENTS: [f64; 4] = [0.05, 0.15, 0.3, 1.0];
const W: f32 = 600.0;
const H: f32 = 160.0;

const ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
<defs><linearGradient id="g" x2="1" y2="1"><stop offset="0" stop-color="#4cc9f0"/><stop offset="1" stop-color="#7b2ff7"/></linearGradient></defs>
<rect x="2" y="3" width="20" height="18" rx="4" fill="url(#g)"/><path d="M6 9l4 3-4 3M12 16h6" stroke="#fff" stroke-width="2" fill="none"/></svg>"##;

/// 32 x 32: four quarters with a soft round edge.
fn quarters_png() -> Vec<u8> {
    let mut px = Vec::new();
    for y in 0..32u32 {
        for x in 0..32u32 {
            let c: [u8; 3] = match (x < 16, y < 16) {
                (true, true) => [255, 107, 107],
                (false, true) => [255, 209, 102],
                (true, false) => [6, 214, 160],
                (false, false) => [17, 138, 178],
            };
            let (dx, dy) = (x as f32 - 15.5, y as f32 - 15.5);
            let a = (16.0 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            px.extend_from_slice(&[c[0], c[1], c[2], (a * 255.0) as u8]);
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, 32, 32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&px).unwrap();
    out
}

fn view(t: f64, reduced: bool, svg: &Arc<ImageSource>, png: &Arc<ImageSource>) -> Element<()> {
    let u = (t / 0.3).clamp(0.0, 1.0) as f32;
    let ease = 1.0 - (1.0 - u).powi(3);
    let rise = if reduced { 0.0 } else { 16.0 * (1.0 - ease) };
    let size = 16.0;
    let text = |key: &str, s: &str| {
        Element::label(key.to_string(), s).text_style(TextStyle::new(0, size, size * 1.25)).color(Color::hex(0xe8ecf2, 1.0))
    };
    let chip = Element::panel("chip")
        .direction_row()
        .pad_xy(12.0, 6.0)
        .gap(8.0)
        .radius(14.0)
        .fill(Color::hex(0x1b2433, 0.95))
        .align_items(taffy::AlignItems::CENTER)
        .child(text("typed", "typed into"))
        // The icon at text height.
        .child(Element::image("icon", svg.clone()).size(size, size))
        .child(text("app", "terminal"))
        .offset(0.0, rise)
        .opacity(ease);
    let big = Element::row("big")
        .gap(24.0)
        .align_items(taffy::AlignItems::CENTER)
        .child(Element::image("svg", svg.clone()).size(64.0, 64.0))
        .child(Element::image("png", png.clone()).size(64.0, 64.0))
        .child(Element::image("png-small", png.clone()).size(24.0, 24.0))
        .child(Element::shape("dot", ShapeKind::Circle).size(6.0, 6.0).fill(Color::hex(0x4cc9f0, 1.0)));
    Element::panel("root")
        .size(W, H)
        .fill(Color::hex(0x0b0f16, 1.0))
        .pad(20.0)
        .gap(20.0)
        .align_items(taffy::AlignItems::CENTER)
        .child(big)
        .child(chip)
}

fn main() {
    let mut out = PathBuf::from("/var/tmp/claude/image-demo");
    let (mut reduced, mut scale) = (false, 1.0f32);
    let mut icon: Option<PathBuf> = None;
    let mut font = "/usr/share/fonts/noto/NotoSans-Medium.ttf".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out takes a directory").into(),
            "--icon" => icon = Some(args.next().expect("--icon takes a file").into()),
            "--reduced" => reduced = true,
            "--scale" => scale = args.next().and_then(|v| v.parse().ok()).expect("--scale takes a number"),
            "--font" => font = args.next().expect("--font takes a file"),
            other => panic!("unknown argument {other}"),
        }
    }
    let (svg, png) = match icon {
        Some(p) => (ImageSource::file(&p), ImageSource::file(&p)),
        None => (ImageSource::bytes(ICON_SVG.as_bytes().to_vec()), ImageSource::bytes(quarters_png())),
    };
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
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("a GPU device");
    let fonts = FontRegistry::new();
    fonts.add_file(font.as_ref()).unwrap_or_else(|e| panic!("cannot load {font}: {e}"));
    let mut off = Offscreen::with_fonts(&device, &queue, fonts.clone());
    let (w, h) = ((W * scale).ceil() as u32, (H * scale).ceil() as u32);
    let mut scene: Scene<()> = Scene::new().with_reduced_motion(reduced).with_text(Box::new(ShapedLayout::new(fonts)));
    scene.set_viewport(0.0, Size::new(W, H), scale);
    let mut sheet = Vec::with_capacity((w * h * 4) as usize * MOMENTS.len());
    for &t in &MOMENTS {
        scene.apply(view(t, reduced, &svg, &png), t);
        sheet.extend(off.render_to_rgba(&scene.sample(t), [w, h], scale));
    }
    std::fs::create_dir_all(&out).expect("create the output directory");
    let file = out.join(if reduced { "image-demo-reduced.png" } else { "image-demo.png" });
    write_png(&file, w, h * MOMENTS.len() as u32, &sheet).expect("write the sheet");
    println!("image_demo: {}", file.display());
}
