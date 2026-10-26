//! GPU particles: determinism across frame rates and seeks, the ring emitter, and particle
//! words forming and handing over to solid text.
//!
//! GPU test: `just test-gpu particles`. Prints the formation timings it measures.

mod support;

use support::{Image, Tolerance};
use vo_render::particles::{
    EmitterLook, EmitterSim, FrameParams, GlyphState, ParticleConfig, Particles, SlotLook, WordGlyph,
};
use vo_render::{Renderer, Target};
use vo_scene::display::{DisplayList, GlyphInstance, Item, ItemKind, Layer};
use vo_scene::element::TextStyle;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Affine, Color, Point, Size};
use vo_scene::text::{ShapedLayout, TextLayout};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

struct Rig {
    gpu: support::Gpu,
    renderer: Renderer,
    target: Target,
    readback: wgpu::Buffer,
    w: u32,
    h: u32,
}

impl Rig {
    fn new(fonts: &FontRegistry, w: u32, h: u32) -> Rig {
        let gpu = support::gpu();
        let renderer = Renderer::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
        let target = Target::new(&gpu.device, w, h, FORMAT);
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (w * 4).next_multiple_of(256) as u64 * h as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Rig { gpu, renderer, target, readback, w, h }
    }

    fn particles(&self, fonts: &FontRegistry, cfg: ParticleConfig) -> Particles {
        Particles::new(&self.gpu.device, &self.gpu.queue, fonts.clone(), cfg)
    }

    fn params(&self, t: f64) -> FrameParams {
        FrameParams {
            t,
            to_px: Affine::IDENTITY,
            px_per_unit: 1.0,
            width: self.w,
            height: self.h,
            format: FORMAT,
            stretch: 1.0 / 60.0,
        }
    }

    /// Draws `list`, then the particles over it, and reads the frame back.
    fn frame(&mut self, list: &DisplayList, parts: &Particles) -> Vec<u8> {
        let d = &self.gpu.device;
        let mut enc = d.create_command_encoder(&Default::default());
        self.renderer.encode(list, 1.0, &self.target, &mut enc);
        let view = self.target.texture().create_view(&Default::default());
        parts.draw_over(&mut enc, &view);
        let row = (self.w * 4).next_multiple_of(256);
        enc.copy_texture_to_buffer(
            self.target.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
            },
            wgpu::Extent3d { width: self.w, height: self.h, depth_or_array_layers: 1 },
        );
        self.gpu.queue.submit([enc.finish()]);
        self.readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        d.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let mut out = Vec::with_capacity((self.w * self.h * 4) as usize);
        {
            let data = self.readback.slice(..).get_mapped_range().expect("mapped");
            for y in 0..self.h {
                let s = (y * row) as usize;
                out.extend_from_slice(&data[s..s + (self.w * 4) as usize]);
            }
        }
        self.readback.unmap();
        out
    }
}

fn empty_list(w: u32, h: u32) -> DisplayList {
    DisplayList { size: Size::new(w as f32, h as f32), scale: 1.0, layers: vec![Layer::root()], ..Default::default() }
}

/// A scripted voice level: syllable bursts.
fn level(t: f64) -> f32 {
    let env = (0.5 + 0.5 * (t * 2.1).sin()) as f32;
    let syl = (0.5 + 0.5 * (t * 13.0).sin()).powi(3) as f32;
    (0.15 + 0.85 * env * syl).clamp(0.0, 1.0)
}

fn ring_sim(_: u32, t: f64) -> EmitterSim {
    let l = level(t);
    EmitterSim {
        radius: 70.0 + 40.0 * l,
        swirl: 1.2 + 4.0 * l,
        level: l,
        noise: 900.0 + 3000.0 * l,
        life: 1.2,
        spread: 0.25,
        scale: 0.02,
    }
}

fn ring_look(t: f64) -> EmitterLook {
    EmitterLook {
        origin: Point::new(160.0, 120.0),
        size: 2.2,
        brightness: 0.35 + 0.65 * level(t),
        ramp: [Color::hex(0xa8e6ff, 0.9), Color::hex(0x5b8cff, 0.7), Color::hex(0xb04cff, 0.0)],
    }
}

fn ring_frame(rig: &mut Rig, parts: &mut Particles, t: f64) -> Vec<u8> {
    parts.advance(t, &ring_sim);
    parts.prepare(&rig.params(t), &|_| ring_look(t), &[]);
    let list = empty_list(rig.w, rig.h);
    rig.frame(&list, parts)
}

fn ring_particles(rig: &Rig, fonts: &FontRegistry) -> Particles {
    let mut p = rig.particles(fonts, ParticleConfig { free_capacity: 4096, word_slots: 1, ..Default::default() });
    p.add_ring(4096, 7, 0.8);
    p
}

/// Steps through frames at `fps` up to `t` and returns the last frame.
fn ring_at_rate(rig: &mut Rig, parts: &mut Particles, fps: f64, t: f64) -> Vec<u8> {
    let n = (t * fps).round() as u64;
    for i in 0..n {
        let ft = i as f64 / fps;
        parts.advance(ft, &ring_sim);
    }
    ring_frame(rig, parts, t)
}

#[test]
fn ring_is_the_same_at_any_frame_rate_and_after_a_seek() {
    let fonts = FontRegistry::new();
    let mut rig = Rig::new(&fonts, 320, 240);
    let t = 2.0;

    let mut a = ring_particles(&rig, &fonts);
    let at_60 = ring_at_rate(&mut rig, &mut a, 60.0, t);
    let mut b = ring_particles(&rig, &fonts);
    let at_144 = ring_at_rate(&mut rig, &mut b, 144.0, t);
    let mut c = ring_particles(&rig, &fonts);
    let one_jump = ring_frame(&mut rig, &mut c, t);
    assert!(at_60.chunks(4).filter(|p| p[3] > 0).count() > 2000, "the ring draws something");
    assert!(at_60 == at_144, "60 fps and 144 Hz frame pacing give different frames");
    assert!(at_60 == one_jump, "one big advance differs from many small ones");

    // Seek back past the start: replay from tick 0.
    ring_frame(&mut rig, &mut c, 3.1);
    assert!(ring_frame(&mut rig, &mut c, t) == at_60, "seek by replay");
    // Seek back to a snapshot.
    let mut d = ring_particles(&rig, &fonts);
    ring_frame(&mut rig, &mut d, 1.0);
    d.snapshot();
    ring_frame(&mut rig, &mut d, 3.1);
    assert!(ring_frame(&mut rig, &mut d, t) == at_60, "seek by snapshot");

    let img = Image { name: "particles-ring".into(), w: rig.w, h: rig.h, channels: 4, px: at_60 };
    support::check(&img, Tolerance { per_pixel: 4, max_fraction: 0.002 }).unwrap();
}

struct Word {
    glyphs: Vec<WordGlyph>,
    origin: Point,
}

fn shape(fonts: &FontRegistry, text: &str, size: f32, at: Point) -> Word {
    let layout = ShapedLayout::new(fonts.clone());
    let shaped = layout.layout(text, &TextStyle::new(0, size, size * 1.3), None);
    let first = shaped.glyphs[0].pos;
    let origin = Point::new(at.x + first.x, at.y + first.y);
    let glyphs = shaped
        .glyphs
        .iter()
        .map(|g| WordGlyph { font: 0, glyph: g.glyph, size, pos: Point::new(g.pos.x - first.x, g.pos.y - first.y) })
        .collect();
    Word { glyphs, origin }
}

/// The solid text of `word`, each glyph at its handover opacity at `t`.
fn word_list(rig: &Rig, parts: &Particles, word: &Word, t: f64, solid_only: bool) -> DisplayList {
    let mut list = empty_list(rig.w, rig.h);
    for (i, g) in word.glyphs.iter().enumerate() {
        let a = if solid_only { 1.0 } else { parts.glyph_alpha(0, i, t) };
        list.glyphs.push(GlyphInstance {
            pos: Point::new(word.origin.x + g.pos.x, word.origin.y + g.pos.y),
            size: g.size,
            font: 0,
            glyph: g.glyph,
            word: 0,
            color: Color::hex(0xf2f4ff, a),
        });
    }
    list.items.push(Item { kind: ItemKind::Glyphs, start: 0, len: list.glyphs.len() as u32 });
    list.layers[0].items = 0..1;
    list
}

fn word_frame(rig: &mut Rig, parts: &mut Particles, word: &Word, t: f64) -> Vec<u8> {
    let look = SlotLook { origin: word.origin, color: Color::hex(0xcfe0ff, 0.9), visible: true };
    parts.prepare(&rig.params(t), &|_| ring_look(t), &[look]);
    let list = word_list(rig, parts, word, t, false);
    rig.frame(&list, parts)
}

/// Light of a premultiplied sRGB pixel over black, linear.
fn light(p: &[u8]) -> f32 {
    let lin = |c: u8| vo_render::color::srgb_to_linear(c as f32 / 255.0);
    (lin(p[0]) + lin(p[1]) + lin(p[2])) / 3.0
}

/// Pearson correlation of two images' light.
fn correlation(a: &[u8], b: &[u8]) -> f32 {
    let (la, lb): (Vec<f32>, Vec<f32>) = a.chunks(4).zip(b.chunks(4)).map(|(x, y)| (light(x), light(y))).unzip();
    let n = la.len() as f32;
    let (ma, mb) = (la.iter().sum::<f32>() / n, lb.iter().sum::<f32>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in la.iter().zip(&lb) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    sab / (saa * sbb).sqrt().max(1e-9)
}

/// Frames stacked top to bottom.
fn strip(frames: &[Vec<u8>]) -> Vec<u8> {
    frames.concat()
}

#[test]
fn a_word_forms_from_particles_and_hands_over_without_a_pop() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let (w, h) = (320, 80);
    let mut rig = Rig::new(&fonts, w, h);
    let mut parts = rig.particles(&fonts, ParticleConfig { free_capacity: 64, word_slots: 1, ..Default::default() });
    let word = shape(&fonts, "particles", 28.0, Point::new(24.0, 22.0));
    let solid = {
        let list = word_list(&rig, &parts, &word, 0.0, true);
        let empty = rig.particles(&fonts, ParticleConfig { free_capacity: 64, word_slots: 1, ..Default::default() });
        rig.frame(&list, &empty)
    };
    let t0 = 0.1;
    parts.set_word(0, t0, &word.glyphs, 1.0);
    assert!(parts.glyph_states(0).iter().all(|s| *s == GlyphState::Fresh));

    let total_solid: f32 = solid.chunks(4).map(light).sum();
    let mut readable_at = None;
    let mut lum = Vec::new();
    let mut shots = Vec::new();
    println!("t_ms  corr  light/solid  glyph0  glyph8");
    for i in 0..=30 {
        let dt = i as f64 * 0.01;
        let px = word_frame(&mut rig, &mut parts, &word, t0 + dt);
        let c = correlation(&px, &solid);
        let l = px.chunks(4).map(light).sum::<f32>() / total_solid;
        lum.push(l);
        println!(
            "{:4.0}  {c:.3}  {l:.3}  {:.2}  {:.2}",
            dt * 1000.0,
            parts.glyph_alpha(0, 0, t0 + dt),
            parts.glyph_alpha(0, 8, t0 + dt)
        );
        if readable_at.is_none() && c > 0.9 {
            readable_at = Some(dt);
        }
        if [2, 5, 8, 12, 20].contains(&i) {
            shots.push(px);
        }
    }
    let readable_at = readable_at.expect("never readable");
    println!("readable (correlation > 0.9 with the solid word) at {:.0} ms", readable_at * 1000.0);
    assert!(readable_at <= 0.2, "{readable_at}");
    // No pop: frame-to-frame change of total light stays small once the word has formed.
    let jumps: Vec<f32> = lum.windows(2).skip(8).map(|w| (w[1] - w[0]).abs()).collect();
    let worst = jumps.iter().cloned().fold(0.0, f32::max);
    println!("largest 10 ms light step after 80 ms: {worst:.3} of the solid word");
    assert!(worst < 0.25, "{worst}");
    assert!((lum.last().unwrap() - 1.0).abs() < 0.02, "settles on the solid word");
    let _ = parts.settled_at(0);

    // A revision: "particles" to "partials" scatters only the changed glyphs.
    let rev = shape(&fonts, "partials", 28.0, Point::new(24.0, 22.0));
    let t1 = 0.6;
    parts.set_word(0, t1, &rev.glyphs, 1.0);
    let states = parts.glyph_states(0);
    println!("revision states: {states:?}");
    assert_eq!(states[..4], [GlyphState::Kept; 4]);
    assert_eq!(*states.last().unwrap(), GlyphState::Kept);
    assert!(states.contains(&GlyphState::Changed));
    for dt in [0.02, 0.06, 0.1, 0.25] {
        shots.push(word_frame(&mut rig, &mut parts, &rev, t1 + dt));
    }
    let px = strip(&shots);
    let img = Image { name: "particles-word".into(), w, h: h * shots.len() as u32, channels: 4, px };
    support::check(&img, Tolerance { per_pixel: 4, max_fraction: 0.002 }).unwrap();
}

#[test]
fn word_frames_do_not_depend_on_when_frames_happen() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let mut rig = Rig::new(&fonts, 320, 80);
    let a_word = shape(&fonts, "forming", 24.0, Point::new(20.0, 20.0));
    let b_word = shape(&fonts, "formed", 24.0, Point::new(20.0, 20.0));
    let cfg = ParticleConfig { free_capacity: 64, word_slots: 1, ..Default::default() };
    let render = |rig: &mut Rig, frame_times: &[f64]| {
        let mut p = rig.particles(&fonts, cfg);
        let mut events = vec![(0.05, &a_word), (0.13, &b_word)].into_iter().peekable();
        let mut last = Vec::new();
        for &t in frame_times {
            while let Some((te, w)) = events.next_if(|(te, _)| *te <= t) {
                p.set_word(0, te, &w.glyphs, 1.0);
            }
            last = word_frame(rig, &mut p, &b_word, t);
        }
        last
    };
    let at_60: Vec<f64> = (0..=12).map(|i| i as f64 / 60.0).chain([0.21]).collect();
    let at_144: Vec<f64> = (0..=30).map(|i| i as f64 / 144.0).chain([0.21]).collect();
    let a = render(&mut rig, &at_60);
    let b = render(&mut rig, &at_144);
    assert!(a.chunks(4).filter(|p| p[3] > 0).count() > 100);
    assert!(a == b, "word frames depend on frame pacing");
}
