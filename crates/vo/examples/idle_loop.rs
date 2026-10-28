//! One idle loop on a live surface, for the budget gate (RD-02): a dot breathes at the
//! ambient rate (`vo_core::ambient::AMBIENT_FPS`, 30 steps a second) and nothing else
//! happens. After `--secs` (default 10) the app asks to exit and drops the loop from its view,
//! as the idle-motion rule asks, so the dot settles to its rest and the run ends.
//!
//!   cargo run --release -p vo --example idle_loop -- [--secs S]
//!
//! Prints `idle_loop: N frames while idle in S s (F per second), exit settled after E s`.
//! `scripts/budget.sh` runs it in a private nested KWin and fails when F is over
//! `VO_BUDGET_IDLE_LOOP_FPS` (default 30).

use vo::scene::display::ShapeKind;
use vo::scene::element::Element;
use vo::scene::geom::{Color, Size};
use vo::scene::taffy::prelude::{AlignItems, JustifyContent};
use vo::scene::{Loop, LoopProp};
use vo::{App, Cx, Diagnostics, Edges, InputMode, Options, OverlaySpec, Placement};
use vo_host::HostDiag;

const SURFACE: Size = Size { w: 240.0, h: 120.0 };

struct Idle {
    secs: f64,
    exiting: bool,
}

#[derive(Clone, Debug)]
struct Exit;

impl App for Idle {
    type Msg = Exit;

    fn init(&mut self, cx: &mut Cx<Exit>) {
        cx.after(self.secs, Exit);
    }

    fn update(&mut self, _: Exit, cx: &mut Cx<Exit>) {
        self.exiting = true;
        cx.exit();
    }

    fn view(&self, _: &vo::Tokens) -> Element<Exit> {
        let accent = Color::hex(0x7dd3fc, 1.0);
        let dot = Element::shape("dot", ShapeKind::Circle).size(28.0, 28.0).fill(accent);
        // The idle loop runs only while the app is not exiting.
        let dot = if self.exiting { dot } else { dot.ambient(LoopProp::Scale, Loop::pulse(0.2, 1.6)) };
        Element::panel("root")
            .size(SURFACE.w, SURFACE.h)
            .radius(16.0)
            .fill(Color::hex(0x0b0d14, 0.95))
            .align_items(AlignItems::CENTER)
            .justify(JustifyContent::CENTER)
            .child(dot)
    }
}

/// Counts frames sampled before and after the exit.
struct Count {
    secs: f64,
    idle: u64,
    last: f64,
}

impl Diagnostics for Count {
    fn frame(&mut self, _: u64, t: f64, _: &HostDiag) {
        if t < self.secs {
            self.idle += 1;
        }
        self.last = t;
    }

    fn exit(&mut self, _: &HostDiag) {
        println!(
            "idle_loop: {} frames while idle in {} s ({:.1} per second), exit settled after {:.2} s",
            self.idle,
            self.secs,
            self.idle as f64 / self.secs,
            (self.last - self.secs).max(0.0)
        );
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut secs = 10.0;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--secs" => secs = args.next().and_then(|v| v.parse().ok()).expect("--secs takes seconds"),
            // Read by the host: open on the real desktop on purpose.
            "--real-display" => {}
            _ => panic!("unknown option {a}"),
        }
    }
    let spec = OverlaySpec {
        size: SURFACE,
        placement: Placement::Anchored { edges: Edges::TOP_RIGHT, margin: [16, 16, 0, 0] },
        input: InputMode::None,
        app_id: "vo-idle-loop".into(),
        ..OverlaySpec::default()
    };
    let options = Options::<Exit> {
        diagnostics: Some(Box::new(Count { secs, idle: 0, last: 0.0 })),
        ..Options::default()
    };
    if let Err(e) = vo::run_overlay_with(Idle { secs, exiting: false }, spec, options) {
        eprintln!("idle_loop: {e}");
        std::process::exit(1);
    }
}
