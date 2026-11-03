//! Clipboard offline (F5): without a host the driver keeps the copied text and answers a
//! paste with it at once.

use std::sync::{Arc, Mutex};

use vo::scene::element::Element;
use vo::scene::geom::Size;
use vo::{exit_check, App, Cx, Input, Options, Scenario};
use vo_core::key::{KeyEvent, KeyState, LogicalKey, Mods, NamedKey};

/// Ctrl+C copies the next copy word, Ctrl+V pastes, Escape exits. Pastes are logged.
struct Clip {
    words: Vec<&'static str>,
    pasted: Arc<Mutex<Vec<Option<String>>>>,
}

impl App for Clip {
    type Msg = ();

    fn update(&mut self, _: (), _: &mut Cx<()>) {}

    fn view(&self, _: &vo::Tokens) -> Element<()> {
        Element::column("root")
    }

    fn key(&mut self, ev: &KeyEvent, cx: &mut Cx<()>) -> bool {
        if ev.state != KeyState::Down {
            return false;
        }
        match ev.key {
            LogicalKey::Char('c') if ev.mods.ctrl => {
                let word = self.words.remove(0);
                cx.copy(word);
            }
            LogicalKey::Char('v') if ev.mods.ctrl => cx.paste(),
            LogicalKey::Named(NamedKey::Escape) => cx.exit(),
            _ => return false,
        }
        true
    }

    fn clipboard(&mut self, text: Option<String>, _: &mut Cx<()>) {
        self.pasted.lock().unwrap().push(text);
    }
}

#[test]
fn copy_then_paste_offline() {
    let mut s = Scenario { size: Size::new(100.0, 40.0), scale: 1.0, duration: 4.0, inputs: Vec::new() };
    s.push(0.1, Input::KeyboardFocus { gained: true, requested: false });
    let ctrl = |s: &mut Scenario<()>, t: f64, c: char| s.chord(t, Mods::CTRL, LogicalKey::Char(c));
    let t = ctrl(&mut s, 0.2, 'v');
    let t = ctrl(&mut s, t + 0.1, 'c');
    let t = ctrl(&mut s, t + 0.1, 'v');
    let t = ctrl(&mut s, t + 0.1, 'c');
    let t = ctrl(&mut s, t + 0.1, 'v');
    let t = ctrl(&mut s, t + 0.1, 'v');
    s.key(t + 0.1, LogicalKey::Named(NamedKey::Escape), None, Mods::NONE);

    let pasted = Arc::new(Mutex::new(Vec::new()));
    let app = Clip { words: vec!["plum", "pear"], pasted: pasted.clone() };
    let report = exit_check(app, &s, 60.0, Options::default());
    assert!(report.ending.after_exit().is_some());
    let some = |w: &str| Some(w.to_string());
    assert_eq!(*pasted.lock().unwrap(), [None, some("plum"), some("pear"), some("pear")]);
}
