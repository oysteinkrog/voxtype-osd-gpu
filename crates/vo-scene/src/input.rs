//! Hit testing and pointer messages, as in `docs/INPUT-MODEL.md`.
//!
//! `hit_test` samples the scene at the event's own time, so the answer does not depend on
//! the frame rate. First cut: enter, leave and click. Drag comes when a design needs it.

use crate::geom::Point;
use crate::scene::{NodeId, Scene};

pub type SurfaceId = u32;

/// A raw pointer event from the host, in surface logical units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerEvent {
    pub surface: SurfaceId,
    pub t: f64,
    pub pos: Point,
    pub kind: PointerKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Enter,
    Leave,
    Motion,
    Down(Button),
    Up(Button),
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Primary,
    Secondary,
    Middle,
}

/// Hovered and pressed node, per scene (one surface each for now).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PointerState {
    hovered: Option<NodeId>,
    pressed: Option<NodeId>,
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

    /// Turns a raw pointer event into app messages, and updates hover and press state.
    pub fn pointer(&mut self, ev: &PointerEvent) -> Vec<M> {
        let mut msgs = Vec::new();
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
            PointerKind::Down(Button::Primary) => self.pointer.pressed = hit,
            PointerKind::Up(Button::Primary) => {
                let pressed = self.pointer.pressed.take();
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
