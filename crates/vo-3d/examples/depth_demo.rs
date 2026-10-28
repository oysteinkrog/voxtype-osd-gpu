//! The depth-ordered stack end to end: an 8 s scene with every part of the 3D layer.
//!
//! - Background gradient, then a lit hoop and an extruded word ("Listening") that turns in
//!   letter by letter on springs (D301, D306).
//! - The voice ring: a tilted torus of points that orbits and breathes with a voice level
//!   (D302), a ball of dust, and bokeh orbs at many depths (FX16).
//! - A 2D vector pill and a transcript whose words arrive over time. The ring reads the text
//!   distance field and moves away from the words, glowing where it passes them (TY19, D304).
//! - A Slug label lying in the ring's plane, sharp under perspective.
//!
//! Every part is a function of `t`: springs and scene changes are events at fixed times, and
//! the points are evaluated in closed form on the GPU.
//!
//!   cargo run --release -p vo-3d --example depth_demo -- [--out DIR]
//!   cargo run --release -p vo-3d --example depth_demo -- --live [--seconds S]
//!   cargo run --release -p vo-3d --example depth_demo -- --preview   (the scrub tool)
//!
//! Offline it writes `DIR/depth_demo.mp4` (1280x720, 60 fps) and `DIR/contact-sheet.png`
//! (default DIR `/var/tmp/claude/m4-3d`), then prints cold-start and per-stage GPU times and
//! checks that frames drawn again out of order hash the same. `--live` shows it as a Wayland
//! overlay; run that only inside a private nested compositor.

use std::collections::BTreeMap;
use std::f32::consts::FRAC_PI_2;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use vo::preview::Preview;
use vo_3d::*;
use vo_core::channel::{Channel, Rest};
use vo_core::hash::rand_f32;
use vo_core::spring::Spring;
use vo_render::Target;
use vo_scene::display::{DisplayList, ShapeKind};
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::taffy::{AlignItems, JustifyContent};
use vo_scene::text::ShapedLayout;
use vo_scene::{Prop, Scene};

const W: u32 = 1280;
const H: u32 = 720;
const FPS: f64 = 60.0;
const DURATION: f64 = 8.0;
const WORDS: [&str; 8] = ["Speak", "and", "the", "ring", "moves", "around", "your", "words."];
const SHEET_AT: [f64; 8] = [0.3, 0.7, 1.4, 2.6, 3.6, 4.6, 5.8, 7.6];

fn word_time(i: usize) -> f64 {
    2.0 + i as f64 * 0.42
}

fn smoothstep(a: f64, b: f64, x: f64) -> f64 {
    let k = ((x - a) / (b - a)).clamp(0.0, 1.0);
    k * k * (3.0 - 2.0 * k)
}

/// 0 to 1 with zero slope at both ends, clamped.
fn smooth01(x: f64) -> f32 {
    let x = x.clamp(0.0, 1.0) as f32;
    x * x * (3.0 - 2.0 * x)
}

/// A made-up voice level: silence, then syllables while the words arrive.
fn level(t: f64) -> f32 {
    let gate = smoothstep(1.7, 2.0, t) * (1.0 - smoothstep(5.6, 6.2, t));
    let syl = ((t * 9.1).sin() * (t * 3.3 + 0.7).sin()).abs().powf(1.3);
    (gate * (0.2 + 0.8 * syl)) as f32
}

/// Everything that is not a pure function of `t`: scenes and springs, changed by events.
struct Demo {
    text: Scene<()>,
    vector: Scene<()>,
    label: Scene<()>,
    word: TurningWord,
    pitch: Channel,
    hoop: MeshId,
    /// Words shown so far.
    shown: usize,
    /// Events applied up to this time.
    applied: f64,
}

impl Demo {
    fn new(fonts: &FontRegistry, r: &mut Renderer3d) -> Demo {
        let layout = || Box::new(ShapedLayout::new(fonts.clone()));
        let size = Size::new(W as f32, H as f32);
        let mut text = Scene::new().with_text(layout());
        text.set_viewport(0.0, size, 1.0);
        let mut vector = Scene::new();
        vector.set_viewport(0.0, size, 1.0);
        let mut label = Scene::new().with_text(layout());
        label.set_viewport(0.0, Size::new(420.0, 60.0), 1.0);
        label.apply(
            Element::panel("root")
                .size(420.0, 60.0)
                .align_items(AlignItems::CENTER)
                .justify(JustifyContent::CENTER)
                .child(
                    Element::label("l", "VOICE RING  /  3D")
                        .text_style(TextStyle::new(0, 34.0, 40.0).tracking(4.0))
                        .color(Color::hex(0xbfe4ff, 0.9)),
                ),
            0.0,
        );
        let face = fonts.get(0).expect("font");
        let glyphs = extrude_text(&face, "Listening", 0.42, 0.09);
        let mut word = TurningWord::new(r, &glyphs, 0.0, Spring::POP);
        word.turn(0.4, 1.0, 0.07, 0.6);
        word.turn(6.9, 0.0, 0.04, 0.3);
        let mut pitch = Channel::new(0.0, 0.32, Spring::critical_to_98(1.6)).with_rest(Rest::UNIT);
        pitch.retarget(0.2, 0.16);
        pitch.retarget(4.2, 0.30);
        let hoop = r.add_mesh(&Mesh::torus(1.25, 0.012, 160, 12));
        let mut d = Demo { text, vector, label, word, pitch, hoop, shown: 0, applied: -1.0 };
        d.apply_views(0.0);
        d
    }

    /// Applies every event up to `t`, in time order.
    fn advance(&mut self, t: f64) {
        while self.shown < WORDS.len() && word_time(self.shown) <= t {
            let at = word_time(self.shown);
            self.shown += 1;
            self.apply_views(at);
        }
        self.applied = self.applied.max(t);
    }

    fn apply_views(&mut self, t: f64) {
        let words = WORDS.iter().enumerate().map(|(i, w)| {
            let on = i < self.shown;
            Element::label(format!("w{i}"), *w)
                .text_style(TextStyle::new(0, 44.0, 54.0))
                .color(Color::WHITE)
                .motion(Prop::Opacity, Spring::SOFT)
                .motion(Prop::Offset, Spring::POP)
                .opacity(if on { 1.0 } else { 0.0 })
                .offset(0.0, if on { 0.0 } else { 18.0 })
        });
        let text = Element::panel("root")
            .size(W as f32, H as f32)
            .align_items(AlignItems::CENTER)
            .justify(JustifyContent::CENTER)
            .child(Element::row("line").gap(16.0).children(words));
        self.text.apply(text, t);
        let live = self.shown > 0;
        let pill = Element::row("pill")
            .size(if live { 132.0 } else { 44.0 }, 44.0)
            .radius(22.0)
            .fill(Color::hex(0x1d2433, 0.85))
            .motion(Prop::Size, Spring::POP)
            .pad(14.0)
            .child(Element::shape("dot", ShapeKind::Circle).size(16.0, 16.0).fill(Color::hex(0xff5a5a, 1.0)));
        let vector =
            Element::panel("root").size(W as f32, H as f32).align_items(AlignItems::CENTER).pad(40.0).child(pill);
        self.vector.apply(vector, t);
    }

    fn ring_model(t: f64) -> Mat4 {
        Mat4::rotate_x(0.62).then(&Mat4::rotate_z(0.08 * (t * 0.5).sin() as f32))
    }

    fn frame(&self, t: f64) -> Frame3d {
        let tf = t as f32;
        let lv = level(t);
        let ring = Demo::ring_model(t);
        let react = |c: PointCloud| PointCloud { reach_px: 40.0, push_px: 18.0, glow: 1.0, ..c };
        let clouds = vec![
            react(PointCloud {
                shape: CloudShape::Torus { major: 1.25, minor: 0.15 },
                model: ring,
                count: 16000,
                seed: 11,
                color: Color::hex(0x76c6ff, 0.8),
                color2: Color::hex(0xa98bff, 0.8),
                size: 0.016,
                min_px: 1.4,
                t: tf,
                speed: 0.3 + 0.4 * lv,
                level: lv,
                level_amp: 2.6,
                defocus: true,
                ..Default::default()
            }),
            PointCloud {
                shape: CloudShape::Ball { radius: 4.0 },
                model: Mat4::translation(Vec3::new(0.0, 0.0, -1.0)),
                count: 1400,
                seed: 23,
                color: Color::hex(0x8fa8c8, 0.45),
                color2: Color::hex(0xffd7a0, 0.45),
                size: 0.012,
                min_px: 1.0,
                t: tf,
                speed: 0.03,
                defocus: true,
                ..Default::default()
            },
        ];
        let orbs = (0..28)
            .map(|i| {
                let h = |k| rand_f32(5, i, k, 0);
                let y = -2.2 + (h(1) * 4.4 + tf * 0.06 * (0.5 + h(4))).rem_euclid(4.4);
                Orb {
                    pos: Vec3::new(-4.0 + 8.0 * h(0), y, -5.0 + 7.5 * h(2)),
                    radius: 0.03 + 0.04 * h(3),
                    color: if h(5) < 0.5 { Color::hex(0xffc477, 1.0) } else { Color::hex(0x77c8ff, 1.0) },
                    intensity: 1.2 + 1.5 * h(6),
                }
            })
            .collect();
        let hoop = MeshDraw {
            mesh: self.hoop,
            model: Mat4::uniform_scale(1.0 + 0.04 * lv).then(&ring),
            material: Material {
                color: Color::hex(0x9fc8ff, 1.0),
                emissive: 0.6 + lv,
                specular: 0.9,
                shininess: 64.0,
                ..Default::default()
            },
        };
        let mut meshes = vec![hoop];
        let word_model = Mat4::translation(Vec3::new(0.0, 1.0, -0.6));
        let ink = Material { color: Color::hex(0xeef2fa, 1.0), specular: 0.7, emissive: 0.05, ..Default::default() };
        meshes.extend(self.word.draws(t, &word_model, ink));
        Frame3d {
            camera: Camera::orbit(
                Vec3::new(0.0, 0.25, 0.0),
                0.28 * (t * 0.35).sin() as f32,
                self.pitch.value(t) as f32,
                4.4,
            ),
            background: Background::Vertical { top: Color::hex(0x0a0d14, 1.0), bottom: Color::hex(0x1a2030, 1.0) },
            light: Light { dir: Vec3::new(-0.5, -0.7, -0.6), ..Default::default() },
            meshes,
            clouds,
            orbs,
            focus: Focus { distance: 4.4, range: 2.6, max_blur_px: 64.0 },
            fog: Some((5.0, 10.0)),
            on_stage: None,
            // A glossy floor under the ring (D305) that fades up over the first half second,
            // with the ring's light pooling on it and a soft cone from above.
            floor: Some(Floor {
                y: -0.92,
                reflect: 0.45 * smooth01(t / 0.5),
                fade: 1.2,
                pools: [
                    Some(Pool {
                        center: Vec3::ZERO,
                        radius: 1.9,
                        color: Color::hex(0x5f8dff, 1.0),
                        intensity: 0.35 + 0.5 * lv,
                    }),
                    Some(Pool {
                        center: Vec3::new(0.9, 0.0, 0.3),
                        radius: 0.9,
                        color: Color::hex(0xa070ff, 1.0),
                        intensity: 0.25,
                    }),
                    None,
                    None,
                ],
            }),
            cones: vec![LightCone {
                apex: Vec3::new(0.0, 2.8, -1.4),
                dir: Vec3::new(0.0, -1.0, 0.4),
                half_angle: 0.38,
                length: 4.4,
                color: Color::hex(0x7aa8ff, 1.0),
                intensity: 0.22 * smooth01(t / 0.5),
            }],
        }
    }

    /// The label lying in the ring's plane, inside the front half of the hole.
    fn plane_model(t: f64) -> Mat4 {
        Mat4::rotate_x(-FRAC_PI_2).then(&Mat4::translation(Vec3::new(-0.88, 0.0, 0.62))).then(&Demo::ring_model(t))
    }
}

struct Lists {
    vector: DisplayList,
    text: DisplayList,
    label: DisplayList,
}

fn lists(d: &Demo, t: f64) -> Lists {
    Lists { vector: d.vector.sample(t), text: d.text.sample(t), label: d.label.sample(t) }
}

/// Draws frame `t`. The scene is laid out at `W` x `H`; `scale` maps it to the target.
fn draw(stack: &mut Stack, d: &Demo, t: f64, target: &Target, scale: f32) {
    let l = lists(d, t);
    let frame = d.frame(t);
    let plane = TextPlane { list: &l.label, model: Demo::plane_model(t), units_per_px: 0.0042, opacity: 1.0 };
    let planes = [plane];
    let layers = Layers {
        scene: Some(&frame),
        vector: Some(&l.vector),
        text: Some(&l.text),
        field_from_text: true,
        planes: &planes,
    };
    stack.render(&layers, target, scale, None);
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let medium = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    if fonts.add_file(medium.as_ref()).is_err() {
        fonts.add_file(&test_font_path()).expect("test font");
    }
    fonts
}

fn frame_hash(px: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    px.hash(&mut h);
    h.finish()
}

fn main() {
    if vo::preview::wanted() {
        let preview = Preview::custom("depth_demo", Size::new(W as f32, H as f32), DURATION, |device, queue| {
            let fonts = fonts();
            let mut stack = Stack::new(device, queue, fonts.clone());
            let mut demo = Demo::new(&fonts, &mut stack.scene);
            move |t: f64, target: &Target| {
                demo.advance(t);
                draw(&mut stack, &demo, t, target, target.width() as f32 / W as f32);
            }
        })
        .marker("words", word_time(0))
        .marker("quiet", 6.2)
        .marker("turn out", 6.9);
        std::process::exit(vo::preview::main(preview));
    }
    let mut args = std::env::args().skip(1);
    let (mut out, mut live, mut seconds) = (PathBuf::from("/var/tmp/claude/m4-3d"), false, DURATION);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--live" => live = true,
            "--seconds" => seconds = args.next().and_then(|s| s.parse().ok()).expect("--seconds needs a number"),
            // Read by the host: open on the real desktop on purpose.
            "--real-display" => {}
            _ => panic!("unknown option {a}"),
        }
    }
    if live {
        live::run(seconds);
    } else {
        offline(&out);
    }
}

fn offline(out: &std::path::Path) {
    std::fs::create_dir_all(out).expect("output directory");
    let t_start = Instant::now();
    let mut idesc = wgpu::InstanceDescriptor::new_without_display_handle();
    idesc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(idesc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a Vulkan adapter");
    let features = adapter.features() & timer::FEATURES;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("depth_demo"),
        required_features: features,
        ..Default::default()
    }))
    .expect("a device");
    let t_device = t_start.elapsed();

    let fonts = fonts();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let t0 = Instant::now();
    let mut stack = Stack::new(&device, &queue, fonts.clone());
    let pipelines = stack.prepare(format);
    let field_pipelines = stack.field.pipeline_time;
    let t1 = Instant::now();
    let mut demo = Demo::new(&fonts, &mut stack.scene);
    let t_meshes = t1.elapsed();
    let target = Target::new(&device, W, H, format);
    let t2 = Instant::now();
    demo.advance(0.0);
    draw(&mut stack, &demo, 0.0, &target, 1.0);
    let first = vo_3d::readback::read_rgba(&device, &queue, target.texture());
    let t_first = t2.elapsed();
    let t_stack = t0.elapsed();
    drop(first);

    let mp4 = out.join("depth_demo.mp4");
    let mut ffmpeg = Command::new(std::env::var("VO_FFMPEG").unwrap_or_else(|_| "ffmpeg".into()))
        .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{W}x{H}")])
        .args(["-r", &FPS.to_string(), "-i", "-", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18"])
        .args(["-map_metadata", "-1", "-fflags", "+bitexact", "-flags:v", "+bitexact"])
        .arg(&mp4)
        .stdin(Stdio::piped())
        .spawn()
        .expect("ffmpeg (set VO_FFMPEG to its path)");
    let mut pipe = ffmpeg.stdin.take().unwrap();

    let frames = (DURATION * FPS).round() as u64;
    let sheet_frames: Vec<u64> = SHEET_AT.iter().map(|t| (t * FPS).round() as u64).collect();
    let mut thumbs = Vec::new();
    let mut hashes = BTreeMap::new();
    let mut stages: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    let mut cpu_frame = Vec::new();
    let start = Instant::now();
    for n in 0..frames {
        let t = n as f64 / FPS;
        let f0 = Instant::now();
        demo.advance(t);
        draw(&mut stack, &demo, t, &target, 1.0);
        for (stage, ms) in stack.stage_times() {
            stages.entry(stage).or_default().push(ms);
        }
        let px = vo_3d::readback::read_rgba(&device, &queue, target.texture());
        cpu_frame.push(f0.elapsed());
        if sheet_frames.contains(&n) {
            hashes.insert(n, frame_hash(&px));
            thumbs.push(thumb(&px, W, H, 4));
        }
        pipe.write_all(&px).expect("write to ffmpeg");
    }
    drop(pipe);
    let ok = ffmpeg.wait().expect("ffmpeg").success();
    let wall = start.elapsed().as_secs_f64();

    // Seek: the sheet frames again, last first. Same pixels or the demo is not a function of t.
    let mut mismatches = Vec::new();
    for &n in sheet_frames.iter().rev() {
        let t = n as f64 / FPS;
        draw(&mut stack, &demo, t, &target, 1.0);
        let px = vo_3d::readback::read_rgba(&device, &queue, target.texture());
        if hashes[&n] != frame_hash(&px) {
            mismatches.push(n);
        }
    }

    let sheet = contact_sheet(&thumbs, W / 4, H / 4);
    let sheet_path = out.join("contact-sheet.png");
    vo_render::offscreen::write_png(&sheet_path, sheet.1, sheet.2, &sheet.0).expect("contact sheet");

    println!(
        "depth_demo: {frames} frames {W}x{H} in {wall:.2} s, video {} ({})",
        mp4.display(),
        if ok { "ok" } else { "ffmpeg FAILED" }
    );
    println!("depth_demo: contact sheet {} (frames {sheet_frames:?})", sheet_path.display());
    println!(
        "cold start: device {:.0} ms; stack {:.0} ms = pipelines {:.0} ms (field compute {:.1} ms) + meshes {:.1} ms + first frame {:.0} ms (incl. readback)",
        ms(t_device),
        ms(t_stack),
        ms(pipelines),
        ms(field_pipelines),
        ms(t_meshes),
        ms(t_first)
    );
    let mut cpu: Vec<f64> = cpu_frame.iter().map(|d| ms(*d)).collect();
    cpu.sort_by(f64::total_cmp);
    println!(
        "cpu per frame incl. readback: median {:.2} ms, p95 {:.2} ms",
        cpu[cpu.len() / 2],
        cpu[cpu.len() * 95 / 100]
    );
    if stages.is_empty() {
        println!("gpu stage times: no timestamp queries on this device");
    }
    let mut total = vec![0.0; frames as usize];
    for (stage, v) in &stages {
        let mut s = v.clone();
        for (i, x) in v.iter().enumerate() {
            total[i] += x;
        }
        s.sort_by(f64::total_cmp);
        println!(
            "gpu {stage:>6}: mean {:.3} ms, p95 {:.3} ms, max {:.3} ms",
            mean(&s),
            s[s.len() * 95 / 100],
            s[s.len() - 1]
        );
    }
    if !stages.is_empty() {
        total.sort_by(f64::total_cmp);
        println!("gpu  total: mean {:.3} ms, p95 {:.3} ms", mean(&total), total[total.len() * 95 / 100]);
    }
    if mismatches.is_empty() {
        println!("seek check: {} frames drawn again out of order, identical", sheet_frames.len());
    } else {
        println!("seek check FAILED at frames {mismatches:?}");
        std::process::exit(1);
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len().max(1) as f64
}

/// A box-filtered thumbnail, `k` times smaller.
fn thumb(px: &[u8], w: u32, h: u32, k: u32) -> Vec<u8> {
    let (tw, th) = (w / k, h / k);
    let mut out = vec![0u8; (tw * th * 4) as usize];
    for y in 0..th {
        for x in 0..tw {
            for c in 0..4 {
                let mut sum = 0u32;
                for dy in 0..k {
                    for dx in 0..k {
                        sum += px[(((y * k + dy) * w + x * k + dx) * 4 + c) as usize] as u32;
                    }
                }
                out[((y * tw + x) * 4 + c) as usize] = (sum / (k * k)) as u8;
            }
        }
    }
    out
}

/// Thumbnails in a 4-column grid with an 8 px gutter.
fn contact_sheet(thumbs: &[Vec<u8>], tw: u32, th: u32) -> (Vec<u8>, u32, u32) {
    let (gap, cols) = (8u32, 4u32);
    let rows = (thumbs.len() as u32).div_ceil(cols);
    let (w, h) = (cols * tw + (cols + 1) * gap, rows * th + (rows + 1) * gap);
    let mut rgba = vec![255u8; (w * h * 4) as usize];
    for (i, t) in thumbs.iter().enumerate() {
        let (ox, oy) = (gap + (i as u32 % cols) * (tw + gap), gap + (i as u32 / cols) * (th + gap));
        for y in 0..th {
            let src = &t[(y * tw * 4) as usize..((y + 1) * tw * 4) as usize];
            let at = (((oy + y) * w + ox) * 4) as usize;
            rgba[at..at + src.len()].copy_from_slice(src);
        }
    }
    (rgba, w, h)
}

/// The same scene as a live Wayland overlay.
mod live {
    use super::*;
    use vo_host::{Frame, Host, HostApp, HostCtl, HostEvent, OverlaySpec, Placement, SurfaceId};
    use vo_host_wayland::WaylandHost;

    struct App {
        seconds: f64,
        spec: OverlaySpec,
        surface: Option<SurfaceId>,
        t0: Option<f64>,
        fonts: FontRegistry,
        stack: Option<(Stack, Demo)>,
    }

    impl HostApp for App {
        fn start(&mut self, host: &mut dyn HostCtl) {
            let id = host.open(&self.spec).expect("open the overlay");
            self.surface = Some(id);
            let gpu = host.gpu().expect("gpu");
            let mut stack = Stack::new(&gpu.device, &gpu.queue, self.fonts.clone());
            let demo = Demo::new(&self.fonts, &mut stack.scene);
            self.stack = Some((stack, demo));
        }

        fn event(&mut self, host: &mut dyn HostCtl, event: HostEvent) {
            match event {
                HostEvent::Configured { id, .. } | HostEvent::Presented { id, .. } if Some(id) == self.surface => {
                    host.request_frame(id)
                }
                HostEvent::Closed { .. } => host.exit(),
                _ => {}
            }
        }

        fn frame(&mut self, host: &mut dyn HostCtl, frame: &Frame<'_>) {
            let t0 = *self.t0.get_or_insert(frame.t);
            let t = frame.t - t0;
            if t > self.seconds {
                host.exit();
                return;
            }
            let t = t.min(DURATION - 1e-3);
            let (stack, demo) = self.stack.as_mut().unwrap();
            demo.advance(t);
            let target = Target::from_texture(frame.texture.clone());
            // The scene is laid out at 1280 x 720; the overlay shows it at its own size.
            draw(stack, demo, t, &target, 1.0);
        }
    }

    pub fn run(seconds: f64) {
        let host = WaylandHost::connect().expect("a Wayland compositor");
        let spec = OverlaySpec {
            size: Size::new(W as f32, H as f32),
            placement: Placement::Centered,
            app_id: "vo-depth-demo".into(),
            ..OverlaySpec::default()
        };
        let mut app = App { seconds, spec, surface: None, t0: None, fonts: fonts(), stack: None };
        host.run(&mut app).expect("host");
    }
}
