//! Text flow demo (FX-002, FX-063, FX-032, FX-039, FX-040). Three bands:
//!
//! - Top: a centred, balanced paragraph that grows word by word. Each line stays centred,
//!   and lines re-centre on a spring as words arrive.
//! - Middle: inline objects in live text. A pause marker grows with the pause, a gap slot
//!   shows a mini waveform of speech that has no words yet and gives way to the words, braces
//!   close around a phrase, and an "end sentence?" chip boxes its words.
//! - Bottom: glyphs on curves. Text rides a pitch curve that changes shape, a comma flies on
//!   an S-curve into its slot with a leader line, and a curved arrow marks a case fix.
//!
//!   cargo run --release -p vo --example text_flow_demo -- [--out DIR] [--check] [--bench]
//!
//! Writes `DIR/text_flow_demo.mp4` and `DIR/text-flow-sheet.png` (default DIR
//! `/var/tmp/claude/fw-flow`). `--check` renders again and compares every frame's hash.
//! `--bench` times every frame on the GPU at 600x300 and 1920x1080.

use std::path::PathBuf;

use vo::scene::curve::{draw_on_path, path_start, place_on_path, Arrow, Flight};
use vo::scene::display::{GlyphInstance, NO_WORD};
use vo::scene::element::{Element, TextStyle};
use vo::scene::flow::Caret;
use vo::scene::geom::{Color, Point, Size};
use vo::scene::inline::Inline;
use vo::scene::path::{make, StrokeStyle};
use vo::scene::taffy::prelude::AlignItems;
use vo::scene::text::{Align, ShapedText, TextLayout};
use vo::scene::{Scene, Transition};
use vo::{App, Cx, Export, FontRegistry, Options, Scenario, ShapedLayout, Video};
use vo_core::words::Word;
use vo_render::offscreen::{write_png, Frame};
use vo_render::{Renderer, Target};

const FPS: f64 = 60.0;
const DURATION: f64 = 6.0;
const SURFACE: Size = Size::new(600.0, 300.0);
const PARAGRAPH: &str = "centred lines stay centred as the sentence grows and every line keeps an even length";
const WORD_GAP: f64 = 0.3;
/// Ids of inline objects, clear of word ids.
const PAUSE: u32 = 1000;
const GAP: u32 = 1001;
const BRACE_OPEN: u32 = 1002;
const BRACE_CLOSE: u32 = 1003;
const CHIP_OPEN: u32 = 1004;
const CHIP_CLOSE: u32 = 1005;

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Tick(f64),
}

struct Demo {
    fonts: FontRegistry,
    t: f64,
}

fn ink() -> Color {
    Color::hex(0xe9edf5, 1.0)
}

fn accent() -> Color {
    Color::hex(0x6cd4ff, 1.0)
}

fn word(id: u32, text: &str) -> Word {
    Word { id, text: text.into() }
}

/// The middle band's words and objects at `t`.
fn inline_state(t: f64) -> (Vec<Word>, Vec<(u32, Inline)>) {
    let mut w = vec![word(1, "so")];
    let mut objects = Vec::new();
    if t < 2.4 {
        // A pause that grows while nothing is said, then the gap slot after the next words.
        let pause = (t - 0.3).clamp(0.0, 1.4) as f32;
        w.push(Inline::word(PAUSE));
        objects.push((PAUSE, Inline::pause(pause)));
    }
    if t >= 1.0 {
        w.extend([word(2, "the"), word(3, "plan")]);
    }
    if (1.4..2.4).contains(&t) {
        let secs = (t - 1.4) as f32;
        let levels: Vec<f32> = (0..(4.0 + secs * 10.0) as usize)
            .map(|i| 0.25 + 0.7 * ((i as f32 * 1.7 + secs * 9.0).sin() * 0.5 + 0.5))
            .collect();
        w.push(Inline::word(GAP));
        objects.push((GAP, Inline::gap(secs.max(0.6), levels).color(accent().fade(0.8))));
    }
    if t >= 2.4 {
        w.extend([word(4, "is"), word(5, "simple")]);
    }
    if t >= 3.2 {
        // Braces around the phrase being rewritten.
        let at = w.iter().position(|x| x.id == 2).unwrap();
        w.insert(at, Inline::word(BRACE_OPEN));
        let end = w.iter().position(|x| x.id == 3).unwrap() + 1;
        w.insert(end, Inline::word(BRACE_CLOSE));
        let amber = Color::hex(0xffb347, 1.0);
        objects.extend([
            (BRACE_OPEN, Inline::brace_open().color(amber)),
            (BRACE_CLOSE, Inline::brace_close().color(amber)),
        ]);
    }
    if t >= 4.0 {
        w.extend([Inline::word(CHIP_OPEN), word(6, "end"), word(7, "sentence?"), Inline::word(CHIP_CLOSE)]);
        objects.extend([
            (CHIP_OPEN, Inline::chip_open().color(accent())),
            (CHIP_CLOSE, Inline::chip_close().color(accent())),
        ]);
    }
    (w, objects)
}

impl Demo {
    fn shape(&self, text: &str, size: f32) -> ShapedText {
        ShapedLayout::new(self.fonts.clone()).layout(text, &TextStyle::new(0, size, size * 1.3), None)
    }

    fn curves(&self) -> Element<Msg> {
        let line = self.shape("rising then falling pitch", 15.0);
        let sentence = self.shape("well I think so", 17.0);
        let comma = self.shape(",", 17.0);
        let big_w = self.shape("W", 17.0);
        Element::canvas("curves", DURATION, move |t, rect, p| {
            // Pitch trace on the left: its shape changes, and the text relays on it each frame.
            let amp = 9.0 + 7.0 * (t * 1.3).sin() as f32;
            let trace = make::wave(rect.x + 12.0, rect.x + 280.0, rect.y + 52.0, amp, 190.0, (t * 0.8) as f32);
            p.stroke(&trace, &StrokeStyle::solid(1.0, accent().fade(0.35)));
            let start = path_start(Align::Center, trace.length(), line.size.w);
            draw_on_path(p, &place_on_path(&line, &trace, start), 15.0, ink());

            // Right: "well I think so" with a slot opening after "well" for a comma.
            let base = Point::new(rect.x + 330.0, rect.y + 78.0);
            let comma_g = comma.glyphs[0];
            let well_end = sentence.glyphs[3].pos.x + sentence.glyphs[3].advance;
            let open = (((t - 0.6) / 0.25).clamp(0.0, 1.0)) as f32 * comma_g.advance;
            let fixed = (t >= 1.8) as u8 as f32;
            for (i, g) in sentence.glyphs.iter().enumerate() {
                let shift = if g.pos.x >= well_end { open } else { 0.0 };
                // The "w" becomes "W" when the arrow lands.
                let (glyph, color) =
                    if i == 0 && fixed > 0.0 { (big_w.glyphs[0].glyph, accent()) } else { (g.glyph, ink()) };
                p.glyph(GlyphInstance {
                    pos: Point::new(base.x + g.pos.x + shift, base.y),
                    size: 17.0,
                    font: g.font,
                    glyph,
                    word: NO_WORD,
                    color,
                });
            }
            let flight = Flight::new(
                Point::new(rect.x + 560.0, rect.y + 18.0),
                Point::new(base.x + well_end + 0.5 * comma_g.advance, base.y),
                0.8,
                0.55,
            );
            let leader = StrokeStyle::solid(1.2, accent().fade(0.7));
            flight.draw(p, t, comma_g.glyph, comma_g.font, comma_g.advance, 17.0, ink(), &leader);

            // A curved arrow over the "w", drawn on from 1.2 s, with its caption.
            let arrow = Arrow::new(Point::new(base.x - 26.0, base.y - 34.0), Point::new(base.x + 5.0, base.y - 18.0));
            let u = ((t - 1.2) / 0.5).clamp(0.0, 1.0) as f32;
            arrow.draw(p, u, &StrokeStyle::solid(1.4, Color::hex(0xffb347, 1.0)));
        })
        .size(SURFACE.w - 24.0, 110.0)
    }
}

impl App for Demo {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        let mut t = 0.0;
        while t <= DURATION {
            cx.after(t, Msg::Tick(t));
            t += 1.0 / 30.0;
        }
    }

    fn update(&mut self, msg: Msg, _: &mut Cx<Msg>) {
        let Msg::Tick(t) = msg;
        self.t = t;
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        let shown = (((self.t - 0.2) / WORD_GAP).floor() as i64 + 1).clamp(0, 99) as usize;
        let words: Vec<Word> = PARAGRAPH.split(' ').take(shown).enumerate().map(|(j, w)| word(j as u32, w)).collect();
        let centred = Element::live_text("centred", &words)
            .text_style(TextStyle::new(0, 18.0, 24.0).centered().balanced())
            .color(ink())
            .word_enter(Transition::fade())
            .caret(Caret { color: Some(accent()), ..Caret::default() })
            .width(SURFACE.w - 24.0)
            .height(76.0);
        let (w, objects) = inline_state(self.t);
        let flow = Element::live_text("inline", &w)
            .text_style(TextStyle::new(0, 18.0, 26.0))
            .color(ink())
            .word_enter(Transition::fade())
            .inline_objects(objects)
            .width(SURFACE.w - 24.0)
            .height(70.0);
        Element::panel("root")
            .size(SURFACE.w, SURFACE.h)
            .pad(12.0)
            .gap(8.0)
            .fill_gradient(Color::hex(0x0b0d14, 1.0), Color::hex(0x161a2a, 1.0))
            .align_items(AlignItems::CENTER)
            .children([centred, flow, self.curves()])
    }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let path = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
    fonts
}

/// Frames for the contact sheet.
const SHEET_AT: [f64; 8] = [0.9, 1.1, 1.9, 2.5, 3.3, 4.1, 4.4, 5.8];

/// Frames in a 2 by 4 grid with an 8 px gutter.
fn contact_sheet(frames: &[Frame]) -> Frame {
    let (tw, th, gap, cols) = (frames[0].width, frames[0].height, 8u32, 2u32);
    let rows = (frames.len() as u32).div_ceil(cols);
    let (w, h) = (cols * tw + (cols + 1) * gap, rows * th + (rows + 1) * gap);
    let mut rgba = vec![255u8; (w * h * 4) as usize];
    for (i, t) in frames.iter().enumerate() {
        let (ox, oy) = (gap + (i as u32 % cols) * (tw + gap), gap + (i as u32 / cols) * (th + gap));
        for y in 0..th {
            let src = &t.rgba[(y * tw * 4) as usize..((y + 1) * tw * 4) as usize];
            let at = (((oy + y) * w + ox) * 4) as usize;
            rgba[at..at + src.len()].copy_from_slice(src);
        }
    }
    Frame { width: w, height: h, rgba }
}

/// GPU time per frame over the whole demo at 600x300 and 1920x1080.
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
        label: Some("text-flow"),
        required_features: wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
        ..Default::default()
    }))
    .expect("a GPU device");
    let period = queue.get_timestamp_period() as f64;
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let buffer = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
    let resolve = buffer(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buffer(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    for (w, h, scale) in [(600u32, 300u32, 1.0f32), (1920, 1080, 3.2)] {
        let mut renderer = Renderer::with_fonts(&device, &queue, fonts.clone());
        let target = Target::new(&device, w, h, vo_render::offscreen::FORMAT);
        let mut scene: Scene<Msg> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
        scene.set_viewport(0.0, Size::new(w as f32 / scale, h as f32 / scale), scale);
        let mut app = Demo { fonts: fonts.clone(), t: 0.0 };
        let mut times = Vec::new();
        let frames = (DURATION * FPS) as u32;
        for i in 0..frames {
            let t = i as f64 / FPS;
            app.t = t;
            scene.apply(app.view(&vo::Tokens::default()), t);
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
            "text_flow_demo bench: {w}x{h}: GPU {:.3} ms median, {:.3} ms p95 per frame",
            times[times.len() / 2],
            times[times.len() * 95 / 100]
        );
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut check) = (PathBuf::from("/var/tmp/claude/fw-flow"), false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--check" => check = true,
            "--bench" => return bench(),
            // Read by the host: open on the real desktop on purpose.
            "--real-display" => {}
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = fonts();
    let options = || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };
    let scenario = Scenario { size: SURFACE, scale: 2.0, duration: DURATION, inputs: Vec::new() };
    std::fs::create_dir_all(&out).expect("create the output directory");
    let mp4 = out.join("text_flow_demo.mp4");
    let spec = Export { fps: FPS, duration: DURATION, png_dir: None, video: Some(Video::h264(&mp4)) };
    let sheet_frames: Vec<u64> = SHEET_AT.iter().map(|t| (t * FPS).round() as u64).collect();
    let mut picked = Vec::new();
    let app = || Demo { fonts: fonts.clone(), t: 0.0 };
    let report = vo::export_with(app(), &scenario, &spec, options(), |n, f| {
        if sheet_frames.contains(&n) {
            picked.push(f.clone());
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("text_flow_demo: {e}");
        std::process::exit(1);
    });
    println!("text_flow_demo: {} frames, {}", report.frames, mp4.display());
    let sheet = contact_sheet(&picked);
    let sheet_path = out.join("text-flow-sheet.png");
    write_png(&sheet_path, sheet.width, sheet.height, &sheet.rgba).expect("write contact sheet");
    println!("text_flow_demo: contact sheet {}", sheet_path.display());
    if check {
        let again = Export { video: None, ..spec.clone() };
        let second = vo::export(app(), &scenario, &again, options()).expect("second render");
        let differ = report.frame_hashes.iter().zip(&second.frame_hashes).filter(|(a, b)| a != b).count();
        println!("text_flow_demo: second render, {differ} of {} frames differ", report.frames);
        if differ > 0 {
            std::process::exit(1);
        }
    }
}
