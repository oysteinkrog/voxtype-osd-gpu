//! The element path: the same popup as a plain vo `App` whose panel is
//! `Element::panel(..).glass(..)`. The framework draws the glass in the display list pass,
//! sends the compositor blur region and paces the orbit at 30 fps by itself.

use std::path::Path;

use vo::scene::element::Element;
use vo::{App, Cx, LogDiagnostics, Options, OverlaySpec, Placement, Scenario};

use crate::{layout, load_fonts, view, Look};

#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Close,
}

pub struct GlassApp {
    look: Look,
    secs: f64,
    shown: bool,
}

impl App for GlassApp {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        cx.after(self.secs, Msg::Close);
    }

    fn update(&mut self, msg: Msg, cx: &mut Cx<Msg>) {
        match msg {
            Msg::Close => {
                // The orbit moves for as long as the panel is shown, so it goes first.
                self.shown = false;
                cx.exit();
            }
        }
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        if !self.shown {
            return Element::column("root");
        }
        let glass = self.look.glass.with_orbit(self.look.hues[0]);
        view(&self.look, 0.0, 0.0, Some(glass))
    }
}

fn options() -> Options<Msg> {
    Options { fonts: load_fonts(), ..Options::default() }
}

/// Renders `secs` seconds at 30 fps to PNG frames in `dir`.
pub fn offline(look: Look, secs: f64, dir: &Path) {
    let (_, size, _) = layout(&load_fonts(), &look);
    let scenario = Scenario { size, scale: 1.0, duration: secs, inputs: Vec::new() };
    let n = vo::render_offline_with(GlassApp { look, secs, shown: true }, &scenario, 30.0, dir, options())
        .expect("offline render");
    println!("glass_demo app: wrote {n} frames to {}", dir.display());
}

/// Runs the popup live for `secs` seconds through `vo::run_overlay_with`.
pub fn live(look: Look, secs: f64) {
    let (_, size, _) = layout(&load_fonts(), &look);
    let spec = OverlaySpec {
        size,
        placement: Placement::FromTop { fraction: 0.2 },
        input: vo::InputMode::None,
        app_id: "vo-glass-demo".into(),
        ..OverlaySpec::default()
    };
    let mut options = options();
    options.diagnostics = Some(Box::new(LogDiagnostics::new(None)));
    if let Err(e) = vo::run_overlay_with(GlassApp { look, secs, shown: true }, spec, options) {
        eprintln!("glass_demo app: {e}");
        std::process::exit(1);
    }
}
