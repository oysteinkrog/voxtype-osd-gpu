//! The popup's text modes, one per `--mode`, each exported to an mp4 and a contact sheet.
//!
//! - `ticker` (T13): one right-anchored line. The line slides left on a 200 ms spring as each
//!   word arrives, old text fades under a 40 px mask at the left edge, and a leading "..." fades
//!   in once the text overflows.
//!
//!   cargo run --release -p vo --example text_modes -- --mode ticker [--out DIR] [--check]
//!
//! Writes `DIR/<mode>.mp4` and `DIR/<mode>-sheet.png` (default DIR
//! `/var/tmp/claude/text-modes`). `--check` renders again and compares every frame's hash.

use std::path::PathBuf;

use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::taffy::prelude::{AlignItems, JustifyContent};
use vo::scene::{Prop, Transition};
use vo::{App, Cx, Export, FontRegistry, Options, Scenario, ShapedLayout, TextLayout, Video};
use vo_core::spring::Spring;
use vo_core::words::Word;
use vo_render::offscreen::{write_png, Frame};

const FPS: f64 = 60.0;
const SIZE: Size = Size::new(960.0, 240.0);

fn bg() -> Element<Msg> {
    Element::panel("root")
        .size(SIZE.w, SIZE.h)
        .fill_gradient(Color::hex(0x0e1016, 1.0), Color::hex(0x161a24, 1.0))
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
}

fn text_color() -> Color {
    Color::hex(0xe6e9f0, 1.0)
}

fn dim_color() -> Color {
    Color::hex(0x8c93a3, 1.0)
}

/// Messages shared by every mode: a tick that carries its own time.
#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Tick(usize),
}

/// The words a dictation delivers, one every `gap` seconds from `start`.
fn arrivals(text: &str, start: f64, gap: f64) -> Vec<(f64, Word)> {
    text.split(' ')
        .enumerate()
        .map(|(i, w)| (start + i as f64 * gap, Word { id: i as u32 + 1, text: w.to_string() }))
        .collect()
}

// T13: ticker.

const TICKER_TEXT: &str = "so the popup keeps a single line and slides it to the left as each new word \
                           arrives at the right edge while the oldest words fade out under the mask";
const TICKER_W: f32 = 560.0;
/// The window offset's spring: about 200 ms to settle per word.
const TICKER_SPRING: Spring = Spring::critical_to_98(0.2);

struct Ticker {
    words: Vec<(f64, Word)>,
    shown: usize,
    measure: ShapedLayout,
    style: TextStyle,
}

impl Ticker {
    fn new(fonts: &FontRegistry) -> Ticker {
        Ticker {
            words: arrivals(TICKER_TEXT, 0.3, 0.28),
            shown: 0,
            measure: ShapedLayout::new(fonts.clone()),
            style: TextStyle::new(0, 30.0, 40.0),
        }
    }
}

impl App for Ticker {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        for (i, (t, _)) in self.words.iter().enumerate() {
            cx.after(*t, Msg::Tick(i + 1));
        }
    }

    fn update(&mut self, msg: Msg, _: &mut Cx<Msg>) {
        let Msg::Tick(n) = msg;
        self.shown = n;
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        let words: Vec<Word> = self.words[..self.shown].iter().map(|(_, w)| w.clone()).collect();
        let joined = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        // The app measures its own line, with the same shaping the scene uses, to know when
        // it overflows.
        let width = self.measure.layout(&joined, &self.style, None).size.w;
        let overflow = width > TICKER_W;
        let line = Element::live_text("line", &words)
            .text_style(self.style)
            .color(text_color())
            // Its own width, so the line never wraps to fit the window.
            .width(width)
            .shrink(0.0)
            .motion(Prop::Size, Spring::SNAP)
            .word_enter(Transition::fade().offset(0.0, 8.0))
            .motion(Prop::Position, TICKER_SPRING)
            .motion(Prop::Offset, Spring::SNAP)
            .motion(Prop::Opacity, Spring::SNAP);
        // Right-anchored: the column's items sit at its right edge and overflow to the left.
        let window = Element::layer("window")
            .size(TICKER_W, self.style.line_height)
            .align_items(AlignItems::FLEX_END)
            .fade_edges(40.0, 0.0, 0.0, 0.0)
            .child(line);
        let ellipsis = Element::label("more", "...")
            .text_style(self.style)
            .color(dim_color())
            .opacity(if overflow { 1.0 } else { 0.0 })
            .motion(Prop::Opacity, Spring::SOFT);
        let pill = Element::panel("pill")
            .direction_row()
            .align_items(AlignItems::CENTER)
            .pad_xy(28.0, 18.0)
            .gap(6.0)
            .radius(38.0)
            .fill(Color::hex(0x1f2433, 1.0))
            .stroke(Color::hex(0xffffff, 0.08), 1.0)
            .child(
                Element::shape("dot", vo::scene::display::ShapeKind::Circle)
                    .size(12.0, 12.0)
                    .fill(Color::hex(0xff4d4d, 1.0)),
            )
            .child(Element::panel("sp").width(8.0))
            .child(ellipsis)
            .child(window);
        bg().child(pill)
    }
}

// Running a mode.

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let path = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
    fonts
}

/// Thumbnails at half size in a 4-wide grid with an 8 px gutter.
fn contact_sheet(frames: &[Frame]) -> Frame {
    let half = |f: &Frame| {
        let (w, h) = (f.width / 2, f.height / 2);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                for c in 0..4 {
                    let at = |dx: u32, dy: u32| f.rgba[(((2 * y + dy) * f.width + 2 * x + dx) * 4 + c) as usize] as u32;
                    rgba[((y * w + x) * 4 + c) as usize] = ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) / 4) as u8;
                }
            }
        }
        Frame { width: w, height: h, rgba }
    };
    let thumbs: Vec<Frame> = frames.iter().map(half).collect();
    let (tw, th, gap) = (thumbs[0].width, thumbs[0].height, 8u32);
    let cols = (thumbs.len() as u32).min(4);
    let rows = (thumbs.len() as u32).div_ceil(cols);
    let (w, h) = (cols * tw + (cols + 1) * gap, rows * th + (rows + 1) * gap);
    let mut rgba = vec![255u8; (w * h * 4) as usize];
    for (i, t) in thumbs.iter().enumerate() {
        let (ox, oy) = (gap + (i as u32 % cols) * (tw + gap), gap + (i as u32 / cols) * (th + gap));
        for y in 0..th {
            let src = &t.rgba[(y * tw * 4) as usize..((y + 1) * tw * 4) as usize];
            let at = (((oy + y) * w + ox) * 4) as usize;
            rgba[at..at + src.len()].copy_from_slice(src);
        }
    }
    Frame { width: w, height: h, rgba }
}

/// Exports `make()` for `duration` seconds, writes the contact sheet of the frames at
/// `sheet_at`, and with `check` renders again and compares the frame hashes.
fn run<A: App<Msg = Msg>>(
    mode: &str,
    out: &std::path::Path,
    check: bool,
    duration: f64,
    sheet_at: &[f64],
    make: impl Fn() -> A,
) {
    let fonts = fonts();
    let options = || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };
    let scenario = Scenario { size: SIZE, scale: 1.0, duration, inputs: Vec::new() };
    let mp4 = out.join(format!("{mode}.mp4"));
    let spec = Export { fps: FPS, duration, png_dir: None, video: Some(Video::h264(&mp4)) };
    let sheet_frames: Vec<u64> = sheet_at.iter().map(|t| (t * FPS).round() as u64).collect();
    let mut picked = Vec::new();
    let report = vo::export_with(make(), &scenario, &spec, options(), |n, f| {
        if sheet_frames.contains(&n) {
            picked.push(f.clone());
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("text_modes: {e}");
        std::process::exit(1);
    });
    println!("text_modes: {mode}: {} frames, {}", report.frames, mp4.display());
    let sheet = contact_sheet(&picked);
    let sheet_path = out.join(format!("{mode}-sheet.png"));
    write_png(&sheet_path, sheet.width, sheet.height, &sheet.rgba).expect("write contact sheet");
    println!("text_modes: {mode}: contact sheet {} (frames {sheet_frames:?})", sheet_path.display());
    if check {
        let again = Export { video: None, ..spec.clone() };
        let second = vo::export(make(), &scenario, &again, options()).expect("second render");
        let differ = report.frame_hashes.iter().zip(&second.frame_hashes).filter(|(a, b)| a != b).count();
        println!("text_modes: {mode}: second render, {differ} of {} frames differ", report.frames);
        if differ > 0 {
            std::process::exit(1);
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut check, mut mode) = (PathBuf::from("/var/tmp/claude/text-modes"), false, String::from("ticker"));
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--mode" => mode = args.next().expect("--mode needs a name"),
            "--check" => check = true,
            // Read by the host: open on the real desktop on purpose.
            "--real-display" => {}
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = fonts();
    match mode.as_str() {
        "ticker" => run(&mode, &out, check, 7.5, &[0.6, 1.4, 2.2, 2.9, 3.0, 3.1, 4.5, 7.4], || Ticker::new(&fonts)),
        _ => panic!("unknown mode {mode}; known: ticker"),
    }
}
