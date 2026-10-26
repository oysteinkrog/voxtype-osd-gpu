//! The dictation popup in the dot-matrix style (design family 21): a dot waveform, live words
//! in a grotesk, and a footer with a red record dot, a dot-font status line and a dot-font
//! clock. Everything that is not the transcript is round dots on whole-pixel grids.
//!
//! A scripted 9 s session plays from timers in `init`, so the live overlay and the offline
//! render are the same run:
//!
//! - 0.2 s the card fades in, the waveform opens from the centre column outward, the status
//!   assembles from scattered dots.
//! - 0.3 to 4.7 s speech levels at 100 Hz, words arrive and get revised, the clock morphs its
//!   changed digit every second.
//! - 4.7 s stop: the waveform collapses to its centre row, the status morphs to "Finishing".
//! - 5.9 s pasted: the centre-row dots fly up and assemble into a check, the status dissolves
//!   into "Pasted · Enter sent".
//! - 7.4 s dismiss: every dot peels off left to right and flies away, then the card fades.
//!
//!   cargo run --release -p vo --example dots_demo -- [--out DIR] [--live] [--scale S]
//!
//! Offline (default) writes `DIR/dots_demo.mp4` and `DIR/contact-sheet.png`, DIR defaulting to
//! `/var/tmp/claude/m4-dots`. `--live` shows it as an overlay instead (run it in a private
//! nested compositor, never on the real display, from an agent shell).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use vo::scene::display::ShapeKind;
use vo::scene::element::{Element, TextStyle as GlyphStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::taffy::prelude::AlignItems;
use vo::scene::{Prop, Transition};
use vo::{App, Cx, Export, FontRegistry, Options, OverlaySpec, Placement, Scenario, Video};
use vo_core::hash::rand_f32;
use vo_core::spring::Spring;
use vo_core::words::WordModel;
use vo_dots::{
    assemble, check_cells, peel_off, Change, DotField, DotFont, DotStack, DotText, DotWave, Dots, Look, Peel, Springs,
    TextStyle,
};
use vo_render::offscreen::{write_png, Frame};

const W: f32 = 760.0;
const H: f32 = 400.0;
const FPS: f64 = 60.0;
const DURATION: f64 = 9.0;

const CARD_W: f32 = 680.0;
const PAD: f32 = 28.0;
/// Waveform grid: pitch, columns, rows.
const WAVE_PITCH: f32 = 5.0;
const WAVE_COLS: u32 = 124;
const WAVE_ROWS: u32 = 13;
/// Footer dot-font grid pitch.
const FONT_PITCH: f32 = 2.0;
const STATUS_COLS: u32 = 230;
const CLOCK_COLS: u32 = 34;

const SHOW: f64 = 0.2;
const SPEECH: (f64, f64) = (0.35, 4.6);
const STOP: f64 = 4.7;
const PASTED: f64 = 5.9;
const DISMISS: f64 = 7.4;
const HIDE: f64 = 7.85;

const SENTENCE: &str =
    "so the plan for tomorrow is to finish the dot matrix popup and make it feel a lot more polished";

fn red() -> Color {
    Color::hex(0xff453a, 1.0)
}

#[derive(Clone, Debug)]
enum Msg {
    Show,
    Level(f32),
    Words(String),
    Tick(u32),
    Stop,
    Pasted,
    Dismiss,
    Hide,
    Prune,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Hidden,
    Listening,
    Finishing,
    Pasted,
    Leaving,
    Gone,
}

struct DotsDemo {
    phase: Phase,
    levels: Vec<f32>,
    wave: Arc<DotWave>,
    /// Dots that leave the waveform: the check, then the peel.
    sparks: Arc<DotField>,
    spark_ids: Vec<u32>,
    footer: Arc<DotField>,
    clock_dots: Arc<DotField>,
    status: DotText,
    clock: DotText,
    words: WordModel,
    show_at: f64,
}

impl DotsDemo {
    fn new() -> DotsDemo {
        let mut wave = DotWave::new(&[], 100.0, 0.0, WAVE_COLS, WAVE_ROWS);
        wave.open_at = f64::INFINITY;
        let style = TextStyle::new(DotFont::SMALL, Color::hex(0xe8e8ea, 1.0));
        DotsDemo {
            phase: Phase::Hidden,
            levels: Vec::new(),
            wave: Arc::new(wave),
            sparks: Arc::new(DotField::new(
                0.0,
                Springs { pos: Spring::SLOW, bright: Spring::SLOW, ..Springs::default() },
            )),
            spark_ids: Vec::new(),
            footer: Arc::new(DotField::new(0.0, Springs::default())),
            clock_dots: Arc::new(DotField::new(0.0, Springs::default())),
            status: DotText::new(style, (0.0, 0.0), 0, 1 << 20),
            clock: DotText::new(style, (0.0, 0.0), 0, 1 << 20),
            words: WordModel::new(),
            show_at: 0.0,
        }
    }

    fn rebuild_wave(&mut self, t: f64) {
        let old = &self.wave;
        let mut w = DotWave::new(&self.levels, 100.0, t, WAVE_COLS, WAVE_ROWS);
        w.open_at = old.open_at;
        w.collapse_at = old.collapse_at;
        w.column_secs = 0.035;
        self.wave = Arc::new(w);
    }

    fn set_clock(&mut self, t: f64, secs: u32, change: Change) {
        let text = format!("{:02}:{:02}", secs / 60, secs % 60);
        self.clock.set(Arc::make_mut(&mut self.clock_dots), t, &text, change);
    }
}

/// A speech-like level at `t`: syllables about 5 a second, a pause after every few, a little
/// floor noise. Seeded, so every run is the same.
fn speech(t: f64) -> f32 {
    let mut v = 0.02 + 0.03 * rand_f32(9, (t * 100.0) as u32, 0, 0);
    let mut at = SPEECH.0;
    let mut k = 0u32;
    while at < SPEECH.1 {
        let len = 0.09 + 0.08 * rand_f32(5, k, 1, 0) as f64;
        let amp = 0.3 + 0.65 * rand_f32(5, k, 2, 0);
        if t >= at && t < at + len {
            let x = ((t - at) / len) as f32;
            v = v.max(amp * (std::f32::consts::PI * x).sin());
        }
        at += len + 0.05 + 0.06 * rand_f32(5, k, 3, 0) as f64;
        if rand_f32(5, k, 4, 0) < 0.18 {
            at += 0.22;
        }
        k += 1;
    }
    v.min(1.0)
}

/// The hypotheses the recogniser sends: words arrive a few at a time, one gets revised.
fn hypotheses() -> Vec<(f64, String)> {
    let words: Vec<&str> = SENTENCE.split(' ').collect();
    let mut out = Vec::new();
    let mut n = 0;
    let mut t = 0.75;
    while n < words.len() && t < STOP {
        n = (n + 1 + (rand_f32(11, n as u32, 0, 0) * 2.5) as usize).min(words.len());
        let mut text = words[..n].join(" ");
        // "tomorrow" is first heard as "to borrow".
        if n == 5 {
            text = text.replace("tomorrow", "to borrow");
        }
        out.push((t, text));
        t += 0.32 + 0.12 * rand_f32(11, n as u32, 1, 0) as f64;
    }
    out.push((STOP + 0.4, SENTENCE.to_string()));
    out
}

impl App for DotsDemo {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        cx.after(SHOW, Msg::Show);
        let mut t = SPEECH.0 - 0.05;
        while t < STOP {
            cx.after(t, Msg::Level(if t < SPEECH.1 { speech(t) } else { 0.02 }));
            t += 0.01;
        }
        for (at, text) in hypotheses() {
            cx.after(at, Msg::Words(text));
        }
        for s in 1..=4 {
            cx.after(SHOW + s as f64, Msg::Tick(s));
        }
        cx.after(STOP, Msg::Stop);
        cx.after(PASTED, Msg::Pasted);
        cx.after(DISMISS, Msg::Dismiss);
        cx.after(HIDE, Msg::Hide);
        for at in [SHOW + 1.0, STOP + 1.0, PASTED + 1.0, HIDE + 0.6] {
            cx.after(at, Msg::Prune);
        }
        cx.after(DURATION - 0.3, Msg::Exit);
    }

    fn update(&mut self, msg: Msg, cx: &mut Cx<Msg>) {
        let t = cx.t();
        match msg {
            Msg::Show => {
                self.phase = Phase::Listening;
                self.show_at = t;
                let mut w = (*self.wave).clone();
                w.open_at = t + 0.06;
                self.wave = Arc::new(w);
                let footer = Arc::make_mut(&mut self.footer);
                self.status.set(footer, t + 0.12, "Listening · Right Ctrl to stop", Change::Dissolve);
                self.set_clock(t + 0.12, 0, Change::Dissolve);
            }
            Msg::Level(l) => {
                self.levels.push(l);
                if self.levels.len() > 400 {
                    self.levels.drain(..100);
                }
                self.rebuild_wave(t);
            }
            Msg::Words(text) => {
                self.words.update(&text);
            }
            Msg::Tick(s) => self.set_clock(t, s, Change::Morph),
            Msg::Stop => {
                self.phase = Phase::Finishing;
                let mut w = (*self.wave).clone();
                w.collapse_at = Some(t);
                self.wave = Arc::new(w);
                let footer = Arc::make_mut(&mut self.footer);
                self.status.set(footer, t + 0.15, "Finishing", Change::Morph);
            }
            Msg::Pasted => {
                self.phase = Phase::Pasted;
                // The waveform's resting dots become free dots that fly into a check.
                let mut cells = Vec::new();
                vo_dots::DotSource::dots(&*self.wave, t, &mut cells);
                let from: Vec<Look> = cells.iter().map(|d| Look::new(d.x, d.y, d.size, d.color)).collect();
                let check = check_cells(19, 11, 2);
                let origin = ((WAVE_COLS as f32 - 19.0) * 0.5, 1.0);
                let look = Look::new(0.0, 0.0, 0.7, Color::WHITE);
                let sparks = Arc::make_mut(&mut self.sparks);
                self.spark_ids = assemble(sparks, t, &from, &check, origin, look, 0, 0.004, 0.18);
                let footer = Arc::make_mut(&mut self.footer);
                self.status.set(footer, t + 0.25, "Pasted · Enter sent", Change::Dissolve);
            }
            Msg::Dismiss => {
                self.phase = Phase::Leaving;
                let peel = Peel { seed: 3, ..Peel::default() };
                let sparks = Arc::make_mut(&mut self.sparks);
                let ids: Vec<u32> = sparks.ids().collect();
                peel_off(sparks, t, &ids, &peel);
                let low = Peel { distance: 30.0, spread: 10.0, sweep: 0.3, ..peel };
                for field in [&mut self.footer, &mut self.clock_dots] {
                    let field = Arc::make_mut(field);
                    let ids: Vec<u32> = field.ids().collect();
                    peel_off(field, t, &ids, &low);
                }
            }
            Msg::Hide => self.phase = Phase::Gone,
            Msg::Prune => {
                Arc::make_mut(&mut self.sparks).prune(t);
                Arc::make_mut(&mut self.footer).prune(t);
                Arc::make_mut(&mut self.clock_dots).prune(t);
            }
            Msg::Exit => cx.exit(),
        }
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        let root = Element::panel("root").size(W, H).align_items(AlignItems::CENTER).pad(20.0);
        if matches!(self.phase, Phase::Hidden | Phase::Gone) {
            return root;
        }
        let show_wave = !matches!(self.phase, Phase::Pasted | Phase::Leaving);
        let mut stack = DotStack::new();
        if show_wave {
            stack = stack.with(self.wave.clone(), (0.0, 0.0));
        }
        stack = stack.with(self.sparks.clone(), (0.0, 0.0));
        let wave = Element::dots("wave", Dots::new(Arc::new(stack), WAVE_PITCH))
            .size(WAVE_COLS as f32 * WAVE_PITCH, WAVE_ROWS as f32 * WAVE_PITCH);

        let dim = matches!(self.phase, Phase::Pasted | Phase::Leaving);
        let text = Element::live_text("words", self.words.words())
            .text_style(GlyphStyle::new(0, 30.0, 40.0).clamp_lines(3))
            .color(if dim { Color::hex(0xffffff, 0.55) } else { Color::WHITE })
            .motion(Prop::TextColor, Spring::SOFT)
            .word_enter(Transition::fade().offset(0.0, 6.0))
            .width(CARD_W - 2.0 * PAD)
            .height(120.0);

        let rec = Element::shape("rec", ShapeKind::Circle)
            .size(12.0, 12.0)
            .fill(red())
            .opacity(if self.phase == Phase::Listening { 1.0 } else { 0.35 })
            .motion(Prop::Opacity, Spring::SOFT)
            .presence(Transition::fade().scale(0.0), Transition::fade());
        let line = DotFont::SMALL.line_height() as f32 * FONT_PITCH;
        let status = Element::dots("status", Dots::new(self.footer.clone(), FONT_PITCH))
            .size(STATUS_COLS as f32 * FONT_PITCH, line);
        let clock = Element::dots("clock", Dots::new(self.clock_dots.clone(), FONT_PITCH))
            .size(CLOCK_COLS as f32 * FONT_PITCH, line);
        let footer = Element::row("footer")
            .gap(12.0)
            .align_items(AlignItems::CENTER)
            .child(rec)
            .child(status)
            .child(Element::panel("spacer").grow(1.0).height(1.0))
            .child(clock);

        let card = Element::panel("card")
            .width(CARD_W)
            .pad(PAD)
            .gap(18.0)
            .radius(26.0)
            .fill(Color::hex(0x0a0a0b, 0.97))
            .border(Color::hex(0xffffff, 0.16), 1.0)
            .presence(Transition::fade().scale(0.94), Transition::fade().scale(0.97))
            .motion(Prop::Opacity, Spring::critical_to_98(0.22))
            .motion(Prop::Scale, Spring::critical_to_98(0.22))
            .child(wave)
            .child(text)
            .child(footer);
        root.child(card)
    }
}

/// Frames for the contact sheet.
const SHEET_AT: [f64; 12] = [0.3, 0.45, 1.6, 2.9, 4.2, 4.85, 5.3, 6.05, 6.4, 7.5, 7.65, 7.95];
const THUMB: u32 = 2;

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

/// Thumbnails in rows of 3 over a mid-grey desk, so the dark card reads.
fn contact_sheet(thumbs: &[Frame]) -> Frame {
    let (tw, th, gap, cols) = (thumbs[0].width, thumbs[0].height, 8u32, 3u32);
    let rows = (thumbs.len() as u32).div_ceil(cols);
    let (w, h) = (cols * tw + (cols + 1) * gap, rows * th + (rows + 1) * gap);
    let mut rgba = vec![255u8; (w * h * 4) as usize];
    for (i, t) in thumbs.iter().enumerate() {
        let (ox, oy) = (gap + (i as u32 % cols) * (tw + gap), gap + (i as u32 / cols) * (th + gap));
        for y in 0..th {
            for x in 0..tw {
                let s = ((y * tw + x) * 4) as usize;
                let a = t.rgba[s + 3] as u32;
                let at = (((oy + y) * w + ox + x) * 4) as usize;
                for c in 0..3 {
                    // Premultiplied over a desk grey.
                    rgba[at + c] = (t.rgba[s + c] as u32 + 0x3a * (255 - a) / 255) as u8;
                }
                rgba[at + 3] = 255;
            }
        }
    }
    Frame { width: w, height: h, rgba }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let path = "/usr/share/fonts/noto/NotoSans-Regular.ttf";
    fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
    fonts
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut live, mut scale) = (PathBuf::from("/var/tmp/claude/m4-dots"), false, 2.0f32);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--live" => live = true,
            "--scale" => scale = args.next().and_then(|s| s.parse().ok()).expect("--scale needs a number"),
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = fonts();
    let options = || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };
    if live {
        let spec = OverlaySpec {
            size: Size::new(W, H),
            placement: Placement::FromTop { fraction: 0.3 },
            app_id: "vo-dots-demo".into(),
            ..OverlaySpec::default()
        };
        if let Err(e) = vo::run_overlay_with(DotsDemo::new(), spec, options()) {
            eprintln!("dots_demo: {e}");
            std::process::exit(1);
        }
        return;
    }
    std::fs::create_dir_all(&out).expect("create the output directory");
    let scenario = Scenario { size: Size::new(W, H), scale, duration: DURATION, inputs: Vec::new() };
    let mp4 = out.join("dots_demo.mp4");
    let spec = Export { fps: FPS, duration: DURATION, png_dir: None, video: Some(Video::h264(&mp4)) };
    let sheet_frames: Vec<u64> = SHEET_AT.iter().map(|t| (t * FPS).round() as u64).collect();
    let start = Instant::now();
    let mut thumbs = Vec::new();
    let report = vo::export_with(DotsDemo::new(), &scenario, &spec, options(), |n, f| {
        if sheet_frames.contains(&n) {
            thumbs.push(thumb(f));
        }
    })
    .unwrap_or_else(|e| {
        eprintln!("dots_demo: {e}");
        std::process::exit(1);
    });
    let secs = start.elapsed().as_secs_f64();
    println!(
        "dots_demo: {} frames {}x{} in {:.2} s ({:.0} fps), {}",
        report.frames,
        report.width,
        report.height,
        secs,
        report.frames as f64 / secs,
        mp4.display()
    );
    let sheet = contact_sheet(&thumbs);
    let path = out.join("contact-sheet.png");
    write_png(&path, sheet.width, sheet.height, &sheet.rgba).expect("write contact sheet");
    println!("dots_demo: contact sheet {} (at {SHEET_AT:?} s)", path.display());
}
