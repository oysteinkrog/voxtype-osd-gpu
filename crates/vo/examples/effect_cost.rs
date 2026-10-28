//! GPU cost of each effect, from timestamps per pass (`vo_render::profile`).
//!
//! Every case draws the same popup-like card (a panel, a title and two lines of text) with
//! one effect on it, at 600x300 and at 1920x1080. A case's cost is its median GPU time per
//! frame; `+ms` is that minus the plain card's, so it is what the effect adds. The table also
//! gives the slowest section (the scene, a layer, a bloom pyramid) and the pooled texture
//! memory.
//!
//!   cargo run --release -p vo --example effect_cost -- [--frames N] [--out TABLE.md]
//!       [--max-ms MS]     exit 1 when a case costs more than MS at 600x300
//!   cargo run --release -p vo --example effect_cost -- --preview   every case, 2 s each
//!   cargo run --release -p vo --example effect_cost -- --sheet OUT.png --frames 11
//!
//! `just budget` runs it and writes the table next to the budget log.

use std::time::Instant;

use vo::scene::depth_stack::{DepthStack, Place, Sheet};
use vo::scene::displace::{Displace, Onset, Ripples};
use vo::scene::display::BackdropRes;
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Point, Rect, Size};
use vo::scene::glass::{Glass, GlassShape};
use vo::scene::material::Material;
use vo::scene::path::StrokeStyle;
use vo::scene::water::{PoolDrop, WaterPool};
use vo::scene::Scene;
use vo::{App, Cx, FontRegistry, Options, Scenario, ShapedLayout};
use vo_render::profile::timestamp_features;
use vo_render::{Renderer, Target};
use vo_sim::water::{Water, WaterConfig, WaterDrop};

/// Seconds each case shows in the preview.
const CASE_SECS: f64 = 2.0;

/// The effects. Each is applied where it works: shape effects on the card's panel, text
/// effects on its labels, layer effects on a layer around the card.
const CASES: &[&str] = &[
    "plain",
    "glow",
    "shadow",
    "text halo",
    "text blur",
    "offscreen",
    "bloom",
    "bloom wide",
    "trail",
    "motion blur",
    "crt style",
    "glitch",
    "backdrop",
    "backdrop half",
    "material backdrop",
    "glass",
    "glass octagon",
    "layer blur r0",
    "layer blur r8",
    "frost r8",
    "frost stack",
    "displace ripples",
    "displace noise",
    "water pool",
];

/// The app material of the "material backdrop" case (EG-02): the card's panel reads its
/// layer's backdrop through four taps 3 px apart, a light frost, under its own fill.
const FROST: u32 = vo::scene::material::CUSTOM_FIRST;
const FROST_WGSL: &str = "
fn mat_frost (m: Material, i: MatIn) -> vec4f {
  var b = vec4f (0.0);
  for (var k = 0; k < 4; k++) {
    let a = f32 (k) * 1.5707963 + 0.785;
    b += tex_backdrop (i.px + 3.0 * vec2f (cos (a), sin (a)));
  }
  let under = 0.25 * b * 0.6;
  return (i.color + under * (1.0 - i.color.a)) * i.cov;
}
";

fn add_frost(r: &mut Renderer) {
    r.add_material(FROST, "mat_frost", FROST_WGSL);
}

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Case(usize),
}

/// The card with case `i`'s effect, at time `t`. `k` scales lengths with the surface, and the
/// card drifts sideways so trails and motion blur have something to show.
fn view(size: Size, i: usize, t: f64) -> Element<Msg> {
    let k = size.h / 300.0;
    let case = CASES[i];
    let drift = (t * 1.7).sin() as f32 * 24.0 * k;
    let text = |key: &str, s: &str, px: f32, c: u32| {
        let e = Element::label(key.to_string(), s)
            .text_style(TextStyle::new(0, px * k, px * 1.25 * k))
            .color(Color::hex(c, 1.0));
        match case {
            "text halo" => e.text_halo(Color::hex(0x4cc9f0, 0.8), 10.0 * k),
            "text blur" => e.text_blur(3.0 * k),
            _ => e,
        }
    };
    let mut card = Element::panel("card")
        .pad(18.0 * k)
        .gap(8.0 * k)
        .radius(14.0 * k)
        .fill(Color::hex(0x10141c, 0.92))
        .border(Color::hex(0x4cc9f0, 0.8), 1.5 * k)
        .child(text("title", case, 30.0, 0xffffff))
        .child(text("l1", "open the configuration file and set the theme", 17.0, 0xc8d3e0))
        .child(text("l2", "so the window shows the live words", 17.0, 0xc8d3e0))
        .child(
            Element::wavy_line("wave", StrokeStyle::solid(2.0 * k, Color::hex(0x4cc9f0, 1.0)), 6.0 * k, 40.0 * k)
                .size(380.0 * k, 20.0 * k),
        )
        .offset(drift, 0.0);
    match case {
        "glow" => card = card.glow(Color::hex(0x4cc9f0, 0.9), 14.0 * k, 1.2),
        "shadow" => card = card.shadow(Color::rgba(0.0, 0.0, 0.0, 0.6), 0.0, 8.0 * k, 18.0 * k),
        "motion blur" => card = card.motion_blur(1.0 / 30.0),
        "material backdrop" => {
            card = card.fill(Color::hex(0x10141c, 0.55)).material(Material::custom(FROST).reads_backdrop())
        }
        // Glass on the rounded rect, and on a chamfered octagon (EG-06), with the orbit on.
        "glass" => card = card.fill(Color::TRANSPARENT).glass(Glass::dark().with_orbit(Color::hex(0x4cc9f0, 1.0))),
        "glass octagon" => {
            let g = Glass::dark().with_orbit(Color::hex(0x4cc9f0, 1.0)).with_shape(GlassShape::chamfer(12.0 * k));
            card = card.fill(Color::TRANSPARENT).glass(g)
        }
        _ => {}
    }
    if case == "frost stack" {
        return frost_stack(size, k);
    }
    let layer = Element::layer("fx").child(card);
    let layer = match case {
        "offscreen" => layer.offscreen(),
        "bloom" => layer.bloom(1.0, 18.0 * k),
        "bloom wide" => layer.bloom(1.0, 60.0 * k),
        "trail" => layer.trail(0.25),
        "crt style" => layer.chromatic(2.0 * k).scanlines(0.35, 3.0 * k).crt(0.12).vignette(0.4),
        "glitch" => layer.glitch(0.6),
        // A blur of the card (EG-04); radius 0 must add no pass at all.
        "layer blur r0" => layer.layer_blur(0.0),
        "layer blur r8" => layer.layer_blur(8.0 * k),
        // The stripes under the card frosted inside its box (EG-04).
        "frost r8" => layer.frost(8.0 * k).radius(14.0 * k),
        // A layer the card's size that captures what is under it and draws it back tinted
        // (EG-01), the base cost of glass, blur and ripples over a backdrop.
        "backdrop" => layer.backdrop(BackdropRes::Full, Color::hex(0xff6b6b, 1.0)),
        "backdrop half" => layer.backdrop(BackdropRes::Half, Color::hex(0xff6b6b, 1.0)),
        // A backdrop that only captures, read by the panel's material (EG-02).
        "material backdrop" => layer.backdrop(BackdropRes::Full, Color::TRANSPARENT),
        // The stripes under the card drawn back displaced (EG-03): four rings alive at any
        // time, one every 0.3 s, and a turning noise field.
        "displace ripples" => {
            let last = (t / 0.3).floor() * 0.3;
            let onsets = (0..4).map(|n| Onset {
                t: last - n as f64 * 0.3,
                at: Point::new((80.0 + 90.0 * n as f32) * k, 60.0 * k),
            });
            layer.displace(Displace::Ripples(Ripples::new().onsets(onsets).amp(5.0 * k).speed(160.0 * k)))
        }
        "displace noise" => layer.displace(Displace::Noise { scale: 50.0 * k, amount: 4.0 * k, speed: 0.25, seed: 3 }),
        _ => layer,
    };
    let root = Element::column("root").size(size.w, size.h).pad_xy(60.0 * k, 40.0 * k);
    if case == "water pool" {
        return root.child(layer).child(water_pool(size, t).view("water"));
    }
    if !case.contains("backdrop") && !case.starts_with("frost") && !case.starts_with("displace") {
        return root.child(layer);
    }
    // Stripes under the card, so the tinted backdrop shows in the preview and the sheets.
    let stripes = Element::row("stripes")
        .children((0..(size.w / (12.0 * k)) as u32).map(|i| {
            let c = if i % 2 == 0 { Color::hex(0xe8ecf2, 1.0) } else { Color::hex(0x3a86ff, 1.0) };
            Element::panel(i as u64).size(12.0 * k, size.h).fill(c)
        }))
        .layout(|s| s.position = vo::scene::taffy::style::Position::Absolute);
    root.child(stripes).child(layer)
}

/// Three depth-stack sheets that frost the two older lines behind them (EG-04): the whole
/// frame has to stay under 1 ms at 600x300.
fn frost_stack(size: Size, k: f32) -> Element<Msg> {
    let stack = DepthStack {
        sheet: Some(Sheet {
            fill: Color::hex(0x16222c, 0.35),
            edge: Color::hex(0x9fd8ff, 0.35),
            edge_width: 1.0,
            radius: 6.0 * k,
            pad: (14.0 * k, 6.0 * k),
        }),
        step_z: 70.0 * k,
        step_rise: 18.0 * k,
        frost: 3.0 * k,
        ..DepthStack::default()
    };
    let lines: Vec<(vo::scene::Key, &str)> =
        ["the oldest line goes back", "an older line behind glass", "the newest line reads"]
            .iter()
            .map(|l| (vo::scene::Key::from(l.to_string()), *l))
            .collect();
    let content = |text: &&str, d: usize, _: &Place| {
        Element::label(format!("t{d}"), text.to_string())
            .text_style(TextStyle::new(0, 20.0 * k, 26.0 * k))
            .color(Color::hex(0xf2f3f5, 1.0))
    };
    let sheet = Size::new(size.w * 0.7, 40.0 * k);
    Element::panel("root")
        .size(size.w, size.h)
        .fill(Color::hex(0x0b0f14, 1.0))
        .child(stack.view("stack", size, sheet, &lines, content, false))
}

/// A pool of water (EG-10) below the card that mirrors it, with a drop every 0.3 s and four
/// rings alive at any time.
fn water_pool(size: Size, t: f64) -> WaterPool {
    let last = (t / 0.3).floor() * 0.3;
    let drops = (0..4).rev().map(|n| PoolDrop::word(last - n as f64 * 0.3, 0.15 + 0.23 * n as f32, 0.8));
    WaterPool::new(size, Rect::new(0.0, size.h * 0.7, size.w, size.h * 0.3))
        .light(false)
        .reduced_motion(false)
        .drops(drops.filter(|d| d.t >= 0.0))
}

/// GPU ms of the water's own work per 60 fps frame at `size`: two ticks and one output pass.
fn water_sim_ms(g: &Gpu, size: Size) -> f64 {
    use vo_sim::Sim;
    let pool = water_pool(size, 0.0);
    let cfg = WaterConfig { reduced_motion: false, ..WaterConfig::for_size(pool.pool.w, pool.pool.h) };
    let mut water = Water::new(&g.device, &g.queue, cfg);
    water.apply(&WaterDrop { x: 0.3, y: 0.3, radius: 0.03, strength: -0.8 });
    water.gpu_cost(2).map_or(0.0, |c| c.tick_ms * 2.0 + c.output_ms)
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let path = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
    fonts
}

/// Cycles through the cases, for the preview and contact sheets.
struct Tour {
    size: Size,
    case: usize,
}

impl App for Tour {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        for i in 1..CASES.len() {
            cx.after(i as f64 * CASE_SECS, Msg::Case(i));
        }
    }

    fn update(&mut self, msg: Msg, _: &mut Cx<Msg>) {
        let Msg::Case(i) = msg;
        self.case = i;
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        view(self.size, self.case, 0.0)
    }
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    name: String,
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
    .expect("no Vulkan adapter");
    let want = timestamp_features();
    let features = if adapter.features().contains(want) { want } else { wgpu::Features::empty() };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("effect-cost"),
        required_features: features,
        ..Default::default()
    }))
    .expect("no device");
    Gpu { device, queue, name: adapter.get_info().name }
}

/// One case at one size.
struct Row {
    case: &'static str,
    gpu_p50: f64,
    gpu_p95: f64,
    cpu_p50: f64,
    slowest: String,
    pool_mb: f64,
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * q).round() as usize]
}

fn measure(g: &Gpu, fonts: &FontRegistry, size: Size, frames: usize) -> Vec<Row> {
    let (w, h) = (size.w as u32, size.h as u32);
    let mut target = Target::new(&g.device, w, h, wgpu::TextureFormat::Rgba8UnormSrgb);
    let mut out = Vec::new();
    for (i, name) in CASES.iter().enumerate() {
        // A renderer per case, so pooled textures belong to the case.
        let mut r = Renderer::with_fonts(&g.device, &g.queue, fonts.clone());
        assert!(r.set_profiling(true), "this device has no timestamp queries");
        add_frost(&mut r);
        // The water's ticks run in their own submits, outside the frame's timestamps.
        let water_ms = if *name == "water pool" {
            vo_sim::water_render::install_pool(&mut r, &water_pool(size, 0.0), 1.0);
            water_sim_ms(g, size)
        } else {
            0.0
        };
        let mut scene: Scene<Msg> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
        scene.set_viewport(0.0, size, 1.0);
        let (mut gpu, mut cpu, mut sections) = (Vec::new(), Vec::new(), Vec::<(String, f64)>::new());
        // Ten frames to warm up pipelines and the atlas.
        for f in 0..frames + 10 {
            let t = f as f64 / 60.0;
            scene.apply(view(size, i, t), t);
            let list = scene.sample(t);
            let started = Instant::now();
            r.render_at(&list, 1.0, &mut target);
            let record = started.elapsed().as_secs_f64() * 1e3;
            let Some(tm) = r.last_timings() else { continue };
            if f >= 10 {
                gpu.push(tm.total_ms + water_ms);
                cpu.push(record);
                for p in tm.passes {
                    match sections.iter_mut().find(|s| s.0 == p.label) {
                        Some(s) => s.1 += p.ms,
                        None => sections.push((p.label, p.ms)),
                    }
                }
            }
        }
        sections.sort_by(|a, b| b.1.total_cmp(&a.1));
        let slowest =
            sections.first().map(|(l, ms)| format!("{l} {:.3}", ms / gpu.len().max(1) as f64)).unwrap_or_default();
        out.push(Row {
            case: name,
            gpu_p50: pct(&mut gpu, 0.5),
            gpu_p95: pct(&mut gpu, 0.95),
            cpu_p50: pct(&mut cpu, 0.5),
            slowest,
            pool_mb: r.pool_bytes() as f64 / 1e6,
        });
    }
    out
}

fn main() {
    if vo::preview::wanted() {
        let fonts = fonts();
        let size = Size::new(600.0, 300.0);
        let scenario = Scenario { size, scale: 1.0, duration: CASES.len() as f64 * CASE_SECS, inputs: Vec::new() };
        let options = move || Options::<Msg> {
            fonts: fonts.clone(),
            renderer_setup: Some(std::sync::Arc::new(move |r: &mut Renderer| {
                add_frost(r);
                vo_sim::water_render::install_pool(r, &water_pool(size, 0.0), 1.0);
            })),
            ..Options::default()
        };
        let mut p = vo::preview::Preview::new("effect_cost", move || Tour { size, case: 0 }, options, scenario);
        for (i, name) in CASES.iter().enumerate() {
            p = p.marker(name.replace(' ', "-"), i as f64 * CASE_SECS + 0.5);
        }
        std::process::exit(vo::preview::main(p));
    }
    let (mut frames, mut out, mut max_ms) = (120usize, None::<String>, None::<f64>);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--frames" => frames = args.next().and_then(|v| v.parse().ok()).expect("--frames takes a number"),
            "--out" => out = args.next(),
            "--max-ms" => max_ms = args.next().and_then(|v| v.parse().ok()),
            "--real-display" => {}
            _ => panic!("unknown option {a}"),
        }
    }
    let g = gpu();
    let fonts = fonts();
    let mut md = format!(
        "GPU time per frame by effect, {frames} frames each, on {}. Median and 95th percentile from \
         timestamp queries; +ms is the median over the plain card's. CPU is the time to record and \
         submit the frame.\n\n",
        g.name
    );
    let mut over = Vec::new();
    let mut slowest = Vec::new();
    for size in [Size::new(600.0, 300.0), Size::new(1920.0, 1080.0)] {
        let rows = measure(&g, &fonts, size, frames);
        let base = rows[0].gpu_p50;
        md.push_str(&format!("### {}x{}\n\n", size.w, size.h));
        md.push_str("| effect | GPU ms p50 | p95 | +ms | CPU ms | slowest section (ms) | pool MB |\n");
        md.push_str("|---|---:|---:|---:|---:|---|---:|\n");
        for r in &rows {
            md.push_str(&format!(
                "| {} | {:.3} | {:.3} | {:+.3} | {:.2} | {} | {:.1} |\n",
                r.case,
                r.gpu_p50,
                r.gpu_p95,
                r.gpu_p50 - base,
                r.cpu_p50,
                r.slowest,
                r.pool_mb
            ));
            if size.w == 600.0 && max_ms.is_some_and(|m| r.gpu_p50 > m) {
                over.push(format!("{} {:.3} ms", r.case, r.gpu_p50));
            }
        }
        md.push('\n');
        if let Some(r) = rows.iter().max_by(|a, b| a.gpu_p50.total_cmp(&b.gpu_p50)) {
            slowest.push(format!("{}x{} {} {:.3} ms", size.w, size.h, r.case, r.gpu_p50));
        }
    }
    print!("{md}");
    if let Some(path) = out {
        std::fs::write(&path, &md).unwrap_or_else(|e| panic!("{path}: {e}"));
    }
    eprintln!("effect_cost: slowest: {}", slowest.join(", "));
    if !over.is_empty() {
        eprintln!("effect_cost: over {} ms at 600x300: {}", max_ms.unwrap_or(0.0), over.join(", "));
        std::process::exit(1);
    }
}
