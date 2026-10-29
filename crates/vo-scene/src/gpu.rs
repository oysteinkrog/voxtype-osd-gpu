//! GPU elements: content an app draws with its own GPU code, such as a 3D scene.
//!
//! `Element::gpu(key, source)` reserves a box. Each sample puts a [`GpuItem`] in the display
//! list with the box in its layer's space and the frame time. The renderer breaks its pass
//! there, hands the item to a painter that knows the source's type (`vo_render::GpuPainter`),
//! and resumes on top of what the painter drew. The scene knows nothing about the drawing; it
//! only keeps waking until `GpuSource::until`.
//!
//! Like a canvas, the drawing must be a pure function of the frame time, so a live session and
//! an export draw the same frames.
//!
//! **Split elements** (EG-20). `Element::gpu_split(key, source)` draws one source twice, cut
//! at the text plane: the half behind it ([`GpuPart::Behind`]) where the element sits, and
//! the half in front of it ([`GpuPart::Front`]) right after the live text, so 3D can pass
//! behind the text and then in front of it. The front half never covers the reading line: it
//! carries the newest line's word boxes ([`reading_line_boxes`]) as [`GpuItem::cut`], and the
//! painter leaves the surface as it was inside them. Without live text the front half comes
//! right after the back half.

use std::any::Any;
use std::fmt::Debug;
use std::sync::Arc;

use serde::Serialize;

use crate::display::{DisplayList, Item, ItemKind, NO_WORD};
use crate::geom::{Affine, Point, Rect};

/// What a GPU element draws. A painter finds its own sources with `as_any().downcast_ref`.
pub trait GpuSource: Debug + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    /// Scene time after which the drawing no longer changes. `f64::INFINITY` keeps the scene
    /// awake for as long as the element is shown.
    fn until(&self) -> f64;
}

/// A source handle: compared by identity, so a view that keeps the same `Arc` leaves the node
/// alone.
#[derive(Clone, Debug)]
pub struct GpuRef(pub Arc<dyn GpuSource>);

impl PartialEq for GpuRef {
    fn eq(&self, o: &GpuRef) -> bool {
        Arc::ptr_eq(&self.0, &o.0)
    }
}

/// One GPU element in a frame.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GpuItem {
    #[serde(skip)]
    pub source: GpuRef,
    /// The element's box in its layer's space.
    pub rect: Rect,
    /// The time this frame shows.
    pub t: f64,
    /// Inherited opacity, which the painter multiplies in.
    pub opacity: f32,
    /// Which part of the drawing this item is: all of it, or one half of a split element.
    pub part: GpuPart,
    /// For a front half: boxes in the item's layer space where the painter must leave the
    /// surface as it found it (the reading line). Empty otherwise.
    #[serde(skip)]
    pub cut: Vec<Rect>,
}

/// Which part of a GPU element's drawing an item is (EG-20). The text plane is the plane
/// through the 3D camera's target that faces the camera: z = 0 on the stage camera.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum GpuPart {
    /// The whole drawing, at the element's place.
    #[default]
    Whole,
    /// Only what lies behind the text plane, at the element's place.
    Behind,
    /// Only what lies in front of the text plane, drawn after the live text.
    Front,
}

/// How far the front half keeps away from the reading line's word boxes, in logical pixels.
pub const READING_PAD: f32 = 4.0;

/// The newest line of live text as one box per word, in surface logical units, grown by
/// `pad` on each side. The newest word is the live-text glyph with the highest word id; its
/// row is every live-text glyph within half an em of that word's median baseline (the same
/// row the envelope check and the kept reading line of EG-03 use).
///
/// The display list has no glyph ink bounds, so a word's box is generous: from a fifth of an
/// em left of its first pen position to an em past its last, and from 1.1 em above the
/// baseline to 0.4 em below it.
pub fn reading_line_boxes(list: &DisplayList, pad: f32) -> Vec<Rect> {
    // Each live-text glyph in surface space: word, pen x, baseline y, em.
    let mut glyphs: Vec<(u32, f32, f32, f32)> = Vec::new();
    for layer in &list.layers {
        let m = layer.transform;
        let k = (m.a * m.d - m.b * m.c).abs().sqrt();
        for item in &list.items[layer.items.start as usize..layer.items.end as usize] {
            if item.kind != ItemKind::Glyphs {
                continue;
            }
            for g in &list.glyphs[item.start as usize..(item.start + item.len) as usize] {
                if g.word != NO_WORD {
                    let p = m.apply(g.pos);
                    glyphs.push((g.word, p.x, p.y, g.size * k));
                }
            }
        }
    }
    let Some(newest) = glyphs.iter().map(|g| g.0).max() else { return Vec::new() };
    let mut ys: Vec<f32> = glyphs.iter().filter(|g| g.0 == newest).map(|g| g.2).collect();
    ys.sort_by(f32::total_cmp);
    let y = ys[ys.len() / 2];
    let em = glyphs.iter().filter(|g| g.0 == newest).map(|g| g.3).fold(0.0, f32::max);
    // Per word on the row: x0, x1, y0, y1.
    let mut words: Vec<(u32, [f32; 4])> = Vec::new();
    for g in glyphs.iter().filter(|g| (g.2 - y).abs() < 0.5 * em.max(g.3)) {
        let b = [g.1 - 0.2 * g.3, g.1 + g.3, g.2 - 1.1 * g.3, g.2 + 0.4 * g.3];
        match words.iter_mut().find(|w| w.0 == g.0) {
            Some((_, w)) => *w = [w[0].min(b[0]), w[1].max(b[1]), w[2].min(b[2]), w[3].max(b[3])],
            None => words.push((g.0, b)),
        }
    }
    words.sort_by_key(|w| w.0);
    words
        .into_iter()
        .map(|(_, [x0, x1, y0, y1])| Rect::new(x0 - pad, y0 - pad, x1 - x0 + 2.0 * pad, y1 - y0 + 2.0 * pad))
        .collect()
}

/// The bounding box of `r` mapped by `m`.
fn map_rect(m: &Affine, r: Rect) -> Rect {
    let ps = [(r.x, r.y), (r.right(), r.y), (r.x, r.bottom()), (r.right(), r.bottom())]
        .map(|(x, y)| m.apply(Point::new(x, y)));
    let (mut x0, mut y0, mut x1, mut y1) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    for p in ps {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

fn chain_opacity(list: &DisplayList, layer: usize) -> f32 {
    let mut o = 1.0;
    let mut at = Some(layer as u32);
    while let Some(i) = at {
        o *= list.layers[i as usize].opacity;
        at = list.layers[i as usize].parent;
    }
    o
}

/// Puts `item` into layer `layer`'s items at `pos`, shifting every range after it.
fn insert_item(list: &mut DisplayList, layer: usize, pos: u32, item: Item) {
    list.items.insert(pos as usize, item);
    for (i, l) in list.layers.iter_mut().enumerate() {
        if i == layer {
            l.items.end += 1;
        } else if l.items.start >= pos {
            l.items.start += 1;
            l.items.end += 1;
        }
    }
}

/// Adds the front half of each split element (EG-20): after the live text that holds the
/// newest word, or right after its back half without live text, with the reading line's
/// boxes as its cut. Called once the list is complete.
pub(crate) fn place_front_halves(list: &mut DisplayList) {
    // Back halves: (layer, position after their item run, gpu index).
    let mut backs: Vec<(usize, u32, usize)> = Vec::new();
    for (li, l) in list.layers.iter().enumerate() {
        for ii in l.items.start..l.items.end {
            let it = list.items[ii as usize];
            if it.kind != ItemKind::Gpu {
                continue;
            }
            for gi in it.start..it.start + it.len {
                if list.gpu[gi as usize].part == GpuPart::Behind {
                    backs.push((li, ii + 1, gi as usize));
                }
            }
        }
    }
    if backs.is_empty() {
        return;
    }
    // The live text: the last run of live-text glyphs in the layer that holds the newest word.
    let runs = |l: &crate::display::Layer| {
        (l.items.start..l.items.end).filter_map(|ii| {
            let it = list.items[ii as usize];
            if it.kind != ItemKind::Glyphs {
                return None;
            }
            let glyphs = &list.glyphs[it.start as usize..(it.start + it.len) as usize];
            glyphs.iter().filter(|g| g.word != NO_WORD).map(|g| g.word).max().map(|w| (ii, w))
        })
    };
    let mut newest: Option<(u32, usize)> = None;
    for (li, l) in list.layers.iter().enumerate() {
        if let Some(w) = runs(l).map(|r| r.1).max() {
            if newest.is_none_or(|(n, _)| w > n) {
                newest = Some((w, li));
            }
        }
    }
    let newest = newest.and_then(|(_, li)| runs(&list.layers[li]).last().map(|(ii, _)| (li, ii + 1)));
    let surface_boxes = reading_line_boxes(list, READING_PAD);
    // Insert from the back so earlier positions stay valid.
    let mut fronts: Vec<(usize, u32, GpuItem)> = Vec::new();
    for &(src, after, gi) in &backs {
        let (dst, mut pos) = match newest {
            Some(at) => at,
            None => (src, after),
        };
        if dst == src && pos < after {
            pos = after;
        }
        let back = &list.gpu[gi];
        let to_surface = list.layers[src].transform;
        let from_surface = list.layers[dst].transform.invert().unwrap_or(Affine::IDENTITY);
        let rect = map_rect(&from_surface, map_rect(&to_surface, back.rect));
        let dst_o = chain_opacity(list, dst);
        let opacity = if dst_o > 0.0 { back.opacity * chain_opacity(list, src) / dst_o } else { 0.0 };
        let cut = surface_boxes.iter().map(|b| map_rect(&from_surface, *b)).collect();
        let front = GpuItem { source: back.source.clone(), rect, t: back.t, opacity, part: GpuPart::Front, cut };
        fronts.push((dst, pos, front));
    }
    fronts.sort_by_key(|f| std::cmp::Reverse(f.1));
    for (dst, pos, front) in fronts {
        list.gpu.push(front);
        let item = Item { kind: ItemKind::Gpu, start: list.gpu.len() as u32 - 1, len: 1 };
        insert_item(list, dst, pos, item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::Element;
    use crate::geom::Size;
    use crate::Scene;
    use vo_core::words::Word;

    #[derive(Debug)]
    struct Dummy;

    impl GpuSource for Dummy {
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn until(&self) -> f64 {
            0.0
        }
    }

    fn words(n: usize) -> Vec<Word> {
        let text = ["rings", "pass", "behind", "the", "words", "and", "then", "in", "front", "of", "older", "lines"];
        (0..n).map(|i| Word { id: i as u32 + 1, text: text[i % text.len()].into() }).collect()
    }

    /// A split element in a box, then live text `n` words long that wraps at 200 units.
    fn list(n: usize, split: bool) -> DisplayList {
        let src: Arc<dyn GpuSource> = Arc::new(Dummy);
        let el = if split { Element::gpu_split("ring", src) } else { Element::gpu("ring", src) };
        let view = Element::column("root")
            .size(300.0, 200.0)
            .child(el.size(300.0, 60.0))
            .child(Element::live_text("live", &words(n)).width(200.0));
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(300.0, 200.0), 1.0);
        scene.apply(view, 0.0);
        scene.sample(5.0)
    }

    /// Live-text glyphs as (word, surface x, surface baseline y).
    fn glyphs(list: &DisplayList) -> Vec<(u32, f32, f32)> {
        let mut out = Vec::new();
        for l in &list.layers {
            for it in &list.items[l.items.start as usize..l.items.end as usize] {
                if it.kind == ItemKind::Glyphs {
                    for g in &list.glyphs[it.start as usize..(it.start + it.len) as usize] {
                        if g.word != NO_WORD {
                            let p = l.transform.apply(g.pos);
                            out.push((g.word, p.x, p.y));
                        }
                    }
                }
            }
        }
        out
    }

    #[test]
    fn reading_line_boxes_are_the_newest_rows_words() {
        let list = list(12, false);
        let g = glyphs(&list);
        let newest = g.iter().map(|g| g.0).max().unwrap();
        let row_y = g.iter().find(|g| g.0 == newest).unwrap().2;
        let rows: std::collections::BTreeSet<i32> = g.iter().map(|g| g.2.round() as i32).collect();
        assert!(rows.len() >= 2, "the text wraps: rows {rows:?}");
        let on_row: std::collections::BTreeSet<u32> =
            g.iter().filter(|g| (g.2 - row_y).abs() < 1.0).map(|g| g.0).collect();
        let boxes = reading_line_boxes(&list, READING_PAD);
        assert_eq!(boxes.len(), on_row.len(), "one box per word on the newest row");
        // Every glyph of the row is inside a box, with the pad round its pen and baseline.
        for &(w, x, y) in g.iter().filter(|g| on_row.contains(&g.0)) {
            let inside = boxes.iter().any(|b| {
                b.x <= x - READING_PAD && b.right() >= x + READING_PAD && b.y <= y - 10.0 && b.bottom() >= y + 4.0
            });
            assert!(inside, "word {w} glyph at {x},{y} outside {boxes:?}");
        }
        // No box reaches an older row's letters (half an x height above its baseline); the
        // boxes are generous, so they may touch its descenders.
        for &(_, x, y) in g.iter().filter(|g| (g.2 - row_y).abs() > 1.0) {
            assert!(!boxes.iter().any(|b| b.contains(Point::new(x + 1.0, y - 5.0))), "older glyph {x},{y} in a box");
        }
        // Boxes go left to right in word order.
        assert!(boxes.windows(2).all(|w| w[0].x < w[1].x), "{boxes:?}");
        assert!(reading_line_boxes(&DisplayList::default(), READING_PAD).is_empty());
    }

    /// Indices into `list.items`, in paint order of the root layer, of the GPU items and of
    /// the last live-text glyph run.
    fn order(list: &DisplayList) -> (Vec<(u32, GpuPart)>, Option<u32>) {
        let l = &list.layers[0];
        let mut gpu = Vec::new();
        let mut text = None;
        for ii in l.items.clone() {
            let it = list.items[ii as usize];
            match it.kind {
                ItemKind::Gpu => gpu.extend((it.start..it.start + it.len).map(|g| (ii, list.gpu[g as usize].part))),
                ItemKind::Glyphs => text = Some(ii),
                _ => {}
            }
        }
        (gpu, text)
    }

    #[test]
    fn split_element_puts_its_front_half_after_the_live_text() {
        let whole = list(5, false);
        assert_eq!(order(&whole).0.iter().map(|g| g.1).collect::<Vec<_>>(), vec![GpuPart::Whole]);
        let split = list(5, true);
        let (gpu, text) = order(&split);
        let text = text.expect("live text");
        assert_eq!(gpu.len(), 2, "{gpu:?}");
        assert_eq!(gpu[0].1, GpuPart::Behind);
        assert!(gpu[0].0 < text, "back half under the text");
        assert_eq!(gpu[1], (text + 1, GpuPart::Front), "front half right after the text");
        let front = split.gpu.iter().find(|g| g.part == GpuPart::Front).unwrap();
        let back = split.gpu.iter().find(|g| g.part == GpuPart::Behind).unwrap();
        assert_eq!((front.rect, front.t, front.opacity), (back.rect, back.t, back.opacity));
        assert_eq!(front.cut, reading_line_boxes(&split, READING_PAD));
        assert!(!front.cut.is_empty());
        // Everything else is the list without the split.
        assert_eq!(split.glyphs, whole.glyphs);
        assert_eq!(split.items.len(), whole.items.len() + 1);

        // Without live text the front half follows the back half.
        let src: Arc<dyn GpuSource> = Arc::new(Dummy);
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(300.0, 200.0), 1.0);
        scene.apply(Element::column("root").child(Element::gpu_split("ring", src).size(100.0, 50.0)), 0.0);
        let list = scene.sample(1.0);
        let (gpu, _) = order(&list);
        assert_eq!(gpu.iter().map(|g| g.1).collect::<Vec<_>>(), vec![GpuPart::Behind, GpuPart::Front]);
        assert!(list.gpu.iter().all(|g| g.cut.is_empty()));
    }
}
