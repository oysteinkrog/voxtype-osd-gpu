//! A notification toast: it slides in, holds while the pointer is over it, closes on a click
//! on its close button, and closes by itself after 4 s.
//!
//! Live, on Wayland (only inside a nested compositor when run by an agent):
//!   cargo run -p vo --example toast
//! Offline, to PNG frames:
//!   cargo run -p vo --example toast -- --offline /var/tmp/claude/toast-frames [--fps 30]
//!
//! In the preview and scrub tool, with the offline script:
//!   cargo run -p vo --example toast -- --preview
//!
//! Options: `--dismiss-after S` (default 4), `--font PATH`, `--cpu-first-frame` (draw the first
//! frames on the CPU while the GPU starts). `VO_DIAG_FRAMES=1` prints a line per frame.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use vo::scene::display::ShapeKind;
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Point, Size};
use vo::scene::{Prop, Scene, Transition};
use vo::{
    App, Cx, Edges, FontRegistry, Input, InputMode, LogDiagnostics, Options, OverlaySpec, Placement, PointerKind,
    Scenario, ShapedLayout,
};
use vo_core::spring::Spring;

/// Counts live heap bytes, for the own-heap figure in the diagnostics.
struct Counting;

static HEAP: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(by: usize) {
    let now = HEAP.fetch_add(by, Ordering::Relaxed) + by;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

// SAFETY: forwards to the system allocator and only adds bookkeeping.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grow(layout.size());
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
            grow(new);
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn heap() -> usize {
    HEAP.load(Ordering::Relaxed)
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
                    .text_style(TextStyle::new(0, 15.0, 19.0))
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
                    .text_style(TextStyle::new(0, 13.0, 17.0))
                    .color(Color::hex(0xc8cad0, 1.0)),
            );
        root.child(toast)
    }
}

const SURFACE: Size = Size::new(352.0, 132.0);

/// Where the toast and its close button sit once settled, in surface coordinates.
fn targets(fonts: &FontRegistry, dismiss_after: f64) -> (Point, Point) {
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
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

/// Hover at 1 s (holds the timer), click the close button at 2.5 s.
fn scripted(toast_at: Point, close_at: Point) -> Scenario<Msg> {
    Scenario {
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
    }
}

fn main() {
    if vo::preview::wanted() {
        let fonts = FontRegistry::new();
        let font = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
        fonts.add_file(font.as_ref()).unwrap_or_else(|e| panic!("cannot load {font}: {e}"));
        let (toast_at, close_at) = targets(&fonts, 4.0);
        let options = move || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };
        let make = || Toast { shown: true, dismiss_after: 4.0, deadline: 0.0, timer: None };
        let preview = vo::preview::Preview::new("toast", make, options, scripted(toast_at, close_at))
            .marker("hover", 1.0)
            .marker("click", 2.5);
        std::process::exit(vo::preview::main(preview));
    }
    let mut args = std::env::args().skip(1);
    let (mut offline, mut fps, mut dismiss_after) = (None::<PathBuf>, 30.0, 4.0);
    let mut cpu_first_frame = false;
    let mut font_path = "/usr/share/fonts/noto/NotoSans-Medium.ttf".to_string();
    while let Some(a) = args.next() {
        let mut v = || args.next().unwrap_or_else(|| panic!("{a} needs a value"));
        match a.as_str() {
            "--offline" => offline = Some(v().into()),
            "--fps" => fps = v().parse().expect("--fps takes a number"),
            "--dismiss-after" => dismiss_after = v().parse().expect("--dismiss-after takes seconds"),
            "--font" => font_path = v(),
            "--cpu-first-frame" => cpu_first_frame = true,
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = FontRegistry::new();
    fonts.add_file(font_path.as_ref()).unwrap_or_else(|e| panic!("cannot load {font_path}: {e}"));
    let (toast_at, close_at) = targets(&fonts, dismiss_after);
    eprintln!(
        "toast: surface {}x{}, toast centre ({:.1}, {:.1}), close button centre ({:.1}, {:.1})",
        SURFACE.w, SURFACE.h, toast_at.x, toast_at.y, close_at.x, close_at.y
    );
    let app = Toast { shown: true, dismiss_after, deadline: 0.0, timer: None };
    let options = || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };

    if let Some(dir) = offline {
        let scenario = scripted(toast_at, close_at);
        let n = vo::render_offline_with(app, &scenario, fps, &dir, options()).expect("offline render");
        eprintln!("toast: wrote {n} frames to {}", dir.display());
        eprintln!("toast: heap peak {:.2} MB", PEAK.load(Ordering::Relaxed) as f64 / 1048576.0);
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
    options.cpu_first_frame = cpu_first_frame;
    let result = vo::run_overlay_with(app, spec, options);
    eprintln!("toast: heap peak {:.2} MB", PEAK.load(Ordering::Relaxed) as f64 / 1048576.0);
    if let Err(e) = result {
        eprintln!("toast: {e}");
        std::process::exit(1);
    }
}
