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
use vo_scene::geom::Size;
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
