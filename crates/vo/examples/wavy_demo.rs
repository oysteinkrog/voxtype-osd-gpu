//! Wavy underline demo (T06): a wavy line draws on under a word in 250 ms; when the word
//! is confirmed the wave straightens and the line fades in 200 ms. The second line changes
//! its wavelength on a spring, the third drifts its phase.
//!
//!   cargo run --release -p vo --example wavy_demo -- [--out DIR] [--check]
//!
//! Writes `DIR/wavy_demo.mp4` and `DIR/wavy-sheet.png` (default DIR `/var/tmp/claude/m4-fx`).
//! `--check` renders again and compares every frame's hash.

use std::path::PathBuf;

use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::path::StrokeStyle;
use vo::scene::taffy::prelude::{AlignItems, JustifyContent};
use vo::scene::{Prop, Transition};
use vo::{App, Cx, Export, FontRegistry, Options, Scenario, Video};
use vo_core::spring::Spring;
use vo_render::offscreen::{write_png, Frame};

const FPS: f64 = 60.0;
const DURATION: f64 = 5.0;
/// Ticks per second.
const RATE: f64 = 20.0;

/// Each word: its text, the line colour, when the line appears and when the word is
/// confirmed.
const WORDS: [(&str, u32, f64, f64); 3] =
    [("recieve", 0xff5d73, 0.4, 2.0), ("their", 0xffb347, 1.0, 3.6), ("tonite", 0x4cc9f0, 1.6, 4.4)];

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
        let ink = Color::hex(0xe6ebf5, 1.0);
        let style = TextStyle::new(0, 44.0 * k, 52.0 * k);
        let mut row = Element::row("words").gap(48.0 * k).align_items(AlignItems::FLEX_START);
        for (i, (text, rgb, shown, confirmed)) in WORDS.into_iter().enumerate() {
            let label = Element::label(format!("word{i}"), text).text_style(style).color(ink).height(52.0 * k);
            // The line stretches to the label's width.
            let mut word =
                Element::column(format!("col{i}")).gap(2.0 * k).align_items(AlignItems::STRETCH).child(label);
            if self.t >= shown {
                // The second line's waves stretch out half way to its confirm.
                let stretched = i == 1 && self.t >= 0.5 * (shown + confirmed);
                let wavelength = if stretched { 22.0 } else { 10.0 } * k;
                let done = self.t >= confirmed;
                let amp = if done { 0.0 } else { 3.0 * k };
                let mut line = Element::wavy_line(
                    format!("wave{i}"),
                    StrokeStyle::solid(2.5 * k, Color::hex(rgb, 1.0)),
                    amp,
                    wavelength,
                )
                .height(12.0 * k)
                .presence(Transition::NONE.trim(0.0, 0.0), Transition::fade())
                .motion(Prop::Trim, Spring::critical_to_98(0.250))
                .motion(Prop::Wave, Spring::SOFT)
                .motion(Prop::Opacity, Spring::critical_to_98(0.200));
                if i == 2 {
                    line = line.wave_phase((self.t * 1.5) as f32);
                }
                if done {
                    line = line.opacity(0.0);
                }
                word = word.child(line);
            }
            row = row.child(word);
        }
        Element::panel("root")
            .size(size.w, size.h)
            .fill_gradient(Color::hex(0x0b0d14, 1.0), Color::hex(0x161a2a, 1.0))
            .align_items(AlignItems::CENTER)
            .justify(JustifyContent::CENTER)
            .child(row)
    }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let path = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
    fonts
}

/// Frames for the contact sheet.
const SHEET_AT: [f64; 8] = [0.45, 0.55, 1.4, 2.05, 2.15, 2.8, 3.75, 4.5];

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
            // Read by the host: open on the real desktop on purpose.
            "--real-display" => {}
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = fonts();
    let size = Size::new(960.0, 400.0);
    let options = || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };
    let scenario = Scenario { size, scale: 1.0, duration: DURATION, inputs: Vec::new() };
    let mp4 = out.join("wavy_demo.mp4");
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
        eprintln!("wavy_demo: {e}");
        std::process::exit(1);
    });
    println!("wavy_demo: {} frames, {}", report.frames, mp4.display());
    let sheet = contact_sheet(&picked);
    let sheet_path = out.join("wavy-sheet.png");
    write_png(&sheet_path, sheet.width, sheet.height, &sheet.rgba).expect("write contact sheet");
    println!("wavy_demo: contact sheet {}", sheet_path.display());
    if check {
        let again = Export { video: None, ..spec.clone() };
        let second = vo::export(app(), &scenario, &again, options()).expect("second render");
        let differ = report.frame_hashes.iter().zip(&second.frame_hashes).filter(|(a, b)| a != b).count();
        println!("wavy_demo: second render, {differ} of {} frames differ", report.frames);
        if differ > 0 {
            std::process::exit(1);
        }
    }
}
