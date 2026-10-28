//! Overflow: how the top line of live text leaves when a line cap drops it from the front.
//!
//! By default the dropped words scroll up with the block and fade (`Overflow::Scroll`).
//! The other forms keep the line where it is and take it away there, while the lines below
//! move up into its place:
//!
//! - **Fold under (MO-33).** `Overflow::Fold` turns the line away about its own baseline,
//!   0 to 80 degrees in 220 ms, through the stage camera (`crate::stage`), like a page
//!   folding under. Its shadow brushes the line below for about 100 ms in the middle.
//! - **Thin (MO-33, flat).** `Overflow::Thin` squeezes the line toward its baseline until
//!   it is gone, for looks with no depth.
//!
//! The leaving line is never the reading line, so the reading-line envelope does not bind
//! it. Under reduced motion every form is a plain fade where the line is.

use vo_core::channel::{Channel, Rest};

use crate::display::{DisplayList, Item, ItemKind, Layer};
use crate::font::FontId;
use crate::geom::{Affine, Color, Point, RRect, Rect, Size};
use crate::stage::Stage;

/// How the top line leaves when the text overflows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Overflow {
    /// It scrolls up with the rest and fades.
    #[default]
    Scroll,
    /// It turns away about its baseline (MO-33).
    Fold(Fold),
    /// It thins to nothing toward its baseline in `duration` seconds (MO-33, flat).
    Thin { duration: f64 },
}

impl Overflow {
    /// The fold from the motion plan: 80 degrees in 220 ms, shadow at 45 %.
    pub const FOLD: Overflow = Overflow::Fold(Fold::DEFAULT);
    /// The flat form: the line thins to nothing in 220 ms.
    pub const THIN: Overflow = Overflow::Thin { duration: 0.22 };

    fn duration(&self) -> f64 {
        match self {
            Overflow::Scroll => 0.0,
            Overflow::Fold(f) => f.duration,
            Overflow::Thin { duration } => *duration,
        }
    }
}

/// A fold under: the line turns away about its baseline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fold {
    /// Final angle in radians; positive turns the top of the line away.
    pub angle: f32,
    /// Seconds from flat to the final angle.
    pub duration: f64,
    /// The shadow on the line below at its strongest; alpha is its strength. Clear for none.
    pub shadow: Color,
}

impl Fold {
    pub const DEFAULT: Fold =
        Fold { angle: 80.0 * std::f32::consts::PI / 180.0, duration: 0.22, shadow: Color::rgba(0.0, 0.0, 0.0, 0.45) };
}

/// On a front drop, a word on the newest line waits this long, while the line before
/// scrolls out of its row, then shows whole in one frame: the reading line never moves and
/// text never crosses text.
pub const DROP_HOLD: f64 = 0.12;
pub const DROP_STEP: vo_core::spring::Spring = vo_core::spring::Spring::critical_to_98(0.02);

/// Share of the duration after which the line fades out, as it nears edge-on.
const FADE_FROM: f32 = 0.55;
/// The shadow brushes the line below between these shares of a fold (about 100 ms of 220).
const SHADOW_SPAN: (f32, f32) = (0.25, 0.7);

/// One glyph of a leaving line, as it was drawn when the line left.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LeavingGlyph {
    pub word: u32,
    pub glyph: u32,
    pub font: FontId,
    /// Pen position on the baseline, in the text box's space.
    pub pos: Point,
    /// Size factor, and opacity then.
    pub scale: f32,
    pub opacity: f32,
}

/// A line on its way out.
#[derive(Clone, Debug)]
pub(crate) struct Leaving {
    style: Overflow,
    start: f64,
    /// The flow line it was on, its baseline and its extent, in the text box's space.
    line: u32,
    baseline: f32,
    /// It rises with the lines below as they scroll up, so they never cross it.
    rise: Channel,
    x0: f32,
    x1: f32,
    /// Em size and line height of the text.
    size: f32,
    line_height: f32,
    glyphs: Vec<LeavingGlyph>,
}

impl Leaving {
    /// A line that leaves at `t` in `style` while the lines below scroll up by `dy`.
    pub(crate) fn new(style: Overflow, t: f64, line: u32, dy: f32, size: f32, line_height: f32) -> Leaving {
        Leaving {
            style,
            start: t,
            line,
            baseline: f32::NAN,
            rise: super::from_to(t, 0.0, -dy, super::SCROLL, Rest::PIXELS, 0.0),
            x0: f32::INFINITY,
            x1: f32::NEG_INFINITY,
            size,
            line_height,
            glyphs: Vec::new(),
        }
    }

    /// Whether `self` is the line `line` that left at `t`.
    pub(crate) fn is(&self, t: f64, line: u32) -> bool {
        self.start == t && self.line == line
    }

    /// Adds a glyph; `advance` widens the line's extent.
    pub(crate) fn push(&mut self, g: LeavingGlyph, advance: f32) {
        if g.opacity <= 0.0 {
            return;
        }
        if self.baseline.is_nan() {
            self.baseline = g.pos.y;
        }
        self.x0 = self.x0.min(g.pos.x);
        self.x1 = self.x1.max(g.pos.x + advance * g.scale);
        self.glyphs.push(g);
    }

    /// When the line is gone.
    pub(crate) fn done_at(&self) -> f64 {
        self.start + self.style.duration()
    }

    /// How far along, 0 to 1.
    fn progress(&self, t: f64) -> f32 {
        let d = self.style.duration().max(1e-3);
        ((t - self.start) / d).clamp(0.0, 1.0) as f32
    }

    /// The line's opacity at `t`: whole, then fading out over the end.
    fn opacity(&self, t: f64) -> f32 {
        let p = self.progress(t);
        if p >= 1.0 {
            return 0.0;
        }
        1.0 - ((p - FADE_FROM) / (1.0 - FADE_FROM)).clamp(0.0, 1.0)
    }

    /// The fold angle at `t`, easing out.
    fn angle(&self, t: f64) -> f32 {
        match self.style {
            Overflow::Fold(f) => f.angle * ease_out(self.progress(t)),
            _ => 0.0,
        }
    }

    /// How tall the line still is at `t`, 1 to 0, for `Overflow::Thin`.
    fn thickness(&self, t: f64) -> f32 {
        match self.style {
            Overflow::Thin { .. } => 1.0 - ease_out(self.progress(t)),
            _ => 1.0,
        }
    }

    /// The shadow's strength on the line below at `t`, 0 to 1.
    fn shadow(&self, t: f64) -> f32 {
        let p = self.progress(t);
        let (a, b) = SHADOW_SPAN;
        if !(a..b).contains(&p) {
            return 0.0;
        }
        (std::f32::consts::PI * (p - a) / (b - a)).sin()
    }
}

fn ease_out(p: f32) -> f32 {
    1.0 - (1.0 - p) * (1.0 - p)
}

impl super::Flow {
    /// Whether the node draws anything beside its words: a leaving line (MO-33) or a margin
    /// mark (MO-32).
    pub(crate) fn has_extras(&self) -> bool {
        !self.leaving.is_empty() || self.has_margin() || self.has_spine()
    }
}

/// Where a node's leaving lines are drawn: the node's place in its layer and its text.
pub(crate) struct Place {
    /// The text box's origin in the current layer, and the text zoom.
    pub origin: Point,
    pub zoom: f32,
    pub color: Color,
    /// The current layer.
    pub layer: u32,
}

/// Draws `leaving` at `t`, under the live text: each line in a layer of its own, turned or
/// squeezed about its baseline, so the lines moving up pass over it.
pub(crate) fn emit(
    leaving: &[Leaving],
    t: f64,
    at: &Place,
    stage: &Stage,
    viewport: Size,
    items: &mut Vec<Item>,
    out: &mut DisplayList,
) {
    let z = at.zoom;
    let map = |p: Point| Point::new(at.origin.x + p.x * z, at.origin.y + p.y * z);
    for l in leaving {
        let alpha = l.opacity(t);
        if alpha <= 0.0 || l.glyphs.is_empty() {
            continue;
        }
        let lift = l.rise.value(t) as f32;
        let base = map(Point::new(l.x0, l.baseline + lift));
        let (w, half) = ((l.x1 - l.x0) * z, l.size * z);
        let parent = &out.layers[at.layer as usize];
        let (transform, warp) = match l.style {
            Overflow::Fold(_) => {
                // A box whose centre line is the baseline, so the turn is about it.
                let rect = Rect::new(base.x, base.y - half, w, 2.0 * half);
                let seen = bounds(&parent.transform, rect);
                let own = stage.tilt(viewport, seen, l.angle(t), 0.0, 0.0);
                let warp = match (own, parent.warp) {
                    (Some(o), Some(p)) => Some(o.then(&p)),
                    (o, p) => o.or(p),
                };
                (parent.transform, warp)
            }
            _ => {
                let k = l.thickness(t).max(0.02);
                let squeeze = Affine { a: 1.0, b: 0.0, c: 0.0, d: k, e: 0.0, f: base.y * (1.0 - k) };
                (squeeze.then(&parent.transform), parent.warp)
            }
        };
        let index = out.layers.len() as u32;
        items.push(Item { kind: ItemKind::Layer, start: index, len: 1 });
        out.layers.push(Layer { parent: Some(at.layer), transform, warp, ..Layer::root() });
        let mut own = Vec::new();
        for g in &l.glyphs {
            crate::scene::push_item(&mut own, ItemKind::Glyphs, out.glyphs.len() as u32);
            out.glyphs.push(crate::display::GlyphInstance {
                pos: map(Point::new(g.pos.x, g.pos.y + lift)),
                size: l.size * g.scale * z,
                font: g.font,
                glyph: g.glyph,
                word: g.word,
                color: at.color.fade(alpha * g.opacity),
            });
        }
        out.layers[index as usize].items = crate::scene::close_items(&mut own, out);
    }
}

/// A fold's shadow at `t`, over the live text: a soft band from the leaving line's baseline
/// down over the line below.
pub(crate) fn emit_shadows(leaving: &[Leaving], t: f64, at: &Place, items: &mut Vec<Item>, out: &mut DisplayList) {
    let z = at.zoom;
    for l in leaving {
        let (Overflow::Fold(f), s) = (l.style, l.shadow(t)) else { continue };
        if s <= 0.0 || f.shadow.a <= 0.0 || l.glyphs.is_empty() {
            continue;
        }
        let half = l.size * z;
        let top = at.origin.y + (l.baseline + l.rise.value(t) as f32) * z + 0.2 * half;
        let r = Rect::new(at.origin.x + l.x0 * z, top, (l.x1 - l.x0) * z, l.line_height * z);
        let c = f.shadow.fade(s);
        let paint = crate::display::Paint::linear(Point::new(r.x, r.y), c, Point::new(r.x, r.y + r.h), c.fade(0.0));
        crate::scene::push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
        out.shapes.push(crate::display::ShapeInstance::rrect(RRect::new(r, 0.0), paint));
    }
}

/// `rect` through `m`, as the box around its corners.
fn bounds(m: &Affine, rect: Rect) -> Rect {
    let pts = [
        Point::new(rect.x, rect.y),
        Point::new(rect.x + rect.w, rect.y),
        Point::new(rect.x, rect.y + rect.h),
        Point::new(rect.x + rect.w, rect.y + rect.h),
    ]
    .map(|p| m.apply(p));
    let x0 = pts.iter().map(|p| p.x).fold(f32::MAX, f32::min);
    let y0 = pts.iter().map(|p| p.y).fold(f32::MAX, f32::min);
    let x1 = pts.iter().map(|p| p.x).fold(f32::MIN, f32::max);
    let y1 = pts.iter().map(|p| p.y).fold(f32::MIN, f32::max);
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(style: Overflow) -> Leaving {
        let mut l = Leaving::new(style, 1.0, 0, 22.0, 16.0, 22.0);
        let g = LeavingGlyph {
            word: 1,
            glyph: 3,
            font: FontId::default(),
            pos: Point::new(4.0, 18.0),
            scale: 1.0,
            opacity: 1.0,
        };
        l.push(g, 9.0);
        l
    }

    #[test]
    fn fold_turns_to_its_angle_and_is_gone_at_the_end() {
        let l = line(Overflow::FOLD);
        assert_eq!(l.angle(1.0), 0.0);
        assert!((l.angle(1.22) - Fold::DEFAULT.angle).abs() < 1e-4);
        assert!(l.angle(1.08) > 0.0 && l.angle(1.08) < Fold::DEFAULT.angle);
        assert_eq!(l.opacity(1.05), 1.0);
        assert_eq!(l.opacity(1.22), 0.0);
        assert!((l.done_at() - 1.22).abs() < 1e-9);
        assert_eq!((l.x0, l.x1, l.baseline), (4.0, 13.0, 18.0));
    }

    #[test]
    fn shadow_brushes_for_about_a_hundred_ms() {
        let l = line(Overflow::FOLD);
        let lit: Vec<f64> = (0..=220).map(|ms| 1.0 + ms as f64 / 1000.0).filter(|&t| l.shadow(t) > 0.0).collect();
        let span = lit.last().unwrap() - lit.first().unwrap();
        assert!((0.08..0.12).contains(&span), "shadow lasts {span} s");
    }

    #[test]
    fn thin_squeezes_toward_the_baseline() {
        let l = line(Overflow::THIN);
        assert_eq!(l.thickness(1.0), 1.0);
        assert!(l.thickness(1.11) < 1.0);
        assert_eq!(l.thickness(1.22), 0.0);
        assert_eq!(l.angle(1.11), 0.0);
    }
}
