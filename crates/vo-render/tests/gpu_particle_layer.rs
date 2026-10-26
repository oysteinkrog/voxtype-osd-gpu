//! Particles from a scene: an emitter element inside a bloom layer whose level the app sets,
//! and live text whose words form from particles, drawn by the renderer itself. Checks that
//! the frames do not depend on the frame rate or on a source rebuilt for every new level.
//!
//! GPU test: `just test-gpu particle_layer`.

mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_core::words::WordModel;
use vo_render::{Renderer, Target};
use vo_scene::element::{Element, TextStyle};
use vo_scene::emitter::{Emitter, RingEmitter, WordParticles};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const W: u32 = 360;
const H: u32 = 240;

/// What the app sends: voice levels, a burst, and dictated words.
enum Event {
    Level(f32),
    Burst,
    Words(&'static str),
}

fn script() -> Vec<(f64, Event)> {
    let mut s = Vec::new();
    for i in 0..48 {
        let t = 0.05 + i as f64 / 30.0;
        let l = (0.5 + 0.5 * (t * 9.0).sin()).powi(2) as f32;
        s.push((t, Event::Level(l)));
    }
    s.push((0.3, Event::Words("hello")));
    s.push((0.7, Event::Words("hello wards")));
    s.push((1.0, Event::Words("hello words")));
    s.push((1.2, Event::Burst));
    s.sort_by(|a, b| a.0.total_cmp(&b.0));
    s
}

struct App {
    ring: RingEmitter,
    model: WordModel,
    bursts: Vec<f64>,
}

impl App {
    fn view(&self) -> Element<()> {
        let mut ring = self.ring.clone();
        ring.bursts = Arc::from(self.bursts.as_slice());
        let emitter = Emitter::new(Arc::new(ring), 4096, 7);
        Element::column("root")
            .pad(12.0)
            .child(Element::layer("glow").bloom(1.2, 10.0).child(Element::emitter("ring", emitter).size(336.0, 150.0)))
            .child(
                Element::live_text("live", self.model.words())
                    .text_style(TextStyle::new(0, 30.0, 40.0))
                    .word_particles(WordParticles::default()),
            )
    }
}

fn render(gpu: &support::Gpu, fonts: &FontRegistry, fps: f64, keep: &[f64]) -> Vec<Vec<u8>> {
    let mut renderer = Renderer::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let target = Target::new(&gpu.device, W, H, FORMAT);
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W as f32, H as f32), 1.0);
    let mut app = App { ring: RingEmitter::new(55.0), model: WordModel::new(), bursts: Vec::new() };
    scene.apply(app.view(), 0.0);
    let events = script();
    let mut next = 0;
    let mut kept = Vec::new();
    let n = (1.6 * fps).round() as u64;
    for i in 0..=n {
        let t = i as f64 / fps;
        while next < events.len() && events[next].0 <= t {
            let (te, e) = &events[next];
            match e {
                Event::Level(l) => app.ring.level.push(*te, *l),
                Event::Burst => app.bursts.push(*te),
                Event::Words(w) => {
                    app.model.update(w);
                }
            }
            scene.apply(app.view(), *te);
            next += 1;
        }
        // Every frame is drawn, as a live session would; some are kept.
        let list = scene.sample(t);
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        renderer.encode(&list, 1.0, &target, &mut enc);
        if keep.iter().any(|k| (k * fps).round() as u64 == i) {
            kept.push(read(gpu, &target, enc));
        } else {
            gpu.queue.submit([enc.finish()]);
        }
    }
    kept
}

/// Submits `enc` with a copy of the target and reads the pixels back.
fn read(gpu: &support::Gpu, target: &Target, mut enc: wgpu::CommandEncoder) -> Vec<u8> {
    let row = (W * 4).next_multiple_of(256);
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: row as u64 * H as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        target.texture().as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
        },
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
    );
    gpu.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = buf.slice(..).get_mapped_range().expect("mapped");
    (0..H).flat_map(|y| data[(y * row) as usize..(y * row + W * 4) as usize].to_vec()).collect()
}

#[test]
fn scene_particles_are_the_same_at_any_frame_rate() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    // On both the 60 fps and the 144 Hz grid.
    let times = [0.25, 0.5, 10.0 / 12.0, 1.0, 1.25, 1.5];
    let a = render(&gpu, &fonts, 60.0, &times);
    let b = render(&gpu, &fonts, 144.0, &times);
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        let differ = x.iter().zip(y).filter(|(p, q)| p != q).count();
        assert_eq!(differ, 0, "frame at {} s: {differ} bytes differ between 60 fps and 144 Hz", times[i]);
    }
    let img = Image { name: "particle-layer".into(), w: W, h: H * times.len() as u32, channels: 4, px: a.concat() };
    support::check(&img, Tolerance { per_pixel: 4, max_fraction: 0.002 }).unwrap();
}

/// A tilted torus of out-of-focus particles (D302, FX16).
#[derive(Debug)]
struct Torus;

impl vo_scene::emitter::EmitterSource for Torus {
    fn sim(&self, _: f64) -> vo_scene::emitter::EmitterSim {
        vo_scene::emitter::EmitterSim { swirl: 0.8, life: 2.0, ..vo_scene::emitter::EmitterSim::torus(70.0, 10.0) }
    }

    fn look(&self, _: f64) -> vo_scene::emitter::EmitterLook {
        vo_scene::emitter::EmitterLook {
            rotation: vo_scene::emitter::tilt_x(1.1),
            size: 1.2,
            brightness: 0.3,
            ramp: [Color::hex(0xbfe0ff, 1.0), Color::hex(0x7aa8ff, 0.8), Color::hex(0x5060ff, 0.0)],
            style: vo_scene::emitter::SpriteStyle::Bokeh { focus: 0.55, aperture: 25.0 },
            ..Default::default()
        }
    }
}

#[test]
fn a_3d_emitter_looks_the_same_in_an_offscreen_layer() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    let frame = |offscreen: bool| {
        let mut renderer = Renderer::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
        let target = Target::new(&gpu.device, W, H, FORMAT);
        let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
        scene.set_viewport(0.0, Size::new(W as f32, H as f32), 1.0);
        let mut e = Emitter::new(Arc::new(Torus), 4096, 3);
        e.three_d = true;
        // Off centre, so the offscreen layer's texture does not start at the target's corner.
        let torus = Element::emitter("torus", e).size(200.0, 140.0);
        let holder = if offscreen { Element::layer("holder").offscreen() } else { Element::layer("holder") };
        let view = Element::column("root").pad_xy(130.0, 60.0).child(holder.child(torus));
        scene.apply(view, 0.0);
        let mut px = Vec::new();
        for i in 0..=60 {
            let t = i as f64 / 60.0;
            let list = scene.sample(t);
            let mut enc = gpu.device.create_command_encoder(&Default::default());
            renderer.encode(&list, 1.0, &target, &mut enc);
            if i == 60 {
                px = read(&gpu, &target, enc);
            } else {
                gpu.queue.submit([enc.finish()]);
            }
        }
        px
    };
    let direct = frame(false);
    let layered = frame(true);
    let lit = direct.chunks(4).filter(|c| c[3] > 8).count();
    // The offscreen texture holds 8-bit sums, so a few bright overlaps round differently;
    // the shape and its place must not change.
    let off = direct.iter().zip(&layered).filter(|(a, b)| a.abs_diff(**b) > 3).count();
    let centre = |px: &[u8]| {
        let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
        for (i, c) in px.chunks(4).enumerate() {
            let k = c[3] as f64;
            sx += k * (i as u32 % W) as f64;
            sy += k * (i as u32 / W) as f64;
            sw += k;
        }
        (sx / sw, sy / sw, sw)
    };
    let (a, b) = (centre(&direct), centre(&layered));
    println!("lit pixels {lit}, {off} channels differ by more than 3; centre {a:?} and {b:?}");
    assert!(lit > 2000, "the torus draws: {lit}");
    assert!(off * 1000 < direct.len(), "an offscreen layer changes the torus: {off} channels");
    assert!((a.0 - b.0).abs() < 0.05 && (a.1 - b.1).abs() < 0.05, "an offscreen layer moves the torus");
    assert!((a.2 - b.2).abs() < a.2 * 1e-3, "an offscreen layer changes the light");
    let img = Image { name: "particle-layer-3d".into(), w: W, h: H, channels: 4, px: direct };
    support::check(&img, Tolerance { per_pixel: 4, max_fraction: 0.002 }).unwrap();
}
