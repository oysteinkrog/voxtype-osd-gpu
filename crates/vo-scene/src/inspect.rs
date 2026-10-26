//! Read-only views of the retained scene for tools: the preview tool's debug overlays and state
//! dumps. Nothing here changes the scene.

use crate::element::{Prop, Slot};
use crate::geom::{Affine, Size};
use crate::scene::{NodeId, Scene};

/// One animated property of a node at some `t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelState {
    pub prop: Prop,
    /// The slot's name, for example `offset.y` or `fill0.a`.
    pub name: &'static str,
    pub value: f64,
    pub target: f64,
    /// When the channel comes to rest; at or before `t` when it is settled.
    pub settles_at: f64,
}

const SLOT_NAMES: [&str; 27] = [
    "x",
    "y",
    "w",
    "h",
    "opacity",
    "offset.x",
    "offset.y",
    "scale",
    "rotation",
    "radius",
    "stroke_width",
    "fill0.r",
    "fill0.g",
    "fill0.b",
    "fill0.a",
    "fill1.r",
    "fill1.g",
    "fill1.b",
    "fill1.a",
    "stroke.r",
    "stroke.g",
    "stroke.b",
    "stroke.a",
    "text.r",
    "text.g",
    "text.b",
    "text.a",
];

fn slot_name(slot: u8) -> &'static str {
    SLOT_NAMES.get(slot as usize).copied().unwrap_or("?")
}

impl<M> Scene<M> {
    /// Every node, parents before children and children in paint order, leaving ones
    /// included.
    pub fn node_ids(&self) -> Vec<NodeId> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<NodeId> = self.root.into_iter().collect();
        while let Some(id) = stack.pop() {
            out.push(id);
            stack.extend(self.nodes[id].children.iter().rev().copied());
        }
        out
    }

    /// Depth below the root (the root is 0).
    pub fn depth(&self, id: NodeId) -> usize {
        let mut d = 0;
        let mut cur = self.nodes[id].parent;
        while let Some(p) = cur {
            d += 1;
            cur = self.nodes[p].parent;
        }
        d
    }

    /// The node's animated properties at `t`, in slot order. Live-text words are left out.
    pub fn channel_states(&self, id: NodeId, t: f64) -> Vec<ChannelState> {
        self.nodes[id]
            .chans
            .iter()
            .map(|(slot, c)| ChannelState {
                prop: Slot::prop(*slot),
                name: slot_name(*slot),
                value: c.value(t),
                target: c.target(),
                settles_at: c.settled_at(),
            })
            .collect()
    }

    /// Where the node is heading: its box with every channel at its target, under its
    /// parent's transform as sampled at `t`. `None` when the node or a parent is not drawn at
    /// `t`.
    pub fn target_box(&self, id: NodeId, t: f64) -> Option<(Affine, Size)> {
        let n = &self.nodes[id];
        let parent = match n.parent {
            Some(p) => self.sampled(p, t)?.transform,
            None => Affine::IDENTITY,
        };
        self.sampled(id, t)?;
        let target = |s: Slot| {
            let slot = s as u8;
            match n.chans.binary_search_by_key(&slot, |(k, _)| *k) {
                Ok(i) => n.chans[i].1.target() as f32,
                Err(_) => Slot::default_value(slot) as f32,
            }
        };
        let size = Size::new(target(Slot::W).max(0.0), target(Slot::H).max(0.0));
        let (x, y) = (target(Slot::X) + target(Slot::OffsetX), target(Slot::Y) + target(Slot::OffsetY));
        let (scale, rotation) = (target(Slot::Scale), target(Slot::Rotation));
        let (cx, cy) = (0.5 * size.w, 0.5 * size.h);
        let local = Affine::translate(-cx, -cy)
            .then(&Affine::scale(scale))
            .then(&Affine::rotate(rotation))
            .then(&Affine::translate(cx + x, cy + y));
        Some((local.then(&parent), size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::Element;
    use crate::geom::Point;

    #[test]
    fn channel_states_and_target_box_follow_a_move() {
        let mut scene: Scene = Scene::new();
        let view = |dx: f32| Element::row("root").child(Element::panel("a").size(40.0, 20.0).offset(dx, 0.0));
        scene.apply(view(0.0), 0.0);
        scene.apply(view(100.0), 1.0);
        let ids = scene.node_ids();
        assert_eq!(ids.len(), 2);
        assert_eq!(scene.path(ids[1]), "root/a");
        assert_eq!(scene.depth(ids[1]), 1);
        let states = scene.channel_states(ids[1], 1.05);
        let ox = states.iter().find(|c| c.name == "offset.x").expect("offset.x channel");
        assert_eq!(ox.target, 100.0);
        assert!(ox.value < 100.0 && ox.settles_at > 1.05, "{ox:?}");
        let (m, size) = scene.target_box(ids[1], 1.05).unwrap();
        assert_eq!(size, Size::new(40.0, 20.0));
        assert_eq!(m.apply(Point::ZERO), Point::new(100.0, 0.0));
    }
}
