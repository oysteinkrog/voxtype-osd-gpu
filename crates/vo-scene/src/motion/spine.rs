//! The overflow spine (MO-34): a line that leaves from the front leaves a record. Where it
//! was, a strip of tiny blocks appears in the margin above the text, one per word and as
//! wide as the word. After a short hold the blocks slide together and close into one
//! history tick, in a row with the ticks of the lines that left before. The reader sees
//! how much they have said.
//!
//! The strip clears when the text does. Under reduced motion each tick appears at once.

use super::{Flow, FlowWord};
use crate::display::{DisplayList, Item, ItemKind, Paint, ShapeInstance};
use crate::geom::{Color, RRect, Rect};
use vo_core::words::WordId;

/// The spine's look and timing. Lengths are in the text box's units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spine {
    pub color: Color,
    /// The strip's centre line, from the top of the text box; negative is above it.
    pub y: f32,
    /// Height of a word block, and of a history tick.
    pub block: f32,
    pub tick: f32,
    /// Width of a history tick, and the space from one tick to the next.
    pub tick_width: f32,
    pub pitch: f32,
    /// How long the blocks show before they close up, and how long closing takes.
    pub hold: f64,
    pub close: f64,
}

impl Spine {
    /// 2 px blocks 5 px above the text that close after 500 ms, in 350 ms, into 2 x 7 px
    /// ticks 5 px apart.
    pub fn new(color: Color) -> Spine {
        Spine { color, y: -5.0, block: 2.0, tick: 7.0, tick_width: 2.0, pitch: 5.0, hold: 0.5, close: 0.35 }
    }

    fn done(&self) -> f64 {
        self.hold + self.close
    }
}

/// Seconds the blocks take to appear.
const APPEAR: f64 = 0.15;
/// Opacity of the word blocks against a tick's.
const BLOCK_INK: f32 = 0.6;

/// One line that left.
#[derive(Clone, Debug)]
struct Entry {
    start: f64,
    /// Each word as `(x, width)`.
    blocks: Vec<(f32, f32)>,
}

/// The spine of one live text node.
#[derive(Clone, Debug, Default)]
pub(crate) struct Spines {
    style: Option<Spine>,
    entries: Vec<Entry>,
}

fn ease(p: f64) -> f32 {
    let p = p.clamp(0.0, 1.0) as f32;
    p * p * (3.0 - 2.0 * p)
}

impl Flow {
    /// Records the words in `front`, which left from the front of `old` at `t`, as a new
    /// entry; clears the spine when the text is empty.
    pub(super) fn update_spine(
        &mut self,
        t: f64,
        style: Option<Spine>,
        old: &[FlowWord],
        front: &[WordId],
        empty: bool,
        reduced: bool,
    ) {
        let Some(style) = style else {
            self.spine = Spines::default();
            return;
        };
        self.spine.style = Some(style);
        if empty {
            self.spine.entries.clear();
            return;
        }
        let blocks: Vec<(f32, f32)> = old
            .iter()
            .filter(|o| front.contains(&o.id))
            .map(|o| (o.x.value(t) as f32, o.width.value(t).max(0.0) as f32))
            .collect();
        if !blocks.is_empty() {
            let start = if reduced { t - style.done() } else { t };
            self.spine.entries.push(Entry { start, blocks });
        }
    }

    /// When the spine comes to rest.
    pub(crate) fn spine_settled_at(&self) -> f64 {
        let done = self.spine.style.map_or(0.0, |s| s.done());
        self.spine.entries.iter().map(|e| e.start + done).fold(f64::NEG_INFINITY, f64::max)
    }

    /// Whether there is a spine to draw.
    pub(crate) fn has_spine(&self) -> bool {
        self.spine.style.is_some() && !self.spine.entries.is_empty()
    }

    /// The spine's bars at `t` in the text box's space, with their opacity.
    pub(crate) fn spine_bars(&self, t: f64) -> Vec<(Rect, f32)> {
        let Some(s) = self.spine.style else { return Vec::new() };
        let mut out = Vec::new();
        for (slot, e) in self.spine.entries.iter().enumerate() {
            let age = t - e.start;
            if age < 0.0 {
                continue;
            }
            let tick_x = slot as f32 * s.pitch;
            if age >= s.done() {
                out.push((Rect::new(tick_x, s.y - 0.5 * s.tick, s.tick_width, s.tick), 1.0));
                continue;
            }
            let shown = ease(age / APPEAR);
            let k = ease((age - s.hold) / s.close.max(1e-3));
            let h = s.block + (s.tick - s.block) * k;
            let ink = (BLOCK_INK + (1.0 - BLOCK_INK) * k) * shown;
            for &(x, w) in &e.blocks {
                let x = x + (tick_x - x) * k;
                let w = w + (s.tick_width - w) * k;
                out.push((Rect::new(x, s.y - 0.5 * h, w, h), ink));
            }
        }
        out
    }
}

/// Draws the spine of `flow` at `t`, faded by `alpha`.
pub(crate) fn emit(
    flow: &Flow,
    t: f64,
    at: &super::overflow::Place,
    alpha: f32,
    items: &mut Vec<Item>,
    out: &mut DisplayList,
) {
    let Some(style) = flow.spine.style else { return };
    let z = at.zoom;
    for (r, k) in flow.spine_bars(t) {
        let r = Rect::new(at.origin.x + r.x * z, at.origin.y + r.y * z, r.w * z, r.h * z);
        crate::scene::push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
        let radius = 0.5 * r.w.min(r.h);
        out.shapes.push(ShapeInstance::rrect(RRect::new(r, radius), Paint::solid(style.color.fade(alpha * k))));
    }
}
