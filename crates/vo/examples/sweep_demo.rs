//! Final-pass sweep demo (T10): a finishing fraction moves a hairline across the text in
//! reading order. Each word brightens briefly as the line reaches it, and takes its final
//! text at that moment, so the casing fixes and the punctuation play as the line crosses.
//!
//!   cargo run --release -p vo --example sweep_demo -- [--out DIR] [--check]
//!
//! Writes `DIR/sweep_demo.mp4` and `DIR/sweep-sheet.png` (default DIR `/var/tmp/claude/m4-fx`).
//! `--check` renders again and compares every frame's hash.

use std::path::PathBuf;

use vo::scene::element::{Element, TextStyle};
use vo::scene::final_pass::FinalPass;
use vo::scene::flow::Revise;
use vo::scene::geom::{Color, Size};
use vo::scene::taffy::prelude::{AlignItems, JustifyContent};
use vo::{App, Cx, Export, FontRegistry, Options, Scenario, Video};
use vo_core::words::Word;
use vo_render::offscreen::{write_png, Frame};

const FPS: f64 = 60.0;
const DURATION: f64 = 3.0;
/// The pass starts here and reads the whole text in `PASS` seconds.
const START: f64 = 0.4;
const PASS: f64 = 1.6;
const TEXT: &str = "so i think the build is fine lets ship it tonight";
const FINAL: &str = "So I think the build is fine, let's ship it tonight.";

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
        for i in 0..=(DURATION * FPS) as usize {
            let t = i as f64 / FPS;
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
        let words = |text: &str| -> Vec<Word> {
            text.split(' ').enumerate().map(|(j, w)| Word { id: j as u32, text: w.into() }).collect()
        };
        let at = ((self.t - START) / PASS).clamp(0.0, 1.0) as f32;
        let mut live = Element::live_text("live", &words(TEXT))
            .text_style(TextStyle::new(0, 34.0 * k, 46.0 * k))
            .color(Color::hex(0xc9d1e3, 0.8))
            .width(560.0 * k)
            .height(100.0 * k)
            .word_revise(Revise::Glyphs);
        if self.t >= START {
            live = live
                .final_pass(FinalPass::new(at).line(Color::hex(0x7fd8ff, 0.9)).lift(0.7))
                .final_words(&words(FINAL));
        }
        Element::panel("root")
            .size(size.w, size.h)
            .fill_gradient(Color::hex(0x0b0d14, 1.0), Color::hex(0x161a2a, 1.0))
            .align_items(AlignItems::CENTER)
            .justify(JustifyContent::CENTER)
            .child(live)
    }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let path = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
    fonts
}

/// Frames for the contact sheet.
const SHEET_AT: [f64; 8] = [0.5, 0.75, 0.95, 1.15, 1.35, 1.6, 1.85, 2.6];

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
    let mp4 = out.join("sweep_demo.mp4");
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
        eprintln!("sweep_demo: {e}");
        std::process::exit(1);
    });
    println!("sweep_demo: {} frames, {}", report.frames, mp4.display());
    let sheet = contact_sheet(&picked);
    let sheet_path = out.join("sweep-sheet.png");
    write_png(&sheet_path, sheet.width, sheet.height, &sheet.rgba).expect("write contact sheet");
    println!("sweep_demo: contact sheet {}", sheet_path.display());
    if check {
        let again = Export { video: None, ..spec.clone() };
        let second = vo::export(app(), &scenario, &again, options()).expect("second render");
        let differ = report.frame_hashes.iter().zip(&second.frame_hashes).filter(|(a, b)| a != b).count();
        println!("sweep_demo: second render, {differ} of {} frames differ", report.frames);
        if differ > 0 {
            std::process::exit(1);
        }
    }
}
