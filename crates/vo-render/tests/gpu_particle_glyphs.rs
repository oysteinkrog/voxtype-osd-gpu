//! Glyph particles (EG-27): each free particle drawn as a small turned letter by the Slug
//! glyph path. A letter at rest is the letter drawn as text; a seek back draws what a fresh
//! run draws; in light mode the letters blend normally and show on a light background.
//!
//! GPU test: `just test-gpu particle_glyphs`.

mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_render::text::slug_gpu::Atlas;
use vo_render::{Renderer, Target as RenderTarget};
use vo_scene::element::{Element, TextStyle};
use vo_scene::emitter::{Emitter, EmitterLook, EmitterSim, EmitterSource, Shape, SpriteStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Point, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const W: u32 = 200;
const H: u32 = 120;
const FPS: f64 = 60.0;
/// Spawn odds on the first ticks, and the seed under which they give exactly one letter in
/// the pool of 64 (`a_letter_at_rest_is_the_letter_as_text` checks the ink is one letter's).
const ONE_EMIT: f32 = 1.0 / 64.0;
const ONE_SEED: u32 = 2;

/// One letter at rest, or a fountain of turning letters.
#[derive(Debug)]
struct Letters {
    sim: EmitterSim,
    /// Spawn only on the first tick.
    once: bool,
    look: EmitterLook,
}

impl EmitterSource for Letters {
    fn sim(&self, t: f64) -> EmitterSim {
        // A particle that does not spawn waits up to a quarter of its life to try again.
        if self.once && t > 0.02 {
            return EmitterSim { emit: 0.0, ..self.sim };
        }
        self.sim
    }

    fn look(&self, _: f64) -> EmitterLook {
        self.look
    }
}

fn look(style: SpriteStyle, ink: Color) -> EmitterLook {
    EmitterLook { ramp: [ink; 3], style, ..Default::default() }
}

/// One particle that never moves and lives 20 s, born on the first ticks: fully faded in
/// from 2 s on. The pool holds 64, the fewest it can; the rest never spawn.
fn still(size: f32) -> Letters {
    Letters {
        sim: EmitterSim { emit: ONE_EMIT, life: 20.0, life_jitter: 0.0, drag: 0.0, ..Default::default() },
        once: true,
        look: look(SpriteStyle::glyphs(0, "R", size), Color::WHITE),
    }
}

fn fountain(ink: Color, size: f32) -> Letters {
    Letters {
        sim: EmitterSim {
            shape: Shape::Disc { radius: 20.0 },
            life: 2.0,
            spawn_speed: 40.0,
            noise: 30.0,
            drag: 0.3,
            ..Default::default()
        },
        once: false,
        look: look(SpriteStyle::glyphs(0, "voxtype", size), ink),
    }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

/// The emitter alone, `W` x `H`, on `bg` (none when transparent).
fn view(source: Arc<dyn EmitterSource>, capacity: u32, bg: Option<Color>) -> Element<()> {
    view_seeded(source, capacity, bg, 11)
}

fn view_seeded(source: Arc<dyn EmitterSource>, capacity: u32, bg: Option<Color>, seed: u32) -> Element<()> {
    let e = Element::emitter("e", Emitter::new(source, capacity, seed)).size(W as f32, H as f32);
    match bg {
        Some(c) => Element::panel("bg").size(W as f32, H as f32).fill(c).child(e),
        None => Element::column("root").child(e),
    }
}

/// How a run draws: the scale, light mode and reduced motion.
#[derive(Clone, Copy)]
struct Mode {
    scale: f32,
    light: bool,
    reduced: bool,
}

const DARK: Mode = Mode { scale: 1.0, light: false, reduced: false };

/// Renders `view` at each of `times` in order, every frame drawn, and returns the frame at the
/// last time.
fn render(gpu: &support::Gpu, fonts: &FontRegistry, view: Element<()>, mode: Mode, times: &[f64]) -> Vec<u8> {
    let Mode { scale, light, reduced } = mode;
    let mut renderer = Renderer::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    renderer.set_light(light);
    let (w, h) = ((W as f32 * scale) as u32, (H as f32 * scale) as u32);
    let target = RenderTarget::new(&gpu.device, w, h, FORMAT);
    let mut scene: Scene<()> =
        Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone()))).with_reduced_motion(reduced);
    scene.set_viewport(0.0, Size::new(W as f32, H as f32), scale);
    scene.apply(view, 0.0);
    let mut out = Vec::new();
    for (i, &t) in times.iter().enumerate() {
        let list = scene.sample(t);
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        renderer.encode(&list, scale, &target, &mut enc);
        if i == times.len() - 1 {
            out = read(gpu, &target, w, h, enc);
        } else {
            gpu.queue.submit([enc.finish()]);
        }
    }
    out
}

/// Every frame at `FPS` from 0 to `t`.
fn frames(t: f64) -> Vec<f64> {
    (0..=(t * FPS).round() as u64).map(|i| i as f64 / FPS).collect()
}

/// Submits `enc` with a copy of the target and reads the pixels back.
fn read(gpu: &support::Gpu, target: &RenderTarget, w: u32, h: u32, mut enc: wgpu::CommandEncoder) -> Vec<u8> {
    let row = (w * 4).next_multiple_of(256);
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: row as u64 * h as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        target.texture().as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    gpu.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = buf.slice(..).get_mapped_range().expect("mapped");
    (0..h).flat_map(|y| data[(y * row) as usize..(y * row + w * 4) as usize].to_vec()).collect()
}

/// The coverage-weighted centre of an RGBA image `w` wide, and its total coverage.
fn centroid(px: &[u8], w: u32) -> (f64, f64, f64) {
    let (mut sx, mut sy, mut sa) = (0.0, 0.0, 0.0);
    for (i, p) in px.chunks(4).enumerate() {
        let a = p[3] as f64;
        sx += a * (i as u32 % w) as f64;
        sy += a * (i as u32 / w) as f64;
        sa += a;
    }
    (sx / sa, sy / sa, sa / 255.0)
}

#[test]
fn a_letter_at_rest_is_the_letter_as_text() {
    rest_is_text(still(40.0), DARK);
}

#[test]
fn with_reduced_motion_a_letter_holds_still_and_upright() {
    // Launched fast and spinning, it still stays where it is born, unturned.
    let mut letters = still(40.0);
    letters.sim.spawn_speed = 80.0;
    letters.sim.swirl = 2.0;
    letters.sim.gravity = [0.0, 30.0, 0.0];
    letters.look.style = match letters.look.style {
        SpriteStyle::Glyph { font, chars, size, .. } => SpriteStyle::Glyph { font, chars, size, spin: 0.5 },
        s => s,
    };
    rest_is_text(letters, Mode { reduced: true, ..DARK });
}

/// The one letter of `letters` (born at the emitter's centre), drawn in `mode`, is the same
/// pixels as the letter drawn as text there.
fn rest_is_text(letters: Letters, mode: Mode) {
    let gpu = support::gpu();
    let fonts = fonts();
    let SpriteStyle::Glyph { size, .. } = letters.look.style else { panic!("not letters") };
    let particle = render(&gpu, &fonts, view_seeded(Arc::new(letters), 1, None, ONE_SEED), mode, &frames(3.0));
    // The same letter as text, its ink box centred on the emitter's centre, as the glyph
    // particle shader centres it.
    let face = fonts.get(0).unwrap();
    let gid = {
        use skrifa::MetadataProvider;
        face.skrifa().charmap().map('R').unwrap()
    };
    let ext = Atlas::default().glyph_in_at(0, || (face.skrifa(), face.location()), gid).unwrap().extents;
    let k = size / face.units_per_em() as f32;
    let mid = (0.5 * (ext[0] + ext[2]), 0.5 * (ext[1] + ext[3]));
    let pen = Point::new(0.5 * W as f32 - k * mid.0, 0.5 * H as f32 + k * mid.1);
    let text = {
        let mut renderer = Renderer::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
        let target = RenderTarget::new(&gpu.device, W, H, FORMAT);
        // Laid out once to find the pen, then laid out afresh moved to the particle's pen.
        let sample = |d: Point| {
            let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
            scene.set_viewport(0.0, Size::new(W as f32, H as f32), 1.0);
            let l = Element::label("t", "R").text_style(TextStyle::new(0, size, size * 1.2)).color(Color::WHITE);
            scene.apply(Element::column("root").child(l.offset(d.x, d.y)), 0.0);
            scene.sample(0.0)
        };
        let at = sample(Point::ZERO).glyphs[0].pos;
        let list = sample(Point::new(pen.x - at.x, pen.y - at.y));
        assert_eq!(list.glyphs.len(), 1, "one glyph");
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        renderer.encode(&list, 1.0, &target, &mut enc);
        read(&gpu, &target, W, H, enc)
    };
    // The mean over the pixels either one inks, not the whole frame, which is mostly empty.
    let pairs: Vec<(&[u8], &[u8])> =
        particle.chunks(4).zip(text.chunks(4)).filter(|(a, b)| a[3] > 0 || b[3] > 0).collect();
    let diff = pairs.iter().flat_map(|(a, b)| a.iter().zip(*b)).map(|(a, b)| a.abs_diff(*b) as f64).sum::<f64>()
        / (4 * pairs.len().max(1)) as f64;
    let worst = particle.iter().zip(&text).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    let (pc, tc) = (centroid(&particle, W), centroid(&text, W));
    println!(
        "rest: {} inked pixels, mean difference {diff:.3}, worst {worst}, ink {:.1} and {:.1}",
        pairs.len(),
        pc.2,
        tc.2
    );
    assert!(tc.2 > 100.0, "the letter is drawn: {:.1} pixels of ink", tc.2);
    assert!((pc.2 - tc.2).abs() < 0.01 * tc.2, "one letter's ink: {:.1}, text {:.1}", pc.2, tc.2);
    assert!((pc.0 - tc.0).hypot(pc.1 - tc.1) < 0.05, "centres {pc:?} and {tc:?}");
    assert!(diff <= 2.0, "mean difference {diff} of 255");
}

#[test]
fn a_seek_back_draws_what_a_fresh_run_draws() {
    let gpu = support::gpu();
    let fonts = fonts();
    let src: Arc<dyn EmitterSource> = Arc::new(fountain(Color::WHITE, 10.0));
    let fresh = render(&gpu, &fonts, view(src.clone(), 600, None), DARK, &frames(0.9));
    let mut times = frames(1.4);
    times.push(0.9);
    let seek = render(&gpu, &fonts, view(src, 600, None), DARK, &times);
    let differ = seek.iter().zip(&fresh).filter(|(a, b)| a != b).count();
    let lit = fresh.chunks(4).filter(|c| c[3] > 0).count();
    println!("seek: {differ} bytes differ, {lit} lit pixels");
    assert!(lit > 500, "letters are drawn");
    assert_eq!(differ, 0, "a seek back gives a different frame");
}

/// Relative luminance of an sRGB pixel, 0 to 1.
fn luminance(p: &[u8]) -> f32 {
    let lin = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(p[0]) + 0.7152 * lin(p[1]) + 0.0722 * lin(p[2])
}

#[test]
fn in_light_mode_the_letters_show_on_a_light_background() {
    let gpu = support::gpu();
    let fonts = fonts();
    let paper = Color::hex(0xf4f1ea, 1.0);
    let ink = Color::hex(0x1f2933, 1.0);
    let px = render(
        &gpu,
        &fonts,
        view(Arc::new(fountain(ink, 12.0)), 600, Some(paper)),
        Mode { light: true, ..DARK },
        &frames(1.2),
    );
    let back = luminance(&px[..4]);
    // The darkest tenth of the pixels that differ from the paper: the letters' cores.
    let mut marked: Vec<f32> =
        px.chunks(4).filter(|p| p[..3].iter().zip(&px[..3]).any(|(a, b)| a.abs_diff(*b) > 24)).map(luminance).collect();
    marked.sort_by(f32::total_cmp);
    let core = &marked[..(marked.len() / 10).max(1)];
    let letters = core.iter().sum::<f32>() / core.len() as f32;
    println!("light: {} marked pixels, letters {letters:.3}, paper {back:.3}", marked.len());
    assert!(marked.len() > 300, "{} letter pixels", marked.len());
    assert!(back - letters >= 0.3, "letters {letters}, paper {back}");
}

#[test]
fn glyph_particles_match_their_goldens() {
    let gpu = support::gpu();
    let fonts = fonts();
    let mut errors = Vec::new();
    for (name, scale, light) in
        [("particle-glyphs", 1.0f32, false), ("particle-glyphs-1.5x", 1.5, false), ("particle-glyphs-light", 1.0, true)]
    {
        let (ink, bg) = if light {
            (Color::hex(0x1f2933, 1.0), Color::hex(0xf4f1ea, 1.0))
        } else {
            (Color::hex(0x9be7ff, 1.0), Color::hex(0x0b0e14, 1.0))
        };
        let px = render(
            &gpu,
            &fonts,
            view(Arc::new(fountain(ink, 11.0)), 600, Some(bg)),
            Mode { scale, light, reduced: false },
            &frames(1.2),
        );
        let (w, h) = ((W as f32 * scale) as u32, (H as f32 * scale) as u32);
        let img = Image { name: name.into(), w, h, channels: 4, px };
        if let Err(e) = support::check(&img, Tolerance { per_pixel: 4, max_fraction: 0.002 }) {
            errors.push(e);
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
