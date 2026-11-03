//! Keyboard focus over the scene's focusable nodes (`Element::on_key`), as in
//! `docs/INPUT-MODEL.md` "Focus order".
//!
//! Tab and Shift+Tab move focus through the tab order: nodes with a positive tab index
//! first, lowest first, then the nodes with index 0 (the default) in document order. A
//! negative index takes a node out of the tab order; it can still hold focus when no other
//! node can. Focus stays on a node for as long as a new view keeps its key, and focus
//! changes reach the app as the node's `Element::on_focus` messages.

use crate::geom::Color;
use crate::input::{KeyEvent, KeyState, LogicalKey, NamedKey};
use crate::scene::{NodeId, Scene};

/// A ring drawn around the focused node, outside its box (`Element::focus_ring`). Looks
/// pick their own colour and width; nothing draws a ring by default.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FocusRing {
    pub color: Color,
    /// Stroke width in logical pixels.
    pub width: f32,
    /// Space between the node's box and the ring's centre line.
    pub gap: f32,
}

impl FocusRing {
    pub fn new(color: Color, width: f32, gap: f32) -> FocusRing {
        FocusRing { color, width, gap }
    }
}

/// Where a key went (`Scene::key`).
#[derive(Clone, Debug, PartialEq)]
pub enum KeyRoute<M> {
    /// The focused node's message for the key.
    Node(M),
    /// Tab or Shift+Tab moved focus (or this is the release of such a Tab). Take the
    /// focus messages with `Scene::take_focus_msgs`. `redraw` is set when a focus ring
    /// appeared or went.
    Focus { redraw: bool },
    /// No node has focus: the key is the app's.
    App,
}

/// The scene's focus state.
#[derive(Debug, Default)]
pub(crate) struct Focus {
    pub focused: Option<NodeId>,
    /// Focus changes not yet turned into messages: the node and whether focus came to it.
    pending: Vec<(NodeId, bool)>,
    /// A Tab press moved focus, so its release goes nowhere either.
    tab_held: bool,
}

impl<M> Scene<M> {
    pub(crate) fn focusable(&self, id: NodeId) -> bool {
        self.nodes.get(id).is_some_and(|n| n.leaving.is_none() && n.handlers.key.is_some())
    }

    /// Keeps focus on its node when that node is still focusable, else moves it to the
    /// first node in the tab order (or the first focusable node when all opted out).
    /// Returns whether a focus ring appeared or went.
    pub(crate) fn refocus(&mut self) -> bool {
        let old = self.focus.focused;
        if old.is_some_and(|id| self.focusable(id)) {
            return false;
        }
        // Most views have no focusable node: no walk, no allocation.
        let any = self.nodes.values().any(|n| n.leaving.is_none() && n.handlers.key.is_some());
        let new = if any {
            let order = self.document_order();
            self.tab_order_of(&order).first().copied().or_else(|| order.first().copied())
        } else {
            None
        };
        self.set_focus(new)
    }

    /// Moves focus to `new`, queueing the leave and enter. Returns whether a ring changed.
    fn set_focus(&mut self, new: Option<NodeId>) -> bool {
        let old = self.focus.focused;
        if old == new {
            return false;
        }
        self.focus.focused = new;
        let ring = |id: Option<NodeId>| id.and_then(|id| self.nodes.get(id)).is_some_and(|n| n.handlers.ring.is_some());
        let redraw = ring(old) || ring(new);
        if let Some(id) = old.filter(|&id| self.nodes.contains_key(id)) {
            self.focus.pending.push((id, false));
        }
        if let Some(id) = new {
            self.focus.pending.push((id, true));
        }
        redraw
    }

    /// The focusable nodes in document order: depth first, in layout order.
    fn document_order(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.root.into_iter().collect();
        while let Some(id) = stack.pop() {
            if self.focusable(id) {
                out.push(id);
            }
            if let Some(n) = self.nodes.get(id) {
                stack.extend(n.flow.iter().rev());
            }
        }
        out
    }

    /// The tab order of focusable nodes given in document order: positive tab indexes
    /// first, ascending, then index 0 in document order. Negative indexes are left out.
    fn tab_order_of(&self, doc: &[NodeId]) -> Vec<NodeId> {
        let index = |id: NodeId| self.nodes[id].handlers.tab_index.unwrap_or(0);
        let mut order: Vec<NodeId> = doc.iter().copied().filter(|&id| index(id) >= 0).collect();
        // Stable: equal indexes keep document order.
        order.sort_by_key(|&id| match index(id) {
            0 => (1, 0),
            i => (0, i),
        });
        order
    }

    /// The focusable nodes in tab order.
    pub fn tab_order(&self) -> Vec<NodeId> {
        self.tab_order_of(&self.document_order())
    }

    /// The node that has keyboard focus, if any node is focusable.
    pub fn focused(&self) -> Option<NodeId> {
        self.focus.focused
    }

    /// Moves focus one step through the tab order, forward or back, wrapping around.
    /// Returns `None` when there is no other node to move to, else whether a ring changed.
    pub fn focus_step(&mut self, back: bool) -> Option<bool> {
        let order = self.tab_order();
        let at = self.focus.focused;
        if !order.iter().any(|&id| Some(id) != at) {
            return None;
        }
        let len = order.len();
        let i = match (order.iter().position(|&id| Some(id) == at), back) {
            (Some(p), false) => (p + 1) % len,
            (Some(p), true) => (p + len - 1) % len,
            (None, false) => 0,
            (None, true) => len - 1,
        };
        Some(self.set_focus(Some(order[i])))
    }
}

impl<M: Clone> Scene<M> {
    /// Routes a key. Tab and Shift+Tab (without Ctrl, Alt or Super) move focus when there is
    /// another node in the tab order to move to; with one focusable node Tab goes to it like
    /// any key. Other keys go to the focused node, or to the app when no node has focus.
    pub fn key(&mut self, ev: &KeyEvent) -> KeyRoute<M> {
        let m = ev.mods;
        if ev.key == LogicalKey::Named(NamedKey::Tab) && !(m.ctrl || m.alt || m.logo) {
            match ev.state {
                KeyState::Down | KeyState::Repeat => {
                    if let Some(redraw) = self.focus_step(m.shift) {
                        self.focus.tab_held = true;
                        return KeyRoute::Focus { redraw };
                    }
                }
                KeyState::Up if self.focus.tab_held => {
                    self.focus.tab_held = false;
                    return KeyRoute::Focus { redraw: false };
                }
                KeyState::Up => {}
            }
        }
        match self.focus.focused.and_then(|id| self.nodes.get(id)).and_then(|n| n.handlers.key) {
            Some(make) => KeyRoute::Node(make(ev.clone())),
            None => KeyRoute::App,
        }
    }

    /// The focus leave and enter messages (`Element::on_focus`) since the last call, in
    /// order: the old node's leave before the new node's enter.
    pub fn take_focus_msgs(&mut self) -> Vec<M> {
        if self.focus.pending.is_empty() {
            return Vec::new();
        }
        std::mem::take(&mut self.focus.pending)
            .into_iter()
            .filter_map(|(id, enter)| {
                let (on, off) = self.nodes.get(id)?.handlers.focus.as_ref()?;
                Some(if enter { on.clone() } else { off.clone() })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::Element;
    use crate::input::Mods;

    #[derive(Clone, Debug, PartialEq)]
    enum Msg {
        Key(KeyEvent),
        In(&'static str),
        Out(&'static str),
    }

    fn ev(t: f64, key: LogicalKey, state: KeyState, mods: Mods) -> KeyEvent {
        KeyEvent { surface: 1, t, state, key, code: 30, text: None, mods }
    }

    fn a(t: f64) -> KeyEvent {
        KeyEvent { text: Some("a".into()), ..ev(t, LogicalKey::Char('a'), KeyState::Down, Mods::NONE) }
    }

    fn tab(scene: &mut Scene<Msg>, t: f64, shift: bool) -> KeyRoute<Msg> {
        let mods = Mods { shift, ..Mods::NONE };
        let r = scene.key(&ev(t, LogicalKey::Named(NamedKey::Tab), KeyState::Down, mods));
        scene.key(&ev(t, LogicalKey::Named(NamedKey::Tab), KeyState::Up, mods));
        r
    }

    fn field(name: &'static str) -> Element<Msg> {
        Element::panel(name).on_key(Msg::Key).on_focus(Msg::In(name), Msg::Out(name))
    }

    fn focused_key(scene: &Scene<Msg>) -> Option<String> {
        let id = scene.focused()?;
        Some(scene.nodes[id].key.to_string())
    }

    fn three(order: [i32; 3]) -> Element<Msg> {
        Element::column("root")
            .child(Element::label("hint", "x"))
            .child(field("a").tab_index(order[0]))
            .child(Element::row("row").child(field("b").tab_index(order[1])))
            .child(field("c").tab_index(order[2]))
    }

    #[test]
    fn keys_reach_the_focused_node_and_a_removed_node_loses_focus() {
        let mut scene: Scene<Msg> = Scene::new();
        scene.apply(Element::column("root").child(Element::label("hint", "x")), 0.0);
        assert_eq!(scene.focused(), None);
        assert_eq!(scene.key(&a(0.0)), KeyRoute::App, "no focusable node: the key goes to the app");

        let editable =
            Element::column("root").child(Element::label("hint", "x")).child(Element::panel("box").on_key(Msg::Key));
        scene.apply(editable.clone(), 1.0);
        let focused = scene.focused().expect("the focusable node gets focus");
        assert_eq!(scene.key(&a(1.0)), KeyRoute::Node(Msg::Key(a(1.0))));
        // A new view with the same node keeps focus on it.
        scene.apply(editable, 2.0);
        assert_eq!(scene.focused(), Some(focused));

        scene.apply(Element::column("root").child(Element::label("hint", "x")), 3.0);
        assert_eq!(scene.focused(), None);
        assert_eq!(scene.key(&a(3.0)), KeyRoute::App);
    }

    #[test]
    fn one_focusable_node_gets_tab_like_any_key() {
        let mut scene: Scene<Msg> = Scene::new();
        scene.apply(Element::column("root").child(Element::panel("box").on_key(Msg::Key)), 0.0);
        let down = ev(0.5, LogicalKey::Named(NamedKey::Tab), KeyState::Down, Mods::NONE);
        assert_eq!(scene.key(&down), KeyRoute::Node(Msg::Key(down.clone())));
        let up = ev(0.6, LogicalKey::Named(NamedKey::Tab), KeyState::Up, Mods::NONE);
        assert_eq!(scene.key(&up), KeyRoute::Node(Msg::Key(up.clone())));
    }

    #[test]
    fn tab_follows_document_order_and_wraps() {
        let mut scene: Scene<Msg> = Scene::new();
        scene.apply(three([0, 0, 0]), 0.0);
        assert_eq!(focused_key(&scene).as_deref(), Some("a"));
        assert_eq!(scene.take_focus_msgs(), vec![Msg::In("a")]);
        assert_eq!(tab(&mut scene, 1.0, false), KeyRoute::Focus { redraw: false });
        assert_eq!(focused_key(&scene).as_deref(), Some("b"), "b sits deeper but comes next");
        assert_eq!(scene.take_focus_msgs(), vec![Msg::Out("a"), Msg::In("b")]);
        tab(&mut scene, 2.0, false);
        tab(&mut scene, 3.0, false);
        assert_eq!(focused_key(&scene).as_deref(), Some("a"), "Tab on the last node wraps to the first");
        tab(&mut scene, 4.0, true);
        assert_eq!(focused_key(&scene).as_deref(), Some("c"), "Shift+Tab on the first wraps to the last");
    }

    #[test]
    fn tab_index_comes_before_document_order() {
        let mut scene: Scene<Msg> = Scene::new();
        // c (1) first, then b (2), then a (0, the default).
        scene.apply(three([0, 2, 1]), 0.0);
        assert_eq!(focused_key(&scene).as_deref(), Some("c"));
        tab(&mut scene, 1.0, false);
        assert_eq!(focused_key(&scene).as_deref(), Some("b"));
        tab(&mut scene, 2.0, false);
        assert_eq!(focused_key(&scene).as_deref(), Some("a"));
    }

    #[test]
    fn opted_out_nodes_are_skipped() {
        let mut scene: Scene<Msg> = Scene::new();
        scene.apply(three([0, -1, 0]), 0.0);
        tab(&mut scene, 1.0, false);
        assert_eq!(focused_key(&scene).as_deref(), Some("c"));
        // Opting out leaves the tab order only: c keeps focus, and with no other tab stop
        // Tab goes to it like any key.
        scene.apply(three([-1, -1, -1]), 2.0);
        assert_eq!(focused_key(&scene).as_deref(), Some("c"));
        // A fresh scene of opted-out nodes focuses the first one.
        let mut fresh: Scene<Msg> = Scene::new();
        fresh.apply(three([-1, -1, -1]), 2.0);
        assert_eq!(focused_key(&fresh).as_deref(), Some("a"));
        let down = ev(3.0, LogicalKey::Named(NamedKey::Tab), KeyState::Down, Mods::NONE);
        assert!(matches!(scene.key(&down), KeyRoute::Node(_)));
    }

    #[test]
    fn ctrl_tab_and_the_release_of_a_moving_tab() {
        let mut scene: Scene<Msg> = Scene::new();
        scene.apply(three([0, 0, 0]), 0.0);
        let ctrl = Mods { ctrl: true, ..Mods::NONE };
        let down = ev(1.0, LogicalKey::Named(NamedKey::Tab), KeyState::Down, ctrl);
        assert!(matches!(scene.key(&down), KeyRoute::Node(_)), "Ctrl+Tab is the node's");
        assert_eq!(focused_key(&scene).as_deref(), Some("a"));
        let down = ev(2.0, LogicalKey::Named(NamedKey::Tab), KeyState::Down, Mods::NONE);
        assert_eq!(scene.key(&down), KeyRoute::Focus { redraw: false });
        let up = ev(2.1, LogicalKey::Named(NamedKey::Tab), KeyState::Up, Mods::NONE);
        assert_eq!(scene.key(&up), KeyRoute::Focus { redraw: false }, "the new node never sees the release");
    }

    #[test]
    fn focus_survives_a_rebuild_that_keeps_the_key() {
        let mut scene: Scene<Msg> = Scene::new();
        scene.apply(three([0, 0, 0]), 0.0);
        tab(&mut scene, 1.0, false);
        scene.take_focus_msgs();
        // The same keys, a new label in front and b's row moved last.
        let view = Element::column("root")
            .child(Element::label("new", "y"))
            .child(Element::label("hint", "x"))
            .child(field("a"))
            .child(field("c"))
            .child(Element::row("row").child(field("b")));
        scene.apply(view, 2.0);
        assert_eq!(focused_key(&scene).as_deref(), Some("b"));
        assert_eq!(scene.take_focus_msgs(), Vec::<Msg>::new(), "no focus change, no message");
        // The order follows the new view: b is now last, so Tab wraps to a.
        tab(&mut scene, 3.0, false);
        assert_eq!(focused_key(&scene).as_deref(), Some("a"));
        // A view without the focused node: focus goes to the first node in the tab order.
        scene.take_focus_msgs();
        scene.apply(Element::column("root").child(field("c")).child(Element::row("row").child(field("b"))), 4.0);
        assert_eq!(focused_key(&scene).as_deref(), Some("c"));
        assert_eq!(scene.take_focus_msgs(), vec![Msg::In("c")], "a is gone, so only the enter");
    }

    #[test]
    fn a_focus_ring_is_drawn_only_on_the_focused_node() {
        let ring = FocusRing::new(Color::WHITE, 2.0, 3.0);
        let view = || {
            Element::column("root")
                .child(field("a").size(40.0, 20.0).focus_ring(ring))
                .child(field("b").size(40.0, 20.0).focus_ring(ring))
        };
        let plain = || Element::column("root").child(field("a").size(40.0, 20.0)).child(field("b").size(40.0, 20.0));
        let shapes = |scene: &Scene<Msg>| scene.sample(1.0).shapes;
        let mut base: Scene<Msg> = Scene::new();
        base.apply(plain(), 0.0);
        let mut scene: Scene<Msg> = Scene::new();
        scene.apply(view(), 0.0);
        let n = shapes(&base).len();
        assert_eq!(shapes(&scene).len(), n + 1, "one ring, on the focused node");
        let before = shapes(&scene).last().copied();
        assert_eq!(tab(&mut scene, 1.0, false), KeyRoute::Focus { redraw: true });
        assert_eq!(shapes(&scene).len(), n + 1);
        assert_ne!(shapes(&scene).last().copied(), before, "the ring moved to b");
    }
}
