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
    BlendMode, Bloom, DisplayList, DotInstance, GlyphInstance, HitShape, Item, ItemKind, Layer, LayerTarget, Paint,
    ShapeInstance, ShapeKind, Trail, NO_WORD,
};
use crate::dots::Dots;
use crate::element::{Element, Handlers, Key, Kind, Motion, Prop, Slot, Transition, DEFAULT_MOTION};
use crate::geom::{Affine, Color, Point, RRect, Rect, Size};
use crate::glass::{Glass, GlassInstance};
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
    pub inside: bool,
    pub glass: Option<Glass>,
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
    /// Live text: each word's own motion, sorted by word id.
    pub words: Vec<WordAnim>,
    /// The motion of a property no `.motion()` call names: the scene's default.
    pub default_motion: Motion,
    /// Reduced motion (MO09): travel and scale jump, effects change slowly.
    pub reduced: bool,
}

/// One live-text word's motion: opacity and an offset from its laid-out place.
pub(crate) struct WordAnim {
    pub id: u32,
    pub text: String,
    pub opacity: Channel,
    pub dx: Channel,
    pub dy: Channel,
}

impl WordAnim {
    fn channels(&self) -> [&Channel; 3] {
        [&self.opacity, &self.dx, &self.dy]
    }
}

impl<M> Node<M> {
    pub fn value(&self, slot: u8, t: f64) -> f64 {
        match self.chans.binary_search_by_key(&slot, |(s, _)| *s) {
            Ok(i) => self.chans[i].1.value(t),
            Err(_) => Slot::default_value(slot),
        }
    }

    /// The rate of change of `slot` at `t`, per second.
    fn velocity(&self, slot: u8, t: f64) -> f64 {
        match self.chans.binary_search_by_key(&slot, |(s, _)| *s) {
            Ok(i) => self.chans[i].1.sample(t).1,
            Err(_) => 0.0,
        }
    }

    pub fn has(&self, slot: u8) -> bool {
        self.chans.binary_search_by_key(&slot, |(s, _)| *s).is_ok()
    }

    fn motion_for(&self, slot: u8) -> Motion {
        let prop = Slot::prop(slot);
        if self.reduced {
            match prop {
                Prop::Position | Prop::Size | Prop::Offset | Prop::Scale | Prop::Rotation => return Motion::Snap,
                // No flashing: glow, bloom and shadow follow a level only slowly.
                Prop::Glow | Prop::Bloom | Prop::Shadow => return Motion::Spring(Spring::SLOW),
                _ => {}
            }
        }
        self.motion.iter().find(|(p, _)| *p == prop).map_or(self.default_motion, |(_, m)| *m)
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
        let words = self.words.iter().flat_map(|w| w.channels());
        self.chans.iter().map(|(_, c)| c).chain(words).map(|c| c.settled_at()).fold(f64::NEG_INFINITY, f64::max)
    }

    /// Live text: matches the word motions to `words` at `t`. A new id starts from `enter`, a
    /// kept id with new text jumps to `change`, and both spring to rest (opacity 1, no
    /// offset). Returns whether any motion started or a word went.
    fn sync_words(&mut self, t: f64, retention: f64) -> bool {
        let Kind::LiveText { words, enter, change, .. } = &self.kind else {
            let had = !self.words.is_empty();
            self.words.clear();
            return had;
        };
        let spring =
            |prop: Prop| match self.motion.iter().find(|(p, _)| *p == prop).map_or(self.default_motion, |m| m.1) {
                Motion::Spring(s) => s,
                Motion::Snap => Spring::SNAP,
            };
        let (fade, slide) = (spring(Prop::Opacity), spring(Prop::Offset));
        let chan = |v: f64, s: Spring, rest: Rest| Channel::new(t, v, s).with_rest(rest).with_retention(retention);
        let reduced = self.reduced;
        let from = |tr: &Transition| {
            let tr = if reduced { tr.reduced() } else { *tr };
            let (x, y) = tr.offset.unwrap_or((0.0, 0.0));
            (tr.opacity.unwrap_or(1.0) as f64, x as f64, y as f64)
        };
        let mut changed = false;
        let mut next = Vec::with_capacity(words.len());
        for w in words {
            let old = self.words.binary_search_by_key(&w.id, |a| a.id).ok();
            let start = match old {
                None => Some(from(enter)),
                Some(i) if self.words[i].text != w.text && *change != Transition::NONE => Some(from(change)),
                Some(_) => None,
            };
            let mut anim = match old {
                Some(i) => {
                    let a = &self.words[i];
                    WordAnim {
                        id: a.id,
                        text: w.text.clone(),
                        opacity: a.opacity.clone(),
                        dx: a.dx.clone(),
                        dy: a.dy.clone(),
                    }
                }
                None => WordAnim {
                    id: w.id,
                    text: w.text.clone(),
                    opacity: chan(1.0, fade, Rest::UNIT),
                    dx: chan(0.0, slide, Rest::PIXELS),
                    dy: chan(0.0, slide, Rest::PIXELS),
                },
            };
            if let Some((o, x, y)) = start {
                for (ch, v, rest) in [(&mut anim.opacity, o, 1.0), (&mut anim.dx, x, 0.0), (&mut anim.dy, y, 0.0)] {
                    if v != rest {
                        ch.snap(t, v);
                        ch.retarget(t, rest);
                        changed = true;
                    }
                }
            }
            next.push(anim);
        }
        next.sort_by_key(|a| a.id);
        changed |= next.len() != self.words.len() || next.iter().zip(&self.words).any(|(a, b)| a.id != b.id);
        self.words = next;
        changed
    }

    /// Live text: a word's `(opacity, dx, dy)` at `t`.
    fn word_motion(&self, id: u32, t: f64) -> (f32, f32, f32) {
        match self.words.binary_search_by_key(&id, |a| a.id) {
            Ok(i) => {
                let a = &self.words[i];
                (a.opacity.value(t).clamp(0.0, 1.0) as f32, a.dx.value(t) as f32, a.dy.value(t) as f32)
            }
            Err(_) => (1.0, 0.0, 0.0),
        }
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
    default_motion: Motion,
    reduced_motion: bool,
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
            default_motion: DEFAULT_MOTION,
            reduced_motion: false,
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

    /// How properties move that no `.motion()` call names. `DEFAULT_MOTION` (a soft spring)
    /// unless set; `Motion::Snap` makes every unnamed change jump, as in a plain toolkit.
    pub fn with_default_motion(mut self, motion: Motion) -> Scene<M> {
        self.default_motion = motion;
        self
    }

    /// Reduced motion (MO09): position, size, offset, scale and rotation jump to new targets,
    /// presence and word transitions keep only their fade, and glow, bloom and shadow follow
    /// their targets slowly so a level cannot make them flash. Fades still show every change.
    pub fn with_reduced_motion(mut self, reduced: bool) -> Scene<M> {
        self.reduced_motion = reduced;
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
        let moving = self.nodes.values().any(|n| {
            n.chans.iter().any(|(_, c)| c.settled_at() > t)
                || n.words.iter().flat_map(|w| w.channels()).any(|c| c.settled_at() > t)
                || (n.alive(t)
                    && match &n.kind {
                        Kind::Wave(w) => w.moving(t),
                        Kind::Progress(p) => p.moving(),
                        Kind::Dots(d) => d.source.moving(t),
                        Kind::Canvas(c) => c.moving(t),
                        _ => false,
                    })
        });
        if moving {
            return Some(t);
        }
        // A feedback trail fades on after its content stops, for 12 half lives: to 0.02%,
        // under one 8-bit level even after sRGB encoding.
        let last = self
            .nodes
            .values()
            .flat_map(|n| n.chans.iter().map(|(_, c)| c.settled_at()).chain([n.since]))
            .fold(f64::NEG_INFINITY, f64::max);
        let fading = self.nodes.values().any(|n| {
            n.alive(t)
                && n.has(Slot::TrailHalfLife as u8)
                && t < last + 12.0 * n.value(Slot::TrailHalfLife as u8, t)
        });
        if fading {
            return Some(t);
        }
        // Glass animates at its own capped rate (FX08), so it wakes at a time, not now.
        self.nodes
            .values()
            .filter(|n| n.alive(t) && matches!(n.kind, Kind::Panel))
            .filter_map(|n| n.glass.as_ref().and_then(|g| g.next_frame(n.since, t)))
            .min_by(f64::total_cmp)
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
        let Element {
            key,
            kind,
            style,
            targets,
            gradient,
            hairline,
            inside,
            glass,
            motion,
            presence,
            handlers,
            children,
        } = el;
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
            inside,
            glass,
            handlers,
            born: t,
            leaving: None,
            since: t,
            entered: t,
            shaped: ShapedText::default(),
            has_layout: false,
            words: Vec::new(),
            default_motion: self.default_motion,
            reduced: self.reduced_motion,
        };
        node.sync_words(t, self.retention);
        let enter = presence.map_or(Transition::NONE, |(enter, _)| enter);
        let enter = if self.reduced_motion { enter.reduced() } else { enter };
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
        let Element {
            key: _,
            kind,
            style,
            targets,
            gradient,
            hairline,
            inside,
            glass,
            motion,
            presence,
            handlers,
            children,
        } = el;
        let retention = self.retention;
        let n = &mut self.nodes[id];
        let relayout = n.style != style || (kind.is_text() && n.kind != kind);
        let mut changed = n.kind != kind || n.gradient != gradient || n.hairline != hairline || n.inside != inside;
        changed |= n.glass != glass;
        changed |= n.handlers.hit != handlers.hit;
        n.style = style;
        n.kind = kind;
        n.motion = motion;
        n.presence = presence;
        n.gradient = gradient;
        n.hairline = hairline;
        n.inside = inside;
        n.glass = glass;
        n.handlers = handlers;
        for &(slot, v) in &targets {
            changed |= n.set_target(slot, v, t, retention);
        }
        changed |= n.sync_words(t, retention);
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
        self.changed |= old != order;
        let n = &mut self.nodes[id];
        let changed = n.flow != flow;
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
        let exit = if n.reduced { exit.reduced() } else { exit };
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
        // Motion blur: the distance moved while the shutter is open, in the parent's units.
        let mut smear = Point::ZERO;
        if n.has(Slot::Shutter as u8) {
            let shutter = n.value(Slot::Shutter as u8, t);
            let v = |a: Slot, b: Slot| (n.velocity(a as u8, t) + n.velocity(b as u8, t)) * shutter;
            smear = Point::new(v(Slot::X, Slot::OffsetX) as f32, v(Slot::Y, Slot::OffsetY) as f32);
            // In the node's own units, which its scale divides.
            let k = n.value(Slot::Scale as u8, t).abs().max(1e-3) as f32;
            smear = Point::new(smear.x / k, smear.y / k);
        }
        let blurred = smear.x.abs() + smear.y.abs() > 0.01;
        if !as_layer && !blurred && m.is_translation() {
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
                let bloom = n.has(Slot::BloomStrength as u8).then(|| Bloom {
                    strength: n.value(Slot::BloomStrength as u8, t).max(0.0) as f32,
                    radius: n.value(Slot::BloomRadius as u8, t).max(0.0) as f32,
                });
                let target = if offscreen { LayerTarget::Offscreen { bloom } } else { LayerTarget::Direct };
                (blend, clip, target, alpha * opacity, 1.0)
            }
            _ => (BlendMode::Normal, None, LayerTarget::Direct, 1.0, alpha * opacity),
        };
        out.layers.push(Layer {
            parent: Some(layer),
            transform,
            opacity: group,
            blend,
            clip,
            target,
            motion_blur: smear,
            trail: n.has(Slot::TrailHalfLife as u8).then(|| Trail {
                key: id.to_bits(),
                half_life: n.value(Slot::TrailHalfLife as u8, t) as f32,
                t,
            }),
            items: 0..0,
        });
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
        if let (Kind::Panel, Some(g)) = (&n.kind, &n.glass) {
            let (sheen, orbit, orbit_strength) = g.motion(n.since, t);
            push_item(items, ItemKind::Glass, out.glass.len() as u32);
            out.glass.push(GlassInstance {
                rect: [m.e, m.f, size.w, size.h],
                radius: radius(n, t),
                glass: *g,
                alpha,
                age: (t - n.since) as f32,
                sheen,
                orbit,
                orbit_strength,
                to_surface: out.layers[layer as usize].transform,
            });
        }
        if let Kind::Canvas(c) = &n.kind {
            let mut painter = crate::canvas::Painter { list: out, items, alpha, layer };
            (c.draw)(t, Rect::new(m.e, m.f, size.w, size.h), &mut painter);
        } else {
            self.draw(n, t, Point::new(m.e, m.f), size, alpha, items, out);
        }
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
        // An inside stroke is a centred one on the outline moved in by half its width; the
        // fill reaches the stroke's centre, so fill and stroke still cover the whole box.
        let boxed = || {
            let inset = if n.inside && !n.hairline && n.has(Slot::Stroke as u8) {
                0.5 * n.value(Slot::StrokeWidth as u8, t).max(0.0) as f32
            } else {
                0.0
            };
            let r = Rect::new(
                rect.x + inset,
                rect.y + inset,
                (rect.w - 2.0 * inset).max(0.0),
                (rect.h - 2.0 * inset).max(0.0),
            );
            RRect::new(r, (radius(n, t) - inset).max(0.0))
        };
        let shape = match &n.kind {
            Kind::Panel | Kind::Layer { .. } => Some(ShapeInstance::rrect(boxed(), fill())),
            Kind::Shape(kind) => Some(match kind {
                ShapeKind::RRect => ShapeInstance::rrect(boxed(), fill()),
                ShapeKind::Circle => {
                    let centre = Point::new(origin.x + 0.5 * size.w, origin.y + 0.5 * size.h);
                    ShapeInstance::circle(centre, 0.5 * size.w.min(size.h), fill())
                }
                ShapeKind::Capsule => ShapeInstance::capsule(rect, fill()),
                ShapeKind::Line => {
                    ShapeInstance::line(origin, Point::new(rect.right(), rect.bottom()), 0.0, Paint::NONE)
                }
                ShapeKind::Band => {
                    let half = 0.5 * size.h;
                    ShapeInstance::band(origin.x, rect.right(), origin.y + half, half, half, fill())
                }
            }),
            Kind::Label { style, .. } => {
                let color = n.color(Slot::Text, t).fade(alpha);
                self.glyphs(n, style.font, style.size, origin, color, |_| NO_WORD, items, out);
                None
            }
            Kind::LiveText { words, style, .. } => {
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
                if n.words.is_empty() || color.a <= 0.0 {
                    self.glyphs(n, style.font, style.size, origin, color, word_of, items, out);
                } else {
                    for g in &n.shaped.glyphs {
                        let word = word_of(g.cluster);
                        let (k, dx, dy) = n.word_motion(word, t);
                        if k <= 0.0 {
                            continue;
                        }
                        push_item(items, ItemKind::Glyphs, out.glyphs.len() as u32);
                        out.glyphs.push(GlyphInstance {
                            pos: Point::new(origin.x + g.pos.x + dx, origin.y + g.pos.y + dy),
                            size: style.size,
                            font: style.font,
                            glyph: g.glyph,
                            word,
                            color: color.fade(k),
                        });
                    }
                }
                None
            }
            Kind::Wave(w) => {
                self.timed_shapes(items, out, |v| w.shapes(rect, t, alpha, v));
                None
            }
            Kind::Progress(p) => {
                self.timed_shapes(items, out, |v| p.shapes(rect, t, alpha, v));
                None
            }
            Kind::Dots(d) => {
                self.dots(d, rect, t, alpha, items, out);
                None
            }
            // Drawn by `emit_body`, which knows the layer.
            Kind::Canvas(_) => None,
            Kind::Path { id, view } => {
                let paint = fill();
                if paint.is_visible() && view.w > 0.0 && view.h > 0.0 {
                    let k = (size.w / view.w).min(size.h / view.h);
                    let pos = Point::new(
                        origin.x + 0.5 * (size.w - view.w * k) - view.x * k,
                        origin.y + 0.5 * (size.h - view.h * k) - view.y * k,
                    );
                    push_item(items, ItemKind::Glyphs, out.glyphs.len() as u32);
                    out.glyphs.push(crate::path::fill_glyph(*id, pos, k, paint.c0));
                }
                None
            }
            Kind::Stack => None,
        };
        if let Some(s) = shape.map(stroked) {
            // Shadow, then glow, then the shape: copies of its outline with a halo fill. The
            // stroke width stays so the halo starts at the stroke's outer edge.
            let halo = |fill: Paint| ShapeInstance { fill, stroke: Paint::NONE, flags: 0, ..s };
            let mut halos = [None, None];
            if n.has(Slot::ShadowColor as u8) {
                let offset = Point::new(n.value(Slot::ShadowX as u8, t) as f32, n.value(Slot::ShadowY as u8, t) as f32);
                let blur = n.value(Slot::ShadowBlur as u8, t).max(0.0) as f32;
                halos[0] = Some(halo(Paint::shadow(n.color(Slot::ShadowColor, t), offset, blur, 0.0).fade(alpha)));
            }
            if n.has(Slot::GlowColor as u8) {
                let radius = n.value(Slot::GlowRadius as u8, t).max(0.0) as f32;
                let strength = n.value(Slot::GlowStrength as u8, t).max(0.0) as f32;
                halos[1] = Some(halo(Paint::glow(n.color(Slot::GlowColor, t), radius, strength).fade(alpha)));
            }
            for h in halos.into_iter().flatten().filter(|h| h.fill.is_visible()) {
                push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
                out.shapes.push(h);
            }
            if s.fill.is_visible() || s.stroke.is_visible() {
                push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
                out.shapes.push(s);
            }
        }
    }

    /// Shapes from a timed element, appended in order.
    fn timed_shapes(&self, items: &mut Vec<Item>, out: &mut DisplayList, f: impl FnOnce(&mut Vec<ShapeInstance>)) {
        let start = out.shapes.len();
        f(&mut out.shapes);
        for i in start..out.shapes.len() {
            push_item(items, ItemKind::Shapes, i as u32);
        }
    }

    /// A dot grid's dots in `rect`, one run of `ItemKind::Dots`.
    fn dots(&self, d: &Dots, rect: Rect, t: f64, alpha: f32, items: &mut Vec<Item>, out: &mut DisplayList) {
        let pitch = d.snapped_pitch(self.scale);
        let (ox, oy) = (rect.x + d.origin.0 * pitch, rect.y + d.origin.1 * pitch);
        let mut cells = Vec::new();
        d.source.dots(t, &mut cells);
        let start = out.dots.len();
        out.dots.extend(cells.iter().filter(|c| c.color.a * alpha > 0.0 && c.size > 0.0).map(|c| DotInstance {
            centre: Point::new(ox + c.x * pitch, oy + c.y * pitch),
            radius: 0.5 * c.size * pitch,
            snap: c.snap.clamp(0.0, 1.0),
            color: c.color.fade(alpha),
        }));
        for i in start..out.dots.len() {
            push_item(items, ItemKind::Dots, i as u32);
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
