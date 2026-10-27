//! HUD kit: the building blocks for radial, angular and audio-driven overlay designs.
//!
//! - **reach**: effect reach and the surface margin. A glowing toast on a surface only as big
//!   as its box, then on the surface `surface_for` sizes from `effect_reach`.
//! - **polar**: a gauge of ticks drawing on, and markers on a ring sliding along the arc when
//!   one goes.
//! - **along_path**: ticks and chevrons along an S-curve, the chevrons turned along it.
//! - **frame**: cut panels: a cyberpunk panel whose outline draws on over marching stripes,
//!   and a mecha plate with notched corners, tabs and cut-out windows.
//! - **spectrum_ring**: a radial spectrum fed by the signal bus's mel bands, here from a
//!   recorded synthetic voice: a mirrored full ring with glow and a half ring growing both
//!   ways.
//! - **grids**: a dot grid and a line grid inside panels, and a graticule with minor dots,
//!   major lines, ticks and mono labels.
//!
//! `cargo run -p vo --example hud_kit -- [--out DIR] [--bench]` writes `contact-sheet.png`
//! (rows of frames per board) to DIR, or with `--bench` prints the GPU time per frame of
//! each board at 600 x 300 (scale 1) and at 1920 x 1080 (scale 3.2), from timestamp queries.

use std::f32::consts::{PI, TAU};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use vo::scene::element::Element;
use vo::scene::element::TextStyle;
use vo::scene::font::FontRegistry;
use vo::scene::geom::{Color, Point, Size};
use vo::scene::path::Contour;
use vo::scene::text::ShapedLayout;
use vo::scene::{
    surface_for, Axis, Corner, Edge, Frame, Graticule, Grid, Notch, Orient, Prop, Scene, Spectrum, Stripes, Ticks,
};
use vo_core::signals::{SignalFrame, Signals, SignalsCell};
use vo_core::spring::Spring;
use vo_render::offscreen::write_png;
use vo_render::{Offscreen, Renderer, Target};

/// A view of a board of `Size`.
type View = fn(Size) -> Element<()>;

/// One board: a view of a `Size`, an optional second view applied at a time, the times its
/// sheet row shows, and its name.
struct Board {
    name: &'static str,
    view: View,
    then: Option<(f64, View)>,
    times: &'static [f64],
}

const BG: u32 = 0x0b0d12;

fn boards() -> Vec<Board> {
    vec![
        Board { name: "reach: tight surface", view: reach_tight, then: None, times: &[0.0] },
        Board { name: "reach: computed margin", view: reach_fitted, then: None, times: &[0.0] },
        Board {
            name: "polar: ticks draw on",
            view: |s| gauge(s, 0.0),
            then: Some((0.0, |s| gauge(s, 1.0))),
            times: &[0.05, 0.15, 0.3, 1.0],
        },
        Board {
            name: "polar: a marker goes, the rest slide",
            view: |s| markers(s, 8),
            then: Some((0.0, |s| markers(s, 7))),
            times: &[0.0, 0.1, 0.25, 1.0],
        },
        Board { name: "along_path: S-curve", view: snake, then: None, times: &[0.0] },
        Board {
            name: "frame: cyberpunk, drawing on",
            view: |s| cyber(s, 0.0),
            then: Some((0.0, |s| cyber(s, 1.0))),
            times: &[0.08, 0.2, 0.4, 1.5],
        },
        Board { name: "frame: mecha plate", view: mecha, then: None, times: &[0.0] },
        Board { name: "grids and graticule", view: grids, then: None, times: &[0.0] },
        Board {
            name: "spectrum_ring: from the signal bus",
            view: spectrum,
            then: None,
            times: &[0.3, 0.55, 0.8, 1.05],
        },
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
fn reach_tight(size: Size) -> Element<()> {
    centred(size, Element::layer("cut").clip().size(240.0, 72.0).child(toast()))
}

/// The toast on the surface `surface_for` sizes from its effect reach; the surface outline
/// shows where the transparent margin ends.
fn reach_fitted(size: Size) -> Element<()> {
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

// Repeaters.

const CYAN: u32 = 0x35e0ff;
const STEEL: u32 = 0x5a6a80;

/// 60 ticks inside a ring, every fifth long, the first 35% lit, trimmed to `drawn`.
fn gauge(size: Size, drawn: f32) -> Element<()> {
    let ticks = Element::polar("ticks", 220.0, 220.0, 100.0, -0.5 * PI, TAU)
        .ticks(
            Ticks::new(60, 8.0, 1.5, Color::hex(STEEL, 1.0))
                .major(5, 16.0, 3.0, Color::hex(0xc8d4e0, 1.0))
                .align(-1.0)
                .lit(0.35, Color::hex(CYAN, 1.0)),
        )
        .trim(0.0, drawn)
        .motion(Prop::Trim, Spring::SOFT);
    centred(size, ticks)
}

/// `n` markers on a ring, turned outward, with a hairline ring behind them.
fn markers(size: Size, n: usize) -> Element<()> {
    let ring = Element::polar("markers", 220.0, 220.0, 90.0, -0.5 * PI, TAU)
        .orient(Orient::Outward)
        .ticks(Ticks::new(120, 2.0, 1.0, Color::hex(STEEL, 1.0)))
        .children((0..n).map(|i| {
            let c = if i == 0 { 0xff3d8b } else { CYAN };
            Element::panel(i.to_string()).size(22.0, 6.0).radius(3.0).fill(Color::hex(c, 1.0)).glow(
                Color::hex(c, 1.0),
                4.0,
                0.8,
            )
        }));
    centred(size, ring)
}

/// Ticks and chevrons along an S-curve, the chevrons turned along it.
fn snake(size: Size) -> Element<()> {
    let pts: Vec<Point> = (0..=64)
        .map(|i| {
            let u = i as f32 / 64.0;
            Point::new(20.0 + 400.0 * u, 110.0 + 70.0 * (u * TAU).sin())
        })
        .collect();
    let contour = Arc::new(Contour::polyline(&pts, false));
    let rail = Element::along_path("rail", 440.0, 220.0, contour.clone(), 0.0, 1.0)
        .ticks(Ticks::new(61, 10.0, 1.2, Color::hex(STEEL, 1.0)).major(10, 18.0, 2.0, Color::hex(CYAN, 1.0)))
        .layout(overlay);
    let chevrons = Element::along_path("chevrons", 440.0, 220.0, contour, 0.04, 0.96)
        .orient(Orient::Along)
        .path_offset(-16.0)
        .children(
            (0..12).map(|i| Element::panel(i.to_string()).size(14.0, 5.0).radius(2.5).fill(Color::hex(0xff3d8b, 1.0))),
        )
        .layout(overlay);
    centred(size, Element::layer("snake").size(440.0, 220.0).child(rail).child(chevrons))
}

// Frames.

/// Uneven chamfers, a slanted notch, marching stripes, a hatched band and a glowing outline
/// trimmed to `drawn`.
fn cyber(size: Size, drawn: f32) -> Element<()> {
    let yellow = Color::hex(0xf5e642, 1.0);
    let frame = Frame::new()
        .corners([Corner::Chamfer(28.0), Corner::Chamfer(8.0), Corner::Chamfer(36.0), Corner::Chamfer(8.0)])
        .notch(Notch::new(Edge::Top, 0.64, 70.0, 9.0).slant(9.0))
        .fill(Color::hex(0x0d0f1a, 0.92))
        .stripes(Stripes::new(-1.05, 14.0, 0.3, Color::hex(0xf5e642, 0.05)).speed(14.0))
        .hatch(10.0, Stripes::hatch(-1.05, 6.0, 2.0, Color::hex(0xf5e642, 0.5)))
        .stroke(2.0, yellow)
        .glow(4.0);
    let panel = Element::frame("cyber", frame).size(340.0, 180.0).trim(0.0, drawn).motion(Prop::Trim, Spring::SOFT);
    centred(size, panel)
}

/// Notched corners, tabs on the left, a slanted notch below and two cut-out windows.
fn mecha(size: Size) -> Element<()> {
    let frame = Frame::new()
        .corners([Corner::Notch(12.0), Corner::Chamfer(22.0), Corner::Notch(12.0), Corner::Round(16.0)])
        .notch(Notch::new(Edge::Left, 0.35, 22.0, -6.0))
        .notch(Notch::new(Edge::Left, 0.65, 22.0, -6.0))
        .notch(Notch::new(Edge::Bottom, 0.5, 60.0, 8.0).slant(8.0))
        .cut_rect(250.0, 30.0, 50.0, 16.0, 4.0)
        .cut_rect(250.0, 54.0, 50.0, 16.0, 4.0)
        .fill(Color::hex(0x1c2430, 1.0))
        .hatch(6.0, Stripes::hatch(0.8, 5.0, 1.0, Color::hex(0x9fb3c8, 0.25)))
        .stroke(3.0, Color::hex(0x9fb3c8, 1.0));
    centred(size, Element::frame("mecha", frame).size(340.0, 180.0))
}

// Grids.

fn grids(size: Size) -> Element<()> {
    let steel = Color::hex(0x9fb3c8, 1.0);
    let cyan = 0x35e0ff;
    let dots = Frame::chamfer(14.0)
        .fill(Color::hex(0x1c2430, 1.0))
        .grid(Grid::dots(Size::new(10.0, 10.0), 2.0, Color::hex(0x9fb3c8, 0.45)).origin(Point::new(5.0, 5.0)))
        .stroke(2.0, steel);
    let lines = Frame::new()
        .corners([Corner::Notch(8.0), Corner::Square, Corner::Chamfer(16.0), Corner::Square])
        .fill(Color::hex(0x0d1420, 1.0))
        .grid(Grid::lines(Size::new(12.0, 12.0), 1.0, Color::hex(cyan, 0.09)))
        .grid(Grid::lines(Size::new(60.0, 60.0), 1.5, Color::hex(cyan, 0.5)))
        .stroke(2.0, Color::hex(cyan, 1.0));
    let style = TextStyle::new(0, 11.0, 13.0);
    let graticule = Graticule::new(Color::hex(0xc8d4e0, 0.6), Color::hex(0xc8d4e0, 0.25))
        .x(Axis::new(0.0, 2.0, 0.5, 0.1).format(1, "s"))
        .y(Axis::new(-60.0, 0.0, 20.0, 5.0))
        .minor_dots()
        .widths(1.5, 1.0)
        .border(1.0, Color::hex(0xc8d4e0, 0.8))
        .labels(style, Color::hex(0xc8d4e0, 1.0), 34.0, 20.0);
    let row = Element::row("grids")
        .gap(20.0)
        .child(Element::frame("dots", dots).size(130.0, 130.0))
        .child(Element::frame("lines", lines).size(130.0, 130.0))
        .child(graticule.view("graticule", Size::new(240.0, 150.0)));
    centred(size, row)
}

/// The mono asset font, the only font the boards use (font 0).
fn mono() -> &'static [u8] {
    vo::scene::assets::get("mono").expect("the mono font asset").bytes
}

fn fonts() -> FontRegistry {
    static FONTS: OnceLock<FontRegistry> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let f = FontRegistry::new();
            f.add(mono().to_vec()).expect("the mono font");
            f
        })
        .clone()
}

// Spectrum.

/// Three seconds of a synthetic voice: two formant humps that wander, in syllables.
fn demo_signals() -> SignalsCell {
    static CELL: OnceLock<SignalsCell> = OnceLock::new();
    CELL.get_or_init(|| {
        let frames = (0..300).map(|i| {
            let t = i as f32 * 0.01;
            let mut f = SignalFrame::silent(t as f64);
            let syllable = (0.5 + 0.5 * (TAU * 2.5 * t).sin()).powf(0.7);
            let (c1, c2) = (5.0 + 3.0 * (TAU * 0.7 * t).sin(), 14.0 + 4.0 * (TAU * 0.45 * t + 1.0).sin());
            for (b, m) in f.mel.iter_mut().enumerate() {
                let x = b as f32;
                let hump = |c: f32, w: f32| (-(x - c) * (x - c) / (2.0 * w * w)).exp();
                *m = (syllable * (0.95 * hump(c1, 2.2) + 0.65 * hump(c2, 2.6)) + 0.05).min(1.0);
            }
            f
        });
        SignalsCell::new(Signals::recorded(frames, 0.0).with_playout(0.0))
    })
    .clone()
}

fn spectrum(size: Size) -> Element<()> {
    let (cyan, magenta) = (Color::hex(0x2fd8ff, 0.9), Color::hex(0xff3df0, 1.0));
    let full = Spectrum::new(cyan, magenta).count(72).mirror().glow(5.0, 0.6);
    let half = Spectrum::new(cyan, magenta).count(40).ring(0.7, 0.9).sweep(PI, PI).both_ways().width(0.7);
    let rings = Element::row("rings")
        .gap(40.0)
        .child(Element::spectrum_ring("full", full).size(220.0, 220.0))
        .child(Element::spectrum_ring("half", half).size(220.0, 220.0));
    centred(size, rings)
}

/// Takes the element out of flow at its parent's top left, so siblings stack.
fn overlay(s: &mut vo::scene::taffy::Style) {
    use vo::scene::taffy::{LengthPercentageAuto, Position};
    s.position = Position::Absolute;
    s.inset.left = LengthPercentageAuto::length(0.0);
    s.inset.top = LengthPercentageAuto::length(0.0);
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
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts())));
    scene.set_viewport(0.0, size, scale);
    scene.set_signals(demo_signals());
    let root = |view: View| Element::panel("bg").size(size.w, size.h).fill(Color::hex(BG, 1.0)).child(view(size));
    scene.apply(root(b.view), 0.0);
    if let Some((at, view)) = b.then {
        if t >= at {
            scene.apply(root(view), at);
        }
    }
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
    off.renderer.add_font(mono().to_vec()).expect("the mono font");
    let cell = Size::new(600.0, 300.0);
    let rows: Vec<Vec<Vec<u8>>> = boards()
        .iter()
        .map(|b| b.times.iter().map(|&t| off.render_to_rgba(&board_list(b, cell, 1.0, t), [600, 300], 1.0)).collect())
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
    r.add_font(mono().to_vec()).expect("the mono font");
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let buf = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
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
