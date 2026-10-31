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

#[path = "motion/letter_motion.rs"]
pub mod letter_motion;

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
/// On a front drop, the old copy of a word that moves in the new layout fades out in this
/// long, and its new copy fades in at its place once it is gone.
pub const DROP_FADE_SECS: f64 = 0.08;
const DROP_FADE: Spring = Spring::critical_to_98(DROP_FADE_SECS);
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
/// The caret reaches a new end of the text back along its line, or on another line, in
/// about 120 ms; an end further on along its line it takes at once.
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

/// Ink under the still profile (`LiveMotion::still`): 98 % of the way in 50 ms, within 1 %
/// by 80 ms.
pub const STILL_FADE: Spring = Spring::critical_to_98(0.05);
/// Words that reflow or scroll under the still profile: at most 150 ms.
pub const STILL_REFLOW: Spring = Spring::critical_to_98(0.15);

/// The edit caret stays solid this long after the text or the caret last changed.
pub const EDIT_HOLD: f64 = 0.5;
/// This long after the last change the edit caret stops blinking and stays on, so an idle
/// editor draws no more frames.
pub const EDIT_STILL: f64 = 10.0;

/// What an editable live text draws besides its glyphs (`Element::edit`): the caret, the
/// selection behind the glyphs, and underlines on edited text.
///
/// Bytes are in the text the node shapes: pass the edit buffer with `Element::live_source`
/// so they match it (`live_spans`). Each piece moves with the glyph next to it, so it stays
/// on the glyphs while lines reflow or scroll, and it is drawn in the node's own space, so a
/// rotated or scaled parent turns it with the text. The node lays out with caret stops
/// (`TextStyle::stops`) while it has a decor, and its end-of-text `Caret` is not drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct EditDecor {
    /// The caret byte; `None` draws no caret.
    pub caret: Option<usize>,
    /// The line the caret is on when its byte is a soft wrap.
    pub affinity: vo_core::edit::Affinity,
    /// Selected bytes; empty for none.
    pub selection: std::ops::Range<usize>,
    /// Edited bytes, each range underlined.
    pub marks: Vec<std::ops::Range<usize>>,
    /// When the text or the caret last changed, in scene time. The caret is solid for
    /// `EDIT_HOLD`, then blinks on and off in hard steps at `CARET_HZ`, and from
    /// `EDIT_STILL` on it stays on.
    pub since: f64,
    /// The bar: width and colour (`gap` is not used). A `None` colour is the text colour.
    pub caret_style: Caret,
    /// The selection fill; `None` is the text colour at 35 %.
    pub selection_color: Option<Color>,
    /// The underline colour; `None` is the text colour.
    pub mark_color: Option<Color>,
    /// With no `caret`, the node still draws its own end-of-text caret (`Element::caret`)
    /// as it would with no decor: for marks shown after an edit while the text grows again.
    /// Off by default, so a decor with no caret, such as a line that does not hold the edit
    /// caret, draws no caret at all.
    pub end_caret: bool,
}

impl Default for EditDecor {
    fn default() -> EditDecor {
        EditDecor {
            caret: None,
            affinity: vo_core::edit::Affinity::Downstream,
            selection: 0..0,
            marks: Vec::new(),
            since: 0.0,
            caret_style: Caret::default(),
            selection_color: None,
            mark_color: None,
            end_caret: false,
        }
    }
}

impl EditDecor {
    /// The caret's opacity at `t`: 1 or 0, never in between.
    pub fn caret_alpha(&self, t: f64) -> f32 {
        let idle = t - self.since;
        if !(EDIT_HOLD..EDIT_STILL).contains(&idle) {
            return 1.0;
        }
        let period = 1.0 / CARET_HZ;
        if (idle - EDIT_HOLD).rem_euclid(period) < CARET_DUTY * period {
            1.0
        } else {
            0.0
        }
    }

    /// The next time after `t` the caret turns on or off, or `None` when it no longer
    /// changes (no caret, or `EDIT_STILL` has passed). A frame is needed only then.
    pub fn wake(&self, t: f64) -> Option<f64> {
        self.caret?;
        let idle = t - self.since;
        if idle >= EDIT_STILL {
            return None;
        }
        if idle < EDIT_HOLD {
            return Some(self.since + EDIT_HOLD);
        }
        let period = 1.0 / CARET_HZ;
        let u = (idle - EDIT_HOLD).rem_euclid(period);
        let on = CARET_DUTY * period;
        let next = if u < on { t - u + on } else { t - u + period };
        // A microsecond past the edge, so a frame there is sure to see the new state.
        Some((next + 1e-6).min(self.since + EDIT_STILL))
    }
}

/// An edit decor placed at `t`, in the text box's space.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct EditFrame {
    /// Selection fills, one per line.
    pub selection: Vec<Rect>,
    /// Underlines, one per line of each mark.
    pub marks: Vec<Rect>,
    /// The caret bar and its blink opacity.
    pub caret: Option<(Rect, f32)>,
    /// Where the caret meets the baseline, moved as the bar is.
    pub foot: Option<Point>,
}

/// Places `decor` on `shaped`, laid out with caret stops, for the frame at `t`. `moved(i)`
/// is how far glyph `i` is drawn from its laid-out place at `t`; each piece moves with the
/// glyph at its start (`edit_anchor`).
pub(crate) fn edit_frame(
    shaped: &ShapedText,
    decor: &EditDecor,
    size: f32,
    t: f64,
    moved: impl Fn(usize) -> (f32, f32),
) -> EditFrame {
    let shift = |r: Rect, byte: usize, line: usize| match edit_anchor(shaped, byte, line) {
        Some(i) => {
            let (dx, dy) = moved(i);
            Rect::new(r.x + dx, r.y + dy, r.w, r.h)
        }
        None => r,
    };
    // The line a selection rect is on, and the first byte of the range on it.
    let on_line = |r: &Rect, from: usize| {
        let mid = r.y + 0.5 * r.h;
        let line = (0..shaped.lines.len())
            .min_by(|&a, &b| (shaped.lines[a].baseline - mid).abs().total_cmp(&(shaped.lines[b].baseline - mid).abs()))
            .unwrap_or(0);
        (line, from.max(shaped.lines.get(line).map_or(0, |l| l.bytes.start)))
    };
    let selection = shaped
        .selection_rects(decor.selection.clone())
        .into_iter()
        .map(|r| {
            let (line, byte) = on_line(&r, decor.selection.start);
            shift(r, byte, line)
        })
        .collect();
    let thick = (0.06 * size).max(1.0);
    let mut marks = Vec::new();
    for m in &decor.marks {
        for r in shaped.selection_rects(m.clone()) {
            let (line, byte) = on_line(&r, m.start);
            let baseline = shaped.lines[line].baseline;
            marks.push(shift(Rect::new(r.x, baseline + 0.12 * size, r.w, thick), byte, line));
        }
    }
    let caret = decor.caret.and_then(|byte| {
        let stop = shaped.stop_at(byte, decor.affinity)?;
        let baseline = shaped.lines.get(stop.line)?.baseline;
        let w = decor.caret_style.width;
        let bar = shift(Rect::new(stop.x - 0.5 * w, baseline - 0.78 * size, w, size), byte, stop.line);
        Some((bar, decor.caret_alpha(t)))
    });
    let foot = caret.map(|(bar, _)| Point::new(bar.x + 0.5 * bar.w, bar.y + 0.78 * size));
    EditFrame { selection, marks, caret, foot }
}

/// The glyph that a decor at `byte` on `line` moves with: the glyph that starts at `byte`,
/// else the last one before it on the line, else the line's first. A line with no glyphs (a
/// blank line) takes the last glyph before `byte`, else the first glyph of the text.
fn edit_anchor(shaped: &ShapedText, byte: usize, line: usize) -> Option<usize> {
    let range = shaped.lines.get(line).map_or(0..0, |l| l.glyphs.clone());
    let on = &shaped.glyphs[range.clone()];
    if let Some(i) = on.iter().position(|g| g.cluster as usize == byte) {
        return Some(range.start + i);
    }
    if let Some(i) = on.iter().rposition(|g| (g.cluster as usize) < byte) {
        return Some(range.start + i);
    }
    if !on.is_empty() {
        return Some(range.start);
    }
    let before = shaped.glyphs.iter().rposition(|g| (g.cluster as usize) < byte);
    before.or((!shaped.glyphs.is_empty()).then_some(0))
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
    /// Per-letter motions: tumbling arrivals, italic arrival, older lines on a wave (EG-16).
    pub letter_motion: letter_motion::LetterMotions,
    /// The caret as a magnet that pulls new words in, with its field drawn (MO-89).
    pub magnet: Option<magnet::Magnet>,
    /// Words set in a font of their own, such as a variable font instance (MO-81).
    pub word_fonts: Vec<(WordId, FontId)>,
    /// The words in `word_fonts` keep the advances of the style's font, so their own font
    /// changes only how they are drawn (`Element::word_fonts_in_place`).
    pub word_fonts_in_place: bool,
    /// What lies between each breathing word's instance and its axes (EG-23,
    /// `Element::word_breath`): edge growth, a lean, a stretch, and the advances a share of
    /// the way to the next width instance.
    pub word_breath: Vec<(WordId, crate::font::var_axes::Breath)>,
    /// The text as typed, when it is more than the words joined by single spaces: runs of
    /// spaces and hard newlines (`Element::live_source`). See `live_spans`.
    pub source: Option<String>,
    /// The still profile (`Element::still_motion`), for editing: words that enter, change or
    /// leave only change ink, within 80 ms (`STILL_FADE`), in place; a letter that comes in
    /// starts at arrival ink (`arrival::INK`), so it is readable in its first frame. Reflow and scroll move
    /// whole words in at most 150 ms (`STILL_REFLOW`), and a revised word is at its place at
    /// once. Revisions diff by letter: kept letters keep their ink, removed ones fade where
    /// they are drawn (a word less than half kept crossfades). Rewraps fade, and settling
    /// ink, letter physics, the magnet and revision marks are off. Motion that is running
    /// when it turns on settles the same way.
    pub still: bool,
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

/// The byte ranges of the words that `fonts` sets in a font of their own, in order, from
/// the words' `ranges` in the shaped text: what `TextLayout::layout_fonts` takes for live
/// text.
pub(crate) fn word_font_ranges(
    words: &[Word],
    ranges: &[std::ops::Range<usize>],
    fonts: &[(WordId, FontId)],
) -> Vec<(std::ops::Range<usize>, FontId)> {
    if fonts.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (w, r) in words.iter().zip(ranges) {
        if let Some(&(_, f)) = fonts.iter().find(|(id, _)| *id == w.id) {
            out.push((r.clone(), f));
        }
    }
    out
}

/// The byte ranges of the words in `breaths` that lie between two width instances, with the
/// instance they lean toward and how far: what `TextLayout::layout_fonts_toward` takes.
pub(crate) fn breath_toward(
    words: &[Word],
    ranges: &[std::ops::Range<usize>],
    breaths: &[(WordId, crate::font::var_axes::Breath)],
) -> Vec<(std::ops::Range<usize>, FontId, f32)> {
    if breaths.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (w, r) in words.iter().zip(ranges) {
        if let Some((_, b)) = breaths.iter().find(|(id, _)| *id == w.id) {
            for &(font, share) in b.toward.iter().flatten() {
                out.push((r.clone(), font, share));
            }
        }
    }
    out
}

/// The text a live text shapes, and the byte range of each word in it.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveSpans<'a> {
    pub text: std::borrow::Cow<'a, str>,
    /// One range per word, in order.
    pub ranges: Vec<std::ops::Range<usize>>,
}

/// Where `words` sit in the text a live text shapes. `source` (`LiveMotion::source`) is
/// used when it holds the words in order with only whitespace around them, so runs of
/// spaces and hard newlines reach the layout. Otherwise the text is the words joined by
/// single spaces.
pub fn live_spans<'a>(words: &[Word], source: Option<&'a str>) -> LiveSpans<'a> {
    if let Some(src) = source {
        if let Some(ranges) = source_ranges(words, src) {
            return LiveSpans { text: std::borrow::Cow::Borrowed(src), ranges };
        }
    }
    let text = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    LiveSpans { text: std::borrow::Cow::Owned(text), ranges: joined_ranges(words) }
}

/// The ranges of `live_spans`, without building the text: for code that runs every frame.
pub(crate) fn word_ranges(words: &[Word], source: Option<&str>) -> Vec<std::ops::Range<usize>> {
    source.and_then(|src| source_ranges(words, src)).unwrap_or_else(|| joined_ranges(words))
}

/// The ranges of `words` joined by single spaces.
fn joined_ranges(words: &[Word]) -> Vec<std::ops::Range<usize>> {
    let mut at = 0;
    words
        .iter()
        .map(|w| {
            let r = at..at + w.text.len();
            at = r.end + 1;
            r
        })
        .collect()
}

/// The ranges of `words` in `src`, when `src` is the words in order with only whitespace
/// between, before and after them, and at least one whitespace character between two words.
fn source_ranges(words: &[Word], src: &str) -> Option<Vec<std::ops::Range<usize>>> {
    let mut ranges = Vec::with_capacity(words.len());
    let mut at = 0;
    for w in words {
        if w.text.is_empty() || w.text.contains(char::is_whitespace) {
            return None;
        }
        let gap = &src[at..];
        let start = at + (gap.len() - gap.trim_start().len());
        if !(ranges.is_empty() || start > at) || !src[start..].starts_with(w.text.as_str()) {
            return None;
        }
        at = start + w.text.len();
        ranges.push(start..at);
    }
    src[at..].trim_start().is_empty().then_some(ranges)
}

impl LiveMotion {
    /// This motion under the still profile (`LiveMotion::still`).
    pub(crate) fn stilled(&self) -> LiveMotion {
        LiveMotion {
            // Letter by letter, so the letters a key leaves alone keep their ink and place:
            // a crossfade would draw the whole word twice, the new copy at arrival ink.
            revise: Revise::Glyphs,
            rewrap: Rewrap::Fade,
            case: CaseChange::default(),
            settle: None,
            letters: None,
            letter_motion: letter_motion::LetterMotions::NONE,
            magnet: None,
            revisions: revisions::RevisionMotion::default(),
            bell: None,
            ..self.clone()
        }
    }
}

/// From `t`, `c` moves from where it is to its final target on `spring`; later segments
/// (a delayed start) are dropped.
fn settle_with(c: &mut Channel, t: f64, spring: Spring) {
    let (now, to) = (c.value(t), c.target());
    c.set_spring(spring);
    c.snap(t, now);
    c.retarget(t, to);
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

/// A ghost under this opacity counts as gone when lines are kept apart.
const GHOST_GONE: f64 = 0.02;

/// A fixed ghost's pen at `t`. Every ghost a front drop leaves is fixed.
fn ghost_pen(g: &Ghost, t: f64) -> (f64, f64) {
    let (x, y) = g.fixed.unwrap_or_default();
    ((x + g.x) as f64 + g.slide.value(t), (y + g.y) as f64 + g.rise.value(t))
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
    /// The glyph's own turn and lean (EG-16).
    pub xform: crate::display::GlyphXform,
}

impl GlyphMotion {
    pub const NONE: GlyphMotion = GlyphMotion {
        dx: 0.0,
        dy: 0.0,
        opacity: 1.0,
        scale: 1.0,
        tint: None,
        xform: crate::display::GlyphXform::IDENTITY,
    };

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

/// Whether kept word `o` has a new place in the layout.
fn moved(o: &FlowWord, p: &PlacedWord) -> bool {
    (o.x.target() as f32 - p.x).abs() >= 0.5 || (o.y.target() as f32 - p.y).abs() >= 0.5
}

/// Whether kept word `o` moved with the scrolling block: only `dy` down from its new place.
/// On a row in `whole` the line may also have shifted along (a "…" put on the new first word,
/// a centred line), and the word then slides along its row while it scrolls, instead of
/// fading out where it was.
fn in_block(o: &FlowWord, p: &PlacedWord, dy: f32, whole: &[u32]) -> bool {
    ((o.x.target() as f32 - p.x).abs() < 0.5 || whole.contains(&p.line)) && (o.y.target() as f32 - p.y - dy).abs() < 1.0
}

/// On a front drop, how far up the first kept word moved, when it moved up a row or more.
fn drop_rows(old: &[FlowWord], words: &[Word], placed: &[Option<PlacedWord>]) -> Option<f32> {
    let (w, p) = words.iter().zip(placed).find_map(|(w, p)| p.as_ref().map(|p| (w, p)))?;
    let o = old.iter().find(|o| o.id == w.id)?;
    let dy = o.y.target() as f32 - p.y;
    (dy >= 0.5).then_some(dy)
}

/// Half the narrowest gap between two neighbouring words on a line of the new layout: the
/// space the text uses, halved. Words that move never come closer than this (s8my). A
/// quarter em, halved, when no line holds two words.
fn half_space(placed: &[Option<PlacedWord>], size: f32) -> f64 {
    let words: Vec<&PlacedWord> = placed.iter().flatten().collect();
    let space = words
        .windows(2)
        .filter(|p| p[0].line == p[1].line)
        .map(|p| p[1].x - (p[0].x + p[0].width))
        .filter(|&d| d > 0.0)
        .fold(f32::INFINITY, f32::min);
    0.5 * if space.is_finite() { space } else { 0.25 * size } as f64
}

/// A glyph new at `make.t` that would draw within `gap` of its neighbour on the row (a word
/// that grew, "stop" to "stop,", before the next word slid over, or before the old copy of
/// a word that left the row faded) is not drawn until it fits all the way, then runs the
/// same fade, pop or rise from there (s8my). The glyph counts at the width it is
/// drawn (a pop starts at no width) and only while it shows. Glyphs drawn before are left
/// alone, so nothing on screen blinks.
fn hold_new_glyphs(next: &mut [FlowWord], ghosts: &[Ghost], line_height: f32, gap: f64, make: &Make) {
    const STEP: f64 = 1.0 / 240.0;
    /// Under this ink a glyph is not seen yet.
    const SEEN: f64 = 0.05;
    let t = make.t;
    let near = 0.9 * line_height as f64;
    for i in 0..next.len() {
        if !next[i].glyphs.iter().any(|g| g.born >= t) {
            continue;
        }
        let mut o = std::mem::take(&mut next[i].glyphs);
        let me = &next[i];
        // The other words on the row, while they show.
        let others: Vec<&FlowWord> = next.iter().filter(|q| q.id != me.id && q.line == me.line).collect();
        let row_settles = others.iter().map(|q| q.x.settled_at().max(q.width.settled_at())).fold(t, f64::max);
        for g in o.iter_mut().filter(|g| g.born >= t) {
            // Whether the glyph, `d` seconds into its own enter, keeps clear at `at`.
            let clear = |at: f64, d: f64| {
                let own = t + d;
                if g.opacity.value(own) < SEEN {
                    return true;
                }
                let half = 0.5 * g.advance as f64;
                let mid = me.x.value(at) + g.x.value(at) + half;
                let (a, b) = (mid - half * g.scale.value(own), mid + half * g.scale.value(own));
                let row = me.y.value(at) + me.roll.value(at);
                !others.iter().any(|q| {
                    let x = q.x.value(at);
                    x < b + gap
                        && x + q.width.value(at) + gap > a
                        && q.glyphs.iter().any(|f| f.opacity.value(at) > SEEN)
                }) && !ghosts.iter().any(|h| {
                    h.word != me.id && h.opacity.value(at) > GHOST_GONE && {
                        let (x, y) = ghost_pen(h, at);
                        (y - row).abs() < near && x < b + gap && x + h.advance as f64 + gap > a
                    }
                })
            };
            // Its enter, started `wait` later, keeps clear until it settles and the row has
            // stopped moving.
            let enter = (g.opacity.settled_at().max(g.scale.settled_at()) - t).clamp(0.0, 0.5);
            let fits = |wait: f64| {
                let span = enter.max(row_settles - t - wait).min(0.6);
                let mut d = 0.0;
                loop {
                    if !clear(t + wait + d, d) {
                        return false;
                    }
                    if d >= span {
                        return true;
                    }
                    d = (d + 4.0 * STEP).min(span);
                }
            };
            if fits(0.0) {
                continue;
            }
            let mut wait = STEP;
            while wait < REWRAP_WAIT_MAX && !fits(wait) {
                wait += STEP;
            }
            // Not drawn while it waits; then its enter runs as it would have.
            g.opacity.postpone(t, wait, 0.0);
            for c in [&mut g.scale, &mut g.rise] {
                let now = c.value(t);
                c.postpone(t, wait, now);
            }
        }
        next[i].glyphs = o;
    }
}

/// A rewrapped word shows once the words still sliding along its new line have left its
/// place, so text never draws over text: the words a reflow pushes along a line start
/// where the newcomer lands. On the newest line it then shows whole, in the frame its old
/// copy goes (reading-line envelope); on older lines it fades in. A word whose place is
/// clear at once keeps its fade. A newest-line word lands at rest, so it also waits for
/// the line scrolling up out of its row (within 0.9 `line_height`). On a front drop it also
/// waits for the old copies the drop left (`ghost_at`: whether one shows over a span and
/// baseline at a time, other than the word's own). Returns when each newest-line word shows.
fn show_when_clear(
    next: &mut [FlowWord],
    rewrapped: &[usize],
    newest: u32,
    line_height: f32,
    make: &Make,
    gap: f64,
    ghost_at: Option<&dyn Fn(f64, f64, f64, f64, WordId) -> bool>,
) -> Vec<(WordId, f64)> {
    const STEP: f64 = 1.0 / 240.0;
    let t = make.t;
    let mut shown = Vec::new();
    for &s in rewrapped {
        let line = next[s].line;
        let row = next[s].y.target() + next[s].roll.target();
        let near = 0.9 * line_height as f64;
        let busy = |at: f64| {
            // Where the word is at `at`, with `gap` kept clear on each side: one that slides
            // with the words before it on its new line never meets them.
            let a = next[s].x.value(at) - gap;
            let b = next[s].x.value(at) + next[s].width.value(at) + gap;
            next.iter().enumerate().any(|(i, o)| {
                let x = o.x.value(at);
                let in_row =
                    o.line == line || (line == newest && (o.y.value(at) + o.roll.value(at) - row).abs() < near);
                i != s && in_row && !rewrapped.contains(&i) && x < b && x + o.width.value(at) > a
            }) || ghost_at.is_some_and(|hit| hit(at, a, b, next[s].y.value(at) + next[s].roll.value(at), next[s].id))
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
/// glyphs on screen (cut by `max_lines`). `ranges` are the words' bytes in the shaped text
/// (`live_spans`); glyphs outside every word, such as a "…", belong to no word.
fn place(shaped: &ShapedText, words: &[Word], ranges: &[std::ops::Range<usize>]) -> Vec<Option<PlacedWord>> {
    let mut out: Vec<Option<PlacedWord>> = (0..words.len()).map(|_| None).collect();
    let (mut line, mut last_y) = (0u32, None::<f32>);
    for (i, g) in shaped.glyphs.iter().enumerate() {
        let c = g.cluster as usize;
        let Some(w) = ranges.partition_point(|r| r.start <= c).checked_sub(1) else { continue };
        if c >= ranges[w].end {
            continue;
        }
        let start = ranges[w].start as u32;
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
            offset: g.cluster - start,
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
        let FlowInput { shaped, words, motion, size, line_height, retention, reduced, crossfade, width, entering } =
            input;
        let calm;
        let motion = if motion.still {
            calm = motion.stilled();
            &calm
        } else {
            motion
        };
        self.size = size;
        let revise = if crossfade { motion.revise } else { Revise::Swap };
        let make = Make { t, retention, size };
        let placed = place(shaped, words, &word_ranges(words, motion.source.as_deref()));
        let gap = half_space(&placed, size);
        let stage = StageTargets::new(&placed, motion.reading.as_ref(), size);
        self.spans = stage.spans.clone();
        (self.origin, self.staged) = (motion.reading.as_ref().map_or(0.0, |r| r.origin), motion.reading.is_some());
        self.line_height = line_height;
        self.accent = motion.settle.and_then(|s| s.accent);
        self.letters = if reduced { None } else { motion.letters };
        self.letter_motion = if reduced { letter_motion::LetterMotions::NONE } else { motion.letter_motion };
        (self.magnet, self.magnet_still) = (motion.magnet, reduced || motion.magnet.is_some_and(|m| m.still));
        let mut old = std::mem::take(&mut self.words);
        // Words new in this update, and words whose text changed.
        let (mut new_ids, mut revised_ids) = (Vec::new(), Vec::new());
        let front_drop = old.iter().position(|o| words.iter().any(|w| w.id == o.id)).is_some_and(|i| i > 0);
        // A drop that takes whole lines and rewraps every line below still scrolls the block
        // by those lines: how far the first kept word moved up.
        let scroll =
            front_scroll(&old, words, &placed).or_else(|| front_drop.then(|| drop_rows(&old, words, &placed))?);
        let whole = if scroll.is_some() { whole_rows(&old, words, &placed) } else { Vec::new() };
        // A front drop reflows calmly: the block scrolls, and a word that moves in the new
        // layout fades out where it was and in at its place, never sliding.
        let calm_drop = front_drop && !reduced;
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
        // On a front drop, the words whose old copies stay behind as ghosts: they go before
        // any word of the new layout reaches them, so the two layouts never overlap.
        let mut dropped: Vec<WordId> = Vec::new();
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
                new_ids.push(w.id);
                let back = match line_shift {
                    Some((line, dx)) if line == p.line && !reduced && !calm_drop => dx,
                    _ => 0.0,
                };
                // A word that arrives while the word before it on its line still slides
                // (moved by an earlier update) starts as far off and as fast, so the two move
                // as one piece instead of the new word landing on its neighbour.
                let follow = next.last().filter(|n| back == 0.0 && !reduced && n.line == p.line).and_then(|n| {
                    let (x, v) = n.x.sample(t);
                    let off = x - n.x.target();
                    (off.abs() >= 0.5).then_some((off, v, n.x.spring()))
                });
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
                // On a front drop a new word on an older line fades in once the old copies of
                // the reflowed words are gone.
                if calm_drop && p.line != stage.newest {
                    for g in &mut glyphs {
                        g.opacity = make.ch(0.0, CROSSFADE, Rest::UNIT);
                        g.opacity.retarget(t + DROP_FADE_SECS, 1.0);
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
                    } else if let Some((off, v, spring)) = follow {
                        let mut c = make.ch(p.x, spring, Rest::PIXELS);
                        c.restart(t, p.x as f64 + off, v, p.x as f64);
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
                revised_ids.push(o.id);
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
            } else if calm_drop && moved(&o, &p) {
                // The old copy scrolls up with the block and fades out quickly; the new copy
                // comes up with the block at its place in the new layout and fades in once the
                // old one is gone. On the newest line it lands at rest and shows whole when its
                // place is clear (reading-line envelope).
                let newest = p.line == stage.newest;
                if newest {
                    handed.push((o.id, self.ghosts.len()));
                }
                self.ghost_word(&o, t, retention);
                dropped.push(o.id);
                let dy = scroll.unwrap_or(0.0);
                for g in self.ghosts.iter_mut().filter(|g| g.word == o.id) {
                    let (rise, k) = (g.rise.value(t) as f32, g.opacity.value(t) as f32);
                    g.rise = make.to(rise, rise - dy, SCROLL, Rest::PIXELS);
                    g.opacity = make.to(k, 0.0, DROP_FADE, Rest::UNIT);
                }
                o.x = make.ch(p.x, FLIP, Rest::PIXELS);
                o.y = if newest {
                    make.ch(p.y, FLIP, Rest::PIXELS)
                } else {
                    make.to(p.y + dy, p.y, SCROLL, Rest::PIXELS)
                };
                for g in &mut o.glyphs {
                    g.opacity = make.ch(if newest { 1.0 } else { 0.0 }, CROSSFADE, Rest::UNIT);
                    if !newest {
                        g.opacity.retarget(t + DROP_FADE_SECS, 1.0);
                    }
                }
                if newest {
                    rewrapped.push(slot as usize);
                }
                line_shift = Some((p.line, 0.0));
                move_to(&mut o.roll, stage.roll, t, stage.travel, newest);
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
            if calm_drop {
                dropped.push(id);
            }
            if let Some(dy) = scroll.filter(|_| front.contains(&id) && !reduced) {
                for g in self.ghosts.iter_mut().filter(|g| g.word == id) {
                    let rise = g.rise.value(t) as f32;
                    g.rise = make.to(rise, rise - dy, SCROLL, Rest::PIXELS);
                }
            }
        }
        let near = 0.9 * line_height as f64;
        let ghosts = &self.ghosts;
        let ghost_at = |at: f64, a: f64, b: f64, row: f64, own: WordId| {
            ghosts.iter().any(|g| {
                g.word != own && dropped.contains(&g.word) && g.opacity.value(at) > GHOST_GONE && {
                    let (x, y) = ghost_pen(g, at);
                    (y - row).abs() < near && x < b && x + g.advance as f64 > a
                }
            })
        };
        let shown = show_when_clear(
            &mut next,
            &rewrapped,
            stage.newest,
            line_height,
            &make,
            gap,
            (!dropped.is_empty()).then_some(&ghost_at as &dyn Fn(f64, f64, f64, f64, WordId) -> bool),
        );
        for &(id, from) in &handed {
            let at = shown.iter().find(|s| s.0 == id).map_or(t, |s| s.1);
            for g in self.ghosts[from..].iter_mut().filter(|g| g.word == id) {
                let k = g.opacity.value(t);
                g.opacity.snap(t, k);
                g.opacity.snap(at, 0.0);
            }
        }
        self.clear_ghosts(&next, &rewrapped, &left, gap, t);
        self.clear_dropped(&next, &dropped, line_height, gap, t);
        if !reduced {
            hold_new_glyphs(&mut next, &self.ghosts, line_height, gap, &make);
        }
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
        if motion.still {
            self.calm(t, &new_ids, &revised_ids);
        }
    }

    /// The still profile: from `t`, every glyph is at its place, ink moves to where it is
    /// going within `STILL_FADE`, and words and the caret reach their places within
    /// `STILL_REFLOW`. Delays and waits that were set up are dropped. A word in `revised`
    /// is at its place at once: its kept letters are already at theirs within it, and the
    /// word sliding there would carry them away and back. The letters of a word in `new`
    /// are at full ink: the word's own enter fade, from arrival ink, is the only fade.
    fn calm(&mut self, t: f64, new: &[WordId], revised: &[WordId]) {
        // Glyphs on their way out stay where they are drawn now, so the word's width can
        // snap without squeezing them.
        for i in 0..self.ghosts.len() {
            let g = &self.ghosts[i];
            if g.fixed.is_some() {
                continue;
            }
            let Some(w) = self.words.iter().find(|w| w.id == g.word) else { continue };
            let k = if g.width > 0.0 { w.width.value(t) as f32 / g.width } else { 1.0 };
            let y = w.y.value(t) as f32 + w.roll.value(t) as f32 + self.opened(w.line, t);
            let x = w.x.value(t) as f32;
            let g = &mut self.ghosts[i];
            (g.fixed, g.x) = (Some((x, y)), g.x * k);
        }
        for w in &mut self.words {
            for c in [&mut w.x, &mut w.y, &mut w.roll, &mut w.look_scale] {
                settle_with(c, t, STILL_REFLOW);
            }
            if revised.contains(&w.id) {
                w.x.snap(t, w.x.target());
            }
            // A changed word is at its new width at once: its glyphs do not stretch.
            w.width.snap(t, w.width.target());
            for c in [&mut w.ink, &mut w.look_opacity] {
                settle_with(c, t, STILL_FADE);
            }
            for c in [&mut w.track, &mut w.fresh] {
                c.snap(t, c.target());
            }
            let fresh = new.contains(&w.id);
            for g in &mut w.glyphs {
                // A letter that comes in is readable in the frame it shows
                // (reading-line envelope): its fade starts at arrival ink, a hair over the
                // floor so rounding never puts it under. A new word's letters are whole at
                // once, as the word fades in.
                let ink = if fresh { 1.0 } else { crate::arrival::INK as f64 + 0.01 };
                let to = g.opacity.target();
                if to >= 1.0 && g.opacity.value(t) < ink {
                    // `snap` also moves the target: it still goes to full ink.
                    g.opacity.snap(t, ink);
                    g.opacity.retarget(t, to);
                }
                settle_with(&mut g.opacity, t, STILL_FADE);
                for c in [&mut g.x, &mut g.rise, &mut g.scale] {
                    c.snap(t, c.target());
                }
            }
        }
        for g in &mut self.ghosts {
            settle_with(&mut g.opacity, t, STILL_FADE);
            // A removed letter fades where it is drawn now, instead of rising out.
            for c in [&mut g.rise, &mut g.scale, &mut g.slide] {
                c.snap(t, c.value(t));
            }
        }
        for (_, c) in &mut self.open {
            c.snap(t, c.target());
        }
        if let Some((x, y, ..)) = &mut self.caret {
            settle_with(x, t, STILL_REFLOW);
            settle_with(y, t, STILL_REFLOW);
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
    /// along that line come within `gap` of its place, so they never draw over it or touch
    /// it.
    fn clear_ghosts(
        &mut self,
        next: &[FlowWord],
        rewrapped: &[usize],
        left: &[(WordId, u32, f64, f64)],
        gap: f64,
        t: f64,
    ) {
        for &(id, line, a, b) in left {
            // Gone before a sliding word comes within `gap` of it, not once they touch.
            let (a, b) = (a - gap, b + gap);
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

    /// On a front drop, each old copy in `dropped` goes in the frame a word of the new
    /// layout first shows over it, so the leaving and the reflowed lines never draw over
    /// each other.
    fn clear_dropped(&mut self, next: &[FlowWord], dropped: &[WordId], line_height: f32, gap: f64, t: f64) {
        if dropped.is_empty() {
            return;
        }
        let near = 0.9 * line_height as f64;
        for &id in dropped {
            let mine: Vec<usize> = (0..self.ghosts.len()).filter(|&i| self.ghosts[i].word == id).collect();
            let mut at = t;
            let gone = loop {
                let shown: Vec<&Ghost> =
                    mine.iter().map(|&i| &self.ghosts[i]).filter(|g| g.opacity.value(at) > GHOST_GONE).collect();
                if shown.is_empty() || at > t + 1.0 {
                    break None;
                }
                let reached = shown.iter().any(|g| {
                    let (x, y) = ghost_pen(g, at);
                    next.iter().any(|o| {
                        let a = o.x.value(at);
                        o.id != id
                            && (o.y.value(at) + o.roll.value(at) - y).abs() < near
                            && a < x + g.advance as f64 + gap
                            && a + o.width.value(at) + gap > x
                            && o.glyphs.iter().any(|f| f.opacity.value(at) > GHOST_GONE)
                    })
                });
                if reached {
                    break Some(at);
                }
                at += 1.0 / 240.0;
            };
            if let Some(at) = gone {
                for &i in &mine {
                    self.ghosts[i].opacity.snap(at, 0.0);
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
                // The caret never trails inside the text: when the end moves on along its line
                // (a word grew, a new word came), it jumps there at once, as the glyphs do. It
                // glides only back along a line or to another line.
                let on = (y as f64 - cy.target()).abs() < 0.5 && x as f64 > cx.value(t);
                move_to(cx, x, t, glide, reduced || on);
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
        let xform = self.letter_pose(w, g, &mut dy, t);
        let pen = Point::new(w.x.target() as f32 + g.x.target() as f32 + dx, w.y.target() as f32 + g.y + dy);
        let (pos, look) = self.on_stage(w, pen, t);
        GlyphMotion {
            dx: pos.x - pen.x + dx,
            dy: pos.y - pen.y + dy,
            opacity: ((g.opacity.value(t) * w.ink.value(t) * w.look_opacity.value(t)) as f32).clamp(0.0, 1.0),
            scale: scale * look,
            tint: self.accent.map(|c| (c, w.fresh.value(t).clamp(0.0, 1.0) as f32)),
            xform,
        }
    }

    /// The per-letter motions of glyph `g` of word `w` at `t` (EG-16): its transform, with
    /// the wave's drop added to `dy`.
    fn letter_pose(&self, w: &FlowWord, g: &FlowGlyph, dy: &mut f32, t: f64) -> crate::display::GlyphXform {
        let m = self.letter_motion;
        let mut xform = crate::display::GlyphXform::IDENTITY;
        if !m.any() {
            return xform;
        }
        let newest = self.words.last().is_none_or(|l| l.line == w.line);
        if m.italic {
            let arrived = w.glyphs.iter().map(|g| g.born).fold(f64::INFINITY, f64::min);
            xform.skew_deg = letter_motion::italic_skew(arrived, t);
        }
        if m.wave && !newest {
            // The line eases in from when the line after it started, so it never jumps.
            let next = self
                .words
                .iter()
                .filter(|o| o.line > w.line)
                .flat_map(|o| o.glyphs.iter().map(|g| g.born))
                .fold(f64::INFINITY, f64::min);
            let k = letter_motion::wave_share(next, t);
            if k > 0.0 {
                let (wy, turn) = letter_motion::wave(w.x.target() as f32 + g.x.target() as f32, t);
                *dy += k * wy;
                xform.rot_deg = k * turn;
            }
        }
        xform
    }

    /// The tumbling copies of new letters at `t` (EG-16): the shaped glyph each one copies,
    /// and where it is.
    pub(crate) fn tumbling(&self, t: f64) -> Vec<(usize, letter_motion::TumblePose)> {
        if !self.letter_motion.tumble {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (i, at) in self.index.iter().enumerate() {
            let Some((wi, gi)) = *at else { continue };
            let w = &self.words[wi as usize];
            let g = &w.glyphs[gi as usize];
            if let Some(p) = letter_motion::tumble(g.born, t, w.id.wrapping_mul(131).wrapping_add(gi)) {
                out.push((i, p));
            }
        }
        out
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
        let (cx, cy, style, [w, up, down]) = self.caret.as_ref()?;
        let alpha = caret_alpha(t - self.arrived);
        let roll = self.words.last().map_or(0.0, |w| w.roll.value(t) as f32 + self.opened(w.line, t));
        let (mut x, y) = (cx.value(t) as f32, cy.value(t) as f32 + roll);
        // Never inside the last word as it is drawn: on a centred line the word and its line
        // still slide to the new middle after the caret took the new end at rest.
        if let Some(last) = self.words.last().filter(|l| (l.y.target() - cy.target()).abs() < 0.5) {
            let edge = last.x.value(t) as f32 + self.magnet_snap(last, t) + last.width.target() as f32;
            x = x.max(edge + style.gap);
        }
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
    /// Per-letter motions (EG-16), off under reduced motion.
    letter_motion: letter_motion::LetterMotions,
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

    fn words(texts: &[&str]) -> Vec<Word> {
        texts.iter().enumerate().map(|(i, t)| Word { id: i as WordId + 1, text: (*t).into() }).collect()
    }

    #[test]
    fn spans_without_a_source_join_by_single_spaces() {
        let s = live_spans(&words(&["hello", "big", "world"]), None);
        assert_eq!(s.text, "hello big world");
        assert_eq!(s.ranges, [0..5, 6..9, 10..15]);
        assert_eq!(word_ranges(&words(&["hello", "big", "world"]), Some("hello big  world!")), s.ranges);
        assert_eq!(live_spans(&[], None).text, "");
    }

    #[test]
    fn spans_follow_runs_of_spaces_and_newlines_in_the_source() {
        let w = words(&["hello", "big", "wörld", "x"]);
        for (src, ranges) in [
            ("hello big wörld x", vec![0..5, 6..9, 10..16, 17..18]),
            ("hello   big  wörld x", vec![0..5, 8..11, 13..19, 20..21]),
            ("hello\nbig\n\nwörld\r\nx", vec![0..5, 6..9, 11..17, 19..20]),
            ("  hello \t big wörld x \n", vec![2..7, 10..13, 14..20, 21..22]),
        ] {
            let s = live_spans(&w, Some(src));
            assert_eq!(s.text, src);
            assert_eq!(s.ranges, ranges, "{src:?}");
            assert!(s.ranges.iter().zip(&w).all(|(r, w)| src[r.clone()] == w.text));
            assert_eq!(word_ranges(&w, Some(src)), s.ranges);
        }
        // Only whitespace around the words: an empty word list fits a blank source.
        assert_eq!(live_spans(&[], Some(" \n")).text, " \n");
    }

    #[test]
    fn a_source_that_does_not_hold_the_words_is_ignored() {
        let w = words(&["hello", "world"]);
        for src in ["hello world!", "helloworld", "hello, world", "world hello", "hello", "hi hello world"] {
            assert_eq!(live_spans(&w, Some(src)).text, "hello world", "{src:?}");
        }
        // A word with a space in it cannot be found word by word.
        assert_eq!(live_spans(&words(&["a b"]), Some("a b")).ranges, vec![std::ops::Range { start: 0, end: 3 }]);
        assert_eq!(live_spans(&words(&["a b"]), Some("a  b")).text, "a b");
    }

    #[test]
    fn fonts_take_the_word_ranges() {
        let w = words(&["hello", "big", "world"]);
        let s = live_spans(&w, Some("hello  big\nworld"));
        assert_eq!(word_font_ranges(&w, &s.ranges, &[(2, 7), (3, 9)]), [(7..10, 7), (11..16, 9)]);
        assert!(word_font_ranges(&w, &s.ranges, &[]).is_empty());
    }

    mod spans_in_layout {
        use super::*;
        use crate::element::TextStyle;
        use crate::font::{test_font_path, FontRegistry};
        use crate::text::{ShapedLayout, TextLayout};

        fn placed(src: Option<&str>, texts: &[&str]) -> Vec<Option<PlacedWord>> {
            let fonts = FontRegistry::new();
            fonts.add_file(&test_font_path()).unwrap();
            let layout = ShapedLayout::new(fonts);
            let w = words(texts);
            let s = live_spans(&w, src);
            let shaped = layout.layout(&s.text, &TextStyle::new(0, 20.0, 26.0), Some(1000.0));
            place(&shaped, &w, &s.ranges)
        }

        /// Each word's glyph offsets are bytes into the word, one per letter here, and a
        /// hard newline starts a new line.
        #[test]
        fn words_keep_their_glyphs_across_spaces_and_newlines() {
            let texts = ["hello", "big", "world"];
            let single = placed(None, &texts);
            let runs = placed(Some("hello    big world"), &texts);
            let broken = placed(Some("hello\nbig\n\nworld"), &texts);
            for p in [&single, &runs, &broken] {
                for (p, t) in p.iter().zip(texts) {
                    let p = p.as_ref().expect("every word is on screen");
                    let offsets: Vec<u32> = p.glyphs.iter().map(|g| g.offset).collect();
                    assert_eq!(offsets, (0..t.len() as u32).collect::<Vec<_>>());
                }
            }
            let at = |p: &[Option<PlacedWord>], i: usize| p[i].as_ref().map(|p| (p.x, p.y, p.line)).unwrap();
            assert_eq!(at(&runs, 0), at(&single, 0));
            assert!(at(&runs, 1).0 > at(&single, 1).0 + 1.0, "a run of spaces pushes the next word along");
            assert_eq!([at(&single, 0).2, at(&single, 1).2, at(&single, 2).2], [0, 0, 0]);
            let (b, w) = (at(&broken, 1), at(&broken, 2));
            assert_eq!((at(&broken, 0).2, b.2, w.2), (0, 1, 2));
            assert_eq!(b.0, at(&broken, 0).0, "a word after a newline starts the line");
            assert!(w.1 - b.1 > 1.5 * 26.0, "a blank line between them: {} {}", b.1, w.1);
        }
    }

    #[test]
    fn edit_caret_blinks_in_steps_and_wakes_only_on_edges() {
        let d = EditDecor { caret: Some(0), since: 1.0, ..EditDecor::default() };
        let mut t = 1.0;
        let mut edges = Vec::new();
        while let Some(next) = d.wake(t) {
            assert!(next > t, "{next} after {t}");
            // Between two edges the caret does not change, and it is fully on or off.
            let a = d.caret_alpha(t);
            assert!(a == 0.0 || a == 1.0);
            for k in 1..8 {
                assert_eq!(d.caret_alpha(t + (next - t) * k as f64 / 8.0), a, "between {t} and {next}");
            }
            edges.push(next);
            t = next;
        }
        assert_eq!(edges[0], 1.0 + EDIT_HOLD, "solid while typing");
        assert!((edges.last().unwrap() - (1.0 + EDIT_STILL)).abs() < 1e-9);
        assert_eq!(d.caret_alpha(1.0 + EDIT_STILL + 5.0), 1.0, "on and still after the blink stops");
        let period = 1.0 / CARET_HZ;
        assert!((edges[1] - edges[0] - CARET_DUTY * period).abs() < 1e-5);
        assert!((edges[2] - edges[0] - period).abs() < 1e-5);
        assert_eq!(d.caret_alpha(edges[1] + 0.01), 0.0);
        assert_eq!(EditDecor { caret: None, ..d }.wake(1.0), None, "no caret, no wake");
    }

    mod edit_decor {
        use super::*;
        use crate::display::{DisplayList, ItemKind};
        use crate::element::{Element, TextStyle};
        use crate::font::{test_font_path, FontRegistry};
        use crate::text::ShapedLayout;
        use crate::Scene;

        const SIZE: f32 = 20.0;
        const STEP: f64 = 1.0 / 120.0;

        fn scene() -> Scene<()> {
            let fonts = FontRegistry::new();
            fonts.add_file(&test_font_path()).unwrap();
            Scene::new().with_text(Box::new(ShapedLayout::new(fonts)))
        }

        fn view(words: &[(WordId, &str)], decor: EditDecor, turn: f32, scale: f32) -> Element<()> {
            let w: Vec<Word> = words.iter().map(|&(id, t)| Word { id, text: t.into() }).collect();
            let src = words.iter().map(|w| w.1).collect::<Vec<_>>().join(" ");
            let live = Element::live_text("live", &w)
                .text_style(TextStyle::new(0, SIZE, 26.0))
                .width(200.0)
                .live_source(src)
                .edit(decor);
            Element::column("root").child(Element::column("box").rotate(turn).scale(scale).child(live))
        }

        /// The layer an instance of `kind` at `index` is drawn in.
        fn layer_of(dl: &DisplayList, kind: ItemKind, index: u32) -> usize {
            dl.layers
                .iter()
                .position(|l| {
                    dl.items[l.items.start as usize..l.items.end as usize]
                        .iter()
                        .any(|i| i.kind == kind && (i.start..i.start + i.len).contains(&index))
                })
                .expect("drawn in a layer")
        }

        /// The caret bar (index, x, middle y) and the last glyph of word `id` (index, x, y).
        fn caret_and_glyph(dl: &DisplayList, id: WordId) -> ((u32, f32, f32), (u32, f32, f32)) {
            let (ci, c) = dl
                .shapes
                .iter()
                .enumerate()
                .find(|(_, s)| s.geom[2] == 2.0 && s.geom[3] == SIZE)
                .expect("the caret is drawn");
            let (gi, g) = dl
                .glyphs
                .iter()
                .enumerate()
                .filter(|(_, g)| g.word == id)
                .max_by(|a, b| a.1.pos.x.total_cmp(&b.1.pos.x))
                .expect("the word is drawn");
            ((ci as u32, c.geom[0], c.geom[1] + 0.5 * c.geom[3]), (gi as u32, g.pos.x, g.pos.y))
        }

        /// R33: "hello big world", caret after "big"; then "really" goes in before "big",
        /// so "big" slides along its line and "world" wraps. In every frame of that reflow
        /// spring the caret keeps its laid-out place relative to the last glyph of "big",
        /// within 1 px, also inside a rotated and scaled parent, where it is drawn in the
        /// glyphs' own layer.
        #[test]
        fn caret_stays_on_the_glyphs_through_a_reflow() {
            for (turn, scale) in [(0.0, 1.0), (0.4, 1.6)] {
                let mut s = scene();
                let caret = |at: usize| EditDecor { caret: Some(at), since: 1.0, ..EditDecor::default() };
                s.apply(view(&[(1, "hello"), (2, "big"), (3, "world")], caret(9), turn, scale), 0.0);
                s.apply(view(&[(1, "hello"), (4, "really"), (2, "big"), (3, "world")], caret(16), turn, scale), 1.0);
                let frames: Vec<f64> = (0..58).map(|k| 1.0 + k as f64 * STEP).collect();
                let at = |t: f64| {
                    let dl = s.sample(t);
                    let ((ci, cx, cy), (gi, gx, gy)) = caret_and_glyph(&dl, 2);
                    let same = layer_of(&dl, ItemKind::Shapes, ci) == layer_of(&dl, ItemKind::Glyphs, gi);
                    assert!(same, "caret and glyphs in one layer at {t}");
                    (cx - gx, cy - gy, gx)
                };
                let (rx, ry, end) = at(*frames.last().unwrap());
                let (_, _, start) = at(1.0);
                assert!(end - start > 20.0, "\"big\" slides along: {start} to {end}");
                for &t in &frames {
                    let (dx, dy, _) = at(t);
                    assert!((dx - rx).abs() <= 1.0 && (dy - ry).abs() <= 1.0, "{turn}: {t}: {dx} {dy} vs {rx} {ry}");
                }
                if turn != 0.0 {
                    let dl = s.sample(2.0);
                    let ((ci, ..), _) = caret_and_glyph(&dl, 2);
                    let m = dl.layers[layer_of(&dl, ItemKind::Shapes, ci)].transform;
                    assert_ne!(m, crate::geom::Affine::IDENTITY, "the parent's turn applies to the caret's layer");
                }
            }
        }

        #[test]
        fn selection_goes_behind_the_glyphs_and_marks_underline() {
            let mut s = scene();
            let decor = EditDecor {
                caret: Some(5),
                selection: 0..5,
                marks: vec![std::ops::Range { start: 6, end: 9 }],
                since: 0.0,
                ..Default::default()
            };
            s.apply(view(&[(1, "hello"), (2, "big"), (3, "world")], decor, 0.0, 1.0), 0.0);
            let dl = s.sample(1.0);
            let item_of = |kind: ItemKind, index: u32| {
                dl.items.iter().position(|i| i.kind == kind && (i.start..i.start + i.len).contains(&index)).unwrap()
            };
            let glyph = |id: WordId| dl.glyphs.iter().enumerate().filter(move |(_, g)| g.word == id);
            let sel = dl.shapes.iter().position(|s| s.geom[3] == 26.0).expect("a selection fill");
            let first = glyph(1).map(|(i, _)| i).min().unwrap();
            assert!(item_of(ItemKind::Shapes, sel as u32) < item_of(ItemKind::Glyphs, first as u32));
            let r = dl.shapes[sel].geom;
            let hello: Vec<f32> = glyph(1).map(|(_, g)| g.pos.x).collect();
            assert!(r[0] <= hello[0] + 0.5 && r[0] + r[2] > *hello.last().unwrap());
            let line = |s: &&crate::display::ShapeInstance| s.geom[3] < 2.0 && s.geom[2] > 10.0;
            let mark = dl.shapes.iter().find(line).expect("an underline").geom;
            let big: Vec<(f32, f32)> = glyph(2).map(|(_, g)| (g.pos.x, g.pos.y)).collect();
            assert!((mark[0] - big[0].0).abs() < 1.0, "under \"big\": {mark:?} {big:?}");
            assert!(mark[1] > big[0].1 && mark[1] < big[0].1 + 0.3 * SIZE, "just under the baseline");
            assert!(mark[0] + mark[2] < glyph(3).map(|(_, g)| g.pos.x).fold(f32::MAX, f32::min));
        }

        /// A decor with no caret hides the node's end-of-text caret unless it asks for it
        /// with `end_caret`; with an edit caret, the end caret never shows.
        #[test]
        fn a_marks_only_decor_keeps_the_end_caret_when_asked() {
            let end = Caret { width: 3.0, ..Caret::default() };
            let carets = |decor: EditDecor| {
                let mut s = scene();
                let root = view(&[(1, "hello"), (2, "big")], decor, 0.0, 1.0);
                s.apply(root.map_live_texts(&mut |e| e.caret(end)), 0.0);
                let dl = s.sample(0.2);
                // Bars as tall as the text: the carets (a mark is 1 or 2 px high).
                dl.shapes.iter().filter(|s| s.geom[3] > 0.5 * SIZE).map(|s| s.geom[2]).collect::<Vec<_>>()
            };
            let hello = std::ops::Range { start: 0, end: 5 };
            let marks = EditDecor { marks: vec![hello], since: 0.0, ..EditDecor::default() };
            assert_eq!(carets(marks.clone()), Vec::<f32>::new(), "no caret by default");
            assert_eq!(carets(EditDecor { end_caret: true, ..marks.clone() }), vec![3.0], "the end caret");
            let edit = EditDecor { caret: Some(2), end_caret: true, ..marks };
            assert_eq!(carets(edit), vec![2.0], "only the edit caret");
        }

        /// The end caret never stands inside the text: as a word grows or a new one comes on
        /// its line it is after the last glyph from the first frame; back to a shorter end it
        /// still glides.
        #[test]
        fn the_end_caret_jumps_on_and_glides_back() {
            let end = Caret { width: 3.0, ..Caret::default() };
            let words = |w: &[(WordId, &str)]| {
                let w: Vec<Word> = w.iter().map(|&(id, t)| Word { id, text: t.into() }).collect();
                Element::column("root").child(
                    Element::live_text("live", &w).text_style(TextStyle::new(0, SIZE, 26.0)).width(300.0).caret(end),
                )
            };
            // The caret's left edge and the right edge of the last glyph, at `t`.
            let at = |s: &mut Scene<()>, t: f64| {
                let dl = s.sample(t);
                let caret = dl.shapes.iter().find(|s| s.geom[2] == 3.0).expect("the caret").geom[0];
                let ink = dl.glyphs.iter().map(|g| g.pos.x).fold(f32::MIN, f32::max);
                (caret, ink)
            };
            let mut s = scene();
            s.apply(words(&[(1, "open")]), 0.0);
            s.sample(0.5);
            for (k, text) in [(1.0, "open"), (1.1, "open conf"), (1.2, "open configuration")] {
                let w: Vec<(WordId, &str)> = text.split(' ').enumerate().map(|(i, t)| (i as WordId + 1, t)).collect();
                s.apply(words(&w), k);
                let (caret, ink) = at(&mut s, k + STEP);
                assert!(caret > ink, "{text}: caret at {caret}, last glyph starts at {ink}");
            }
            let settled = at(&mut s, 2.0).0;
            s.apply(words(&[(1, "open")]), 2.0);
            let back = at(&mut s, 2.0 + STEP).0;
            assert!(back > at(&mut s, 3.0).0 + 10.0 && back <= settled, "back along the line it glides: {back}");
        }

        /// The display list says where the caret meets its baseline, in surface space, for
        /// the envelope check, also while the caret blinks off.
        #[test]
        fn the_caret_foot_is_in_the_display_list() {
            let mut s = scene();
            let decor = EditDecor { caret: Some(9), since: 0.0, ..EditDecor::default() };
            s.apply(view(&[(1, "hello"), (2, "big"), (3, "world")], decor.clone(), 0.0, 1.0), 0.0);
            let off = (0..400).map(|k| 1.0 + k as f64 * 0.005).find(|&t| decor.caret_alpha(t) == 0.0).unwrap();
            for t in [0.2, off] {
                let dl = s.sample(t);
                let foot = dl.edit_caret.expect("the caret foot");
                let g = dl.glyphs.iter().filter(|g| g.word == 2).max_by(|a, b| a.pos.x.total_cmp(&b.pos.x)).unwrap();
                assert!((foot.y - g.pos.y).abs() < 0.01 && foot.x > g.pos.x, "{t}: {foot:?} {:?}", g.pos);
            }
            s.apply(Element::column("root"), 2.0);
            assert_eq!(s.sample(2.0).edit_caret, None);
        }

        /// R43: once the text is at rest, the scene wakes only at the caret's blink edges,
        /// and not at all after `EDIT_STILL`. Without a decor the node has no caret.
        #[test]
        fn an_idle_editor_wakes_only_for_the_blink() {
            let mut s = scene();
            let decor = EditDecor { caret: Some(3), since: 0.0, ..Default::default() };
            s.apply(view(&[(1, "hello"), (2, "big")], decor.clone(), 0.0, 1.0), 0.0);
            for t in [2.0, 2.3, 4.01, 7.77] {
                assert_eq!(s.next_wake(t), decor.wake(t), "{t}");
            }
            assert_eq!(s.next_wake(EDIT_STILL + 0.1), None);
            assert!(caret_and_glyph(&s.sample(EDIT_STILL + 0.1), 2).0 .1.is_finite());
            // Moving the caret alone is no new layout: the glyphs stay where they are.
            let before: Vec<_> = s.sample(20.0).glyphs.iter().map(|g| g.pos).collect();
            s.apply(
                view(&[(1, "hello"), (2, "big")], EditDecor { caret: Some(9), since: 20.0, ..decor }, 0.0, 1.0),
                20.0,
            );
            let after: Vec<_> = s.sample(20.0).glyphs.iter().map(|g| g.pos).collect();
            assert_eq!(before, after);
            assert_eq!(s.next_wake(20.0), Some(20.0 + EDIT_HOLD));
        }
    }

    /// M4.8: under the still profile a word that enters, changes or leaves only changes
    /// ink, for at most 80 ms, even with a look's own enter and change transitions and the
    /// palimpsest revise.
    mod still_profile {
        use super::*;
        use crate::display::DisplayList;
        use crate::element::{Element, Prop, TextStyle, Transition};
        use crate::font::{test_font_path, FontRegistry};
        use crate::text::ShapedLayout;
        use crate::Scene;
        use vo_core::spring::Spring;

        const STEP: f64 = 1.0 / 240.0;

        fn scene() -> Scene<()> {
            let fonts = FontRegistry::new();
            fonts.add_file(&test_font_path()).unwrap();
            Scene::new().with_text(Box::new(ShapedLayout::new(fonts)))
        }

        fn view(words: &[(WordId, &str)], still: bool) -> Element<()> {
            let w: Vec<Word> = words.iter().map(|&(id, t)| Word { id, text: t.into() }).collect();
            // A look that moves words a lot on its own.
            let rise =
                Transition { opacity: Some(0.0), offset: Some((0.0, 12.0)), blur: Some(4.0), ..Transition::NONE };
            Element::column("root").child(
                Element::live_text("live", &w)
                    .text_style(TextStyle::new(0, 20.0, 26.0))
                    .width(400.0)
                    .motion(Prop::Opacity, Spring::critical_to_98(0.3))
                    .motion(Prop::Offset, Spring::critical_to_98(0.3))
                    .word_enter(rise)
                    .word_change(rise)
                    .still_motion(still),
            )
        }

        /// Each drawn glyph: word, glyph, position, size and ink.
        fn glyphs(dl: &DisplayList) -> Vec<(u32, u32, Point, f32, f32)> {
            dl.glyphs.iter().map(|g| (g.word, g.glyph, g.pos, g.size, g.color.a)).collect()
        }

        /// Runs `from` then `to` at 1.0. Returns whether every frame drew each glyph only where
        /// it rests before or after, at its size and with no blur, and the first time from
        /// which every frame matches the end within 1 % ink (a glyph under 1 % counts as gone).
        /// Also returns the lowest ink a new letter (one with a place only after the change) is
        /// drawn with.
        fn run(from: &[(WordId, &str)], to: &[(WordId, &str)], still: bool) -> (bool, f64, f32) {
            let mut s = scene();
            s.apply(view(from, still), 0.0);
            let before = glyphs(&s.sample(0.99));
            s.apply(view(to, still), 1.0);
            let after = glyphs(&s.sample(3.0));
            let at_rest = |g: &(u32, u32, Point, f32, f32)| {
                before.iter().chain(&after).any(|r| {
                    r.0 == g.0
                        && r.1 == g.1
                        && (r.2.x - g.2.x).abs() < 0.01
                        && (r.2.y - g.2.y).abs() < 0.01
                        && r.3 == g.3
                })
            };
            let near = |a: &(u32, u32, Point, f32, f32), b: &(u32, u32, Point, f32, f32)| {
                a.0 == b.0 && a.1 == b.1 && (a.2.x - b.2.x).abs() < 0.01 && (a.2.y - b.2.y).abs() < 0.01
            };
            let (mut in_place, mut done, mut new_ink) = (true, 1.0, 1.0f32);
            let mut t = 1.0;
            while t < 1.5 {
                let dl = s.sample(t);
                let mut now = glyphs(&dl);
                in_place &= now.iter().filter(|g| g.4 > 0.0).all(at_rest);
                for g in now.iter().filter(|g| g.4 > 0.0 && !before.iter().any(|b| near(g, b))) {
                    new_ink = new_ink.min(g.4);
                }
                // Under 1 % ink a glyph is gone.
                now.retain(|g| g.4 >= 0.01);
                in_place &= dl.glyph_fx.iter().all(|(_, fx)| fx.blur == 0.0);
                let same = now.len() == after.len()
                    && now.iter().zip(&after).all(|(a, b)| (a.4 - b.4).abs() < 0.01 && at_rest(a));
                if !same {
                    done = t + STEP;
                }
                t += STEP;
            }
            (in_place, done, new_ink)
        }

        const HELLO: &[(WordId, &str)] = &[(1, "hello")];
        const HELLO_WORLD: &[(WordId, &str)] = &[(1, "hello"), (2, "world")];
        const HELLO_WORD: &[(WordId, &str)] = &[(1, "hello"), (2, "word")];
        /// A space typed inside a word: the word keeps its tail, and its head is a new word.
        const SPLIT_BEFORE: &[(WordId, &str)] = &[(1, "configuration")];
        const SPLIT_AFTER: &[(WordId, &str)] = &[(2, "config"), (1, "uration")];

        #[test]
        fn enter_revise_and_exit_change_only_ink_for_80_ms() {
            for (name, from, to) in [
                ("enter", HELLO, HELLO_WORLD),
                ("revise", HELLO_WORLD, HELLO_WORD),
                ("exit", HELLO_WORLD, HELLO),
                ("split", SPLIT_BEFORE, SPLIT_AFTER),
            ] {
                let (in_place, done, new_ink) = run(from, to, true);
                assert!(in_place, "{name}: a glyph left its place");
                // Readable in the frame it shows (reading-line envelope, rule 1).
                assert!(new_ink >= crate::arrival::INK - 1e-3, "{name}: a new letter at {new_ink}");
                assert!(done > 1.0 && done <= 1.08 + 1e-9, "{name}: settled at {done}");
            }
        }

        /// The same changes without the profile move glyphs and take longer: the check above
        /// can tell.
        #[test]
        fn without_the_profile_the_look_moves_words() {
            for (from, to) in [(HELLO, HELLO_WORLD), (HELLO_WORLD, HELLO_WORD)] {
                let (in_place, done, _) = run(from, to, false);
                assert!(!in_place && done > 1.15, "{done}");
            }
        }

        /// Typing inside a word under the profile: the letters a key leaves alone stay at
        /// full ink and are drawn once, a removed letter fades where it stood, and a typed
        /// letter ends at full ink.
        #[test]
        fn a_key_inside_a_word_keeps_the_other_letters() {
            for (from, to) in [("hello", "helxlo"), ("hello", "helo")] {
                let mut s = scene();
                s.apply(view(&[(1, from)], true), 0.0);
                let before = glyphs(&s.sample(0.99));
                s.apply(view(&[(1, to)], true), 1.0);
                let after = glyphs(&s.sample(3.0));
                let gone = before.len().saturating_sub(after.len());
                assert!(after.iter().all(|g| g.4 >= 0.99), "{to} at rest: {after:?}");
                let mut t = 1.0;
                while t < 1.5 {
                    let now: Vec<_> = glyphs(&s.sample(t)).into_iter().filter(|g| g.4 > 0.0).collect();
                    assert!(now.len() <= after.len() + gone, "{to} at {t}: {} glyphs drawn", now.len());
                    // A kept letter: at its place after the change, and drawn before.
                    let kept = after.iter().filter(|a| {
                        before.iter().filter(|b| b.1 == a.1).count() >= after.iter().filter(|b| b.1 == a.1).count()
                    });
                    for k in kept {
                        let g = now.iter().find(|g| g.1 == k.1 && (g.2.x - k.2.x).abs() < 0.01);
                        assert!(g.is_some_and(|g| g.4 >= 0.99), "{to} at {t}: glyph {} at {:?}", k.1, g);
                    }
                    // Whatever is fading is on the baseline it was drawn on.
                    assert!(now.iter().all(|g| (g.2.y - after[0].2.y).abs() < 0.01), "{to} at {t}");
                    t += STEP;
                }
            }
        }

        /// R34: turning the profile on while a word is entering settles it within 150 ms.
        #[test]
        fn running_motion_settles_when_the_profile_turns_on() {
            let mut s = scene();
            s.apply(view(HELLO, false), 0.0);
            s.apply(view(HELLO_WORLD, false), 1.0);
            s.apply(view(HELLO_WORLD, true), 1.03);
            let end = glyphs(&s.sample(3.0));
            let now = glyphs(&s.sample(1.03 + 0.15));
            assert_eq!(now.len(), end.len());
            for (a, b) in now.iter().zip(&end) {
                assert!((a.2.x - b.2.x).abs() < 0.01 && (a.2.y - b.2.y).abs() < 0.01 && (a.4 - b.4).abs() < 0.01);
            }
        }
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
