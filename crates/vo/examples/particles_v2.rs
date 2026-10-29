//! Particles v2 studies: points of light instead of soft blobs. Two layouts, each built only
//! from scene elements:
//!
//! - `--concept 09`: two tilted rings of sparks around the mic, coloured by angle, with
//!   dotted strands of sparks flowing out to both sides, and live text whose words form from
//!   distinct dots and hand over to solid text with no change in light.
//! - `--concept 24`: a river of light: glowing ribbons that sway from the mic to the text,
//!   with sparks flowing along the same strands.
//! - `--concept 26`: shapes (EG-26): a swarm of sparks above the text forms a check mark at
//!   1.2 s, then scatters into a microphone icon read from SVG at 2.6 s.
//!
//! Offline, to PNG frames (default 600x300 at scale 1):
//!   cargo run --release -p vo --example particles_v2 -- --concept 09 --offline DIR [--fps 60]
//! GPU time per frame, ticks included, at 600x300 and 1920x1080:
//!   cargo run --release -p vo --example particles_v2 -- --bench
//! In the preview and scrub tool:
//!   cargo run -p vo --example particles_v2 -- --concept 24 --preview

use std::path::PathBuf;
use std::sync::Arc;

use vo::scene::element::{Element, TextStyle};
use vo::scene::emitter::{
    tilt_x, ColorBy, Emitter, EmitterLook, EmitterSim, EmitterSource, LevelTrack, Shape, ShapeTarget, SpriteStyle,
    Target as Form, WordParticles,
};
use vo::scene::geom::{Color, Point, Size};
use vo::scene::Scene;
use vo::{App, Cx, FontRegistry, Options, Scenario, ShapedLayout};
use vo_core::words::WordModel;
use vo_render::{Renderer, Target};

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Level(f32),
    Words(&'static str),
}

/// What a recogniser and a microphone would send, as `(seconds, message)`.
fn script() -> Vec<(f64, Msg)> {
    let mut s = Vec::new();
    for i in 0..150 {
        let t = 0.1 + i as f64 / 30.0;
        let phrase = (0.5 + 0.5 * (t * 1.9).sin()) as f32;
        let syllable = (0.5 + 0.5 * (t * 12.0).sin()).powi(3) as f32;
        s.push((t, Msg::Level((0.1 + 0.9 * phrase * syllable).min(1.0))));
    }
    let words = [
        (0.4, "make"),
        (0.8, "make it"),
        (1.1, "make it feel"),
        (1.5, "make it feel a"),
        (1.8, "make it feel a lot"),
        (2.2, "make it feel a lot more"),
        (2.7, "make it feel a lot more polish"),
        (3.1, "make it feel a lot more polished"),
    ];
    s.extend(words.iter().map(|&(t, w)| (t, Msg::Words(w))));
    s
}

/// A rotation about z by `a` radians after `m`, as rows.
fn turn(m: [[f32; 3]; 3], a: f32) -> [[f32; 3]; 3] {
    let (s, c) = a.sin_cos();
    let rz = [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]];
    let mut out = [[0.0; 3]; 3];
    for (r, row) in out.iter_mut().enumerate() {
        for (k, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|j| rz[r][j] * m[j][k]).sum();
        }
    }
    out
}

/// A tilted ring of sparks that swells and spins with the voice (concept 09).
#[derive(Debug)]
struct HaloRing {
    level: LevelTrack,
    radius: f32,
    tilt: f32,
    spin: f32,
    ramp: [Color; 3],
}

impl EmitterSource for HaloRing {
    fn sim(&self, t: f64) -> EmitterSim {
        let l = self.level.at(t).clamp(0.0, 1.2);
        EmitterSim {
            swirl: 0.3 + 1.0 * l,
            spawn_speed: 4.0 + 20.0 * l,
            noise: 10.0 + 40.0 * l,
            noise_scale: 0.025,
            life: 2.2,
            ring_pull: 7.0,
            drag: 1.4,
            level: l,
            ..EmitterSim::ring(self.radius * (1.0 + 0.06 * l), 0.22 + 0.2 * l)
        }
    }

    fn look(&self, t: f64) -> EmitterLook {
        let l = self.level.at(t).clamp(0.0, 1.2);
        EmitterLook {
            rotation: turn(tilt_x(self.tilt), self.spin),
            size: 1.0,
            brightness: 1.0 + 0.8 * l,
            ramp: self.ramp,
            style: SpriteStyle::Spark { accents: 0.06, gain: 1.0, twinkle: 0.25 },
            color_by: ColorBy::Angle,
            ..EmitterLook::default()
        }
    }
}

/// Sparks and ribbons flowing along a swaying path (concept 09's side streams, concept 24's
/// river of light).
#[derive(Debug)]
struct River {
    level: LevelTrack,
    path: Arc<[Point]>,
    strands: u32,
    sway: f32,
    ribbons: u32,
    ramp: [Color; 3],
    speed: f32,
    size: f32,
    wavelength: f32,
}

impl EmitterSource for River {
    fn sim(&self, t: f64) -> EmitterSim {
        let l = self.level.at(t).clamp(0.0, 1.2);
        EmitterSim {
            shape: Shape::Path {
                strands: self.strands,
                sway: self.sway * (0.55 + 0.45 * l),
                wavelength: self.wavelength,
                wave_speed: 0.3,
                width: 2.5,
            },
            spawn_speed: self.speed,
            emit: 1.0,
            life: 1.0,
            level: l,
            ..EmitterSim::default()
        }
    }

    fn look(&self, t: f64) -> EmitterLook {
        let l = self.level.at(t).clamp(0.0, 1.2);
        EmitterLook {
            size: self.size,
            brightness: 0.8 + 0.6 * l,
            ramp: self.ramp,
            style: SpriteStyle::Spark { accents: 0.05, gain: 1.0, twinkle: 0.25 },
            ribbons: self.ribbons,
            ribbon_width: 1.5,
            ribbon_alpha: 0.55 + 0.35 * l,
            ..EmitterLook::default()
        }
    }

    fn path(&self) -> Option<Arc<[Point]>> {
        Some(self.path.clone())
    }
}

/// A straight path from `a` to `b`.
fn line(a: Point, b: Point) -> Arc<[Point]> {
    Arc::from([a, b])
}

/// A swarm that forms a check mark, then a microphone icon (concept 26, EG-26).
#[derive(Debug)]
struct Swarm {
    targets: Arc<[ShapeTarget]>,
    ramp: [Color; 3],
}

impl Swarm {
    /// When the swarm stops spawning: its sparks die out as the check mark forms.
    const FORM: f64 = 1.2;

    fn new(ramp: [Color; 3]) -> Swarm {
        let mic = r#"<svg viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2">
            <rect x="9" y="2" width="6" height="12" rx="3"/>
            <path d="M19 10v2a7 7 0 0 1-14 0v-2"/><path d="M12 19v3"/></svg>"#;
        let mic = Form::Svg(Arc::new(vo::scene::svg::parse(mic).expect("mic icon")));
        Swarm {
            targets: Arc::from([
                ShapeTarget { sweep: 0.25, ..ShapeTarget::new(Swarm::FORM, Form::CheckMark, 96.0) },
                ShapeTarget { scatter: true, ..ShapeTarget::new(2.6, mic, 104.0) },
            ]),
            ramp,
        }
    }
}

impl EmitterSource for Swarm {
    fn sim(&self, t: f64) -> EmitterSim {
        EmitterSim {
            shape: Shape::Disc { radius: 46.0 },
            emit: if t < Swarm::FORM { 1.0 } else { 0.0 },
            life: 0.9,
            spawn_speed: 20.0,
            swirl: 1.2,
            noise: 140.0,
            noise_scale: 0.03,
            drag: 2.0,
            ..Default::default()
        }
    }

    fn look(&self, _: f64) -> EmitterLook {
        EmitterLook { size: 1.1, brightness: 0.9, ramp: self.ramp, style: SpriteStyle::SPARK, ..Default::default() }
    }

    fn targets(&self) -> Option<Arc<[ShapeTarget]>> {
        Some(self.targets.clone())
    }
}

const CYAN: u32 = 0x36d6ff;
const BLUE: u32 = 0x3a6bff;
const VIOLET: u32 = 0xa24bff;

struct Popup {
    concept: u32,
    size: Size,
    level: LevelTrack,
    model: WordModel,
    left: Arc<[Point]>,
    right: Arc<[Point]>,
    river: Arc<[Point]>,
    swarm: Arc<dyn EmitterSource>,
}

impl Popup {
    fn new(concept: u32, size: Size) -> Popup {
        let w = size.w;
        Popup {
            concept,
            size,
            level: LevelTrack::new(16.0),
            model: WordModel::new(),
            left: line(Point::new(-40.0, 0.0), Point::new(-0.5 * w + 30.0, 0.0)),
            right: line(Point::new(40.0, 0.0), Point::new(0.5 * w - 30.0, 0.0)),
            river: line(Point::new(-0.5 * w + 40.0, 0.0), Point::new(0.5 * w - 40.0, 0.0)),
            swarm: Arc::new(Swarm::new([Color::hex(0xdff4ff, 1.0), Color::hex(CYAN, 1.0), Color::hex(BLUE, 0.0)])),
        }
    }

    fn ring(&self, tilt: f32, spin: f32, ramp: [Color; 3]) -> Arc<dyn EmitterSource> {
        Arc::new(HaloRing { level: self.level.clone(), radius: 60.0, tilt, spin, ramp })
    }

    fn river(&self, path: &Arc<[Point]>, strands: u32, sway: f32, ribbons: u32, speed: f32) -> Arc<dyn EmitterSource> {
        Arc::new(River {
            level: self.level.clone(),
            path: path.clone(),
            strands,
            sway,
            ribbons,
            ramp: [Color::hex(CYAN, 1.0), Color::hex(BLUE, 1.0), Color::hex(VIOLET, 0.3)],
            speed,
            size: 0.7,
            wavelength: if ribbons > 0 { 220.0 } else { 130.0 },
        })
    }

    fn text(&self, size: f32) -> Element<Msg> {
        Element::live_text("live", self.model.words())
            .text_style(TextStyle::new(0, size, size * 1.3))
            .color(Color::hex(0xf4f7ff, 1.0))
            .word_particles(WordParticles {
                color: Color::hex(0xbfe8ff, 1.0),
                style: SpriteStyle::SPARK,
                dot_area: 5.0,
                // The newest word keeps its last glyphs as dots, as in concept 09.
                hold: if self.concept == 24 { 0.0 } else { 0.4 },
                ..WordParticles::default()
            })
    }
}

impl App for Popup {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        for (t, m) in script() {
            cx.after(t, m);
        }
    }

    fn update(&mut self, msg: Msg, cx: &mut Cx<Msg>) {
        match msg {
            Msg::Level(l) => self.level.push(cx.t(), l),
            Msg::Words(w) => {
                self.model.update(w);
            }
        }
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        let (w, h) = (self.size.w, self.size.h);
        // Emitters fill the glow layer's box, on top of each other: each after the first moves
        // up by the box height.
        let bw = w - 36.0;
        let emitter = |key: &'static str, src: Arc<dyn EmitterSource>, n: u32, seed: u32, three_d: bool, bh: f32| {
            let mut e = Emitter::new(src, n, seed);
            e.three_d = three_d;
            let up = if key == "ring-a" || key == "river" { 0.0 } else { -bh };
            Element::emitter(key, e).size(bw, bh).margin_edges(up, 0.0, 0.0, 0.0)
        };
        let card = Element::column("card")
            .pad(18.0)
            .gap(8.0)
            .radius(22.0)
            .fill(Color::hex(0x0a0f24, 0.9))
            .stroke(Color::hex(0x6d8cff, 0.35), 1.0);
        if self.concept == 24 {
            let river = Element::layer("glow").bloom(0.9, 8.0).child(emitter(
                "river",
                self.river(&self.river, 7, 46.0, 7, 170.0),
                4096,
                5,
                false,
                120.0,
            ));
            card.child(river).child(self.text(22.0))
        } else if self.concept == 26 {
            // "river" is the key that takes no margin: the only emitter here.
            let swarm =
                Element::layer("glow").bloom(0.8, 6.0).child(emitter("river", self.swarm.clone(), 2000, 8, false, 150.0));
            card.child(swarm).child(self.text(24.0))
        } else {
            let pink = [Color::hex(VIOLET, 1.0), Color::hex(0x6a5cff, 1.0), Color::hex(CYAN, 1.0)];
            let cyan = [Color::hex(CYAN, 1.0), Color::hex(BLUE, 1.0), Color::hex(VIOLET, 1.0)];
            let rings = Element::layer("glow")
                .bloom(0.8, 6.0)
                .child(emitter("ring-a", self.ring(0.95, 0.55, cyan), 1536, 3, true, 150.0))
                .child(emitter("ring-b", self.ring(1.05, -0.55, pink), 1536, 4, true, 150.0))
                .child(emitter("left", self.river(&self.left, 4, 30.0, 0, 60.0), 1024, 6, false, 150.0))
                .child(emitter("right", self.river(&self.right, 4, 30.0, 0, 60.0), 1024, 7, false, 150.0));
            card.child(rings).child(self.text(24.0))
        }
        .size(w, h)
    }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let font = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(font.as_ref()).unwrap_or_else(|e| panic!("cannot load {font}: {e}"));
    fonts
}

const SURFACE: Size = Size::new(600.0, 300.0);

fn scenario() -> Scenario<Msg> {
    Scenario { size: SURFACE, scale: 1.0, duration: 4.0, inputs: Vec::new() }
}

/// GPU time per frame, from before the particle ticks to the end of the frame, for a
/// steady 60 fps run of each concept at 600x300 and at 1920x1080.
fn bench() {
    let fonts = fonts();
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
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("particles-v2"),
        required_features: wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
        ..Default::default()
    }))
    .expect("a GPU device");
    let period = queue.get_timestamp_period() as f64;
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    for concept in [9, 24, 26] {
        for (w, h, scale) in [(600u32, 300u32, 1.0f32), (1920, 1080, 3.2)] {
            let mut renderer = Renderer::with_fonts(&device, &queue, fonts.clone());
            let target = Target::new(&device, w, h, vo_render::offscreen::FORMAT);
            let mut scene: Scene<Msg> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
            scene.set_viewport(0.0, Size::new(w as f32 / scale, h as f32 / scale), scale);
            let mut app = Popup::new(concept, SURFACE);
            let events = script();
            let mut next = 0;
            let mut times = Vec::new();
            for i in 0..240 {
                let t = i as f64 / 60.0;
                while next < events.len() && events[next].0 <= t {
                    match &events[next].1 {
                        Msg::Level(l) => app.level.push(events[next].0, *l),
                        Msg::Words(s) => {
                            app.model.update(s);
                        }
                    }
                    scene.apply(app.view(&vo::Tokens::default()), events[next].0);
                    next += 1;
                }
                if i == 0 {
                    scene.apply(app.view(&vo::Tokens::default()), 0.0);
                }
                let list = scene.sample(t);
                let mut start = device.create_command_encoder(&Default::default());
                start.write_timestamp(&qs, 0);
                queue.submit([start.finish()]);
                let mut enc = device.create_command_encoder(&Default::default());
                renderer.encode(&list, scale, &target, &mut enc);
                enc.write_timestamp(&qs, 1);
                enc.resolve_query_set(&qs, 0..2, &resolve, 0);
                enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, 16);
                queue.submit([enc.finish()]);
                read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                let ts: [u64; 2] = bytemuck::pod_read_unaligned(&read.slice(..).get_mapped_range().unwrap()[..16]);
                read.unmap();
                if i >= 30 {
                    times.push((ts[1] - ts[0]) as f64 * period / 1e6);
                }
            }
            times.sort_by(f64::total_cmp);
            println!(
                "particles_v2 bench: concept {concept:02} at {w}x{h}: GPU {:.3} ms median, {:.3} ms p95 per frame",
                times[times.len() / 2],
                times[times.len() * 95 / 100]
            );
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut offline, mut fps, mut concept, mut bench_it) = (None::<PathBuf>, 60.0, 9u32, false);
    while let Some(a) = args.next() {
        let mut v = || args.next().unwrap_or_else(|| panic!("{a} needs a value"));
        match a.as_str() {
            "--offline" => offline = Some(v().into()),
            "--fps" => fps = v().parse().expect("--fps takes a number"),
            "--concept" => concept = v().parse().expect("--concept takes 9, 24 or 26"),
            "--bench" => bench_it = true,
            "--preview" => {}
            _ => panic!("unknown option {a}"),
        }
    }
    if bench_it {
        return bench();
    }
    if vo::preview::wanted() {
        let fonts = fonts();
        let options = move || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };
        let make = move || Popup::new(concept, SURFACE);
        let preview = vo::preview::Preview::new("particles_v2", make, options, scenario()).marker("polished", 3.1);
        std::process::exit(vo::preview::main(preview));
    }
    let dir = offline.unwrap_or_else(|| panic!("give --offline DIR, --bench or --preview"));
    let fonts = fonts();
    let options = Options::<Msg> { fonts, ..Options::default() };
    let n =
        vo::render_offline_with(Popup::new(concept, SURFACE), &scenario(), fps, &dir, options).expect("offline render");
    eprintln!("particles_v2: wrote {n} frames to {}", dir.display());
}
