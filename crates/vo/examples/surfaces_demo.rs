//! One app, three surfaces (WP12: FX-133, FX-193, FX-194): a listening card, a fullscreen
//! click-through scrim with a screen-edge glow under it, and a ticker bar at the bottom.
//!
//! The edge glow follows a made-up speech level for 3 s, then rests. The ticker changes its
//! text once a second until 8 s, so it draws while the scrim, settled, draws nothing. At 9 s
//! the app drops the scrim and the ticker: they fade out and close. At 10 s the card leaves
//! and the app exits.
//!
//!   cargo run --release -p vo --example surfaces_demo                 live (nested KWin only)
//!   cargo run --release -p vo --example surfaces_demo -- --no-scrim   live, without the scrim
//!   cargo run --release -p vo --example surfaces_demo -- --offline DIR [--fps 10]
//!   cargo run --release -p vo --example surfaces_demo -- --bench
//!
//! `--offline` writes the card's frames to DIR and each extra surface's to DIR/<key>, with the
//! scrim at 960x540. `--bench` prints the scrim's GPU time per frame at 600x300 and 1920x1080
//! (timestamp queries), its pooled texture memory, and the swapchain memory a fullscreen
//! surface needs. `VO_DIAG_FRAMES=1` prints every frame of every surface.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use vo::scene::element::{Element, TextStyle, Transition};
use vo::scene::geom::{Color, Size};
use vo::scene::Scene;
use vo::{
    App, Cx, Edges, FontRegistry, InputMode, LogDiagnostics, Options, OverlaySpec, Placement, Scenario, Stack,
    SurfaceView, Tokens,
};
use vo_render::Renderer;

/// Counts live heap bytes, for the own-heap figure in the diagnostics.
struct Counting;

static HEAP: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator and only adds bookkeeping.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            HEAP.fetch_add(layout.size(), Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        HEAP.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new) };
        if !p.is_null() {
            HEAP.fetch_sub(layout.size(), Ordering::Relaxed);
            HEAP.fetch_add(new, Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn heap() -> usize {
    HEAP.load(Ordering::Relaxed)
}

const CARD: Size = Size::new(420.0, 120.0);
/// Level samples per second, for the first `LEVEL_SECS`.
const RATE: f64 = 10.0;
const LEVEL_SECS: f64 = 3.0;
const TICKS: u32 = 8;

fn accent() -> Color {
    Color::hex(0x4cc9f0, 1.0)
}

/// A made-up speech envelope, 0 to 1.
fn level_at(t: f64) -> f32 {
    let s = (t * 5.1).sin() * 0.5 + (t * 2.3).sin() * 0.3 + (t * 9.7).sin() * 0.2;
    (0.35 + 0.65 * s.abs()) as f32
}

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Level(u32),
    Tick,
    Drop,
    Done,
}

struct Demo {
    scrim: bool,
    /// The scrim's size before the compositor says: the output, as far as the app knows.
    scrim_guess: Size,
    level: f32,
    ticks: u32,
    extras: bool,
    shown: bool,
}

impl Demo {
    fn new(scrim: bool, scrim_guess: Size) -> Demo {
        Demo { scrim, scrim_guess, level: 0.0, ticks: 0, extras: true, shown: true }
    }

    fn scrim_view(&self) -> Element<Msg> {
        let edge = Element::panel("edge").grow(1.0).stroke(accent().fade(0.9), 2.0).glow(
            accent().fade(0.8),
            28.0 + 50.0 * self.level,
            0.2 + 0.8 * self.level,
        );
        Element::panel("dim")
            .fill(Color::hex(0x05070c, 0.42))
            .presence(Transition::fade(), Transition::fade())
            .child(edge)
    }

    fn ticker_view(&self) -> Element<Msg> {
        let text = format!("{} words so far, {} s", self.ticks * 3, self.ticks);
        Element::panel("bar")
            .radius(22.0)
            .pad_xy(20.0, 12.0)
            .fill(Color::hex(0x15171c, 0.92))
            .stroke(Color::hex(0xffffff, 0.08), 1.0)
            .presence(Transition::fade().offset(0.0, 16.0), Transition::fade().offset(0.0, 16.0))
            .child(
                Element::label("text", text).text_style(TextStyle::new(0, 15.0, 20.0)).color(Color::hex(0xd8dae0, 1.0)),
            )
    }
}

impl App for Demo {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        cx.after(0.0, Msg::Level(0));
        cx.after(1.0, Msg::Tick);
        cx.after(9.0, Msg::Drop);
        cx.after(10.0, Msg::Done);
    }

    fn update(&mut self, msg: Msg, cx: &mut Cx<Msg>) {
        match msg {
            Msg::Level(n) => {
                let t = n as f64 / RATE;
                if t < LEVEL_SECS {
                    self.level = level_at(t);
                    cx.after(1.0 / RATE, Msg::Level(n + 1));
                } else {
                    self.level = 0.25;
                }
            }
            Msg::Tick => {
                self.ticks += 1;
                if self.ticks < TICKS {
                    cx.after(1.0, Msg::Tick);
                }
            }
            Msg::Drop => self.extras = false,
            Msg::Done => {
                self.shown = false;
                cx.exit();
            }
        }
    }

    fn view(&self, _: &Tokens) -> Element<Msg> {
        let root = Element::column("root").size(CARD.w, CARD.h);
        if !self.shown {
            return root;
        }
        let bar = Element::panel("level").height(6.0).width(40.0 + 300.0 * self.level).radius(3.0).fill(accent()).glow(
            accent(),
            10.0,
            0.8,
        );
        let card = Element::panel("card")
            .grow(1.0)
            .pad(22.0)
            .gap(14.0)
            .radius(18.0)
            .fill(Color::hex(0x101319, 0.94))
            .stroke(accent().fade(0.35), 1.0)
            .presence(Transition::fade().scale(0.96), Transition::fade().scale(0.98))
            .child(Element::label("title", "Listening").text_style(TextStyle::new(0, 20.0, 26.0)).color(Color::WHITE))
            .child(bar);
        root.child(card)
    }

    fn surfaces(&self, _: &Tokens) -> Vec<SurfaceView<Msg>> {
        if !self.extras {
            return Vec::new();
        }
        let mut out = Vec::new();
        if self.scrim {
            let spec = OverlaySpec {
                size: self.scrim_guess,
                placement: Placement::Fullscreen,
                input: InputMode::None,
                layer: Stack::Top,
                app_id: "vo-scrim".into(),
                ..OverlaySpec::default()
            };
            out.push(SurfaceView::new("scrim", spec, self.scrim_view()));
        }
        let ticker = OverlaySpec {
            size: Size::new(420.0, 44.0),
            placement: Placement::Anchored { edges: Edges::BOTTOM, margin: [0, 0, 32, 0] },
            input: InputMode::None,
            app_id: "vo-ticker".into(),
            ..OverlaySpec::default()
        };
        out.push(SurfaceView::new("ticker", ticker, self.ticker_view()));
        out
    }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_system("Noto Sans").expect("Noto Sans through fontconfig");
    fonts
}

fn device(timestamps: bool) -> (wgpu::Device, wgpu::Queue) {
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
        label: Some("surfaces-demo"),
        required_features: features,
        ..Default::default()
    }))
    .expect("a GPU device")
}

/// The scrim's display list at `size`, with the edge glow at `level`.
fn scrim_list(size: Size, level: f32) -> vo::scene::DisplayList {
    let mut demo = Demo::new(true, size);
    demo.level = level;
    let mut scene: Scene<Msg> = Scene::new();
    scene.set_viewport(0.0, size, 1.0);
    let view = demo.surfaces(&Tokens::default()).into_iter().find(|s| s.key == "scrim").expect("scrim").view;
    scene.apply(Element::column("vo-surface").size(size.w, size.h).child(view.grow(1.0)), 0.0);
    scene.sample(5.0)
}

/// GPU time per frame of the scrim and edge glow, and what a fullscreen surface holds.
fn bench() {
    let (device, queue) = device(true);
    let period = queue.get_timestamp_period() as f64;
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let buffer = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
    let resolve = buffer(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buffer(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    for (w, h) in [(600u32, 300u32), (1920, 1080)] {
        for (label, level) in [("glow at peak", 1.0), ("glow at rest", 0.25)] {
            let mut r = Renderer::new(&device, &queue);
            let list = scrim_list(Size::new(w as f32, h as f32), level);
            let target = vo_render::Target::new(&device, w, h, vo_render::offscreen::FORMAT);
            let mut times = Vec::new();
            for i in 0..220 {
                let mut enc = device.create_command_encoder(&Default::default());
                enc.write_timestamp(&qs, 0);
                r.encode(&list, 1.0, &target, &mut enc);
                enc.write_timestamp(&qs, 1);
                enc.resolve_query_set(&qs, 0..2, &resolve, 0);
                enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, 16);
                queue.submit([enc.finish()]);
                read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
                device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
                let ts: [u64; 2] =
                    bytemuck::pod_read_unaligned(&read.slice(..).get_mapped_range().expect("mapped")[..16]);
                read.unmap();
                // The first frames compile pipelines and fill the pool.
                if i >= 20 {
                    times.push((ts[1] - ts[0]) as f64 * period / 1e6);
                }
            }
            times.sort_by(f64::total_cmp);
            let (median, p95) = (times[times.len() / 2], times[times.len() * 95 / 100]);
            println!(
                "surfaces_demo bench: scrim {w}x{h} {label}: GPU {median:.3} ms median, {p95:.3} ms p95; \
                 pooled textures {:.2} MB; {} shapes",
                r.pool_bytes() as f64 / 1048576.0,
                list.shapes.len()
            );
        }
        // A swapchain of 8-bit RGBA images; the host asks for a frame latency of 2, so the
        // driver keeps about 3.
        let image = w as f64 * h as f64 * 4.0 / 1048576.0;
        println!("surfaces_demo bench: {w}x{h} swapchain: {image:.2} MB per image, about {:.1} MB for 3", image * 3.0);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut offline, mut fps, mut scrim, mut run_bench) = (None::<PathBuf>, 10.0, true, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--offline" => offline = Some(args.next().expect("--offline DIR").into()),
            "--fps" => fps = args.next().and_then(|v| v.parse().ok()).expect("--fps N"),
            "--no-scrim" => scrim = false,
            "--bench" => run_bench = true,
            other => panic!("unknown argument {other}"),
        }
    }
    if run_bench {
        bench();
        return;
    }
    let fonts = fonts();
    let options = || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };

    if let Some(dir) = offline {
        let scenario = Scenario { size: CARD, scale: 1.0, duration: 11.0, inputs: Vec::new() };
        let app = Demo::new(scrim, Size::new(960.0, 540.0));
        let n = vo::render_offline_with(app, &scenario, fps, &dir, options()).expect("offline render");
        eprintln!("surfaces_demo: wrote {n} card frames to {}, extra surfaces in its subfolders", dir.display());
        return;
    }

    let spec = OverlaySpec {
        size: CARD,
        placement: Placement::FromTop { fraction: 0.3 },
        input: InputMode::None,
        app_id: "vo-card".into(),
        ..OverlaySpec::default()
    };
    let mut options = options();
    options.diagnostics = Some(Box::new(LogDiagnostics::new(Some(heap))));
    if let Err(e) = vo::run_overlay_with(Demo::new(scrim, Size::new(1920.0, 1080.0)), spec, options) {
        eprintln!("surfaces_demo: {e}");
        std::process::exit(1);
    }
}
