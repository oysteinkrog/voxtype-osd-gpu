//! Focus order offline (F4c): three fields, Tab, Tab, Shift+Tab land where expected, and
//! each field gets only the keys typed while it has focus.

use std::sync::{Arc, Mutex};

use vo::scene::element::Element;
use vo::scene::geom::Size;
use vo::{exit_check, App, Cx, Input, Options, Scenario};
use vo_core::key::{KeyEvent, KeyState, LogicalKey, Mods, NamedKey};

#[derive(Clone, Debug)]
enum Msg {
    Key(usize, KeyEvent),
    Focus(usize, bool),
}

#[derive(Default)]
struct Seen {
    text: [String; 3],
    /// Focus messages in order, as (field, gained).
    focus: Vec<(usize, bool)>,
}

struct Fields(Arc<Mutex<Seen>>);

impl App for Fields {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, cx: &mut Cx<Msg>) {
        let mut seen = self.0.lock().unwrap();
        match msg {
            Msg::Focus(i, gained) => seen.focus.push((i, gained)),
            Msg::Key(_, ev) if ev.key == LogicalKey::Named(NamedKey::Escape) => cx.exit(),
            Msg::Key(i, ev) => {
                if ev.state == KeyState::Down {
                    if let Some(t) = &ev.text {
                        seen.text[i].push_str(t);
                    }
                }
            }
        }
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        let on_key: [fn(KeyEvent) -> Msg; 3] = [|e| Msg::Key(0, e), |e| Msg::Key(1, e), |e| Msg::Key(2, e)];
        let field = |i: usize| {
            Element::panel(format!("f{i}"))
                .size(100.0, 20.0)
                .on_key(on_key[i])
                .on_focus(Msg::Focus(i, true), Msg::Focus(i, false))
        };
        Element::column("root").child(field(0)).child(Element::row("row").child(field(1))).child(field(2))
    }
}

#[test]
fn tab_tab_shift_tab_lands_on_the_middle_field() {
    let mut s = Scenario { size: Size::new(200.0, 120.0), scale: 1.0, duration: 5.0, inputs: Vec::new() };
    s.push(0.1, Input::KeyboardFocus { gained: true, requested: false });
    let tab = LogicalKey::Named(NamedKey::Tab);
    let t = s.type_text(0.2, "ab");
    let t = s.key(t + 0.1, tab, None, Mods::NONE);
    let t = s.type_text(t + 0.1, "cd");
    let t = s.key(t + 0.1, tab, None, Mods::NONE);
    let t = s.type_text(t + 0.1, "ef");
    let t = s.chord(t + 0.1, Mods::SHIFT, tab);
    let t = s.type_text(t + 0.1, "g");
    s.key(t + 0.1, LogicalKey::Named(NamedKey::Escape), None, Mods::NONE);

    let seen = Arc::new(Mutex::new(Seen::default()));
    let report = exit_check(Fields(seen.clone()), &s, 60.0, Options::default());
    assert!(report.ending.after_exit().is_some(), "Escape reached the focused field: {:?}", report.ending);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.text, ["ab".to_string(), "cdg".into(), "ef".into()]);
    assert_eq!(
        seen.focus,
        [(0, true), (0, false), (1, true), (1, false), (2, true), (2, false), (1, true)],
        "first field at start, then Tab, Tab, Shift+Tab"
    );
}
