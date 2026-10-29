//! Ink demo (EG-12): six dictated words seed a fluid behind the text. Each word drops dye where
//! it lands and pushes it up from its onset, harder when the voice is louder. The newest line
//! sits on a clear band; the dye blooms around it and shows in full once the line moves up,
//! and the older line smears along the flow.
//!
//!   cargo run --release -p vo --example ink_demo -- [--style S] [--out DIR] [--check] [--cost]
//!
//! `--style` is `ink` (default), `smoke` or `watercolour`. Writes `DIR/ink-<style>.png`, a strip
//! of five moments (default DIR `/var/tmp/claude/swarm/EG-12`). `VO_LIGHT=1` draws light mode,
//! `VO_REDUCED_MOTION=1` still dye. `--check` runs the reading-line envelope check and renders
//! the scenario twice at 60 fps, comparing every frame's hash. `--cost` prints the GPU time per
//! frame (p50, p95) with the fluid's own ticks and output pass added.
//!
//! `tests/ink_envelope.rs` runs the same app through `vo::envelope::check_scenario`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use vo::scene::element::{Element, TextStyle};
use vo::scene::fluid::{word_boxes, FluidLayer, InkStyle, InkWord};
use vo::scene::geom::{Color, Point, Rect, Size};
use vo::scene::text::TextLayout;
use vo::{App, Cx, Export, FontRegistry, Input, Options, Scenario, ShapedLayout, Tokens};
use vo_core::words::Word;
use vo_render::offscreen::{write_png, Frame};

pub const W: f32 = 600.0;
pub const H: f32 = 300.0;
pub const DURATION: f64 = 4.0;
/// The text's box.
const TEXT_AT: Point = Point::new(60.0, 110.0);
const TEXT_W: f32 = 330.0;
const SIZE: f32 = 34.0;
const LINE_H: f32 = 44.0;

const TEXT: [&str; 6] = ["ink", "blooms", "where", "the", "words", "land"];
/// When each word arrives, and the voice level at its onset.
pub const ONSETS: [f64; 6] = [0.2, 0.45, 0.7, 1.0, 1.25, 1.5];
const LEVELS: [f32; 6] = [0.5, 0.9, 0.4, 1.0, 0.6, 0.8];

pub fn text_style() -> TextStyle {
    TextStyle::new(0, SIZE, LINE_H)
}

pub fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&vo::scene::font::test_font_path()).expect("test font");
    fonts
}

/// Each word's box in the popup, from the layout the live text uses.
pub fn boxes(fonts: &FontRegistry) -> Vec<Rect> {
    let text = TEXT.join(" ");
    let shaped = ShapedLayout::new(fonts.clone()).layout(&text, &text_style(), Some(TEXT_W));
    word_boxes(&text, &shaped, SIZE)
        .into_iter()
        .map(|r| Rect::new(r.x + TEXT_AT.x, r.y + TEXT_AT.y, r.w, r.h))
        .collect()
}

pub struct Demo {
    pub style: InkStyle,
    boxes: Vec<Rect>,
    words: Vec<Word>,
    /// When each word arrived.
    onsets: Vec<f64>,
    light: bool,
    reduced: bool,
}

impl Demo {
    pub fn new(style: InkStyle) -> Demo {
        let env = |k: &str| std::env::var(k).is_ok_and(|v| v == "1");
        Demo {
            style,
            boxes: boxes(&fonts()),
            words: Vec::new(),
            onsets: Vec::new(),
            light: env("VO_LIGHT"),
            reduced: env("VO_REDUCED_MOTION"),
        }
    }

    /// The fluid with one ink word per word so far, and the newest word's line kept clear.
    pub fn fluid(&self) -> FluidLayer {
        let words = self.onsets.iter().enumerate().map(|(i, &t)| InkWord { t, rect: self.boxes[i], level: LEVELS[i] });
        let mut f = FluidLayer::new(Size::new(W, H), Point::ZERO)
            .style(self.style)
            .light(self.light)
            .reduced_motion(self.reduced)
            .smear(0.08)
            .words(words);
        if let Some(r) = self.onsets.len().checked_sub(1).map(|i| self.boxes[i]) {
            f = f.keep_clear(r.y - 4.0, r.y + r.h + 4.0);
        }
        if self.style == InkStyle::Smoke {
            f = f.glow(0.8);
        }
        f
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
        let (bg, fg) = if self.light { (0xf4efe4, 0x1a1f26) } else { (0x0e1117, 0xf2f3f5) };
        let live =
            Element::live_text("live", &self.words).text_style(text_style()).color(Color::hex(fg, 1.0)).width(TEXT_W);
        let fluid = self.fluid();
        Element::panel("root")
            .size(W, H)
            .fill(Color::hex(bg, 1.0))
            .child(fluid.view("ink"))
            .child(at(live, TEXT_AT.x, TEXT_AT.y))
            .child(fluid.smear_view("smear"))
    }
}

fn at<M>(e: Element<M>, x: f32, y: f32) -> Element<M> {
    e.layout(|s| {
        s.position = vo::scene::taffy::style::Position::Absolute;
        s.inset.left = vo::scene::taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = vo::scene::taffy::style::LengthPercentageAuto::length(y);
    })
}

/// The six words at their onsets.
pub fn scenario() -> Scenario<Vec<Word>> {
    let inputs = (1..=TEXT.len())
        .map(|n| {
            let words = TEXT[..n].iter().enumerate().map(|(i, w)| Word { id: i as u32, text: (*w).into() }).collect();
            (ONSETS[n - 1], Input::Msg(words))
        })
        .collect();
    Scenario { size: Size::new(W, H), scale: 1.0, duration: DURATION, inputs }
}

/// Options with the fluid installed on every renderer.
pub fn options(fonts: FontRegistry, style: InkStyle) -> Options<Vec<Word>> {
    let fluid = Demo::new(style).fluid();
    Options {
        fonts,
        reduced_motion: fluid.reduced_motion,
        light: fluid.light,
        renderer_setup: Some(Arc::new(move |r| vo_sim::fluid_render::install_layer(r, &fluid, 1.0))),
        ..Options::default()
    }
}

/// The moments the strip shows.
const STRIP_AT: [f64; 5] = [0.6, 1.1, 1.6, 2.4, 3.6];
const FPS: f64 = 10.0;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut check, mut cost, mut style) =
        (PathBuf::from("/var/tmp/claude/swarm/EG-12"), false, false, InkStyle::Ink);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--check" => check = true,
            "--cost" => cost = true,
            "--style" => {
                style = match args.next().as_deref() {
                    Some("ink") => InkStyle::Ink,
                    Some("smoke") => InkStyle::Smoke,
                    Some("watercolour") => InkStyle::Watercolour,
                    s => panic!("unknown style {s:?}: ink, smoke or watercolour"),
                }
            }
            _ => panic!("unknown option {a}"),
        }
    }
    let name = match style {
        InkStyle::Ink => "ink",
        InkStyle::Smoke => "smoke",
        InkStyle::Watercolour => "watercolour",
    };
    let fonts = fonts();
    let spec = Export { fps: FPS, duration: DURATION, png_dir: None, video: None };
    let picked_at: Vec<u64> = STRIP_AT.iter().map(|t| (t * FPS).round() as u64).collect();
    let mut picked: Vec<Frame> = Vec::new();
    vo::export_with(Demo::new(style), &scenario(), &spec, options(fonts.clone(), style), |n, f| {
        if picked_at.contains(&n) {
            picked.push(f.clone());
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("ink_demo: {e}");
        std::process::exit(1);
    });
    let (w, h) = (picked[0].width, picked[0].height);
    let rgba: Vec<u8> = picked.iter().flat_map(|f| f.rgba.iter().copied()).collect();
    std::fs::create_dir_all(&out).expect("create the output directory");
    let path = out.join(format!("ink-{name}.png"));
    write_png(&path, w, h * picked.len() as u32, &rgba).expect("write the strip");
    println!("ink_demo: strip {}", path.display());
    let mut failed = false;
    if check {
        let env = vo::envelope::Envelope::default();
        let r = vo::envelope::check_scenario(Demo::new(style), &scenario(), 60.0, options(fonts.clone(), style), &env);
        println!("ink_demo: envelope {r}");
        let spec60 = Export { fps: 60.0, ..spec.clone() };
        let a =
            vo::export(Demo::new(style), &scenario(), &spec60, options(fonts.clone(), style)).expect("first render");
        let b =
            vo::export(Demo::new(style), &scenario(), &spec60, options(fonts.clone(), style)).expect("second render");
        let differ = a.frame_hashes.iter().zip(&b.frame_hashes).filter(|(x, y)| x != y).count();
        println!("ink_demo: two 60 fps renders, {differ} of {} frames differ", a.frames);
        failed |= differ > 0 || !r.violations.is_empty();
    }
    if cost {
        print_cost(&fonts, &spec, style);
    }
    if failed {
        std::process::exit(1);
    }
}

/// GPU time per frame of the demo at 60 fps, with the fluid's own work added: two ticks per
/// frame and one output pass.
fn print_cost(fonts: &FontRegistry, spec: &Export, style: InkStyle) {
    let times = Arc::new(Mutex::new(Vec::new()));
    let sink = times.clone();
    let opts = Options {
        frame_timings: Some(Arc::new(move |ft: &vo::FrameTiming| sink.lock().unwrap().push(ft.timings.total_ms))),
        ..options(fonts.clone(), style)
    };
    let spec60 = Export { fps: 60.0, ..spec.clone() };
    vo::export(Demo::new(style), &scenario(), &spec60, opts).expect("cost render");
    let mut ms = times.lock().unwrap().clone();
    if ms.is_empty() {
        println!("ink_demo: this device has no timestamp queries");
        return;
    }
    let sim = fluid_cost(&Demo::new(style).fluid()).unwrap_or(0.0);
    for v in &mut ms {
        *v += sim;
    }
    ms.sort_by(f64::total_cmp);
    let pct = |q: f64| ms[((ms.len() - 1) as f64 * q).round() as usize];
    println!(
        "ink_demo: {} frames at 600x300, GPU p50 {:.3} ms, p95 {:.3} ms (fluid ticks and output {sim:.3} ms each)",
        ms.len(),
        pct(0.5),
        pct(0.95)
    );
}

/// GPU ms of two fluid ticks and one output pass on the layer's grid, or `None` without
/// timestamp queries.
pub fn fluid_cost(layer: &FluidLayer) -> Option<f64> {
    use vo_sim::fluid::{Fluid, FluidSplat};
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
    // Every tick is the same dispatches whatever the state, so a fresh fluid with one splat
    // costs what the demo's does.
    let cfg = vo_sim::fluid_render::layer_config(&layer.clone().reduced_motion(false), 1.0);
    let mut fluid = Fluid::new(&device, &queue, cfg);
    fluid.apply(&FluidSplat {
        x: 0.3,
        y: 0.5,
        radius: 0.04,
        velocity: [0.2, -0.5],
        color: [0.2, 0.4, 1.0],
        amount: 1.0,
    });
    let c = fluid.gpu_cost(2)?;
    Some(c.tick_ms * 2.0 + c.output_ms)
}
