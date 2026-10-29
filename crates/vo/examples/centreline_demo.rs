//! Pen strokes from the real font (EG-29): three dictated words, each written along its own
//! glyphs' centre lines in pen order (base strokes, then dots and crossbars), over the type at
//! an ink that rises from 0.7 as the pen finishes each letter. The pen is drawn at each
//! glyph's stem width with the ink's own width table, translucent, so the strip shows how
//! the strokes sit in the letters.
//!
//!   cargo run --release -p vo --example centreline_demo -- [--font NAME] [--out DIR] [--check]
//!
//! `--font` is a built-in font asset (default `grotesk`; `extra-condensed-bold` for Saira).
//! Writes `DIR/centreline-<font>.png`, a strip of five moments (default DIR
//! `/var/tmp/claude/swarm/EG-29`). `--check` runs the reading-line envelope check.
//!
//! `tests/centreline_envelope.rs` runs the same app through `vo::envelope::check_scenario`.

use std::path::PathBuf;
use std::sync::Arc;

use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Point, Size};
use vo::scene::glyph_morph::WordOutline;
use vo::scene::path::StrokeStyle;
use vo::scene::stroke_write::{StrokeSource, WriteStyle, Writing};
use vo::scene::text::TextLayout;
use vo::{App, Cx, Export, FontRegistry, Input, Options, Scenario, ShapedLayout, Tokens};
use vo_core::words::Word;
use vo_render::offscreen::write_png;

pub const W: f32 = 600.0;
pub const H: f32 = 300.0;
pub const DURATION: f64 = 4.0;
const SIZE: f32 = 52.0;
const LINE_H: f32 = 64.0;
const TEXT_AT: Point = Point::new(40.0, 110.0);
/// Pen speed in layer units per second, and the pen distance a lift costs.
const SPEED: f32 = 600.0;
const LIFT: f32 = 12.0;

const TEXT: [&str; 3] = ["writing", "tiff", "Hamburg"];
pub const ONSETS: [f64; 3] = [0.2, 1.0, 1.8];

pub fn fonts(name: &str) -> (FontRegistry, u32) {
    let fonts = FontRegistry::new();
    let id = fonts.asset(name).expect("font asset");
    (fonts, id)
}

/// One word placed on the line and its writing.
struct Placed {
    onset: f64,
    writing: Writing,
}

pub struct Demo {
    fonts: FontRegistry,
    font: u32,
    words: Vec<Word>,
    placed: Arc<Vec<Placed>>,
    /// Where the next word starts.
    pen_x: f32,
}

impl Demo {
    pub fn new(name: &str) -> Demo {
        let (fonts, font) = fonts(name);
        Demo { fonts, font, words: Vec::new(), placed: Arc::new(Vec::new()), pen_x: 0.0 }
    }

    fn style(&self) -> TextStyle {
        TextStyle::new(self.font, SIZE, LINE_H)
    }
}

impl App for Demo {
    type Msg = Vec<Word>;

    fn update(&mut self, words: Vec<Word>, cx: &mut Cx<Vec<Word>>) {
        let mut placed: Vec<Placed> = Vec::new();
        for p in self.placed.iter() {
            placed.push(Placed { onset: p.onset, writing: p.writing.clone() });
        }
        for w in words.iter().skip(self.words.len()) {
            let shaped = ShapedLayout::new(self.fonts.clone()).layout(&w.text, &self.style(), None);
            let width = shaped.glyphs.last().map_or(0.0, |g| g.pos.x + g.advance);
            let word = WordOutline::new(&self.fonts, &shaped, SIZE, Point::new(TEXT_AT.x + self.pen_x, TEXT_AT.y));
            // Offline and replayable: analyse now rather than on the worker.
            let writing = Writing::with_source(&self.fonts, &w.text, word, LIFT, StrokeSource::FontNow).word_id(w.id);
            placed.push(Placed { onset: cx.t(), writing });
            self.pen_x += width + 0.3 * SIZE;
        }
        self.placed = Arc::new(placed);
        self.words = words;
    }

    fn view(&self, _: &Tokens) -> Element<Vec<Word>> {
        let placed = self.placed.clone();
        let until = placed.iter().map(|p| p.onset + (p.writing.length() / SPEED) as f64).fold(0.0, f64::max);
        let canvas = Element::canvas("writing", until, move |t, _, p| {
            for w in placed.iter() {
                let s = ((t - w.onset).max(0.0) as f32) * SPEED;
                let stem = (0..w.writing.word.glyphs.len()).filter_map(|i| w.writing.stem(i)).fold(0.0, f32::max);
                let style = WriteStyle {
                    pen: StrokeStyle::solid(stem.max(2.0), Color::hex(0xf59e0b, 0.55)),
                    head: 0.4 * stem,
                    head_color: Color::hex(0xe0f2fe, 1.0),
                    ink: Color::hex(0xf2f3f5, 1.0),
                    ink_floor: 0.7,
                };
                w.writing.draw(p, s, &style);
            }
        })
        .size(W, H);
        Element::panel("root").size(W, H).fill(Color::hex(0x0e1117, 1.0)).child(canvas)
    }
}

/// The three words at their onsets.
pub fn scenario() -> Scenario<Vec<Word>> {
    let inputs = (1..=TEXT.len())
        .map(|n| {
            let words = TEXT[..n].iter().enumerate().map(|(i, w)| Word { id: i as u32, text: (*w).into() }).collect();
            (ONSETS[n - 1], Input::Msg(words))
        })
        .collect();
    Scenario { size: Size::new(W, H), scale: 1.0, duration: DURATION, inputs }
}

pub fn options(fonts: FontRegistry) -> Options<Vec<Word>> {
    Options { fonts, ..Options::default() }
}

const STRIP_AT: [f64; 5] = [0.6, 1.4, 2.2, 2.8, 3.8];
const FPS: f64 = 10.0;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut check, mut font) = (PathBuf::from("/var/tmp/claude/swarm/EG-29"), false, "grotesk".to_string());
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--check" => check = true,
            "--font" => font = args.next().expect("--font needs a name"),
            _ => panic!("unknown option {a}"),
        }
    }
    let demo = || Demo::new(&font);
    let spec = Export { fps: FPS, duration: DURATION, png_dir: None, video: None };
    let picked_at: Vec<u64> = STRIP_AT.iter().map(|t| (t * FPS).round() as u64).collect();
    let mut picked = Vec::new();
    vo::export_with(demo(), &scenario(), &spec, options(demo().fonts), |n, f| {
        if picked_at.contains(&n) {
            picked.push(f.clone());
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("centreline_demo: {e}");
        std::process::exit(1);
    });
    let (w, h) = (picked[0].width, picked[0].height);
    let rgba: Vec<u8> = picked.iter().flat_map(|f: &vo_render::offscreen::Frame| f.rgba.iter().copied()).collect();
    std::fs::create_dir_all(&out).expect("create the output directory");
    let path = out.join(format!("centreline-{font}.png"));
    write_png(&path, w, h * picked.len() as u32, &rgba).expect("write the strip");
    println!("centreline_demo: strip {}", path.display());
    if check {
        let r = vo::envelope::check_scenario(
            demo(),
            &scenario(),
            60.0,
            options(demo().fonts),
            &vo::envelope::Envelope::default(),
        );
        println!("centreline_demo: envelope {r}");
        if !r.violations.is_empty() {
            std::process::exit(1);
        }
    }
}
