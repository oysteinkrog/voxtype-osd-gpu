//! Three wildcard motion studies on one contact sheet, each a row of frames at 600x300:
//!
//! 1. Depth stack history (MO-86, `vo_scene::depth_stack`): older lines recede on glass
//!    sheets behind the flat reading line; a new line pushes the stack back with parallax.
//! 2. Kinetic lattice (MO-85, `vo_scene::lattice`): strands between words, bow by pause,
//!    weight by loudness, pulled taut as each word arrives.
//! 3. Typographic sound sculpture (MO-88, `vo_3d::Sculpture`): letters with extruded bodies
//!    whose depth follows loudness, laid flat when the sentence is finished.
//!
//!   cargo run --release -p vo-3d --example wild_space -- [--out DIR] [--reduced]
//!   cargo run --release -p vo-3d --example wild_space -- --bench
//!
//! Writes `DIR/wild_space.png` (default DIR `/var/tmp/claude/fw-wild-space`). `--bench`
//! prints GPU time per frame for each study at 600x300.

use std::path::PathBuf;
use std::sync::Arc;

use vo_3d::*;
use vo_render::offscreen::{write_png, FORMAT};
use vo_render::{Offscreen, Renderer, Target};
use vo_scene::depth_stack::{DepthStack, Place, Sheet};
use vo_scene::element::{Element, Key, TextStyle};
use vo_scene::font::FontRegistry;
use vo_scene::geom::{Color, Point, Size};
use vo_scene::lattice::{anchors, LatticeLook, LatticeWord};
use vo_scene::text::{ShapedLayout, TextLayout};
use vo_scene::{DisplayList, Scene};

const W: u32 = 600;
const H: u32 = 300;
const BG: u32 = 0x0d1117;
const TEXT: u32 = 0xe8edf4;

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.set_default_fallbacks(&[fonts.asset("sans").unwrap()]);
    fonts
}

fn bg(child: Element<()>) -> Element<()> {
    Element::panel("root").size(W as f32, H as f32).fill(Color::hex(BG, 1.0)).child(child)
}

fn at(e: Element<()>, x: f32, y: f32) -> Element<()> {
    use vo_scene::taffy::prelude::{auto, length};
    e.layout(move |s| {
        s.position = vo_scene::taffy::Position::Absolute;
        s.inset = vo_scene::taffy::Rect { left: length(x), top: length(y), right: auto(), bottom: auto() };
    })
}

/// A scene with `views` applied at their times, sampled at `t`.
fn play(fonts: &FontRegistry, views: Vec<(f64, Element<()>)>, t: f64) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W as f32, H as f32), 1.0);
    for (at, v) in views {
        if at <= t {
            scene.apply(v, at);
        }
    }
    scene.sample(t)
}

// 1. Depth stack.

const STACK: [&str; 5] = [
    "Schedule the design review",
    "Send the customer update",
    "Book the team sync",
    "Draft the product brief",
    "Share it with the team",
];

fn stack_view(fonts: &FontRegistry, n: usize, reduced: bool) -> Element<()> {
    let font = fonts.asset("sans").unwrap();
    let style = TextStyle::new(font, 24.0, 30.0);
    let s = DepthStack {
        sheet: Some(Sheet {
            fill: Color::hex(0x16222c, 0.88),
            edge: Color::hex(0x9fd8ff, 0.35),
            edge_width: 1.0,
            radius: 6.0,
            pad: (16.0, 8.0),
        }),
        ..DepthStack::default()
    };
    let lines: Vec<(Key, &str)> = STACK[..n].iter().map(|l| (Key::from(l.to_string()), *l)).collect();
    let content = move |text: &&str, d: usize, p: &Place| {
        Element::label(format!("t{d}"), text.to_string())
            .text_style(style)
            .color(Color::hex(TEXT, 1.0))
            .text_blur(p.blur)
    };
    let stack = s.view("stack", Size::new(520.0, 230.0), Size::new(380.0, 46.0), &lines, content, reduced);
    bg(at(stack, 60.0, 40.0))
}

fn stack_frame(fonts: &FontRegistry, t: f64, reduced: bool) -> DisplayList {
    let views = (3..=5).map(|n| ((n - 3) as f64, stack_view(fonts, n, reduced))).collect();
    play(fonts, views, t)
}

// 2. Kinetic lattice.

const LATTICE: [(&str, f32, f32); 7] = [
    ("The", 0.0, 0.5),
    ("future", 0.42, 0.8),
    ("feels", 0.34, 0.6),
    ("closer", 0.42, 0.9),
    ("with", 0.34, 0.4),
    ("every", 0.34, 0.55),
    ("day", 0.34, 0.75),
];

fn lattice_view(fonts: &FontRegistry, n: usize, reduced: bool) -> Element<()> {
    let font = fonts.asset("sans").unwrap();
    let style = TextStyle::new(font, 28.0, 34.0);
    let words: Vec<&str> = LATTICE[..n].iter().map(|w| w.0).collect();
    let mut at_s = 0.0;
    let timed: Vec<LatticeWord> = LATTICE[..n]
        .iter()
        .map(|&(_, pause, loud)| {
            at_s += pause as f64;
            LatticeWord { pause, loud, at: at_s }
        })
        .collect();
    let layout = ShapedLayout::new(fonts.clone());
    let nodes = anchors(&layout, &words, &style, None);
    let mono = TextStyle::new(fonts.asset("mono").unwrap_or(font), 10.0, 12.0);
    let look = LatticeLook { label: Some(mono), ..LatticeLook::default() };
    let size = Size::new(560.0, 120.0);
    let text = Element::label("words", words.join(" ")).text_style(style).color(Color::hex(TEXT, 1.0));
    let lattice = look.view("lattice", size, timed.into(), nodes.into(), reduced);
    bg(at(
        Element::layer("box").size(size.w, size.h).child(at(text, 0.0, 0.0)).child(at(lattice, 0.0, 0.0)),
        24.0,
        100.0,
    ))
}

fn lattice_frame(fonts: &FontRegistry, t: f64, reduced: bool) -> DisplayList {
    let mut at_s = 0.0;
    let views = (1..=LATTICE.len())
        .map(|n| {
            at_s += LATTICE[n - 1].1 as f64;
            (at_s, lattice_view(fonts, n, reduced))
        })
        .collect();
    play(fonts, views, t)
}

// 3. Sound sculpture.

const SCULPT: &str = "the future feels quiet now";
const LOUD: [f32; 5] = [0.25, 0.7, 1.0, 0.55, 0.3];

fn sculpture(fonts: &FontRegistry) -> Arc<Sculpture> {
    let font = fonts.asset("sans").unwrap();
    Arc::new(Sculpture::new(fonts.get(font).unwrap(), font, vo_3d::sculpture::DEFAULT_CHARSET, SculptLook::default()))
}

/// Feeds the sculpture the first `n` words at `t`, or lays it flat with `n` = 0.
fn sculpt_events(fonts: &FontRegistry, s: &Sculpture, until: f64) {
    let font = fonts.asset("sans").unwrap();
    let style = TextStyle::new(font, 44.0, 52.0);
    let shaped = ShapedLayout::new(fonts.clone()).layout(SCULPT, &style, None);
    let starts: Vec<usize> = SCULPT.match_indices(' ').map(|(i, _)| i + 1).collect();
    let word_of = |cluster: usize| starts.iter().filter(|&&s| s <= cluster).count();
    for n in 1..=LOUD.len() {
        let t = 0.3 * (n - 1) as f64;
        if t > until {
            return;
        }
        let letters: Vec<Letter> = shaped
            .glyphs
            .iter()
            .enumerate()
            .filter(|(_, g)| word_of(g.cluster as usize) < n)
            .map(|(i, g)| Letter {
                glyph: g.glyph,
                font: g.font,
                pos: Point::new(40.0 + g.pos.x, 100.0 + g.pos.y),
                size: 44.0,
                key: i as u64,
                depth: 30.0 * LOUD[word_of(g.cluster as usize)],
            })
            .collect();
        s.set(t, &letters, None);
    }
    if until >= 2.2 {
        s.flatten(2.2);
    }
}

fn sculpt_frame(fonts: &FontRegistry, t: f64, reduced: bool) -> DisplayList {
    let s = sculpture(fonts);
    if !reduced {
        sculpt_events(fonts, &s, t);
    }
    sculpt_view(fonts, &s, t)
}

fn sculpt_view(fonts: &FontRegistry, s: &Arc<Sculpture>, t: f64) -> DisplayList {
    let font = fonts.asset("sans").unwrap();
    let style = TextStyle::new(font, 44.0, 52.0);
    let shown = (t / 0.3).floor() as usize + 1;
    let text: Vec<&str> = SCULPT.split(' ').take(shown.min(LOUD.len())).collect();
    let view = bg(Element::layer("box")
        .size(W as f32, H as f32)
        .child(at(Element::gpu("sculpt", s.source()).size(W as f32, H as f32), 0.0, 0.0))
        .child(at(
            Element::label("words", text.join(" ")).text_style(style).color(Color::hex(TEXT, 1.0)).width(560.0),
            40.0,
            100.0,
        )));
    play(fonts, vec![(0.0, view)], t)
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
        label: Some("wild-space"),
        required_features: wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
        ..Default::default()
    }))
    .expect("a GPU device")
}

fn bench(fonts: FontRegistry) {
    let (device, queue) = gpu();
    let period = queue.get_timestamp_period() as f64;
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let buf = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
    let resolve = buf(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buf(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    let target = Target::new(&device, W, H, FORMAT);
    let sculpt = sculpture(&fonts);
    sculpt_events(&fonts, &sculpt, 10.0);
    let lists: [(&str, Box<dyn Fn(f64) -> DisplayList>); 3] = [
        ("depth stack", Box::new(|t| stack_frame(&fonts, 1.0 + t, false))),
        ("lattice", Box::new(|t| lattice_frame(&fonts, 1.5 + t, false))),
        ("sculpture", Box::new(|t| sculpt_view(&fonts, &sculpt, 0.5 + t))),
    ];
    for (name, list) in lists {
        let mut renderer = Renderer::with_fonts(&device, &queue, fonts.clone());
        install(&mut renderer);
        let mut times = Vec::new();
        for i in 0..80 {
            let l = list(i as f64 / 120.0);
            let mut enc = device.create_command_encoder(&Default::default());
            enc.write_timestamp(&qs, 0);
            renderer.encode(&l, 1.0, &target, &mut enc);
            enc.write_timestamp(&qs, 1);
            enc.resolve_query_set(&qs, 0..2, &resolve, 0);
            enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, 16);
            queue.submit([enc.finish()]);
            read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let ts: [u64; 2] = bytemuck::pod_read_unaligned(&read.slice(..).get_mapped_range().unwrap()[..16]);
            read.unmap();
            if i >= 10 {
                times.push((ts[1] - ts[0]) as f64 * period / 1e6);
            }
        }
        times.sort_by(f64::total_cmp);
        println!(
            "wild_space bench: {name:12} 600x300: {:.3} ms median, {:.3} ms p95",
            times[times.len() / 2],
            times[times.len() * 95 / 100]
        );
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("/var/tmp/claude/fw-wild-space");
    let mut reduced = false;
    let fonts = fonts();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--reduced" => reduced = true,
            "--bench" => return bench(fonts),
            _ => panic!("unknown option {a}"),
        }
    }
    let (device, queue) = gpu();
    let rows: [Vec<f64>; 3] =
        [vec![0.9, 1.06, 1.12, 1.25, 2.5], vec![1.18, 1.56, 1.62, 1.9, 3.5], vec![0.3, 0.95, 1.6, 2.4, 3.5]];
    let (cols, gap) = (5u32, 8u32);
    let (sw, sh) = (cols * W + (cols + 1) * gap, 3 * H + 4 * gap);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    for (r, times) in rows.iter().enumerate() {
        for (c, &t) in times.iter().enumerate() {
            // A fresh renderer per frame: a sculpture's meshes upload once per source.
            let mut off = Offscreen::with_fonts(&device, &queue, fonts.clone());
            install(&mut off.renderer);
            let list = match r {
                0 => stack_frame(&fonts, t, reduced),
                1 => lattice_frame(&fonts, t, reduced),
                _ => sculpt_frame(&fonts, t, reduced),
            };
            let rgba = off.render_to_rgba(&list, [W, H], 1.0);
            let (x0, y0) = (gap + c as u32 * (W + gap), gap + r as u32 * (H + gap));
            for y in 0..H {
                let src = (y * W * 4) as usize;
                let dst = (((y0 + y) * sw + x0) * 4) as usize;
                sheet[dst..dst + (W * 4) as usize].copy_from_slice(&rgba[src..src + (W * 4) as usize]);
            }
        }
    }
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join(if reduced { "wild_space_reduced.png" } else { "wild_space.png" });
    write_png(&path, sw, sh, &sheet).unwrap();
    println!("wild_space: wrote {}", path.display());
}
