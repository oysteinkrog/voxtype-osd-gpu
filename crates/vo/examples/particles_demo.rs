//! Particles demo (design family 09): a voice ring that follows a scripted level, then a
//! sentence that forms word by word from particles, with one word revised on the way.
//!
//! Offline, to an mp4, a few PNG frames and frame hashes:
//!   cargo run --release -p vo --example particles_demo -- --out /var/tmp/claude/particles-demo
//! The same at 60 fps and at 144 fps, frames at shared times compared:
//!   cargo run --release -p vo --example particles_demo -- --check
//! Live, on Wayland (only inside a nested compositor when run by an agent):
//!   cargo run --release -p vo --example particles_demo -- --live
//!
//! The script is a function of time, and so is everything drawn: the ring steps at a fixed
//! tick with inputs taken at each tick's time, and words re-anchor at their event times. A
//! live run and an export give the same frames at the same times.

use std::io::Write;
use std::path::PathBuf;

use vo_render::offscreen::write_png;
use vo_render::particles::{EmitterLook, EmitterSim, FrameParams, ParticleConfig, Particles, SlotLook, WordGlyph};
use vo_render::{Renderer, Target};
use vo_scene::display::{DisplayList, GlyphInstance, Item, ItemKind, Layer, Paint, ShapeInstance};
use vo_scene::element::TextStyle;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Affine, Color, Point, RRect, Rect, Size};
use vo_scene::text::{ShapedLayout, TextLayout};

const W: u32 = 1280;
const H: u32 = 400;
const DURATION: f64 = 6.5;
const TEXT_SIZE: f32 = 36.0;
const TEXT_X: f32 = 330.0;
const BASELINE: f32 = 214.0;
const RING: Point = Point { x: 170.0, y: 200.0 };

/// Word arrivals and revisions: `(t, word index, text)`, in time order.
const SCRIPT: &[(f64, usize, &str)] = &[
    (2.00, 0, "Particles"),
    (2.30, 1, "gather"),
    (2.60, 2, "into"),
    (2.90, 3, "wards"),
    (3.20, 4, "as"),
    (3.40, 3, "words"),
    (3.50, 5, "you"),
    (3.85, 6, "speak."),
];

fn smooth(a: f64, b: f64, x: f64) -> f64 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A speech-like level: syllable bursts from 0.4 s to 4.4 s, then quiet.
fn level(t: f64) -> f32 {
    let on = smooth(0.3, 0.6, t) * (1.0 - smooth(4.2, 4.7, t));
    let phrase = 0.55 + 0.45 * (t * 1.7).sin();
    let syl = (0.5 + 0.5 * (t * 11.0 + 0.6 * (t * 3.1).sin()).sin()).powf(2.5);
    (0.06 + 0.94 * on * phrase * syl) as f32
}

/// The level, smoothed with a fast attack and slow release, as a closed form over a short
/// window so it stays a function of `t`.
fn envelope(t: f64) -> f32 {
    let mut e = 0.0f32;
    for i in (0..24).rev() {
        let l = level(t - i as f64 * 0.01);
        let k = if l > e { 0.6 } else { 0.08 };
        e += (l - e) * k;
    }
    e
}

fn ring_sim(_: u32, t: f64) -> EmitterSim {
    let l = envelope(t);
    EmitterSim {
        radius: 62.0 + 46.0 * l,
        swirl: 0.9 + 4.5 * l,
        level: l,
        noise: 160.0 + 900.0 * l,
        life: 1.1,
        spread: 0.12 + 0.25 * l,
        scale: 0.025,
    }
}

fn ring_look(t: f64) -> EmitterLook {
    let l = envelope(t);
    // The ring dims a little once words are on screen, and settles when the voice stops.
    let words = 1.0 - 0.25 * smooth(2.0, 2.6, t) as f32;
    EmitterLook {
        origin: RING,
        size: 1.3 + 0.9 * l,
        brightness: (0.1 + 0.32 * l) * words,
        ramp: [Color::hex(0x9fe8ff, 1.0), Color::hex(0x4f7dff, 0.8), Color::hex(0xa04cff, 0.0)],
    }
}

struct Demo {
    renderer: Renderer,
    particles: Particles,
    layout: ShapedLayout,
    /// Current text per word slot.
    words: Vec<Option<String>>,
    next_event: usize,
}

impl Demo {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, fonts: FontRegistry) -> Demo {
        let renderer = Renderer::with_fonts(device, queue, fonts.clone());
        let cfg = ParticleConfig { free_capacity: 24_576, word_slots: 8, slot_capacity: 1024, tick_hz: 120.0, seed: 9 };
        let mut particles = Particles::new(device, queue, fonts.clone(), cfg);
        particles.add_ring(24_576, 21, 0.9);
        Demo { renderer, particles, layout: ShapedLayout::new(fonts), words: vec![None; 8], next_event: 0 }
    }

    /// Glyphs of one word, relative to its first pen position.
    fn word_glyphs(&self, text: &str) -> Vec<WordGlyph> {
        let shaped = self.layout.layout(text, &TextStyle::new(0, TEXT_SIZE, TEXT_SIZE * 1.3), None);
        let first = shaped.glyphs.first().map_or(Point::ZERO, |g| g.pos);
        shaped
            .glyphs
            .iter()
            .map(|g| WordGlyph {
                font: 0,
                glyph: g.glyph,
                size: TEXT_SIZE,
                pos: Point::new(g.pos.x - first.x, g.pos.y - first.y),
            })
            .collect()
    }

    /// Applies script events up to `t`, each at its own time.
    fn apply_events(&mut self, t: f64) {
        while let Some(&(te, slot, text)) = SCRIPT.get(self.next_event) {
            if te > t {
                break;
            }
            let glyphs = self.word_glyphs(text);
            self.particles.set_word(slot as u32, te, &glyphs, 1.0);
            self.words[slot] = Some(text.to_string());
            self.next_event += 1;
        }
    }

    /// Word origins from the current line, by the byte offset each word starts at.
    fn origins(&self) -> Vec<Option<Point>> {
        let present: Vec<(usize, &str)> =
            self.words.iter().enumerate().filter_map(|(i, w)| w.as_deref().map(|w| (i, w))).collect();
        let line = present.iter().map(|(_, w)| *w).collect::<Vec<_>>().join(" ");
        let shaped = self.layout.layout(&line, &TextStyle::new(0, TEXT_SIZE, TEXT_SIZE * 1.3), None);
        let mut out = vec![None; self.words.len()];
        let mut start = 0u32;
        for (slot, w) in present {
            if let Some(g) = shaped.glyphs.iter().find(|g| g.cluster == start) {
                out[slot] = Some(Point::new(TEXT_X + g.pos.x, BASELINE + g.pos.y - shaped.glyphs[0].pos.y));
            }
            start += w.len() as u32 + 1;
        }
        out
    }

    fn display_list(&self, t: f64, origins: &[Option<Point>]) -> DisplayList {
        let mut list = DisplayList {
            size: Size::new(W as f32, H as f32),
            scale: 1.0,
            layers: vec![Layer::root()],
            ..Default::default()
        };
        let panel = RRect::new(Rect::new(16.0, 16.0, W as f32 - 32.0, H as f32 - 32.0), 28.0);
        let fill = Paint::linear(
            Point::new(0.0, 16.0),
            Color::hex(0x10141c, 0.94),
            Point::new(0.0, H as f32 - 16.0),
            Color::hex(0x07090d, 0.94),
        );
        list.shapes.push(ShapeInstance::rrect(panel, fill).with_stroke(Paint::solid(Color::hex(0x2a3446, 0.8)), 1.0));
        list.items.push(Item { kind: ItemKind::Shapes, start: 0, len: 1 });
        for (slot, text) in self.words.iter().enumerate() {
            let (Some(text), Some(origin)) = (text, origins[slot]) else { continue };
            for (i, g) in self.word_glyphs(text).iter().enumerate() {
                let a = self.particles.glyph_alpha(slot as u32, i, t);
                if a <= 0.0 {
                    continue;
                }
                list.glyphs.push(GlyphInstance {
                    pos: Point::new(origin.x + g.pos.x, origin.y + g.pos.y),
                    size: g.size,
                    font: 0,
                    glyph: g.glyph,
                    word: slot as u32,
                    color: Color::hex(0xeef3ff, a),
                });
            }
        }
        list.items.push(Item { kind: ItemKind::Glyphs, start: 0, len: list.glyphs.len() as u32 });
        list.layers[0].items = 0..2;
        list
    }

    /// Records frame `t` into `target`.
    fn frame(&mut self, t: f64, target: &Target, enc: &mut wgpu::CommandEncoder) {
        self.apply_events(t);
        self.particles.advance(t, &ring_sim);
        let origins = self.origins();
        let looks: Vec<SlotLook> = origins
            .iter()
            .map(|o| SlotLook {
                origin: o.unwrap_or(Point::ZERO),
                color: Color::hex(0x9fd8ff, 1.0),
                visible: o.is_some(),
            })
            .collect();
        let params = FrameParams {
            t,
            to_px: Affine::scale(target.width() as f32 / W as f32),
            px_per_unit: target.width() as f32 / W as f32,
            width: target.width(),
            height: target.height(),
            format: target.format(),
            stretch: 1.0 / 50.0,
        };
        self.particles.prepare(&params, &|_| ring_look(t), &looks);
        let list = self.display_list(t, &origins);
        self.renderer.encode(&list, params.px_per_unit, target, enc);
        let view = target.texture().create_view(&Default::default());
        self.particles.draw_over(enc, &view);
    }
}

fn headless() -> (wgpu::Device, wgpu::Queue) {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("no GPU adapter");
    pollster::block_on(adapter.request_device(&Default::default())).expect("no GPU device")
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).expect("test font");
    fonts
}

/// Renders frames at `fps` and hands each to `sink(index, t, rgba)`.
fn render(fps: f64, sink: &mut dyn FnMut(u64, f64, &[u8])) {
    let (device, queue) = headless();
    let mut demo = Demo::new(&device, &queue, fonts());
    let target = Target::new(&device, W, H, wgpu::TextureFormat::Rgba8UnormSrgb);
    let row = (W * 4).next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: row as u64 * H as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    let frames = (DURATION * fps).round() as u64;
    for n in 0..frames {
        let t = n as f64 / fps;
        let mut enc = device.create_command_encoder(&Default::default());
        demo.frame(t, &target, &mut enc);
        enc.copy_texture_to_buffer(
            target.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
            },
            wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        {
            let data = readback.slice(..).get_mapped_range().expect("mapped");
            for y in 0..H as usize {
                let s = y * row as usize;
                rgba[y * W as usize * 4..(y + 1) * W as usize * 4].copy_from_slice(&data[s..s + W as usize * 4]);
            }
        }
        readback.unmap();
        sink(n, t, &rgba);
    }
}

fn fnv(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn export(out: PathBuf) {
    std::fs::create_dir_all(out.join("frames")).unwrap();
    let fps = 60.0;
    let mut ffmpeg = std::process::Command::new(std::env::var("VO_FFMPEG").unwrap_or_else(|_| "ffmpeg".into()))
        .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{W}x{H}")])
        .args(["-r", "60", "-i", "-", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "14", "-preset", "slow"])
        .arg(out.join("particles_demo.mp4"))
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("ffmpeg; install it or set VO_FFMPEG");
    let mut stdin = ffmpeg.stdin.take().unwrap();
    let shots = [0.45, 1.2, 2.04, 2.1, 2.16, 2.3, 2.95, 3.42, 3.47, 3.55, 4.05, 5.5];
    let mut hashes = String::new();
    render(fps, &mut |n, t, rgba| {
        stdin.write_all(rgba).unwrap();
        hashes.push_str(&format!("{n} {:016x}\n", fnv(rgba)));
        if shots.iter().any(|s| (s * fps).round() as u64 == n) {
            let path = out.join("frames").join(format!("t{:04}ms.png", (t * 1000.0).round() as u64));
            write_png(&path, W, H, rgba).unwrap();
        }
    });
    drop(stdin);
    assert!(ffmpeg.wait().unwrap().success(), "ffmpeg failed");
    std::fs::write(out.join("frame-hashes.txt"), hashes).unwrap();
    println!("wrote {}", out.join("particles_demo.mp4").display());
}

/// Renders at 60 fps and at 144 fps and compares the frames both have (every 1/12 s).
fn check() {
    let mut at_60 = Vec::new();
    render(60.0, &mut |n, _, rgba| {
        if n % 5 == 0 {
            at_60.push(fnv(rgba));
        }
    });
    let mut at_144 = Vec::new();
    render(144.0, &mut |n, _, rgba| {
        if n % 12 == 0 {
            at_144.push(fnv(rgba));
        }
    });
    let same = at_60.iter().zip(&at_144).filter(|(a, b)| a == b).count();
    println!("shared frame times: {}; identical: {same}", at_60.len().min(at_144.len()));
    assert_eq!(same, at_60.len().min(at_144.len()), "frames differ between 60 fps and 144 fps");
}

#[cfg(feature = "wayland")]
mod live {
    use vo_host::{Frame, Host, HostApp, HostCtl, HostEvent, OverlaySpec, Placement, SurfaceId};
    use vo_host_wayland::WaylandHost;

    use super::*;

    struct LiveApp {
        demo: Option<Demo>,
        start: f64,
        surface: Option<SurfaceId>,
        hold: f64,
    }

    impl HostApp for LiveApp {
        fn start(&mut self, host: &mut dyn HostCtl) {
            let spec = OverlaySpec {
                size: vo_host::Size::new(W as f32, H as f32),
                placement: Placement::Centered,
                app_id: "vo-particles-demo".into(),
                ..Default::default()
            };
            self.surface = Some(host.open(&spec).expect("open surface"));
        }

        fn event(&mut self, host: &mut dyn HostCtl, event: HostEvent) {
            if let HostEvent::Configured { id, t, .. } = event {
                if self.demo.is_none() {
                    let gpu = host.gpu().expect("GPU");
                    self.demo = Some(Demo::new(&gpu.device, &gpu.queue, fonts()));
                    self.start = t;
                }
                host.request_frame(id);
            }
            // The host drops a request made while it draws, so ask again once a frame is shown.
            if let HostEvent::Presented { id, .. } = event {
                host.request_frame(id);
            }
        }

        fn frame(&mut self, host: &mut dyn HostCtl, frame: &Frame<'_>) {
            let Some(demo) = &mut self.demo else { return };
            let t = (frame.t - self.start).max(0.0);
            if std::env::var_os("VO_DIAG_FRAMES").is_some() {
                eprintln!("frame {} t {t:.3}", frame.frame);
            }
            let gpu = host.gpu().expect("GPU");
            let target = Target::from_texture(frame.texture.clone());
            let mut enc = gpu.device.create_command_encoder(&Default::default());
            demo.frame(t.min(self.hold), &target, &mut enc);
            gpu.queue.submit([enc.finish()]);
            if t > DURATION + 1.0 {
                host.exit();
            }
        }
    }

    /// Plays the script on a layer-shell overlay. `hold` freezes the clock at that time, for
    /// a screenshot.
    pub fn run(hold: f64) {
        let host = WaylandHost::connect().expect("Wayland");
        let mut app = LiveApp { demo: None, start: 0.0, surface: None, hold };
        host.run(&mut app).expect("host");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    if args.iter().any(|a| a == "--check") {
        check();
    } else if args.iter().any(|a| a == "--live") {
        #[cfg(feature = "wayland")]
        live::run(arg("--hold").and_then(|s| s.parse().ok()).unwrap_or(f64::INFINITY));
        #[cfg(not(feature = "wayland"))]
        panic!("built without the wayland feature");
    } else {
        export(PathBuf::from(arg("--out").unwrap_or_else(|| "/var/tmp/claude/particles-demo".into())));
    }
}
