//! Repeaters: children and ticks placed around a ring ([`Element::polar`]) or along a path
//! ([`Element::along_path`]).
//!
//! A repeater is a box like any other; its children skip flex layout and sit on the ring or
//! path instead, spread evenly over its sweep or range. Each child gets two channels of its
//! own, its place along the ring or path and its distance from it, so when a child comes or
//! goes the others slide along the arc to their new places on their own springs, not across
//! the chord. Their motion is `Prop::Position` on the child, so `.motion(Prop::Position, ..)`
//! on a child gives it its own spring. [`Element::radial`] moves one child in or out of the
//! ring (`Prop::Offset`).
//!
//! The ring itself (radius, start angle, sweep) or the path range (normal offset, from, to)
//! are channels of the repeater, `Prop::Ring`. Ticks ([`Ticks`]) are drawn by the repeater
//! itself, one SDF line each, so a ring of 120 ticks costs 120 instances and no nodes. The
//! repeater's trim (`.trim`, `Prop::Trim`) draws the ticks on.
//!
//! Angles are radians, clockwise on screen, 0 pointing right, around the centre of the box.
//! Paths are in the repeater's own box space.

use std::f32::consts::TAU;
use std::sync::Arc;

use crate::display::{DisplayList, Item, ItemKind, Paint, ShapeInstance};
use crate::element::{Element, Key, Kind, Slot};
use crate::geom::{Affine, Color, Point, Rect, Size};
use crate::path::Contour;
use crate::reach::Insets;
use crate::scene::{Node, NodeId};
use slotmap::SlotMap;

/// Where the children of a repeater go.
#[derive(Clone, Debug, PartialEq)]
pub enum RepeatPath {
    /// Around the centre of the box at `radius`, from angle `start` over `sweep`. A sweep of a
    /// full turn spaces `n` children `1/n` of a turn apart; a shorter one puts the first and
    /// last child at its ends.
    Ring { radius: f32, start: f32, sweep: f32 },
    /// Along `contour` (in the box's space) from fraction `from` to `to` of its length,
    /// `offset` to the left of the direction of travel (negative goes right).
    Path { contour: Arc<Contour>, from: f32, to: f32, offset: f32 },
}

/// How a placed child turns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orient {
    /// It keeps its own rotation.
    #[default]
    Upright,
    /// Its +x axis points away from the ring's centre, or to the left of the path.
    Outward,
    /// Its +x axis points along the ring (clockwise) or the path.
    Along,
}

/// Tick marks drawn by the repeater: `count` of them over the sweep or range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ticks {
    pub count: u32,
    pub length: f32,
    pub width: f32,
    pub color: Color,
    /// Every `major_every`-th tick (from the first) is a major tick; 0 for none.
    pub major_every: u32,
    pub major_length: f32,
    pub major_width: f32,
    pub major_color: Color,
    /// Where a tick sits across the ring or path: -1 all inside, 0 centred, 1 all outside.
    pub align: f32,
    /// Ticks up to this fraction of the count are drawn in `lit_color` (a gauge).
    pub lit: f32,
    pub lit_color: Color,
}

impl Ticks {
    pub fn new(count: u32, length: f32, width: f32, color: Color) -> Ticks {
        Ticks {
            count,
            length,
            width,
            color,
            major_every: 0,
            major_length: length,
            major_width: width,
            major_color: color,
            align: 0.0,
            lit: 0.0,
            lit_color: color,
        }
    }

    /// Every `every`-th tick is longer, wider and in its own colour.
    pub fn major(self, every: u32, length: f32, width: f32, color: Color) -> Ticks {
        Ticks { major_every: every, major_length: length, major_width: width, major_color: color, ..self }
    }

    /// -1 all inside the ring, 0 centred on it, 1 all outside.
    pub fn align(self, align: f32) -> Ticks {
        Ticks { align: align.clamp(-1.0, 1.0), ..self }
    }

    /// The first `fraction` of the ticks in `color`.
    pub fn lit(self, fraction: f32, color: Color) -> Ticks {
        Ticks { lit: fraction.clamp(0.0, 1.0), lit_color: color, ..self }
    }
}

/// What a repeater element is (`Kind::Repeat`).
#[derive(Clone, Debug, PartialEq)]
pub struct Repeat {
    pub path: RepeatPath,
    pub orient: Orient,
    pub ticks: Option<Ticks>,
}

impl Repeat {
    /// The ring numbers the element asks for: radius or offset, start or from, sweep or to.
    fn targets(&self) -> [f32; 3] {
        match &self.path {
            RepeatPath::Ring { radius, start, sweep } => [*radius, *start, *sweep],
            RepeatPath::Path { from, to, offset, .. } => [*offset, *from, *to],
        }
    }

    /// Where item `i` of `n` goes, from the ring numbers: an angle on a ring, a fraction of
    /// the length on a path. A closed ring or path spaces the items evenly round it; an open
    /// one puts the first and last at its ends, and a single item in the middle.
    pub fn place(&self, [_, a, b]: [f32; 3], i: usize, n: usize) -> f32 {
        let (start, span, closed) = match &self.path {
            RepeatPath::Ring { .. } => (a, b, b.abs() >= TAU - 1e-4),
            RepeatPath::Path { contour, .. } => (a, b - a, contour.closed && (b - a).abs() >= 1.0 - 1e-4),
        };
        if closed {
            start + span * i as f32 / n.max(1) as f32
        } else if n <= 1 {
            start + 0.5 * span
        } else {
            start + span * i as f32 / (n - 1) as f32
        }
    }

    /// The point at place `at` and distance `r` from the ring or path, in a box of `size`,
    /// with the outward direction and the direction of travel.
    fn frame(&self, size: Size, at: f32, r: f32) -> Option<(Point, Point, Point)> {
        match &self.path {
            RepeatPath::Ring { .. } => {
                let (s, c) = at.sin_cos();
                let p = Point::new(0.5 * size.w + r * c, 0.5 * size.h + r * s);
                Some((p, Point::new(c, s), Point::new(-s, c)))
            }
            RepeatPath::Path { contour, .. } => {
                let (p, d) = contour.at(at.clamp(0.0, 1.0) * contour.length())?;
                // Left of the direction of travel, with y down.
                let normal = Point::new(d.y, -d.x);
                Some((Point::new(p.x + normal.x * r, p.y + normal.y * r), normal, d))
            }
        }
    }

    /// The centre of a child at place `at` and distance `r`, with its turn, in the repeater's
    /// box of `size`.
    pub fn locate(&self, size: Size, at: f32, r: f32) -> (Point, f32) {
        let Some((p, out, along)) = self.frame(size, at, r) else { return (Point::ZERO, 0.0) };
        let turn = match self.orient {
            Orient::Upright => 0.0,
            Orient::Outward => out.y.atan2(out.x),
            Orient::Along => along.y.atan2(along.x),
        };
        (p, turn)
    }
}

impl<M> Element<M> {
    /// A ring repeater: its children sit around the centre of its `w` x `h` box at `radius`,
    /// spread from angle `start` (radians, clockwise, 0 right) over `sweep` (a full turn,
    /// `TAU`, closes the ring). Add ticks with [`Element::ticks`] and turn children with
    /// [`Element::orient`].
    pub fn polar(key: impl Into<Key>, w: f32, h: f32, radius: f32, start: f32, sweep: f32) -> Element<M> {
        let repeat = Repeat { path: RepeatPath::Ring { radius, start, sweep }, orient: Orient::Upright, ticks: None };
        Element::repeat(key, w, h, repeat)
    }

    /// A path repeater: its children sit along `contour` (in its `w` x `h` box's space) from
    /// fraction `from` to `to` of its length.
    pub fn along_path(key: impl Into<Key>, w: f32, h: f32, contour: Arc<Contour>, from: f32, to: f32) -> Element<M> {
        let repeat =
            Repeat { path: RepeatPath::Path { contour, from, to, offset: 0.0 }, orient: Orient::Upright, ticks: None };
        Element::repeat(key, w, h, repeat)
    }

    fn repeat(key: impl Into<Key>, w: f32, h: f32, repeat: Repeat) -> Element<M> {
        let targets = repeat.targets();
        let mut e = Element::new(key, Kind::Repeat(repeat), taffy::Style::default()).size(w, h);
        // Ticks fully drawn unless `.trim` says otherwise, so a presence trim draws them on.
        e.targets.push((Slot::TrimEnd as u8, 1.0));
        for (slot, v) in [Slot::RepeatA, Slot::RepeatB, Slot::RepeatC].into_iter().zip(targets) {
            e.targets.push((slot as u8, v as f64));
        }
        e
    }

    /// Ticks drawn by a repeater.
    pub fn ticks(mut self, ticks: Ticks) -> Element<M> {
        match &mut self.kind {
            Kind::Repeat(r) => r.ticks = Some(ticks),
            _ => debug_assert!(false, "ticks on a node that is not a repeater: {}", self.key),
        }
        self
    }

    /// How a repeater's children turn.
    pub fn orient(mut self, orient: Orient) -> Element<M> {
        match &mut self.kind {
            Kind::Repeat(r) => r.orient = orient,
            _ => debug_assert!(false, "orient on a node that is not a repeater: {}", self.key),
        }
        self
    }

    /// A path repeater: its children sit `offset` to the left of the path (negative goes
    /// right).
    pub fn path_offset(mut self, offset: f32) -> Element<M> {
        match &mut self.kind {
            Kind::Repeat(Repeat { path: RepeatPath::Path { offset: o, .. }, .. }) => *o = offset,
            _ => debug_assert!(false, "path_offset on a node that is not a path repeater: {}", self.key),
        }
        self.targets.retain(|(k, _)| *k != Slot::RepeatA as u8);
        self.targets.push((Slot::RepeatA as u8, offset as f64));
        self
    }

    /// A repeater's child: `dr` further out from the ring (or further left of the path) than
    /// the others. Springs with `Prop::Offset`.
    pub fn radial(mut self, dr: f32) -> Element<M> {
        self.targets.retain(|(k, _)| *k != Slot::Radial as u8);
        self.targets.push((Slot::Radial as u8, dr as f64));
        self
    }
}

/// A repeater's child skips flex layout: it is placed, not laid out.
pub(crate) fn adopt(parent: &Kind, child: &mut taffy::Style) {
    if matches!(parent, Kind::Repeat(_)) {
        child.position = taffy::style::Position::Absolute;
    }
}

/// Slots the repeater sets on its children, which their own view never names.
pub(crate) fn placed_slot(slot: u8) -> bool {
    slot == Slot::ItemAt as u8 || slot == Slot::ItemRadius as u8
}

/// The ring numbers a repeater heads to.
fn ring_targets<M>(n: &Node<M>) -> [f32; 3] {
    let target = |s: Slot| {
        let slot = s as u8;
        n.chans.iter().find(|(k, _)| *k == slot).map_or(Slot::default_value(slot), |(_, c)| c.target()) as f32
    };
    [target(Slot::RepeatA), target(Slot::RepeatB), target(Slot::RepeatC)]
}

/// The ring numbers at `t`.
fn ring_at<M>(n: &Node<M>, t: f64) -> [f32; 3] {
    let v = |s: Slot| n.value(s as u8, t) as f32;
    [v(Slot::RepeatA), v(Slot::RepeatB), v(Slot::RepeatC)]
}

/// Moves every repeater's children toward their places. Children that are leaving keep
/// theirs; the others spread over the ring. A child placed for the first time starts at its
/// place. Returns whether any motion changed.
pub(crate) fn place_children<M>(nodes: &mut SlotMap<NodeId, Node<M>>, t: f64, retention: f64) -> bool {
    let repeaters: Vec<NodeId> =
        nodes.iter().filter(|(_, n)| matches!(n.kind, Kind::Repeat(_))).map(|(id, _)| id).collect();
    let mut changed = false;
    for id in repeaters {
        let n = &nodes[id];
        let Kind::Repeat(r) = &n.kind else { continue };
        let ring = ring_targets(n);
        let count = n.flow.len();
        let places: Vec<(NodeId, f32)> =
            n.flow.iter().enumerate().map(|(i, &c)| (c, r.place(ring, i, count))).collect();
        for (c, at) in places {
            let child = &mut nodes[c];
            for (slot, v) in [(Slot::ItemAt, at), (Slot::ItemRadius, ring[0])] {
                let slot = slot as u8;
                if child.has(slot) {
                    changed |= child.set_target(slot, v as f64, t, retention);
                } else {
                    child.start_at(slot, v as f64, t, retention);
                    changed = true;
                }
            }
        }
    }
    changed
}

/// The transform of `n` with the box of `size` to its parent's space at `t` when its parent
/// is a repeater: its place on the ring or path instead of its layout box.
pub(crate) fn placed<M>(nodes: &SlotMap<NodeId, Node<M>>, n: &Node<M>, t: f64, size: Size) -> Option<Affine> {
    child_transform(&nodes[n.parent?], n, t, size)
}

/// The transform of a repeater's child from its own space to the repeater's at `t`, with
/// the child's box of `size`. `None` when the parent is not a repeater or the child was
/// never placed.
pub(crate) fn child_transform<M>(parent: &Node<M>, child: &Node<M>, t: f64, size: Size) -> Option<Affine> {
    let Kind::Repeat(r) = &parent.kind else { return None };
    if !child.has(Slot::ItemAt as u8) {
        return None;
    }
    let v = |n: &Node<M>, s: Slot| n.value(s as u8, t) as f32;
    let psize = Size::new(v(parent, Slot::W).max(0.0), v(parent, Slot::H).max(0.0));
    let r_at = v(child, Slot::ItemRadius) + v(child, Slot::Radial);
    let (centre, turn) = r.locate(psize, v(child, Slot::ItemAt), r_at);
    let (ox, oy) = (v(child, Slot::OffsetX), v(child, Slot::OffsetY));
    let (scale, rotation) = (v(child, Slot::Scale), v(child, Slot::Rotation) + turn);
    let (cx, cy) = (0.5 * size.w, 0.5 * size.h);
    Some(if scale == 1.0 && rotation == 0.0 {
        Affine::translate(centre.x - cx + ox, centre.y - cy + oy)
    } else {
        Affine::translate(-cx, -cy)
            .then(&Affine::scale(scale))
            .then(&Affine::rotate(rotation))
            .then(&Affine::translate(centre.x + ox, centre.y + oy))
    })
}

/// The box a placed child covers at `t` in the repeater's space, turned boxes included.
pub(crate) fn placed_box<M>(parent: &Node<M>, child: &Node<M>, t: f64) -> Option<Rect> {
    let v = |s: Slot| child.value(s as u8, t) as f32;
    let size = Size::new(v(Slot::W).max(0.0), v(Slot::H).max(0.0));
    let m = child_transform(parent, child, t, size)?;
    let corners = [(0.0, 0.0), (size.w, 0.0), (0.0, size.h), (size.w, size.h)].map(|(x, y)| m.apply(Point::new(x, y)));
    let (x0, y0) = corners.iter().fold((f32::MAX, f32::MAX), |(x, y), p| (x.min(p.x), y.min(p.y)));
    let (x1, y1) = corners.iter().fold((f32::MIN, f32::MIN), |(x, y), p| (x.max(p.x), y.max(p.y)));
    Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// How far a child of `child` size placed by `r` may reach outside the repeater's box of
/// `size`, before layout.
pub(crate) fn child_overflow(r: &Repeat, size: Option<Size>, child: Option<Size>) -> Insets {
    let size = size.unwrap_or(Size::ZERO);
    let half_diag = child.map_or(0.0, |c| 0.5 * c.w.hypot(c.h));
    let [dist, _, _] = r.targets();
    match &r.path {
        RepeatPath::Ring { .. } => Insets::uniform((dist.abs() + half_diag - 0.5 * size.w.min(size.h)).max(0.0)),
        RepeatPath::Path { contour, .. } => {
            let pts = contour.resample(64);
            let Some(first) = pts.first() else { return Insets::ZERO };
            let bounds = pts.iter().fold(Rect::new(first.x, first.y, 0.0, 0.0), |b, p| {
                let (x0, y0) = (b.x.min(p.x), b.y.min(p.y));
                Rect::new(x0, y0, b.right().max(p.x) - x0, b.bottom().max(p.y) - y0)
            });
            let ink = Insets::uniform(dist.abs() + half_diag).expand(bounds);
            Insets::outside(ink, Rect::from_size(size))
        }
    }
}

/// The ticks of a repeater at `t`, at `origin` in the current layer.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw<M>(
    r: &Repeat,
    n: &Node<M>,
    t: f64,
    origin: Point,
    size: Size,
    alpha: f32,
    items: &mut Vec<Item>,
    out: &mut DisplayList,
) {
    let Some(ticks) = r.ticks else { return };
    if ticks.count == 0 {
        return;
    }
    let ring = ring_at(n, t);
    let trim_start = n.value(Slot::TrimStart as u8, t).clamp(0.0, 1.0) as f32;
    let trim_end =
        if n.has(Slot::TrimEnd as u8) { n.value(Slot::TrimEnd as u8, t).clamp(0.0, 1.0) as f32 } else { 1.0 };
    let count = ticks.count as usize;
    for i in 0..count {
        let f = if count > 1 { i as f32 / (count - 1) as f32 } else { 0.0 };
        if trim_end <= trim_start || f < trim_start || f > trim_end + 1e-6 {
            continue;
        }
        let major = ticks.major_every > 0 && (i as u32).is_multiple_of(ticks.major_every);
        let (len, width, mut color) = if major {
            (ticks.major_length, ticks.major_width, ticks.major_color)
        } else {
            (ticks.length, ticks.width, ticks.color)
        };
        if ticks.lit > 0.0 && (i as f32 + 0.5) / count as f32 <= ticks.lit {
            color = ticks.lit_color;
        }
        let color = color.fade(alpha);
        if color.a <= 0.0 || width <= 0.0 {
            continue;
        }
        let Some((p, out_dir, _)) = r.frame(size, r.place(ring, i, count), ring[0]) else { continue };
        let a = 0.5 * len * (ticks.align - 1.0);
        let b = 0.5 * len * (ticks.align + 1.0);
        let p0 = Point::new(origin.x + p.x + out_dir.x * a, origin.y + p.y + out_dir.y * a);
        let p1 = Point::new(origin.x + p.x + out_dir.x * b, origin.y + p.y + out_dir.y * b);
        let index = out.shapes.len() as u32;
        out.shapes.push(ShapeInstance::line(p0, p1, width, Paint::solid(color)));
        match items.last_mut() {
            Some(last) if last.kind == ItemKind::Shapes && last.start + last.len == index => last.len += 1,
            _ => items.push(Item { kind: ItemKind::Shapes, start: index, len: 1 }),
        }
    }
}

/// How far a repeater's ticks reach outside its box of `size`.
pub(crate) fn reach(r: &Repeat, size: Option<Size>) -> f32 {
    let Some(t) = r.ticks else { return 0.0 };
    let [dist, _, _] = r.targets();
    let len = t.length.max(t.major_length);
    let out = 0.5 * len * (t.align + 1.0) + 0.5 * t.width.max(t.major_width);
    match &r.path {
        RepeatPath::Ring { .. } => (dist + out - size.map_or(0.0, |s| 0.5 * s.w.min(s.h))).max(0.0),
        RepeatPath::Path { .. } => child_overflow(r, size, None).largest() + out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Scene;
    use std::f32::consts::FRAC_PI_2;

    fn ring(start: f32, sweep: f32) -> Repeat {
        Repeat { path: RepeatPath::Ring { radius: 10.0, start, sweep }, orient: Orient::Upright, ticks: None }
    }

    #[test]
    fn a_full_ring_spaces_items_evenly_and_an_arc_includes_its_ends() {
        let full = ring(0.0, TAU);
        assert_eq!(full.place([10.0, 0.0, TAU], 1, 4), TAU / 4.0);
        let arc = ring(0.0, 1.0);
        assert_eq!(arc.place([10.0, 0.0, 1.0], 0, 3), 0.0);
        assert_eq!(arc.place([10.0, 0.0, 1.0], 2, 3), 1.0);
        assert_eq!(arc.place([10.0, 0.0, 1.0], 0, 1), 0.5);
    }

    #[test]
    fn locate_on_a_ring_and_orient() {
        let mut r = ring(0.0, TAU);
        let (p, turn) = r.locate(Size::new(100.0, 100.0), FRAC_PI_2, 20.0);
        assert!((p.x - 50.0).abs() < 1e-4 && (p.y - 70.0).abs() < 1e-4, "{p:?}");
        assert_eq!(turn, 0.0);
        r.orient = Orient::Outward;
        assert!((r.locate(Size::new(100.0, 100.0), 1.0, 20.0).1 - 1.0).abs() < 1e-5);
        r.orient = Orient::Along;
        assert!((r.locate(Size::new(100.0, 100.0), 1.0, 20.0).1 - (1.0 + FRAC_PI_2)).abs() < 1e-5);
    }

    #[test]
    fn locate_along_a_path_offsets_to_the_left() {
        let line = Contour::polyline(&[Point::new(0.0, 50.0), Point::new(100.0, 50.0)], false);
        let r = Repeat {
            path: RepeatPath::Path { contour: Arc::new(line), from: 0.0, to: 1.0, offset: 0.0 },
            orient: Orient::Along,
            ticks: None,
        };
        // Moving right with y down, left is up.
        let (p, turn) = r.locate(Size::new(100.0, 100.0), 0.25, 5.0);
        assert!((p.x - 25.0).abs() < 1e-3 && (p.y - 45.0).abs() < 1e-3, "{p:?}");
        assert!(turn.abs() < 1e-6);
        assert_eq!(r.place([0.0, 0.0, 1.0], 2, 5), 0.5);
    }

    #[test]
    fn ticks_reach_past_a_small_box() {
        let r = Repeat {
            path: RepeatPath::Ring { radius: 50.0, start: 0.0, sweep: TAU },
            orient: Orient::Upright,
            ticks: Some(Ticks::new(60, 8.0, 2.0, Color::WHITE).align(1.0)),
        };
        // Out to 50 + 8 + 1 from the centre of a 100 box.
        assert_eq!(reach(&r, Some(Size::new(100.0, 100.0))), 9.0);
    }

    fn dot(key: &'static str) -> Element<()> {
        Element::panel(key).size(10.0, 10.0).fill(Color::WHITE)
    }

    fn dial(keys: &[&'static str]) -> Element<()> {
        let ring = Element::polar("ring", 200.0, 200.0, 80.0, 0.0, TAU).children(keys.iter().map(|k| dot(k)));
        Element::column("root").size(200.0, 200.0).child(ring)
    }

    fn centre_of(scene: &Scene<()>, key: &str, t: f64) -> Point {
        let id = scene.find(&["root", "ring", key]).expect("child");
        let s = scene.sampled(id, t).expect("drawn");
        s.transform.apply(Point::new(5.0, 5.0))
    }

    #[test]
    fn children_sit_on_the_ring() {
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(200.0, 200.0), 1.0);
        scene.apply(dial(&["a", "b", "c", "d"]), 0.0);
        let near = |p: Point, x: f32, y: f32| (p.x - x).abs() < 1e-3 && (p.y - y).abs() < 1e-3;
        assert!(near(centre_of(&scene, "a", 0.0), 180.0, 100.0), "{:?}", centre_of(&scene, "a", 0.0));
        assert!(near(centre_of(&scene, "b", 0.0), 100.0, 180.0), "{:?}", centre_of(&scene, "b", 0.0));
        assert!(near(centre_of(&scene, "d", 0.0), 100.0, 20.0), "{:?}", centre_of(&scene, "d", 0.0));
        // Applying the same view again changes nothing.
        assert!(!scene.apply(dial(&["a", "b", "c", "d"]), 1.0));
        assert_eq!(scene.next_wake(1.0), None);
    }

    #[test]
    fn a_child_going_slides_the_others_along_the_arc() {
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(200.0, 200.0), 1.0);
        scene.apply(dial(&["a", "b", "c", "d"]), 0.0);
        scene.apply(dial(&["a", "c", "d"]), 1.0);
        // "c" heads from a half turn to a third of a turn, on the ring all the way.
        let mut moved = false;
        for i in 1..30 {
            let p = centre_of(&scene, "c", 1.0 + i as f64 * 0.02);
            let r = (p.x - 100.0).hypot(p.y - 100.0);
            assert!((r - 80.0).abs() < 1e-2, "off the ring at step {i}: {r}");
            moved |= p.y > 1.0 + 100.0;
        }
        assert!(moved);
        let end = centre_of(&scene, "c", 10.0);
        let want = Point::new(100.0 + 80.0 * (TAU / 3.0).cos(), 100.0 + 80.0 * (TAU / 3.0).sin());
        assert!((end.x - want.x).abs() < 1e-2 && (end.y - want.y).abs() < 1e-2, "{end:?}");
    }

    #[test]
    fn ticks_are_one_line_each_and_trim_draws_them_on() {
        let view = |trim: f32| {
            Element::<()>::polar("ring", 200.0, 200.0, 80.0, 0.0, TAU)
                .ticks(Ticks::new(60, 6.0, 1.0, Color::WHITE).major(5, 12.0, 2.0, Color::WHITE))
                .trim(0.0, trim)
        };
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(200.0, 200.0), 1.0);
        scene.apply(view(1.0), 0.0);
        assert_eq!(scene.sample(0.0).shapes.len(), 60);
        let mut half: Scene<()> = Scene::new();
        half.set_viewport(0.0, Size::new(200.0, 200.0), 1.0);
        half.apply(view(0.5), 0.0);
        assert_eq!(half.sample(0.0).shapes.len(), 30);
    }

    #[test]
    fn radial_moves_one_child_out_and_reach_counts_the_ring() {
        let ring =
            Element::<()>::polar("ring", 100.0, 100.0, 60.0, 0.0, TAU).child(dot("a").radial(10.0)).child(dot("b"));
        // Children out to 60 + half the dot's diagonal, past a 100 box.
        let r = ring.effect_reach(0.0);
        assert!((r.left - (10.0 + 50f32.sqrt())).abs() < 1e-3, "{r:?}");
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(100.0, 100.0), 1.0);
        scene.apply(Element::column("root").size(100.0, 100.0).child(ring), 0.0);
        let id = scene.find(&["root", "ring", "a"]).unwrap();
        let p = scene.sampled(id, 0.0).unwrap().transform.apply(Point::new(5.0, 5.0));
        assert!((p.x - 120.0).abs() < 1e-3 && (p.y - 50.0).abs() < 1e-3, "{p:?}");
        // The scene's ink includes the placed children.
        let ink = scene.ink_bounds(0.0).unwrap();
        assert!(ink.right() >= 125.0 - 1e-3, "{ink:?}");
    }

    #[test]
    fn ticks_and_children_follow_a_path() {
        let line = Arc::new(Contour::polyline(&[Point::new(0.0, 50.0), Point::new(100.0, 50.0)], false));
        let rail = Element::<()>::along_path("rail", 100.0, 100.0, line, 0.0, 1.0)
            .ticks(Ticks::new(11, 8.0, 1.0, Color::WHITE))
            .child(dot("a"))
            .child(dot("b"));
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(100.0, 100.0), 1.0);
        scene.apply(Element::column("root").size(100.0, 100.0).child(rail), 0.0);
        let list = scene.sample(0.0);
        // 11 ticks and two dots.
        assert_eq!(list.shapes.len(), 13);
        let id = scene.find(&["root", "rail", "b"]).unwrap();
        let p = scene.sampled(id, 0.0).unwrap().transform.apply(Point::new(5.0, 5.0));
        assert!((p.x - 100.0).abs() < 1e-3 && (p.y - 50.0).abs() < 1e-3, "{p:?}");
    }

    #[test]
    fn a_presence_trim_draws_the_ticks_on_without_a_trim_of_its_own() {
        use crate::element::Transition;
        let ring = Element::<()>::polar("ring", 200.0, 200.0, 80.0, 0.0, TAU)
            .ticks(Ticks::new(60, 6.0, 1.0, Color::WHITE))
            .presence(Transition::NONE.trim(0.0, 0.0), Transition::NONE);
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(200.0, 200.0), 1.0);
        scene.apply(ring, 0.0);
        assert_eq!(scene.sample(0.0).shapes.len(), 0);
        assert_eq!(scene.sample(5.0).shapes.len(), 60);
    }
}
