//! Orbiting Words (EG-20, round 2 design 20): a 3D ring stands round three lines of live text,
//! with beads running along it. The ring is split at the text plane
//! (`Element::gpu_split`): its far side passes behind the words, its near side in front of
//! the older lines, and the newest line (the one being read) is never covered.
//!
//!   cargo run -p vo-3d --example orbit_words_demo -- [--out DIR] [--check]
//!   cargo run -p vo-3d --example orbit_words_demo -- --bench
//!
//! The default writes `DIR/orbit_words_demo.png` (default DIR `/var/tmp/claude/swarm/EG-20`):
//! rows for the dark look, the light look, reduced motion (the orbit stands still) and the
//! ring drawn whole under the text, at five moments. `--check` runs the reading-line envelope
//! check on the scenario and renders every moment twice, comparing the bytes. `--bench`
//! prints the GPU time per frame at 600x300 with no ring, the ring whole and the ring split,
//! and the split's cost over the whole ring's; without timestamp queries it says so. Both
//! exit 0 when they pass.
//!
//! `tests/gpu_split.rs` checks the halves pixel by pixel on the same view.

use std::f32::consts::FRAC_PI_2;
use std::path::PathBuf;
use std::sync::Arc;

use vo_3d::*;
use vo_core::words::Word;
use vo_render::offscreen::{write_png, FORMAT};
use vo_render::{Offscreen, Renderer, Target};
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::gpu::GpuSource;
use vo_scene::text::ShapedLayout;
use vo_scene::{DisplayList, Scene};

pub const W: f32 = 600.0;
pub const H: f32 = 300.0;
pub const DURATION: f64 = 5.0;

const TEXT: [&str; 15] = [
    "the", "ring", "goes", "round", "behind", "the", "words", "and", "comes", "back", "in", "front", "of", "older",
    "lines",
];

/// A word every 0.25 s from 0.2 s on.
pub fn arrival(i: usize) -> f64 {
    0.2 + 0.25 * i as f64
}

pub fn words(n: usize) -> Vec<Word> {
    TEXT[..n.min(TEXT.len())].iter().enumerate().map(|(i, w)| Word { id: i as u32, text: (*w).into() }).collect()
}

/// The words that have arrived by `t`.
pub fn words_at(t: f64) -> Vec<Word> {
    words((0..TEXT.len()).filter(|&i| arrival(i) <= t).count())
}

/// The ring and its beads, in stage space: logical pixels about the item's centre, so the
/// plane z = 0 is the text's plane.
#[derive(Debug)]
pub struct Ring {
    /// Reduced motion: the orbit stands still at its pose at t = 0.
    pub still: bool,
    pub light: bool,
}

const BEADS: usize = 5;

impl Source3d for Ring {
    fn meshes(&self) -> Vec<Mesh> {
        vec![Mesh::torus(1.0, 0.03, 192, 12), Mesh::cuboid(Vec3::new(1.0, 1.0, 1.0))]
    }

    fn frame(&self, t: f64, size: Size) -> Frame3d {
        let t = if self.still { 0.0 } else { t as f32 };
        let r = 100.0 * size.h / H;
        // Turned about the vertical so its left side comes forward, rocking a little.
        let turn = 0.75 + 0.12 * (0.5 * t).sin();
        let pose = Mat4::rotate_z(0.22).then(&Mat4::rotate_y(turn));
        let (ring_c, bead_c) = if self.light { (0x1f5fbf, 0xd9480f) } else { (0x6fe8ff, 0xffb35c) };
        let mut meshes = vec![MeshDraw {
            mesh: MeshId(0),
            model: Mat4::uniform_scale(r).then(&Mat4::rotate_x(FRAC_PI_2)).then(&pose),
            material: Material { color: Color::hex(ring_c, 1.0), emissive: 0.6, ..Default::default() },
        }];
        for i in 0..BEADS {
            let a = 0.8 * t + i as f32 * std::f32::consts::TAU / BEADS as f32;
            let at = Vec3::new(r * a.cos(), r * a.sin(), 0.0);
            meshes.push(MeshDraw {
                mesh: MeshId(1),
                model: Mat4::uniform_scale(9.0 * size.h / H)
                    .then(&Mat4::rotate_x(1.3 * t + i as f32))
                    .then(&Mat4::rotate_y(0.9 * t))
                    .then(&Mat4::translation(at))
                    .then(&pose),
                material: Material { color: Color::hex(bead_c, 1.0), emissive: 0.5, ..Default::default() },
            });
        }
        Frame3d {
            meshes,
            light: Light { dir: Vec3::new(0.3, -0.6, -1.0), ..Default::default() },
            on_stage: Some(OnStage { overscan: 0.0 }),
            ..Default::default()
        }
    }

    fn until(&self) -> f64 {
        if self.still {
            0.0
        } else {
            f64::INFINITY
        }
    }
}

/// How the ring is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    None,
    /// `Element::gpu`: all of it under the text.
    Whole,
    /// `Element::gpu_split`.
    Split,
}

pub struct Look {
    pub bg: Color,
    pub text: Color,
}

pub fn look(light: bool) -> Look {
    if light {
        Look { bg: Color::hex(0xf1eee7, 1.0), text: Color::hex(0x1b1e24, 1.0) }
    } else {
        Look { bg: Color::hex(0x0b0f17, 1.0), text: Color::hex(0xf2f4f7, 1.0) }
    }
}

/// The popup: the ring over the whole box, then three lines of live text in the middle.
/// `bg: false` leaves the root clear, to measure text coverage.
pub fn view<M: 'static>(ring: &Arc<dyn GpuSource>, words: &[Word], mode: Mode, light: bool, bg: bool) -> Element<M> {
    let l = look(light);
    let live = Element::live_text("live", words).text_style(TextStyle::new(0, 26.0, 32.0)).color(l.text).width(380.0);
    let mut root = Element::panel("root").size(W, H);
    if bg {
        root = root.fill(l.bg);
    }
    match mode {
        Mode::None => {}
        Mode::Whole => root = root.child(at(Element::gpu("ring", ring.clone()).size(W, H), 0.0, 0.0)),
        Mode::Split => root = root.child(at(Element::gpu_split("ring", ring.clone()).size(W, H), 0.0, 0.0)),
    }
    root.child(at(live, 110.0, 96.0))
}

fn at<M>(e: Element<M>, x: f32, y: f32) -> Element<M> {
    e.layout(|s| {
        s.position = vo_scene::taffy::style::Position::Absolute;
        s.inset.left = vo_scene::taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = vo_scene::taffy::style::LengthPercentageAuto::length(y);
    })
}

pub fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).expect("test font");
    fonts
}

/// The display list at `t` with the words that have arrived by then.
pub fn sample(
    fonts: &FontRegistry,
    ring: &Arc<dyn GpuSource>,
    mode: Mode,
    light: bool,
    bg: bool,
    t: f64,
) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(view(ring, &words_at(t), mode, light, bg), 0.0);
    scene.sample(t)
}

/// The app, for the envelope check: words arrive, the ring orbits.
pub struct Demo {
    ring: Arc<dyn GpuSource>,
    words: Vec<Word>,
    light: bool,
}

impl Demo {
    pub fn new(still: bool, light: bool) -> Demo {
        Demo { ring: item(Ring { still, light }), words: Vec::new(), light }
    }
}

impl vo::App for Demo {
    type Msg = Vec<Word>;

    fn update(&mut self, words: Vec<Word>, _: &mut vo::Cx<Vec<Word>>) {
        self.words = words;
    }

    fn view(&self, _: &vo::Tokens) -> Element<Vec<Word>> {
        view(&self.ring, &self.words, Mode::Split, self.light, true)
    }
}

pub fn scenario() -> vo::Scenario<Vec<Word>> {
    let inputs = (1..=TEXT.len()).map(|n| (arrival(n - 1), vo::Input::Msg(words(n)))).collect();
    vo::Scenario { size: Size::new(W, H), scale: 1.0, duration: DURATION, inputs }
}

pub fn options(fonts: FontRegistry, reduced: bool) -> vo::Options<Vec<Word>> {
    vo::Options { fonts, reduced_motion: reduced, renderer_setup: Some(Arc::new(install)), ..vo::Options::default() }
}

fn gpu(timestamps: bool) -> Option<(wgpu::Device, wgpu::Queue)> {
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
    let features = if timestamps { vo_3d::timer::FEATURES } else { wgpu::Features::empty() };
    if !adapter.features().contains(features) {
        return None;
    }
    Some(
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("orbit-words-demo"),
            required_features: features,
            ..Default::default()
        }))
        .expect("a GPU device"),
    )
}

fn bench(fonts: FontRegistry) {
    let Some((device, queue)) = gpu(true) else {
        println!("orbit_words_demo bench: this GPU has no timestamp queries; no timing");
        return;
    };
    let period = queue.get_timestamp_period() as f64;
    let mut renderer = Renderer::with_fonts(&device, &queue, fonts.clone());
    install(&mut renderer);
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let buf = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
    let resolve = buf(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buf(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    let (w, h) = (W as u32, H as u32);
    let target = Target::new(&device, w, h, FORMAT);
    let ring = item(Ring { still: false, light: false });
    // Pre-sampled lists, so the timing is the GPU's alone.
    let lists = |mode| (0..200).map(|i| sample(&fonts, &ring, mode, false, true, 4.0 + i as f64 / 60.0)).collect();
    let runs: [(&str, Mode, Vec<DisplayList>); 3] = [
        ("no ring", Mode::None, lists(Mode::None)),
        ("whole", Mode::Whole, lists(Mode::Whole)),
        ("split", Mode::Split, lists(Mode::Split)),
    ];
    let mut p95 = Vec::new();
    // Interleaved rounds, so clock changes hit the three alike.
    let mut times: Vec<Vec<f64>> = vec![Vec::new(); runs.len()];
    for round in 0..3 {
        for (k, (_, _, lists)) in runs.iter().enumerate() {
            for (i, list) in lists.iter().enumerate() {
                let mut enc = device.create_command_encoder(&Default::default());
                enc.write_timestamp(&qs, 0);
                renderer.encode(list, 1.0, &target, &mut enc);
                enc.write_timestamp(&qs, 1);
                enc.resolve_query_set(&qs, 0..2, &resolve, 0);
                enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, 16);
                queue.submit([enc.finish()]);
                read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                let ts: [u64; 2] = bytemuck::pod_read_unaligned(&read.slice(..).get_mapped_range().unwrap()[..16]);
                read.unmap();
                if round > 0 || i >= 40 {
                    times[k].push((ts[1] - ts[0]) as f64 * period / 1e6);
                }
            }
        }
    }
    for ((name, mode, _), t) in runs.iter().zip(&mut times) {
        t.sort_by(f64::total_cmp);
        let (median, q) = (t[t.len() / 2], t[t.len() * 95 / 100]);
        p95.push(q);
        let _ = mode;
        println!("orbit_words_demo bench: {name:8} {w}x{h}: {median:.3} ms median, {q:.3} ms p95");
    }
    let (whole, split) = (p95[1] - p95[0], p95[2] - p95[0]);
    println!(
        "orbit_words_demo bench: the ring costs {whole:.3} ms p95 whole, {split:.3} ms split in two halves: {:.2}x \
         (limit 1.3x); frame p95 {:.3} ms (budget 1 ms)",
        split / whole.max(1e-6),
        p95[2]
    );
}

/// The moments the sheet shows.
const SHEET_AT: [f64; 5] = [1.2, 2.2, 3.0, 4.0, 4.8];

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut check) = (PathBuf::from("/var/tmp/claude/swarm/EG-20"), false);
    let fonts = fonts();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--check" => check = true,
            "--bench" => return bench(fonts),
            _ => panic!("unknown option {a}"),
        }
    }
    let (w, h) = (W as u32, H as u32);
    let (device, queue) = gpu(false).expect("a GPU device");
    let mut off = Offscreen::with_fonts(&device, &queue, fonts.clone());
    install(&mut off.renderer);
    // Dark, light, reduced motion, and the whole ring under the text for comparison.
    let rows: [(Arc<dyn GpuSource>, Mode, bool); 4] = [
        (item(Ring { still: false, light: false }), Mode::Split, false),
        (item(Ring { still: false, light: true }), Mode::Split, true),
        (item(Ring { still: true, light: false }), Mode::Split, false),
        (item(Ring { still: false, light: false }), Mode::Whole, false),
    ];
    let (cols, gap) = (SHEET_AT.len() as u32, 8u32);
    let (sw, sh) = (cols * w + (cols + 1) * gap, rows.len() as u32 * h + (rows.len() as u32 + 1) * gap);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    let mut differ = 0;
    for (row, (ring, mode, light)) in rows.iter().enumerate() {
        for (c, &t) in SHEET_AT.iter().enumerate() {
            let rgba = off.render_to_rgba(&sample(&fonts, ring, *mode, *light, true, t), [w, h], 1.0);
            if check {
                let again = off.render_to_rgba(&sample(&fonts, ring, *mode, *light, true, t), [w, h], 1.0);
                differ += (again != rgba) as usize;
            }
            let (x0, y0) = (gap + c as u32 * (w + gap), gap + row as u32 * (h + gap));
            for y in 0..h {
                let src = (y * w * 4) as usize;
                let dst = (((y0 + y) * sw + x0) * 4) as usize;
                sheet[dst..dst + (w * 4) as usize].copy_from_slice(&rgba[src..src + (w * 4) as usize]);
            }
        }
    }
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join("orbit_words_demo.png");
    write_png(&path, sw, sh, &sheet).unwrap();
    println!("orbit_words_demo: wrote {}", path.display());
    if check {
        let env = vo::envelope::Envelope::default();
        let mut ok = differ == 0;
        println!("orbit_words_demo: {differ} of {} moments differ when drawn again", rows.len() * SHEET_AT.len());
        for (name, reduced) in [("motion", false), ("reduced motion", true)] {
            let r = vo::envelope::check_scenario(
                Demo::new(reduced, false),
                &scenario(),
                60.0,
                options(fonts.clone(), reduced),
                &env,
            );
            println!("orbit_words_demo: envelope ({name}) {r}");
            ok &= r.violations.is_empty();
        }
        if !ok {
            std::process::exit(1);
        }
    }
}
