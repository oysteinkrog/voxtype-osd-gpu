//! The raymarched hero (`vo_3d::hero`) over time: a liquid chrome blob (concept 05), an
//! aurora orb (01), a jellyfish bell (15), and two scenes of several parts with see-through
//! skins: a jellyfish (bell, four membranes, six drip beads) and chrome with an aurora inside.
//! Each is driven by a made-up voice level so it swells and ripples while speech runs and
//! settles in the pauses. Last, the chrome blob over a panel and a waveform that it reflects
//! (EG-17, `vo_3d::env`): the layer under it captures a backdrop, and the chrome's environment
//! is built from it.
//!
//!   cargo run --release -p vo-3d --example hero_demo -- [--out DIR]
//!   cargo run --release -p vo-3d --example hero_demo -- --bench
//!
//! The default writes `DIR/hero_demo.png` (default DIR `/var/tmp/claude/fw-hero`): the five
//! heroes at six moments, one row each, a row of the two scenes under reduced motion, and a
//! row of the reflecting chrome over its panel and waveform.
//! `--bench` prints GPU time per frame (timestamp queries) for each hero alone, in a box 0.55
//! of the frame's height wide plus its halo, in a 600x300 and a 1920x1080 frame, at 48 and at
//! 24 march steps; then the two scenes in a 200 px box at 600x300 and 48 steps against their
//! 0.5 ms p95 budget; then the chrome over the panel and waveform at 600x300 with and without
//! its environment, against the 0.1 ms p95 the environment may add, and the environment's
//! memory. Without timestamp queries it says so and exits 0.

use std::path::PathBuf;
use std::sync::Arc;

use vo_3d::*;
use vo_render::offscreen::{write_png, FORMAT};
use vo_render::{Offscreen, Renderer, Target};
use vo_scene::display::BackdropRes;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::gpu::GpuSource;
use vo_scene::taffy::{self, AlignItems, JustifyContent};
use vo_scene::{DisplayList, Scene};

/// A speech-like level: syllables in bursts, with a pause every few seconds.
fn level(t: f64) -> f32 {
    let t = t as f32;
    let syllable = (t * 9.0).sin().abs();
    let phrase = (0.5 + 0.5 * (t * 1.7).sin()).powf(0.7);
    let pause = if (t * 0.3).fract() > 0.75 { 0.0 } else { 1.0 };
    (syllable * phrase * pause).clamp(0.0, 1.0)
}

/// The level smoothed the way a theme would ease it: a quick rise, a slow fall.
fn eased(t: f64) -> f32 {
    (0..12).map(|i| level(t - i as f64 * 0.03) * (1.0 - i as f32 / 12.0)).sum::<f32>() / 6.5
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Chrome,
    Aurora,
    Bell,
    Jellyfish,
    AuroraChrome,
    /// The chrome blob reflecting the layers under it.
    ChromeEnv,
}

#[derive(Debug)]
struct Hero {
    kind: Kind,
    steps: u32,
    reduced: bool,
}

impl HeroSource for Hero {
    fn frame(&self, t: f64, _size: Size) -> HeroFrame {
        let v = eased(t).min(1.0);
        let phase = t as f32;
        let f = match self.kind {
            Kind::Chrome | Kind::ChromeEnv => HeroFrame {
                shape: HeroShape::Blob { lobes: 5 },
                look: HeroLook::Chrome {
                    tint: Color::hex(0xdfe8ff, 1.0),
                    top: Color::hex(0x8ea4d8, 1.0),
                    bottom: Color::hex(0x06070c, 1.0),
                    light: Color::hex(0xffffff, 1.0),
                    env: matches!(self.kind, Kind::ChromeEnv).then(Env::default),
                },
                stretch: [1.0, 0.5 + 0.12 * v, 0.8],
                wobble: 0.25 + 0.75 * v,
                phase,
                spin: 0.2 * phase,
                ..Default::default()
            },
            Kind::Aurora => HeroFrame {
                shape: HeroShape::Orb,
                look: HeroLook::Glow {
                    a: Color::hex(0x2b6cff, 1.0),
                    b: Color::hex(0x8a4dff, 1.0),
                    c: Color::hex(0x2de0c8, 1.0),
                    rim: Color::hex(0xd8c8ff, 1.0),
                    body: 0.85,
                    halo: 0.35 + 0.4 * v,
                    halo_width: 0.12,
                },
                stretch: [0.72 + 0.1 * v; 3],
                wobble: v,
                phase: 0.6 * phase,
                spin: 0.3 * phase,
                ..Default::default()
            },
            Kind::Bell => HeroFrame {
                shape: HeroShape::Bell,
                look: HeroLook::Glow {
                    a: Color::hex(0x1846ff, 1.0),
                    b: Color::hex(0x6a3cff, 1.0),
                    c: Color::hex(0x23c8ff, 1.0),
                    rim: Color::hex(0x9fe8ff, 1.0),
                    body: 0.35,
                    halo: 0.25 + 0.35 * v,
                    halo_width: 0.1,
                },
                stretch: [0.8; 3],
                wobble: 0.6 * v,
                phase,
                spin: 0.15 * phase,
                ..Default::default()
            },
            Kind::Jellyfish => HeroFrame::jellyfish(phase, v, self.reduced),
            Kind::AuroraChrome => HeroFrame::aurora_chrome(phase, v, self.reduced),
        };
        HeroFrame { steps: self.steps, ..f }
    }
}

/// One hero centred in a frame of `size` in a `d` px box.
fn view(h: &Arc<dyn GpuSource>, size: Size, d: f32) -> Element<()> {
    Element::panel("root")
        .size(size.w, size.h)
        .fill(Color::hex(0x07090f, 1.0))
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
        .child(Element::gpu("hero", h.clone()).size(d, d))
}

fn sample(h: &Arc<dyn GpuSource>, size: Size, d: f32, t: f64) -> DisplayList {
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(view(h, size, d), 0.0);
    scene.sample(t)
}

fn at(e: Element<()>, x: f32, y: f32) -> Element<()> {
    e.layout(|s| {
        s.position = taffy::style::Position::Absolute;
        s.inset.left = taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = taffy::style::LengthPercentageAuto::length(y);
    })
}

/// The hero over a panel and a waveform (the level over the last half second) that it
/// reflects: the hero's layer captures what is under it.
fn reflecting_view(h: &Arc<dyn GpuSource>, size: Size, d: f32, t: f64) -> Element<()> {
    let wave = Element::row("wave").gap(0.012 * size.w).align_items(AlignItems::CENTER).children((0..24).map(|i| {
        let v = eased(t - (23 - i) as f64 * 0.02);
        let hgt = size.h * (0.04 + 0.3 * v);
        Element::panel(i as u64).size(0.012 * size.w, hgt).radius(0.006 * size.w).fill(Color::hex(0x35c2ff, 1.0))
    }));
    let panel = Element::panel("panel")
        .size(0.86 * size.w, 0.36 * size.h)
        .radius(0.04 * size.h)
        .fill(Color::hex(0x1b2238, 1.0))
        .border(Color::hex(0x3a4668, 1.0), 1.0);
    let mut layer = Element::layer("hero-layer").size(size.w, size.h).backdrop(BackdropRes::Half, Color::TRANSPARENT);
    layer = layer.child(at(Element::gpu("hero", h.clone()).size(d, d), 0.5 * (size.w - d), 0.32 * size.h - 0.5 * d));
    Element::panel("root")
        .size(size.w, size.h)
        .fill(Color::hex(0x07090f, 1.0))
        .child(at(panel, 0.07 * size.w, 0.56 * size.h))
        .child(at(
            Element::panel("accent").size(0.18 * size.w, 0.05 * size.h).fill(Color::hex(0xff5a3c, 1.0)),
            0.12 * size.w,
            0.62 * size.h,
        ))
        .child(at(wave, 0.38 * size.w, 0.6 * size.h))
        .child(layer)
}

fn sample_reflecting(h: &Arc<dyn GpuSource>, size: Size, d: f32, t: f64) -> DisplayList {
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(reflecting_view(h, size, d, t), 0.0);
    scene.sample(t)
}

/// A device, with timestamp queries when the adapter has them (the bool).
fn gpu() -> (wgpu::Device, wgpu::Queue, bool) {
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
    let ts = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    let timed = adapter.features().contains(ts);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("hero-demo"),
        required_features: if timed { ts } else { wgpu::Features::empty() },
        ..Default::default()
    }))
    .expect("a GPU device");
    (device, queue, timed)
}

const KINDS: [(&str, Kind); 5] = [
    ("chrome", Kind::Chrome),
    ("aurora", Kind::Aurora),
    ("bell", Kind::Bell),
    ("jellyfish", Kind::Jellyfish),
    ("aurora-chrome", Kind::AuroraChrome),
];

/// The scenes' budget: p95 per frame at 600x300 in a 200 px box, 48 steps.
const SCENE_BUDGET_MS: f64 = 0.5;

fn bench() {
    let (device, queue, timed) = gpu();
    if !timed {
        println!("hero_demo bench: this GPU has no timestamp queries; nothing measured");
        return;
    }
    let period = queue.get_timestamp_period() as f64;
    let mut renderer = Renderer::new(&device, &queue);
    install(&mut renderer);
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let buf = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
    let resolve = buf(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buf(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    let mut measure_in = |h: &Arc<dyn GpuSource>, w: u32, ht: u32, d: Option<f32>, reflecting: bool| {
        let size = Size::new(w as f32, ht as f32);
        let target = Target::new(&device, w, ht, FORMAT);
        let mut times = Vec::new();
        for i in 0..140 {
            let (d, t) = (d.unwrap_or(size.h * 0.55), 1.0 + i as f64 / 60.0);
            let list = if reflecting { sample_reflecting(h, size, d, t) } else { sample(h, size, d, t) };
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
        (times[times.len() / 2], times[times.len() * 95 / 100])
    };
    let mut measure = |h: &Arc<dyn GpuSource>, w: u32, ht: u32, d: Option<f32>| measure_in(h, w, ht, d, false);
    for (name, kind) in KINDS {
        for steps in [48, 24] {
            let h = hero(Hero { kind, steps, reduced: false });
            let mut line = format!("hero_demo bench: {name:13} {steps:2} steps");
            for (w, ht) in [(600u32, 300u32), (1920, 1080)] {
                let (median, p95) = measure(&h, w, ht, None);
                line += &format!("  {w}x{ht}: {median:.3} ms median, {p95:.3} ms p95;");
            }
            println!("{line}");
        }
    }
    for (name, kind) in [("jellyfish", Kind::Jellyfish), ("aurora-chrome", Kind::AuroraChrome)] {
        let (median, p95) = measure(&hero(Hero { kind, steps: 48, reduced: false }), 600, 300, Some(200.0));
        let verdict = if p95 <= SCENE_BUDGET_MS { "within" } else { "OVER" };
        println!(
            "hero_demo bench: {name:13} 200 px box, 600x300, 48 steps: {median:.3} ms median, {p95:.3} ms p95, \
             {verdict} the {SCENE_BUDGET_MS} ms budget"
        );
    }
    // The environment's cost: the same scene (panel, waveform, backdrop), chrome with and
    // without it. The source key is None, so it is rebuilt on every 1/30 s tick.
    let (m0, p0) = measure_in(&hero(Hero { kind: Kind::Chrome, steps: 48, reduced: false }), 600, 300, None, true);
    let (m1, p1) = measure_in(&hero(Hero { kind: Kind::ChromeEnv, steps: 48, reduced: false }), 600, 300, None, true);
    let added = p1 - p0;
    let verdict = if added <= ENV_BUDGET_MS { "within" } else { "OVER" };
    println!(
        "hero_demo bench: chrome over panel and waveform, 600x300: without env {m0:.3} ms median, {p0:.3} ms p95; \
         with env {m1:.3} ms median, {p1:.3} ms p95; adds {added:.3} ms p95, {verdict} the {ENV_BUDGET_MS} ms budget"
    );
    println!(
        "hero_demo bench: env texture {}x{} Rgba16Float, {} KB",
        env::ENV_SIZE[0],
        env::ENV_SIZE[1],
        env::ENV_BYTES / 1024
    );
}

/// What the chrome's environment may add, p95 per frame at 600x300.
const ENV_BUDGET_MS: f64 = 0.1;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("/var/tmp/claude/fw-hero");
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--bench" => return bench(),
            _ => panic!("unknown option {a}"),
        }
    }
    let (w, h) = (300u32, 200u32);
    let size = Size::new(w as f32, h as f32);
    let (device, queue, _) = gpu();
    let mut off = Offscreen::new(&device, &queue);
    install(&mut off.renderer);
    let (cols, gap) = (6u32, 6u32);
    // The five heroes, then a row of the two scenes under reduced motion, three moments each,
    // then the chrome reflecting its panel and waveform.
    let rows = KINDS.len() as u32 + 2;
    let (sw, sh) = (cols * w + (cols + 1) * gap, rows * h + (rows + 1) * gap);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    let rows_of = KINDS.iter().map(|(_, k)| (*k, false)).chain([(Kind::Jellyfish, true), (Kind::AuroraChrome, true)]);
    let mut cells = Vec::new();
    for (i, (kind, reduced)) in rows_of.enumerate() {
        if reduced {
            // Reduced motion: three columns each, the scene at three moments.
            for c in 0..3u32 {
                cells.push((KINDS.len(), (i - KINDS.len()) as u32 * 3 + c, kind, true, c));
            }
        } else {
            for c in 0..cols {
                cells.push((i, c, kind, false, c));
            }
        }
    }
    for c in 0..cols {
        cells.push((KINDS.len() + 1, c, Kind::ChromeEnv, false, c));
    }
    for (r, c, kind, reduced, moment) in cells {
        let src = hero(Hero { kind, steps: 48, reduced });
        let t = 0.4 + 0.35 * moment as f64;
        let list = match kind {
            Kind::ChromeEnv => sample_reflecting(&src, size, size.h * 0.4, t),
            _ => sample(&src, size, size.h * 0.55, t),
        };
        let rgba = off.render_to_rgba(&list, [w, h], 1.0);
        let (x0, y0) = (gap + c * (w + gap), gap + r as u32 * (h + gap));
        for y in 0..h {
            let s = (y * w * 4) as usize;
            let d = (((y0 + y) * sw + x0) * 4) as usize;
            sheet[d..d + (w * 4) as usize].copy_from_slice(&rgba[s..s + (w * 4) as usize]);
        }
    }
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join("hero_demo.png");
    write_png(&path, sw, sh, &sheet).unwrap();
    println!("hero_demo: wrote {}", path.display());
}
