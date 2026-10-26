//! Vector animation spike: paths that draw themselves, shape and icon morphs, blob merge, a
//! light head on a panel border, text on a path and a size morph, 12 s at 1280x720 and 60 fps.
//!
//! Every frame is `frame(t)`, a pure function of the time: the timeline (`vo_core::timeline`)
//! says when each part moves and its closed-form springs say how. No scene, no state.
//!
//!   cargo run --release -p vo --example vector_demo -- [--out DIR] [--check] [--bench]
//!
//! Writes `DIR/vector_demo.mp4` (default DIR `/var/tmp/claude/m4-vector`) and
//! `DIR/contact-sheet.png`. `--check` renders twice and compares every frame's hash.
//! `--bench` times every frame on the GPU with timestamp queries and prints the cold cost of
//! the vector pipeline and of the first path fill.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

use vo::scene::display::{DisplayList, GlyphInstance, Item, ItemKind, Layer, Paint, ShapeInstance, NO_WORD};
use vo::scene::element::TextStyle;
use vo::scene::geom::{Affine, Color, Point, RRect, Rect, Size};
use vo::scene::path::kurbo::{self, Shape};
use vo::scene::path::{fill_glyph, icons, push_blob, push_stroke, Contour, Morph, Outline, StrokeStyle};
use vo::scene::text::TextLayout;
use vo::{FontRegistry, ShapedLayout};
use vo_core::spring::Spring;
use vo_core::timeline::{all, clip, delay, stagger, Schedule, Timeline};
use vo_core::track::CubicBezier;
use vo_render::offscreen::{write_png, Frame};
use vo_render::{Offscreen, Renderer, Target};

const W: f32 = 1280.0;
const H: f32 = 720.0;
const FPS: f64 = 60.0;
const DURATION: f64 = 12.0;

fn bg() -> Color {
    Color::hex(0x0e1015, 1.0)
}
fn panel_fill() -> Color {
    Color::hex(0x1a1d24, 1.0)
}
fn ink() -> Color {
    Color::hex(0xe8eaf0, 1.0)
}
fn accent() -> Color {
    Color::hex(0x4ade80, 1.0)
}
fn warm() -> Color {
    Color::hex(0xf87171, 1.0)
}
fn cool() -> Color {
    Color::hex(0x7dd3fc, 1.0)
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let l = |x: f32, y: f32| x + (y - x) * t;
    Color::rgba(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b), l(a.a, b.a))
}

/// When each part moves.
fn timeline() -> Timeline {
    all([
        // Act 1: three icons draw themselves, then fill.
        delay(0.3, stagger(0.3, 0.6, ["draw.0", "draw.1", "draw.2"].map(|n| clip(n, 1.2)))),
        delay(2.0, clip("fill", 0.5)),
        delay(2.6, clip("act1.out", 0.4)),
        // Act 2: dot, capsule, panel; a light head runs the border; two dots merge.
        delay(3.0, clip("dot", 0.3)),
        delay(3.35, clip("capsule", 0.5)),
        delay(3.9, clip("panel", 0.8)),
        delay(4.3, clip("head", 2.0)),
        delay(4.4, clip("label", 0.4)),
        delay(3.3, clip("merge.in", 1.2)),
        delay(4.9, clip("merge.out", 1.0)),
        delay(6.0, clip("act2.out", 0.4)),
        // Act 3: mic to check to stop.
        delay(6.4, clip("icon.in", 0.4)),
        delay(7.0, clip("morph.1", 1.1)),
        delay(8.1, clip("morph.2", 0.9)),
        delay(9.0, clip("act3.out", 0.3)),
        // Act 4: a path draws on, text runs along it; a word shrinks.
        delay(9.3, clip("curve", 0.8)),
        delay(9.5, clip("ride", 2.2)),
        delay(9.6, clip("word.in", 0.4)),
        delay(10.6, clip("shrink", 0.8)),
        delay(11.6, clip("act4.out", 0.4)),
        clip("all", DURATION),
    ])
}

/// A layer tree built in paint order: `begin` opens a layer, `end` closes it.
struct Builder {
    list: DisplayList,
    stack: Vec<(u32, Vec<Item>, Affine)>,
}

impl Builder {
    fn new() -> Builder {
        let mut b =
            Builder { list: DisplayList { size: Size::new(W, H), scale: 1.0, ..Default::default() }, stack: Vec::new() };
        b.begin(Affine::IDENTITY, 1.0);
        b
    }

    /// Opens a layer with `m` relative to its parent and group opacity `opacity`.
    fn begin(&mut self, m: Affine, opacity: f32) {
        let (parent, pm) = self.stack.last().map_or((None, Affine::IDENTITY), |s| (Some(s.0), s.2));
        let transform = m.then(&pm);
        let idx = self.list.layers.len() as u32;
        self.list.layers.push(Layer { parent, transform, opacity, ..Layer::root() });
        if let Some(top) = self.stack.last_mut() {
            top.1.push(Item { kind: ItemKind::Layer, start: idx, len: 1 });
        }
        self.stack.push((idx, Vec::new(), transform));
    }

    fn end(&mut self) {
        let (idx, items, _) = self.stack.pop().expect("open layer");
        let start = self.list.items.len() as u32;
        self.list.items.extend(items);
        self.list.layers[idx as usize].items = start..self.list.items.len() as u32;
    }

    fn item(&mut self, item: Option<Item>) {
        self.stack.last_mut().expect("open layer").1.extend(item);
    }

    fn shape(&mut self, s: ShapeInstance) {
        let item = Item { kind: ItemKind::Shapes, start: self.list.shapes.len() as u32, len: 1 };
        self.list.shapes.push(s);
        self.item(Some(item));
    }

    fn glyph(&mut self, g: GlyphInstance) {
        let item = Item { kind: ItemKind::Glyphs, start: self.list.glyphs.len() as u32, len: 1 };
        self.list.glyphs.push(g);
        self.item(Some(item));
    }

    fn stroke(&mut self, c: &Contour, style: &StrokeStyle) {
        let item = push_stroke(&mut self.list, c, style);
        self.item(item);
    }

    fn outline(&mut self, o: &Outline, style: &StrokeStyle) {
        for c in &o.contours {
            self.stroke(c, style);
        }
    }

    fn finish(mut self) -> DisplayList {
        while !self.stack.is_empty() {
            self.end();
        }
        self.list
    }
}

/// Everything built once: flattened icons, morphs, the registered fill paths, shaped text.
struct Assets {
    sched: Schedule,
    icons: [Outline; 3],
    fills: [u32; 3],
    morph1: Morph,
    morph2: Morph,
    curve: Outline,
    ride: Vec<(u32, f32, f32)>,
    ride_width: f32,
    word: Vec<(u32, f32)>,
    word_width: f32,
    font: u32,
}

const REF_SIZE: f32 = 100.0;

fn shape_line(layout: &ShapedLayout, font: u32, text: &str) -> (Vec<(u32, f32)>, f32) {
    let shaped = layout.layout(text, &TextStyle::new(font, REF_SIZE, REF_SIZE * 1.3), None);
    (shaped.glyphs.iter().map(|g| (g.glyph, g.pos.x)).collect(), shaped.size.w)
}

impl Assets {
    fn new(fonts: &FontRegistry) -> Assets {
        let font = fonts
            .add_system("Noto Sans")
            .or_else(|_| fonts.add_file(&vo::scene::font::test_font_path()))
            .expect("a font");
        let layout = ShapedLayout::new(fonts.clone());
        let set = [icons::mic(), icons::check(), icons::stop()];
        let fills = [0, 1, 2].map(|i| fonts.add_path(icons::outline(&set[i], 2.0)));
        let flat = |p: &kurbo::BezPath| Outline::new(p, 0.005);
        let icons = [flat(&set[0]), flat(&set[1]), flat(&set[2])];
        let morph1 = Morph::new(&icons[0], &icons[1], 128);
        let morph2 = Morph::new(&icons[1], &icons[2], 128);
        let mut curve = kurbo::BezPath::new();
        curve.move_to((140.0, 360.0));
        curve.curve_to((420.0, 120.0), (760.0, 600.0), (1140.0, 330.0));
        let curve = Outline::new(&curve, 0.05);
        let (glyphs, ride_width) = shape_line(&layout, font, "time is an input to every frame");
        // Each glyph's centre along the line, from the next glyph's pen position.
        let ride = glyphs
            .iter()
            .enumerate()
            .map(|(i, &(g, x))| {
                let next = glyphs.get(i + 1).map_or(ride_width, |n| n.1);
                (g, x, 0.5 * (next - x))
            })
            .collect();
        let (word, word_width) = shape_line(&layout, font, "Dictation");
        Assets {
            sched: timeline().schedule(),
            icons,
            fills,
            morph1,
            morph2,
            curve,
            ride,
            ride_width,
            word,
            word_width,
            font,
        }
    }
}

/// The frame at `t`.
fn frame(a: &Assets, t: f64) -> DisplayList {
    let s = &a.sched;
    let mut b = Builder::new();
    b.shape(ShapeInstance::rrect(RRect::new(Rect::new(0.0, 0.0, W, H), 0.0), Paint::solid(bg())));
    let out = |name: &str| 1.0 - s.eased(name, t, CubicBezier::IN) as f32;
    if t < 3.0 {
        act_icons(a, t, out("act1.out"), &mut b);
    } else if t < 6.4 {
        act_panel(a, t, out("act2.out"), &mut b);
    } else if t < 9.3 {
        act_morph(a, t, out("act3.out"), &mut b);
    } else {
        act_text(a, t, out("act4.out"), &mut b);
    }
    b.finish()
}

/// A pen tip: the last `len` units of `o` up to `head`, brighter and glowing.
fn pen_tip(b: &mut Builder, o: &Outline, head: f32, len: f32, width: f32, color: Color) {
    let style = StrokeStyle::solid(width, color).ramp(head - len, color.fade(0.0), head, Color::WHITE).glow(width);
    b.outline(&o.trim(head - len, head), &style);
}

fn act_icons(a: &Assets, t: f64, fade: f32, b: &mut Builder) {
    let s = &a.sched;
    let k = 5.0;
    let draw = Spring::critical_to_98(1.0);
    let fill = s.eased("fill", t, CubicBezier::OUT) as f32;
    b.begin(Affine::IDENTITY, fade);
    for i in 0..3 {
        let at = Affine::scale(k).then(&Affine::translate(640.0 - 60.0 + (i as f32 - 1.0) * 220.0, 300.0));
        let o = a.icons[i].transform(&at);
        let f = s.spring(&format!("draw.{i}"), t, draw, 0.0, 1.0) as f32;
        let head = f * o.length();
        let color = [ink(), accent(), warm()][i];
        b.outline(&o.trim(0.0, head), &StrokeStyle::solid(10.0, color.fade(1.0 - fill)));
        if f < 0.995 && f > 0.0 {
            pen_tip(b, &o, head, 40.0, 10.0, color);
        }
        if fill > 0.0 {
            let pos = at.apply(Point::ZERO);
            b.glyph(fill_glyph(a.fills[i], pos, k, color.fade(fill)));
        }
    }
    b.end();
}

fn act_panel(a: &Assets, t: f64, fade: f32, b: &mut Builder) {
    let s = &a.sched;
    let pop = |n: &str| s.spring(n, t, Spring::POP, 0.0, 1.0) as f32;
    let (dot, cap, pan) = (pop("dot"), pop("capsule"), pop("panel"));
    b.begin(Affine::IDENTITY, fade);
    // Dot to capsule to panel: size and radius on springs, one rounded rect.
    let w = (18.0 + 182.0 * cap + 400.0 * pan) * dot;
    let h = (18.0 + 102.0 * pan) * dot;
    let r = (0.5 * h) + (22.0 - 0.5 * h) * pan.clamp(0.0, 1.0);
    let (cx, cy) = (640.0, 290.0);
    let rect = RRect::new(Rect::new(cx - 0.5 * w, cy - 0.5 * h, w, h), r.max(0.0));
    let fill = mix(accent(), panel_fill(), cap);
    b.shape(ShapeInstance::rrect(rect, Paint::solid(fill)).with_hairline(Paint::solid(Color::hex(0x3a3f4b, pan.clamp(0.0, 1.0)))));
    // The light head: a window on the border, wrapping the seam, fading tail and glow.
    if s.active("head", t) || s.progress("head", t) >= 1.0 {
        let border = Outline::new(
            &kurbo::RoundedRect::new(
                rect.rect.x as f64,
                rect.rect.y as f64,
                rect.rect.right() as f64,
                rect.rect.bottom() as f64,
                rect.radii[0] as f64,
            )
            .to_path(0.01),
            0.05,
        );
        let total = border.length();
        let lt = s.local("head", t) as f32;
        let speed = 900.0;
        let fade_in = (lt / 0.3).clamp(0.0, 1.0);
        for (phase, color) in [(0.0, cool()), (0.5, accent())] {
            let head = lt * speed + phase * total;
            let tail = 260.0;
            let style = StrokeStyle::solid(2.5, color)
                .ramp(head - tail, color.fade(0.0), head, mix(color, Color::WHITE, 0.6).fade(fade_in))
                .glow(7.0);
            b.stroke(&border.window(0, head, tail), &style);
        }
    }
    // Label in the panel.
    let lab = s.eased("label", t, CubicBezier::OUT) as f32;
    if lab > 0.0 {
        let size = 34.0;
        let k = size / REF_SIZE;
        let x0 = cx - 0.5 * a.word_width * k;
        for &(g, x) in &a.word {
            b.glyph(GlyphInstance {
                pos: Point::new(x0 + x * k, cy + 12.0 + 8.0 * (1.0 - lab)),
                size,
                font: a.font,
                glyph: g,
                word: NO_WORD,
                color: ink().fade(lab),
            });
        }
    }
    // Two dots that merge and split again (smooth-min union).
    let d = 140.0 * (1.0 - pop("merge.in")) + 140.0 * pop("merge.out");
    let (ya, ra, rb) = (540.0, 26.0, 18.0);
    let grow = s.spring("merge.in", t + 0.3, Spring::POP, 0.0, 1.0) as f32;
    if grow > 0.0 {
        let item = push_blob(
            &mut b.list,
            (Point::new(640.0 - d, ya), ra * grow),
            (Point::new(640.0 + d, ya), rb * grow),
            34.0,
            cool(),
            Color::hex(0xc084fc, 1.0),
        );
        b.item(Some(item));
    }
    b.end();
}

fn act_morph(a: &Assets, t: f64, fade: f32, b: &mut Builder) {
    let s = &a.sched;
    let inn = s.spring("icon.in", t, Spring::POP, 0.0, 1.0) as f32;
    let m1 = s.spring("morph.1", t, Spring::from_frequency(11.0, 0.62), 0.0, 1.0) as f32;
    let m2 = s.spring("morph.2", t, Spring::from_frequency(11.0, 0.62), 0.0, 1.0) as f32;
    let k = 10.0 * (0.6 + 0.4 * inn);
    let at = Affine::translate(-12.0, -12.0).then(&Affine::scale(k)).then(&Affine::translate(640.0, 330.0));
    let (parts, color) = if s.local("morph.2", t) <= 0.0 {
        (a.morph1.sample(m1), mix(ink(), accent(), m1))
    } else {
        (a.morph2.sample(m2), mix(accent(), warm(), m2))
    };
    b.begin(Affine::IDENTITY, fade * inn.clamp(0.0, 1.0));
    for p in parts {
        let style = StrokeStyle::solid(2.2 * k * p.weight, color.fade(p.weight.sqrt())).glow(3.0 * p.weight);
        b.stroke(&p.contour.transform(&at), &style);
    }
    b.end();
}

fn act_text(a: &Assets, t: f64, fade: f32, b: &mut Builder) {
    let s = &a.sched;
    b.begin(Affine::IDENTITY, fade);
    // The curve draws on.
    let total = a.curve.length();
    let f = s.spring("curve", t, Spring::critical_to_98(0.8), 0.0, 1.0) as f32;
    b.outline(&a.curve.trim(0.0, f * total), &StrokeStyle::solid(1.5, Color::hex(0x3a3f4b, 1.0)));
    // Text rides along it: each glyph on its own layer, rotated to the tangent.
    let size = 40.0;
    let k = size / REF_SIZE;
    let ride = s.eased("ride", t, CubicBezier::new(0.3, 0.0, 0.2, 1.0)) as f32;
    let start = -a.ride_width * k + ride * (0.5 * (total + a.ride_width * k));
    for &(g, x, half) in &a.ride {
        let along = start + (x + half) * k;
        if along < 0.0 || along > f * total {
            continue;
        }
        let Some((p, tan)) = a.curve.contours[0].at(along) else { continue };
        let angle = tan.y.atan2(tan.x);
        let edge = (along / 60.0).min((f * total - along) / 60.0).clamp(0.0, 1.0);
        b.begin(Affine::rotate(angle).then(&Affine::translate(p.x, p.y)), edge);
        b.glyph(GlyphInstance {
            pos: Point::new(-half * k, -10.0),
            size,
            font: a.font,
            glyph: g,
            word: NO_WORD,
            color: mix(cool(), ink(), (x / a.ride_width).clamp(0.0, 1.0)),
        });
        b.end();
    }
    // Size morph: the word scales from 72 to 44 on a spring, instances only.
    let inn = s.eased("word.in", t, CubicBezier::OUT) as f32;
    let size = s.spring("shrink", t, Spring::POP, 72.0, 44.0) as f32;
    let k = size / REF_SIZE;
    let x0 = 640.0 - 0.5 * a.word_width * k;
    for &(g, x) in &a.word {
        b.glyph(GlyphInstance {
            pos: Point::new(x0 + x * k, 610.0),
            size,
            font: a.font,
            glyph: g,
            word: NO_WORD,
            color: ink().fade(inn),
        });
    }
    b.end();
}

struct Args {
    out: PathBuf,
    check: bool,
    bench: bool,
}

fn args() -> Args {
    let mut a = Args { out: PathBuf::from("/var/tmp/claude/m4-vector"), check: false, bench: false };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--out" => a.out = it.next().expect("--out DIR").into(),
            "--check" => a.check = true,
            "--bench" => a.bench = true,
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

fn device(timestamps: bool) -> (wgpu::Device, wgpu::Queue) {
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
    let features = if timestamps {
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS
    } else {
        wgpu::Features::empty()
    };
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("vector-demo"),
        required_features: features,
        ..Default::default()
    }))
    .expect("a GPU device")
}

fn frame_count() -> u64 {
    (DURATION * FPS).round() as u64
}

/// Renders every frame; returns the hashes. Feeds `video` and the contact sheet when given.
fn render_all(a: &Assets, fonts: &FontRegistry, mut video: Option<&mut dyn Write>, sheet: &mut Vec<Frame>) -> Vec<u64> {
    let (device, queue) = device(false);
    let mut off = Offscreen::with_fonts(&device, &queue, fonts.clone());
    let n = frame_count();
    let picks: Vec<u64> = (0..16).map(|i| ((i as f64 + 0.5) / 16.0 * n as f64) as u64).collect();
    let mut hashes = Vec::with_capacity(n as usize);
    let mut seen = 0u64;
    let mut take = |f: Frame, hashes: &mut Vec<u64>, video: &mut Option<&mut dyn Write>| {
        hashes.push(vo::frame_hash(&f.rgba));
        if let Some(v) = video.as_mut() {
            v.write_all(&f.rgba).expect("ffmpeg stdin");
        }
        if picks.contains(&seen) {
            sheet.push(f);
        }
        seen += 1;
    };
    for i in 0..n {
        let list = frame(a, i as f64 / FPS);
        if let Some(f) = off.submit(&list, [W as u32, H as u32], 1.0) {
            take(f, &mut hashes, &mut video);
        }
    }
    for f in off.finish() {
        take(f, &mut hashes, &mut video);
    }
    hashes
}

/// A 4 x 4 grid of frames at half size, composited over black.
fn write_sheet(path: &std::path::Path, frames: &[Frame]) {
    let (fw, fh) = (W as usize / 2, H as usize / 2);
    let (cols, rows) = (4, frames.len().div_ceil(4));
    let (sw, sh) = (cols * fw + (cols + 1) * 8, rows * fh + (rows + 1) * 8);
    let mut px = vec![0u8; sw * sh * 4];
    for p in px.chunks_exact_mut(4) {
        p.copy_from_slice(&[40, 40, 44, 255]);
    }
    for (i, f) in frames.iter().enumerate() {
        let (ox, oy) = (8 + (i % cols) * (fw + 8), 8 + (i / cols) * (fh + 8));
        for y in 0..fh {
            for x in 0..fw {
                let mut acc = [0u32; 3];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let s = ((2 * y + dy) * f.width as usize + 2 * x + dx) * 4;
                    for c in 0..3 {
                        acc[c] += f.rgba[s + c] as u32;
                    }
                }
                let d = ((oy + y) * sw + ox + x) * 4;
                px[d..d + 4].copy_from_slice(&[(acc[0] / 4) as u8, (acc[1] / 4) as u8, (acc[2] / 4) as u8, 255]);
            }
        }
    }
    write_png(path, sw as u32, sh as u32, &px).expect("write contact sheet");
}

/// GPU time per frame from timestamp queries, and the cold cost of the new pipelines.
fn bench(a: &Assets, fonts: &FontRegistry) {
    let (device, queue) = device(true);
    let mut r = Renderer::with_fonts(&device, &queue, fonts.clone());
    let mut target = Target::new(&device, W as u32, H as u32, vo_render::offscreen::FORMAT);
    let wait = |d: &wgpu::Device| d.poll(wgpu::PollType::wait_indefinitely()).expect("poll");

    // Cold costs: shapes first (warms the shape pipeline), then the first frame with vectors,
    // then the first path fill, each timed on the CPU including the GPU wait.
    let mut base = Builder::new();
    base.shape(ShapeInstance::rrect(RRect::new(Rect::new(0.0, 0.0, W, H), 0.0), Paint::solid(bg())));
    let base = base.finish();
    r.render(&base, &mut target);
    wait(&device);
    let timed = |r: &mut Renderer, list: &DisplayList, target: &mut Target| {
        let t0 = Instant::now();
        r.render(list, target);
        wait(&device);
        t0.elapsed().as_secs_f64() * 1e3
    };
    let vec_frame = frame(a, 8.5);
    let cold_vec = timed(&mut r, &vec_frame, &mut target);
    let warm_vec = timed(&mut r, &vec_frame, &mut target);
    let fill_frame = frame(a, 2.4);
    let cold_fill = timed(&mut r, &fill_frame, &mut target);
    let warm_fill = timed(&mut r, &fill_frame, &mut target);
    println!("cold first frame with vectors: {cold_vec:.2} ms (warm {warm_vec:.2} ms)");
    println!("cold first frame with three path fills (glyph pipeline, path encode): {cold_fill:.2} ms (warm {warm_fill:.2} ms)");

    let qs = device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let period = queue.get_timestamp_period() as f64;
    let mut per_act: [Vec<f64>; 4] = Default::default();
    let mut cpu_build = Vec::new();
    for i in 0..frame_count() {
        let t = i as f64 / FPS;
        let c0 = Instant::now();
        let list = frame(a, t);
        cpu_build.push(c0.elapsed().as_secs_f64() * 1e3);
        let mut enc = device.create_command_encoder(&Default::default());
        enc.write_timestamp(&qs, 0);
        r.encode(&list, 1.0, &target, &mut enc);
        enc.write_timestamp(&qs, 1);
        enc.resolve_query_set(&qs, 0..2, &resolve, 0);
        enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, 16);
        queue.submit([enc.finish()]);
        read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        wait(&device);
        let ts: [u64; 2] = bytemuck_read(&read.slice(..).get_mapped_range().expect("mapped"));
        read.unmap();
        let ms = (ts[1].wrapping_sub(ts[0])) as f64 * period / 1e6;
        let act = if t < 3.0 { 0 } else if t < 6.4 { 1 } else if t < 9.3 { 2 } else { 3 };
        per_act[act].push(ms);
    }
    let stats = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        (mean, v[v.len() / 2], v[(v.len() * 95) / 100], v[v.len() - 1])
    };
    for (name, v) in ["icons draw-on", "panel, head, blobs", "icon morph", "text on path"].iter().zip(per_act.iter_mut()) {
        let (mean, med, p95, max) = stats(v);
        println!("GPU per frame, {name}: mean {mean:.3} ms, median {med:.3}, p95 {p95:.3}, max {max:.3} (1280x720)");
    }
    let (mean, _, p95, max) = stats(&mut cpu_build);
    println!("CPU frame(t) build (trim, morph, display list): mean {mean:.3} ms, p95 {p95:.3}, max {max:.3}");
    let _ = target.width();
}

fn bytemuck_read(b: &[u8]) -> [u64; 2] {
    [u64::from_le_bytes(b[0..8].try_into().unwrap()), u64::from_le_bytes(b[8..16].try_into().unwrap())]
}

fn main() {
    let args = args();
    let fonts = FontRegistry::new();
    let assets = Assets::new(&fonts);
    if args.bench {
        bench(&assets, &fonts);
        return;
    }
    std::fs::create_dir_all(&args.out).expect("out dir");
    let mp4 = args.out.join("vector_demo.mp4");
    let ffmpeg = std::env::var_os("VO_FFMPEG").unwrap_or_else(|| "ffmpeg".into());
    let mut child = Command::new(ffmpeg)
        .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s"])
        .arg(format!("{}x{}", W as u32, H as u32))
        .args(["-r", &FPS.to_string(), "-i", "-"])
        .args("-c:v libx264 -preset medium -crf 16 -pix_fmt yuv420p -movflags +faststart".split(' '))
        .arg(&mp4)
        .stdin(Stdio::piped())
        .spawn()
        .expect("ffmpeg (install it or set VO_FFMPEG)");
    let t0 = Instant::now();
    let mut sheet = Vec::new();
    let mut stdin = child.stdin.take().unwrap();
    let hashes = render_all(&assets, &fonts, Some(&mut stdin), &mut sheet);
    drop(stdin);
    assert!(child.wait().expect("ffmpeg").success(), "ffmpeg failed");
    let secs = t0.elapsed().as_secs_f64();
    write_sheet(&args.out.join("contact-sheet.png"), &sheet);
    println!("{} frames in {secs:.2} s -> {}", hashes.len(), mp4.display());
    println!("contact sheet: {}", args.out.join("contact-sheet.png").display());
    if args.check {
        let again = render_all(&assets, &fonts, None, &mut Vec::new());
        let same = hashes == again;
        println!("determinism: second render {}", if same { "matches every frame" } else { "DIFFERS" });
        assert!(same);
    }
}
