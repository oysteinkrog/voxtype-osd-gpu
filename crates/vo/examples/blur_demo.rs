//! Blur demo (EG-04): dictated lines on a depth stack of frosted sheets. Every four words
//! make a line; each new line pushes the older sheets back. Each sheet frosts what is behind
//! it, and the text on older sheets is blurred more the further back it is, so the far words
//! go out of focus. The newest line, the one being read, stays sharp.
//!
//!   cargo run --release -p vo --example blur_demo -- [--frost R] [--out DIR] [--check] [--reduced]
//!
//! `--frost` is the frost radius in logical units (default 3, 0 for none). Writes
//! `DIR/blur-stack.png`, a strip of five moments (default DIR `/var/tmp/claude/swarm/EG-04`).
//! `--check` runs the reading-line envelope check on the scenario and renders it twice,
//! comparing every frame's hash. `vo-preview run blur_demo` shows it in a window.
//!
//! `tests/blur_envelope.rs` runs the same app through `vo::envelope::check_scenario`.

use std::path::PathBuf;

use vo::scene::depth_stack::{DepthStack, Place, Sheet};
use vo::scene::element::{Element, Key, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::{App, Cx, Export, FontRegistry, Input, Options, Scenario, Tokens};
use vo_core::words::Word;
use vo_render::offscreen::{write_png, Frame};

pub const W: f32 = 460.0;
pub const H: f32 = 210.0;
pub const DURATION: f64 = 5.0;

const TEXT: [&str; 16] = [
    "frost", "softens", "what", "lies", "behind", "each", "sheet", "of", "glass", "while", "the", "newest", "line",
    "stays", "sharp", "here",
];
/// Words per line.
const PER_LINE: usize = 4;
/// Text blur per place back, in logical units.
const DEPTH_BLUR: f32 = 1.2;

pub struct Demo {
    pub frost: f32,
    pub reduced: bool,
    words: Vec<Word>,
}

impl Demo {
    pub fn new(frost: f32, reduced: bool) -> Demo {
        Demo { frost, reduced, words: Vec::new() }
    }

    fn stack(&self) -> DepthStack {
        DepthStack {
            sheet: Some(Sheet {
                fill: Color::hex(0x16222c, 0.35),
                edge: Color::hex(0x9fd8ff, 0.35),
                edge_width: 1.0,
                radius: 6.0,
                pad: (14.0, 6.0),
            }),
            step_z: 70.0,
            step_rise: 18.0,
            frost: self.frost,
            ..DepthStack::default()
        }
    }
}

impl App for Demo {
    type Msg = Vec<Word>;

    fn update(&mut self, words: Vec<Word>, _: &mut Cx<Vec<Word>>) {
        self.words = words;
    }

    fn view(&self, _: &Tokens) -> Element<Vec<Word>> {
        // Lines of four words, keyed by their first word's id.
        let lines: Vec<(Key, &[Word])> =
            self.words.chunks(PER_LINE).map(|l| (Key::from(format!("line{}", l[0].id)), l)).collect();
        let n = lines.len();
        let content = |line: &&[Word], d: usize, _: &Place| {
            let id = line[0].id;
            let text = if d == 0 && n > 0 {
                // The reading line is live text, so the envelope check finds it.
                Element::live_text(format!("live{id}"), line)
            } else {
                Element::label(format!("text{id}"), line.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "))
            };
            let text = text.text_style(TextStyle::new(0, 20.0, 26.0)).color(Color::hex(0xf2f3f5, 1.0));
            // Out of focus with depth; 0 on the newest line draws no pass.
            Element::layer(format!("focus{id}")).layer_blur(DEPTH_BLUR * d as f32).child(text)
        };
        let stack = self.stack().view("stack", Size::new(W, H), Size::new(340.0, 40.0), &lines, content, self.reduced);
        Element::panel("root").size(W, H).fill(Color::hex(0x0b0f14, 1.0)).child(stack)
    }
}

/// A word every 0.25 s from 0.2 s on.
pub fn scenario() -> Scenario<Vec<Word>> {
    let inputs = (1..=TEXT.len())
        .map(|n| {
            let words =
                TEXT[..n].iter().enumerate().map(|(i, w)| Word { id: i as u32 + 1, text: (*w).into() }).collect();
            (0.2 + (n - 1) as f64 * 0.25, Input::Msg(words))
        })
        .collect();
    Scenario { size: Size::new(W, H), scale: 1.0, duration: DURATION, inputs }
}

pub fn options(fonts: FontRegistry, reduced: bool) -> Options<Vec<Word>> {
    Options { fonts, reduced_motion: reduced, ..Options::default() }
}

/// The moments the strip shows.
const STRIP_AT: [f64; 5] = [0.9, 1.9, 2.9, 3.9, 4.9];
const FPS: f64 = 10.0;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut check, mut frost, mut reduced) =
        (PathBuf::from("/var/tmp/claude/swarm/EG-04"), false, 3.0, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--check" => check = true,
            "--reduced" => reduced = true,
            "--frost" => frost = args.next().and_then(|v| v.parse().ok()).expect("--frost needs a number"),
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = FontRegistry::new();
    fonts.add_file(&vo::scene::font::test_font_path()).expect("test font");
    let spec = Export { fps: FPS, duration: DURATION, png_dir: None, video: None };
    let picked_at: Vec<u64> = STRIP_AT.iter().map(|t| (t * FPS).round() as u64).collect();
    let mut picked: Vec<Frame> = Vec::new();
    let app = || Demo::new(frost, reduced);
    let report = vo::export_with(app(), &scenario(), &spec, options(fonts.clone(), reduced), |n, f| {
        if picked_at.contains(&n) {
            picked.push(f.clone());
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("blur_demo: {e}");
        std::process::exit(1);
    });
    let (w, h) = (picked[0].width, picked[0].height);
    let rgba: Vec<u8> = picked.iter().flat_map(|f| f.rgba.iter().copied()).collect();
    std::fs::create_dir_all(&out).expect("create the output directory");
    write_png(&out.join("blur-stack.png"), w, h * picked.len() as u32, &rgba).expect("write the strip");
    println!("blur_demo: {} frames, strip {}", report.frames, out.join("blur-stack.png").display());
    if check {
        let env = vo::envelope::Envelope::default();
        let r = vo::envelope::check_scenario(app(), &scenario(), 60.0, options(fonts.clone(), reduced), &env);
        println!("blur_demo: envelope {r}");
        let second = vo::export(app(), &scenario(), &spec, options(fonts, reduced)).expect("second render");
        let differ = report.frame_hashes.iter().zip(&second.frame_hashes).filter(|(a, b)| a != b).count();
        println!("blur_demo: second render, {differ} of {} frames differ", report.frames);
        if differ > 0 || !r.violations.is_empty() {
            std::process::exit(1);
        }
    }
}
