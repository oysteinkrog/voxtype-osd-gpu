//! A notification toast: it slides in, holds while the pointer is over it, closes on a click
//! on its close button, and closes by itself after 4 s.
//!
//! Live, on Wayland (only inside a nested compositor when run by an agent):
//!   cargo run -p vo --example toast
//! Offline, to PNG frames:
//!   cargo run -p vo --example toast -- --offline /var/tmp/claude/toast-frames [--fps 30]
//!
//! Options: `--dismiss-after S` (default 4), `--font PATH`. `VO_DIAG_FRAMES=1` prints a line
//! per frame.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use skrifa::instance::{LocationRef, Size as FontSize};
use skrifa::{FontRef, MetadataProvider};
use vo::scene::display::ShapeKind;
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Point, Size};
use vo::scene::text::{PlacedGlyph, ShapedText, TextLayout};
use vo::scene::{Prop, Scene, Transition};
use vo::{App, Cx, Edges, Input, InputMode, LogDiagnostics, Options, OverlaySpec, Placement, PointerKind, Scenario};
use vo_core::spring::Spring;

/// Counts live heap bytes, for the own-heap figure in the diagnostics.
struct Counting;

static HEAP: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator and only adds bookkeeping.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            HEAP.fetch_add(layout.size(), Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        HEAP.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new) };
        if !p.is_null() {
            HEAP.fetch_sub(layout.size(), Ordering::Relaxed);
            HEAP.fetch_add(new, Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn heap() -> usize {
    HEAP.load(Ordering::Relaxed)
}

/// Text through the font's cmap and advances: no shaping, no kerning. A stand-in for the
/// harfrust line fitter (plan decision 8), so the toast shows real glyphs.
struct CmapText {
    font: Vec<u8>,
}

impl TextLayout for CmapText {
    fn layout(&self, text: &str, style: &TextStyle, max_width: Option<f32>) -> ShapedText {
        let font = FontRef::new(&self.font).expect("font parsed at startup");
        let size = FontSize::new(style.size);
        let cmap = font.charmap();
        let adv = font.glyph_metrics(size, LocationRef::default());
        let m = font.metrics(size, LocationRef::default());
        let baseline0 = (style.line_height - (m.ascent - m.descent)) / 2.0 + m.ascent;
        let advance = |c: char| cmap.map(c).map_or((0, style.size * 0.5), |g| (g.to_u32(), adv.advance_width(g).unwrap_or(0.0)));
        let space = advance(' ').1;
        let mut glyphs = Vec::new();
        let (mut x, mut line, mut widest, mut any) = (0.0f32, 0usize, 0.0f32, false);
        let mut offset = 0;
        for word in text.split(' ') {
            let start = offset;
            offset += word.len() + 1;
            if word.is_empty() {
                continue;
            }
            let w: f32 = word.chars().map(|c| advance(c).1).sum();
            if any {
                if max_width.is_none_or(|max| x + space + w <= max + 1e-3) {
                    x += space;
                } else {
                    widest = widest.max(x);
                    line += 1;
                    x = 0.0;
                }
            }
            let baseline = line as f32 * style.line_height + baseline0;
            for (i, c) in word.char_indices() {
                let (glyph, a) = advance(c);
                glyphs.push(PlacedGlyph { glyph, pos: Point::new(x, baseline), cluster: (start + i) as u32 });
                x += a;
            }
            any = true;
        }
        widest = widest.max(x);
        let lines = if any { line + 1 } else { 0 };
        ShapedText { size: Size::new(widest.ceil(), lines as f32 * style.line_height), glyphs }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Hold,
    Release,
    Dismiss,
    Timeout,
}

struct Toast {
    shown: bool,
    dismiss_after: f64,
    deadline: f64,
    timer: Option<vo::TimerId>,
}

impl App for Toast {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        self.deadline = cx.t() + self.dismiss_after;
        self.timer = Some(cx.after(self.dismiss_after, Msg::Timeout));
    }

    fn update(&mut self, msg: Msg, cx: &mut Cx<Msg>) {
        eprintln!("toast: {msg:?} at t={:.3}", cx.t());
        match msg {
            Msg::Hold => {
                if let Some(id) = self.timer.take() {
                    cx.cancel(id);
                }
            }
            Msg::Release if self.shown && self.timer.is_none() => {
                // Give back what was left, but at least a second.
                let left = (self.deadline - cx.t()).max(1.0);
                self.deadline = cx.t() + left;
                self.timer = Some(cx.after(left, Msg::Timeout));
            }
            Msg::Release => {}
            Msg::Dismiss | Msg::Timeout => {
                self.shown = false;
                if let Some(id) = self.timer.take() {
                    cx.cancel(id);
                }
                cx.exit();
            }
        }
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        let root = Element::column("root").pad(16.0);
        if !self.shown {
            return root;
        }
        let close = Element::shape("close", ShapeKind::Circle)
            .size(18.0, 18.0)
            .fill(Color::hex(0xffffff, 0.12))
            .hairline(Color::hex(0xffffff, 0.4))
            .on_hover(Msg::Hold, Msg::Release)
            .on_click(Msg::Dismiss);
        let header = Element::row("header")
            .gap(8.0)
            .child(Element::shape("dot", ShapeKind::Circle).size(10.0, 10.0).fill(Color::hex(0x4cc38a, 1.0)))
            .child(
                Element::label("title", "Saved")
                    .text_style(TextStyle { font: 0, size: 15.0, line_height: 19.0 })
                    .color(Color::WHITE)
                    .grow(1.0),
            )
            .child(close);
        let toast = Element::panel("toast")
            .width(320.0)
            .pad(14.0)
            .gap(6.0)
            .radius(14.0)
            .fill_gradient(Color::hex(0x24262b, 0.96), Color::hex(0x1b1c20, 0.96))
            .stroke(Color::hex(0xffffff, 0.08), 1.0)
            .motion(Prop::Size, Spring::SNAP)
            .presence(Transition::fade().offset(0.0, -12.0).scale(0.96), Transition::fade().scale(0.98))
            .on_hover(Msg::Hold, Msg::Release)
            .child(header)
            .child(
                Element::label("body", "The recording was transcribed and copied to the clipboard.")
                    .text_style(TextStyle { font: 0, size: 13.0, line_height: 17.0 })
                    .color(Color::hex(0xc8cad0, 1.0)),
            );
        root.child(toast)
    }
}

const SURFACE: Size = Size::new(352.0, 132.0);

/// Where the toast and its close button sit once settled, in surface coordinates.
fn targets(text: Box<dyn TextLayout>, dismiss_after: f64) -> (Point, Point) {
    let mut scene = Scene::new().with_text(text);
    scene.set_viewport(0.0, SURFACE, 1.0);
    let app = Toast { shown: true, dismiss_after, deadline: 0.0, timer: None };
    scene.apply(app.view(&vo::Tokens::default()), 0.0);
    let centre = |path: &[&str]| {
        let id = scene.find(path).expect("node exists");
        let s = scene.sampled(id, 2.0).expect("drawn after settling");
        s.transform.apply(Point::new(s.size.w / 2.0, s.size.h / 2.0))
    };
    (centre(&["root", "toast"]), centre(&["root", "toast", "header", "close"]))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut offline, mut fps, mut dismiss_after) = (None::<PathBuf>, 30.0, 4.0);
    let mut font_path = "/usr/share/fonts/noto/NotoSans-Medium.ttf".to_string();
    while let Some(a) = args.next() {
        let mut v = || args.next().unwrap_or_else(|| panic!("{a} needs a value"));
        match a.as_str() {
            "--offline" => offline = Some(v().into()),
            "--fps" => fps = v().parse().expect("--fps takes a number"),
            "--dismiss-after" => dismiss_after = v().parse().expect("--dismiss-after takes seconds"),
            "--font" => font_path = v(),
            _ => panic!("unknown option {a}"),
        }
    }
    let font = std::fs::read(&font_path).unwrap_or_else(|e| panic!("cannot read {font_path}: {e}"));
    FontRef::new(&font).expect("a font file");
    let text = || Box::new(CmapText { font: font.clone() }) as Box<dyn TextLayout>;
    let (toast_at, close_at) = targets(text(), dismiss_after);
    eprintln!(
        "toast: surface {}x{}, toast centre ({:.1}, {:.1}), close button centre ({:.1}, {:.1})",
        SURFACE.w, SURFACE.h, toast_at.x, toast_at.y, close_at.x, close_at.y
    );
    let app = Toast { shown: true, dismiss_after, deadline: 0.0, timer: None };
    let options = || Options::<Msg> { fonts: vec![font.clone()], text: Some(text()), ..Options::default() };

    if let Some(dir) = offline {
        // Hover at 1 s (holds the timer), click the close button at 2.5 s.
        let scenario = Scenario {
            size: SURFACE,
            scale: 1.0,
            duration: 6.0,
            inputs: vec![
                (1.0, Input::Pointer { pos: toast_at, kind: PointerKind::Enter }),
                (1.0, Input::Pointer { pos: toast_at, kind: PointerKind::Motion }),
                (2.4, Input::Pointer { pos: close_at, kind: PointerKind::Motion }),
                (2.5, Input::Pointer { pos: close_at, kind: PointerKind::Down(vo::scene::input::Button::Primary) }),
                (2.55, Input::Pointer { pos: close_at, kind: PointerKind::Up(vo::scene::input::Button::Primary) }),
            ],
        };
        let n = vo::render_offline_with(app, &scenario, fps, &dir, options()).expect("offline render");
        eprintln!("toast: wrote {n} frames to {}", dir.display());
        return;
    }

    let spec = OverlaySpec {
        size: SURFACE,
        placement: Placement::Anchored { edges: Edges::TOP_RIGHT, margin: [16, 16, 0, 0] },
        input: InputMode::Regions(Default::default()),
        app_id: "vo-toast".into(),
        ..OverlaySpec::default()
    };
    let mut options = options();
    options.diagnostics = Some(Box::new(LogDiagnostics::new(Some(heap))));
    if let Err(e) = vo::run_overlay_with(app, spec, options) {
        eprintln!("toast: {e}");
        std::process::exit(1);
    }
}
