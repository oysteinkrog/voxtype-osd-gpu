//! Liquid revision (EG-32): a revised word flows into its correction in the live text.
//!
//! `Revise::Liquid` diffs a revised word like the palimpsest (`palimpsest::diff_ends`): the
//! letters both versions share are kept and slide on the flow's springs, the rest is
//! replaced. The removed letters do not sink as ghosts. For `LIQUID_SECS` the scene puts a
//! GPU item after the word's glyphs whose source is a [`LiquidRevision`]: the old letters
//! where they were, the kept and new letters where the springs have them in this frame, the
//! word's ink and its material. A painter draws it (`vo_3d::glyph_fx`, which registers itself
//! with [`install_painter`]):
//!
//! - On the reading line the text keeps drawing the word: kept letters stay solid and new
//!   letters arrive at `palimpsest::ARRIVE_INK` (at least 70 % ink) from their first frame,
//!   so the reading-line envelope holds. The liquid is only the old letters draining into the
//!   new ones, and colour around the new letters.
//! - On older lines the text hides the word while it flows and the painter draws all of it:
//!   kept letters solid at their sprung places, the old letters draining and the new ones
//!   filling through one smooth union.
//!
//! Without a painter `Revise::Liquid` is `Revise::Palimpsest`, and says so once in a debug
//! build. Under reduced motion the corrected word shows at once.

use std::any::Any;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::gpu::GpuSource;
use crate::material::Material;

/// How long the liquid runs from the revision, in seconds. At most 400 ms.
pub const LIQUID_SECS: f64 = 0.36;

static PAINTER: AtomicBool = AtomicBool::new(false);
static FELL_BACK: AtomicBool = AtomicBool::new(false);

thread_local! {
    static ON_THIS_THREAD: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Says a painter for [`LiquidRevision`] is installed, so `Revise::Liquid` stops falling
/// back to the palimpsest. `vo_3d::glyph_fx::install` calls it.
pub fn install_painter() {
    PAINTER.store(true, Ordering::Relaxed);
}

/// Whether a painter for [`LiquidRevision`] is installed.
pub fn painter() -> bool {
    ON_THIS_THREAD.with(Cell::get).unwrap_or_else(|| PAINTER.load(Ordering::Relaxed))
}

/// Overrides [`painter`] on the calling thread, for tests; `None` clears the override.
#[doc(hidden)]
pub fn painter_on_this_thread(on: Option<bool>) {
    ON_THIS_THREAD.with(|c| c.set(on));
}

fn fall_back_once() {
    if cfg!(debug_assertions) && !FELL_BACK.swap(true, Ordering::Relaxed) {
        eprintln!("vo-scene: Revise::Liquid has no painter (vo_3d::glyph_fx::install); using Revise::Palimpsest");
    }
}

/// Which letters of the word a liquid glyph is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiquidPart {
    /// A letter both versions share: solid, at its sprung place.
    Kept,
    /// A letter of the old text that is not in the new one: it drains.
    Old,
    /// A letter of the new text that is not in the old one: it fills.
    New,
}

/// One glyph of a liquid revision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiquidGlyph {
    pub font: FontId,
    pub glyph: u32,
    /// Pen position on the baseline, in the GPU item's box (logical units).
    pub pos: Point,
    /// Em size in logical units.
    pub size: f32,
    pub part: LiquidPart,
    /// The glyph's own opacity in the text this frame, 0 to 1.
    pub ink: f32,
}

/// What the liquid revision of one word hands its painter in one frame. Plain data: the
/// scene builds a new one each frame while the word flows.
#[derive(Clone, Debug, PartialEq)]
pub struct LiquidRevision {
    pub word: WordId,
    pub glyphs: Vec<LiquidGlyph>,
    /// Scene time of the revision.
    pub start: f64,
    /// Seconds the liquid runs.
    pub secs: f32,
    /// The word's text colour, straight alpha.
    pub ink: Color,
    /// The text's material, placed in the text's box, if it has one.
    pub material: Option<Material>,
    /// The word is on the reading line: the text draws it, the painter only the old letters
    /// draining and colour around the new ones.
    pub reading: bool,
}

impl LiquidRevision {
    /// How far the liquid is at `t`, 0 to 1.
    pub fn progress(&self, t: f64) -> f32 {
        ((t - self.start) / self.secs.max(1e-3) as f64).clamp(0.0, 1.0) as f32
    }
}

impl GpuSource for LiquidRevision {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn until(&self) -> f64 {
        self.start + self.secs as f64
    }
}

/// An old letter of a word in liquid revision, where it was drawn at the revision.
pub(crate) struct LiquidOld {
    pub glyph: u32,
    pub font: FontId,
    /// Pen position from the word's, and offset from its baseline.
    pub x: f32,
    pub y: f32,
    pub ink: f32,
}

/// A word in liquid revision.
pub(crate) struct LiquidWord {
    pub id: WordId,
    pub start: f64,
    pub old: Vec<LiquidOld>,
}

impl Flow {
    /// The revision of word `o` to `w` for `Revise::Liquid`: the palimpsest without a painter,
    /// the new word at once under reduced motion, otherwise the liquid.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn revise_liquid(
        &mut self,
        o: &mut FlowWord,
        w: &Word,
        p: &PlacedWord,
        graphemes: &[u32],
        make: &Make,
        reduced: bool,
        joins: bool,
    ) {
        if !painter() {
            fall_back_once();
            self.palimpsest(o, w, p, graphemes, make, reduced, joins);
            return;
        }
        self.liquid.retain(|l| l.id != o.id);
        if reduced {
            o.glyphs = p.glyphs.iter().zip(graphemes).map(|(g, &gr)| make.rest(g, gr)).collect();
            o.spread = false;
            return;
        }
        let t = make.t;
        let old_g: Vec<&str> = o.text.graphemes(true).collect();
        let new_g: Vec<&str> = w.text.graphemes(true).collect();
        let ops = palimpsest::diff_ends(&old_g, &new_g);
        let s = stretch(o, t);
        let mut olds: Vec<Option<FlowGlyph>> = std::mem::take(&mut o.glyphs).into_iter().map(Some).collect();
        let mut glyphs = Vec::with_capacity(p.glyphs.len());
        for (n, &gr) in p.glyphs.iter().zip(graphemes) {
            let claimed = match ops.get(gr as usize) {
                Some(GraphemeOp::Kept(from)) => olds
                    .iter_mut()
                    .find(|g| {
                        g.as_ref()
                            .is_some_and(|g| g.grapheme as usize == *from && g.glyph == n.glyph && g.font == n.font)
                    })
                    .and_then(Option::take),
                _ => None,
            };
            glyphs.push(match claimed {
                // A survivor keeps its ink and slides on its spring, never jumping.
                Some(mut g) => {
                    let x = g.x.value(t) as f32 * s;
                    g.x = make.to(x, n.x, FLIP, Rest::PIXELS);
                    FlowGlyph { grapheme: gr, advance: n.advance, y: n.y, ..g }
                }
                // New letters arrive in place at reading ink and fill; `born` marks them new.
                None => {
                    make.enter(n, gr, (if joins { 1.0 } else { palimpsest::ARRIVE_INK }, 0.0, 1.0), palimpsest::ARRIVE)
                }
            });
        }
        let old = olds
            .into_iter()
            .flatten()
            .filter_map(|g| {
                let ink = g.opacity.value(t) as f32;
                (ink > 0.0).then(|| LiquidOld {
                    glyph: g.glyph,
                    font: g.font,
                    x: g.x.value(t) as f32 * s,
                    y: g.y + g.rise.value(t) as f32,
                    ink,
                })
            })
            .collect();
        self.liquid.push(LiquidWord { id: o.id, start: t, old });
        o.glyphs = glyphs;
        o.spread = false;
    }

    /// Words in liquid revision at `t`: id, start and whether the word is on the reading line.
    pub(crate) fn liquid_words(&self, t: f64) -> Vec<(WordId, f64, bool)> {
        self.liquid
            .iter()
            .filter(|l| t >= l.start && t < l.start + LIQUID_SECS && self.words.iter().any(|w| w.id == l.id))
            .map(|l| (l.id, l.start, self.on_reading_line(l.id)))
            .collect()
    }

    /// Whether glyph `i` of the node's shaped text arrived with the revision at `start`.
    pub(crate) fn glyph_is_new(&self, i: usize, start: f64) -> bool {
        let Some(Some((w, gi))) = self.index.get(i) else { return false };
        self.words[*w as usize].glyphs[*gi as usize].born == start
    }

    /// The old letters of word `id` at `t` in the text box's space: glyph, font, pen position,
    /// scale and ink. They ride with the word.
    pub(crate) fn liquid_old(&self, id: WordId, t: f64) -> Vec<(u32, FontId, Point, f32, f32)> {
        let (Some(l), Some(w)) = (self.liquid.iter().find(|l| l.id == id), self.words.iter().find(|w| w.id == id))
        else {
            return Vec::new();
        };
        let y = w.y.value(t) as f32 + self.opened(w.line, t);
        l.old
            .iter()
            .map(|g| {
                let pen = Point::new(w.x.value(t) as f32 + g.x, y + g.y);
                let (pos, look) = self.on_stage(w, pen, t);
                let opacity = (g.ink as f64 * w.look_opacity.value(t)).clamp(0.0, 1.0) as f32;
                (g.glyph, g.font, pos, look, opacity)
            })
            .collect()
    }

    /// When the last liquid revision ends.
    pub(crate) fn liquid_settled_at(&self) -> f64 {
        self.liquid.iter().map(|l| l.start + LIQUID_SECS).fold(f64::NEG_INFINITY, f64::max)
    }
}
