//! Motion effects demo: a glowing comet on a feedback trail (FX11) with motion blur from its
//! spring speed (FX12), and a pill with a rim that orbits its border and a sheen that sweeps
//! across it when it enters (FX08).
//!
//! The comet's position is a spring target set 20 times a second along a looping path, so the
//! trail and the blur come from real spring motion, not from a scripted smear.
//!
//!   cargo run --release -p vo --example comet_demo -- [--out DIR] [--check]
//!
//! Writes `DIR/comet_demo.mp4` and `DIR/comet-sheet.png` (default DIR `/var/tmp/claude/m4-fx`).
//! `--check` renders again and compares every frame's hash.

use std::path::PathBuf;
use std::time::Instant;

use vo::scene::display::ShapeKind;
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::taffy::prelude::{AlignItems, JustifyContent};
use vo::scene::Prop;
use vo::{App, Cx, Export, FontRegistry, Options, Scenario, Video};
use vo_core::spring::Spring;
use vo_render::offscreen::{write_png, Frame};

const FPS: f64 = 60.0;
const DURATION: f64 = 6.0;
/// Comet targets per second.
const RATE: f64 = 20.0;
/// When the pill enters, so its sheen shows in the video.
const PILL_AT: f64 = 0.8;

fn accent() -> Color {
    Color::hex(0x4cc9f0, 1.0)
}

/// The comet's target at `t`: a figure eight across the upper part of the frame.
fn path(size: Size, t: f64) -> (f32, f32) {
    let a = t * 1.7;
    let x = 0.5 + 0.38 * a.sin();
    let y = 0.32 + 0.16 * (2.0 * a).sin();
    (x as f32 * size.w, y as f32 * size.h)
}

struct Demo {
    size: Size,
    t: f64,
    pill: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Tick(f64),
    Pill,
}

impl App for Demo {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        for i in 0..(DURATION * RATE) as usize {
            let t = i as f64 / RATE;
            cx.after(t, Msg::Tick(t));
        }
        cx.after(PILL_AT, Msg::Pill);
    }

    fn update(&mut self, msg: Msg, _: &mut Cx<Msg>) {
        match msg {
            Msg::Tick(t) => self.t = t,
            Msg::Pill => self.pill = true,
        }
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        let size = self.size;
        let k = size.h / 400.0;
        let r = 9.0 * k;
        let (x, y) = path(size, self.t);
        let comet = Element::shape("comet", ShapeKind::Circle)
            .size(2.0 * r, 2.0 * r)
            .fill(Color::hex(0xe0f7ff, 1.0))
            .glow(accent(), 6.0 * k, 1.0)
            .offset(x - r, y - r)
            .motion(Prop::Offset, Spring::from_frequency(3.0, 0.9))
            .motion_blur(1.0 / 60.0);
        let sky = Element::layer("sky").size(size.w, size.h).trail(0.12).bloom(0.5, 8.0 * k).child(comet);
        let mut root = Element::panel("root")
            .size(size.w, size.h)
            .fill_gradient(Color::hex(0x0b0d14, 1.0), Color::hex(0x141826, 1.0))
            .child(Element::layer("sky-box").size(0.0, 0.0).child(sky));
        if self.pill {
            let pill = Element::panel("pill")
                .size(360.0 * k, 72.0 * k)
                .radius(36.0 * k)
                .fill(Color::hex(0x1d2233, 1.0))
                .border(Color::hex(0x2b3247, 1.0), 2.0 * k)
                .rim(accent(), 2.0 * k, 0.09, -0.125, 2.4)
                .sheen(Color::rgba(1.0, 1.0, 1.0, 0.22), 1.1)
                .align_items(AlignItems::CENTER)
                .justify(JustifyContent::CENTER)
                .child(
                    Element::label("caption", "trail, blur, rim and sheen")
                        .text_style(TextStyle::new(0, 24.0 * k, 30.0 * k))
                        .color(Color::hex(0xc5cbe0, 1.0)),
                );
            root = root.child(
                Element::column("bottom")
                    .size(size.w, size.h)
                    .align_items(AlignItems::CENTER)
                    .justify(JustifyContent::FLEX_END)
                    .pad(48.0 * k)
                    .child(pill),
            );
        }
        root
    }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let path = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
    fonts
}

/// Frames for the contact sheet.
const SHEET_AT: [f64; 8] = [0.5, 1.0, 1.3, 1.7, 2.4, 3.1, 4.0, 5.2];

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
    let mp4 = out.join("comet_demo.mp4");
    let spec = Export { fps: FPS, duration: DURATION, png_dir: None, video: Some(Video::h264(&mp4)) };
    let sheet_frames: Vec<u64> = SHEET_AT.iter().map(|t| (t * FPS).round() as u64).collect();
    let mut picked = Vec::new();
    let start = Instant::now();
    let app = || Demo { size, t: 0.0, pill: false };
    let report = vo::export_with(app(), &scenario, &spec, options(), |n, f| {
        if sheet_frames.contains(&n) {
            picked.push(f.clone());
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("comet_demo: {e}");
        std::process::exit(1);
    });
    let secs = start.elapsed().as_secs_f64();
    println!("comet_demo: {} frames in {:.2} s, {}", report.frames, secs, mp4.display());
    let sheet = contact_sheet(&picked);
    let sheet_path = out.join("comet-sheet.png");
    write_png(&sheet_path, sheet.width, sheet.height, &sheet.rgba).expect("write contact sheet");
    println!("comet_demo: contact sheet {}", sheet_path.display());
    if check {
        let again = Export { video: None, ..spec.clone() };
        let second = vo::export(app(), &scenario, &again, options()).expect("second render");
        let differ = report.frame_hashes.iter().zip(&second.frame_hashes).filter(|(a, b)| a != b).count();
        println!("comet_demo: second render, {differ} of {} frames differ", report.frames);
        if differ > 0 {
            std::process::exit(1);
        }
    }
}
