//! Live text motion: words that never snap (TY08, TY14).
//!
//! The layout places every word at once for the new text. This module keeps where each
//! word is on screen and springs it to that place, so the text reads as one moving thing:
//!
//! - **Revise in place (T02).** A word whose text changes keeps its id. With
//!   `Revise::Crossfade` its old glyphs fade out over 120 ms while the new ones fade in at
//!   the same place. The word's width springs, and the words after it slide by the
//!   difference (FLIP, 200 ms).
//! - **Removal.** A word that goes fades out where it was in 120 ms.
//! - **Rewrap travel (T11).** A word that lands on another line glides there with a 220 ms
//!   spring. Words that change line start 12 ms apart in reading order. The wrap is decided
//!   once per change, by the layout, and never moves during the glide.
//! - **Caret (T17).** A bar after the last word. Its position springs in 120 ms, it stays
//!   solid while words arrive and for 400 ms after, then blinks at about 1 Hz.
//!
//! Positions are in the text box's space, with `y` on the word's baseline. `sample` adds the
//! difference between the sprung position and the laid-out one to each glyph, so a settled
//! word draws exactly where the layout put it.

use vo_core::channel::{Channel, Rest};
use vo_core::spring::Spring;
use vo_core::words::{Word, WordId};

use crate::geom::{Color, Point, Rect};
use crate::text::ShapedText;

/// Words that stay on their line move to their new place in about 200 ms.
pub const FLIP: Spring = Spring::critical_to_98(0.200);
/// A word that changes line glides there in about 220 ms.
pub const REWRAP: Spring = Spring::SOFT;
/// Delay between words that change line, in reading order.
pub const REWRAP_STAGGER: f64 = 0.012;
/// Old and new glyphs of a revised word cross in about 120 ms.
pub const CROSSFADE: Spring = Spring::SNAP;
/// The caret reaches the new end of the text in about 120 ms.
pub const CARET_GLIDE: Spring = Spring::SNAP;
/// The caret stays solid this long after words last arrived.
pub const CARET_HOLD: f64 = 0.4;
/// Caret blinks per second.
pub const CARET_HZ: f64 = 1.06;
/// Share of a blink period the caret is fully on.
const CARET_DUTY: f64 = 0.6;
const CARET_FADE_OUT: f64 = 0.12;
const CARET_FADE_IN: f64 = 0.08;
/// Frame rate while the caret fades.
const CARET_FPS: f64 = 30.0;

/// A text caret after the last word.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caret {
    /// Bar width in logical units.
    pub width: f32,
    /// Gap between the last word and the bar.
    pub gap: f32,
    /// Bar colour; `None` uses the text colour.
    pub color: Option<Color>,
}

impl Default for Caret {
    fn default() -> Caret {
        Caret { width: 2.0, gap: 2.0, color: None }
    }
}

/// What a word whose text changes does with its glyphs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Revise {
    /// The new glyphs replace the old ones at once. The word still moves and resizes by
    /// spring. For text whose glyphs something else animates, such as particle text.
    #[default]
    Swap,
    /// The old glyphs fade out over 120 ms while the new ones fade in at the same place.
    Crossfade,
}

/// How live text moves, set on the element.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveMotion {
    pub caret: Option<Caret>,
    pub revise: Revise,
}

/// What `Flow::update` needs from the node.
pub(crate) struct FlowInput<'a> {
    pub shaped: &'a ShapedText,
    pub words: &'a [Word],
    pub motion: &'a LiveMotion,
    /// Em size and line height of the text.
    pub size: f32,
    pub line_height: f32,
    pub retention: f64,
    /// Reduced motion: positions jump, fades stay.
    pub reduced: bool,
    /// Revised words may crossfade. Off for particle words, whose changed glyphs re-form
    /// from particles instead.
    pub crossfade: bool,
}

/// Where a word is on screen, sprung toward where the layout put it.
struct FlowWord {
    id: WordId,
    text: String,
    /// Pen position of the first glyph, on the baseline.
    x: Channel,
    y: Channel,
    /// From the first glyph's pen position to the end of the last glyph's advance.
    width: Channel,
    line: u32,
    glyphs: Vec<FlowGlyph>,
}

struct FlowGlyph {
    glyph: u32,
    /// Pen position from the word's, as sprung.
    x: Channel,
    /// Offset from the word's baseline, from shaping.
    y: f32,
    opacity: Channel,
}

/// A glyph on its way out: an old glyph of a revised word, or a glyph of a removed word.
struct Ghost {
    glyph: u32,
    word: WordId,
    /// `None` while the word is on screen, so the ghost moves with it; the word's last place
    /// once it is gone.
    fixed: Option<(f32, f32)>,
    /// Pen position from the word's, as drawn when the ghost was made.
    x: f32,
    y: f32,
    /// The word's width then. While the word is on screen the ghost's `x` stretches with it.
    width: f32,
    opacity: Channel,
}

/// One word as the new layout placed it.
struct Placed {
    /// `(index in the shaped glyphs, glyph id, pen x from the word's, y from the baseline)`.
    glyphs: Vec<(usize, u32, f32, f32)>,
    x: f32,
    y: f32,
    width: f32,
    line: u32,
}

/// A glyph's motion at `t`, on top of where the layout put it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GlyphMotion {
    pub dx: f32,
    pub dy: f32,
    pub opacity: f32,
}

impl GlyphMotion {
    pub const NONE: GlyphMotion = GlyphMotion { dx: 0.0, dy: 0.0, opacity: 1.0 };
}

/// A ghost glyph to draw at `t`, in the text box's space.
pub(crate) struct GhostGlyph {
    pub word: WordId,
    pub glyph: u32,
    pub pos: Point,
    pub opacity: f32,
}

/// The live text motion state of one node.
#[derive(Default)]
pub(crate) struct Flow {
    /// In reading order, only words the layout drew.
    words: Vec<FlowWord>,
    /// For each glyph of the node's shaped text: `(word slot, glyph slot)`, or `None` for a
    /// glyph outside any word (an ellipsis).
    index: Vec<Option<(u32, u32)>>,
    ghosts: Vec<Ghost>,
    /// Caret `(x, y)` on the baseline, and its bar size `(width, ascent, descent)`.
    caret: Option<(Channel, Channel, Caret, [f32; 3])>,
    /// When words last arrived, changed or went.
    arrived: f64,
}

/// How far a word's glyphs spread at `t` against their laid-out places: its sprung width
/// over its laid-out width. A revised word's glyphs move with its width, so they never run
/// into the words that slide out of the way.
fn stretch(w: &FlowWord, t: f64) -> f32 {
    let target = w.width.target();
    if target > 0.0 {
        (w.width.value(t) / target) as f32
    } else {
        1.0
    }
}

fn chan(t: f64, v: f64, spring: Spring, rest: Rest, retention: f64) -> Channel {
    Channel::new(t, v, spring).with_rest(rest).with_retention(retention)
}

/// Moves `ch` toward `target` from `at` with `spring`, or jumps there under reduced motion.
fn move_to(ch: &mut Channel, target: f32, at: f64, spring: Spring, reduced: bool) {
    let target = target as f64;
    if reduced {
        if ch.target() != target {
            ch.snap(at, target);
        }
    } else {
        ch.set_spring(spring);
        ch.retarget(at, target);
    }
}

/// The words as the layout placed them, aligned with `words`; `None` for a word with no
/// glyphs on screen (cut by `max_lines`).
fn place(shaped: &ShapedText, words: &[Word]) -> Vec<Option<Placed>> {
    let mut starts = Vec::with_capacity(words.len());
    let mut at = 0u32;
    for w in words {
        starts.push(at);
        at += w.text.len() as u32 + 1;
    }
    let mut out: Vec<Option<Placed>> = (0..words.len()).map(|_| None).collect();
    let (mut line, mut last_y) = (0u32, None::<f32>);
    for (i, g) in shaped.glyphs.iter().enumerate() {
        if g.cluster >= at {
            continue;
        }
        let w = starts.partition_point(|s| *s <= g.cluster) - 1;
        let p = out[w].get_or_insert_with(|| {
            if last_y.is_some_and(|y| g.pos.y > y + 0.5) {
                line += 1;
            }
            last_y = Some(g.pos.y);
            Placed { glyphs: Vec::new(), x: g.pos.x, y: g.pos.y, width: 0.0, line }
        });
        p.glyphs.push((i, g.glyph, g.pos.x - p.x, g.pos.y - p.y));
        p.width = p.width.max(g.pos.x + g.advance - p.x);
    }
    out
}

impl Flow {
    /// Takes the new layout of the node's words at `t` and springs every word toward it.
    pub fn update(&mut self, t: f64, input: FlowInput<'_>) {
        let FlowInput { shaped, words, motion, size, line_height, retention, reduced, crossfade } = input;
        let crossfade = crossfade && motion.revise == Revise::Crossfade;
        let placed = place(shaped, words);
        let ch = |v: f32, spring: Spring, rest: Rest| chan(t, v as f64, spring, rest, retention);
        let mut old = std::mem::take(&mut self.words);
        let mut next = Vec::with_capacity(words.len());
        let mut index = vec![None; shaped.glyphs.len()];
        let (mut stagger, mut arrived) = (0u32, false);
        for (w, p) in words.iter().zip(placed) {
            let prev = old.iter().position(|o| o.id == w.id).map(|i| old.remove(i));
            let Some(p) = p else {
                if let Some(o) = prev {
                    self.ghost_word(o, t, retention);
                }
                continue;
            };
            let slot = next.len() as u32;
            for (gi, &(si, ..)) in p.glyphs.iter().enumerate() {
                index[si] = Some((slot, gi as u32));
            }
            let Some(mut o) = prev else {
                arrived = true;
                let glyphs = p
                    .glyphs
                    .iter()
                    .map(|&(_, glyph, x, y)| FlowGlyph {
                        glyph,
                        x: ch(x, FLIP, Rest::PIXELS),
                        y,
                        opacity: ch(1.0, CROSSFADE, Rest::UNIT),
                    })
                    .collect();
                next.push(FlowWord {
                    id: w.id,
                    text: w.text.clone(),
                    x: ch(p.x, FLIP, Rest::PIXELS),
                    y: ch(p.y, FLIP, Rest::PIXELS),
                    width: ch(p.width, FLIP, Rest::PIXELS),
                    line: p.line,
                    glyphs,
                });
                continue;
            };
            if o.text != w.text && crossfade {
                arrived = true;
                // Old glyphs fade out where they are; new ones fade in at their places.
                let (s, width) = (stretch(&o, t), o.width.value(t) as f32);
                for g in &o.glyphs {
                    let k = g.opacity.value(t);
                    if k > 0.0 {
                        let mut opacity = chan(t, k, CROSSFADE, Rest::UNIT, retention);
                        opacity.retarget(t, 0.0);
                        self.ghosts.push(Ghost {
                            glyph: g.glyph,
                            word: o.id,
                            fixed: None,
                            x: g.x.value(t) as f32 * s,
                            y: g.y,
                            width,
                            opacity,
                        });
                    }
                }
                o.glyphs = p
                    .glyphs
                    .iter()
                    .map(|&(_, glyph, x, y)| {
                        let mut opacity = ch(0.0, CROSSFADE, Rest::UNIT);
                        opacity.retarget(t, 1.0);
                        FlowGlyph { glyph, x: ch(x, FLIP, Rest::PIXELS), y, opacity }
                    })
                    .collect();
                o.text = w.text.clone();
            } else if o.text == w.text && o.glyphs.len() == p.glyphs.len() {
                for (g, &(_, glyph, x, y)) in o.glyphs.iter_mut().zip(&p.glyphs) {
                    move_to(&mut g.x, x, t, FLIP, reduced);
                    (g.glyph, g.y) = (glyph, y);
                }
            } else {
                arrived |= o.text != w.text;
                o.text = w.text.clone();
                o.glyphs = p
                    .glyphs
                    .iter()
                    .map(|&(_, glyph, x, y)| FlowGlyph {
                        glyph,
                        x: ch(x, FLIP, Rest::PIXELS),
                        y,
                        opacity: ch(1.0, CROSSFADE, Rest::UNIT),
                    })
                    .collect();
            }
            if o.line != p.line {
                let at = t + REWRAP_STAGGER * stagger as f64;
                stagger += 1;
                move_to(&mut o.x, p.x, at, REWRAP, reduced);
                move_to(&mut o.y, p.y, at, REWRAP, reduced);
            } else {
                move_to(&mut o.x, p.x, t, FLIP, reduced);
                move_to(&mut o.y, p.y, t, FLIP, reduced);
            }
            move_to(&mut o.width, p.width, t, FLIP, reduced);
            o.line = p.line;
            next.push(o);
        }
        for o in old {
            arrived = true;
            self.ghost_word(o, t, retention);
        }
        self.words = next;
        self.index = index;
        self.ghosts.retain(|g| g.opacity.target() > 0.0 || g.opacity.settled_at() > t);
        if arrived {
            self.arrived = t;
        }
        self.update_caret(t, motion.caret, size, line_height, retention, reduced);
    }

    /// Fades out every glyph of a word that left the screen, where it is at `t`.
    fn ghost_word(&mut self, o: FlowWord, t: f64, retention: f64) {
        let at = (o.x.value(t) as f32, o.y.value(t) as f32);
        for g in self.ghosts.iter_mut().filter(|g| g.word == o.id && g.fixed.is_none()) {
            g.fixed = Some(at);
        }
        for g in &o.glyphs {
            let k = g.opacity.value(t);
            if k > 0.0 {
                let mut opacity = chan(t, k, CROSSFADE, Rest::UNIT, retention);
                opacity.retarget(t, 0.0);
                let x = g.x.value(t) as f32 * stretch(&o, t);
                let width = o.width.value(t) as f32;
                self.ghosts.push(Ghost { glyph: g.glyph, word: o.id, fixed: Some(at), x, y: g.y, width, opacity });
            }
        }
    }

    fn update_caret(
        &mut self,
        t: f64,
        caret: Option<Caret>,
        size: f32,
        line_height: f32,
        retention: f64,
        reduced: bool,
    ) {
        let Some(style) = caret else {
            self.caret = None;
            return;
        };
        let bar = [style.width, 0.78 * size, 0.22 * size];
        let (x, y) = match self.words.last() {
            Some(w) => (w.x.target() as f32 + w.width.target() as f32 + style.gap, w.y.target() as f32),
            // No words: the start of the first line, with the baseline where text would sit.
            None => (0.0, 0.5 * line_height + 0.28 * size),
        };
        match &mut self.caret {
            Some((cx, cy, s, b)) => {
                move_to(cx, x, t, CARET_GLIDE, reduced);
                move_to(cy, y, t, CARET_GLIDE, reduced);
                (*s, *b) = (style, bar);
            }
            None => {
                let c = |v: f32| chan(t, v as f64, CARET_GLIDE, Rest::PIXELS, retention);
                self.caret = Some((c(x), c(y), style, bar));
            }
        }
    }

    /// The motion of glyph `i` of the node's shaped text at `t`.
    pub fn glyph(&self, i: usize, t: f64) -> GlyphMotion {
        let Some(Some((w, g))) = self.index.get(i) else { return GlyphMotion::NONE };
        let w = &self.words[*w as usize];
        let g = &w.glyphs[*g as usize];
        let off = |c: &Channel| (c.value(t) - c.target()) as f32;
        GlyphMotion {
            dx: off(&w.x) + g.x.value(t) as f32 * stretch(w, t) - g.x.target() as f32,
            dy: off(&w.y),
            opacity: g.opacity.value(t).clamp(0.0, 1.0) as f32,
        }
    }

    /// Glyphs on their way out at `t`.
    pub fn ghosts(&self, t: f64) -> impl Iterator<Item = GhostGlyph> + '_ {
        self.ghosts.iter().filter_map(move |g| {
            let opacity = g.opacity.value(t).clamp(0.0, 1.0) as f32;
            if opacity <= 0.0 {
                return None;
            }
            let (x, y, gx) = match g.fixed {
                Some((x, y)) => (x, y, g.x),
                None => {
                    let w = self.words.iter().find(|w| w.id == g.word)?;
                    let k = if g.width > 0.0 { w.width.value(t) as f32 / g.width } else { 1.0 };
                    (w.x.value(t) as f32, w.y.value(t) as f32, g.x * k)
                }
            };
            Some(GhostGlyph { word: g.word, glyph: g.glyph, pos: Point::new(x + gx, y + g.y), opacity })
        })
    }

    /// A word's box at `t` in the text box's space, from its sprung position and width:
    /// what decorations and the caret follow.
    pub fn word_bounds(&self, id: WordId, t: f64, size: f32) -> Option<Rect> {
        let w = self.words.iter().find(|w| w.id == id)?;
        let (x, y) = (w.x.value(t) as f32, w.y.value(t) as f32);
        Some(Rect::new(x, y - 0.78 * size, w.width.value(t).max(0.0) as f32, size))
    }

    /// The caret bar at `t` in the text box's space, its colour if set, and its opacity.
    pub fn caret(&self, t: f64) -> Option<(Rect, Option<Color>, f32)> {
        let (x, y, style, [w, up, down]) = self.caret.as_ref()?;
        let alpha = caret_alpha(t - self.arrived);
        let (x, y) = (x.value(t) as f32, y.value(t) as f32);
        Some((Rect::new(x, y - up, *w, up + down), style.color, alpha))
    }

    /// When every word, glyph, ghost and the caret's glide come to rest.
    pub fn settled_at(&self) -> f64 {
        let words = self
            .words
            .iter()
            .flat_map(|w| [&w.x, &w.y, &w.width].into_iter().chain(w.glyphs.iter().flat_map(|g| [&g.x, &g.opacity])));
        let ghosts = self.ghosts.iter().map(|g| &g.opacity);
        let caret = self.caret.iter().flat_map(|(x, y, ..)| [x, y]);
        words.chain(ghosts).chain(caret).map(|c| c.settled_at()).fold(f64::NEG_INFINITY, f64::max)
    }

    /// When the caret's blink next needs a frame, if there is a caret.
    pub fn wake(&self, t: f64) -> Option<f64> {
        self.caret.as_ref()?;
        let idle = t - self.arrived;
        if idle < CARET_HOLD {
            return Some(self.arrived + CARET_HOLD);
        }
        let period = 1.0 / CARET_HZ;
        let u = (idle - CARET_HOLD).rem_euclid(period);
        let on = CARET_DUTY * period;
        let start = t - u;
        Some(if u < on {
            start + on
        } else if u < on + CARET_FADE_OUT {
            t + 1.0 / CARET_FPS
        } else if u < period - CARET_FADE_IN {
            start + period - CARET_FADE_IN
        } else {
            t + 1.0 / CARET_FPS
        })
    }
}

/// Caret opacity `idle` seconds after words last arrived: solid for `CARET_HOLD`, then on
/// for 60% of each period, a 120 ms fade out, off, and an 80 ms fade in.
pub fn caret_alpha(idle: f64) -> f32 {
    if idle < CARET_HOLD {
        return 1.0;
    }
    let period = 1.0 / CARET_HZ;
    let u = (idle - CARET_HOLD).rem_euclid(period);
    let on = CARET_DUTY * period;
    let a = if u < on {
        1.0
    } else if u < on + CARET_FADE_OUT {
        1.0 - (u - on) / CARET_FADE_OUT
    } else if u < period - CARET_FADE_IN {
        0.0
    } else {
        (u - (period - CARET_FADE_IN)) / CARET_FADE_IN
    };
    a as f32
}
