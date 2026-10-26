//! GPU particles: determinism across frame rates and seeks, the ring emitter, and particle
//! words forming and handing over to solid text.
//!
//! GPU test: `just test-gpu particles`. Prints the formation timings it measures.

mod support;

use support::{Image, Tolerance};
use vo_render::particles::{
    tilt_x, Camera, EmitterLook, EmitterSim, EmitterSpec, FrameParams, GlyphState, MaskField, ParticleConfig,
    Particles, PointForm, Shape, Shock, SlotLook, SpriteStyle, WordGlyph,
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
            camera: None,
            blend: Default::default(),
            depth: None,
            samples: 1,
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
    let r = 70.0 + 40.0 * l;
    EmitterSim {
        swirl: 1.2 + 4.0 * l,
        level: l,
        noise: 900.0 + 3000.0 * l,
        noise_scale: 0.02,
        life: 1.2,
        ..EmitterSim::ring(r, 0.25)
    }
}

fn ring_look(t: f64) -> EmitterLook {
    EmitterLook {
        origin: Point::new(160.0, 120.0),
        size: 2.2,
        brightness: 0.35 + 0.65 * level(t),
        ramp: [Color::hex(0xa8e6ff, 0.9), Color::hex(0x5b8cff, 0.7), Color::hex(0xb04cff, 0.0)],
        ..Default::default()
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
    let look = SlotLook::new(word.origin, Color::hex(0xcfe0ff, 0.9));
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

/// Every free-particle feature in one scene: a ring with ribbons, a burst and a shockwave;
/// a tilted 3D torus with depth of field; a disc of particles hugging the edges of a word's
/// mask; and dots that assemble into a check mark.
struct Features {
    word_points: Vec<[f32; 2]>,
}

const TEXT_AT: Point = Point { x: 420.0, y: 150.0 };
const DISC_AT: Point = Point { x: 470.0, y: 140.0 };

fn feature_sim(e: u32, t: f64) -> EmitterSim {
    match e {
        0 => {
            let burst = (t >= 1.0) as u32;
            let shock =
                (t >= 1.2).then_some(Shock { age: (t - 1.2) as f32, speed: 160.0, width: 18.0, strength: 2500.0 });
            EmitterSim {
                swirl: 2.5,
                noise: 400.0,
                life: 1.4,
                spawn_speed: if (1.0..1.05).contains(&t) { 220.0 } else { 0.0 },
                epoch: burst,
                shock,
                ..EmitterSim::ring(55.0, 0.15)
            }
        }
        1 => EmitterSim { swirl: 1.6, noise: 120.0, noise_scale: 0.03, life: 2.0, ..EmitterSim::torus(70.0, 10.0) },
        _ => EmitterSim {
            shape: Shape::Disc { radius: 90.0 },
            noise: 60.0,
            noise_scale: 0.04,
            life: 2.5,
            drag: 3.0,
            mask: -4000.0,
            ..Default::default()
        },
    }
}

fn feature_look(e: u32) -> EmitterLook {
    match e {
        0 => EmitterLook {
            origin: Point::new(110.0, 130.0),
            size: 1.8,
            brightness: 0.5,
            ramp: [Color::hex(0xffe9a8, 1.0), Color::hex(0xff8a3d, 0.8), Color::hex(0xff3d6e, 0.0)],
            trail_width: 0.7,
            trail_alpha: 0.8,
            ..Default::default()
        },
        1 => EmitterLook {
            origin: Point::new(270.0, 130.0),
            rotation: tilt_x(1.1),
            size: 2.0,
            brightness: 0.45,
            ramp: [Color::hex(0xa8e6ff, 1.0), Color::hex(0x6a8cff, 0.8), Color::hex(0x9b5cff, 0.0)],
            style: SpriteStyle::Bokeh { focus: 1.0, aperture: 40.0 },
            ..Default::default()
        },
        _ => EmitterLook {
            origin: DISC_AT,
            size: 1.2,
            brightness: 0.6,
            ramp: [Color::hex(0xb8ffcf, 1.0), Color::hex(0x3ddc84, 0.8), Color::hex(0x1b8f5a, 0.0)],
            ..Default::default()
        },
    }
}

impl Features {
    fn new(rig: &Rig, fonts: &FontRegistry) -> (Features, Particles) {
        let cfg = ParticleConfig { free_capacity: 12_288, word_slots: 2, slot_capacity: 512, ..Default::default() };
        let mut p = rig.particles(fonts, cfg);
        p.add_emitter(EmitterSpec { trails: 96, warmup: 0.6, ..EmitterSpec::new(2048, 3) });
        p.add_emitter(EmitterSpec { three_d: true, warmup: 0.6, ..EmitterSpec::new(4096, 5) });
        p.add_emitter(EmitterSpec { warmup: 0.6, ..EmitterSpec::new(6144, 7) });
        // The disc hugs the outline of a word laid out (but not drawn) at TEXT_AT.
        let word = shape(fonts, "edges", 40.0, TEXT_AT);
        p.set_word(1, 0.0, &word.glyphs, 1.0);
        let off = [word.origin.x - DISC_AT.x, word.origin.y - DISC_AT.y];
        let pts: Vec<[f32; 2]> = p.slot_points(1).iter().map(|q| [q[0] + off[0], q[1] + off[1]]).collect();
        p.set_word(1, 0.0, &[], 1.0);
        let mask = MaskField::from_points(&pts, [-100.0, -80.0, 200.0, 140.0], 2.0, 2);
        p.set_mask(0.0, &mask);
        (Features { word_points: pts }, p)
    }

    fn check_mark() -> Vec<Point> {
        let mut pts = Vec::new();
        let seg = |a: Point, b: Point, n: usize, pts: &mut Vec<Point>| {
            for i in 0..n {
                let k = i as f32 / (n - 1) as f32;
                pts.push(Point::new(a.x + (b.x - a.x) * k, a.y + (b.y - a.y) * k));
            }
        };
        seg(Point::new(0.0, 20.0), Point::new(16.0, 36.0), 7, &mut pts);
        seg(Point::new(20.0, 32.0), Point::new(52.0, 0.0), 12, &mut pts);
        pts
    }

    fn frame(&self, rig: &mut Rig, p: &mut Particles, events: &mut usize, t: f64) -> Vec<u8> {
        if *events == 0 && t >= 0.5 {
            let pts = Features::check_mark();
            let starts: Vec<Point> =
                (0..pts.len()).map(|i| Point::new((i % 6) as f32 * 9.0, 70.0 + (i / 6) as f32 * 9.0)).collect();
            p.set_points(
                0,
                0.5,
                &PointForm {
                    points: &pts,
                    starts: Some(&starts),
                    size: 30.0,
                    persist: true,
                    sweep: 0.08,
                    scatter: false,
                },
            );
            *events = 1;
        }
        p.advance(t, &feature_sim);
        let mut params = rig.params(t);
        params.camera = Some(Camera::overlay(rig.w as f32, rig.h as f32, 400.0, 1.0));
        let dots = SlotLook {
            size: 2.4,
            style: SpriteStyle::Dot { pitch: 0.0 },
            ..SlotLook::new(Point::new(560.0, 100.0), Color::hex(0xffffff, 0.95))
        };
        p.prepare(&params, &feature_look, &[dots]);
        let _ = &self.word_points;
        rig.frame(&empty_list(rig.w, rig.h), p)
    }
}

#[test]
fn every_feature_is_the_same_at_any_frame_rate() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let mut rig = Rig::new(&fonts, 640, 240);
    let run = |rig: &mut Rig, fps: f64, end: f64, keep: &[f64]| {
        let (f, mut p) = Features::new(rig, &fonts);
        let mut events = 0;
        let mut kept = Vec::new();
        let n = (end * fps).round() as u64;
        for i in 0..=n {
            let t = i as f64 / fps;
            let px = f.frame(rig, &mut p, &mut events, t);
            if keep.iter().any(|k| (k * fps).round() as u64 == i) {
                kept.push(px);
            }
        }
        kept
    };
    let times = [0.25, 0.75, 1.0 + 1.0 / 12.0, 1.5, 2.0];
    let a = run(&mut rig, 60.0, 2.0, &times);
    let b = run(&mut rig, 144.0, 2.0, &times);
    assert_eq!(a.len(), times.len());
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        assert!(x == y, "frame at {} s differs between 60 fps and 144 Hz", times[i]);
    }
    let img = Image {
        name: "particles-features".into(),
        w: rig.w,
        h: rig.h * times.len() as u32,
        channels: 4,
        px: a.concat(),
    };
    support::check(&img, Tolerance { per_pixel: 4, max_fraction: 0.002 }).unwrap();
}

/// Particle words fed from a vo-scene `LiveText` element through `ParticleText`, with a
/// revision: frames are the same at 60 fps and 144 Hz, words are readable in time.
#[test]
fn live_text_forms_through_particle_text() {
    use vo_core::words::WordModel;
    use vo_render::particle_text::ParticleText;
    use vo_scene::element::Element;
    use vo_scene::Scene;

    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let mut rig = Rig::new(&fonts, 420, 80);
    // Hypotheses at their times, as a recogniser would send them.
    let script = [(0.1, "hello"), (0.45, "hello there"), (0.8, "hello their"), (1.15, "hello their friend")];
    let render = |rig: &mut Rig, fps: f64, keep: &[f64]| -> Vec<Vec<u8>> {
        let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
        scene.set_viewport(0.0, Size::new(rig.w as f32, rig.h as f32), 1.0);
        let mut model = WordModel::new();
        let mut p = rig.particles(&fonts, ParticleConfig { free_capacity: 64, word_slots: 4, ..Default::default() });
        let mut text = ParticleText::new(4, 1.0, Color::hex(0x9fd8ff, 1.0));
        let mut next = 0;
        let mut kept = Vec::new();
        let n = (1.6 * fps).round() as u64;
        for i in 0..=n {
            let t = i as f64 / fps;
            while next < script.len() && script[next].0 <= t {
                let (te, hyp) = script[next];
                model.update(hyp);
                let view = Element::column("root")
                    .pad(20.0)
                    .child(Element::live_text("live", model.words()).text_style(TextStyle::new(0, 30.0, 40.0)));
                scene.apply(view, te);
                text.update(&mut p, te, &scene.sample(te).glyphs);
                next += 1;
            }
            p.advance(t, &|_, _| EmitterSim::default());
            let mut list = scene.sample(t);
            let looks = text.apply(&p, t, &mut list.glyphs);
            p.prepare(&rig.params(t), &|_| EmitterLook::default(), &looks);
            let px = rig.frame(&list, &p);
            if keep.iter().any(|k| (k * fps).round() as u64 == i) {
                kept.push(px);
            }
        }
        kept
    };
    // On both the 60 fps and the 144 Hz grid.
    let times = [0.5, 7.0 / 12.0, 10.0 / 12.0, 1.0, 1.25, 1.5];
    let a = render(&mut rig, 60.0, &times);
    let b = render(&mut rig, 144.0, &times);
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        assert!(x == y, "frame at {} s differs between 60 fps and 144 Hz", times[i]);
    }
    let img = Image {
        name: "particles-live-text".into(),
        w: rig.w,
        h: rig.h * times.len() as u32,
        channels: 4,
        px: a.concat(),
    };
    support::check(&img, Tolerance { per_pixel: 4, max_fraction: 0.002 }).unwrap();
}

/// After a revision the word is readable again within 250 ms, and only the changed glyphs
/// light up.
#[test]
fn a_revision_is_readable_again_within_250_ms() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let (w, h) = (320, 80);
    let mut rig = Rig::new(&fonts, w, h);
    let mut parts = rig.particles(&fonts, ParticleConfig { free_capacity: 64, word_slots: 1, ..Default::default() });
    let old = shape(&fonts, "wards", 28.0, Point::new(24.0, 22.0));
    let new = shape(&fonts, "words", 28.0, Point::new(24.0, 22.0));
    let solid = {
        let list = word_list(&rig, &parts, &new, 0.0, true);
        let empty = rig.particles(&fonts, ParticleConfig { free_capacity: 64, word_slots: 1, ..Default::default() });
        rig.frame(&list, &empty)
    };
    parts.set_word(0, 0.0, &old.glyphs, 1.0);
    let t1 = 0.5;
    parts.set_word(0, t1, &new.glyphs, 1.0);
    assert_eq!(
        parts.glyph_states(0),
        [GlyphState::Kept, GlyphState::Changed, GlyphState::Kept, GlyphState::Kept, GlyphState::Kept]
    );
    let mut readable = None;
    let mut lit_outside = 0.0f32;
    // Columns of the changed glyph, from its pen position to the next one.
    let (x0, x1) = (new.origin.x + new.glyphs[1].pos.x - 2.0, new.origin.x + new.glyphs[2].pos.x + 2.0);
    for i in 0..=40 {
        let t = t1 + i as f64 * 0.01;
        let px = word_frame(&mut rig, &mut parts, &new, t);
        let c = correlation(&px, &solid);
        if readable.is_none() && c > 0.9 {
            readable = Some(t - t1);
        }
        // Kept glyphs stay solid the whole time: only the changed one hands over again.
        for g in [0, 2, 3, 4] {
            assert_eq!(parts.glyph_alpha(0, g, t), 1.0, "glyph {g} at {t}");
        }
        // Once the scatter has faded, no light is left away from the changed glyph.
        let mut lit = 0.0f32;
        for (k, (p, s)) in px.chunks(4).zip(solid.chunks(4)).enumerate() {
            let x = (k as u32 % w) as f32;
            if x < x0 || x > x1 {
                lit = lit.max(light(p) - light(s));
            }
        }
        if t - t1 >= 0.2 {
            lit_outside = lit_outside.max(lit);
        }
        if i % 5 == 0 {
            println!("{:3.0} ms: correlation {c:.3}, light outside {lit:.3}", (t - t1) * 1000.0);
        }
    }
    let readable = readable.expect("never readable again");
    println!(
        "revision readable again after {:.0} ms; light outside the changed glyph after 200 ms {lit_outside:.3}",
        readable * 1000.0
    );
    assert!(readable <= 0.25, "{readable}");
    assert!(lit_outside < 0.35, "kept glyphs lit up: {lit_outside}");
}
