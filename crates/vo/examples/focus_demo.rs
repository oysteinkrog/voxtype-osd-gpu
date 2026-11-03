//! Focus order demo (F4c): three text fields. Tab and Shift+Tab move keyboard focus between
//! them, the focused field draws a ring, and typed keys go to the focused field.
//!
//! In the preview and scrub tool, with the scripted typing:
//!   cargo run -p vo --example focus_demo -- --preview
//! Offline, to PNG frames:
//!   cargo run -p vo --example focus_demo -- --offline /var/tmp/claude/focus-frames [--fps 30]
//! Live, on Wayland (agents: only inside a nested compositor), takes keyboard focus through a
//! focus proxy:
//!   cargo run -p vo --example focus_demo
//!
//! Script: type "kiwi", Tab, "lime", Tab, "fig", Shift+Tab, "s", Ctrl+A, Ctrl+C, Tab, Ctrl+V,
//! Escape. The fields end as "kiwi", "limes" and "figlimes". Ctrl+C, Ctrl+X and Ctrl+V use
//! the clipboard (offline, the driver's own).

use std::path::PathBuf;

use vo::scene::element::{Element, TextStyle};
use vo::scene::focus::FocusRing;
use vo::scene::geom::{Color, Size};
use vo::{App, Cx, FocusProxySpec, FontRegistry, Input, Options, OverlaySpec, Scenario};
use vo_core::edit::{Dir, EditAction, EditBuffer, Unit};
use vo_core::edit_keymap::{EditCmd, TextBoxKeys};
use vo_core::key::{KeyEvent, LogicalKey, Mods, NamedKey};

const SURFACE: Size = Size::new(360.0, 196.0);
const LABELS: [&str; 3] = ["Fruit", "Colour", "Note"];

#[derive(Clone, Debug)]
enum Msg {
    Key(usize, KeyEvent),
    Focus(usize, bool),
}

struct Fields {
    bufs: [EditBuffer; 3],
    keys: TextBoxKeys,
    focused: Option<usize>,
    live: bool,
}

impl Fields {
    fn new(live: bool) -> Fields {
        Fields { bufs: std::array::from_fn(|_| EditBuffer::new("")), keys: TextBoxKeys::default(), focused: None, live }
    }
}

impl App for Fields {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        if self.live {
            cx.take_keyboard_focus(FocusProxySpec { app_id: "vo-focus-demo".into(), output: None });
        }
    }

    fn update(&mut self, msg: Msg, cx: &mut Cx<Msg>) {
        match msg {
            Msg::Focus(i, gained) => {
                self.focused = if gained { Some(i) } else { self.focused.filter(|&f| f != i) };
                self.keys.reset();
            }
            Msg::Key(i, ev) => match self.keys.map(&ev, false) {
                Some(EditCmd::Edit(action)) => {
                    self.bufs[i].apply_plain(action, ev.t);
                }
                Some(EditCmd::Resume | EditCmd::Finish) => {
                    let texts: Vec<&str> = self.bufs.iter().map(EditBuffer::text).collect();
                    eprintln!("focus_demo: fields {texts:?}");
                    cx.release_keyboard_focus();
                    cx.exit();
                }
                Some(EditCmd::Copy) => cx.copy(selected(&self.bufs[i])),
                Some(EditCmd::Cut) => {
                    cx.copy(selected(&self.bufs[i]));
                    self.bufs[i].apply_plain(EditAction::Delete(Dir::Back, Unit::Grapheme), ev.t);
                }
                Some(EditCmd::Paste) => cx.paste(),
                Some(EditCmd::Collapse) | None => {}
            },
        }
    }

    fn keyboard_focus(&mut self, gained: bool, _: bool, _: &mut Cx<Msg>) {
        eprintln!("focus_demo: keyboard focus {gained}");
    }

    /// Pasted text goes into the focused field.
    fn clipboard(&mut self, text: Option<String>, cx: &mut Cx<Msg>) {
        if let (Some(i), Some(text)) = (self.focused, text) {
            self.bufs[i].apply_plain(EditAction::Insert(text), cx.t());
        }
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        let ring = FocusRing::new(Color::hex(0x7aa2ff, 0.9), 2.0, 3.0);
        let on_key: [fn(KeyEvent) -> Msg; 3] = [|e| Msg::Key(0, e), |e| Msg::Key(1, e), |e| Msg::Key(2, e)];
        let mut col = Element::column("root").pad(16.0).gap(12.0);
        for (i, label) in LABELS.iter().enumerate() {
            let text = self.bufs[i].text();
            let shown = if self.focused == Some(i) { format!("{text}|") } else { text.to_string() };
            let field = Element::panel(format!("field{i}"))
                .width(328.0)
                .pad(8.0)
                .radius(8.0)
                .fill(Color::hex(0x24262b, 0.96))
                .stroke(Color::hex(0xffffff, 0.12), 1.0)
                .on_key(on_key[i])
                .on_focus(Msg::Focus(i, true), Msg::Focus(i, false))
                .focus_ring(ring)
                .child(
                    Element::row("row")
                        .gap(10.0)
                        .child(
                            Element::label("label", *label)
                                .text_style(TextStyle::new(0, 13.0, 17.0))
                                .color(Color::hex(0x9a9ca3, 1.0))
                                .width(56.0),
                        )
                        .child(
                            Element::label("text", shown).text_style(TextStyle::new(0, 15.0, 19.0)).color(Color::WHITE),
                        ),
                );
            col = col.child(field);
        }
        col
    }
}

/// The selected text of `buf`; empty with a caret.
fn selected(buf: &EditBuffer) -> String {
    let sel = buf.selection();
    buf.text()[sel.anchor.min(sel.focus)..sel.anchor.max(sel.focus)].to_string()
}

/// Type into the first field, Tab, Tab, Shift+Tab, copy, Tab, paste, then Escape.
fn scripted() -> Scenario<Msg> {
    let mut s = Scenario { size: SURFACE, scale: 1.0, duration: 8.0, inputs: Vec::new() };
    s.push(0.3, Input::KeyboardFocus { gained: true, requested: false });
    let tab = LogicalKey::Named(NamedKey::Tab);
    let t = s.type_text(0.6, "kiwi");
    let t = s.key(t + 0.4, tab, None, Mods::NONE);
    let t = s.type_text(t + 0.3, "lime");
    let t = s.key(t + 0.4, tab, None, Mods::NONE);
    let t = s.type_text(t + 0.3, "fig");
    let t = s.chord(t + 0.4, Mods::SHIFT, tab);
    let t = s.type_text(t + 0.3, "s");
    let t = s.chord(t + 0.4, Mods::CTRL, LogicalKey::Char('a'));
    let t = s.chord(t + 0.2, Mods::CTRL, LogicalKey::Char('c'));
    let t = s.key(t + 0.3, tab, None, Mods::NONE);
    let t = s.chord(t + 0.3, Mods::CTRL, LogicalKey::Char('v'));
    s.key(t + 0.8, LogicalKey::Named(NamedKey::Escape), None, Mods::NONE);
    s
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let font = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(font.as_ref()).unwrap_or_else(|e| panic!("cannot load {font}: {e}"));
    fonts
}

fn main() {
    if vo::preview::wanted() {
        let fonts = fonts();
        let options = move || Options::<Msg> { fonts: fonts.clone(), ..Options::default() };
        let preview = vo::preview::Preview::new("focus_demo", || Fields::new(false), options, scripted())
            .marker("tab", 1.2)
            .marker("shift-tab", 3.0);
        std::process::exit(vo::preview::main(preview));
    }
    let mut args = std::env::args().skip(1);
    let (mut offline, mut fps) = (None::<PathBuf>, 30.0);
    while let Some(a) = args.next() {
        let mut v = || args.next().unwrap_or_else(|| panic!("{a} needs a value"));
        match a.as_str() {
            "--offline" => offline = Some(v().into()),
            "--fps" => fps = v().parse().expect("--fps takes a number"),
            // Read by the host: open on the real desktop on purpose.
            "--real-display" => {}
            _ => panic!("unknown option {a}"),
        }
    }
    let options = Options::<Msg> { fonts: fonts(), ..Options::default() };
    if let Some(dir) = offline {
        let n = vo::render_offline_with(Fields::new(false), &scripted(), fps, &dir, options).expect("offline render");
        eprintln!("focus_demo: wrote {n} frames to {}", dir.display());
        return;
    }
    let spec = OverlaySpec { size: SURFACE, app_id: "vo-focus-demo".into(), ..OverlaySpec::default() };
    if let Err(e) = vo::run_overlay_with(Fields::new(true), spec, options) {
        eprintln!("focus_demo: {e}");
        std::process::exit(1);
    }
}
