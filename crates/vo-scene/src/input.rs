//! Hit testing and pointer messages, as in `docs/INPUT-MODEL.md`.
//!
//! `hit_test` samples the scene at the event's own time, so the answer does not depend on
//! the frame rate. Enter, leave and click; and for `Element::on_pointer`, press with a click
//! count, drag and release at node-local positions.

use crate::geom::{Point, Size};

use crate::scene::{NodeId, Scene};
pub use vo_core::key::{KeyEvent, KeyState, LogicalKey, Mods, NamedKey, RepeatInfo};
pub use vo_core::text_input::{Preedit, TextInputEvent, TextInputState};

pub type SurfaceId = u32;

/// A raw pointer event from the host, in surface logical units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerEvent {
    pub surface: SurfaceId,
    pub t: f64,
    pub pos: Point,
    pub kind: PointerKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerKind {
    Enter,
    Leave,
    Motion,
    Down(Button),
    Up(Button),
    Cancel,
    /// The pointer moved by `(dx, dy)` logical pixels on the screen, from the compositor's
    /// relative pointer events. Unlike `Motion` it does not depend on where the surface is,
    /// so it stays right while the surface itself moves. `pos` is the last known position.
    /// Hosts without relative pointer events never send it.
    Relative {
        dx: f32,
        dy: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Primary,
    Secondary,
    Middle,
}

/// What `Element::on_pointer` gets: the primary button's press, drag and release on the
/// node, at the node's own coordinates (its box from the top left, after its transform).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerInput {
    /// The event's time on the host clock.
    pub t: f64,
    /// Where, in the node's box. A drag can go outside it.
    pub local: Point,
    /// The node's box size.
    pub size: Size,
    pub phase: PointerPhase,
    /// The node's `Element::pointer_tag`, so one handler can serve several nodes.
    pub tag: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerPhase {
    /// The button went down. `clicks` is 1, or 2 and 3 for a double and triple click: a
    /// press on the same node within `MULTI_CLICK_SECS` of the last one and within
    /// `MULTI_CLICK_PX` of it. A fourth press starts at 1 again.
    Press { clicks: u8 },
    /// The pointer moved while the button is held. The node keeps the pointer outside its box.
    Drag,
    /// The button went up.
    Release,
    /// The pointer moved by `(dx, dy)` on the screen while the button is held, from relative
    /// pointer events (`PointerKind::Relative`), alongside `Drag`. For a node that moves the
    /// surface it sits on, such as a drag area: the travel stays right while the surface
    /// moves, which positions relative to the surface do not. Not every host sends it.
    Travel { dx: f32, dy: f32 },
}

/// Longest gap between the presses of a double or triple click, in seconds.
pub const MULTI_CLICK_SECS: f64 = 0.4;
/// Farthest a press of a double or triple click may be from the one before, in logical px.
pub const MULTI_CLICK_PX: f32 = 4.0;

/// The pointer's look over a node (`Element::cursor`). Hosts show the compositor's own
/// cursor for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cursor {
    #[default]
    Default,
    /// The I-beam, over editable text.
    Text,
    /// The hand, over something to click.
    Pointer,
}

/// Hovered and pressed node, per scene (one surface each for now).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PointerState {
    hovered: Option<NodeId>,
    pressed: Option<NodeId>,
    /// The last press on an `on_pointer` node: the node, when, where and its click count.
    last_press: Option<(NodeId, f64, Point, u8)>,
}

impl PointerState {
    /// Forgets nodes that are gone or leaving, so a removed node never gets a click.
    pub(crate) fn forget_missing(&mut self, present: impl Fn(NodeId) -> bool) {
        self.hovered = self.hovered.filter(|id| present(*id));
        self.pressed = self.pressed.filter(|id| present(*id));
    }
}

impl<M: Clone> Scene<M> {
    /// The topmost hit target under `pos` at `t`. Leaving nodes and nodes at opacity 0 are
    /// skipped. Uses the same sampling as the frame, so what is hit is what is drawn.
    pub fn hit_test(&self, pos: Point, t: f64) -> Option<NodeId> {
        let list = self.sample(t);
        list.hits.iter().rev().find_map(|h| {
            let local = h.transform.invert()?.apply(pos);
            h.shape.contains(local).then(|| NodeId::from_bits(h.node))
        })
    }

    /// `pos` in node `id`'s own coordinates at `t`, and the node's box size, when it is
    /// drawn as a hit target then.
    fn local_in(&self, id: NodeId, pos: Point, t: f64) -> Option<(Point, Size)> {
        let list = self.sample(t);
        let h = list.hits.iter().rev().find(|h| h.node == id.to_bits())?;
        let r = h.shape.rect;
        let p = h.transform.invert()?.apply(pos);
        Some((Point::new(p.x - r.x, p.y - r.y), Size::new(r.w, r.h)))
    }

    /// The `on_pointer` message for node `id`, if it has a handler and is drawn.
    fn pointer_msg(&self, id: NodeId, ev: &PointerEvent, phase: PointerPhase) -> Option<M> {
        let n = self.nodes.get(id)?;
        let make = n.handlers.pointer?;
        let (local, size) = self.local_in(id, ev.pos, ev.t)?;
        Some(make(PointerInput { t: ev.t, local, size, phase, tag: n.handlers.tag }))
    }

    /// The cursor to show: the pressed node's while the button is held, else the hovered
    /// node's (`Element::cursor`), else the default.
    pub fn cursor(&self) -> Cursor {
        self.pointer
            .pressed
            .or(self.pointer.hovered)
            .and_then(|id| self.nodes.get(id))
            .and_then(|n| n.handlers.cursor)
            .unwrap_or_default()
    }

    /// Turns a raw pointer event into app messages, and updates hover and press state.
    pub fn pointer(&mut self, ev: &PointerEvent) -> Vec<M> {
        let mut msgs = Vec::new();
        // Relative motion changes no hover state: it only reaches a node held by a press.
        if let PointerKind::Relative { dx, dy } = ev.kind {
            if let Some(id) = self.pointer.pressed {
                msgs.extend(self.pointer_msg(id, ev, PointerPhase::Travel { dx, dy }));
            }
            return msgs;
        }
        let hit = match ev.kind {
            PointerKind::Leave | PointerKind::Cancel => None,
            _ => self.hit_test(ev.pos, ev.t),
        };
        let before = self.pointer.hovered;
        if hit != before {
            if let Some((_, leave)) = before.and_then(|id| self.nodes.get(id)).and_then(|n| n.handlers.hover.as_ref()) {
                msgs.push(leave.clone());
            }
            if let Some((enter, _)) = hit.and_then(|id| self.nodes[id].handlers.hover.as_ref()) {
                msgs.push(enter.clone());
            }
            self.pointer.hovered = hit;
        }
        match ev.kind {
            PointerKind::Down(Button::Primary) => {
                self.pointer.pressed = hit;
                let on = hit.filter(|&id| self.nodes[id].handlers.pointer.is_some());
                if let Some(id) = on {
                    let clicks = match self.pointer.last_press {
                        Some((last, at, p, n))
                            if last == id
                                && ev.t - at <= MULTI_CLICK_SECS
                                && (ev.pos.x - p.x).hypot(ev.pos.y - p.y) <= MULTI_CLICK_PX =>
                        {
                            n % 3 + 1
                        }
                        _ => 1,
                    };
                    self.pointer.last_press = Some((id, ev.t, ev.pos, clicks));
                    msgs.extend(self.pointer_msg(id, ev, PointerPhase::Press { clicks }));
                }
            }
            PointerKind::Motion => {
                if let Some(id) = self.pointer.pressed {
                    msgs.extend(self.pointer_msg(id, ev, PointerPhase::Drag));
                }
            }
            PointerKind::Up(Button::Primary) => {
                let pressed = self.pointer.pressed.take();
                if let Some(id) = pressed {
                    msgs.extend(self.pointer_msg(id, ev, PointerPhase::Release));
                }
                if pressed.is_some() && pressed == hit {
                    if let Some(msg) = hit.and_then(|id| self.nodes[id].handlers.click.clone()) {
                        msgs.push(msg);
                    }
                }
            }
            PointerKind::Cancel | PointerKind::Leave => self.pointer.pressed = None,
            _ => {}
        }
        msgs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::Element;

    #[derive(Clone, Debug, PartialEq)]
    enum Msg {
        P(PointerInput),
        Click,
    }

    fn ev(t: f64, x: f32, y: f32, kind: PointerKind) -> PointerEvent {
        PointerEvent { surface: 1, t, pos: Point::new(x, y), kind }
    }

    /// A 100x20 text box at (10, 30) inside a padded column, tagged 7.
    fn scene() -> Scene<Msg> {
        let mut scene = Scene::new();
        let text = Element::panel("text").size(100.0, 20.0).on_pointer(Msg::P).pointer_tag(7).cursor(Cursor::Text);
        let button = Element::panel("button").size(20.0, 20.0).on_click(Msg::Click);
        scene.apply(Element::column("root").pad_xy(10.0, 30.0).child(text).child(button), 0.0);
        scene
    }

    fn inputs(msgs: Vec<Msg>) -> Vec<(PointerPhase, Point)> {
        msgs.into_iter()
            .filter_map(|m| match m {
                Msg::P(p) => Some((p.phase, p.local)),
                Msg::Click => None,
            })
            .collect()
    }

    #[test]
    fn press_drag_release_at_local_positions() {
        let mut s = scene();
        let press = s.pointer(&ev(1.0, 25.0, 40.0, PointerKind::Down(Button::Primary)));
        assert_eq!(
            press,
            vec![Msg::P(PointerInput {
                t: 1.0,
                local: Point::new(15.0, 10.0),
                size: Size::new(100.0, 20.0),
                phase: PointerPhase::Press { clicks: 1 },
                tag: 7,
            })]
        );
        // A drag past the box's right edge still reaches it.
        let drag = s.pointer(&ev(1.1, 150.0, 41.0, PointerKind::Motion));
        assert_eq!(inputs(drag), [(PointerPhase::Drag, Point::new(140.0, 11.0))]);
        let up = s.pointer(&ev(1.2, 150.0, 41.0, PointerKind::Up(Button::Primary)));
        assert_eq!(inputs(up), [(PointerPhase::Release, Point::new(140.0, 11.0))]);
        // A move with no button is no drag.
        assert!(s.pointer(&ev(1.3, 30.0, 40.0, PointerKind::Motion)).is_empty());
    }

    #[test]
    fn relative_motion_reaches_only_a_pressed_node_as_travel() {
        let mut s = scene();
        let rel = |t| ev(t, 25.0, 40.0, PointerKind::Relative { dx: 3.0, dy: -2.0 });
        // Not pressed: nothing, not even hover.
        assert!(s.pointer(&rel(0.9)).is_empty());
        s.pointer(&ev(1.0, 25.0, 40.0, PointerKind::Down(Button::Primary)));
        assert_eq!(
            inputs(s.pointer(&rel(1.1))),
            [(PointerPhase::Travel { dx: 3.0, dy: -2.0 }, Point::new(15.0, 10.0))]
        );
        s.pointer(&ev(1.2, 25.0, 40.0, PointerKind::Up(Button::Primary)));
        assert!(s.pointer(&rel(1.3)).is_empty());
    }

    #[test]
    fn double_and_triple_clicks_count_on_the_same_spot() {
        let mut s = scene();
        let clicks = |s: &mut Scene<Msg>, t: f64, x: f32| -> u8 {
            let msgs = s.pointer(&ev(t, x, 40.0, PointerKind::Down(Button::Primary)));
            s.pointer(&ev(t + 0.05, x, 40.0, PointerKind::Up(Button::Primary)));
            match inputs(msgs).first() {
                Some((PointerPhase::Press { clicks }, _)) => *clicks,
                other => panic!("no press: {other:?}"),
            }
        };
        assert_eq!(clicks(&mut s, 1.0, 30.0), 1);
        assert_eq!(clicks(&mut s, 1.2, 31.0), 2);
        assert_eq!(clicks(&mut s, 1.4, 32.0), 3);
        assert_eq!(clicks(&mut s, 1.6, 32.0), 1, "a fourth press starts again");
        assert_eq!(clicks(&mut s, 2.5, 32.0), 1, "too slow");
        assert_eq!(clicks(&mut s, 2.7, 60.0), 1, "too far");
    }

    #[test]
    fn the_cursor_follows_the_hovered_and_pressed_node() {
        let mut s = scene();
        assert_eq!(s.cursor(), Cursor::Default);
        s.pointer(&ev(1.0, 25.0, 40.0, PointerKind::Enter));
        assert_eq!(s.cursor(), Cursor::Text);
        s.pointer(&ev(1.1, 25.0, 40.0, PointerKind::Down(Button::Primary)));
        s.pointer(&ev(1.2, 15.0, 60.0, PointerKind::Motion));
        assert_eq!(s.cursor(), Cursor::Text, "the pressed text box keeps its cursor");
        s.pointer(&ev(1.3, 15.0, 60.0, PointerKind::Up(Button::Primary)));
        assert_eq!(s.cursor(), Cursor::Default, "over the button, which sets none");
    }
}
