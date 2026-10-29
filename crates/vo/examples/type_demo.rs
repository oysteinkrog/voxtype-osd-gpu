//! Type demo, three pages.
//!
//! - `fonts` (WP7, FX-003, FX-004): a mecha-style header in Barlow Condensed Bold whose kanji
//!   fall back to the Japanese label face, a body line with Norwegian letters, then one line
//!   in every built-in font.
//! - `fx` (WP18, FX-013, FX-018): offset text shadows on a light card, and outlined text,
//!   alone and with a glow.
//! - `labels` (FX-005, FX-049, FX-037): vertical side labels in Japanese and Latin, the
//!   same box autofitting short, medium and long text, and live text with a gradient caret.
//! - `turn` (EG-15): letters turned, leaned and moved one by one in a single glyph draw: a
//!   line on a wave, a word tumbling in, leaning digits and a ring of letters.
//!
//!   cargo run --release -p vo --example type_demo -- [--out DIR] [--bench]
//!
//! Writes `DIR/type-demo.png`, `DIR/type-fx.png`, `DIR/type-labels.png` and `DIR/type-turn.png` (default DIR
//! `/var/tmp/claude/fw-fonts`).
//! `--bench` times each page's frame on the GPU at 600x300 and 1920x1080.

use std::path::PathBuf;

use vo::scene::element::{Element, TextStyle};
use vo::scene::flow::Caret;
use vo::scene::geom::{Color, Size};
use vo::scene::text::TextLayout;
use vo::scene::{DisplayList, Scene};
use vo::{FontRegistry, ShapedLayout};
use vo_core::words::Word;
use vo_render::offscreen::write_png;
use vo_render::{Offscreen, Renderer, Target};

const SURFACE: Size = Size::new(600.0, 300.0);

const SPECIMEN: [(&str, &str); 10] = [
    ("sans", "sans: Noto Sans, the quick brown fox"),
    ("serif", "serif: Noto Serif, the quick brown fox"),
    ("mono", "mono: JetBrains Mono 0123456789"),
    ("grotesk", "grotesk: Inter, the quick brown fox"),
    ("condensed", "CONDENSED: BARLOW CONDENSED MEDIUM"),
    ("condensed-bold", "CONDENSED-BOLD: BARLOW CONDENSED BOLD"),
    ("extra-condensed-bold", "EXTRA-CONDENSED-BOLD: SAIRA EXTRACONDENSED EXTRABOLD"),
    ("thin", "thin: Barlow Thin, the quick brown fox"),
    ("jp-labels", "jp-labels: 音声入力 テキストに変換中"),
    ("jp-labels-bold", "jp-labels-bold: 作戦継続中 未来の都市"),
];

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.set_default_fallbacks(&[fonts.asset("sans").unwrap()]);
    for (name, _) in SPECIMEN {
        fonts.asset(name).unwrap_or_else(|e| panic!("{e}"));
    }
    let jp = fonts.asset("jp-labels-bold").unwrap();
    for name in ["condensed", "condensed-bold", "extra-condensed-bold"] {
        fonts.set_fallbacks(fonts.asset(name).unwrap(), &[jp]);
    }
    fonts
}

#[derive(Clone, Copy, Debug)]
enum Page {
    Fonts,
    Fx,
    /// Parts of `Fx`, for the bench: the shadow card, the plain outline.
    Shadow,
    Outline,
    Labels,
    Turn,
}

fn view(fonts: &FontRegistry, page: Page) -> Element<()> {
    match page {
        Page::Fonts => fonts_page(fonts),
        Page::Labels => labels_page(fonts),
        Page::Turn => turn_page(fonts),
        fx => fx_page(fonts, fx),
    }
}

fn fx_page(fonts: &FontRegistry, page: Page) -> Element<()> {
    let show = |part: Page| matches!(page, Page::Fx) || std::mem::discriminant(&page) == std::mem::discriminant(&part);
    let style = |name: &str, size: f32| TextStyle::new(fonts.asset(name).unwrap(), size, size * 1.2);
    let ink = Color::hex(0x1d1a16, 1.0);
    let card =
        Element::panel("card")
            .pad(12.0)
            .gap(2.0)
            .radius(10.0)
            .fill(Color::hex(0xece6da, 1.0))
            .child(
                Element::label("soft", "so the plan for tomorrow")
                    .text_style(style("serif", 26.0))
                    .color(ink)
                    .text_shadow(Color::hex(0x000000, 0.4), 0.0, 2.0, 6.0),
            )
            .child(
                Element::label("hard", "is to finish the office mirror")
                    .text_style(style("serif", 26.0))
                    .color(Color::hex(0xc0392b, 1.0))
                    .text_shadow(ink.fade(0.85), -1.5, -1.5, 0.0),
            );
    let orange = Color::hex(0xff8a1f, 1.0);
    let mut bg = Element::panel("bg").size(SURFACE.w, SURFACE.h).pad(14.0).gap(10.0).fill(Color::hex(0x0b0806, 1.0));
    if show(Page::Shadow) {
        bg = bg.child(card);
    }
    if show(Page::Outline) {
        bg = bg.child(
            Element::label("outline", "VOICE DICTATION")
                .text_style(style("condensed-bold", 46.0))
                .color(orange)
                .text_outline(1.5),
        );
    }
    if show(Page::Fx) {
        bg = bg.child(
            Element::label("neon", "LISTENING")
                .text_style(style("condensed-bold", 46.0))
                .color(Color::hex(0x9ff0ff, 1.0))
                .text_outline(2.0)
                .text_halo(Color::hex(0x2fb8ff, 0.7), 8.0),
        );
    }
    bg
}

fn labels_page(fonts: &FontRegistry) -> Element<()> {
    let orange = Color::hex(0xff8a1f, 1.0);
    let style = |name: &str, size: f32| TextStyle::new(fonts.asset(name).unwrap(), size, size * 1.15);
    let side = |key: &'static str, text: &str, name: &str| {
        Element::label(key, text).text_style(style(name, 18.0).vertical()).color(orange)
    };
    let fit = |key: &'static str, text: &str| {
        Element::panel(key).fill(Color::hex(0x2a1a0c, 1.0)).radius(4.0).child(
            Element::label("text", text)
                .text_style(style("extra-condensed-bold", 40.0).fit(10.0, 2.0))
                .size(150.0, 92.0)
                .color(orange),
        )
    };
    let words: Vec<Word> = ["live", "text", "with", "a", "gradient", "caret"]
        .iter()
        .enumerate()
        .map(|(i, s)| Word { id: i as u32 + 1, text: (*s).into() })
        .collect();
    let live = Element::live_text("live", &words)
        .text_style(style("grotesk", 22.0))
        .width(470.0)
        .color(Color::hex(0xe8ebf0, 1.0))
        .caret(Caret {
            width: 3.0,
            color: Some(Color::hex(0x9ff0ff, 1.0)),
            gradient: Some(Color::hex(0xff3fa4, 1.0)),
            ..Caret::default()
        });
    let boxes = Element::row("fits")
        .gap(8.0)
        .child(fit("short", "REC"))
        .child(fit("medium", "LISTENING NOW"))
        .child(fit("long", "TRANSCRIBING THE LAST THIRTY SECONDS"));
    let body = Element::column("body").pad(14.0).gap(14.0).child(boxes).child(live);
    Element::panel("bg")
        .direction_row()
        .size(SURFACE.w, SURFACE.h)
        .pad(14.0)
        .gap(10.0)
        .fill(Color::hex(0x0b0806, 1.0))
        .child(side("jp", "音声入力録音中", "jp-labels-bold"))
        .child(side("latin", "VOICE", "extra-condensed-bold"))
        .child(body)
}

fn fonts_page(fonts: &FontRegistry) -> Element<()> {
    let orange = Color::hex(0xff8a1f, 1.0);
    let style = |name: &str, size: f32| TextStyle::new(fonts.asset(name).unwrap(), size, size * 1.25);
    let mut col = Element::column("col")
        .pad(14.0)
        .gap(3.0)
        .child(
            Element::label("head", "音声入力 VOICE DICTATION").text_style(style("condensed-bold", 32.0)).color(orange),
        )
        .child(
            Element::label("body", "録音中 LISTENING · Blåbærsyltetøy på Ærø")
                .text_style(style("condensed", 20.0))
                .color(orange),
        );
    for (name, line) in SPECIMEN {
        col = col.child(Element::label(name, line).text_style(style(name, 14.0)).color(Color::hex(0xe8ebf0, 1.0)));
    }
    Element::panel("bg").size(SURFACE.w, SURFACE.h).fill(Color::hex(0x0b0806, 1.0)).child(col)
}

/// Per-glyph transforms (EG-15), drawn with the glyphs of shaped lines.
fn turn_page(fonts: &FontRegistry) -> Element<()> {
    use vo::scene::display::{GlyphInstance, GlyphXform, NO_WORD};
    use vo::scene::geom::Point;
    let sans = fonts.asset("sans").unwrap();
    let shape = |text: &str, size: f32| {
        ShapedLayout::new(fonts.clone()).layout(text, &TextStyle::new(sans, size, size * 1.25), None)
    };
    let wave = shape("letters riding a wave, each one turned to the slope", 18.0);
    let tumble = shape("tumbling in", 26.0);
    let digits = shape("0123456789", 24.0);
    let ring = shape("ROUND AND ROUND · ", 15.0);
    let ink = Color::hex(0xe8ebf0, 1.0);
    let accent = Color::hex(0x7dd3fc, 1.0);
    let canvas = Element::canvas("turn", f64::INFINITY, move |_, rect, p| {
        let mut put = |g: &vo::scene::text::PlacedGlyph, pos: Point, size: f32, color: Color, xform: GlyphXform| {
            p.glyph(GlyphInstance { pos, size, font: g.font, glyph: g.glyph, word: NO_WORD, color, xform });
        };
        // A line on a wave: each letter rides the curve and turns to its slope.
        let (x0, y0) = (rect.x + 20.0, rect.y + 46.0);
        for g in &wave.glyphs {
            let x = g.pos.x;
            let lift = (x * 0.035).sin() * 10.0;
            let slope = (x * 0.035).cos() * 0.35;
            let xf = GlyphXform {
                rot_deg: -slope.atan().to_degrees(),
                offset: Point::new(0.0, -lift),
                ..GlyphXform::IDENTITY
            };
            put(g, Point::new(x0 + x, y0), 18.0, ink, xf);
        }
        // A word tumbling in: the later the letter, the further it still has to turn and fall.
        let (x0, y0) = (rect.x + 20.0, rect.y + 130.0);
        let n = tumble.glyphs.len().max(1) as f32;
        for (i, g) in tumble.glyphs.iter().enumerate() {
            let left = i as f32 / n;
            let xf = GlyphXform {
                rot_deg: left * left * 200.0,
                offset: Point::new(left * 30.0, -left * left * 50.0),
                ..GlyphXform::IDENTITY
            };
            put(g, Point::new(x0 + g.pos.x, y0), 26.0, accent.fade(1.0 - 0.6 * left), xf);
        }
        // Leaning digits, from a left lean to an italic.
        let (x0, y0) = (rect.x + 20.0, rect.y + 190.0);
        for (i, g) in digits.glyphs.iter().enumerate() {
            let xf = GlyphXform { skew_deg: -14.0 + i as f32 * 3.5, ..GlyphXform::IDENTITY };
            put(g, Point::new(x0 + g.pos.x * 1.3, y0), 24.0, ink, xf);
        }
        // A ring: each letter placed on a circle and turned to face out.
        let c = Point::new(rect.x + rect.w - 120.0, rect.y + 170.0);
        let r = 80.0;
        let total = ring.size.w.max(1.0);
        for g in &ring.glyphs {
            let a = (g.pos.x + 0.5 * g.advance) / total * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            let at = Point::new(c.x + r * a.cos(), c.y + r * a.sin());
            // The pen sits half an advance left of the centre; the turn is about the ink centre.
            let pos = Point::new(at.x - 0.5 * g.advance, at.y + 5.0);
            let xf = GlyphXform::rotate(a.to_degrees() + 90.0);
            put(g, pos, 15.0, accent, xf);
        }
    })
    .size(SURFACE.w, SURFACE.h);
    Element::panel("bg").size(SURFACE.w, SURFACE.h).fill(Color::hex(0x0b0806, 1.0)).child(canvas)
}

fn sample(fonts: &FontRegistry, page: Page, size: Size, scale: f32) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, size, scale);
    scene.apply(view(fonts, page), 0.0);
    scene.sample(10.0)
}

/// A headless Vulkan device with timestamp queries.
fn device() -> (wgpu::Device, wgpu::Queue) {
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
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("type-demo"),
        required_features: wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
        ..Default::default()
    }))
    .expect("a GPU device")
}

/// GPU time per frame at 600x300 and at 1920x1080 (the same layout at 3.2 times the scale).
fn bench() {
    let fonts = fonts();
    let (device, queue) = device();
    let period = queue.get_timestamp_period() as f64;
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let buffer = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
    let resolve = buffer(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buffer(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    for (page, (w, h, scale)) in [Page::Fonts, Page::Fx, Page::Shadow, Page::Outline, Page::Labels, Page::Turn]
        .into_iter()
        .flat_map(|p| [(600u32, 300u32, 1.0f32), (1920, 1080, 3.2)].map(|s| (p, s)))
    {
        let mut renderer = Renderer::with_fonts(&device, &queue, fonts.clone());
        let target = Target::new(&device, w, h, vo_render::offscreen::FORMAT);
        let list = sample(&fonts, page, Size::new(w as f32 / scale, h as f32 / scale), scale);
        let mut times = Vec::new();
        for i in 0..150 {
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
            let ts = {
                let bytes = read.slice(..).get_mapped_range().unwrap();
                let at = |i: usize| u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap());
                (at(0), at(1))
            };
            read.unmap();
            // The first frames fill the glyph atlas.
            if i >= 30 {
                times.push((ts.1 - ts.0) as f64 * period / 1e6);
            }
        }
        times.sort_by(f64::total_cmp);
        println!(
            "type_demo bench: {page:?} {w}x{h}: {} glyphs, GPU {:.3} ms median, {:.3} ms p95 per frame",
            list.glyphs.len(),
            times[times.len() / 2],
            times[times.len() * 95 / 100]
        );
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("/var/tmp/claude/fw-fonts");
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--bench" => return bench(),
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = fonts();
    let scale = 2.0;
    let (device, queue) = device();
    let mut off = Offscreen::with_fonts(&device, &queue, fonts.clone());
    std::fs::create_dir_all(&out).expect("create the output directory");
    for (page, file) in [
        (Page::Fonts, "type-demo.png"),
        (Page::Fx, "type-fx.png"),
        (Page::Labels, "type-labels.png"),
        (Page::Turn, "type-turn.png"),
    ] {
        let list = sample(&fonts, page, SURFACE, scale);
        let (w, h) = ((SURFACE.w * scale) as u32, (SURFACE.h * scale) as u32);
        let rgba = off.render_to_rgba(&list, [w, h], scale);
        let path = out.join(file);
        write_png(&path, w, h, &rgba).expect("write the png");
        println!("{}", path.display());
    }
}
