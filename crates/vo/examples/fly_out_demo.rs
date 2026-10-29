//! Effects that leave the popup (EG-08): a margin that follows the show, and a fly-out.
//!
//! A small card shows. At 2.5 s it gets a glow of radius 40, so the surface grows to make room;
//! at 4 s the glow goes and the surface shrinks back to the first view's size once the glow
//! has faded. At 7 s the card is sent: it leaves the panel and flies 800 px up on a
//! full-screen click-through fly-out surface, fading as it goes, and the app exits. Under
//! `VO_REDUCED_MOTION=1` the card fades in place and no fly-out opens.
//!
//!   VO_SURFACE_LOG=1 cargo run -p vo --example fly_out_demo                 live (nested KWin only)
//!   cargo run -p vo --example fly_out_demo -- --offline DIR [--fps 10]
//!
//! Live, `VO_SURFACE_LOG=1` logs the main surface's size at the start and at each margin
//! change, and the diagnostics log when the fly-out opens and closes.

use std::path::PathBuf;

use vo::scene::element::{Element, Prop, TextStyle, Transition};
use vo::scene::geom::{Color, Point, Size};
use vo::{
    App, Cx, FontRegistry, InputMode, LogDiagnostics, Options, OverlaySpec, Placement, Scenario, SurfaceView, Tokens,
};
use vo_core::spring::Spring;

const PANEL: Size = Size::new(320.0, 96.0);
const PAD: f32 = 16.0;

fn accent() -> Color {
    Color::hex(0x4cc9f0, 1.0)
}

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Glow(bool),
    Send,
}

struct Demo {
    glow: bool,
    sent: bool,
    reduced: bool,
}

impl Demo {
    fn card(&self) -> Element<Msg> {
        let card = Element::panel("card")
            .size(PANEL.w - 2.0 * PAD, PANEL.h - 2.0 * PAD)
            .pad(14.0)
            .radius(12.0)
            .fill(Color::hex(0x101319, 0.96))
            .stroke(accent().fade(0.4), 1.0)
            .child(
                Element::label("text", "Sent to the editor")
                    .text_style(TextStyle::new(0, 18.0, 24.0))
                    .color(Color::WHITE),
            );
        if self.glow {
            card.glow(accent(), 40.0, 1.0)
        } else {
            card
        }
    }
}

impl App for Demo {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        // Late enough that the first frame is on screen even when the GPU starts slowly.
        cx.after(2.5, Msg::Glow(true));
        cx.after(4.0, Msg::Glow(false));
        cx.after(7.0, Msg::Send);
    }

    fn update(&mut self, msg: Msg, cx: &mut Cx<Msg>) {
        match msg {
            Msg::Glow(on) => self.glow = on,
            Msg::Send => {
                self.sent = true;
                cx.exit();
            }
        }
    }

    fn view(&self, _: &Tokens) -> Element<Msg> {
        let root = Element::column("root").size(PANEL.w, PANEL.h).pad(PAD);
        if self.sent {
            return root;
        }
        // Under reduced motion the card fades where it is; otherwise it leaves the panel at
        // once and its copy on the fly-out takes over.
        let exit = if self.reduced { Transition::fade() } else { Transition::NONE };
        root.child(self.card().presence(Transition::fade(), exit))
    }

    fn surfaces(&self, _: &Tokens) -> Vec<SurfaceView<Msg>> {
        if !self.sent || self.reduced {
            return Vec::new();
        }
        let flying = self
            .card()
            .offset(0.0, -800.0)
            .opacity(0.0)
            .presence(Transition::NONE.offset(0.0, 0.0).opacity(1.0), Transition::fade())
            .motion(Prop::Offset, Spring::critical_to_98(0.9))
            .motion(Prop::Opacity, Spring::critical_to_98(1.0));
        vec![SurfaceView::fly_out("sent", Point::new(PAD, PAD), flying)]
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut offline, mut fps) = (None::<PathBuf>, 10.0);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--offline" => offline = Some(args.next().expect("--offline DIR").into()),
            "--fps" => fps = args.next().and_then(|v| v.parse().ok()).expect("--fps N"),
            other => panic!("unknown argument {other}"),
        }
    }
    let fonts = FontRegistry::new();
    fonts.add_system("Noto Sans").expect("Noto Sans through fontconfig");
    let options = Options::<Msg> { fonts, ..Options::default() };
    let app = Demo { glow: false, sent: false, reduced: options.reduced_motion };

    if let Some(dir) = offline {
        let scenario = Scenario { size: PANEL, scale: 1.0, duration: 10.5, inputs: Vec::new() };
        let n = vo::render_offline_with(app, &scenario, fps, &dir, options).expect("offline render");
        eprintln!("fly_out_demo: wrote {n} frames to {}, the fly-out in its subfolder", dir.display());
        return;
    }

    let spec = OverlaySpec {
        size: PANEL,
        placement: Placement::LineAt { fraction: 0.85, y: PANEL.h / 2.0 },
        input: InputMode::None,
        app_id: "vo-fly-out-demo".into(),
        ..OverlaySpec::default()
    };
    let mut options = options;
    options.diagnostics = Some(Box::new(LogDiagnostics::new(None)));
    if let Err(e) = vo::run_overlay_with(app, spec, options) {
        eprintln!("fly_out_demo: {e}");
        std::process::exit(1);
    }
}
