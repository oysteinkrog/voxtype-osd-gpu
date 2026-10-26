//! A 10 s title card at 1920x1080 and 60 fps, exported to an mp4: a kicker, four title words, a
//! rule and a row of blocks enter on staggered springs, hold, and leave in a staggered exit.
//!
//! The timeline (`vo_core::timeline`) says when each part moves. Its cues become timers in
//! `init`, each cue changes the app state at its own time, and the scene's springs do the
//! motion. Nothing reads a clock while rendering.
//!
//!   cargo run --release -p vo --example title_card -- [--out DIR] [--check] [--png]
//!
//! Writes `DIR/title_card.mp4` (default DIR `/var/tmp/claude/m2-titlecard`) and
//! `DIR/contact-sheet.png` (8 frames). `--check` renders a second time without video and
//! compares every frame's hash. `--png` also writes the frames as PNGs to `DIR/frames`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use vo::scene::display::ShapeKind;
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::Prop;
use vo::{App, Cx, Export, FontRegistry, Options, Scenario, Video};
use vo_core::spring::Spring;
use vo_core::timeline::{all, chain, clip, delay, marker, sequence, stagger, wait, Edge, Schedule, Timeline};
use vo_render::offscreen::{write_png, Frame};

const W: f32 = 1920.0;
const H: f32 = 1080.0;
const FPS: f64 = 60.0;
const DURATION: f64 = 10.0;
const WORDS: [&str; 4] = ["Time", "is", "an", "input."];
const BLOCKS: usize = 12;
const RULE: f32 = 1600.0;

const BLACK: u32 = 0;
const MEDIUM: u32 = 1;

fn accent() -> Color {
    Color::hex(0xff5a36, 1.0)
}

/// When each part enters and leaves. Entry clips are named after the part; exit clips are
/// `exit.<part>`.
fn timeline() -> Timeline {
    let word = |i: usize| format!("word.{i}");
    let block = |i: usize| format!("block.{i}");
    let intro = all([
        clip("kicker", 0.4),
        delay(0.2, stagger(0.14, 0.5, (0..WORDS.len()).map(|i| clip(word(i), 0.6)))),
        delay(0.75, clip("rule", 0.8)),
        delay(1.0, stagger(0.05, 0.5, (0..BLOCKS).map(|i| clip(block(i), 0.5)))),
        delay(1.6, clip("sub", 0.6)),
    ]);
    let exit = sequence(
        0.12,
        [
            stagger(0.03, 0.3, (0..BLOCKS).rev().map(|i| clip(format!("exit.{}", block(i)), 0.4))),
            clip("exit.sub", 0.4),
            clip("exit.rule", 0.5),
            stagger(0.06, 0.2, (0..WORDS.len()).map(|i| clip(format!("exit.{}", word(i)), 0.4))),
            clip("exit.kicker", 0.4),
        ],
    );
    all([
        chain([wait(0.4), marker("in"), intro]),
        delay(8.2, chain([marker("out"), exit])),
        // Fixes the card's length at 10 s.
        clip("card", DURATION),
    ])
}

/// Where a part is in its life.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Waiting,
    In,
    Out,
}

struct TitleCard {
    schedule: Schedule,
    phase: BTreeMap<String, Phase>,
}

impl TitleCard {
    fn new() -> TitleCard {
        let schedule = timeline().schedule();
        let phase = schedule
            .spans()
            .iter()
            .filter(|s| !s.name.starts_with("exit.") && s.name != "card")
            .map(|s| (s.name.clone(), Phase::Waiting))
            .collect();
        TitleCard { schedule, phase }
    }

    fn phase(&self, part: &str) -> Phase {
        self.phase.get(part).copied().unwrap_or(Phase::Waiting)
    }

    /// Opacity, offset and scale targets for a part, by phase: it rises in from below and
    /// leaves upwards.
    fn motion(&self, e: Element<Msg>, part: &str, rise: f32, spring: Spring) -> Element<Msg> {
        let e = e.motion(Prop::Opacity, Spring::SOFT).motion(Prop::Offset, spring).motion(Prop::Scale, spring);
        match self.phase(part) {
            Phase::Waiting => e.opacity(0.0).offset(0.0, rise).scale(0.92),
            Phase::In => e.opacity(1.0).offset(0.0, 0.0).scale(1.0),
            Phase::Out => e.opacity(0.0).offset(0.0, -rise * 0.6).scale(1.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Cue(String, Edge),
}

impl App for TitleCard {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        let t0 = cx.t();
        let cues: Vec<_> = self.schedule.cues(t0).into_iter().map(|c| (c.t, c.name.to_string(), c.edge)).collect();
        for (t, name, edge) in cues {
            if edge != Edge::End {
                cx.after(t - t0, Msg::Cue(name, edge));
            }
        }
    }

    fn update(&mut self, msg: Msg, cx: &mut Cx<Msg>) {
        let Msg::Cue(name, edge) = msg;
        match (edge, name.strip_prefix("exit.")) {
            (Edge::Start, Some(part)) => {
                self.phase.insert(part.to_string(), Phase::Out);
            }
            (Edge::Start, None) => {
                if let Some(p) = self.phase.get_mut(&name) {
                    *p = Phase::In;
                }
            }
            (Edge::Marker, _) => eprintln!("title_card: marker {name} at {:.3} s", cx.t()),
            _ => {}
        }
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        let kicker = Element::row("kicker")
            .gap(18.0)
            .child(Element::shape("bar", ShapeKind::Capsule).size(56.0, 10.0).fill(accent()))
            .child(
                Element::label("text", "VO FRAMEWORK  /  MILESTONE 2")
                    .text_style(TextStyle::new(MEDIUM, 26.0, 34.0))
                    .color(Color::hex(0x9aa0aa, 1.0)),
            );
        let words = WORDS.iter().enumerate().map(|(i, w)| {
            let color = if i == WORDS.len() - 1 { accent() } else { Color::WHITE };
            let label =
                Element::label(format!("w{i}"), *w).text_style(TextStyle::new(BLACK, 168.0, 196.0)).color(color);
            self.motion(label, &format!("word.{i}"), 90.0, Spring::POP)
        });
        let rule_w = if self.phase("rule") == Phase::In { RULE } else { 0.0 };
        let rule = Element::shape("rule", ShapeKind::RRect)
            .size(rule_w, 6.0)
            .fill(accent())
            .motion(Prop::Size, Spring::SLOW)
            .opacity(if self.phase("rule") == Phase::Out { 0.0 } else { 1.0 });
        let blocks = (0..BLOCKS).map(|i| {
            let v = 0x30 + (i as u32 * 0xa0) / BLOCKS as u32;
            let fill = if i == BLOCKS - 1 { accent() } else { Color::hex(v << 16 | v << 8 | v, 1.0) };
            let b = Element::shape(format!("b{i}"), ShapeKind::RRect).size(64.0, 64.0).radius(8.0).fill(fill);
            self.motion(b, &format!("block.{i}"), 48.0, Spring::POP)
        });
        let sub = Element::label("sub", "Every frame sampled at exactly n / 60 s. Two renders, the same pixels.")
            .text_style(TextStyle::new(MEDIUM, 34.0, 44.0))
            .color(Color::hex(0xc8ccd4, 1.0));

        Element::panel("root")
            .size(W, H)
            .pad_xy(160.0, 250.0)
            .fill_gradient(Color::hex(0x101216, 1.0), Color::hex(0x1a1d23, 1.0))
            .child(self.motion(kicker, "kicker", 30.0, Spring::SOFT))
            .child(Element::row("title").gap(40.0).children(words))
            .child(Element::panel("gap1").height(36.0))
            .child(rule)
            .child(Element::panel("gap2").height(56.0))
            .child(Element::row("blocks").gap(20.0).children(blocks))
            .child(Element::panel("gap3").height(56.0))
            .child(self.motion(sub, "sub", 30.0, Spring::SOFT))
    }
}

/// Frames for the contact sheet: entry, hold and exit.
const SHEET_AT: [f64; 8] = [0.5, 0.8, 1.2, 1.7, 2.6, 8.3, 8.8, 9.1];
const THUMB: u32 = 4;

/// A 1/4 size thumbnail by box filter.
fn thumb(f: &Frame) -> Frame {
    let (w, h) = (f.width / THUMB, f.height / THUMB);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            for c in 0..4 {
                let mut sum = 0u32;
                for dy in 0..THUMB {
                    for dx in 0..THUMB {
                        sum += f.rgba[(((y * THUMB + dy) * f.width + x * THUMB + dx) * 4 + c) as usize] as u32;
                    }
                }
                rgba[((y * w + x) * 4 + c) as usize] = (sum / (THUMB * THUMB)) as u8;
            }
        }
    }
    Frame { width: w, height: h, rgba }
}

/// Thumbnails in a 4 by 2 grid with an 8 px gutter.
fn contact_sheet(thumbs: &[Frame]) -> Frame {
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

/// Peak resident memory of this process, from the kernel.
fn peak_rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0.0)
        / 1024.0
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut check, mut png) = (PathBuf::from("/var/tmp/claude/m2-titlecard"), false, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--check" => check = true,
            "--png" => png = true,
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = FontRegistry::new();
    for path in ["/usr/share/fonts/noto/NotoSans-Black.ttf", "/usr/share/fonts/noto/NotoSans-Medium.ttf"] {
        fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
    }
    let options = || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };
    let scenario = Scenario { size: Size::new(W, H), scale: 1.0, duration: DURATION, inputs: Vec::new() };
    let mp4 = out.join("title_card.mp4");
    let spec = Export {
        fps: FPS,
        duration: DURATION,
        png_dir: png.then(|| out.join("frames")),
        video: Some(Video::h264(&mp4)),
    };
    let sheet_frames: Vec<u64> = SHEET_AT.iter().map(|t| (t * FPS).round() as u64).collect();

    let start = Instant::now();
    let mut thumbs = Vec::new();
    let report = vo::export_with(TitleCard::new(), &scenario, &spec, options(), |n, f| {
        if sheet_frames.contains(&n) {
            thumbs.push(thumb(f));
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("title_card: {e}");
        std::process::exit(1);
    });
    let secs = start.elapsed().as_secs_f64();
    let size = std::fs::metadata(&mp4).map(|m| m.len()).unwrap_or(0);
    println!(
        "title_card: {} frames {}x{} in {:.2} s ({:.0} fps), {} ({:.2} MB), peak RSS {:.0} MB",
        report.frames,
        report.width,
        report.height,
        secs,
        report.frames as f64 / secs,
        mp4.display(),
        size as f64 / 1e6,
        peak_rss_mb()
    );
    let sheet = contact_sheet(&thumbs);
    let sheet_path = out.join("contact-sheet.png");
    write_png(&sheet_path, sheet.width, sheet.height, &sheet.rgba).expect("write contact sheet");
    println!("title_card: contact sheet {} (frames {:?})", sheet_path.display(), sheet_frames);

    if check {
        let start = Instant::now();
        let again = Export { png_dir: None, video: None, ..spec.clone() };
        let second = vo::export(TitleCard::new(), &scenario, &again, options()).expect("second render");
        let differ = report.frame_hashes.iter().zip(&second.frame_hashes).filter(|(a, b)| a != b).count();
        println!(
            "title_card: second render in {:.2} s, {} of {} frames differ",
            start.elapsed().as_secs_f64(),
            differ,
            report.frames
        );
        if differ > 0 || second.frames != report.frames {
            std::process::exit(2);
        }
    }
}
