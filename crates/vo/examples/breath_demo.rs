//! Smooth variable font axes (EG-23): three dictated lines in Roboto Flex, one breathing
//! through each axis over four seconds, weight 400 to 600, width 95 to 105 and slant 0 to
//! -5, every frame between the quantised instances (`var_axes::breathe`). A fourth line runs
//! the weight ramp in the quantised instances alone, for comparison: it thickens in steps.
//!
//!   cargo run --release -p vo --example breath_demo -- [--out DIR] [--check]
//!
//! Writes `DIR/breath-strip.png`, the four lines at five moments (default DIR
//! `/var/tmp/claude/swarm/EG-23`). `--check` runs the reading-line envelope check.
//!
//! `tests/breath_envelope.rs` runs the same app through `vo::envelope::check_scenario`.

use std::path::PathBuf;

use vo::scene::element::{Element, TextStyle};
use vo::scene::font::var_axes::{breathe, Axes, Breath};
use vo::scene::geom::{Color, Size};
use vo::{App, Cx, Export, FontRegistry, Options, Scenario, Tokens};
use vo_core::words::Word;
use vo_render::offscreen::write_png;

pub const W: f32 = 600.0;
pub const H: f32 = 300.0;
pub const DURATION: f64 = 5.5;
/// The ramps run from `START` for `SECS`.
const START: f64 = 1.0;
const SECS: f64 = 4.0;
const SIZE: f32 = 30.0;
const TEXT: &str = "breathing with the voice";
const TICK: f64 = 1.0 / 60.0;

pub fn fonts() -> (FontRegistry, u32) {
    let fonts = FontRegistry::new();
    let id = fonts.asset("flex").expect("font asset");
    (fonts, id)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tick(f64);

pub struct Demo {
    fonts: FontRegistry,
    font: u32,
    t: f64,
}

impl Demo {
    /// The demo drawing from `fonts`, which must be the table the scene shapes with: the
    /// instances it adds go there.
    pub fn with_fonts(fonts: FontRegistry) -> Demo {
        let font = fonts.asset("flex").expect("font asset");
        Demo { fonts, font, t: 0.0 }
    }

    /// One line: its words at `axes`, smooth or in the quantised instance alone.
    fn line(&self, key: &str, id0: u32, axes: Axes, smooth: bool) -> Element<Tick> {
        let words: Vec<Word> =
            TEXT.split(' ').enumerate().map(|(i, w)| Word { id: id0 + i as u32, text: w.into() }).collect();
        let breaths = words
            .iter()
            .filter_map(|w| {
                let b = if smooth {
                    breathe(&self.fonts, self.font, &w.text, axes)
                } else {
                    self.fonts.add_instance(self.font, &axes.quantized().settings()).map(Breath::plain)
                };
                b.map(|b| (w.id, b))
            })
            .collect();
        Element::live_text(key.to_string(), &words)
            .text_style(TextStyle::new(self.font, SIZE, 1.3 * SIZE))
            .color(Color::hex(0xf2f3f5, 1.0))
            .word_breath(breaths)
            .width(W - 60.0)
    }
}

impl App for Demo {
    type Msg = Tick;

    fn init(&mut self, cx: &mut Cx<Tick>) {
        let mut t = 0.0;
        while t <= DURATION {
            cx.after(t, Tick(t));
            t += TICK;
        }
    }

    fn update(&mut self, Tick(t): Tick, _: &mut Cx<Tick>) {
        self.t = t;
    }

    fn view(&self, _: &Tokens) -> Element<Tick> {
        let k = ((self.t - START) / SECS).clamp(0.0, 1.0) as f32;
        let weight = Axes { wght: 400.0 + 200.0 * k, ..Axes::REST };
        let width = Axes { wdth: 95.0 + 10.0 * k, ..Axes::REST };
        let slant = Axes { slnt: -5.0 * k, ..Axes::REST };
        Element::panel("root")
            .size(W, H)
            .pad(30.0)
            .gap(14.0)
            .fill(Color::hex(0x0e1117, 1.0))
            .child(self.line("weight", 0, weight, true))
            .child(self.line("width", 10, width, true))
            .child(self.line("slant", 20, slant, true))
            .child(self.line("steps", 30, weight, false))
    }
}

/// No inputs: the ramps run on the app's own ticks.
pub fn scenario() -> Scenario<Tick> {
    Scenario { size: Size::new(W, H), scale: 1.0, duration: DURATION, inputs: Vec::new() }
}

pub fn options(fonts: FontRegistry) -> Options<Tick> {
    Options { fonts, ..Options::default() }
}

const STRIP_AT: [f64; 5] = [0.9, 2.0, 3.0, 4.0, 5.2];
const FPS: f64 = 10.0;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut check) = (PathBuf::from("/var/tmp/claude/swarm/EG-23"), false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--check" => check = true,
            _ => panic!("unknown option {a}"),
        }
    }
    let spec = Export { fps: FPS, duration: DURATION, png_dir: None, video: None };
    let picked_at: Vec<u64> = STRIP_AT.iter().map(|t| (t * FPS).round() as u64).collect();
    let mut picked = Vec::new();
    let (fonts, _) = fonts();
    vo::export_with(Demo::with_fonts(fonts.clone()), &scenario(), &spec, options(fonts), |n, f| {
        if picked_at.contains(&n) {
            picked.push(f.clone());
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("breath_demo: {e}");
        std::process::exit(1);
    });
    let (w, h) = (picked[0].width, picked[0].height);
    let rgba: Vec<u8> = picked.iter().flat_map(|f: &vo_render::offscreen::Frame| f.rgba.iter().copied()).collect();
    std::fs::create_dir_all(&out).expect("create the output directory");
    let path = out.join("breath-strip.png");
    write_png(&path, w, h * picked.len() as u32, &rgba).expect("write the strip");
    println!("breath_demo: strip {}", path.display());
    if check {
        let (fonts, _) = self::fonts();
        let demo = Demo::with_fonts(fonts.clone());
        let r =
            vo::envelope::check_scenario(demo, &scenario(), 60.0, options(fonts), &vo::envelope::Envelope::default());
        println!("breath_demo: envelope {r}");
        if !r.violations.is_empty() {
            std::process::exit(1);
        }
    }
}
