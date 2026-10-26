//! The retained scene: nodes reconciled from elements by key path, each owning its animated
//! properties as `vo-core` channels.
//!
//! - `apply(view, t)` runs when app state changes, at the event's timestamp. Every changed
//!   target starts a new spring segment at exactly `t`, so a 144 Hz session and a 60 fps
//!   export give the same motion.
//! - `sample(t)` is pure. It reads the channels at `t` and builds the display list.
//! - `next_wake(t)` is `None` once everything has settled, so the host draws nothing.
//!
//! Presence: a node with `.presence(enter, exit)` starts at `enter` and springs to its
//! targets. When the view drops it, it springs to `exit` and stays, out of layout, until that
//! settles. Without `.presence` a dropped node goes at once.

use slotmap::{new_key_type, Key as _, KeyData, SlotMap};
use taffy::prelude::AvailableSpace;
use vo_core::channel::{Channel, Rest};
use vo_core::spring::Spring;

use crate::display::{
    BlendMode, DisplayList, GlyphInstance, HitShape, Item, ItemKind, Layer, LayerTarget, Paint, ShapeInstance,
    ShapeKind, NO_WORD,
};
use crate::element::{Element, Handlers, Key, Kind, Motion, Prop, Slot, Transition, DEFAULT_MOTION};
use crate::geom::{Affine, Color, Point, RRect, Rect, Size};
use crate::input::PointerState;
use crate::layout::LayoutView;
use crate::text::{EstimateText, ShapedText, TextLayout, ELLIPSIS_CLUSTER};

new_key_type! {
    /// A retained node. Stable for as long as its key path stays in the view.
    pub struct NodeId;
}

impl NodeId {
    pub fn to_bits(self) -> u64 {
        self.data().as_ffi()
    }

    pub fn from_bits(bits: u64) -> NodeId {
        KeyData::from_ffi(bits).into()
    }
}

/// Where a node is in its life, at some time `t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Presence {
    /// Created at `since`; its enter motion is still running.
    Entering {
        since: f64,
    },
    Present,
    /// Dropped from the view at `since`; drawn until `until`, then removed.
    Leaving {
        since: f64,
        until: f64,
    },
}

pub(crate) struct Node<M> {
    pub key: Key,
    pub parent: Option<NodeId>,
    pub kind: Kind,
    pub style: taffy::Style,
    pub cache: taffy::Cache,
    pub layout: taffy::Layout,
    pub hoisted: Vec<taffy::NodeId>,
    /// Paint order, leaving children included.
    pub children: Vec<NodeId>,
    /// Layout order: the children that are not leaving.
    pub flow: Vec<NodeId>,
    /// Sorted by slot.
    pub chans: Vec<(u8, Channel)>,
    pub motion: Vec<(Prop, Motion)>,
    pub presence: Option<(Transition, Transition)>,
    pub gradient: bool,
    pub hairline: bool,
    pub handlers: Handlers<M>,
    pub born: f64,
    /// `(since, until)`.
    pub leaving: Option<(f64, f64)>,
    /// When the node last entered: created, or brought back while leaving.
    pub since: f64,
    /// When the enter motion settles.
    pub entered: f64,
    pub shaped: ShapedText,
    pub has_layout: bool,
}

impl<M> Node<M> {
    pub fn value(&self, slot: u8, t: f64) -> f64 {
        match self.chans.binary_search_by_key(&slot, |(s, _)| *s) {
            Ok(i) => self.chans[i].1.value(t),
            Err(_) => Slot::default_value(slot),
        }
    }

    pub fn has(&self, slot: u8) -> bool {
        self.chans.binary_search_by_key(&slot, |(s, _)| *s).is_ok()
    }

    fn motion_for(&self, slot: u8) -> Motion {
        let prop = Slot::prop(slot);
        self.motion.iter().find(|(p, _)| *p == prop).map_or(DEFAULT_MOTION, |(_, m)| *m)
    }

    fn new_channel(&self, slot: u8, t: f64, value: f64, retention: f64) -> Channel {
        let spring = match self.motion_for(slot) {
            Motion::Spring(s) => s,
            Motion::Snap => Spring::SOFT,
        };
        let rest = if Slot::is_unit(slot) { Rest::UNIT } else { Rest::PIXELS };
        Channel::new(t, value, spring).with_rest(rest).with_retention(retention)
    }

    /// Moves `slot` toward `value` from `t` with the slot's motion. A slot with no channel
    /// yet starts from its default value (opacity 1, offset 0), so a property that appears
    /// on a live node animates in like any other change. Returns whether the motion changed:
    /// false when the slot already heads to `value`.
    pub fn set_target(&mut self, slot: u8, value: f64, t: f64, retention: f64) -> bool {
        let i = match self.chans.binary_search_by_key(&slot, |(s, _)| *s) {
            Ok(i) => i,
            Err(i) => {
                let ch = self.new_channel(slot, t, Slot::default_value(slot), retention);
                self.chans.insert(i, (slot, ch));
                i
            }
        };
        let motion = self.motion_for(slot);
        let ch = &mut self.chans[i].1;
        let last = |ch: &Channel| ch.segments().last().map(|s| (s.t0, s.target));
        let before = last(ch);
        match motion {
            Motion::Spring(s) => {
                ch.set_spring(s);
                ch.retarget(t, value);
            }
            Motion::Snap => {
                if ch.target() != value || !ch.is_settled(t) {
                    ch.snap(t, value);
                }
            }
        }
        last(ch) != before
    }

    /// Puts `slot` at `value` at `t`, at rest, for an enter motion to start from.
    fn start_at(&mut self, slot: u8, value: f64, t: f64, retention: f64) {
        match self.chans.binary_search_by_key(&slot, |(s, _)| *s) {
            Ok(i) => self.chans[i].1.snap(t, value),
            Err(i) => {
                let ch = self.new_channel(slot, t, value, retention);
                self.chans.insert(i, (slot, ch));
            }
        }
    }

    fn settled_at(&self) -> f64 {
        self.chans.iter().map(|(_, c)| c.settled_at()).fold(f64::NEG_INFINITY, f64::max)
    }

    fn color(&self, slot: Slot, t: f64) -> Color {
        let s = slot as u8;
        let c = |i: u8| self.value(s + i, t).clamp(0.0, 1.0) as f32;
        Color::rgba(c(0), c(1), c(2), c(3))
    }

    /// Whether the node is drawn at `t`: created by then and not yet gone.
    fn alive(&self, t: f64) -> bool {
        t >= self.born && self.leaving.is_none_or(|(_, until)| t < until)
    }
}

/// A box as sampled at some `t`, in surface space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sampled {
    /// Node space to surface space.
    pub transform: Affine,
    pub size: Size,
    pub opacity: f32,
}

pub struct Scene<M = ()> {
    pub(crate) nodes: SlotMap<NodeId, Node<M>>,
    pub(crate) root: Option<NodeId>,
    last_t: f64,
    viewport: Size,
    scale: f32,
    text: Box<dyn TextLayout>,
    retention: f64,
    layout_dirty: bool,
    /// Something drawn or hit-tested changed during the current `apply`.
    changed: bool,
    pub(crate) pointer: PointerState,
}

impl<M> Default for Scene<M> {
    fn default() -> Self {
        Scene::new()
    }
}

impl<M> Scene<M> {
    /// An empty scene with estimated text metrics and no viewport (the root takes its
    /// content size).
    pub fn new() -> Scene<M> {
        Scene {
            nodes: SlotMap::with_key(),
            root: None,
            last_t: f64::NEG_INFINITY,
            viewport: Size::ZERO,
            scale: 1.0,
            text: Box::new(EstimateText::default()),
            retention: f64::INFINITY,
            layout_dirty: false,
            changed: false,
            pointer: PointerState::default(),
        }
    }

    /// Replaces the text measurer. Takes effect at the next layout.
    pub fn with_text(mut self, text: Box<dyn TextLayout>) -> Scene<M> {
        self.text = text;
        self
    }

    /// Keep channel history only `secs` back from each retarget. The default keeps all of
    /// it, which seeking and export want; a long-running overlay should set a few seconds.
    pub fn with_retention(mut self, secs: f64) -> Scene<M> {
        self.retention = secs;
        self
    }

    /// Sets the surface size (logical units; zero lets the root take its content size) and
    /// the scale factor, and lays out again at `t` if the size changed.
    pub fn set_viewport(&mut self, t: f64, size: Size, scale: f32) {
        self.scale = scale;
        if size != self.viewport {
            self.viewport = size;
            if let Some(root) = self.root {
                self.mark_layout(root);
                self.run_layout(t);
            }
        }
    }

    /// Number of retained nodes, leaving ones included.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn root(&self) -> Option<NodeId> {
        self.root
    }

    /// The node at a key path from the root, root key first.
    pub fn find(&self, path: &[&str]) -> Option<NodeId> {
        let (first, rest) = path.split_first()?;
        let mut id = self.root.filter(|r| self.nodes[*r].key.0 == *first)?;
        for k in rest {
            id = *self.nodes[id].children.iter().find(|c| self.nodes[**c].key.0 == *k)?;
        }
        Some(id)
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(id)
    }

    /// The key path of a node, joined with `/`.
    pub fn path(&self, id: NodeId) -> String {
        let mut keys = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            keys.push(self.nodes[c].key.0.as_ref());
            cur = self.nodes[c].parent;
        }
        keys.reverse();
        keys.join("/")
    }

    pub fn presence(&self, id: NodeId, t: f64) -> Presence {
        let n = &self.nodes[id];
        match n.leaving {
            Some((since, until)) if t >= since => Presence::Leaving { since, until },
            _ if t < n.entered => Presence::Entering { since: n.since },
            _ => Presence::Present,
        }
    }

    /// The layout box a node is heading to (relative to its parent), from the last layout.
    pub fn layout_target(&self, id: NodeId) -> Rect {
        let l = &self.nodes[id].layout;
        Rect::new(l.location.x, l.location.y, l.size.width, l.size.height)
    }

    /// Applies a new view at `t`. Events must come in time order.
    ///
    /// Returns whether anything changed: a retarget, a node added, removed or reordered, a
    /// node starting to leave, a drawn property or hit flag, or anything that needs layout.
    /// When it returns false, `sample` gives the same lists as before for every `t`, so a
    /// host needs no new frame.
    pub fn apply(&mut self, view: Element<M>, t: f64) -> bool {
        debug_assert!(
            t >= self.last_t,
            "scene event at t = {t} after one at {}; events must be in time order",
            self.last_t
        );
        self.last_t = self.last_t.max(t);
        self.changed = false;
        self.drop_gone(t);
        match self.root {
            Some(r) if self.nodes[r].key == view.key && self.nodes[r].kind.same_variant(&view.kind) => {
                self.revive(r);
                self.update(r, view, t)
            }
            old => {
                if let Some(r) = old {
                    self.remove_subtree(r);
                }
                let r = self.create(None, view, t);
                self.root = Some(r);
                self.layout_dirty = true;
            }
        }
        if self.layout_dirty {
            self.changed = true;
            self.run_layout(t);
        }
        self.pointer.forget_missing(|id| self.nodes.get(id).is_some_and(|n| n.leaving.is_none()));
        self.changed
    }

    /// The time at or after `t` from which the display list may differ from `sample(t)`:
    /// `Some(t)` while anything moves, `None` once everything has settled.
    pub fn next_wake(&self, t: f64) -> Option<f64> {
        let moving = self.nodes.values().any(|n| n.chans.iter().any(|(_, c)| c.settled_at() > t));
        moving.then_some(t)
    }

    /// The display list at `t`. Pure: the same scene gives the same list for the same `t`.
    pub fn sample(&self, t: f64) -> DisplayList {
        let mut out = DisplayList::default();
        self.sample_into(t, &mut out);
        out
    }

    /// `sample` into an existing list, reusing its allocations.
    pub fn sample_into(&self, t: f64, out: &mut DisplayList) {
        out.clear();
        out.size = self.viewport;
        out.scale = self.scale;
        out.layers.push(Layer::root());
        let mut items = Vec::new();
        if let Some(root) = self.root {
            self.emit(root, t, Affine::IDENTITY, 1.0, 0, false, &mut items, out);
        }
        out.layers[0].items = close_items(&mut items, out);
    }

    /// Box, transform and opacity of a node at `t`, composed with its ancestors. `None` if
    /// it is not drawn at `t`.
    pub fn sampled(&self, id: NodeId, t: f64) -> Option<Sampled> {
        let mut chain = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            chain.push(c);
            cur = self.nodes[c].parent;
        }
        let (mut m, mut opacity, mut size) = (Affine::IDENTITY, 1.0, Size::ZERO);
        for &c in chain.iter().rev() {
            let n = &self.nodes[c];
            if !n.alive(t) {
                return None;
            }
            let (local, s, o) = local_transform(n, t);
            m = local.then(&m);
            opacity *= o;
            size = s;
        }
        Some(Sampled { transform: m, size, opacity })
    }

    // Reconcile.

    fn create(&mut self, parent: Option<NodeId>, el: Element<M>, t: f64) -> NodeId {
        let Element { key, kind, style, targets, gradient, hairline, motion, presence, handlers, children } = el;
        self.changed = true;
        let mut node = Node {
            key,
            parent,
            kind,
            style,
            cache: taffy::Cache::new(),
            layout: taffy::Layout::with_order(0),
            hoisted: Vec::new(),
            children: Vec::new(),
            flow: Vec::new(),
            chans: Vec::new(),
            motion,
            presence,
            gradient,
            hairline,
            handlers,
            born: t,
            leaving: None,
            since: t,
            entered: t,
            shaped: ShapedText::default(),
            has_layout: false,
        };
        let enter = presence.map_or(Transition::NONE, |(enter, _)| enter);
        for (slot, start) in enter.slots() {
            node.start_at(slot, start, t, self.retention);
            let target = targets.iter().find(|(s, _)| *s == slot).map_or(Slot::default_value(slot), |(_, v)| *v);
            node.set_target(slot, target, t, self.retention);
        }
        for &(slot, v) in &targets {
            if !node.has(slot) {
                node.start_at(slot, v, t, self.retention);
            }
        }
        node.entered = node.settled_at().max(t);
        let id = self.nodes.insert(node);
        check_keys(&children, || self.path(id));
        let ids: Vec<NodeId> = children.into_iter().map(|c| self.create(Some(id), c, t)).collect();
        let n = &mut self.nodes[id];
        n.flow.clone_from(&ids);
        n.children = ids;
        id
    }

    fn update(&mut self, id: NodeId, el: Element<M>, t: f64) {
        let Element { key: _, kind, style, targets, gradient, hairline, motion, presence, handlers, children } = el;
        let retention = self.retention;
        let n = &mut self.nodes[id];
        let relayout = n.style != style || (kind.is_text() && n.kind != kind);
        let mut changed = n.kind != kind || n.gradient != gradient || n.hairline != hairline;
        changed |= n.handlers.hit != handlers.hit;
        n.style = style;
        n.kind = kind;
        n.motion = motion;
        n.presence = presence;
        n.gradient = gradient;
        n.hairline = hairline;
        n.handlers = handlers;
        for &(slot, v) in &targets {
            changed |= n.set_target(slot, v, t, retention);
        }
        // Visual slots the view no longer sets go back to their defaults.
        let unset: Vec<u8> = n
            .chans
            .iter()
            .map(|(s, _)| *s)
            .filter(|s| *s >= Slot::Opacity as u8 && !targets.iter().any(|(k, _)| k == s))
            .collect();
        for slot in unset {
            changed |= n.set_target(slot, Slot::default_value(slot), t, retention);
        }
        self.changed |= changed;
        if relayout {
            self.mark_layout(id);
        }
        self.reconcile_children(id, children, t);
    }

    fn reconcile_children(&mut self, id: NodeId, children: Vec<Element<M>>, t: f64) {
        check_keys(&children, || self.path(id));
        let old = std::mem::take(&mut self.nodes[id].children);
        let mut used = vec![false; old.len()];
        let mut flow = Vec::with_capacity(children.len());
        for el in children {
            let found = old.iter().enumerate().position(|(i, &c)| {
                !used[i] && self.nodes[c].key == el.key && self.nodes[c].kind.same_variant(&el.kind)
            });
            let c = match found {
                Some(i) => {
                    used[i] = true;
                    let c = old[i];
                    let revived = self.revive(c);
                    self.update(c, el, t);
                    if revived {
                        let n = &mut self.nodes[c];
                        n.since = t;
                        n.entered = n.settled_at().max(t);
                    }
                    c
                }
                None => self.create(Some(id), el, t),
            };
            flow.push(c);
        }
        // Children the view dropped: leave with their exit motion, or go now.
        let mut order = flow.clone();
        let mut anchor: Option<NodeId> = None;
        for (i, &c) in old.iter().enumerate() {
            if used[i] {
                anchor = Some(c);
                continue;
            }
            if self.start_leaving(c, t) {
                // Keep its place in paint order, after the sibling it followed.
                let at = anchor.and_then(|a| order.iter().position(|x| *x == a)).map_or(0, |p| p + 1);
                order.insert(at, c);
                anchor = Some(c);
            } else {
                self.remove_subtree(c);
            }
        }
        let n = &mut self.nodes[id];
        let changed = n.flow != flow;
        self.changed |= n.children != order;
        n.flow = flow;
        n.children = order;
        if changed {
            self.mark_layout(id);
        }
    }

    /// A leaving node whose key is back in the view enters again from where it is.
    /// Returns whether it was leaving.
    fn revive(&mut self, id: NodeId) -> bool {
        let revived = self.nodes[id].leaving.take().is_some();
        self.changed |= revived;
        revived
    }

    /// Starts the exit motion. Returns false when the node should go at once.
    fn start_leaving(&mut self, id: NodeId, t: f64) -> bool {
        let retention = self.retention;
        let n = &mut self.nodes[id];
        if n.leaving.is_some() {
            return true;
        }
        let Some((_, exit)) = n.presence else { return false };
        for (slot, v) in exit.slots() {
            n.set_target(slot, v, t, retention);
        }
        self.changed = true;
        let until = self.subtree_settled_at(id);
        if until <= t {
            return false;
        }
        self.nodes[id].leaving = Some((t, until));
        true
    }

    fn subtree_settled_at(&self, id: NodeId) -> f64 {
        let n = &self.nodes[id];
        n.children.iter().map(|&c| self.subtree_settled_at(c)).fold(n.settled_at(), f64::max)
    }

    /// Removes leaving nodes whose exit has settled by `t`.
    fn drop_gone(&mut self, t: f64) {
        let gone: Vec<NodeId> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.leaving.is_some_and(|(_, until)| until <= t))
            .map(|(id, _)| id)
            .collect();
        for id in gone {
            if !self.nodes.contains_key(id) {
                continue;
            }
            match self.nodes[id].parent {
                Some(p) => self.nodes[p].children.retain(|c| *c != id),
                None => self.root = None,
            }
            self.changed = true;
            self.remove_subtree(id);
        }
    }

    fn remove_subtree(&mut self, id: NodeId) {
        if let Some(n) = self.nodes.remove(id) {
            for c in n.children {
                self.remove_subtree(c);
            }
        }
    }

    // Layout.

    /// Clears the layout cache of `id` and its ancestors, so the next layout recomputes them.
    fn mark_layout(&mut self, id: NodeId) {
        self.layout_dirty = true;
        let mut cur = Some(id);
        while let Some(c) = cur {
            let n = &mut self.nodes[c];
            n.cache.clear();
            cur = n.parent;
        }
    }

    /// Runs taffy and turns the boxes into position and size targets at `t`. A node laid
    /// out for the first time starts at rest at its box.
    fn run_layout(&mut self, t: f64) {
        self.layout_dirty = false;
        let Some(root) = self.root else { return };
        let avail = |v: f32| if v > 0.0 { AvailableSpace::Definite(v) } else { AvailableSpace::MaxContent };
        let space = taffy::Size { width: avail(self.viewport.w), height: avail(self.viewport.h) };
        {
            let mut view = LayoutView { nodes: &mut self.nodes, text: &*self.text };
            taffy::compute_root_layout(&mut view, root.into(), space);
        }
        let retention = self.retention;
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let n = &mut self.nodes[id];
            let l = n.layout;
            let boxes =
                [(Slot::X, l.location.x), (Slot::Y, l.location.y), (Slot::W, l.size.width), (Slot::H, l.size.height)];
            for (slot, v) in boxes {
                if n.has_layout {
                    n.set_target(slot as u8, v as f64, t, retention);
                } else {
                    n.start_at(slot as u8, v as f64, t, retention);
                }
            }
            n.has_layout = true;
            stack.extend(n.flow.iter().copied());
        }
    }

    // Sampling.

    /// Emits node `id` and its subtree. `parent_m` maps the parent's space to the current
    /// layer's space; `alpha` is the opacity inherited since that layer.
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &self,
        id: NodeId,
        t: f64,
        parent_m: Affine,
        alpha: f32,
        layer: u32,
        leaving: bool,
        items: &mut Vec<Item>,
        out: &mut DisplayList,
    ) {
        let n = &self.nodes[id];
        if !n.alive(t) {
            return;
        }
        let (local, size, opacity) = local_transform(n, t);
        if opacity <= 0.0 {
            return;
        }
        let leaving = leaving || n.leaving.is_some_and(|(since, _)| t >= since);
        let m = local.then(&parent_m);
        let as_layer = matches!(n.kind, Kind::Layer { .. });
        if !as_layer && m.is_translation() {
            self.emit_body(id, t, m, size, alpha * opacity, layer, leaving, items, out);
            return;
        }
        // A layer element, or a scale or rotation: the node gets a layer of its own, so
        // instances inside it stay axis aligned.
        let index = out.layers.len() as u32;
        items.push(Item { kind: ItemKind::Layer, start: index, len: 1 });
        let transform = m.then(&out.layers[layer as usize].transform);
        let (blend, clip, target, group, inside) = match n.kind {
            Kind::Layer { blend, clip, offscreen } => {
                let clip = clip.then(|| RRect::new(Rect::from_size(size), radius(n, t)));
                let target = if offscreen { LayerTarget::Offscreen { effect: 0 } } else { LayerTarget::Direct };
                (blend, clip, target, alpha * opacity, 1.0)
            }
            _ => (BlendMode::Normal, None, LayerTarget::Direct, 1.0, alpha * opacity),
        };
        out.layers.push(Layer { parent: Some(layer), transform, opacity: group, blend, clip, target, items: 0..0 });
        let mut own = Vec::new();
        self.emit_body(id, t, Affine::IDENTITY, size, inside, index, leaving, &mut own, out);
        out.layers[index as usize].items = close_items(&mut own, out);
    }

    /// Hit shape, visuals and children of a node whose space maps to the current layer by
    /// `m`, a translation.
    #[allow(clippy::too_many_arguments)]
    fn emit_body(
        &self,
        id: NodeId,
        t: f64,
        m: Affine,
        size: Size,
        alpha: f32,
        layer: u32,
        leaving: bool,
        items: &mut Vec<Item>,
        out: &mut DisplayList,
    ) {
        let n = &self.nodes[id];
        if n.handlers.hit && !leaving {
            let transform = m.then(&out.layers[layer as usize].transform);
            let shape = RRect::new(Rect::from_size(size), radius(n, t));
            out.hits.push(HitShape { node: id.to_bits(), transform, shape });
        }
        self.draw(n, t, Point::new(m.e, m.f), size, alpha, items, out);
        for &c in &n.children {
            self.emit(c, t, m, alpha, layer, leaving, items, out);
        }
    }

    /// The node's own visuals at `origin` in the current layer.
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        n: &Node<M>,
        t: f64,
        origin: Point,
        size: Size,
        alpha: f32,
        items: &mut Vec<Item>,
        out: &mut DisplayList,
    ) {
        let rect = Rect::new(origin.x, origin.y, size.w, size.h);
        let fill = || {
            if !n.has(Slot::Fill0 as u8) {
                return Paint::NONE;
            }
            let c0 = n.color(Slot::Fill0, t);
            let p = if n.gradient {
                let c1 = n.color(Slot::Fill1, t);
                Paint::linear(origin, c0, Point::new(origin.x, origin.y + size.h), c1)
            } else {
                Paint::solid(c0)
            };
            p.fade(alpha)
        };
        let stroked = |mut s: ShapeInstance| {
            if n.has(Slot::Stroke as u8) {
                let paint = Paint::solid(n.color(Slot::Stroke, t)).fade(alpha);
                s = if n.hairline {
                    s.with_hairline(paint)
                } else {
                    s.with_stroke(paint, n.value(Slot::StrokeWidth as u8, t).max(0.0) as f32)
                };
            }
            s
        };
        let shape = match &n.kind {
            Kind::Panel | Kind::Layer { .. } => Some(ShapeInstance::rrect(RRect::new(rect, radius(n, t)), fill())),
            Kind::Shape(kind) => Some(match kind {
                ShapeKind::RRect => ShapeInstance::rrect(RRect::new(rect, radius(n, t)), fill()),
                ShapeKind::Circle => {
                    let centre = Point::new(origin.x + 0.5 * size.w, origin.y + 0.5 * size.h);
                    ShapeInstance::circle(centre, 0.5 * size.w.min(size.h), fill())
                }
                ShapeKind::Capsule => ShapeInstance::capsule(rect, fill()),
                ShapeKind::Line => {
                    ShapeInstance::line(origin, Point::new(rect.right(), rect.bottom()), 0.0, Paint::NONE)
                }
            }),
            Kind::Label { style, .. } => {
                let color = n.color(Slot::Text, t).fade(alpha);
                self.glyphs(n, style.font, style.size, origin, color, |_| NO_WORD, items, out);
                None
            }
            Kind::LiveText { words, style } => {
                let color = n.color(Slot::Text, t).fade(alpha);
                // Byte offset where each word starts in the joined text.
                let mut starts = Vec::with_capacity(words.len());
                let mut at = 0u32;
                for w in words {
                    starts.push((at, w.id));
                    at += w.text.len() as u32 + 1;
                }
                let word_of = |cluster: u32| {
                    if cluster == ELLIPSIS_CLUSTER {
                        return NO_WORD;
                    }
                    let i = starts.partition_point(|(s, _)| *s <= cluster);
                    if i == 0 {
                        NO_WORD
                    } else {
                        starts[i - 1].1
                    }
                };
                self.glyphs(n, style.font, style.size, origin, color, word_of, items, out);
                None
            }
            Kind::Stack => None,
        };
        if let Some(s) = shape.map(stroked) {
            if s.fill.is_visible() || s.stroke.is_visible() {
                push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
                out.shapes.push(s);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn glyphs(
        &self,
        n: &Node<M>,
        font: u32,
        size: f32,
        origin: Point,
        color: Color,
        word_of: impl Fn(u32) -> u32,
        items: &mut Vec<Item>,
        out: &mut DisplayList,
    ) {
        if color.a <= 0.0 {
            return;
        }
        for g in &n.shaped.glyphs {
            push_item(items, ItemKind::Glyphs, out.glyphs.len() as u32);
            out.glyphs.push(GlyphInstance {
                pos: Point::new(origin.x + g.pos.x, origin.y + g.pos.y),
                size,
                font,
                glyph: g.glyph,
                word: word_of(g.cluster),
                color,
            });
        }
    }
}

fn radius<M>(n: &Node<M>, t: f64) -> f32 {
    n.value(Slot::Radius as u8, t).max(0.0) as f32
}

/// Node space to parent space at `t`, the box size, and the node's own opacity.
fn local_transform<M>(n: &Node<M>, t: f64) -> (Affine, Size, f32) {
    let v = |s: Slot| n.value(s as u8, t) as f32;
    let size = Size::new(v(Slot::W).max(0.0), v(Slot::H).max(0.0));
    let (x, y) = (v(Slot::X) + v(Slot::OffsetX), v(Slot::Y) + v(Slot::OffsetY));
    let (scale, rotation) = (v(Slot::Scale), v(Slot::Rotation));
    let opacity = v(Slot::Opacity).clamp(0.0, 1.0);
    let m = if scale == 1.0 && rotation == 0.0 {
        Affine::translate(x, y)
    } else {
        let (cx, cy) = (0.5 * size.w, 0.5 * size.h);
        Affine::translate(-cx, -cy)
            .then(&Affine::scale(scale))
            .then(&Affine::rotate(rotation))
            .then(&Affine::translate(cx + x, cy + y))
    };
    (m, size, opacity)
}

/// Appends one instance at `index` to the item list, extending the last run when it is the
/// same kind and adjacent.
fn push_item(items: &mut Vec<Item>, kind: ItemKind, index: u32) {
    if let Some(last) = items.last_mut() {
        if last.kind == kind && last.start + last.len == index {
            last.len += 1;
            return;
        }
    }
    items.push(Item { kind, start: index, len: 1 });
}

/// Moves a layer's items into the list and returns their range.
fn close_items(items: &mut Vec<Item>, out: &mut DisplayList) -> std::ops::Range<u32> {
    let start = out.items.len() as u32;
    out.items.append(items);
    start..out.items.len() as u32
}

/// Sibling keys must differ. Checked in debug builds only.
fn check_keys<M>(children: &[Element<M>], parent: impl FnOnce() -> String) {
    if cfg!(debug_assertions) && children.len() > 1 {
        let mut keys: Vec<&Key> = children.iter().map(|c| &c.key).collect();
        keys.sort();
        if let Some(w) = keys.windows(2).find(|w| w[0] == w[1]) {
            panic!("duplicate key '{}' among the children of {}", w[0], parent());
        }
    }
}

impl From<NodeId> for taffy::NodeId {
    fn from(id: NodeId) -> taffy::NodeId {
        taffy::NodeId::from(id.to_bits())
    }
}

impl From<taffy::NodeId> for NodeId {
    fn from(id: taffy::NodeId) -> NodeId {
        NodeId::from_bits(u64::from(id))
    }
}
