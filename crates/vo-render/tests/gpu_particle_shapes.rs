//! Particles that form shapes and keep inside text (EG-26): emitter elements whose sources give
//! shape targets and masks with their times, drawn by the renderer.
//!
//! GPU test: `just test-gpu particle_shapes`.

mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_render::{Renderer, Target as RenderTarget};
use vo_scene::element::{Element, TextStyle};
use vo_scene::emitter::{
    text_mask, text_path, Emitter, EmitterLook, EmitterSim, EmitterSource, MaskField, Shape, ShapeTarget, SpriteStyle, Target,
    TimedMask,
};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Point, Size};
use vo_scene::text::{ShapedLayout, TextLayout};
use vo_scene::Scene;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const W: u32 = 600;
const H: u32 = 300;
/// The check mark's square, in units, round the emitter's centre.
const CHECK: f32 = 160.0;

#[derive(Debug)]
struct Source {
    sim: EmitterSim,
    look: EmitterLook,
    targets: Option<Arc<[ShapeTarget]>>,
    masks: Option<Arc<[TimedMask]>>,
}

impl EmitterSource for Source {
    fn sim(&self, _: f64) -> EmitterSim {
        self.sim
    }

    fn look(&self, _: f64) -> EmitterLook {
        self.look
    }

    fn targets(&self) -> Option<Arc<[ShapeTarget]>> {
        self.targets.clone()
    }

    fn masks(&self) -> Option<Arc<[TimedMask]>> {
        self.masks.clone()
    }
}

fn dots() -> EmitterLook {
    EmitterLook {
        size: 1.0,
        style: SpriteStyle::Dot { pitch: 0.0 },
        ramp: [Color::WHITE, Color::WHITE, Color::hex(0xffffff, 0.0)],
        ..Default::default()
    }
}

/// No free particles: only the shape's.
fn shape_only(targets: Vec<ShapeTarget>) -> Source {
    Source {
        sim: EmitterSim { emit: 0.0, ..Default::default() },
        look: dots(),
        targets: Some(targets.into()),
        masks: None,
    }
}

fn icon() -> Target {
    // A Lucide-style bell: strokes and a filled clapper.
    let svg = r#"<svg viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2">
        <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"/>
        <circle cx="12" cy="20" r="1.5" fill="black"/></svg>"#;
    Target::Svg(Arc::new(vo_scene::svg::parse(svg).unwrap()))
}

/// Renders `source` as a `W` x `H` emitter element at `scale`, every frame at `fps` up to the
/// last of `keep`, and returns the frames at `keep`.
#[allow(clippy::too_many_arguments)]
fn render(
    gpu: &support::Gpu,
    fonts: &FontRegistry,
    source: Arc<dyn EmitterSource>,
    capacity: u32,
    scale: f32,
    reduced: bool,
    fps: f64,
    keep: &[f64],
) -> Vec<Vec<u8>> {
    let mut renderer = Renderer::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let (w, h) = ((W as f32 * scale) as u32, (H as f32 * scale) as u32);
    let target = RenderTarget::new(&gpu.device, w, h, FORMAT);
    let mut scene: Scene<()> =
        Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone()))).with_reduced_motion(reduced);
    scene.set_viewport(0.0, Size::new(W as f32, H as f32), scale);
    let view = Element::column("root").child(Element::emitter("e", Emitter::new(source, capacity, 5)).size(W as f32, H as f32));
    scene.apply(view, 0.0);
    let last = keep.iter().copied().fold(0.0, f64::max);
    let mut kept = Vec::new();
    for i in 0..=(last * fps).round() as u64 {
        let t = i as f64 / fps;
        let list = scene.sample(t);
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        renderer.encode(&list, scale, &target, &mut enc);
        if keep.iter().any(|k| (k * fps).round() as u64 == i) {
            kept.push(read(gpu, &target, w, h, enc));
        } else {
            gpu.queue.submit([enc.finish()]);
        }
    }
    kept
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

/// Share of the light (RGB sums) in pixels where `inside(x, y)` holds, pixel centres in
/// emitter space at scale 1, and the total light.
fn light_share(px: &[u8], inside: impl Fn(Point) -> bool) -> (f64, f64) {
    let (mut all, mut ins) = (0.0, 0.0);
    for (i, c) in px.chunks(4).enumerate() {
        let l = (c[0] as u32 + c[1] as u32 + c[2] as u32) as f64;
        if l == 0.0 {
            continue;
        }
        let p = Point::new((i as u32 % W) as f32 + 0.5 - W as f32 / 2.0, (i as u32 / W) as f32 + 0.5 - H as f32 / 2.0);
        all += l;
        if inside(p) {
            ins += l;
        }
    }
    (if all > 0.0 { ins / all } else { 0.0 }, all)
}

fn seg_dist(p: Point, a: Point, b: Point) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let k = (((p.x - a.x) * dx + (p.y - a.y) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (p.x - a.x - k * dx).hypot(p.y - a.y - k * dy)
}

/// Whether `p` is inside the check mark stroked 4 px wide.
fn in_check(p: Point) -> bool {
    let m = |x: f32, y: f32| Point::new((x - 12.0) * CHECK / 24.0, (y - 12.0) * CHECK / 24.0);
    let (a, b, c) = (m(4.5, 12.5), m(9.5, 17.5), m(19.5, 6.5));
    seg_dist(p, a, b).min(seg_dist(p, b, c)) <= 2.0
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

#[test]
fn particles_form_a_check_mark() {
    let gpu = support::gpu();
    let fonts = fonts();
    let src = Arc::new(shape_only(vec![ShapeTarget::new(0.2, Target::CheckMark, CHECK)]));
    let frames = render(&gpu, &fonts, src, 600, 1.0, false, 60.0, &[0.25, 1.7]);
    let (early, _) = light_share(&frames[0], in_check);
    let (share, light) = light_share(&frames[1], in_check);
    println!("check mark: {:.1} % of the light inside a 4 px stroke at 1.5 s, {:.1} % just after it applies", share * 100.0, early * 100.0);
    assert!(light > 0.0, "the shape draws");
    assert!(share >= 0.85, "{:.1} % inside", share * 100.0);
    assert!(early < 0.6, "the particles fly in rather than appear: {:.1} %", early * 100.0);
}

#[test]
fn reduced_motion_shows_the_shape_already_formed() {
    let gpu = support::gpu();
    let fonts = fonts();
    let src = Arc::new(shape_only(vec![ShapeTarget::new(0.2, Target::CheckMark, CHECK)]));
    let frames = render(&gpu, &fonts, src, 600, 1.0, true, 60.0, &[0.25, 1.7]);
    let (early, l0) = light_share(&frames[0], in_check);
    let (late, l1) = light_share(&frames[1], in_check);
    println!("reduced motion: {:.1} % inside at once, {:.1} % later; light {l0} then {l1}", early * 100.0, late * 100.0);
    assert!(early >= 0.85 && late >= 0.85);
    assert!(frames[0] == frames[1], "the shape does not move after it appears");
}

/// "vo" at 140 units, centred on the emitter, and its mask with a halo of `blur` cells.
fn text(fonts: &FontRegistry, cell: f32, margin: f32, blur: u32) -> MaskField {
    let shaped = ShapedLayout::new(fonts.clone()).layout("vo", &TextStyle::new(0, 140.0, 150.0), None);
    // The ink's centre on the emitter's.
    let ink = vo_scene::path::kurbo::Shape::bounding_box(&text_path(fonts, &shaped, 140.0, Point::ZERO));
    let origin = Point::new(-ink.center().x as f32, -ink.center().y as f32);
    text_mask(fonts, &shaped, 140.0, origin, cell, margin, blur)
}

fn masked(fonts: &FontRegistry, strength: f32) -> Source {
    // A halo about 48 units wide, so the force reaches the whole box the particles start in.
    let field = text(fonts, 3.0, 40.0, 8);
    let r = field.rect;
    // The particles start in the ink's box and 30 units round it.
    let pad = 30.0;
    Source {
        sim: EmitterSim {
            shape: Shape::Box { half: [r[2] / 2.0 - 40.0 + pad, r[3] / 2.0 - 40.0 + pad, 0.0] },
            life: 20.0,
            life_jitter: 0.0,
            drag: 4.0,
            noise: 8.0,
            noise_scale: 0.03,
            mask: strength,
            ..Default::default()
        },
        look: dots(),
        targets: None,
        masks: Some(Arc::from([TimedMask { at: 0.0, mask: Arc::new(field) }])),
    }
}

#[test]
fn a_text_mask_keeps_particles_inside_the_text() {
    let gpu = support::gpu();
    let fonts = fonts();
    // Coverage to measure against: the ink and a 2 to 4 unit halo.
    let cover = text(&fonts, 1.0, 10.0, 1);
    let inside = |p: Point| cover.at(p) > 0.0;
    let share = |strength: f32| {
        let f = render(&gpu, &fonts, Arc::new(masked(&fonts, strength)), 2000, 1.0, false, 60.0, &[2.0]);
        light_share(&f[0], inside).0
    };
    let with = share(-4000.0);
    let without = share(0.0);
    println!("text mask: {:.1} % of the light inside the text with it, {:.1} % without", with * 100.0, without * 100.0);
    assert!(with >= 0.80, "with the mask: {:.1} %", with * 100.0);
    assert!(without <= 0.30, "without the mask: {:.1} %", without * 100.0);
}

#[test]
fn a_seek_back_replays_shapes_and_masks() {
    let gpu = support::gpu();
    let fonts = fonts();
    let mut src = masked(&fonts, -3000.0);
    src.targets = Some(Arc::from([
        ShapeTarget::new(0.3, Target::CheckMark, CHECK),
        ShapeTarget { scatter: true, ..ShapeTarget::new(0.8, icon(), CHECK) },
    ]));
    let mut masks = src.masks.as_deref().unwrap().to_vec();
    masks.push(TimedMask { at: 0.65, mask: Arc::new(text(&fonts, 4.0, 40.0, 3)) });
    src.masks = Some(masks.into());
    let src: Arc<dyn EmitterSource> = Arc::new(src);
    // A live run to 1.4 s, then a frame at 0.9 s; and a fresh run to 0.9 s.
    let seek = {
        let mut renderer = Renderer::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
        let target = RenderTarget::new(&gpu.device, W, H, FORMAT);
        let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
        scene.set_viewport(0.0, Size::new(W as f32, H as f32), 1.0);
        let e = Element::emitter("e", Emitter::new(src.clone(), 2000, 5)).size(W as f32, H as f32);
        scene.apply(Element::column("root").child(e), 0.0);
        let times: Vec<f64> = (0..=84).map(|i| i as f64 / 60.0).chain([0.9]).collect();
        let mut out = Vec::new();
        for (i, &t) in times.iter().enumerate() {
            let list = scene.sample(t);
            let mut enc = gpu.device.create_command_encoder(&Default::default());
            renderer.encode(&list, 1.0, &target, &mut enc);
            if i == times.len() - 1 {
                out = read(&gpu, &target, W, H, enc);
            } else {
                gpu.queue.submit([enc.finish()]);
            }
        }
        out
    };
    let fresh = render(&gpu, &fonts, src, 2000, 1.0, false, 60.0, &[0.9]).pop().unwrap();
    let differ = seek.iter().zip(&fresh).filter(|(a, b)| a != b).count();
    let lit = fresh.chunks(4).filter(|c| c[3] > 0).count();
    println!("seek: {differ} bytes differ, {lit} lit pixels");
    assert!(lit > 500, "something draws");
    assert_eq!(differ, 0, "a seek back gives a different frame");
}

#[test]
fn an_icon_formed_at_scale_1_5_matches_its_golden() {
    let gpu = support::gpu();
    let fonts = fonts();
    let src = Arc::new(Source {
        look: EmitterLook {
            size: 1.2,
            style: SpriteStyle::SPARK,
            ramp: [Color::hex(0xbfe4ff, 1.0), Color::hex(0x7cc4ff, 0.9), Color::hex(0x3a6bff, 0.0)],
            ..Default::default()
        },
        ..shape_only(vec![ShapeTarget::new(0.1, icon(), 200.0)])
    });
    let px = render(&gpu, &fonts, src, 2000, 1.5, false, 60.0, &[1.2]).pop().unwrap();
    let (w, h) = ((W as f32 * 1.5) as u32, (H as f32 * 1.5) as u32);
    let lit = px.chunks(4).filter(|c| c[3] > 8).count();
    assert!(lit > 2000, "the icon draws: {lit}");
    let img = Image { name: "particle-shape-icon-1.5x".into(), w, h, channels: 4, px };
    support::check(&img, Tolerance { per_pixel: 4, max_fraction: 0.002 }).unwrap();
}
