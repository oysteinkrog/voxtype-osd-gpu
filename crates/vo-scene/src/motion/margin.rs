//! The margin mark (MO-32, "the line changes its address"): a short bar in the left margin
//! beside the newest line. When the text wraps, the mark splits: its top half stays beside
//! the line that is now the previous one and fades as that line moves on, and its bottom
//! half travels to the new line and grows back to full height. A host that keeps the newest
//! line on a fixed reading line (the popup's reading-line anchor) moves the text up, so on
//! screen the bottom half stays where the reader looks and the top half rides away.
//!
//! A line cap that drops the first line is not a wrap: the mark scrolls with its line.
//! Under reduced motion the mark moves to the new line at once and nothing splits.

use vo_core::channel::{Channel, Rest};
use vo_core::spring::Spring;
use vo_core::words::WordId;

use super::{Flow, FlowWord};
use crate::display::{DisplayList, Item, ItemKind, Paint, ShapeInstance};
use crate::geom::{Color, RRect, Rect};

/// The margin mark's look.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarginMark {
    pub color: Color,
    /// Bar width, and the gap from the bar to the text box's left edge.
    pub width: f32,
    pub gap: f32,
    /// Bar height as a share of the em size.
    pub height: f32,
    /// The bottom half's travel to the new line and its growth back to full height.
    pub travel: Spring,
    /// How long the top half takes to fade.
    pub fade: f64,
}

impl MarginMark {
    /// A 2 px bar 8 px left of the text, 90 % of the em tall, 280 ms travel, 450 ms fade.
    pub fn new(color: Color) -> MarginMark {
        MarginMark { color, width: 2.0, gap: 8.0, height: 0.9, travel: Spring::critical_to_98(0.28), fade: 0.45 }
    }
}

/// The top half left beside the previous line.
#[derive(Clone, Debug)]
struct Piece {
    /// A word on that line, which the piece follows, and the piece's centre from that
    /// word's line centre. `y` is the place to use once the word is gone.
    word: WordId,
    offset: f32,
    y: f32,
    opacity: Channel,
}

/// The margin mark's state on one live text node.
#[derive(Clone, Debug, Default)]
pub(crate) struct Margin {
    style: Option<MarginMark>,
    /// The first word of the newest line when last seen.
    first: Option<WordId>,
    /// The bar's centre from the newest line's centre, and its height as a share of full.
    dy: Option<Channel>,
    tall: Option<Channel>,
    pieces: Vec<Piece>,
}

/// The centre of word `w`'s line at `t`, in the text box's space.
fn centre(flow: &Flow, w: &FlowWord, t: f64) -> f32 {
    w.y.value(t) as f32 + w.roll.value(t) as f32 + flow.opened(w.line, t) - 0.28 * flow.size
}

impl Flow {
    /// Follows the newest line after an update at `t`, and splits the mark on a wrap.
    pub(crate) fn update_margin(&mut self, t: f64, style: Option<MarginMark>, reduced: bool) {
        let Some(style) = style else {
            self.margin = Margin::default();
            return;
        };
        self.margin.style = Some(style);
        let Some(last) = self.words.last() else {
            self.margin.first = None;
            return;
        };
        let line = last.line;
        let first = self.words.iter().find(|w| w.line == line).map_or(last.id, |w| w.id);
        let full = style.height * self.size;
        let unit = |v: f64| Channel::new(t, v, style.travel).with_rest(Rest::UNIT);
        match self.margin.first {
            Some(old) if old != first => {
                let prev = self.words.iter().find(|w| w.id == old && w.line < line);
                let new = self.words.iter().find(|w| w.id == first);
                if let (Some(prev), Some(new), false) = (prev, new, reduced) {
                    let (c_old, c_new) = (centre(self, prev, t), centre(self, new, t));
                    let lag = self.margin.dy.as_ref().map_or(0.0, |c| c.value(t) as f32);
                    let mut opacity = Channel::new(t, 1.0, Spring::critical_to_98(style.fade));
                    opacity.retarget(t, 0.0);
                    self.margin.pieces.push(Piece {
                        word: old,
                        offset: lag - 0.25 * full,
                        y: c_old + lag - 0.25 * full,
                        opacity: opacity.with_rest(Rest::UNIT),
                    });
                    let mut dy = Channel::new(t, (c_old + lag + 0.25 * full - c_new) as f64, style.travel)
                        .with_rest(Rest::PIXELS);
                    dy.retarget(t, 0.0);
                    let mut tall = unit(0.5);
                    tall.retarget(t, 1.0);
                    (self.margin.dy, self.margin.tall) = (Some(dy), Some(tall));
                } else {
                    (self.margin.dy, self.margin.tall) = (Some(unit(0.0)), Some(unit(1.0)));
                }
            }
            Some(_) => {}
            None => (self.margin.dy, self.margin.tall) = (Some(unit(0.0)), Some(unit(1.0))),
        }
        self.margin.first = Some(first);
        self.margin.pieces.retain(|p| p.opacity.settled_at() > t || p.opacity.target() > 0.0);
    }

    /// When the margin mark comes to rest.
    pub(crate) fn margin_settled_at(&self) -> f64 {
        let m = &self.margin;
        m.dy.iter()
            .chain(&m.tall)
            .chain(m.pieces.iter().map(|p| &p.opacity))
            .map(|c| c.settled_at())
            .fold(f64::NEG_INFINITY, f64::max)
    }

    /// Whether there is a margin mark to draw.
    pub(crate) fn has_margin(&self) -> bool {
        self.margin.style.is_some() && self.margin.first.is_some()
    }

    /// The margin mark's bars at `t` in the text box's space, with their opacity: the mark
    /// beside the newest line first, then the halves left beside earlier lines.
    pub(crate) fn margin_bars(&self, t: f64) -> Vec<(Rect, f32)> {
        let m = &self.margin;
        let (Some(style), Some(first)) = (m.style, m.first) else { return Vec::new() };
        let Some(w) = self.words.iter().find(|w| w.id == first) else { return Vec::new() };
        let full = style.height * self.size;
        let x = -style.gap - style.width;
        let bar = |c: f32, h: f32| Rect::new(x, c - 0.5 * h, style.width, h);
        let dy = m.dy.as_ref().map_or(0.0, |c| c.value(t) as f32);
        let tall = m.tall.as_ref().map_or(1.0, |c| c.value(t) as f32).clamp(0.0, 1.0);
        let mut out = vec![(bar(centre(self, w, t) + dy, full * tall), 1.0)];
        for p in &m.pieces {
            let k = p.opacity.value(t).clamp(0.0, 1.0) as f32;
            if k <= 0.0 {
                continue;
            }
            let c = self.words.iter().find(|w| w.id == p.word).map_or(p.y, |w| centre(self, w, t) + p.offset);
            out.push((bar(c, 0.5 * full), k));
        }
        out
    }
}

/// Draws the margin mark of `flow` at `t` in `color` faded by `alpha`, the text box at
/// `origin` with zoom `z`.
pub(crate) fn emit(
    flow: &Flow,
    t: f64,
    at: &super::overflow::Place,
    alpha: f32,
    items: &mut Vec<Item>,
    out: &mut DisplayList,
) {
    let Some(style) = flow.margin.style else { return };
    let z = at.zoom;
    for (r, k) in flow.margin_bars(t) {
        let r = Rect::new(at.origin.x + r.x * z, at.origin.y + r.y * z, r.w * z, r.h * z);
        crate::scene::push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
        out.shapes.push(ShapeInstance::rrect(RRect::new(r, 0.5 * r.w), Paint::solid(style.color.fade(alpha * k))));
    }
}
