//! Water pool demo (EG-10): live text over a pool of dark water. Each word that arrives drops
//! on the water under it; the pool shows the text above it mirrored and wobbling, with a bright
//! edge and glints. The newest line stays sharp and still; only its reflection moves.
//!
//!   cargo run --release -p vo --example water_pool_demo -- [--out DIR] [--check] [--cost]
//!
//! Writes `DIR/water-pool.png`, a strip of five moments (default DIR
//! `/var/tmp/claude/swarm/EG-10`). `VO_LIGHT=1` draws light mode, `VO_REDUCED_MOTION=1` a flat
//! pool. `--check` runs the reading-line envelope check and renders the scenario twice at
//! 60 fps, comparing every frame's hash. `--cost` prints the GPU time per frame (p50, p95)
//! with the water's own ticks and output pass added.
//!
//! `tests/water_envelope.rs` runs the same app through `vo::envelope::check_scenario`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Rect, Size};
use vo::scene::water::{PoolDrop, WaterPool};
use vo::{App, Cx, Export, FontRegistry, Input, Options, Scenario, Tokens};
use vo_core::words::Word;
use vo_render::offscreen::{write_png, Frame};

pub const W: f32 = 600.0;
pub const H: f32 = 300.0;
pub const DURATION: f64 = 4.0;
/// The pool, below three lines of text.
pub const POOL: Rect = Rect::new(0.0, 168.0, W, H - 168.0);

const TEXT: [&str; 14] =
    ["each", "word", "drops", "on", "the", "water", "below", "and", "the", "pool", "mirrors", "the", "lines", "above"];

pub struct Demo {
    words: Vec<Word>,
    /// When each word arrived: the drops' times.
    onsets: Vec<f64>,
    light: bool,
    reduced: bool,
}

impl Demo {
    pub fn new() -> Demo {
        let env = |k: &str| std::env::var(k).is_ok_and(|v| v == "1");
        Demo { words: Vec::new(), onsets: Vec::new(), light: env("VO_LIGHT"), reduced: env("VO_REDUCED_MOTION") }
    }

    /// The pool with one drop per word, spread across it, louder every third word.
    pub fn pool(&self) -> WaterPool {
        let drops = self.onsets.iter().enumerate().map(|(i, &t)| {
            let x = 0.08 + (i % 7) as f32 * 0.14;
            let level = if i % 3 == 2 { 1.0 } else { 0.5 };
            PoolDrop::word(t, x, level)
        });
        WaterPool::new(Size::new(W, H), POOL).light(self.light).reduced_motion(self.reduced).radius(0.0).drops(drops)
    }
}

impl Default for Demo {
    fn default() -> Demo {
        Demo::new()
    }
}

impl App for Demo {
    type Msg = Vec<Word>;

    fn update(&mut self, words: Vec<Word>, cx: &mut Cx<Vec<Word>>) {
        if words.len() > self.words.len() {
            self.onsets.push(cx.t());
        }
        self.words = words;
    }

    fn view(&self, _: &Tokens) -> Element<Vec<Word>> {
        let (bg, fg) = if self.light { (0xeef2f6, 0x1a1f26) } else { (0x0e1117, 0xf2f3f5) };
        let live = Element::live_text("live", &self.words)
            .text_style(TextStyle::new(0, 30.0, 40.0))
            .color(Color::hex(fg, 1.0))
            .width(W - 60.0);
        Element::panel("root")
            .size(W, H)
            .fill(Color::hex(bg, 1.0))
            .child(at(live, 30.0, 24.0))
            .child(self.pool().view("water"))
    }
}

fn at<M>(e: Element<M>, x: f32, y: f32) -> Element<M> {
    e.layout(|s| {
        s.position = vo::scene::taffy::style::Position::Absolute;
        s.inset.left = vo::scene::taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = vo::scene::taffy::style::LengthPercentageAuto::length(y);
    })
}

/// A word every 0.2 s from 0.2 s on.
pub fn scenario() -> Scenario<Vec<Word>> {
    let inputs = (1..=TEXT.len())
        .map(|n| {
            let words = TEXT[..n].iter().enumerate().map(|(i, w)| Word { id: i as u32, text: (*w).into() }).collect();
            (0.2 + (n - 1) as f64 * 0.2, Input::Msg(words))
        })
        .collect();
    Scenario { size: Size::new(W, H), scale: 1.0, duration: DURATION, inputs }
}

/// Options with the water installed on every renderer.
pub fn options(fonts: FontRegistry) -> Options<Vec<Word>> {
    let pool = Demo::new().pool();
    Options {
        fonts,
        reduced_motion: pool.reduced_motion,
        light: pool.light,
        renderer_setup: Some(Arc::new(move |r| vo_sim::water_render::install_pool(r, &pool, 1.0))),
        ..Options::default()
    }
}

/// The moments the strip shows.
const STRIP_AT: [f64; 5] = [0.6, 1.2, 1.8, 2.6, 3.6];
const FPS: f64 = 10.0;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut check, mut cost) = (PathBuf::from("/var/tmp/claude/swarm/EG-10"), false, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--check" => check = true,
            "--cost" => cost = true,
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = FontRegistry::new();
    fonts.add_file(&vo::scene::font::test_font_path()).expect("test font");
    let spec = Export { fps: FPS, duration: DURATION, png_dir: None, video: None };
    let picked_at: Vec<u64> = STRIP_AT.iter().map(|t| (t * FPS).round() as u64).collect();
    let mut picked: Vec<Frame> = Vec::new();
    vo::export_with(Demo::new(), &scenario(), &spec, options(fonts.clone()), |n, f| {
        if picked_at.contains(&n) {
            picked.push(f.clone());
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("water_pool_demo: {e}");
        std::process::exit(1);
    });
    let (w, h) = (picked[0].width, picked[0].height);
    let rgba: Vec<u8> = picked.iter().flat_map(|f| f.rgba.iter().copied()).collect();
    std::fs::create_dir_all(&out).expect("create the output directory");
    let path = out.join("water-pool.png");
    write_png(&path, w, h * picked.len() as u32, &rgba).expect("write the strip");
    println!("water_pool_demo: strip {}", path.display());
    let mut failed = false;
    if check {
        let env = vo::envelope::Envelope::default();
        let r = vo::envelope::check_scenario(Demo::new(), &scenario(), 60.0, options(fonts.clone()), &env);
        println!("water_pool_demo: envelope {r}");
        let spec60 = Export { fps: 60.0, ..spec.clone() };
        let a = vo::export(Demo::new(), &scenario(), &spec60, options(fonts.clone())).expect("first render");
        let b = vo::export(Demo::new(), &scenario(), &spec60, options(fonts.clone())).expect("second render");
        let differ = a.frame_hashes.iter().zip(&b.frame_hashes).filter(|(x, y)| x != y).count();
        println!("water_pool_demo: two 60 fps renders, {differ} of {} frames differ", a.frames);
        failed |= differ > 0 || !r.violations.is_empty();
    }
    if cost {
        print_cost(&fonts, &spec);
    }
    if failed {
        std::process::exit(1);
    }
}

/// GPU time per frame of the demo at 60 fps, with the water's own work added: two ticks per
/// frame and one output pass, measured on a surface stepped to 1.5 s.
fn print_cost(fonts: &FontRegistry, spec: &Export) {
    let times = Arc::new(Mutex::new(Vec::new()));
    let sink = times.clone();
    let opts = Options {
        frame_timings: Some(Arc::new(move |ft: &vo::FrameTiming| sink.lock().unwrap().push(ft.timings.total_ms))),
        ..options(fonts.clone())
    };
    let spec60 = Export { fps: 60.0, ..spec.clone() };
    vo::export(Demo::new(), &scenario(), &spec60, opts).expect("cost render");
    let mut ms = times.lock().unwrap().clone();
    if ms.is_empty() {
        println!("water_pool_demo: this device has no timestamp queries");
        return;
    }
    let water = water_cost().unwrap_or(0.0);
    for v in &mut ms {
        *v += water;
    }
    ms.sort_by(f64::total_cmp);
    let pct = |q: f64| ms[((ms.len() - 1) as f64 * q).round() as usize];
    println!(
        "water_pool_demo: {} frames at 600x300, GPU p50 {:.3} ms, p95 {:.3} ms (water ticks and output {water:.3} ms each)",
        ms.len(),
        pct(0.5),
        pct(0.95)
    );
}

/// GPU ms of two water ticks and one output pass on the demo's grid, or `None` without
/// timestamp queries.
pub fn water_cost() -> Option<f64> {
    use vo_sim::water::{Water, WaterConfig, WaterDrop};
    use vo_sim::Sim;
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .ok()?;
    let required_features = adapter.features() & wgpu::Features::TIMESTAMP_QUERY;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { required_features, ..Default::default() }))
            .ok()?;
    // Every tick is the same dispatch whatever the heights, so a fresh surface with one drop
    // costs what the demo's does.
    let cfg = WaterConfig { reduced_motion: false, ..WaterConfig::for_size(POOL.w, POOL.h) };
    let mut water = Water::new(&device, &queue, cfg);
    water.apply(&WaterDrop { x: 0.3, y: 0.3, radius: 0.03, strength: -0.8 });
    let c = water.gpu_cost(2)?;
    Some(c.tick_ms * 2.0 + c.output_ms)
}
