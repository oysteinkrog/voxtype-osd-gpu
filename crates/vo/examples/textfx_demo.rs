//! Text effects demo (TY17): live-text words that blur in, form out of noise or wipe into
//! place as they arrive, a label with a breathing halo, and one with a chromatic glitch.
//!
//!   cargo run --release -p vo --example textfx_demo -- [--out DIR] [--check]
//!
//! Writes `DIR/textfx_demo.mp4` and `DIR/textfx-sheet.png` (default DIR `/var/tmp/claude/m4-fx`).
//! `--check` renders again and compares every frame's hash.

use std::path::PathBuf;

use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::taffy::prelude::{AlignItems, JustifyContent};
use vo::scene::{Motion, Prop, Transition};
use vo::{App, Cx, Export, FontRegistry, Options, Scenario, Video};
use vo_core::spring::Spring;
use vo_core::words::Word;
use vo_render::offscreen::{write_png, Frame};

const FPS: f64 = 60.0;
const DURATION: f64 = 6.0;
/// Ticks per second.
const RATE: f64 = 8.0;
/// Seconds between two words of a line.
const WORD_GAP: f64 = 0.25;

/// Each line: when its first word arrives, its words, and how they enter.
fn lines() -> [(f64, &'static str, Transition); 3] {
    [
        (0.2, "words blur in as they arrive", Transition::NONE.opacity(0.0).blur(14.0)),
        (1.7, "or form out of noise, grain by grain", Transition::NONE.dissolve(1.0)),
        (3.4, "or wipe into place from the left", Transition::NONE.wipe(1.0)),
    ]
}

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
        let style = TextStyle::new(0, 34.0 * k, 44.0 * k);
        let mut column = Element::column("lines").gap(10.0 * k).align_items(AlignItems::FLEX_START);
        for (i, (start, text, enter)) in lines().into_iter().enumerate() {
            let shown = ((self.t - start) / WORD_GAP).floor().max(-1.0) as i64 + 1;
            let words: Vec<Word> = text
                .split(' ')
                .take(shown.max(0) as usize)
                .enumerate()
                .map(|(j, w)| Word { id: j as u32, text: w.into() })
                .collect();
            let line = Element::live_text(format!("line{i}"), &words)
                .text_style(style)
                .color(ink)
                .width(800.0 * k)
                .height(44.0 * k)
                .word_enter(enter)
                .motion(Prop::Opacity, Spring::SLOW);
            column = column.child(line);
        }
        // The halo breathes between two radii, one step a second.
        let wide = (self.t as u32) % 2 == 1;
        let halo = Element::label("halo", "halo")
            .text_style(TextStyle::new(0, 44.0 * k, 52.0 * k))
            .color(ink)
            .text_halo(Color::hex(0x4cc9f0, 0.9), if wide { 22.0 * k } else { 10.0 * k })
            .motion(Prop::TextFx, Spring::SLOW);
        // A glitch: the colour channels split for one tick in seven.
        let tick = (self.t * RATE).round() as u32;
        let glitch = if tick % 7 == 3 { 4.0 * k } else { 0.0 };
        let chroma = Element::label("chroma", "signal")
            .text_style(TextStyle::new(0, 44.0 * k, 52.0 * k))
            .color(ink)
            .text_chroma(glitch)
            .motion(Prop::TextFx, Motion::Snap);
        let badges = Element::row("badges").gap(64.0 * k).align_items(AlignItems::CENTER).child(halo).child(chroma);
        Element::panel("root")
            .size(size.w, size.h)
            .fill_gradient(Color::hex(0x0b0d14, 1.0), Color::hex(0x161a2a, 1.0))
            .align_items(AlignItems::CENTER)
            .justify(JustifyContent::CENTER)
            .gap(28.0 * k)
            .child(column)
            .child(badges)
    }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let path = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
    fonts
}

/// Frames for the contact sheet.
const SHEET_AT: [f64; 8] = [0.35, 0.7, 1.3, 2.3, 3.0, 3.6, 4.4, 5.9];

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
    let mp4 = out.join("textfx_demo.mp4");
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
        eprintln!("textfx_demo: {e}");
        std::process::exit(1);
    });
    println!("textfx_demo: {} frames, {}", report.frames, mp4.display());
    let sheet = contact_sheet(&picked);
    let sheet_path = out.join("textfx-sheet.png");
    write_png(&sheet_path, sheet.width, sheet.height, &sheet.rgba).expect("write contact sheet");
    println!("textfx_demo: contact sheet {}", sheet_path.display());
    if check {
        let again = Export { video: None, ..spec.clone() };
        let second = vo::export(app(), &scenario, &again, options()).expect("second render");
        let differ = report.frame_hashes.iter().zip(&second.frame_hashes).filter(|(a, b)| a != b).count();
        println!("textfx_demo: second render, {differ} of {} frames differ", report.frames);
        if differ > 0 {
            std::process::exit(1);
        }
    }
}
