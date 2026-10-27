//! Word particles from a source (MO-69): a word's particles start where the source's light
//! was when the word was said, not in a cloud round the word, and the word still ends as the
//! same solid text.
//!
//! GPU test: `just test-gpu word_source`.

mod support;

use vo_core::words::WordModel;
use vo_render::{Renderer, Target};
use vo_scene::element::{Element, TextStyle};
use vo_scene::emitter::{WordParticles, WordSource};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Point, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const W: u32 = 320;
const H: u32 = 200;
/// The text sits below this line; the source is above it.
const TEXT_TOP: f32 = 130.0;

/// A source standing still at the top left: its arc has one point.
fn source() -> WordSource {
    WordSource {
        centre: Point::new(40.0, 20.0),
        radius: [0.0, 0.0],
        from: 0.0,
        to: 0.0,
        period: 3.0,
        lag: 0.2,
        spread: 0.2,
    }
}

fn view(model: &WordModel, from: Option<WordSource>) -> Element<()> {
    Element::column("root").pad_edges(TEXT_TOP, 100.0, 0.0, 100.0).child(
        Element::live_text("live", model.words())
            .text_style(TextStyle::new(0, 30.0, 40.0))
            .word_particles(WordParticles { from, ..WordParticles::default() }),
    )
}

/// Frames at `keep`, the word "hello" coming at 0.3 s.
fn render(gpu: &support::Gpu, fonts: &FontRegistry, from: Option<WordSource>, keep: &[f64]) -> Vec<Vec<u8>> {
    let mut renderer = Renderer::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let target = Target::new(&gpu.device, W, H, FORMAT);
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W as f32, H as f32), 1.0);
    let mut model = WordModel::new();
    scene.apply(view(&model, from), 0.0);
    let fps = 60.0;
    let mut kept = Vec::new();
    let mut said = false;
    for i in 0..=(1.6 * fps) as u64 {
        let t = i as f64 / fps;
        if !said && t >= 0.3 {
            model.update("hello");
            scene.apply(view(&model, from), 0.3);
            said = true;
        }
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

/// Summed alpha in rows `y0..y1`.
fn ink(px: &[u8], y0: u32, y1: u32) -> u64 {
    (y0..y1).flat_map(|y| (0..W).map(move |x| ((y * W + x) * 4 + 3) as usize)).map(|i| px[i] as u64).sum()
}

#[test]
fn word_particles_start_at_their_source_and_end_as_the_same_text() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    // Just after the word came, and long after it formed.
    let times = [0.32, 1.5];
    let cloud = render(&gpu, &fonts, None, &times);
    let from = render(&gpu, &fonts, Some(source()), &times);
    let top = TEXT_TOP as u32 - 40;
    let (a, b) = (ink(&cloud[0], 0, top), ink(&from[0], 0, top));
    assert!(b > 4 * a.max(50), "particles near the source: {b} with it, {a} without");
    let differ = cloud[1].iter().zip(&from[1]).filter(|(p, q)| p.abs_diff(**q) > 4).count();
    assert!(differ < (W * H) as usize / 500, "the formed word differs in {differ} bytes");
}
