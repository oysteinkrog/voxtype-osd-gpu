//! A dictation popup built from scene elements only: a particle ring that follows the voice
//! level inside a bloom layer, and live text whose words form from particles. The app sets
//! levels and words; the renderer runs the particles.
//!
//! Live, on Wayland (only inside a nested compositor when run by an agent):
//!   cargo run -p vo --example voice_particles
//! Offline, to PNG frames:
//!   cargo run -p vo --example voice_particles -- --offline /var/tmp/claude/voice-particles [--fps 60]
//! In the preview and scrub tool:
//!   cargo run -p vo --example voice_particles -- --preview
//!
//! Options: `--font PATH`.

use std::path::PathBuf;
use std::sync::Arc;

use vo::scene::element::{Element, TextStyle};
use vo::scene::emitter::{Emitter, RingEmitter, WordParticles};
use vo::scene::geom::{Color, Size};
use vo::{App, Cx, Edges, FontRegistry, InputMode, Options, OverlaySpec, Placement, Scenario};
use vo_core::words::WordModel;

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Level(f32),
    Words(&'static str),
    Burst,
    Done,
}

/// What a recogniser and a microphone would send, as `(seconds, message)`.
fn script() -> Vec<(f64, Msg)> {
    let mut s = Vec::new();
    // Levels at 30 per second: syllables inside a slower phrase envelope.
    for i in 0..150 {
        let t = 0.1 + i as f64 / 30.0;
        let phrase = (0.5 + 0.5 * (t * 1.9).sin()) as f32;
        let syllable = (0.5 + 0.5 * (t * 12.0).sin()).powi(3) as f32;
        s.push((t, Msg::Level((0.1 + 0.9 * phrase * syllable).min(1.0))));
    }
    let words = [
        (0.5, "particles"),
        (0.9, "particles gather"),
        (1.3, "particles gather into"),
        (1.7, "particles gather into wards"),
        (2.1, "particles gather into words"),
        (2.6, "particles gather into words as"),
        (3.0, "particles gather into words as you"),
        (3.4, "particles gather into words as you speak"),
    ];
    s.extend(words.iter().map(|&(t, w)| (t, Msg::Words(w))));
    s.push((3.9, Msg::Burst));
    s.push((5.0, Msg::Done));
    s
}

struct Popup {
    ring: RingEmitter,
    model: WordModel,
    bursts: Vec<f64>,
}

impl Popup {
    fn new() -> Popup {
        let mut ring = RingEmitter::new(46.0);
        ring.size = 1.3;
        Popup { ring, model: WordModel::new(), bursts: Vec::new() }
    }
}

impl App for Popup {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        for (t, m) in script() {
            cx.after(t, m);
        }
    }

    fn update(&mut self, msg: Msg, cx: &mut Cx<Msg>) {
        let t = cx.t();
        match msg {
            Msg::Level(l) => self.ring.level.push(t, l),
            Msg::Words(w) => {
                self.model.update(w);
            }
            Msg::Burst => self.bursts.push(t),
            Msg::Done => cx.exit(),
        }
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        let mut ring = self.ring.clone();
        ring.bursts = Arc::from(self.bursts.as_slice());
        let emitter = Emitter::new(Arc::new(ring), 6144, 11);
        let glow = Element::layer("glow").bloom(1.4, 12.0).child(Element::emitter("ring", emitter).size(412.0, 130.0));
        let text = Element::live_text("live", self.model.words())
            .text_style(TextStyle::new(0, 22.0, 30.0))
            .color(Color::hex(0xf2f6ff, 1.0))
            .word_particles(WordParticles { color: Color::hex(0x9fd4ff, 1.0), ..WordParticles::default() });
        Element::column("root").pad(14.0).gap(6.0).radius(18.0).fill(Color::hex(0x0d1018, 0.92)).child(glow).child(text)
    }
}

const SURFACE: Size = Size::new(440.0, 210.0);

fn scenario() -> Scenario<Msg> {
    Scenario { size: SURFACE, scale: 1.0, duration: 5.0, inputs: Vec::new() }
}

fn main() {
    let mut font_path = "/usr/share/fonts/noto/NotoSans-Medium.ttf".to_string();
    if vo::preview::wanted() {
        let fonts = FontRegistry::new();
        fonts.add_file(font_path.as_ref()).unwrap_or_else(|e| panic!("cannot load {font_path}: {e}"));
        let options = move || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };
        let preview = vo::preview::Preview::new("voice_particles", Popup::new, options, scenario())
            .marker("wards", 1.7)
            .marker("words", 2.1)
            .marker("burst", 3.9);
        std::process::exit(vo::preview::main(preview));
    }
    let mut args = std::env::args().skip(1);
    let (mut offline, mut fps) = (None::<PathBuf>, 60.0);
    while let Some(a) = args.next() {
        let mut v = || args.next().unwrap_or_else(|| panic!("{a} needs a value"));
        match a.as_str() {
            "--offline" => offline = Some(v().into()),
            "--fps" => fps = v().parse().expect("--fps takes a number"),
            "--font" => font_path = v(),
            _ => panic!("unknown option {a}"),
        }
    }
    let fonts = FontRegistry::new();
    fonts.add_file(font_path.as_ref()).unwrap_or_else(|e| panic!("cannot load {font_path}: {e}"));
    let options = || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };
    if let Some(dir) = offline {
        let n = vo::render_offline_with(Popup::new(), &scenario(), fps, &dir, options()).expect("offline render");
        eprintln!("voice_particles: wrote {n} frames to {}", dir.display());
        return;
    }
    let spec = OverlaySpec {
        size: SURFACE,
        placement: Placement::Anchored { edges: Edges::TOP, margin: [24, 0, 0, 0] },
        input: InputMode::Regions(Default::default()),
        app_id: "vo-voice-particles".into(),
        ..OverlaySpec::default()
    };
    if let Err(e) = vo::run_overlay_with(Popup::new(), spec, options()) {
        eprintln!("voice_particles: {e}");
        std::process::exit(1);
    }
}
