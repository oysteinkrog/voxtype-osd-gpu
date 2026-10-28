//! Displacement demo (EG-03): a bar waveform and live text under a layer that bends what is
//! under it. Each word that arrives starts a ripple ring from the waveform; the older line
//! bends with the rings while the newest line, the one being read, stays sharp and still.
//!
//!   cargo run --release -p vo --example displace_demo -- [--source S] [--out DIR] [--check]
//!
//! `--source` is `ripples` (default), `noise`, `curl` or `page`. Writes
//! `DIR/displace-<source>.png`, a strip of five moments (default DIR
//! `/var/tmp/claude/swarm/EG-03`). `--check` runs the reading-line envelope check on the
//! scenario and renders it twice, comparing every frame's hash.
//!
//! `tests/displace_envelope.rs` runs the same app through `vo::envelope::check_scenario`.

use std::path::PathBuf;

use vo::scene::displace::{Displace, Onset, PageCurl, Ripples};
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Point, Size};
use vo::scene::taffy::AlignItems;
use vo::{App, Cx, Export, FontRegistry, Input, Options, Scenario, Tokens};
use vo_core::words::Word;
use vo_render::offscreen::{write_png, Frame};

pub const W: f32 = 420.0;
pub const H: f32 = 170.0;
pub const DURATION: f64 = 3.6;

const TEXT: [&str; 10] = ["ripples", "bend", "the", "older", "line", "while", "the", "newest", "one", "holds"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    Ripples,
    Noise,
    Curl,
    Page,
}

pub struct Demo {
    pub source: Source,
    words: Vec<Word>,
    /// When each word arrived: the ripples' onsets.
    onsets: Vec<f64>,
    t: f64,
}

impl Demo {
    pub fn new(source: Source) -> Demo {
        Demo { source, words: Vec::new(), onsets: Vec::new(), t: 0.0 }
    }

    fn displace(&self) -> Displace {
        match self.source {
            Source::Ripples => Displace::Ripples(
                Ripples::new()
                    .onsets(self.onsets.iter().enumerate().map(|(i, &t)| Onset {
                        t,
                        // Along the waveform, left to right as words arrive.
                        at: Point::new(40.0 + (i % 8) as f32 * 46.0, 32.0),
                    }))
                    .amp(4.0)
                    .speed(140.0)
                    .wave(16.0, 9.0),
            ),
            Source::Noise => Displace::Noise { scale: 60.0, amount: 2.5, speed: 0.25, seed: 7 },
            Source::Curl => Displace::Curl { scale: 70.0, amount: 3.0, speed: 0.25, seed: 11 },
            Source::Page => Displace::PageCurl(PageCurl {
                edge: Point::new(W, H),
                dir: Point::new(1.0, 0.6),
                radius: 18.0,
                reach: 140.0,
            }),
        }
    }
}

impl App for Demo {
    type Msg = Vec<Word>;

    fn update(&mut self, words: Vec<Word>, cx: &mut Cx<Vec<Word>>) {
        if words.len() > self.words.len() {
            self.onsets.push(cx.t());
        }
        self.words = words;
        self.t = cx.t();
    }

    fn view(&self, _: &Tokens) -> Element<Vec<Word>> {
        let bars =
            Element::row("wave").gap(3.0).align_items(AlignItems::CENTER).height(44.0).children((0..56u32).map(|i| {
                let x = i as f32;
                let h = 6.0 + 32.0 * (0.5 + 0.5 * (x * 0.37).sin() * (x * 0.11).cos()).abs();
                Element::panel(i as u64).size(4.0, h).radius(2.0).fill(Color::hex(0x6cb6ff, 1.0))
            }));
        let live = Element::live_text("live", &self.words)
            .text_style(TextStyle::new(0, 22.0, 28.0))
            .color(Color::hex(0xf2f3f5, 1.0))
            .width(W - 40.0);
        // The page curl lifts with time; the other sources run at full strength.
        let strength = match self.source {
            Source::Page => (self.t / DURATION).clamp(0.0, 1.0) as f32,
            _ => 1.0,
        };
        let fx = Element::layer("fx").size(W, H).displace(self.displace()).displace_strength(strength);
        Element::panel("root")
            .size(W, H)
            .fill(Color::hex(0x14161b, 1.0))
            .child(at(bars, 14.0, 10.0))
            .child(at(live, 20.0, 66.0))
            .child(at(fx, 0.0, 0.0))
    }
}

fn at<M>(e: Element<M>, x: f32, y: f32) -> Element<M> {
    e.layout(|s| {
        s.position = vo::scene::taffy::style::Position::Absolute;
        s.inset.left = vo::scene::taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = vo::scene::taffy::style::LengthPercentageAuto::length(y);
    })
}

/// A word every 0.3 s from 0.2 s on.
pub fn scenario() -> Scenario<Vec<Word>> {
    let inputs = (1..=TEXT.len())
        .map(|n| {
            let words = TEXT[..n].iter().enumerate().map(|(i, w)| Word { id: i as u32, text: (*w).into() }).collect();
            (0.2 + (n - 1) as f64 * 0.3, Input::Msg(words))
        })
        .collect();
    Scenario { size: Size::new(W, H), scale: 1.0, duration: DURATION, inputs }
}

pub fn options(fonts: FontRegistry) -> Options<Vec<Word>> {
    Options { fonts, reduced_motion: false, ..Options::default() }
}

/// The moments the strip shows.
const STRIP_AT: [f64; 5] = [0.8, 1.4, 2.0, 2.9, 3.5];
const FPS: f64 = 10.0;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut check, mut source) = (PathBuf::from("/var/tmp/claude/swarm/EG-03"), false, Source::Ripples);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--check" => check = true,
            "--source" => {
                source = match args.next().as_deref() {
                    Some("ripples") => Source::Ripples,
                    Some("noise") => Source::Noise,
                    Some("curl") => Source::Curl,
                    Some("page") => Source::Page,
                    s => panic!("--source is ripples, noise, curl or page, not {s:?}"),
                }
            }
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = FontRegistry::new();
    fonts.add_file(&vo::scene::font::test_font_path()).expect("test font");
    let spec = Export { fps: FPS, duration: DURATION, png_dir: None, video: None };
    let picked_at: Vec<u64> = STRIP_AT.iter().map(|t| (t * FPS).round() as u64).collect();
    let mut picked: Vec<Frame> = Vec::new();
    let report = vo::export_with(Demo::new(source), &scenario(), &spec, options(fonts.clone()), |n, f| {
        if picked_at.contains(&n) {
            picked.push(f.clone());
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("displace_demo: {e}");
        std::process::exit(1);
    });
    let (w, h) = (picked[0].width, picked[0].height);
    let rgba: Vec<u8> = picked.iter().flat_map(|f| f.rgba.iter().copied()).collect();
    std::fs::create_dir_all(&out).expect("create the output directory");
    let name = format!("displace-{}.png", format!("{source:?}").to_lowercase());
    write_png(&out.join(&name), w, h * picked.len() as u32, &rgba).expect("write the strip");
    println!("displace_demo: {} frames, strip {}", report.frames, out.join(&name).display());
    if check {
        let env = vo::envelope::Envelope::default();
        let r = vo::envelope::check_scenario(Demo::new(source), &scenario(), 60.0, options(fonts.clone()), &env);
        println!("displace_demo: envelope {r}");
        let second = vo::export(Demo::new(source), &scenario(), &spec, options(fonts)).expect("second render");
        let differ = report.frame_hashes.iter().zip(&second.frame_hashes).filter(|(a, b)| a != b).count();
        println!("displace_demo: second render, {differ} of {} frames differ", report.frames);
        if differ > 0 || !r.violations.is_empty() {
            std::process::exit(1);
        }
    }
}
