//! A container that morphs between a dot, a pill and a card (`container::morph`): a rounded
//! rect whose size and corner radius spring together with a slight overshoot, about 320 ms,
//! and whose content is clipped to the shape while it moves.
//!
//! The content keeps its own size and stays centred, so a growing container uncovers it from
//! the middle and a shrinking one covers it again; nothing reflows during the morph. Give the
//! content the size it has in the largest shape.
//!
//! ```ignore
//! let shape = if open { Shape::Card { w: 320.0, h: 120.0, radius: 18.0 } } else { Shape::Dot(14.0) };
//! container::morph("popup", shape, Color::hex(0x1c1f26, 1.0), content)
//! ```

use taffy::prelude::length;
use taffy::style::{AlignItems, JustifyContent, Position};
use vo_core::spring::Spring;

use crate::element::{Element, Key, Prop};
use crate::geom::Color;

/// The container spring: the overshoot peaks after about 260 ms at about 1.5%, and it has
/// settled by about 320 ms.
pub const MORPH: Spring = Spring::from_frequency(20.0, 0.8);

/// What the container looks like.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    /// A circle this wide.
    Dot(f32),
    /// Fully rounded ends.
    Pill { w: f32, h: f32 },
    /// A rounded rect.
    Card { w: f32, h: f32, radius: f32 },
}

impl Shape {
    /// Width, height and corner radius.
    pub fn size_radius(self) -> (f32, f32, f32) {
        match self {
            Shape::Dot(d) => (d, d, 0.5 * d),
            Shape::Pill { w, h } => (w, h, 0.5 * h.min(w)),
            Shape::Card { w, h, radius } => (w, h, radius.min(0.5 * w.min(h))),
        }
    }
}

/// A container in `shape`, filled with `fill`, holding `content` centred and clipped to the
/// shape. Changing `shape` between views morphs it on [`MORPH`]. A dot hides the content: it
/// fades out as the container closes to a dot and back in as it opens.
pub fn morph<M>(key: impl Into<Key>, shape: Shape, fill: Color, content: Element<M>) -> Element<M> {
    morph_with(key, shape, fill, MORPH, content)
}

/// [`morph`] on another spring.
pub fn morph_with<M>(
    key: impl Into<Key>,
    shape: Shape,
    fill: Color,
    spring: Spring,
    content: Element<M>,
) -> Element<M> {
    let (w, h, r) = shape.size_radius();
    // The clip layer has the container's size and sits at its top left, out of the flow
    // (absolute), so the content cannot hold it open. Its size and radius move on the same
    // spring, so the clip and the background stay one shape.
    let clip = Element::layer("clip")
        .clip()
        .size(w, h)
        .radius(r)
        .layout(|s| {
            s.position = Position::Absolute;
            s.inset.left = length(0.0);
            s.inset.top = length(0.0);
        })
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
        .motion(Prop::Size, spring)
        .motion(Prop::Position, spring)
        .motion(Prop::Radius, spring)
        .child(
            content
                .shrink(0.0)
                .opacity(if matches!(shape, Shape::Dot(_)) { 0.0 } else { 1.0 })
                .motion(Prop::Position, spring)
                .motion(Prop::Opacity, Spring::SNAP),
        );
    Element::panel(key)
        .size(w, h)
        .radius(r)
        .fill(fill)
        .motion(Prop::Size, spring)
        .motion(Prop::Radius, spring)
        .child(clip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Size;
    use crate::scene::Scene;

    fn view(shape: Shape) -> Element<()> {
        let content = Element::shape("bar", crate::display::ShapeKind::RRect).size(200.0, 40.0).fill(Color::WHITE);
        Element::column("root").pad(20.0).child(morph("c", shape, Color::hex(0x202020, 1.0), content))
    }

    #[test]
    fn the_spring_settles_in_about_320_ms_with_a_small_overshoot() {
        let settle = MORPH.settle_time(-1.0, 0.0, 0.02, 1.0);
        assert!((0.25..0.40).contains(&settle), "{settle}");
        let peak = (1..200).map(|i| MORPH.eval(-1.0, 0.0, i as f64 * 0.005).0).fold(f64::MIN, f64::max);
        assert!((0.01..0.06).contains(&peak), "overshoot {peak}");
    }

    #[test]
    fn content_is_clipped_to_the_moving_shape() {
        let mut scene: Scene = Scene::new();
        scene.set_viewport(0.0, Size::new(400.0, 200.0), 1.0);
        scene.apply(view(Shape::Dot(16.0)), 0.0);
        scene.apply(view(Shape::Card { w: 240.0, h: 80.0, radius: 16.0 }), 1.0);
        let clip_of = |t: f64| {
            let list = scene.sample(t);
            list.layers.iter().find_map(|l| l.clip).expect("a clipped layer")
        };
        let (start, mid, end) = (clip_of(1.0), clip_of(1.1), clip_of(2.0));
        assert!((start.rect.w - 16.0).abs() < 0.5 && (start.radii[0] - 8.0).abs() < 0.5, "{start:?}");
        assert!(mid.rect.w > 40.0 && mid.rect.w < 250.0, "{mid:?}");
        assert!(
            (end.rect.w - 240.0).abs() < 0.5 && (end.rect.h - 80.0).abs() < 0.5 && (end.radii[0] - 16.0).abs() < 0.5,
            "{end:?}"
        );
        // The overshoot: at some point the width passes 240.
        assert!((0..40).any(|i| clip_of(1.0 + i as f64 * 0.01).rect.w > 241.0));
        assert!(scene.next_wake(2.0).is_none());
    }
}
