//! The popup's text modes, one per `--mode`, each exported to an mp4 and a contact sheet.
//!
//! - `ticker` (T13): one right-anchored line. The line slides left on a 200 ms spring as each
//!   word arrives, old text fades under a 40 px mask at the left edge, and a leading "..." fades
//!   in once the text overflows.
//!
//! - `telemetry` (T21, T24): readouts that arrive at 20 Hz and change on screen at most every
//!   500 ms (`Throttle`), with the digits rolling and the decimal point and unit held still by
//!   a right-aligned cell; and two countdowns whose digits roll down (`Roll::Down`), one by the
//!   second and one in 0.1 s steps at 10 Hz.
//!
//! - `log` (T15, T16): the tall log. Each paragraph has a time stamp in a gutter, aligned to its
//!   first line, that rolls in with `digit_roll`. The newest paragraph is bright and slightly
//!   larger, the ones before dim in 300 ms, and a marker dot glides to the newest paragraph.
//!   The log is bottom-anchored in a clip that fades at the top, and the rows spring upward.
//!
//!   cargo run --release -p vo --example text_modes -- --mode ticker [--out DIR] [--check]
//!
//! Writes `DIR/<mode>.mp4` and `DIR/<mode>-sheet.png` (default DIR
//! `/var/tmp/claude/text-modes`). `--check` renders again and compares every frame's hash.

use std::path::PathBuf;

use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::roll::{digit_roll, digit_roll_with, Roll};
use vo::scene::taffy::prelude::{AlignItems, JustifyContent};
use vo::scene::{Prop, Transition};
use vo::{App, Cx, Export, FontRegistry, Options, Scenario, ShapedLayout, TextLayout, Video};
use vo_core::spring::Spring;
use vo_core::throttle::Throttle;
use vo_core::words::Word;
use vo_render::offscreen::{write_png, Frame};

const FPS: f64 = 60.0;
const SIZE: Size = Size::new(960.0, 240.0);

fn bg() -> Element<Msg> {
    bg_sized(SIZE)
}

fn bg_sized(size: Size) -> Element<Msg> {
    Element::panel("root")
        .size(size.w, size.h)
        .fill_gradient(Color::hex(0x0e1016, 1.0), Color::hex(0x161a24, 1.0))
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
}

fn text_color() -> Color {
    Color::hex(0xe6e9f0, 1.0)
}

fn dim_color() -> Color {
    Color::hex(0x8c93a3, 1.0)
}

/// Messages shared by every mode: a numbered tick, and a throttle flush for value `k`.
#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Tick(usize),
    Flush(usize),
}

/// The words a dictation delivers, one every `gap` seconds from `start`.
fn arrivals(text: &str, start: f64, gap: f64) -> Vec<(f64, Word)> {
    text.split(' ')
        .enumerate()
        .map(|(i, w)| (start + i as f64 * gap, Word { id: i as u32 + 1, text: w.to_string() }))
        .collect()
}

// T13: ticker.

const TICKER_TEXT: &str = "so the popup keeps a single line and slides it to the left as each new word \
                           arrives at the right edge while the oldest words fade out under the mask";
const TICKER_W: f32 = 560.0;
/// The window offset's spring: about 200 ms to settle per word.
const TICKER_SPRING: Spring = Spring::critical_to_98(0.2);

struct Ticker {
    words: Vec<(f64, Word)>,
    shown: usize,
    measure: ShapedLayout,
    style: TextStyle,
}

impl Ticker {
    fn new(fonts: &FontRegistry) -> Ticker {
        Ticker {
            words: arrivals(TICKER_TEXT, 0.3, 0.28),
            shown: 0,
            measure: ShapedLayout::new(fonts.clone()),
            style: TextStyle::new(0, 30.0, 40.0),
        }
    }
}

impl App for Ticker {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        for (i, (t, _)) in self.words.iter().enumerate() {
            cx.after(*t, Msg::Tick(i + 1));
        }
    }

    fn update(&mut self, msg: Msg, _: &mut Cx<Msg>) {
        if let Msg::Tick(n) = msg {
            self.shown = n;
        }
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        let words: Vec<Word> = self.words[..self.shown].iter().map(|(_, w)| w.clone()).collect();
        let joined = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        // The app measures its own line, with the same shaping the scene uses, to know when
        // it overflows.
        let width = self.measure.layout(&joined, &self.style, None).size.w;
        let overflow = width > TICKER_W;
        let line = Element::live_text("line", &words)
            .text_style(self.style)
            .color(text_color())
            // Its own width, so the line never wraps to fit the window.
            .width(width)
            .shrink(0.0)
            .motion(Prop::Size, Spring::SNAP)
            .word_enter(Transition::fade().offset(0.0, 8.0))
            .motion(Prop::Position, TICKER_SPRING)
            .motion(Prop::Offset, Spring::SNAP)
            .motion(Prop::Opacity, Spring::SNAP);
        // Right-anchored: the column's items sit at its right edge and overflow to the left.
        let window = Element::layer("window")
            .size(TICKER_W, self.style.line_height)
            .align_items(AlignItems::FLEX_END)
            .fade_edges(40.0, 0.0, 0.0, 0.0)
            .child(line);
        let ellipsis = Element::label("more", "...")
            .text_style(self.style)
            .color(dim_color())
            .opacity(if overflow { 1.0 } else { 0.0 })
            .motion(Prop::Opacity, Spring::SOFT);
        let pill = Element::panel("pill")
            .direction_row()
            .align_items(AlignItems::CENTER)
            .pad_xy(28.0, 18.0)
            .gap(6.0)
            .radius(38.0)
            .fill(Color::hex(0x1f2433, 1.0))
            .stroke(Color::hex(0xffffff, 0.08), 1.0)
            .child(
                Element::shape("dot", vo::scene::display::ShapeKind::Circle)
                    .size(12.0, 12.0)
                    .fill(Color::hex(0xff4d4d, 1.0)),
            )
            .child(Element::panel("sp").width(8.0))
            .child(ellipsis)
            .child(window);
        bg().child(pill)
    }
}

// T21 and T24: telemetry numbers and countdowns.

/// Ticks per second of the telemetry feed.
const FEED_HZ: f64 = 20.0;
const TELEMETRY_SECS: f64 = 6.0;

/// One readout: a label, how to format its value, its unit, and the value at `t`.
struct Readout {
    label: &'static str,
    decimals: usize,
    unit: &'static str,
    value: fn(f64) -> f64,
}

const READOUTS: [Readout; 4] = [
    Readout {
        label: "LATENCY",
        decimals: 0,
        unit: "ms",
        value: |t| 172.0 + 22.0 * (1.3 * t).sin() + jitter(t, 0, 14.0),
    },
    Readout {
        label: "LEVEL",
        decimals: 1,
        unit: "dBFS",
        value: |t| -14.0 - 7.0 * (2.1 * t).sin().abs() + jitter(t, 1, 3.0),
    },
    Readout { label: "VRAM", decimals: 1, unit: "/ 8.0 GB", value: |t| 2.4 + 0.1 * (t / 1.5).floor() },
    Readout { label: "SPEED", decimals: 0, unit: "wpm", value: |t| 96.0 + 9.0 * t + jitter(t, 2, 6.0) },
];

/// A seeded jitter of `amp` around 0 for feed tick at `t`.
fn jitter(t: f64, stream: u32, amp: f64) -> f64 {
    (vo_core::hash::rand_f64(21, (t * FEED_HZ).round() as u32, stream, 0) - 0.5) * amp
}

struct Telemetry {
    throttles: Vec<Throttle<f64>>,
    flush_pending: Vec<bool>,
    t: f64,
}

impl Telemetry {
    fn new() -> Telemetry {
        Telemetry {
            throttles: READOUTS.iter().map(|_| Throttle::new(0.5)).collect(),
            flush_pending: vec![false; READOUTS.len()],
            t: 0.0,
        }
    }

    fn tile(&self, k: usize) -> Element<Msg> {
        let r = &READOUTS[k];
        let v = self.throttles[k].shown().copied().unwrap_or(0.0);
        let text = format!("{v:.*}", r.decimals);
        let style = TextStyle::new(0, 34.0, 42.0);
        // Right-aligned in a fixed cell: the decimal point and the unit never move.
        let value = Element::row(format!("cell{k}")).width(118.0).justify(JustifyContent::FLEX_END).child(digit_roll(
            format!("v{k}"),
            &text,
            style,
            text_color(),
            Spring::SOFT,
        ));
        Element::column(format!("tile{k}"))
            .pad_xy(18.0, 14.0)
            .gap(4.0)
            .radius(12.0)
            .fill(Color::hex(0x1f2433, 1.0))
            .child(
                Element::label(format!("l{k}"), r.label).text_style(TextStyle::new(0, 13.0, 16.0)).color(dim_color()),
            )
            .child(Element::row(format!("r{k}")).align_items(AlignItems::BASELINE).gap(8.0).child(value).child(
                Element::label(format!("u{k}"), r.unit).text_style(TextStyle::new(0, 16.0, 20.0)).color(dim_color()),
            ))
    }
}

impl App for Telemetry {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        for i in 0..=(TELEMETRY_SECS * FEED_HZ) as usize {
            cx.after(i as f64 / FEED_HZ, Msg::Tick(i));
        }
    }

    fn update(&mut self, msg: Msg, cx: &mut Cx<Msg>) {
        let t = cx.t();
        self.t = t;
        match msg {
            Msg::Tick(_) => {
                for (k, r) in READOUTS.iter().enumerate() {
                    if self.throttles[k].push(t, (r.value)(t)).is_none() && !self.flush_pending[k] {
                        let due = self.throttles[k].due().expect("a value waits");
                        cx.after(due - t, Msg::Flush(k));
                        self.flush_pending[k] = true;
                    }
                }
            }
            Msg::Flush(k) => {
                self.throttles[k].flush(t);
                self.flush_pending[k] = false;
            }
        }
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        let tiles = Element::row("tiles").gap(12.0).children((0..READOUTS.len()).map(|k| self.tile(k)));
        // Audio left counts down by the second from 4; the fade-out counts 1.5 s down in
        // 0.1 s steps, ten a second, so its digit spring is short.
        let secs_left = (5.0 - self.t).ceil().clamp(0.0, 4.0) as u32;
        let fade_left = ((5.5 - self.t.max(4.0)) * 10.0).floor().clamp(0.0, 15.0) / 10.0;
        let style = TextStyle::new(0, 18.0, 24.0);
        let count = |key: &str, text: String, spring: Spring| {
            digit_roll_with(key.to_string(), &text, style, text_color(), spring, Roll::Down)
        };
        let words = |key: &str, text: &str| {
            Element::label(key.to_string(), text.to_string()).text_style(style).color(dim_color())
        };
        let status = Element::row("status")
            .gap(6.0)
            .child(words("fin", "Finishing ·"))
            .child(count("left", secs_left.to_string(), Spring::SOFT))
            .child(words("left-u", "s of audio left"))
            .child(Element::panel("sp").width(40.0))
            .child(words("fade", "Fading out in"))
            .child(count("fade-n", format!("{fade_left:.1}"), Spring::critical_to_98(0.08)))
            .child(words("fade-u", "s"));
        bg().gap(22.0).child(tiles).child(status)
    }
}

// T15 and T16: the tall log.

const LOG_SIZE: Size = Size::new(760.0, 420.0);
const LOG_TEXT_W: f32 = 520.0;
const LOG_GAP: f32 = 14.0;
/// Paragraphs: when the first word comes, the stamp, and the text.
const LOG: [(f64, &str, &str); 4] = [
    (0.2, "00:00", "Open the popup in tall mode and keep the newest paragraph bright."),
    (2.0, "00:12", "The one before dims in three hundred milliseconds while the marker moves down."),
    (3.8, "00:31", "Each stamp sits beside the first line of its paragraph and rolls in."),
    (5.6, "00:47", "Older text scrolls up under a soft edge at the top of the log."),
];
const LOG_WORD_GAP: f64 = 0.16;
/// The newest paragraph's scale.
const ACTIVE_SCALE: f32 = 1.03;

struct Log {
    /// `(paragraph, words shown)` per tick, in tick order.
    steps: Vec<(f64, usize, usize)>,
    step: Option<usize>,
    measure: ShapedLayout,
    style: TextStyle,
}

impl Log {
    fn new(fonts: &FontRegistry) -> Log {
        let mut steps = Vec::new();
        for (p, (start, _, text)) in LOG.iter().enumerate() {
            for w in 0..text.split(' ').count() {
                steps.push((start + w as f64 * LOG_WORD_GAP, p, w + 1));
            }
        }
        Log { steps, step: None, measure: ShapedLayout::new(fonts.clone()), style: TextStyle::new(0, 20.0, 28.0) }
    }

    /// The paragraphs on screen, with how many words of each show.
    fn shown(&self) -> Vec<(usize, usize)> {
        let Some(i) = self.step else { return Vec::new() };
        let (_, last, words) = self.steps[i];
        let mut out: Vec<(usize, usize)> = (0..last).map(|p| (p, LOG[p].2.split(' ').count())).collect();
        out.push((last, words));
        out
    }

    fn height(&self, text: &str) -> f32 {
        self.measure.layout(text, &self.style, Some(LOG_TEXT_W)).size.h
    }
}

impl App for Log {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        for (i, (t, _, _)) in self.steps.iter().enumerate() {
            cx.after(*t, Msg::Tick(i));
        }
    }

    fn update(&mut self, msg: Msg, _: &mut Cx<Msg>) {
        if let Msg::Tick(i) = msg {
            self.step = Some(i);
        }
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        let shown = self.shown();
        let newest = shown.len().saturating_sub(1);
        let rows_spring = Spring::critical_to_98(0.26);
        let dim_spring = Spring::critical_to_98(0.3);
        let mut active_y = 0.0;
        let mut y = 0.0;
        let rows = shown.iter().map(|&(p, n)| {
            let (_, stamp, text) = LOG[p];
            let words: Vec<Word> =
                text.split(' ').take(n).enumerate().map(|(i, w)| Word { id: i as u32, text: w.to_string() }).collect();
            let joined = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
            let h = self.height(&joined);
            if p == shown[newest].0 {
                active_y = y;
            }
            y += h + LOG_GAP;
            let active = p == shown[newest].0;
            // Scale about the left edge: the scale is about the box centre, so move the box
            // back by half the growth.
            let s = if active { ACTIVE_SCALE } else { 1.0 };
            let body = Element::live_text(format!("t{p}"), &words)
                .text_style(self.style)
                .color(text_color())
                .width(LOG_TEXT_W)
                .word_enter(Transition::fade().offset(0.0, 6.0))
                .opacity(if active { 1.0 } else { 0.42 })
                .scale(s)
                .offset(LOG_TEXT_W * (s - 1.0) / 2.0, h * (s - 1.0) / 2.0)
                .motion(Prop::Opacity, dim_spring)
                .motion(Prop::Scale, dim_spring)
                .motion(Prop::Offset, dim_spring);
            let stamp_style = TextStyle::new(0, 15.0, self.style.line_height);
            let stamp_color = if active { Color::hex(0xffa64d, 1.0) } else { dim_color() };
            let gutter = Element::row(format!("g{p}")).width(64.0).child(digit_roll(
                format!("s{p}"),
                stamp,
                stamp_style,
                stamp_color,
                Spring::SOFT,
            ));
            Element::row(format!("p{p}"))
                .align_items(AlignItems::FLEX_START)
                .child(gutter)
                .child(body)
                .motion(Prop::Position, rows_spring)
        });
        let rows: Vec<Element<Msg>> = rows.collect();
        let total = (y - LOG_GAP).max(0.0);
        // Bottom-anchored: the rows' column sits at the bottom of the clip and grows upward.
        let log_h = 300.0;
        let text = Element::column("rows").gap(LOG_GAP).children(rows).motion(Prop::Position, rows_spring);
        // The marker rail: one dot, offset to the newest paragraph's first line.
        let lh = self.style.line_height;
        let dot = Element::shape("dot", vo::scene::display::ShapeKind::Circle)
            .size(8.0, 8.0)
            .fill(Color::hex(0xffa64d, 1.0))
            .opacity(if shown.is_empty() { 0.0 } else { 1.0 })
            .offset(0.0, (log_h - total).max(0.0) + active_y + lh / 2.0 - 4.0 - (total - log_h).max(0.0))
            .motion(Prop::Offset, rows_spring);
        let rail = Element::column("rail").width(14.0).height(log_h).child(dot);
        let clip = Element::layer("clip")
            .size(64.0 + LOG_TEXT_W + 20.0, log_h)
            .justify(JustifyContent::FLEX_END)
            .fade_edges(0.0, 48.0, 0.0, 0.0)
            .child(text);
        let card = Element::panel("card")
            .direction_row()
            .pad(22.0)
            .gap(10.0)
            .radius(18.0)
            .fill(Color::hex(0x1b1f2b, 1.0))
            .stroke(Color::hex(0xffffff, 0.07), 1.0)
            .child(rail)
            .child(clip);
        bg_sized(LOG_SIZE).child(card)
    }
}

// Running a mode.

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let path = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
    fonts
}

/// Thumbnails at half size in a 4-wide grid with an 8 px gutter.
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
    let (tw, th, gap) = (thumbs[0].width, thumbs[0].height, 8u32);
    let cols = (thumbs.len() as u32).min(4);
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

/// Exports `make()` for `duration` seconds, writes the contact sheet of the frames at
/// `sheet_at`, and with `check` renders again and compares the frame hashes.
#[allow(clippy::too_many_arguments)]
fn run<A: App<Msg = Msg>>(
    mode: &str,
    size: Size,
    out: &std::path::Path,
    check: bool,
    duration: f64,
    sheet_at: &[f64],
    make: impl Fn() -> A,
) {
    let fonts = fonts();
    let options = || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };
    let scenario = Scenario { size, scale: 1.0, duration, inputs: Vec::new() };
    let mp4 = out.join(format!("{mode}.mp4"));
    let spec = Export { fps: FPS, duration, png_dir: None, video: Some(Video::h264(&mp4)) };
    let sheet_frames: Vec<u64> = sheet_at.iter().map(|t| (t * FPS).round() as u64).collect();
    let mut picked = Vec::new();
    let report = vo::export_with(make(), &scenario, &spec, options(), |n, f| {
        if sheet_frames.contains(&n) {
            picked.push(f.clone());
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("text_modes: {e}");
        std::process::exit(1);
    });
    println!("text_modes: {mode}: {} frames, {}", report.frames, mp4.display());
    let sheet = contact_sheet(&picked);
    let sheet_path = out.join(format!("{mode}-sheet.png"));
    write_png(&sheet_path, sheet.width, sheet.height, &sheet.rgba).expect("write contact sheet");
    println!("text_modes: {mode}: contact sheet {} (frames {sheet_frames:?})", sheet_path.display());
    if check {
        let again = Export { video: None, ..spec.clone() };
        let second = vo::export(make(), &scenario, &again, options()).expect("second render");
        let differ = report.frame_hashes.iter().zip(&second.frame_hashes).filter(|(a, b)| a != b).count();
        println!("text_modes: {mode}: second render, {differ} of {} frames differ", report.frames);
        if differ > 0 {
            std::process::exit(1);
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut check, mut mode) = (PathBuf::from("/var/tmp/claude/text-modes"), false, String::from("ticker"));
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--mode" => mode = args.next().expect("--mode needs a name"),
            "--check" => check = true,
            // Read by the host: open on the real desktop on purpose.
            "--real-display" => {}
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = fonts();
    match mode.as_str() {
        "ticker" => {
            run(&mode, SIZE, &out, check, 7.5, &[0.6, 1.4, 2.2, 2.9, 3.0, 3.1, 4.5, 7.4], || Ticker::new(&fonts))
        }
        "telemetry" => {
            run(&mode, SIZE, &out, check, TELEMETRY_SECS, &[0.3, 0.55, 1.0, 1.52, 3.0, 4.02, 4.6, 5.9], Telemetry::new)
        }
        "log" => {
            run(&mode, LOG_SIZE, &out, check, 8.2, &[1.0, 2.0, 2.15, 2.4, 3.9, 5.0, 5.7, 8.1], || Log::new(&fonts))
        }
        _ => panic!("unknown mode {mode}; known: ticker, telemetry, log"),
    }
}
