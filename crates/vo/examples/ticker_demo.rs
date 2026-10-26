//! Masks demo (FX04): a ticker line scrolling inside a clip whose edges fade, and a clock
//! and a word count whose changed digits roll like an odometer, carries included.
//!
//!   cargo run --release -p vo --example ticker_demo -- [--out DIR] [--check]
//!
//! Writes `DIR/ticker_demo.mp4` and `DIR/ticker-sheet.png` (default DIR `/var/tmp/claude/m4-fx`).
//! `--check` renders again and compares every frame's hash.

use std::path::PathBuf;

use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::roll::digit_roll;
use vo::scene::taffy::prelude::{AlignItems, JustifyContent};
use vo::scene::Prop;
use vo::{App, Cx, Export, FontRegistry, Options, Scenario, Video};
use vo_core::spring::Spring;
use vo_render::offscreen::{write_png, Frame};

const FPS: f64 = 60.0;
const DURATION: f64 = 6.0;
/// Ticks per second.
const RATE: f64 = 4.0;
/// Ticker speed in logical units per second.
const SPEED: f32 = 90.0;
const LINE: &str = "so the edges fade instead of cutting words in half, and the numbers roll";

struct Demo {
    size: Size,
    t: f64,
}

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Tick(f64),
}

impl App for Demo {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        for i in 0..=(DURATION * RATE) as usize {
            let t = i as f64 / RATE;
            cx.after(t, Msg::Tick(t));
        }
    }

    fn update(&mut self, msg: Msg, _: &mut Cx<Msg>) {
        let Msg::Tick(t) = msg;
        self.t = t;
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        let size = self.size;
        let k = size.h / 400.0;
        let text = Color::hex(0xdde3f0, 1.0);
        // The ticker's target runs one tick ahead, so the spring glides instead of stepping.
        let x = 520.0 * k - SPEED * k * (self.t + 1.0 / RATE) as f32;
        let words = Element::label("line", LINE)
            .text_style(TextStyle::new(0, 30.0 * k, 40.0 * k))
            .color(text)
            .shrink(0.0)
            .offset(x, 0.0)
            .motion(Prop::Offset, Spring::from_frequency(1.5, 1.0));
        let ticker =
            Element::layer("ticker").size(560.0 * k, 40.0 * k).fade_edges(60.0 * k, 0.0, 60.0 * k, 0.0).child(words);
        let secs = self.t.floor() as u32 + 57;
        let clock = format!("{:02}:{:02}", secs / 60, secs % 60);
        // A word count that steps four times a second and carries past 99.
        let words = format!("{} words", 92 + (self.t * RATE) as u32);
        let digits = |key: &str, text: &str, size: f32, color: u32| {
            let style = TextStyle::new(0, size * k, 1.2 * size * k);
            digit_roll(key.to_string(), text, style, Color::hex(color, 1.0), Spring::SOFT)
        };
        let timer = Element::row("numbers")
            .gap(48.0 * k)
            .align_items(AlignItems::CENTER)
            .child(digits("clock", &clock, 64.0, 0x4cc9f0))
            .child(digits("count", &words, 36.0, 0x9aa3b8));
        Element::panel("root")
            .size(size.w, size.h)
            .fill_gradient(Color::hex(0x0b0d14, 1.0), Color::hex(0x141826, 1.0))
            .align_items(AlignItems::CENTER)
            .justify(JustifyContent::CENTER)
            .gap(36.0 * k)
            .child(timer)
            .child(
                Element::panel("card")
                    .size(600.0 * k, 72.0 * k)
                    .radius(36.0 * k)
                    .fill(Color::hex(0x1d2233, 1.0))
                    .align_items(AlignItems::CENTER)
                    .justify(JustifyContent::CENTER)
                    .child(ticker),
            )
    }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let path = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
    fonts
}

/// Frames for the contact sheet.
const SHEET_AT: [f64; 8] = [0.3, 1.0, 2.0, 2.08, 2.16, 3.0, 3.1, 5.9];

/// Thumbnails in a 4 by 2 grid with an 8 px gutter, at half size.
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
    let (tw, th, gap, cols) = (thumbs[0].width, thumbs[0].height, 8u32, 4u32);
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

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut check) = (PathBuf::from("/var/tmp/claude/m4-fx"), false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--check" => check = true,
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = fonts();
    let size = Size::new(960.0, 400.0);
    let options = || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };
    let scenario = Scenario { size, scale: 1.0, duration: DURATION, inputs: Vec::new() };
    let mp4 = out.join("ticker_demo.mp4");
    let spec = Export { fps: FPS, duration: DURATION, png_dir: None, video: Some(Video::h264(&mp4)) };
    let sheet_frames: Vec<u64> = SHEET_AT.iter().map(|t| (t * FPS).round() as u64).collect();
    let mut picked = Vec::new();
    let app = || Demo { size, t: 0.0 };
    let report = vo::export_with(app(), &scenario, &spec, options(), |n, f| {
        if sheet_frames.contains(&n) {
            picked.push(f.clone());
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("ticker_demo: {e}");
        std::process::exit(1);
    });
    println!("ticker_demo: {} frames, {}", report.frames, mp4.display());
    let sheet = contact_sheet(&picked);
    let sheet_path = out.join("ticker-sheet.png");
    write_png(&sheet_path, sheet.width, sheet.height, &sheet.rgba).expect("write contact sheet");
    println!("ticker_demo: contact sheet {}", sheet_path.display());
    if check {
        let again = Export { video: None, ..spec.clone() };
        let second = vo::export(app(), &scenario, &again, options()).expect("second render");
        let differ = report.frame_hashes.iter().zip(&second.frame_hashes).filter(|(a, b)| a != b).count();
        println!("ticker_demo: second render, {differ} of {} frames differ", report.frames);
        if differ > 0 {
            std::process::exit(1);
        }
    }
}
