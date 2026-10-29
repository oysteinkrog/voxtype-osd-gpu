//! Shape groups (EG-22): the 03 capsule bulging around the spoken word.
//!
//! A pill holds a sentence, one word at a time. When a word arrives, a bulge grows around it
//! and melts into the pill (a smooth union of the capsule and one bulge member per word), and
//! the previous word's bulge relaxes back. While words are arriving the pill's edge ripples by
//! about a pixel. Fill, stroke and glow all follow the melted outline. Everything is drawn by
//! one canvas, pure in `t`.
//!
//! Reduced motion keeps the current word's bulge at its settled size, swaps it at once, and
//! drops the ripple.
//!
//!   cargo run -p vo --example capsule_bulge -- [--out DIR] [--reduced] [--scale S]
//!
//! Writes `DIR/capsule-bulge.png` (default DIR `/var/tmp/claude/capsule-bulge`): one row per
//! moment, from before the first word to after the last.

use std::path::PathBuf;

use vo::scene::canvas::{bulge_grow, Painter};
use vo::scene::display::{GroupPrim, Paint};
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Point, Rect, Size};
use vo::scene::taffy::prelude::{AlignItems, JustifyContent};
use vo::scene::Scene;
use vo::{FontRegistry, ShapedLayout};
use vo_render::offscreen::write_png;
use vo_render::Offscreen;

const WORDS: [&str; 5] = ["open", "the", "settings", "panel", "now"];
/// When each word arrives, in seconds.
const AT: [f64; 5] = [0.4, 0.85, 1.25, 1.9, 2.4];
/// The moments in the output, one row each.
const MOMENTS: [f64; 8] = [0.2, 0.45, 0.55, 0.9, 1.35, 1.6, 2.0, 3.2];

const W: f32 = 600.0;
const H: f32 = 140.0;
const PILL_H: f32 = 34.0;
/// Space inside the pill before the first word and after the last.
const PAD: f32 = 18.0;
const TEXT: f32 = 20.0;

fn accent() -> Color {
    Color::hex(0x4cc9f0, 1.0)
}

/// Each word's box width: a rough advance for the font size, the same in the layout and the
/// canvas so the bulges sit on the words.
fn word_w(w: &str) -> f32 {
    w.chars().count() as f32 * 0.56 * TEXT + 16.0
}

/// The pill and its group members at `t`.
fn members(t: f64, rect: Rect, reduced: bool) -> (Rect, Vec<GroupPrim>) {
    let total: f32 = WORDS.iter().map(|w| word_w(w)).sum::<f32>() + 2.0 * PAD;
    let pill = Rect::new(rect.x + 0.5 * (rect.w - total), rect.y + 0.5 * (rect.h - PILL_H), total, PILL_H);
    let speaking = t >= AT[0] && t < AT[AT.len() - 1] + 0.6;
    let (amp, phase) = if reduced || !speaking { (0.0, 0.0) } else { (1.0, (t * 0.6) as f32) };
    let mut prims = vec![GroupPrim::capsule(pill).with_ripple(amp, 48.0, phase)];
    let mut x = pill.x + PAD;
    for (i, w) in WORDS.iter().enumerate() {
        let ww = word_w(w);
        // Grows when the word arrives, relaxes when the next one does.
        let next = AT.get(i + 1).map_or(0.0, |n| bulge_grow(t - n, reduced));
        let size = bulge_grow(t - AT[i], reduced) * (1.0 - next);
        if size > 0.01 {
            prims.push(GroupPrim::bulge(pill, x + 0.5 * ww, ww + 10.0, PILL_H + 16.0 * size));
        }
        x += ww;
    }
    (pill, prims)
}

fn draw(t: f64, rect: Rect, p: &mut Painter, reduced: bool) {
    let (pill, prims) = members(t, rect, reduced);
    let g = p.shape_group(&prims, 14.0);
    p.shape(vo::scene::display::ShapeInstance { fill: Paint::glow(accent(), 10.0, 0.7), ..g });
    let fill = Paint::linear(
        Point::new(0.0, pill.y - 10.0),
        Color::hex(0x1b2a44, 1.0),
        Point::new(0.0, pill.bottom() + 10.0),
        Color::hex(0x0c1424, 1.0),
    );
    p.shape(vo::scene::display::ShapeInstance { fill, ..g }.with_stroke(Paint::solid(accent()), 1.5));
}

fn view(t: f64, reduced: bool) -> Element<()> {
    let canvas = Element::canvas("pill", f64::INFINITY, move |t, rect, p| draw(t, rect, p, reduced))
        .size(W, H)
        .layout(|s| s.position = vo::scene::taffy::style::Position::Absolute);
    let words = Element::row("words")
        .size(W, H)
        .justify(JustifyContent::CENTER)
        .align_items(AlignItems::CENTER)
        .layout(|s| s.position = vo::scene::taffy::style::Position::Absolute)
        .children(WORDS.iter().zip(AT).enumerate().map(|(i, (w, at))| {
            let shown = if t >= at { 1.0 } else { 0.0 };
            Element::row(format!("box{i}"))
                .size(word_w(w), H)
                .justify(JustifyContent::CENTER)
                .align_items(AlignItems::CENTER)
                .child(
                    Element::label(format!("w{i}"), w.to_string())
                        .text_style(TextStyle::new(0, TEXT, TEXT * 1.2))
                        .color(Color::hex(0xeaf6ff, 1.0))
                        .opacity(shown),
                )
        }));
    Element::panel("root").size(W, H).fill(Color::hex(0x0b0f16, 1.0)).child(canvas).child(words)
}

fn main() {
    let mut out = PathBuf::from("/var/tmp/claude/capsule-bulge");
    let mut reduced = false;
    let mut scale = 1.0f32;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out takes a directory").into(),
            "--reduced" => reduced = true,
            "--scale" => scale = args.next().and_then(|v| v.parse().ok()).expect("--scale takes a number"),
            other => panic!("unknown argument {other}"),
        }
    }
    let fonts = FontRegistry::new();
    let path = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
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
    let mut off = Offscreen::with_fonts(&device, &queue, fonts.clone());
    let (w, h) = ((W * scale).ceil() as u32, (H * scale).ceil() as u32);
    let mut sheet = Vec::with_capacity((w * h * 4) as usize * MOMENTS.len());
    for t in MOMENTS {
        let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
        scene.set_viewport(0.0, Size::new(W, H), scale);
        scene.apply(view(t, reduced), t);
        let list = scene.sample(t);
        sheet.extend(off.render_to_rgba(&list, [w, h], scale));
    }
    std::fs::create_dir_all(&out).expect("create the output directory");
    let file = out.join(if reduced { "capsule-bulge-reduced.png" } else { "capsule-bulge.png" });
    write_png(&file, w, h * MOMENTS.len() as u32, &sheet).expect("write the sheet");
    println!("capsule_bulge: {} moments at {scale}x, {}", MOMENTS.len(), file.display());
}
