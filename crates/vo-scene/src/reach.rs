//! Effect reach: how far an element draws outside its layout box.
//!
//! A glow, a shadow, a bloom, a stroke centred on the edge, particles and enter or exit
//! motion all put ink outside the box that layout gives an element. A surface that is only
//! as big as the panel cuts that ink off at its edge. This module computes the reach from
//! the same numbers the renderer uses, so a host can size the surface's transparent margin
//! from it instead of from a constant:
//!
//! - [`Element::effect_reach`]: from a view, before layout. Children count as if they sat
//!   inside their parent's box, which is what flex and block layout do.
//! - [`Scene::effect_reach`] and [`Scene::ink_bounds`]: from the retained scene after layout,
//!   with every node at its laid-out place.
//! - [`Scene::clipped`]: how far ink at `t` crosses a surface's edges, for tests and warnings.
//! - [`surface_for`]: the surface size and the panel's place in it for a content size and a
//!   reach.
//!
//! Each length is where the effect falls under one 8-bit level, matching the renderer:
//! a glow of radius `r` and strength `s` reaches `r ln(512 s)` (`shapes.wgsl`), a shadow its
//! offset plus three standard deviations, a bloom about three times its radius, text effects
//! as far as the glyph quad grows (`glyph_bounds` in `vo-render`).

use serde::Serialize;

use crate::element::{Element, Kind, Slot, Transition};
use crate::emitter::EmitterSource;
use crate::geom::{Point, Rect, Size};
use crate::scene::{NodeId, Scene};

/// Distances outside a box, one per side. Never negative.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Insets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Insets {
    pub const ZERO: Insets = Insets { left: 0.0, top: 0.0, right: 0.0, bottom: 0.0 };

    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Insets {
        Insets { left, top, right, bottom }
    }

    pub const fn uniform(v: f32) -> Insets {
        Insets { left: v, top: v, right: v, bottom: v }
    }

    /// The larger of the two on each side.
    pub fn max(self, o: Insets) -> Insets {
        Insets {
            left: self.left.max(o.left),
            top: self.top.max(o.top),
            right: self.right.max(o.right),
            bottom: self.bottom.max(o.bottom),
        }
    }

    /// Both added side by side: an effect that reaches `o` from a box already grown by `self`.
    pub fn plus(self, o: Insets) -> Insets {
        Insets {
            left: self.left + o.left,
            top: self.top + o.top,
            right: self.right + o.right,
            bottom: self.bottom + o.bottom,
        }
    }

    /// The largest side.
    pub fn largest(&self) -> f32 {
        self.left.max(self.top).max(self.right).max(self.bottom)
    }

    pub fn is_zero(&self) -> bool {
        self.largest() <= 0.0
    }

    /// Each side rounded up to a whole logical pixel.
    pub fn ceil(self) -> Insets {
        Insets { left: self.left.ceil(), top: self.top.ceil(), right: self.right.ceil(), bottom: self.bottom.ceil() }
    }

    /// `r` grown by these insets.
    pub fn expand(&self, r: Rect) -> Rect {
        Rect::new(r.x - self.left, r.y - self.top, r.w + self.left + self.right, r.h + self.top + self.bottom)
    }

    /// How far `inner` sticks out of `outer` on each side.
    pub fn outside(inner: Rect, outer: Rect) -> Insets {
        Insets {
            left: (outer.x - inner.x).max(0.0),
            top: (outer.y - inner.y).max(0.0),
            right: (inner.right() - outer.right()).max(0.0),
            bottom: (inner.bottom() - outer.bottom()).max(0.0),
        }
    }

    fn non_negative(self) -> Insets {
        self.max(Insets::ZERO)
    }
}

/// A surface fitted around content: its size, and where the content's top-left corner goes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct SurfaceFit {
    pub size: Size,
    pub origin: Point,
}

/// The surface for `content` with room for `reach` on every side, in whole logical pixels.
/// Put the panel at `origin`; everything outside it stays transparent and click-through.
pub fn surface_for(content: Size, reach: Insets) -> SurfaceFit {
    let r = reach.ceil();
    SurfaceFit {
        size: Size::new((content.w + r.left + r.right).ceil(), (content.h + r.top + r.bottom).ceil()),
        origin: Point::new(r.left, r.top),
    }
}

/// Glow of radius `r` at `strength`: out to where `strength e^(-d / r)` is under 1/512.
pub fn glow_reach(radius: f32, strength: f32) -> f32 {
    if radius <= 0.0 || strength <= 0.0 {
        return 0.0;
    }
    radius * (512.0 * strength).max(1.0).ln()
}

/// A bloom of `radius` logical units: its widest pyramid level fades under one 8-bit level
/// at about three radii.
pub fn bloom_reach(radius: f32) -> f32 {
    3.0 * radius.max(0.0)
}

/// A vector stroke's own glow (`StrokeStyle::glow`): out to where it is under 1/255.
pub fn stroke_glow_reach(width: f32, glow: f32) -> f32 {
    0.5 * width.max(0.0) + if glow > 0.0 { 2.5 * glow } else { 0.0 }
}

/// How far a particle can get from its emitter's origin, from its simulation and look.
pub fn particle_reach(sim: &crate::emitter::EmitterSim, look: &crate::emitter::EmitterLook) -> f32 {
    use crate::emitter::Shape;
    let shape = match sim.shape {
        Shape::Point => 0.0,
        Shape::Ring { radius, spread } => radius * (1.0 + 0.5 * spread.abs()),
        Shape::Disc { radius } => radius,
        Shape::Line { half_length, .. } => half_length,
        Shape::Box { half } => half[0].abs().max(half[1].abs()),
        Shape::Torus { major, minor } => major + minor,
        // The path's own extent is added by `EmitterSource::reach`.
        Shape::Path { sway, width, .. } => sway.abs() + width.abs(),
    };
    let life = sim.life.max(0.0) * (1.0 + sim.life_jitter.abs());
    // With drag `k` a speed `v` carries a particle at most `v / k`; without drag, `v life`.
    let coast = if sim.drag > 1e-3 { (1.0 / sim.drag).min(life) } else { life };
    let push =
        |accel: f32| accel.abs() * (0.5 * life * life).min(if sim.drag > 1e-3 { life / sim.drag } else { f32::MAX });
    let g = (sim.gravity[0] * sim.gravity[0] + sim.gravity[1] * sim.gravity[1]).sqrt();
    let shock = sim.shock.map_or(0.0, |s| push(s.strength).min(s.speed * life));
    let travel = sim.spawn_speed.abs() * coast + push(g) + push(sim.noise) + shock;
    let origin = (look.origin.x * look.origin.x + look.origin.y * look.origin.y).sqrt();
    // A ring pull holds particles near its circle once they get there.
    let held = if sim.ring_pull > 0.0 { sim.ring_radius + travel.min(shape.max(sim.ring_radius)) } else { f32::MAX };
    origin + (shape + travel).min(held) + 2.0 * look.size.max(0.0)
}

/// What the reach of one element or node is computed from.
pub(crate) struct ReachIn<'a> {
    /// A slot's values: the element's target, or a node's value and target. Empty when unset.
    pub get: &'a dyn Fn(Slot) -> Vec<f64>,
    pub kind: &'a Kind,
    pub inside: bool,
    pub hairline: bool,
    pub presence: Option<(Transition, Transition)>,
    /// The box size, when known.
    pub size: Option<Size>,
    pub t: f64,
}

/// How far one element draws outside its own box (children not included).
pub(crate) fn own_reach(r: &ReachIn) -> Insets {
    let one = |s: Slot| (r.get)(s).into_iter().map(f64::abs).fold(0.0f64, f64::max) as f32;
    let set = |s: Slot| !(r.get)(s).is_empty();
    // Effects around the box, and how far motion moves the box.
    let mut effects = Insets::ZERO;
    let mut motion = Insets::ZERO;
    let mut uniform = 0.0f32;
    // A stroke centred on the edge puts half its width outside.
    if set(Slot::Stroke) && !r.inside {
        uniform = uniform.max(if r.hairline { 1.0 } else { 0.5 * one(Slot::StrokeWidth) });
    }
    if set(Slot::GlowColor) {
        let strength = if set(Slot::GlowStrength) { one(Slot::GlowStrength) } else { 1.0 };
        uniform = uniform.max(glow_reach(one(Slot::GlowRadius), strength));
    }
    if set(Slot::ShadowColor) {
        let blur = 3.0 * one(Slot::ShadowBlur);
        for dx in (r.get)(Slot::ShadowX) {
            effects.left = effects.left.max(blur + (-dx as f32).max(0.0));
            effects.right = effects.right.max(blur + (dx as f32).max(0.0));
        }
        for dy in (r.get)(Slot::ShadowY) {
            effects.top = effects.top.max(blur + (-dy as f32).max(0.0));
            effects.bottom = effects.bottom.max(blur + (dy as f32).max(0.0));
        }
        uniform = uniform.max(blur);
    }
    if set(Slot::BloomRadius) {
        uniform = uniform.max(bloom_reach(one(Slot::BloomRadius)));
    }
    // Text effects grow each glyph's quad (`glyph_bounds` in vo-render).
    let text = 0.7 * one(Slot::TextBlur) + one(Slot::TextChroma) + 1.4 * one(Slot::TextHaloRadius);
    if text > 0.0 {
        uniform = uniform.max(text + 1.0);
    }
    uniform = uniform.max(one(Slot::StyleChroma)).max(0.5 * one(Slot::RimWidth));
    uniform = uniform.max(kind_reach(r.kind, r.size, r.t));
    // Motion: an offset moves the ink with it; spring overshoot adds about 5% of the move.
    let offset = |s: Slot| (r.get)(s).into_iter().map(|v| v as f32).collect::<Vec<_>>();
    let mut moves: Vec<(f32, f32, f32)> = Vec::new();
    for x in offset(Slot::OffsetX) {
        moves.push((x, 0.0, 1.0));
    }
    for y in offset(Slot::OffsetY) {
        moves.push((0.0, y, 1.0));
    }
    if let Some((enter, exit)) = r.presence {
        for tr in [enter, exit] {
            let (x, y) = tr.offset.unwrap_or((0.0, 0.0));
            moves.push((x, y, tr.scale.unwrap_or(1.0)));
        }
    }
    for s in (r.get)(Slot::Scale) {
        moves.push((0.0, 0.0, s as f32));
    }
    for (x, y, s) in moves {
        let k = 1.05;
        let grow = r.size.map_or(0.0, |z| 0.5 * (s - 1.0).max(0.0) * z.w.max(z.h)) * k;
        motion.left = motion.left.max(grow + (-x * k).max(0.0));
        motion.right = motion.right.max(grow + (x * k).max(0.0));
        motion.top = motion.top.max(grow + (-y * k).max(0.0));
        motion.bottom = motion.bottom.max(grow + (y * k).max(0.0));
    }
    // What the app declared for drawing the framework cannot see (canvas, GPU elements).
    let declared =
        Insets::new(one(Slot::ReachLeft), one(Slot::ReachTop), one(Slot::ReachRight), one(Slot::ReachBottom));
    // Effects sit on the moved box, so they add to the motion.
    motion.plus(effects.max(Insets::uniform(uniform))).max(declared).non_negative()
}

/// Reach that comes from what the element draws, around its box.
fn kind_reach(kind: &Kind, size: Option<Size>, t: f64) -> f32 {
    let half = size.map_or(0.0, |s| 0.5 * s.w.min(s.h));
    match kind {
        Kind::Emitter(e) => {
            let source: &dyn EmitterSource = &*e.source;
            (source.reach(t) - half).max(0.0)
        }
        Kind::StrokePath(a) => stroke_glow_reach(a.style.width, a.style.glow),
        Kind::Morph(a) => stroke_glow_reach(a.style.width, a.style.glow),
        Kind::WavyLine(s) => stroke_glow_reach(s.width, s.glow),
        Kind::Repeat(r) => crate::repeat::reach(r, size),
        _ => 0.0,
    }
}

impl<M> Element<M> {
    /// How far this element and its children may draw outside its box at `t`: glows,
    /// shadows, blooms, strokes, text effects, particles, and the offsets and scales of its
    /// motion. Children count as if they sat inside their parent's box. Use it to size a
    /// surface's transparent margin ([`surface_for`]).
    pub fn effect_reach(&self, t: f64) -> Insets {
        let get = |s: Slot| -> Vec<f64> {
            let first = s as u8;
            self.targets.iter().filter(|(k, _)| *k == first).map(|(_, v)| *v).collect()
        };
        let size = fixed_size(&self.style);
        let own = own_reach(&ReachIn {
            get: &get,
            kind: &self.kind,
            inside: self.inside,
            hairline: self.hairline,
            presence: self.presence,
            size,
            t,
        });
        self.children.iter().fold(own, |acc, c| {
            let mut r = c.effect_reach(t);
            // A repeater's children sit on its ring or path, which may leave its box.
            if let Kind::Repeat(rep) = &self.kind {
                r = r.plus(crate::repeat::child_overflow(rep, size, fixed_size(&c.style)));
            }
            acc.max(r)
        })
    }

    /// Declares ink outside the box that the framework cannot see, such as a canvas or a
    /// GPU element that draws past its edges. Counted by `effect_reach`.
    pub fn declare_reach(mut self, r: Insets) -> Element<M> {
        for (slot, v) in [
            (Slot::ReachLeft, r.left),
            (Slot::ReachTop, r.top),
            (Slot::ReachRight, r.right),
            (Slot::ReachBottom, r.bottom),
        ] {
            let slot = slot as u8;
            self.targets.retain(|(k, _)| *k != slot);
            self.targets.push((slot, v.max(0.0) as f64));
        }
        self
    }
}

impl Slot {
    /// The motion group of a slot from `ReachLeft` up.
    pub(crate) fn hud_prop(slot: u8) -> crate::element::Prop {
        use crate::element::Prop;
        match slot {
            s if s == Slot::ItemAt as u8 || s == Slot::ItemRadius as u8 => Prop::Position,
            s if (Slot::RepeatA as u8..=Slot::RepeatC as u8).contains(&s) => Prop::Ring,
            _ => Prop::Offset,
        }
    }

    /// Whether a slot from `ReachLeft` up settles at the fine threshold for 0 to 1 values.
    pub(crate) fn hud_unit(slot: u8) -> bool {
        // Angles and fractions along a path.
        slot == Slot::ItemAt as u8 || slot == Slot::RepeatB as u8 || slot == Slot::RepeatC as u8
    }
}

fn fixed_size(style: &taffy::Style) -> Option<Size> {
    let len = |d: taffy::Dimension| {
        let raw = d.into_raw();
        (raw.tag() == taffy::CompactLength::LENGTH_TAG).then(|| raw.value())
    };
    Some(Size::new(len(style.size.width)?, len(style.size.height)?))
}

impl<M> Scene<M> {
    /// Every node's box grown by its own reach, at `t`, in the root's parent space (where
    /// the root box sits at its layout position). `None` for an empty scene.
    pub fn ink_bounds(&self, t: f64) -> Option<Rect> {
        let root = self.root?;
        let mut acc: Option<Rect> = None;
        self.ink_walk(root, Point::ZERO, t, &mut acc);
        acc
    }

    /// How far the scene's ink at `t` reaches outside the root's box.
    pub fn effect_reach(&self, t: f64) -> Insets {
        let (Some(root), Some(ink)) = (self.root, self.ink_bounds(t)) else { return Insets::ZERO };
        Insets::outside(ink, self.node_box(root, Point::ZERO, t))
    }

    /// How far the ink at `t` crosses the edges of a surface of `size` with the root at its
    /// layout position. Zero when everything fits.
    pub fn clipped(&self, t: f64, size: Size) -> Insets {
        self.ink_bounds(t).map_or(Insets::ZERO, |ink| Insets::outside(ink, Rect::from_size(size)))
    }

    /// The node's box at `t` in its parent's space, offset by `origin`.
    fn node_box(&self, id: NodeId, origin: Point, t: f64) -> Rect {
        let n = &self.nodes[id];
        if let Some(r) = n.parent.and_then(|p| crate::repeat::placed_box(&self.nodes[p], n, t)) {
            return Rect::new(origin.x + r.x, origin.y + r.y, r.w, r.h);
        }
        let v = |s: Slot| n.value(s as u8, t) as f32;
        Rect::new(origin.x + v(Slot::X), origin.y + v(Slot::Y), v(Slot::W).max(0.0), v(Slot::H).max(0.0))
    }

    fn ink_walk(&self, id: NodeId, origin: Point, t: f64, acc: &mut Option<Rect>) {
        let n = &self.nodes[id];
        if t < n.born || n.leaving.is_some_and(|(_, until)| t >= until) {
            return;
        }
        let b = self.node_box(id, origin, t);
        let get = |s: Slot| -> Vec<f64> {
            let slot = s as u8;
            if !n.has(slot) {
                return Vec::new();
            }
            let ch = n.chans.iter().find(|(k, _)| *k == slot).map(|(_, c)| c.target());
            let mut v = vec![n.value(slot, t)];
            v.extend(ch);
            v
        };
        let reach = own_reach(&ReachIn {
            get: &get,
            kind: &n.kind,
            inside: n.inside,
            hairline: n.hairline,
            presence: n.presence,
            size: Some(Size::new(b.w, b.h)),
            t,
        });
        let ink = reach.expand(b);
        *acc = Some(match *acc {
            None => ink,
            Some(a) => union(a, ink),
        });
        // Children sit in this node's space; their offsets are in their own reach.
        let inner = Point::new(b.x, b.y);
        for &c in &n.children {
            self.ink_walk(c, inner, t, acc);
        }
    }
}

fn union(a: Rect, b: Rect) -> Rect {
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (x1, y1) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Color;

    #[test]
    fn glow_reach_matches_the_shader() {
        // The vertex stage grows the quad by r ln(512 s).
        assert!((glow_reach(10.0, 1.0) - 10.0 * 512f32.ln()).abs() < 1e-3);
        assert_eq!(glow_reach(10.0, 0.0), 0.0);
        assert_eq!(glow_reach(0.0, 1.0), 0.0);
        // A very weak glow never reaches past its edge.
        assert_eq!(glow_reach(10.0, 1.0 / 1024.0), 0.0);
    }

    #[test]
    fn element_reach_counts_each_effect() {
        let plain = Element::<()>::panel("p").size(100.0, 40.0).fill(Color::WHITE);
        assert_eq!(plain.effect_reach(0.0), Insets::ZERO);

        let glow = plain.clone().glow(Color::WHITE, 8.0, 1.0);
        assert!((glow.effect_reach(0.0).left - glow_reach(8.0, 1.0)).abs() < 1e-3);

        // A shadow down and to the right reaches further on those sides.
        let shadow = plain.clone().shadow(Color::BLACK, 6.0, 10.0, 4.0).effect_reach(0.0);
        assert_eq!(shadow, Insets::new(12.0, 12.0, 18.0, 22.0));

        // A centred stroke puts half its width outside; a border stays inside.
        assert_eq!(plain.clone().stroke(Color::WHITE, 4.0).effect_reach(0.0), Insets::uniform(2.0));
        assert_eq!(plain.clone().border(Color::WHITE, 4.0).effect_reach(0.0), Insets::ZERO);

        // A child's glow counts for its parent.
        let parent = Element::<()>::column("c").child(glow.clone());
        assert_eq!(parent.effect_reach(0.0), glow.effect_reach(0.0));
    }

    #[test]
    fn motion_counts_offsets_and_scale() {
        let e = Element::<()>::panel("p")
            .size(100.0, 40.0)
            .presence(Transition::fade().offset(0.0, -20.0).scale(1.2), Transition::fade());
        let r = e.effect_reach(0.0);
        // 10% growth of the longer side on each side, plus 5% spring overshoot.
        assert!((r.left - 10.5).abs() < 1e-3, "{r:?}");
        assert!((r.top - (10.5 + 21.0)).abs() < 1e-3, "{r:?}");
        let declared = Element::<()>::panel("p").declare_reach(Insets::new(1.0, 2.0, 3.0, 4.0));
        assert_eq!(declared.effect_reach(0.0), Insets::new(1.0, 2.0, 3.0, 4.0));
    }

    #[test]
    fn surface_fits_content_and_reach() {
        let fit = surface_for(Size::new(300.5, 100.0), Insets::new(10.2, 0.0, 10.2, 31.0));
        assert_eq!(fit.size, Size::new(323.0, 131.0));
        assert_eq!(fit.origin, Point::new(11.0, 0.0));
    }

    #[test]
    fn particles_reach_their_travel() {
        use crate::emitter::{EmitterLook, EmitterSim, Shape};
        let sim =
            EmitterSim { shape: Shape::Disc { radius: 10.0 }, spawn_speed: 50.0, drag: 2.0, ..Default::default() };
        let look = EmitterLook { size: 2.0, ..Default::default() };
        // 10 for the disc, 50 / 2 = 25 of coasting, 4 for the sprite.
        assert!((particle_reach(&sim, &look) - 39.0).abs() < 1e-3);
    }
}
