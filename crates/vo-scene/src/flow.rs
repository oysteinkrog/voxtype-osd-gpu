//! Live text motion: words that never snap (TY08, TY14).
//!
//! The layout places every word at once for the new text. This module keeps where each
//! word is on screen and springs it to that place, so the text reads as one moving thing:
//!
//! - **Revise in place (T02).** A word whose text changes keeps its id. With
//!   `Revise::Crossfade` its old glyphs fade out over 120 ms while the new ones fade in at
//!   the same place. The word's width springs, and the words after it slide by the
//!   difference (FLIP, 200 ms).
//! - **Glyph diff (T08, T09).** With `Revise::Glyphs` a revised word is diffed by grapheme
//!   (longest common subsequence). Kept glyphs slide to their new places. A letter whose
//!   case changed rolls: the old one out upward, the new one in from below, 180 ms; or,
//!   with `CaseChange::Grow` (F5), it grows from x-height to cap height. An inserted comma
//!   or full stop scales in from the baseline with a small overshoot, 140 ms. Other new
//!   letters rise in and removed ones rise out. When less than half the word is kept it
//!   crossfades instead.
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

use unicode_segmentation::UnicodeSegmentation;
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
/// A changed letter rolls out and in, and new letters rise in, in about 180 ms.
pub const GLYPH_ROLL: Spring = Spring::critical_to_98(0.180);
/// Inserted punctuation scales in with a small overshoot (about 10%) in about 140 ms.
pub const GLYPH_POP: Spring = Spring::from_frequency(40.0, 0.6);
/// How far a rolling letter travels, in ems.
pub const ROLL_EMS: f32 = 0.35;
/// How far a new letter rises in, or a removed one rises out, in ems.
pub const RISE_EMS: f32 = 0.25;
/// x-height over cap height, for `CaseChange::Grow`. Close to Noto Sans and most UI fonts.
pub const X_TO_CAP: f32 = 0.75;
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
    /// Only the glyphs that changed move: a grapheme diff of the old and new word.
    Glyphs,
}

/// How `Revise::Glyphs` shows a letter whose case changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaseChange {
    /// The old letter rolls out upward and the new one rolls in from below.
    #[default]
    Roll,
    /// The letter grows from x-height to cap height, or shrinks back (F5).
    Grow,
}

/// How live text moves, set on the element.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveMotion {
    pub caret: Option<Caret>,
    pub revise: Revise,
    pub case: CaseChange,
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
    /// Revised words may crossfade or diff. Off for particle words, whose changed glyphs
    /// re-form from particles instead.
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
    /// The glyphs spread with the width (a crossfade); otherwise each glyph springs alone.
    spread: bool,
    line: u32,
    glyphs: Vec<FlowGlyph>,
}

/// A glyph's own motion: where it sits in its word, and its enter or change.
struct FlowGlyph {
    glyph: u32,
    /// Index of its grapheme in the word's text.
    grapheme: u32,
    advance: f32,
    /// Pen position from the word's, as sprung.
    x: Channel,
    /// Offset from the word's baseline, from shaping.
    y: f32,
    opacity: Channel,
    /// Vertical travel, down positive (a roll or a rise).
    rise: Channel,
    /// Size factor about the glyph's baseline centre.
    scale: Channel,
}

/// A glyph on its way out: an old glyph of a revised word, or a glyph of a removed word.
struct Ghost {
    glyph: u32,
    word: WordId,
    advance: f32,
    /// `None` while the word is on screen, so the ghost moves with it; the word's last place
    /// once it is gone.
    fixed: Option<(f32, f32)>,
    /// Pen position from the word's, as drawn when the ghost was made.
    x: f32,
    y: f32,
    /// The word's width then, for a crossfade: while the word is on screen the ghost's `x`
    /// stretches with it. 0 for no stretch.
    width: f32,
    opacity: Channel,
    rise: Channel,
    scale: Channel,
}

/// One glyph as the new layout placed it.
#[derive(Clone, Copy)]
struct Placed {
    /// Index in the shaped glyphs.
    index: usize,
    glyph: u32,
    /// Pen position from the word's, and offset from its baseline.
    x: f32,
    y: f32,
    advance: f32,
    /// Byte offset of the glyph's cluster in the word's text.
    offset: u32,
}

/// One word as the new layout placed it.
struct PlacedWord {
    glyphs: Vec<Placed>,
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
    /// Multiplies the em size; `dx` already keeps the glyph centred.
    pub scale: f32,
}

impl GlyphMotion {
    pub const NONE: GlyphMotion = GlyphMotion { dx: 0.0, dy: 0.0, opacity: 1.0, scale: 1.0 };
}

/// A ghost glyph to draw at `t`, in the text box's space.
pub(crate) struct GhostGlyph {
    pub word: WordId,
    pub glyph: u32,
    pub pos: Point,
    pub opacity: f32,
    pub scale: f32,
}

/// What happens to one grapheme of a revised word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphemeOp {
    /// The same grapheme as old grapheme `n`: it slides to its new place.
    Kept(usize),
    /// Old grapheme `n` in another case ("t" to "T").
    Case(usize),
    /// New.
    Enter,
}

/// Diffs two words by grapheme: the op for each new grapheme, and for each old one whether
/// it leaves (neither kept nor a case change). Kept graphemes are a longest common
/// subsequence. Between two kept runs, old and new graphemes pair up in order when they
/// differ only in case.
pub fn diff_graphemes(old: &[&str], new: &[&str]) -> (Vec<GraphemeOp>, Vec<bool>) {
    let (n, m) = (old.len(), new.len());
    // lcs[i][j]: length of the LCS of old[i..] and new[j..].
    let mut lcs = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let mut ops = vec![GraphemeOp::Enter; m];
    let mut leaves = vec![true; n];
    let (mut i, mut j) = (0, 0);
    let (mut run_i, mut run_j) = (0, 0);
    let pair_run = |ri: usize, i: usize, rj: usize, j: usize, ops: &mut [GraphemeOp], leaves: &mut [bool]| {
        for (a, b) in (ri..i).zip(rj..j) {
            if old[a] != new[b] && old[a].to_lowercase() == new[b].to_lowercase() {
                ops[b] = GraphemeOp::Case(a);
                leaves[a] = false;
            }
        }
    };
    while i < n && j < m {
        if old[i] == new[j] {
            pair_run(run_i, i, run_j, j, &mut ops, &mut leaves);
            ops[j] = GraphemeOp::Kept(i);
            leaves[i] = false;
            i += 1;
            j += 1;
            (run_i, run_j) = (i, j);
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pair_run(run_i, n, run_j, m, &mut ops, &mut leaves);
    (ops, leaves)
}

/// Whether a grapheme is punctuation that scales in from the baseline.
fn is_punct(g: &str) -> bool {
    g.chars()
        .all(|c| c.is_ascii_punctuation() || "\u{2026}\u{2013}\u{2014}\u{2018}\u{2019}\u{201c}\u{201d}".contains(c))
}

/// How far a word's glyphs spread at `t` against their laid-out places: its sprung width
/// over its laid-out width, while it crossfades. A crossfading word's glyphs move with its
/// width, so they never run into the words that slide out of the way.
fn stretch(w: &FlowWord, t: f64) -> f32 {
    let target = w.width.target();
    if w.spread && target > 0.0 {
        (w.width.value(t) / target) as f32
    } else {
        1.0
    }
}

fn chan(t: f64, v: f64, spring: Spring, rest: Rest, retention: f64) -> Channel {
    Channel::new(t, v, spring).with_rest(rest).with_retention(retention)
}

/// A channel at `from` at `t` that springs to `to`.
fn from_to(t: f64, from: f32, to: f32, spring: Spring, rest: Rest, retention: f64) -> Channel {
    let mut c = chan(t, from as f64, spring, rest, retention);
    if from != to {
        c.retarget(t, to as f64);
    }
    c
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
fn place(shaped: &ShapedText, words: &[Word]) -> Vec<Option<PlacedWord>> {
    let mut starts = Vec::with_capacity(words.len());
    let mut at = 0u32;
    for w in words {
        starts.push(at);
        at += w.text.len() as u32 + 1;
    }
    let mut out: Vec<Option<PlacedWord>> = (0..words.len()).map(|_| None).collect();
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
            PlacedWord { glyphs: Vec::new(), x: g.pos.x, y: g.pos.y, width: 0.0, line }
        });
        p.glyphs.push(Placed {
            index: i,
            glyph: g.glyph,
            x: g.pos.x - p.x,
            y: g.pos.y - p.y,
            advance: g.advance,
            offset: g.cluster - starts[w],
        });
        p.width = p.width.max(g.pos.x + g.advance - p.x);
    }
    out
}

/// The grapheme index of each placed glyph, from its byte offset in `text`.
fn graphemes_of(text: &str, glyphs: &[Placed]) -> Vec<u32> {
    let starts: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
    glyphs.iter().map(|g| (starts.partition_point(|s| *s <= g.offset as usize).max(1) - 1) as u32).collect()
}

/// Motion settings for building glyphs at one `t`.
struct Make {
    t: f64,
    retention: f64,
    size: f32,
}

impl Make {
    fn ch(&self, v: f32, spring: Spring, rest: Rest) -> Channel {
        chan(self.t, v as f64, spring, rest, self.retention)
    }

    fn to(&self, from: f32, to: f32, spring: Spring, rest: Rest) -> Channel {
        from_to(self.t, from, to, spring, rest, self.retention)
    }

    /// A glyph at rest at its place.
    fn rest(&self, p: &Placed, grapheme: u32) -> FlowGlyph {
        FlowGlyph {
            glyph: p.glyph,
            grapheme,
            advance: p.advance,
            x: self.ch(p.x, FLIP, Rest::PIXELS),
            y: p.y,
            opacity: self.ch(1.0, CROSSFADE, Rest::UNIT),
            rise: self.ch(0.0, GLYPH_ROLL, Rest::PIXELS),
            scale: self.ch(1.0, GLYPH_POP, Rest::UNIT),
        }
    }

    /// A glyph that comes in: `(opacity, rise in ems, scale)` at the start, and the spring.
    fn enter(&self, p: &Placed, grapheme: u32, from: (f32, f32, f32), spring: Spring) -> FlowGlyph {
        FlowGlyph {
            opacity: self.to(from.0, 1.0, spring, Rest::UNIT),
            rise: self.to(from.1 * self.size, 0.0, spring, Rest::PIXELS),
            scale: self.to(from.2, 1.0, spring, Rest::UNIT),
            ..self.rest(p, grapheme)
        }
    }
}

impl Flow {
    /// Takes the new layout of the node's words at `t` and springs every word toward it.
    pub fn update(&mut self, t: f64, input: FlowInput<'_>) {
        let FlowInput { shaped, words, motion, size, line_height, retention, reduced, crossfade } = input;
        let revise = if crossfade { motion.revise } else { Revise::Swap };
        let make = Make { t, retention, size };
        let placed = place(shaped, words);
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
            for (gi, g) in p.glyphs.iter().enumerate() {
                index[g.index] = Some((slot, gi as u32));
            }
            let graphemes = graphemes_of(&w.text, &p.glyphs);
            let Some(mut o) = prev else {
                arrived = true;
                let glyphs = p.glyphs.iter().zip(&graphemes).map(|(g, &gr)| make.rest(g, gr)).collect();
                next.push(FlowWord {
                    id: w.id,
                    text: w.text.clone(),
                    x: make.ch(p.x, FLIP, Rest::PIXELS),
                    y: make.ch(p.y, FLIP, Rest::PIXELS),
                    width: make.ch(p.width, FLIP, Rest::PIXELS),
                    spread: false,
                    line: p.line,
                    glyphs,
                });
                continue;
            };
            if o.text != w.text {
                arrived = true;
                let diffed = revise == Revise::Glyphs && self.revise_glyphs(&mut o, w, &p, &graphemes, &make, motion);
                if !diffed {
                    if revise == Revise::Swap {
                        o.glyphs = p.glyphs.iter().zip(&graphemes).map(|(g, &gr)| make.rest(g, gr)).collect();
                        o.spread = false;
                    } else {
                        self.crossfade(&mut o, &p, &graphemes, &make);
                    }
                }
                o.text = w.text.clone();
            } else if o.glyphs.len() == p.glyphs.len() {
                for ((g, n), &gr) in o.glyphs.iter_mut().zip(&p.glyphs).zip(&graphemes) {
                    move_to(&mut g.x, n.x, t, FLIP, reduced);
                    (g.glyph, g.y, g.advance, g.grapheme) = (n.glyph, n.y, n.advance, gr);
                }
            } else {
                o.glyphs = p.glyphs.iter().zip(&graphemes).map(|(g, &gr)| make.rest(g, gr)).collect();
                o.spread = false;
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

    /// Old glyphs fade out where they are; new ones fade in at their places, and both spread
    /// with the word's width.
    fn crossfade(&mut self, o: &mut FlowWord, p: &PlacedWord, graphemes: &[u32], make: &Make) {
        let t = make.t;
        let (s, width) = (stretch(o, t), o.width.value(t) as f32);
        for g in &o.glyphs {
            let k = g.opacity.value(t) as f32;
            if k > 0.0 {
                self.ghosts.push(Ghost {
                    glyph: g.glyph,
                    word: o.id,
                    advance: g.advance,
                    fixed: None,
                    x: g.x.value(t) as f32 * s,
                    y: g.y,
                    width,
                    opacity: make.to(k, 0.0, CROSSFADE, Rest::UNIT),
                    rise: make.ch(g.rise.value(t) as f32, GLYPH_ROLL, Rest::PIXELS),
                    scale: make.ch(g.scale.value(t) as f32, GLYPH_POP, Rest::UNIT),
                });
            }
        }
        o.glyphs = p
            .glyphs
            .iter()
            .zip(graphemes)
            .map(|(g, &gr)| FlowGlyph { opacity: make.to(0.0, 1.0, CROSSFADE, Rest::UNIT), ..make.rest(g, gr) })
            .collect();
        o.spread = true;
    }

    /// The glyph diff (T08, T09). Returns false, changing nothing, when less than half the
    /// word is kept: a crossfade reads better then.
    fn revise_glyphs(
        &mut self,
        o: &mut FlowWord,
        w: &Word,
        p: &PlacedWord,
        graphemes: &[u32],
        make: &Make,
        motion: &LiveMotion,
    ) -> bool {
        let t = make.t;
        let old_g: Vec<&str> = o.text.graphemes(true).collect();
        let new_g: Vec<&str> = w.text.graphemes(true).collect();
        let (ops, leaves) = diff_graphemes(&old_g, &new_g);
        let kept = ops.iter().filter(|op| matches!(op, GraphemeOp::Kept(_))).count();
        if 2 * kept < old_g.len().max(new_g.len()) {
            return false;
        }
        let s = stretch(o, t);
        let (size, word) = (make.size, o.id);
        // Old glyphs by grapheme, taken in order as new glyphs claim them.
        let mut old_glyphs: Vec<Option<FlowGlyph>> = std::mem::take(&mut o.glyphs).into_iter().map(Some).collect();
        let ghost = |g: &FlowGlyph, to: (f32, f32), spring: Spring, ghosts: &mut Vec<Ghost>| {
            let k = g.opacity.value(t) as f32;
            if k <= 0.0 {
                return;
            }
            ghosts.push(Ghost {
                glyph: g.glyph,
                word,
                advance: g.advance,
                fixed: None,
                x: g.x.value(t) as f32 * s,
                y: g.y,
                width: 0.0,
                opacity: make.to(k, 0.0, spring, Rest::UNIT),
                rise: make.to(g.rise.value(t) as f32, to.0 * size, spring, Rest::PIXELS),
                scale: make.to(g.scale.value(t) as f32, to.1, spring, Rest::UNIT),
            });
        };
        let mut glyphs = Vec::with_capacity(p.glyphs.len());
        for (n, &gr) in p.glyphs.iter().zip(graphemes) {
            let op = ops.get(gr as usize).copied().unwrap_or(GraphemeOp::Enter);
            let claim = |old_glyphs: &mut Vec<Option<FlowGlyph>>, from: usize| {
                old_glyphs.iter_mut().find(|g| g.as_ref().is_some_and(|g| g.grapheme as usize == from))?.take()
            };
            let g = match op {
                GraphemeOp::Kept(from) => match claim(&mut old_glyphs, from) {
                    Some(mut g) => {
                        // Slides from where it is drawn now to its new place.
                        let x = g.x.value(t) as f32 * s;
                        g.x = make.to(x, n.x, FLIP, Rest::PIXELS);
                        FlowGlyph { glyph: n.glyph, grapheme: gr, advance: n.advance, y: n.y, ..g }
                    }
                    None => make.enter(n, gr, (0.0, 0.0, 1.0), CROSSFADE),
                },
                GraphemeOp::Case(from) => {
                    let upper = new_g[gr as usize] != new_g[gr as usize].to_lowercase();
                    let (grow_from, shrink_to) =
                        if upper { (X_TO_CAP, 1.0 / X_TO_CAP) } else { (1.0 / X_TO_CAP, X_TO_CAP) };
                    while let Some(old) = claim(&mut old_glyphs, from) {
                        match motion.case {
                            CaseChange::Roll => ghost(&old, (-ROLL_EMS, 1.0), GLYPH_ROLL, &mut self.ghosts),
                            CaseChange::Grow => ghost(&old, (0.0, shrink_to), GLYPH_ROLL, &mut self.ghosts),
                        }
                    }
                    match motion.case {
                        CaseChange::Roll => make.enter(n, gr, (0.0, ROLL_EMS, 1.0), GLYPH_ROLL),
                        CaseChange::Grow => make.enter(n, gr, (0.0, 0.0, grow_from), GLYPH_ROLL),
                    }
                }
                GraphemeOp::Enter if is_punct(new_g.get(gr as usize).copied().unwrap_or("")) => {
                    make.enter(n, gr, (1.0, 0.0, 0.0), GLYPH_POP)
                }
                GraphemeOp::Enter => make.enter(n, gr, (0.0, RISE_EMS, 1.0), GLYPH_ROLL),
            };
            glyphs.push(g);
        }
        // Old glyphs no new glyph claimed: removed graphemes, and spare glyphs of kept ones.
        for g in old_glyphs.into_iter().flatten() {
            let gone = leaves.get(g.grapheme as usize).copied().unwrap_or(true);
            let to = if gone { -RISE_EMS } else { 0.0 };
            ghost(&g, (to, 1.0), GLYPH_ROLL, &mut self.ghosts);
        }
        o.glyphs = glyphs;
        o.spread = false;
        true
    }

    /// Fades out every glyph of a word that left the screen, where it is at `t`.
    fn ghost_word(&mut self, o: FlowWord, t: f64, retention: f64) {
        let at = (o.x.value(t) as f32, o.y.value(t) as f32);
        for g in self.ghosts.iter_mut().filter(|g| g.word == o.id && g.fixed.is_none()) {
            let k = if g.width > 0.0 { o.width.value(t) as f32 / g.width } else { 1.0 };
            g.x *= k;
            g.fixed = Some(at);
        }
        let s = stretch(&o, t);
        for g in &o.glyphs {
            let k = g.opacity.value(t);
            if k > 0.0 {
                let ch = |v: f64, spring: Spring, rest: Rest| chan(t, v, spring, rest, retention);
                let mut opacity = ch(k, CROSSFADE, Rest::UNIT);
                opacity.retarget(t, 0.0);
                self.ghosts.push(Ghost {
                    glyph: g.glyph,
                    word: o.id,
                    advance: g.advance,
                    fixed: Some(at),
                    x: g.x.value(t) as f32 * s,
                    y: g.y,
                    width: 0.0,
                    opacity,
                    rise: ch(g.rise.value(t), GLYPH_ROLL, Rest::PIXELS),
                    scale: ch(g.scale.value(t), GLYPH_POP, Rest::UNIT),
                });
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
        let scale = g.scale.value(t).max(0.0) as f32;
        GlyphMotion {
            dx: off(&w.x) + g.x.value(t) as f32 * stretch(w, t) - g.x.target() as f32 + 0.5 * g.advance * (1.0 - scale),
            dy: off(&w.y) + g.rise.value(t) as f32,
            opacity: g.opacity.value(t).clamp(0.0, 1.0) as f32,
            scale,
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
            let scale = g.scale.value(t).max(0.0) as f32;
            let pos = Point::new(x + gx + 0.5 * g.advance * (1.0 - scale), y + g.y + g.rise.value(t) as f32);
            Some(GhostGlyph { word: g.word, glyph: g.glyph, pos, opacity, scale })
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
        let words = self.words.iter().flat_map(|w| {
            [&w.x, &w.y, &w.width]
                .into_iter()
                .chain(w.glyphs.iter().flat_map(|g| [&g.x, &g.opacity, &g.rise, &g.scale]))
        });
        let ghosts = self.ghosts.iter().flat_map(|g| [&g.opacity, &g.rise, &g.scale]);
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

#[cfg(test)]
mod tests {
    use super::*;
    use GraphemeOp::*;

    fn g(s: &str) -> Vec<&str> {
        s.graphemes(true).collect()
    }

    #[test]
    fn case_fix_changes_one_letter() {
        let (ops, leaves) = diff_graphemes(&g("hello"), &g("Hello"));
        assert_eq!(ops, vec![Case(0), Kept(1), Kept(2), Kept(3), Kept(4)]);
        assert!(leaves.iter().all(|l| !l));
    }

    #[test]
    fn inserted_comma_enters_and_the_rest_is_kept() {
        let (ops, leaves) = diff_graphemes(&g("hello"), &g("hello,"));
        assert_eq!(ops, vec![Kept(0), Kept(1), Kept(2), Kept(3), Kept(4), Enter]);
        assert!(leaves.iter().all(|l| !l));
        assert!(is_punct(",") && is_punct(".") && !is_punct("a"));
    }

    #[test]
    fn substitutions_leave_and_enter() {
        // "there" to "their": "the" and the "r" kept; "e" leaves, "i" enters.
        let (ops, leaves) = diff_graphemes(&g("there"), &g("their"));
        assert_eq!(ops.iter().filter(|o| matches!(o, Kept(_))).count(), 4);
        assert_eq!(leaves.iter().filter(|l| **l).count(), 1);
        assert_eq!(ops.iter().filter(|o| **o == Enter).count(), 1);
    }

    #[test]
    fn graphemes_not_bytes() {
        // A combining accent stays one grapheme.
        let (ops, _) = diff_graphemes(&g("cafe\u{301}"), &g("Cafe\u{301}"));
        assert_eq!(ops, vec![Case(0), Kept(1), Kept(2), Kept(3)]);
    }
}
