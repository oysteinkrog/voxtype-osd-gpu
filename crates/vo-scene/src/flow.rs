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
//! - **Palimpsest (MO-20).** The default. `Revise::Palimpsest` diffs like `Revise::Glyphs`,
//!   but survivors keep their ink, removed letters sink into a short-lived ghost and new
//!   letters arrive in place (module `palimpsest`).
//! - **Liquid (EG-32).** `Revise::Liquid` diffs like the palimpsest, and the old letters
//!   drain into the new ones as liquid drawn by a GPU painter, in the word's material, for
//!   at most 400 ms (module `liquid`). Without a painter it is the palimpsest.
//! - **Around a revision (MO-21 to MO-23).** Opt-in with `revisions`: width escrow holds
//!   the neighbours through a burst, a hinge brackets moved word breaks, and paired margin
//!   marks point from an older changed line to the newest one (module `revisions`).
//! - **Removal.** A word that goes fades out where it was in 120 ms.
//! - **Rewrap.** A word that lands on another line fades out where it was and in at its new
//!   place, sliding with the words before it there, so it never crosses other words. While
//!   words after it on its new line still slide through that place it waits, then shows (on
//!   the newest line whole, so it is readable in the frame it arrives). With
//!   `Rewrap::Glide` (T11) it glides there with a 220 ms spring instead, words that change
//!   line 12 ms apart in reading order. The wrap is decided once per change, by the layout,
//!   and never moves during the glide.
//! - **Settling ink (T03, F1).** With `SettleInk`, the newest words arrive lighter and a
//!   little looser and settle to full ink in 300 to 600 ms, slower for words revised often.
//!   A revision starts the settle again; a stable list from the recogniser holds a word
//!   until it is stable.
//! - **Reading stage (T14).** With `Element::reading` the newest line holds a fixed
//!   height in the text box and older lines recede upward, each taking the look of its age
//!   (scale about a point on the line, and opacity), 260 ms by default. A word that rewraps
//!   onto the new line glides along the stage: its rewrap travel and the roll cancel.
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

use crate::font::FontId;
use crate::geom::{Color, Point, Rect};
use crate::lines::LineLook;
use crate::text::ShapedText;

#[path = "motion/liquid.rs"]
pub mod liquid;
#[path = "motion/palimpsest.rs"]
pub mod palimpsest;
#[path = "motion/revisions.rs"]
pub mod revisions;

#[path = "motion/letter_physics.rs"]
pub mod letter_physics;

#[path = "motion/magnet.rs"]
pub mod magnet;

/// Words that stay on their line move to their new place in about 200 ms.
pub const FLIP: Spring = Spring::critical_to_98(0.200);
/// A word that changes line glides there in about 220 ms.
pub const REWRAP: Spring = Spring::SOFT;
/// Delay between words that change line, in reading order.
pub const REWRAP_STAGGER: f64 = 0.012;
/// A word inserted between kept words starts to fade in this long after they start to move.
pub const INSERT_DELAY: f64 = 0.1;
/// A word that rewraps onto a line waits at most this long for the words still sliding
/// through its place on that line.
pub const REWRAP_WAIT_MAX: f64 = 0.3;
/// When words leave from the front, the rest scroll up together in about 220 ms.
pub const SCROLL: Spring = Spring::SOFT;
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
    /// Colour at the bottom of the bar (FX-037): the bar fades from its colour at the top to
    /// this. `None` is a plain bar.
    pub gradient: Option<Color>,
}

impl Default for Caret {
    fn default() -> Caret {
        Caret { width: 2.0, gap: 2.0, color: None, gradient: None }
    }
}

/// How a word moves to another line when the text reflows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rewrap {
    /// It fades out where it is and in at its new place, sliding there with the words before
    /// it on its new line, so it never crosses the text between.
    #[default]
    Fade,
    /// It glides to its new place in about 220 ms, words in reading order a few ms apart
    /// (T11). Calm for a single change; with words arriving faster than the glide, words
    /// in flight cross the lines between. A front drop always fades.
    Glide,
}

/// What a word whose text changes does with its glyphs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Revise {
    /// The new glyphs replace the old ones at once. The word still moves and resizes by
    /// spring. For text whose glyphs something else animates, such as particle text.
    Swap,
    /// The palimpsest (MO-20), the default: letters that survive stay; removed ones sink
    /// 4 px into a 20 % ghost that holds for 500 ms; new ones arrive in place. See
    /// [`palimpsest`].
    #[default]
    Palimpsest,
    /// The old glyphs fade out over 120 ms while the new ones fade in at the same place.
    Crossfade,
    /// Only the glyphs that changed move: a grapheme diff of the old and new word.
    Glyphs,
    /// The old word rolls up and fades while the new one rolls in from below into the same
    /// slot, in about 220 ms (FX-065).
    Roll,
    /// The old word shrinks and floats above its replacement at 35% opacity, and its line
    /// opens to make room, for `GHOST_HOLD` before both fade back (FX-064). The opening
    /// pushes the lines below down: leave room under the text for it.
    Ghost,
    /// The liquid morph (EG-32): the old letters drain and the new ones fill through a smooth
    /// union, drawn by a GPU painter, in at most 400 ms. Kept letters slide on their springs;
    /// on the reading line the text still draws the word. Without a painter it is
    /// `Palimpsest`. See [`liquid`].
    Liquid,
}

/// A rolled word's travel and fade take about 220 ms.
pub const WORD_ROLL: Spring = Spring::critical_to_98(0.220);
/// How far a rolled word travels, in ems.
pub const ROLL_WORD_EMS: f32 = 0.6;
/// A displaced ghost: its size, opacity, lift above the baseline in ems, how far its line
/// opens in ems, how long it stays, and the spring it moves on.
pub const GHOST_SCALE: f32 = 0.62;
pub const GHOST_ALPHA: f32 = 0.35;
pub const GHOST_LIFT_EMS: f32 = 0.95;
pub const GHOST_OPEN_EMS: f32 = 0.55;
pub const GHOST_HOLD: f64 = 1.6;
pub const GHOST_MOVE: Spring = Spring::critical_to_98(0.260);

/// How `Revise::Glyphs` shows a letter whose case changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaseChange {
    /// The old letter rolls out upward and the new one rolls in from below.
    #[default]
    Roll,
    /// The letter grows from x-height to cap height, or shrinks back (F5).
    Grow,
}

/// Settling ink (T03, F1): the newest words arrive lighter and a little looser, and
/// settle to full ink and normal spacing. A revision starts the settle again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SettleInk {
    /// Opacity a new or revised word arrives at.
    pub ink: f32,
    /// Extra space between its letters on arrival, in ems.
    pub tracking: f32,
    /// Settle time, in seconds, of a word on its first version.
    pub fastest: f64,
    /// Settle time of a word revised three times or more. Words the recogniser keeps
    /// changing settle slower, so the eye trusts them later.
    pub slowest: f64,
    /// A colour a new or revised word arrives in, mixing to the text colour as it settles.
    pub accent: Option<Color>,
}

impl SettleInk {
    /// The same settle, with new words arriving in `color`.
    pub fn accent(self, color: Color) -> SettleInk {
        SettleInk { accent: Some(color), ..self }
    }
}

impl Default for SettleInk {
    fn default() -> SettleInk {
        SettleInk { ink: 0.55, tracking: 0.03, fastest: 0.3, slowest: 0.6, accent: None }
    }
}

/// The active-line reading stage: where the newest line sits and how older lines look.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// The centre of the newest line, from the top of the text box, in logical units.
    pub line_y: f32,
    /// The look of each line age, newest first. Lines older than the last look keep it.
    pub looks: Vec<LineLook>,
    /// Where on a line its scale is centred, from 0 (left end) to 1 (right end).
    pub origin: f32,
    /// Moves lines up and eases their looks.
    pub travel: Spring,
}

impl Reading {
    /// The teleprompter: the newest line at `line_y` at full size; the line before at 0.88
    /// and 0.45 opacity; older lines faded out; 260 ms; scaled about the left end.
    pub fn teleprompter(line_y: f32) -> Reading {
        Reading {
            line_y,
            looks: vec![LineLook::new(1.0, 1.0), LineLook::new(0.88, 0.45), LineLook::new(0.88, 0.0)],
            origin: 0.0,
            travel: Spring::critical_to_98(0.26),
        }
    }

    fn look(&self, age: u32) -> LineLook {
        let last = self.looks.len().saturating_sub(1);
        self.looks.get((age as usize).min(last)).copied().unwrap_or(LineLook::new(1.0, 1.0))
    }
}

/// Whether a glyph on the newest line breaks the reading-line envelope of the motion plan
/// (MO-08): ink under 70 % of rest, more than 3 px off its baseline, scaled or blurred.
/// `ink` and `scale` are relative to rest, `dy` and `blur` in layout units.
pub(crate) fn breaks_envelope(ink: f32, dy: f32, scale: f32, blur: f32) -> bool {
    ink < 0.7 || dy.abs() > 3.0 || (scale - 1.0).abs() > 0.01 || blur > 0.1
}

/// How live text moves, set on the element.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveMotion {
    pub caret: Option<Caret>,
    pub revise: Revise,
    pub rewrap: Rewrap,
    pub case: CaseChange,
    pub settle: Option<SettleInk>,
    /// The words the recogniser marks stable, when it says. With a list, a word not on it
    /// holds its arrival ink until it is; without one, every word settles by its age.
    pub stable: Option<Vec<WordId>>,
    /// The active-line reading stage (T14).
    pub reading: Option<Reading>,
    /// Width escrow, correction hinge and revision address (MO-21 to MO-23).
    pub revisions: revisions::RevisionMotion,
    /// New letters drop into the line as small rigid bodies (MO-83).
    pub letters: Option<letter_physics::LetterPhysics>,
    /// The caret as a magnet that pulls new words in, with its field drawn (MO-89).
    pub magnet: Option<magnet::Magnet>,
    /// Words set in a font of their own, such as a variable font instance (MO-81).
    pub word_fonts: Vec<(WordId, FontId)>,
    /// How the top line leaves when a line cap drops it (MO-33).
    pub overflow: overflow::Overflow,
    /// Draws glyphs on the newest line that break the reading-line envelope in this colour
    /// (`Element::envelope_tint`, MO-08).
    pub envelope_tint: Option<crate::geom::Color>,
    /// How a new word's enter transition is applied: inside the reading-line envelope by
    /// default (`Element::word_arrival`, `crate::arrival`).
    pub arrival: crate::arrival::Arrival,
    /// A mark beside the newest line that splits when the text wraps (MO-32).
    pub margin: Option<margin::MarginMark>,
    /// A record in the top margin of each line that left from the front (MO-34).
    pub spine: Option<spine::Spine>,
    /// The newest line tightens as it nears the edge and relaxes on the wrap (MO-31).
    pub bell: Option<bell::Bell>,
    /// Per-word typographic channels (MO-03).
    pub typo: Option<crate::typo::Typos>,
    /// Words whose glyphs something else draws for now, such as a GPU effect that morphs a
    /// revised word (`Element::words_drawn_elsewhere`). They keep their place in the line.
    pub elsewhere: Vec<WordId>,
}

/// The byte ranges of `words` joined by spaces that `fonts` sets in a font of their own, in
/// order: what `TextLayout::layout_fonts` takes for live text.
pub(crate) fn word_font_ranges(words: &[Word], fonts: &[(WordId, FontId)]) -> Vec<(std::ops::Range<usize>, FontId)> {
    if fonts.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut at = 0;
    for w in words {
        if let Some(&(_, f)) = fonts.iter().find(|(id, _)| *id == w.id) {
            out.push((at..at + w.text.len(), f));
        }
        at += w.text.len() + 1;
    }
    out
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
    /// Width of the text box, for the margin bell.
    pub width: f32,
    /// Words whose enter fade still runs. A letter that joins such a word comes in at full
    /// ink of its own, so it rides the word's fade with the letters already there instead
    /// of starting a fade of its own on top (RD-04).
    pub entering: Vec<WordId>,
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
    /// Settling ink: opacity factor and extra letter spacing in ems.
    ink: Channel,
    track: Channel,
    /// How new the word still looks, from 1 on arrival to 0 settled: the accent's share.
    fresh: Channel,
    /// How many times the text changed.
    revisions: u32,
    /// Reading stage: vertical roll of the word's line, and its line's look.
    roll: Channel,
    look_scale: Channel,
    look_opacity: Channel,
}

/// A glyph's own motion: where it sits in its word, and its enter or change.
struct FlowGlyph {
    glyph: u32,
    /// The face it is in (a fallback face for characters the style's font lacks).
    font: FontId,
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
    /// When the glyph came in, for letter physics.
    born: f64,
}

/// A glyph on its way out: an old glyph of a revised word, or a glyph of a removed word.
struct Ghost {
    glyph: u32,
    /// The face it is in (a fallback face for characters the style's font lacks).
    font: FontId,
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
    /// Extra travel along the line, for a displaced ghost that centres over its word.
    slide: Channel,
}

/// One glyph as the new layout placed it.
#[derive(Clone, Copy)]
struct Placed {
    /// Index in the shaped glyphs.
    index: usize,
    glyph: u32,
    /// The face it is in (a fallback face for characters the style's font lacks).
    font: FontId,
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
    /// The settling accent and its share, while the word is new.
    pub tint: Option<(Color, f32)>,
}

impl GlyphMotion {
    pub const NONE: GlyphMotion = GlyphMotion { dx: 0.0, dy: 0.0, opacity: 1.0, scale: 1.0, tint: None };

    /// `base` with the settling accent mixed in; its alpha stays.
    pub fn tint(&self, base: Color) -> Color {
        match self.tint {
            Some((c, k)) if k > 0.0 => Color {
                r: base.r + (c.r - base.r) * k,
                g: base.g + (c.g - base.g) * k,
                b: base.b + (c.b - base.b) * k,
                a: base.a,
            },
            _ => base,
        }
    }
}

/// A ghost glyph to draw at `t`, in the text box's space.
pub(crate) struct GhostGlyph {
    pub word: WordId,
    pub glyph: u32,
    pub font: FontId,
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

/// When words left from the front and the kept words moved up together (a line cap
/// dropping the oldest words), how far down the block was: the vertical move most kept
/// words share with no move along their line. `None` for any other change.
fn front_scroll(old: &[FlowWord], words: &[Word], placed: &[Option<PlacedWord>]) -> Option<f32> {
    let first = old.iter().position(|o| words.iter().any(|w| w.id == o.id))?;
    if first == 0 {
        return None;
    }
    // Votes per move, in half pixels.
    let mut votes: Vec<(i32, u32)> = Vec::new();
    for (w, p) in words.iter().zip(placed) {
        let (Some(p), Some(o)) = (p, old.iter().find(|o| o.id == w.id)) else { continue };
        let (dx, dy) = (o.x.target() as f32 - p.x, o.y.target() as f32 - p.y);
        if dx.abs() >= 0.5 || dy.abs() < 0.5 {
            continue;
        }
        let key = (dy * 2.0).round() as i32;
        match votes.iter_mut().find(|v| v.0 == key) {
            Some(v) => v.1 += 1,
            None => votes.push((key, 1)),
        }
    }
    let (key, _) = votes.into_iter().max_by_key(|v| v.1)?;
    Some(key as f32 / 2.0)
}

/// Whether kept word `o` moved with the scrolling block: only `dy` down from its new place.
/// On a row in `whole` the line may also have shifted along (a "…" put on the new first word,
/// a centred line), and the word then slides along its row while it scrolls, instead of
/// fading out where it was.
fn in_block(o: &FlowWord, p: &PlacedWord, dy: f32, whole: &[u32]) -> bool {
    ((o.x.target() as f32 - p.x).abs() < 0.5 || whole.contains(&p.line)) && (o.y.target() as f32 - p.y - dy).abs() < 1.0
}

/// A rewrapped word shows once the words still sliding along its new line have left its
/// place, so text never draws over text: the words a reflow pushes along a line start
/// where the newcomer lands. On the newest line it then shows whole, in the frame its old
/// copy goes (reading-line envelope); on older lines it fades in. A word whose place is
/// clear at once keeps its fade. A newest-line word lands at rest, so it also waits for
/// the line scrolling up out of its row (within 0.9 `line_height`). Returns when each
/// newest-line word shows.
fn show_when_clear(
    next: &mut [FlowWord],
    rewrapped: &[usize],
    newest: u32,
    line_height: f32,
    make: &Make,
) -> Vec<(WordId, f64)> {
    const STEP: f64 = 1.0 / 240.0;
    let t = make.t;
    let mut shown = Vec::new();
    for &s in rewrapped {
        let (line, a) = (next[s].line, next[s].x.target());
        let b = a + next[s].width.target();
        let row = next[s].y.target() + next[s].roll.target();
        let near = 0.9 * line_height as f64;
        let busy = |at: f64| {
            next.iter().enumerate().any(|(i, o)| {
                let x = o.x.value(at);
                let in_row =
                    o.line == line || (line == newest && (o.y.value(at) + o.roll.value(at) - row).abs() < near);
                i != s && in_row && !rewrapped.contains(&i) && x < b && x + o.width.value(at) > a
            })
        };
        let mut at = t;
        while at < t + REWRAP_WAIT_MAX && busy(at) {
            at += STEP;
        }
        if line == newest {
            shown.push((next[s].id, at));
        }
        if at == t {
            continue;
        }
        for g in &mut next[s].glyphs {
            if line == newest {
                g.opacity = make.ch(0.0, overflow::DROP_STEP, Rest::UNIT);
                g.opacity.snap(at, 1.0);
            } else {
                g.opacity = make.ch(0.0, CROSSFADE, Rest::UNIT);
                g.opacity.retarget(at, 1.0);
            }
        }
    }
    shown
}

/// The new lines that hold the same words as one old line, no more and no fewer: rows that
/// moved as a whole, so their words cannot slide over a word that rewrapped onto them.
fn whole_rows(old: &[FlowWord], words: &[Word], placed: &[Option<PlacedWord>]) -> Vec<u32> {
    // (new line, the old line all its words came from, how many words)
    let mut rows: Vec<(u32, Option<u32>, usize)> = Vec::new();
    for (w, p) in words.iter().zip(placed) {
        let Some(p) = p else { continue };
        let was = old.iter().find(|o| o.id == w.id).map(|o| o.line);
        match rows.iter_mut().find(|r| r.0 == p.line) {
            Some(r) => {
                if r.1 != was {
                    r.1 = None;
                }
                r.2 += 1;
            }
            None => rows.push((p.line, was, 1)),
        }
    }
    rows.into_iter()
        .filter(|&(_, was, n)| was.is_some_and(|l| old.iter().filter(|o| o.line == l).count() == n))
        .map(|r| r.0)
        .collect()
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
            font: g.font,
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
            font: p.font,
            grapheme,
            advance: p.advance,
            x: self.ch(p.x, FLIP, Rest::PIXELS),
            y: p.y,
            opacity: self.ch(1.0, CROSSFADE, Rest::UNIT),
            rise: self.ch(0.0, GLYPH_ROLL, Rest::PIXELS),
            scale: self.ch(1.0, GLYPH_POP, Rest::UNIT),
            born: self.t,
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
        let FlowInput { shaped, words, motion, size, line_height, retention, reduced, crossfade, width, entering } = input;
        self.size = size;
        let revise = if crossfade { motion.revise } else { Revise::Swap };
        let make = Make { t, retention, size };
        let placed = place(shaped, words);
        let stage = StageTargets::new(&placed, motion.reading.as_ref(), size);
        self.spans = stage.spans.clone();
        (self.origin, self.staged) = (motion.reading.as_ref().map_or(0.0, |r| r.origin), motion.reading.is_some());
        self.line_height = line_height;
        self.accent = motion.settle.and_then(|s| s.accent);
        self.letters = if reduced { None } else { motion.letters };
        (self.magnet, self.magnet_still) = (motion.magnet, reduced || motion.magnet.is_some_and(|m| m.still));
        let mut old = std::mem::take(&mut self.words);
        let scroll = front_scroll(&old, words, &placed);
        let whole = if scroll.is_some() { whole_rows(&old, words, &placed) } else { Vec::new() };
        let front_drop = old.iter().position(|o| words.iter().any(|w| w.id == o.id)).is_some_and(|i| i > 0);
        // Words that left from the front, which scroll out with the rest.
        let front: Vec<WordId> = match scroll {
            Some(_) => old.iter().map(|o| o.id).take_while(|id| words.iter().all(|w| w.id != *id)).collect(),
            None => Vec::new(),
        };
        self.update_spine(t, motion.spine, &old, &front, words.is_empty(), reduced);
        self.start_marks(t, &old, words, &placed, motion, reduced);
        // Width escrow (MO-22): words that move along their line wait for a quiet moment.
        let escrow = motion.revisions.escrow && revisions::escrow_holds(&old, words, &placed, size);
        let hold = if escrow { t + revisions::ESCROW_QUIET } else { t };
        let mut next: Vec<FlowWord> = Vec::with_capacity(words.len());
        let mut index = vec![None; shaped.glyphs.len()];
        let (mut stagger, mut arrived) = (0u32, false);
        // Slots in `next` of words that faded off one line onto another.
        let mut rewrapped: Vec<usize> = Vec::new();
        // The words that faded off a line, with that line and their span on it.
        let mut left: Vec<(WordId, u32, f64, f64)> = Vec::new();
        // Words that rewrapped onto the newest line, with where their old copies start in
        // `self.ghosts`: the old copy goes in the frame the new one shows.
        let mut handed: Vec<(WordId, usize)> = Vec::new();
        // How far the last kept word's line moved, `(line, from old x to new x)`. A new word
        // on that line starts as far back and moves with it, so a line that shifts (a centred
        // line that grows, words after a revised one) slides as one piece instead of the old
        // words sliding over the new one.
        let mut line_shift: Option<(u32, f32)> = None;
        for (w, p) in words.iter().zip(placed) {
            let prev = old.iter().position(|o| o.id == w.id).map(|i| old.remove(i));
            let Some(p) = p else {
                if let Some(o) = prev {
                    self.ghost_word(&o, t, retention);
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
                let back = match line_shift {
                    Some((line, dx)) if line == p.line && !reduced => dx,
                    _ => 0.0,
                };
                let mut glyphs: Vec<FlowGlyph> =
                    p.glyphs.iter().zip(&graphemes).map(|(g, &gr)| make.rest(g, gr)).collect();
                // A word inserted before kept words shows once they have moved out of its way.
                let after = words.iter().skip_while(|v| v.id != w.id).skip(1);
                let inserted = !reduced && after.into_iter().any(|v| old.iter().any(|o| o.id == v.id));
                if inserted {
                    for g in &mut glyphs {
                        g.opacity = make.ch(0.0, CROSSFADE, Rest::UNIT);
                        g.opacity.retarget(t + INSERT_DELAY, 1.0);
                    }
                }
                // A word on the newest line of a front drop shows in place, whole, once the
                // line before has scrolled out of its row, so text never crosses text.
                let dropped = scroll.is_some() && !reduced && p.line == stage.newest;
                if dropped {
                    self.drop_hold = Some((p.line, t + overflow::DROP_HOLD));
                    for g in &mut glyphs {
                        g.opacity = make.ch(0.0, overflow::DROP_STEP, Rest::UNIT);
                        g.opacity.snap(t + overflow::DROP_HOLD, 1.0);
                    }
                }
                // A word that joins, in a later update, a line whose words are still held
                // back waits for them, so the line never shows its later words first.
                let held = self.drop_hold.filter(|&(line, until)| line == p.line && until > t);
                if let Some((_, until)) = held.filter(|_| !reduced && !dropped) {
                    for g in &mut glyphs {
                        g.opacity = make.ch(0.0, overflow::DROP_STEP, Rest::UNIT);
                        g.opacity.snap(until, 1.0);
                    }
                }
                next.push(FlowWord {
                    id: w.id,
                    text: w.text.clone(),
                    x: if hold > t {
                        let mut c = make.ch(p.x - back, FLIP, Rest::PIXELS);
                        c.retarget(hold, p.x as f64);
                        c
                    } else {
                        make.to(p.x - back, p.x, FLIP, Rest::PIXELS)
                    },
                    // A word that arrives with a scroll comes up with the block, except on
                    // the newest line, which is read where it stands (reading-line envelope).
                    y: match scroll {
                        Some(dy) if !reduced && p.line != stage.newest => make.to(p.y + dy, p.y, SCROLL, Rest::PIXELS),
                        _ => make.ch(p.y, FLIP, Rest::PIXELS),
                    },
                    width: make.ch(p.width, FLIP, Rest::PIXELS),
                    spread: false,
                    line: p.line,
                    glyphs,
                    ink: make.ch(1.0, CROSSFADE, Rest::UNIT),
                    track: make.ch(0.0, CROSSFADE, Rest::UNIT),
                    fresh: make.ch(0.0, CROSSFADE, Rest::UNIT),
                    revisions: 0,
                    roll: make.ch(stage.roll, FLIP, Rest::PIXELS),
                    look_scale: make.ch(stage.look(p.line).scale, FLIP, Rest::UNIT),
                    look_opacity: make.ch(stage.look(p.line).opacity, FLIP, Rest::UNIT),
                });
                if let Some(o) = next.last_mut() {
                    settle(o, true, motion, &make, reduced);
                }
                continue;
            };
            let revised = o.text != w.text;
            if revised {
                o.revisions += 1;
                arrived = true;
                let joins = entering.contains(&w.id);
                let diffed =
                    revise == Revise::Glyphs && self.revise_glyphs(&mut o, w, &p, &graphemes, &make, motion, joins);
                if !diffed {
                    match revise {
                        Revise::Swap => {
                            o.glyphs = p.glyphs.iter().zip(&graphemes).map(|(g, &gr)| make.rest(g, gr)).collect();
                            o.spread = false;
                        }
                        Revise::Palimpsest => self.palimpsest(&mut o, w, &p, &graphemes, &make, reduced, joins),
                        Revise::Liquid => self.revise_liquid(&mut o, w, &p, &graphemes, &make, reduced, joins),
                        Revise::Roll => self.roll(&mut o, &p, &graphemes, &make, reduced),
                        Revise::Ghost => self.displace(&mut o, &p, &graphemes, &make, reduced),
                        Revise::Crossfade | Revise::Glyphs => self.crossfade(&mut o, &p, &graphemes, &make),
                    }
                }
                o.text = w.text.clone();
            } else if o.glyphs.len() == p.glyphs.len() {
                for ((g, n), &gr) in o.glyphs.iter_mut().zip(&p.glyphs).zip(&graphemes) {
                    move_to(&mut g.x, n.x, t, FLIP, reduced);
                    (g.glyph, g.font, g.y, g.advance, g.grapheme) = (n.glyph, n.font, n.y, n.advance, gr);
                }
            } else {
                o.glyphs = p.glyphs.iter().zip(&graphemes).map(|(g, &gr)| make.rest(g, gr)).collect();
                o.spread = false;
            }
            let shifted = line_shift;
            line_shift = (o.line == p.line).then(|| (p.line, p.x - o.x.target() as f32));
            let look = stage.look(p.line);
            if let Some(dy) = scroll.filter(|&dy| in_block(&o, &p, dy, &whole)) {
                // Part of the block: every such word starts the same distance below its new
                // place and takes the same spring, so the text scrolls as one piece.
                let lag = |c: &Channel| if reduced { 0.0 } else { (c.value(t) - c.target()) as f32 };
                let dy = if reduced { 0.0 } else { dy };
                let dx = if reduced { 0.0 } else { o.x.target() as f32 - p.x };
                o.x = make.to(p.x + dx + lag(&o.x), p.x, SCROLL, Rest::PIXELS);
                o.y = make.to(p.y + dy + lag(&o.y), p.y, SCROLL, Rest::PIXELS);
                move_to(&mut o.roll, stage.roll, t, stage.travel, reduced);
            } else if o.line != p.line && (front_drop || motion.rewrap == Rewrap::Fade) {
                // A gliding word would cut across the lines it passes, so the word fades out
                // where it is and in at its place,
                // sliding there with the words before it on its new line. On the newest
                // line the reader is on the word: it hands over in one frame instead, the
                // old copy gone as the new one shows whole, so it is never drawn
                // twice and never missing.
                let newest = p.line == stage.newest;
                if newest {
                    handed.push((o.id, self.ghosts.len()));
                }
                self.ghost_word(&o, t, retention);
                if !reduced && !front_drop && scroll.is_none() {
                    let a = o.x.value(t);
                    left.push((o.id, o.line, a, a + o.width.value(t)));
                }
                let back = match shifted {
                    Some((line, dx)) if line == p.line && !reduced => dx,
                    _ => 0.0,
                };
                o.x = make.to(p.x - back, p.x, FLIP, Rest::PIXELS);
                line_shift = Some((p.line, back));
                o.y = make.ch(p.y, REWRAP, Rest::PIXELS);
                let from = if newest { 1.0 } else { 0.0 };
                for g in &mut o.glyphs {
                    g.opacity = make.to(from, 1.0, CROSSFADE, Rest::UNIT);
                }
                if !reduced {
                    rewrapped.push(slot as usize);
                }
                // On the newest line it lands at rest, where it is read, not riding the roll.
                move_to(&mut o.roll, stage.roll, t, stage.travel, reduced || newest);
            } else if o.line != p.line {
                let at = t + REWRAP_STAGGER * stagger as f64;
                stagger += 1;
                move_to(&mut o.x, p.x, at, REWRAP, reduced);
                move_to(&mut o.y, p.y, at, REWRAP, reduced);
                // The roll moves with the rewrap, so a word that drops onto the new line
                // slides along the stage instead of jumping a line.
                move_to(&mut o.roll, stage.roll, at, REWRAP, reduced);
            } else {
                revisions::move_after(&mut o.x, p.x, t, hold, FLIP, reduced);
                move_to(&mut o.y, p.y, t, FLIP, reduced);
                move_to(&mut o.roll, stage.roll, t, stage.travel, reduced);
            }
            move_to(&mut o.look_scale, look.scale, t, stage.travel, reduced);
            move_to(&mut o.look_opacity, look.opacity, t, stage.travel, reduced);
            move_to(&mut o.width, p.width, t, FLIP, reduced);
            o.line = p.line;
            settle(&mut o, revised, motion, &make, reduced);
            next.push(o);
        }
        for o in old {
            arrived = true;
            let id = o.id;
            if motion.overflow != overflow::Overflow::Scroll && !reduced && front.contains(&id) {
                self.leave(&o, t, motion.overflow, scroll.unwrap_or(0.0));
                continue;
            }
            self.ghost_word(&o, t, retention);
            if let Some(dy) = scroll.filter(|_| front.contains(&id) && !reduced) {
                for g in self.ghosts.iter_mut().filter(|g| g.word == id) {
                    let rise = g.rise.value(t) as f32;
                    g.rise = make.to(rise, rise - dy, SCROLL, Rest::PIXELS);
                }
            }
        }
        let shown = show_when_clear(&mut next, &rewrapped, stage.newest, line_height, &make);
        for &(id, from) in &handed {
            let at = shown.iter().find(|s| s.0 == id).map_or(t, |s| s.1);
            for g in self.ghosts[from..].iter_mut().filter(|g| g.word == id) {
                let k = g.opacity.value(t);
                g.opacity.snap(t, k);
                g.opacity.snap(at, 0.0);
            }
        }
        self.clear_ghosts(&next, &rewrapped, &left, t);
        self.words = next;
        self.index = index;
        self.update_margin(t, motion.margin, reduced);
        self.update_bell(t, motion.bell, width, reduced);
        self.ghosts.retain(|g| g.opacity.target() > 0.0 || g.opacity.settled_at() > t);
        self.liquid.retain(|l| l.start + liquid::LIQUID_SECS > t && self.words.iter().any(|w| w.id == l.id));
        self.leaving.retain(|l| l.done_at() > t);
        if arrived {
            self.arrived = t;
        }
        let glide = if scroll.is_some() { SCROLL } else { CARET_GLIDE };
        self.update_caret(t, motion.caret, glide, retention, reduced);
        if hold > t {
            // The caret waits with the words it follows.
            if let Some((cx, ..)) = &mut self.caret {
                let (to, now) = (cx.target(), cx.value(t));
                revisions::cancel_pending(cx, t);
                cx.retarget(t, now);
                cx.retarget(hold, to);
            }
        }
    }

    /// FX-065: the old glyphs roll up and fade, the new ones roll in from below and fade in,
    /// and both spread with the word's width. Under reduced motion only the fades stay.
    fn roll(&mut self, o: &mut FlowWord, p: &PlacedWord, graphemes: &[u32], make: &Make, reduced: bool) {
        let t = make.t;
        let travel = if reduced { 0.0 } else { ROLL_WORD_EMS * make.size };
        let (s, width) = (stretch(o, t), o.width.value(t) as f32);
        for g in &o.glyphs {
            let k = g.opacity.value(t) as f32;
            if k > 0.0 {
                self.ghosts.push(Ghost {
                    glyph: g.glyph,
                    font: g.font,
                    word: o.id,
                    advance: g.advance,
                    fixed: None,
                    x: g.x.value(t) as f32 * s,
                    y: g.y,
                    width,
                    opacity: make.to(k, 0.0, WORD_ROLL, Rest::UNIT),
                    rise: make.to(g.rise.value(t) as f32, g.rise.value(t) as f32 - travel, WORD_ROLL, Rest::PIXELS),
                    scale: make.ch(g.scale.value(t) as f32, GLYPH_POP, Rest::UNIT),
                    slide: make.ch(0.0, WORD_ROLL, Rest::PIXELS),
                });
            }
        }
        o.glyphs = p
            .glyphs
            .iter()
            .zip(graphemes)
            .map(|(g, &gr)| FlowGlyph {
                // Under reduced motion nothing rolls: the new word crossfades in place, so
                // it starts readable, at the palimpsest's arrival ink.
                opacity: make.to(if reduced { palimpsest::ARRIVE_INK } else { 0.0 }, 1.0, WORD_ROLL, Rest::UNIT),
                rise: make.to(travel, 0.0, WORD_ROLL, Rest::PIXELS),
                ..make.rest(g, gr)
            })
            .collect();
        o.spread = true;
    }

    /// FX-064: the old word shrinks to `GHOST_SCALE` and floats `GHOST_LIFT_EMS` above the
    /// new one, centred over it, at `GHOST_ALPHA`; the new word fades in where it is, and the
    /// line opens by `GHOST_OPEN_EMS`. After `GHOST_HOLD` the ghost fades and the line closes.
    fn displace(&mut self, o: &mut FlowWord, p: &PlacedWord, graphemes: &[u32], make: &Make, reduced: bool) {
        let t = make.t;
        let s = stretch(o, t);
        let old_width = o.width.value(t) as f32;
        // The shrunk old word's left edge, from the new word's pen position.
        let left = 0.5 * (p.width - old_width * GHOST_SCALE);
        let lift = -GHOST_LIFT_EMS * make.size;
        for g in &o.glyphs {
            let k = g.opacity.value(t) as f32;
            if k <= 0.0 {
                continue;
            }
            let x = g.x.value(t) as f32 * s;
            // Where the glyph's pen goes, less what the scale's own centring adds.
            let to = left + x * GHOST_SCALE - x - 0.5 * g.advance * (1.0 - GHOST_SCALE);
            let mut opacity = make.to(k, GHOST_ALPHA * k, GHOST_MOVE, Rest::UNIT);
            opacity.retarget(t + GHOST_HOLD, 0.0);
            let rise0 = g.rise.value(t) as f32;
            let move_ = |from: f32, to: f32, rest| {
                if reduced {
                    make.ch(to, GHOST_MOVE, rest)
                } else {
                    make.to(from, to, GHOST_MOVE, rest)
                }
            };
            self.ghosts.push(Ghost {
                glyph: g.glyph,
                font: g.font,
                word: o.id,
                advance: g.advance,
                fixed: None,
                x,
                y: g.y,
                width: 0.0,
                opacity,
                rise: move_(rise0, lift, Rest::PIXELS),
                scale: move_(g.scale.value(t) as f32, GHOST_SCALE, Rest::UNIT),
                slide: move_(0.0, to, Rest::PIXELS),
            });
        }
        o.glyphs = p
            .glyphs
            .iter()
            .zip(graphemes)
            .map(|(g, &gr)| FlowGlyph { opacity: make.to(0.0, 1.0, CROSSFADE, Rest::UNIT), ..make.rest(g, gr) })
            .collect();
        o.spread = false;
        let open = GHOST_OPEN_EMS * make.size;
        let mut ch = if reduced {
            make.ch(open, GHOST_MOVE, Rest::PIXELS)
        } else {
            make.to(0.0, open, GHOST_MOVE, Rest::PIXELS)
        };
        ch.retarget(t + GHOST_HOLD, 0.0);
        self.open.retain(|(_, c)| c.target() > 0.0 || c.settled_at() > t);
        self.open.push((p.line, ch));
    }

    /// How far the lines open above line `line` at `t`, from displaced ghosts on it or above.
    fn opened(&self, line: u32, t: f64) -> f32 {
        self.open.iter().filter(|(l, _)| *l <= line).map(|(_, c)| c.value(t).max(0.0) as f32).sum()
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
                    font: g.font,
                    word: o.id,
                    advance: g.advance,
                    fixed: None,
                    x: g.x.value(t) as f32 * s,
                    y: g.y,
                    width,
                    opacity: make.to(k, 0.0, CROSSFADE, Rest::UNIT),
                    rise: make.ch(g.rise.value(t) as f32, GLYPH_ROLL, Rest::PIXELS),
                    scale: make.ch(g.scale.value(t) as f32, GLYPH_POP, Rest::UNIT),
                    slide: make.ch(0.0, GLYPH_ROLL, Rest::PIXELS),
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
    #[allow(clippy::too_many_arguments)]
    fn revise_glyphs(
        &mut self,
        o: &mut FlowWord,
        w: &Word,
        p: &PlacedWord,
        graphemes: &[u32],
        make: &Make,
        motion: &LiveMotion,
        joins: bool,
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
                font: g.font,
                word,
                advance: g.advance,
                fixed: None,
                x: g.x.value(t) as f32 * s,
                y: g.y,
                width: 0.0,
                opacity: make.to(k, 0.0, spring, Rest::UNIT),
                rise: make.to(g.rise.value(t) as f32, to.0 * size, spring, Rest::PIXELS),
                scale: make.to(g.scale.value(t) as f32, to.1, spring, Rest::UNIT),
                slide: make.ch(0.0, spring, Rest::PIXELS),
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
                        FlowGlyph { glyph: n.glyph, font: n.font, grapheme: gr, advance: n.advance, y: n.y, ..g }
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
                // A letter that joins a word still fading in takes the word's fade (RD-04).
                GraphemeOp::Enter => make.enter(n, gr, (if joins { 1.0 } else { 0.0 }, RISE_EMS, 1.0), GLYPH_ROLL),
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

    /// Hands a word that left from the front to its leaving line (MO-33), as drawn at `t`.
    fn leave(&mut self, o: &FlowWord, t: f64, style: overflow::Overflow, dy: f32) {
        let i = match self.leaving.iter().position(|l| l.is(t, o.line)) {
            Some(i) => i,
            None => {
                self.leaving.push(overflow::Leaving::new(style, t, o.line, dy, self.size, self.line_height));
                self.leaving.len() - 1
            }
        };
        let s = stretch(o, t);
        let y = o.y.value(t) as f32 + self.opened(o.line, t);
        for g in &o.glyphs {
            let scale = g.scale.value(t).max(0.0) as f32;
            let x = o.x.value(t) as f32 + g.x.value(t) as f32 * s + 0.5 * g.advance * (1.0 - scale);
            let (pos, look) = self.on_stage(o, Point::new(x, y + g.y + g.rise.value(t) as f32), t);
            let opacity = (g.opacity.value(t) * o.ink.value(t) * o.look_opacity.value(t)).clamp(0.0, 1.0) as f32;
            let glyph =
                overflow::LeavingGlyph { word: o.id, glyph: g.glyph, font: g.font, pos, scale: scale * look, opacity };
            self.leaving[i].push(glyph, g.advance);
        }
    }

    /// Lines on their way out (MO-33).
    pub(crate) fn leaving(&self) -> &[overflow::Leaving] {
        &self.leaving
    }

    /// Fades out every glyph of a word that left the screen, where it is at `t`.
    /// The ghost of a word that faded off a line is gone by the time the words sliding
    /// along that line reach its place, so they never draw over it.
    fn clear_ghosts(&mut self, next: &[FlowWord], rewrapped: &[usize], left: &[(WordId, u32, f64, f64)], t: f64) {
        for &(id, line, a, b) in left {
            let reached = |at: f64| {
                next.iter().enumerate().any(|(i, o)| {
                    let x = o.x.value(at);
                    o.line == line && !rewrapped.contains(&i) && x < b && x + o.width.value(at) > a
                })
            };
            let mut at = t;
            while at < t + REWRAP_WAIT_MAX && !reached(at) {
                at += 1.0 / 240.0;
            }
            if at < t + REWRAP_WAIT_MAX {
                for g in self.ghosts.iter_mut().filter(|g| g.word == id) {
                    g.opacity.snap(at, 0.0);
                }
            }
        }
    }

    fn ghost_word(&mut self, o: &FlowWord, t: f64, retention: f64) {
        let at = (o.x.value(t) as f32, o.y.value(t) as f32 + o.roll.value(t) as f32);
        for g in self.ghosts.iter_mut().filter(|g| g.word == o.id && g.fixed.is_none()) {
            let k = if g.width > 0.0 { o.width.value(t) as f32 / g.width } else { 1.0 };
            g.x *= k;
            g.fixed = Some(at);
        }
        let s = stretch(o, t);
        for g in &o.glyphs {
            let k = g.opacity.value(t) * o.look_opacity.value(t);
            if k > 0.0 {
                let ch = |v: f64, spring: Spring, rest: Rest| chan(t, v, spring, rest, retention);
                let mut opacity = ch(k, CROSSFADE, Rest::UNIT);
                opacity.retarget(t, 0.0);
                self.ghosts.push(Ghost {
                    glyph: g.glyph,
                    font: g.font,
                    word: o.id,
                    advance: g.advance,
                    fixed: Some(at),
                    x: g.x.value(t) as f32 * s,
                    y: g.y,
                    width: 0.0,
                    opacity,
                    rise: ch(g.rise.value(t), GLYPH_ROLL, Rest::PIXELS),
                    scale: ch(g.scale.value(t), GLYPH_POP, Rest::UNIT),
                    slide: ch(0.0, GLYPH_ROLL, Rest::PIXELS),
                });
            }
        }
    }

    fn update_caret(&mut self, t: f64, caret: Option<Caret>, glide: Spring, retention: f64, reduced: bool) {
        let (size, line_height) = (self.size, self.line_height);
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
                move_to(cx, x, t, glide, reduced);
                move_to(cy, y, t, glide, reduced);
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
        let Some(Some((w, gi))) = self.index.get(i) else { return GlyphMotion::NONE };
        let w = &self.words[*w as usize];
        let g = &w.glyphs[*gi as usize];
        let off = |c: &Channel| (c.value(t) - c.target()) as f32;
        let scale = g.scale.value(t).max(0.0) as f32;
        // Tracking spreads the letters about the word's middle, so the word stays in place.
        let middle = 0.5 * (w.glyphs.len() as f32 - 1.0);
        let track = w.track.value(t) as f32 * self.size * (*gi as f32 - middle);
        let centre = 0.5 * g.advance * (1.0 - scale);
        let dx = off(&w.x) + g.x.value(t) as f32 * stretch(w, t) - g.x.target() as f32 + centre + track;
        let mut dy = off(&w.y) + g.rise.value(t) as f32 + self.opened(w.line, t);
        let mut dx = dx;
        if let Some(l) = self.letter(w, *gi as usize, t) {
            (dx, dy) = (dx + l.dx, dy + l.dy);
        }
        dx += self.magnet_snap(w, t);
        dx += self.bell_dx(w, w.x.target() as f32 + g.x.target() as f32, t);
        let pen = Point::new(w.x.target() as f32 + g.x.target() as f32 + dx, w.y.target() as f32 + g.y + dy);
        let (pos, look) = self.on_stage(w, pen, t);
        GlyphMotion {
            dx: pos.x - pen.x + dx,
            dy: pos.y - pen.y + dy,
            opacity: (g.opacity.value(t) * w.ink.value(t) * w.look_opacity.value(t)).clamp(0.0, 1.0) as f32,
            scale: scale * look,
            tint: self.accent.map(|c| (c, w.fresh.value(t).clamp(0.0, 1.0) as f32)),
        }
    }

    /// Where the reading stage puts a pen position of word `w` at `t`: scaled about its
    /// line's origin by the line's look, and rolled. Returns the place and the scale.
    fn on_stage(&self, w: &FlowWord, pen: Point, t: f64) -> (Point, f32) {
        let s = w.look_scale.value(t) as f32;
        let roll = w.roll.value(t) as f32;
        let ox = self.spans.get(w.line as usize).map_or(0.0, |sp| sp.0 + self.origin * (sp.1 - sp.0));
        let oy = w.y.value(t) as f32 - 0.28 * self.size;
        (Point::new(ox + (pen.x - ox) * s, oy + (pen.y - oy) * s + roll), s)
    }

    /// Whether word `id` is on the newest line, where the reading-line envelope holds. True
    /// before the first layout.
    pub(crate) fn on_reading_line(&self, id: WordId) -> bool {
        let Some(last) = self.words.last() else { return true };
        self.words.iter().find(|w| w.id == id).is_none_or(|w| w.line == last.line)
    }

    /// The newest line's box at `t` in the text box's space, when the text has a reading
    /// stage: where a host can centre the line being read.
    pub fn reading_line(&self, t: f64) -> Option<Rect> {
        let w = self.words.last()?;
        if self.spans.is_empty() || !self.staged {
            return None;
        }
        let (x0, x1, _) = self.spans[w.line as usize];
        let centre = w.y.value(t) as f32 - 0.28 * self.size + w.roll.value(t) as f32;
        Some(Rect::new(x0, centre - 0.5 * self.line_height, x1 - x0, self.line_height))
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
                    let y = w.y.value(t) as f32 + w.roll.value(t) as f32 + self.opened(w.line, t);
                    (w.x.value(t) as f32, y, g.x * k)
                }
            };
            let scale = g.scale.value(t).max(0.0) as f32;
            let x = x + gx + g.slide.value(t) as f32 + 0.5 * g.advance * (1.0 - scale);
            let pos = Point::new(x, y + g.y + g.rise.value(t) as f32);
            Some(GhostGlyph { word: g.word, glyph: g.glyph, font: g.font, pos, opacity, scale })
        })
    }

    /// A word's box at `t` in the text box's space, from its sprung position and width:
    /// what decorations and the caret follow.
    pub fn word_bounds(&self, id: WordId, t: f64, size: f32) -> Option<Rect> {
        let w = self.words.iter().find(|w| w.id == id)?;
        let (x, y) = (w.x.value(t) as f32, w.y.value(t) as f32 + w.roll.value(t) as f32 + self.opened(w.line, t));
        Some(Rect::new(x, y - 0.78 * size, w.width.value(t).max(0.0) as f32, size))
    }

    /// The caret bar at `t` in the text box's space, its colour if set, and its opacity.
    pub fn caret(&self, t: f64) -> Option<(Rect, Option<Color>, f32)> {
        let (x, y, style, [w, up, down]) = self.caret.as_ref()?;
        let alpha = caret_alpha(t - self.arrived);
        let roll = self.words.last().map_or(0.0, |w| w.roll.value(t) as f32 + self.opened(w.line, t));
        let (x, y) = (x.value(t) as f32, y.value(t) as f32 + roll);
        let x = x + self.bell_caret_dx(x, t);
        Some((Rect::new(x, y - up, *w, up + down), style.color, alpha))
    }

    /// The caret's style, if there is a caret.
    pub fn caret_style(&self) -> Option<&Caret> {
        self.caret.as_ref().map(|(_, _, style, _)| style)
    }

    /// Letter physics of glyph `gi` of word `w` at `t`, while it is still moving.
    fn letter(&self, w: &FlowWord, gi: usize, t: f64) -> Option<letter_physics::LetterState> {
        let p = self.letters.as_ref()?;
        let last = w.glyphs.iter().map(|g| g.born).fold(f64::NEG_INFINITY, f64::max);
        if t >= p.settled_at([last + p.stagger * w.glyphs.len() as f64]) {
            return None;
        }
        let starts = p.starts(w.glyphs.iter().map(|g| g.born));
        let newest = self.words.last().is_some_and(|l| l.line == w.line);
        Some(p.letter(&starts, gi, t, self.size, w.id, newest))
    }

    /// When the last letter that letter physics moves is locked in place.
    fn letters_settled_at(&self) -> f64 {
        let Some(p) = &self.letters else { return f64::NEG_INFINITY };
        let mut at = f64::NEG_INFINITY;
        for w in &self.words {
            let n = w.glyphs.len() as f64;
            for g in &w.glyphs {
                at = at.max(g.born + p.stagger * n + p.lock);
            }
        }
        at
    }

    /// Letters of live words in flight at `t`: each one's pen position in the text box's
    /// space, its speed, and the colour its motion vector draws in.
    pub fn letter_vectors(&self, t: f64) -> Vec<(Point, Point, Color)> {
        let Some(color) = self.letters.and_then(|p| p.vectors) else { return Vec::new() };
        let mut out = Vec::new();
        for w in &self.words {
            for (gi, g) in w.glyphs.iter().enumerate() {
                let Some(l) = self.letter(w, gi, t) else { break };
                if l.vx.hypot(l.vy) < 1.0 {
                    continue;
                }
                let pen = Point::new(
                    w.x.value(t) as f32 + g.x.value(t) as f32 + l.dx + 0.5 * g.advance,
                    w.y.value(t) as f32 + w.roll.value(t) as f32 + g.y + l.dy,
                );
                out.push((pen, Point::new(l.vx, l.vy), color));
            }
        }
        out
    }

    /// When every word, glyph, ghost and the caret's glide come to rest.
    pub fn settled_at(&self) -> f64 {
        let words = self.words.iter().flat_map(|w| {
            [&w.x, &w.y, &w.width, &w.ink, &w.track, &w.fresh, &w.roll, &w.look_scale, &w.look_opacity]
                .into_iter()
                .chain(w.glyphs.iter().flat_map(|g| [&g.x, &g.opacity, &g.rise, &g.scale]))
        });
        let ghosts = self.ghosts.iter().flat_map(|g| [&g.opacity, &g.rise, &g.scale, &g.slide]);
        let ghosts = ghosts.chain(self.open.iter().map(|(_, c)| c));
        let caret = self.caret.iter().flat_map(|(x, y, ..)| [x, y]);
        let extras = self.margin_settled_at().max(self.spine_settled_at()).max(self.bell_settled_at());
        let leaving = self.leaving.iter().map(|l| l.done_at()).fold(extras, f64::max);
        let leaving =
            leaving.max(self.letters_settled_at()).max(self.magnet_settled_at()).max(self.liquid_settled_at());
        words.chain(ghosts).chain(caret).map(|c| c.settled_at()).fold(leaving.max(self.marks.settled_at()), f64::max)
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

#[path = "motion/overflow.rs"]
pub mod overflow;

#[path = "motion/margin.rs"]
pub mod margin;

#[path = "motion/spine.rs"]
pub mod spine;

#[path = "motion/bell.rs"]
pub mod bell;

/// The live text motion state of one node.
#[derive(Default)]
pub(crate) struct Flow {
    /// In reading order, only words the layout drew.
    words: Vec<FlowWord>,
    /// For each glyph of the node's shaped text: `(word slot, glyph slot)`, or `None` for a
    /// glyph outside any word (an ellipsis).
    index: Vec<Option<(u32, u32)>>,
    ghosts: Vec<Ghost>,
    /// Words in liquid revision (EG-32).
    liquid: Vec<liquid::LiquidWord>,
    /// Lines that left from the front, folding or thinning away (MO-33).
    leaving: Vec<overflow::Leaving>,
    /// The margin mark (MO-32).
    margin: margin::Margin,
    /// The overflow spine (MO-34).
    spine: spine::Spines,
    /// The margin bell (MO-31).
    bell: bell::Bells,
    /// The newest line of the last front drop and when its words show (MO-33).
    drop_hold: Option<(u32, f64)>,
    /// Caret `(x, y)` on the baseline, and its bar size `(width, ascent, descent)`.
    caret: Option<(Channel, Channel, Caret, [f32; 3])>,
    /// When words last arrived, changed or went.
    arrived: f64,
    /// Em size of the text, for tracking.
    size: f32,
    line_height: f32,
    /// Each line's `(x0, x1, baseline)` in the current layout, by line number.
    spans: Vec<(f32, f32, f32)>,
    /// The reading stage's scale origin along a line, and whether there is a stage.
    origin: f32,
    staged: bool,
    /// Lines opened for displaced ghosts: the line and how far it opens.
    open: Vec<(u32, Channel)>,
    /// The settling accent, if any.
    accent: Option<Color>,
    /// Running correction hinges and revision addresses (MO-21, MO-23).
    marks: revisions::Marks,
    /// Letter physics, off under reduced motion.
    letters: Option<letter_physics::LetterPhysics>,
    /// The magnetic caret, and whether it is still (reduced motion).
    magnet: Option<magnet::Magnet>,
    magnet_still: bool,
}

/// What the reading stage asks of every word for one layout.
struct StageTargets {
    spans: Vec<(f32, f32, f32)>,
    /// The roll every line takes: the newest line's centre to `line_y`. 0 without a stage.
    roll: f32,
    newest: u32,
    reading: Option<Reading>,
    travel: Spring,
}

impl StageTargets {
    fn new(placed: &[Option<PlacedWord>], reading: Option<&Reading>, size: f32) -> StageTargets {
        let mut spans: Vec<(f32, f32, f32)> = Vec::new();
        for p in placed.iter().flatten() {
            let i = p.line as usize;
            if spans.len() <= i {
                spans.resize(i + 1, (f32::INFINITY, f32::NEG_INFINITY, p.y));
            }
            let sp = &mut spans[i];
            (sp.0, sp.1, sp.2) = (sp.0.min(p.x), sp.1.max(p.x + p.width), p.y);
        }
        let newest = spans.len().saturating_sub(1) as u32;
        let roll = match (reading, spans.last()) {
            (Some(r), Some(sp)) => r.line_y - (sp.2 - 0.28 * size),
            _ => 0.0,
        };
        let travel = reading.map_or(FLIP, |r| r.travel);
        StageTargets { spans, roll, newest, reading: reading.cloned(), travel }
    }

    /// The look of a word on `line`: its age's look on a stage, plain without one.
    fn look(&self, line: u32) -> LineLook {
        match &self.reading {
            Some(r) => r.look(self.newest.saturating_sub(line)),
            None => LineLook::new(1.0, 1.0),
        }
    }
}

/// Settling ink for a word at `make.t`: a `fresh` word (new, or its text changed) starts at
/// the arrival ink and tracking; it settles at once without a stable list, or once the list
/// names it.
fn settle(o: &mut FlowWord, fresh: bool, motion: &LiveMotion, make: &Make, reduced: bool) {
    let t = make.t;
    let Some(st) = motion.settle else {
        o.ink.retarget(t, 1.0);
        o.track.retarget(t, 0.0);
        o.fresh.retarget(t, 0.0);
        return;
    };
    if fresh {
        o.ink.snap(t, st.ink as f64);
        o.track.snap(t, if reduced { 0.0 } else { st.tracking as f64 });
        o.fresh.snap(t, 1.0);
    }
    let stable = motion.stable.as_ref().is_none_or(|ids| ids.contains(&o.id));
    if stable && o.ink.target() != 1.0 {
        let k = o.revisions.min(3) as f64 / 3.0;
        let spring = Spring::critical_to_98(st.fastest + (st.slowest - st.fastest) * k);
        o.ink.set_spring(spring);
        o.track.set_spring(spring);
        o.fresh.set_spring(spring);
        o.ink.retarget(t, 1.0);
        o.track.retarget(t, 0.0);
        o.fresh.retarget(t, 0.0);
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

    /// RD-04: a live word that grows or shrinks while its enter fade runs.
    mod growing_word {
        use crate::arrival::{Arrival, INK};
        use crate::element::{Element, Prop, TextStyle, Transition};
        use crate::font::{test_font_path, FontRegistry};
        use crate::text::ShapedLayout;
        use crate::Scene;
        use vo_core::spring::Spring;
        use vo_core::words::Word;

        const STEP: f64 = 1.0 / 120.0;

        fn scene(reduced: bool) -> Scene<()> {
            let fonts = FontRegistry::new();
            fonts.add_file(&test_font_path()).unwrap();
            Scene::new().with_text(Box::new(ShapedLayout::new(fonts))).with_reduced_motion(reduced)
        }

        fn view(text: &str, enter: Transition, arrival: Arrival) -> Element<()> {
            let w = [Word { id: 1, text: text.into() }];
            Element::column("root").child(
                Element::live_text("live", &w)
                    .text_style(TextStyle::new(0, 20.0, 26.0))
                    .width(400.0)
                    .motion(Prop::Opacity, Spring::critical_to_98(0.2))
                    .motion(Prop::Offset, Spring::critical_to_98(0.25))
                    .word_enter(enter)
                    .word_arrival(arrival),
            )
        }

        /// The ink of each letter of the word as it stands at `t` (ghosts of removed letters
        /// come after the word's own glyphs and are left out).
        /// A letter at no ink is not drawn: it counts as 0.
        fn inks(s: &mut Scene<()>, letters: usize, t: f64) -> Vec<f32> {
            let list = s.sample(t);
            let mut own: Vec<f32> =
                list.glyphs.iter().filter(|g| g.word == 1).map(|g| g.color.a).take(letters).collect();
            own.resize(letters, 0.0);
            own
        }

        /// Runs `steps` (text at time) and returns the lowest ink of any letter at any frame
        /// up to `end`, and every letter's ink at `end`.
        fn run(steps: &[(&str, f64)], enter: Transition, arrival: Arrival, reduced: bool, end: f64) -> (f32, Vec<f32>) {
            let mut s = scene(reduced);
            let mut low = f32::MAX;
            for (i, &(text, at)) in steps.iter().enumerate() {
                s.apply(view(text, enter, arrival), at);
                let until = steps.get(i + 1).map_or(end, |n| n.1);
                let letters = text.len();
                let mut t = at;
                while t < until - 1e-9 {
                    low = inks(&mut s, letters, t).into_iter().fold(low, f32::min);
                    t += STEP;
                }
            }
            let last = steps.last().unwrap().0.len();
            (low, inks(&mut s, last, end))
        }

        const GROW: &[(&str, f64)] = &[("hel", 0.0), ("hello", 0.05)];
        const GROW_TWICE: &[(&str, f64)] = &[("he", 0.0), ("hell", 0.04), ("hello", 0.09)];
        const SHRINK: &[(&str, f64)] = &[("hellos", 0.0), ("hello", 0.05)];
        const ALL: [&[(&str, f64)]; 3] = [GROW, GROW_TWICE, SHRINK];

        fn enters() -> [Transition; 3] {
            [
                Transition::fade().offset(0.0, 7.0),
                Transition::fade().offset(0.0, 5.0).blur(3.0),
                Transition::fade().opacity(0.7),
            ]
        }

        #[test]
        fn every_letter_reaches_full_ink_at_the_end_of_the_fade() {
            for steps in ALL {
                for enter in enters() {
                    for arrival in [Arrival::Readable, Arrival::Free] {
                        let (_, end) = run(steps, enter, arrival, false, 1.0);
                        assert!(
                            end.iter().all(|&a| a >= 1.0 - 1.0 / 255.0),
                            "{steps:?} {enter:?} {arrival:?}: {end:?}"
                        );
                    }
                }
            }
        }

        #[test]
        fn a_readable_word_is_readable_in_every_frame() {
            for steps in ALL {
                for enter in enters() {
                    let (low, _) = run(steps, enter, Arrival::Readable, false, 1.0);
                    assert!(low >= INK - 1.0 / 255.0, "{steps:?} {enter:?}: lowest ink {low}");
                }
            }
        }

        #[test]
        fn reduced_motion_holds_the_same_ink() {
            // Reduced motion drops the travel and keeps the fade (`Transition::reduced`).
            for steps in ALL {
                let (low, end) = run(steps, Transition::fade().offset(0.0, 7.0), Arrival::Readable, true, 1.0);
                assert!(low >= INK - 1.0 / 255.0, "{steps:?}: lowest ink {low}");
                assert!(end.iter().all(|&a| a >= 1.0 - 1.0 / 255.0), "{steps:?}: {end:?}");
            }
        }
    }

    #[test]
    fn graphemes_not_bytes() {
        // A combining accent stays one grapheme.
        let (ops, _) = diff_graphemes(&g("cafe\u{301}"), &g("Cafe\u{301}"));
        assert_eq!(ops, vec![Case(0), Kept(1), Kept(2), Kept(3)]);
    }
}
