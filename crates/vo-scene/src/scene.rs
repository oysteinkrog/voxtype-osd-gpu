//! The retained scene: nodes reconciled from elements by key path, each owning its animated
//! properties as `vo-core` channels.
//!
//! - `apply(view, t)` runs when app state changes, at the event's timestamp. Every changed
//!   target starts a new spring segment at exactly `t`, so a 144 Hz session and a 60 fps
//!   export give the same motion.
//! - `sample(t)` is pure. It reads the channels at `t` and builds the display list.
//! - `next_wake(t)` is `None` once everything has settled, so the host draws nothing.
//!
//! Presence: a node with `.presence(enter, exit)` starts at `enter` and springs to its
//! targets. When the view drops it, it springs to `exit` and stays, out of layout, until that
//! settles. Without `.presence` a dropped node goes at once.

use slotmap::{new_key_type, Key as _, KeyData, SlotMap};
use taffy::prelude::AvailableSpace;
use vo_core::channel::{Channel, Rest};
use vo_core::spring::Spring;

use crate::bind::Bound;
use crate::display::MATERIAL_SHIFT;
use crate::display::{
    Backdrop, BackdropRes, BlendMode, Displace, Bloom, DisplayList, DotInstance, GlyphFx, GlyphInstance, HitShape, Item, ItemKind, Layer, LayerStyle,
    LayerTarget, Paint, ShapeInstance, ShapeKind, Trail, NO_WORD,
};
use crate::display::{BloomBand, BloomShape};
use crate::dots::Dots;
use crate::element::BAND_SLOTS;
use crate::element::{Element, Handlers, Key, Kind, MarkShape, Motion, Prop, Slot, Transition, DEFAULT_MOTION};
use crate::emitter::{ParticleWord, ParticleWords, WORD_PARTICLES_BUSY};
use crate::final_pass::{self, FinalPass};
use crate::flow::{Flow, FlowInput};
use crate::geom::{Affine, Color, Point, RRect, Rect, Size};
use crate::glass::{Glass, GlassInstance};
use crate::inline::OBJECT_GLYPH;
use crate::input::PointerState;
use crate::layout::LayoutView;
use crate::material::Material;
use crate::text::{EstimateText, ShapedText, TextLayout, ELLIPSIS_CLUSTER};
use crate::typo::Envelope;
use vo_core::signals::{SignalBlock, SignalsCell};

new_key_type! {
    /// A retained node. Stable for as long as its key path stays in the view.
    pub struct NodeId;
}

impl NodeId {
    pub fn to_bits(self) -> u64 {
        self.data().as_ffi()
    }

    pub fn from_bits(bits: u64) -> NodeId {
        KeyData::from_ffi(bits).into()
    }
}

/// Frame rate of ambient loops such as an orbiting rim (FX08).
const AMBIENT_FPS: f64 = 30.0;
/// Glitch patterns per second (FX13).
const GLITCH_FPS: f64 = 15.0;
/// Flicker levels per second (FX13): six changes make at most three flashes a second.
const FLICKER_FPS: f64 = 6.0;

/// Where a node is in its life, at some time `t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Presence {
    /// Created at `since`; its enter motion is still running.
    Entering {
        since: f64,
    },
    Present,
    /// Dropped from the view at `since`; drawn until `until`, then removed.
    Leaving {
        since: f64,
        until: f64,
    },
}

pub(crate) struct Node<M> {
    pub key: Key,
    pub parent: Option<NodeId>,
    pub kind: Kind,
    pub style: taffy::Style,
    pub cache: taffy::Cache,
    pub layout: taffy::Layout,
    pub hoisted: Vec<taffy::NodeId>,
    /// Paint order, leaving children included.
    pub children: Vec<NodeId>,
    /// Layout order: the children that are not leaving.
    pub flow: Vec<NodeId>,
    /// Sorted by slot.
    pub chans: Vec<(u8, Channel)>,
    pub motion: Vec<(Prop, Motion)>,
    /// Signal bus bindings, and the store they read (set only when there are bindings).
    pub binds: Vec<Bound>,
    pub signals: Option<SignalsCell>,
    pub presence: Option<(Transition, Transition)>,
    pub gradient: bool,
    pub hairline: bool,
    pub inside: bool,
    pub glass: Option<Glass>,
    pub material: Option<Material>,
    pub afterglow: Option<[f32; 3]>,
    pub handlers: Handlers<M>,
    pub born: f64,
    /// `(since, until)`.
    pub leaving: Option<(f64, f64)>,
    /// When the node last entered: created, or brought back while leaving.
    pub since: f64,
    /// When the enter motion settles.
    pub entered: f64,
    pub shaped: ShapedText,
    /// The font size `shaped` was laid out at: the style's, or smaller with autofit.
    pub fitted: f32,
    pub has_layout: bool,
    /// Live text: each word's own motion, sorted by word id.
    pub words: Vec<WordAnim>,
    /// Live text: when a word last came or went.
    pub words_at: f64,
    /// Live text: the current-word mark's motion.
    pub mark: Option<MarkAnim>,
    /// Live text: when the final pass appeared, and the words it changed (MO-40).
    pub pass_since: Option<f64>,
    pub pass_changed: Vec<final_pass::Changed>,
    /// Live text: where each word is on screen, sprung toward its laid-out place.
    pub live: Flow,
    /// The motion of a property no `.motion()` call names: the scene's default.
    pub default_motion: Motion,
    /// Reduced motion (MO09): travel and scale jump, effects change slowly.
    pub reduced: bool,
    /// Ambient loops and ripples (FX-203, FX-101).
    pub ambient: crate::ambient::NodeAmbient,
    /// Live text: per-word typographic channels (MO-03).
    pub typo: crate::typo::TypoState,
}

/// One live-text word's motion: opacity, an offset from its laid-out place, and its text
/// effects (blur, dissolve and wipe, TY17).
pub(crate) struct WordAnim {
    pub id: u32,
    pub text: String,
    /// When the word entered or its text last changed.
    pub since: f64,
    pub opacity: Channel,
    pub dx: Channel,
    pub dy: Channel,
    pub blur: Channel,
    pub dissolve: Channel,
    pub wipe: Channel,
    /// 1 while the word holds the current-word role and its mark has an accent colour.
    pub accent: Channel,
    /// When the final pass reached the word, for its brighten pulse. Can be ahead of the
    /// time it was set, for words the pass reached together.
    pub passed: Option<f64>,
}

impl WordAnim {
    fn channels(&self) -> [&Channel; 7] {
        [&self.opacity, &self.dx, &self.dy, &self.blur, &self.dissolve, &self.wipe, &self.accent]
    }
}

/// How fast the accent colour leaves a word and reaches the next.
const ACCENT: Spring = Spring::critical_to_98(0.150);
/// Both edges of a box or underline mark.
const MARK_MOVE: Spring = Spring::critical_to_98(0.180);
/// Karaoke: the leading edge reaches the next word first, the trailing edge follows.
const MARK_LEAD: Spring = Spring::critical_to_98(0.070);
const MARK_TRAIL: Spring = Spring::critical_to_98(0.220);

/// Live text: the current-word mark. The live piece follows the current word's sprung box
/// (`Flow::word_bounds`), so it moves with the word when the text reflows. On a hop each
/// edge blends from where the mark was drawn to the new word's box, by `p0` and `p1` (0 to
/// 1). Pieces still running off lines the mark left are kept in `gone`.
pub(crate) struct MarkAnim {
    word: Option<u32>,
    /// The current word's baseline in the last layout, to tell a line jump.
    line: f32,
    /// Where the edges start a hop from.
    from: (f32, f32),
    /// The live piece is clipped to this range: the new line's start while it enters there.
    clip: (f32, f32),
    p0: Channel,
    p1: Channel,
    opacity: Channel,
    gone: Vec<MarkPiece>,
}

/// `Flow::word_bounds` puts a word's top this many ems above its baseline.
const BOUNDS_ASCENT: f32 = 0.78;
const NO_CLIP: (f32, f32) = (f32::NEG_INFINITY, f32::INFINITY);

impl MarkAnim {
    fn channels(&self) -> impl Iterator<Item = &Channel> {
        let gone = self.gone.iter().flat_map(|p| [&p.x0, &p.x1, &p.opacity]);
        gone.chain([&self.p0, &self.p1, &self.opacity])
    }

    /// The live piece at `t` as `(baseline, x0, x1)`, or `None` when no word holds the role.
    fn live_span(&self, flow: &Flow, t: f64, size: f32) -> Option<(f32, f32, f32)> {
        let b = flow.word_bounds(self.word?, t, size)?;
        let (p0, p1) = (self.p0.value(t) as f32, self.p1.value(t) as f32);
        let x0 = self.from.0 + (b.x - self.from.0) * p0;
        let x1 = self.from.1 + (b.x + b.w - self.from.1) * p1;
        Some((b.y + BOUNDS_ASCENT * size, x0.max(self.clip.0), x1.min(self.clip.1)))
    }

    /// The pieces to draw at `t`: `(baseline, x0, x1, opacity)`.
    fn pieces(&self, flow: &Flow, t: f64, size: f32) -> Vec<(f32, f32, f32, f32)> {
        let mut v: Vec<_> = self.gone.iter().filter_map(|p| p.drawn(t)).collect();
        let k = self.opacity.value(t).clamp(0.0, 1.0) as f32;
        if let Some((baseline, x0, x1)) = self.live_span(flow, t, size) {
            if k > 0.0 && x1 - x0 > 0.25 {
                v.push((baseline, x0, x1, k));
            }
        }
        v
    }
}

/// A piece of the mark left behind on a line, drawn from `x0` to `x1` clipped to `lo..hi`.
struct MarkPiece {
    baseline: f32,
    lo: f32,
    hi: f32,
    x0: Channel,
    x1: Channel,
    opacity: Channel,
}

impl MarkPiece {
    fn drawn(&self, t: f64) -> Option<(f32, f32, f32, f32)> {
        let k = self.opacity.value(t).clamp(0.0, 1.0) as f32;
        let (x0, x1) = ((self.x0.value(t) as f32).max(self.lo), (self.x1.value(t) as f32).min(self.hi));
        (k > 0.0 && x1 - x0 > 0.25).then_some((self.baseline, x0, x1, k))
    }

    fn settled(&self, t: f64) -> bool {
        [&self.x0, &self.x1, &self.opacity].iter().all(|c| c.is_settled(t))
    }
}

/// The springs for the left and right edges of a mark moving right or left. With `lead` the
/// edge in front gets there first and the other follows; otherwise both move together.
fn edge_springs(right: bool, lead: bool) -> (Spring, Spring) {
    let (front, back) = if lead { (MARK_LEAD, MARK_TRAIL) } else { (MARK_MOVE, MARK_MOVE) };
    if right {
        (back, front)
    } else {
        (front, back)
    }
}

/// A live text's final pass, its word boxes `(id, x0, x1, baseline)` and its lines.
type PassLines = (FinalPass, Vec<(u32, f32, f32, f32)>, Vec<final_pass::ReadLine>);

/// Byte offset where each word starts in the joined text, with its id.
fn word_starts(words: &[vo_core::words::Word]) -> Vec<(u32, u32)> {
    let mut starts = Vec::with_capacity(words.len());
    let mut at = 0u32;
    for w in words {
        starts.push((at, w.id));
        at += w.text.len() as u32 + 1;
    }
    starts
}

/// The id of the word a glyph of `cluster` belongs to, or `NO_WORD`.
fn word_at(starts: &[(u32, u32)], cluster: u32) -> u32 {
    if cluster == ELLIPSIS_CLUSTER {
        return NO_WORD;
    }
    let i = starts.partition_point(|(s, _)| *s <= cluster);
    if i == 0 {
        NO_WORD
    } else {
        starts[i - 1].1
    }
}

/// Each word's ink extent on its first line, `(id, x0, x1, baseline)`, in layout order.
fn word_boxes(shaped: &ShapedText, starts: &[(u32, u32)]) -> Vec<(u32, f32, f32, f32)> {
    let mut out: Vec<(u32, f32, f32, f32)> = Vec::new();
    for g in &shaped.glyphs {
        let id = word_at(starts, g.cluster);
        if id == NO_WORD {
            continue;
        }
        let (x0, x1) = (g.pos.x, g.pos.x + g.advance);
        match out.iter_mut().find(|b| b.0 == id) {
            Some(b) if b.3 == g.pos.y => (b.1, b.2) = (b.1.min(x0), b.2.max(x1)),
            Some(_) => {}
            None => out.push((id, x0, x1, g.pos.y)),
        }
    }
    out
}

/// The extent of the words on the line at `baseline`.
fn line_extent(boxes: &[(u32, f32, f32, f32)], baseline: f32) -> (f32, f32) {
    boxes
        .iter()
        .filter(|b| b.3 == baseline)
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), b| (lo.min(b.1), hi.max(b.2)))
}

/// Live text with a final pass and final words: each word the pass has reached, by the
/// node's last layout, takes its final text (`final_pass::merge`). Records the words that
/// changed, for their nod and bracket, and when the pass appeared.
fn release_finals<M>(n: &mut Node<M>, kind: &mut Kind, t: f64) {
    let Kind::LiveText { words, pass, finals, .. } = kind else { return };
    let Some(pass) = pass else {
        n.pass_since = None;
        n.pass_changed.clear();
        return;
    };
    n.pass_since.get_or_insert(t);
    if finals.is_empty() {
        n.pass_changed.clear();
        return;
    }
    let boxes = match &n.kind {
        Kind::LiveText { words: old, .. } => word_boxes(&n.shaped, &word_starts(old)),
        _ => Vec::new(),
    };
    let lines = final_pass::read_lines(&boxes);
    let pos = pass.at * final_pass::total(&lines);
    let all = pass.at >= 1.0;
    let reached = |id: u32| {
        all || boxes.iter().find(|b| b.0 == id).is_some_and(|b| final_pass::reading_at(&lines, b.3, b.1) < pos)
    };
    let (shown, changed) = final_pass::merge(words, finals, reached);
    *words = shown;
    n.pass_changed.retain(|c| words.iter().any(|w| w.id == c.id));
    for (id, old) in changed {
        if !n.pass_changed.iter().any(|c| c.id == id) {
            n.pass_changed.push(final_pass::Changed { id, at: t, old });
        }
    }
}

/// The character at byte offset `cluster` of the words joined by spaces.
fn glyph_char(words: &[vo_core::words::Word], starts: &[(u32, u32)], cluster: u32) -> Option<char> {
    let i = starts.partition_point(|(s, _)| *s <= cluster).checked_sub(1)?;
    let w = words.get(i)?;
    w.text.get((cluster - starts[i].0) as usize..)?.chars().next()
}

/// `a` mixed toward `b` by `k`, straight alpha.
fn mix(a: Color, b: Color, k: f32) -> Color {
    let l = |x: f32, y: f32| x + (y - x) * k;
    Color::rgba(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b), l(a.a, b.a))
}

impl<M> Node<M> {
    pub fn value(&self, slot: u8, t: f64) -> f64 {
        self.base_value(slot, t) + self.ambient.offset(slot, t, self.reduced)
    }

    fn ripples(&self) -> Option<&crate::ambient::Ripples> {
        match &self.kind {
            Kind::Ripples(r) => Some(r),
            _ => None,
        }
    }

    /// When the node's ambient loops or ripples next change after `t`.
    fn ambient_wake(&self, t: f64) -> Option<f64> {
        self.ambient.wake(t, self.reduced, self.ripples())
    }

    /// When the node's ambient loops and ripples are at rest for good.
    fn ambient_rest_at(&self) -> f64 {
        self.ambient.rest_at(self.reduced, self.ripples())
    }

    /// The value from the slot's channel alone, without ambient loops.
    fn base_value(&self, slot: u8, t: f64) -> f64 {
        let v = match self.chans.binary_search_by_key(&slot, |(s, _)| *s) {
            Ok(i) => self.chans[i].1.value(t),
            Err(_) => Slot::default_value(slot),
        };
        if self.binds.is_empty() {
            return v;
        }
        let signals = self.signals.as_ref().map(SignalsCell::get);
        crate::bind::apply(&self.binds, signals.as_deref(), slot, v, t)
    }

    /// The rate of change of `slot` at `t`, per second.
    fn velocity(&self, slot: u8, t: f64) -> f64 {
        let base = match self.chans.binary_search_by_key(&slot, |(s, _)| *s) {
            Ok(i) => self.chans[i].1.sample(t).1,
            Err(_) => 0.0,
        };
        base + self.ambient.velocity(slot, t, self.reduced)
    }

    pub fn has(&self, slot: u8) -> bool {
        self.chans.binary_search_by_key(&slot, |(s, _)| *s).is_ok()
    }

    fn motion_for(&self, slot: u8) -> Motion {
        let prop = Slot::prop(slot);
        if self.reduced {
            match prop {
                Prop::Position | Prop::Size | Prop::Offset | Prop::Scale | Prop::Rotation | Prop::TextSize => {
                    return Motion::Snap
                }
                // No flashing: glow, bloom and shadow follow a level only slowly.
                Prop::Glow | Prop::Bloom | Prop::Shadow => return Motion::Spring(Spring::SLOW),
                _ => {}
            }
        }
        self.motion.iter().find(|(p, _)| *p == prop).map_or(self.default_motion, |(_, m)| *m)
    }

    fn new_channel(&self, slot: u8, t: f64, value: f64, retention: f64) -> Channel {
        let spring = match self.motion_for(slot) {
            Motion::Spring(s) => s,
            Motion::Snap => Spring::SOFT,
        };
        let rest = if Slot::is_unit(slot) { Rest::UNIT } else { Rest::PIXELS };
        Channel::new(t, value, spring).with_rest(rest).with_retention(retention)
    }

    /// Moves `slot` toward `value` from `t` with the slot's motion. A slot with no channel
    /// yet starts from its default value (opacity 1, offset 0), so a property that appears
    /// on a live node animates in like any other change. Returns whether the motion changed:
    /// false when the slot already heads to `value`.
    pub fn set_target(&mut self, slot: u8, value: f64, t: f64, retention: f64) -> bool {
        let i = match self.chans.binary_search_by_key(&slot, |(s, _)| *s) {
            Ok(i) => i,
            Err(i) => {
                let ch = self.new_channel(slot, t, Slot::default_value(slot), retention);
                self.chans.insert(i, (slot, ch));
                i
            }
        };
        let motion = self.motion_for(slot);
        let ch = &mut self.chans[i].1;
        let last = |ch: &Channel| ch.segments().last().map(|s| (s.t0, s.target));
        let before = last(ch);
        match motion {
            Motion::Spring(s) => {
                ch.set_spring(s);
                ch.retarget(t, value);
            }
            Motion::Snap => {
                if ch.target() != value || !ch.is_settled(t) {
                    ch.snap(t, value);
                }
            }
        }
        last(ch) != before
    }

    /// Puts `slot` at `value` at `t`, at rest, for an enter motion to start from.
    pub(crate) fn start_at(&mut self, slot: u8, value: f64, t: f64, retention: f64) {
        match self.chans.binary_search_by_key(&slot, |(s, _)| *s) {
            Ok(i) => self.chans[i].1.snap(t, value),
            Err(i) => {
                let ch = self.new_channel(slot, t, value, retention);
                self.chans.insert(i, (slot, ch));
            }
        }
    }

    fn settled_at(&self) -> f64 {
        let words = self.words.iter().flat_map(|w| w.channels());
        let mark = self.mark.iter().flat_map(|m| m.channels());
        let chans = self.chans.iter().map(|(_, c)| c).chain(words).chain(mark).map(|c| c.settled_at());
        chans.fold(self.typo.settled_at(), f64::max)
    }

    /// Live text: matches the word motions to `words` at `t`. A new id starts from `enter`, a
    /// kept id with new text jumps to `change`, and both spring to rest (opacity 1, no
    /// offset, no text effect). Returns whether any motion started or a word went.
    fn sync_words(&mut self, t: f64, retention: f64) -> bool {
        let Kind::LiveText { words, enter, change, live, .. } = &self.kind else {
            let had = !self.words.is_empty();
            self.words.clear();
            return had;
        };
        let ids: Vec<u32> = words.iter().map(|w| w.id).collect();
        let typo_changed = self.typo.sync(t, &ids, live.typo.as_ref(), self.reduced, retention);
        let spring =
            |prop: Prop| match self.motion.iter().find(|(p, _)| *p == prop).map_or(self.default_motion, |m| m.1) {
                Motion::Spring(s) => s,
                Motion::Snap => Spring::SNAP,
            };
        let (fade, slide) = (spring(Prop::Opacity), spring(Prop::Offset));
        let chan = |v: f64, s: Spring, rest: Rest| Channel::new(t, v, s).with_rest(rest).with_retention(retention);
        let reduced = self.reduced;
        let from = |tr: &Transition| {
            let tr = if reduced { tr.reduced() } else { *tr };
            let (x, y) = tr.offset.unwrap_or((0.0, 0.0));
            let fx = [tr.blur, tr.dissolve, tr.wipe].map(|v| v.unwrap_or(0.0) as f64);
            ([tr.opacity.unwrap_or(1.0) as f64, x as f64, y as f64], fx)
        };
        let mut changed = false;
        let mut next = Vec::with_capacity(words.len());
        for w in words {
            let old = self.words.binary_search_by_key(&w.id, |a| a.id).ok();
            let start = match old {
                None => Some(from(&live.arrival.apply(*enter))),
                Some(i) if self.words[i].text != w.text && *change != Transition::NONE => Some(from(change)),
                Some(_) => None,
            };
            let mut anim = match old {
                Some(i) => {
                    let a = &self.words[i];
                    WordAnim {
                        id: a.id,
                        text: w.text.clone(),
                        since: if a.text != w.text { t } else { a.since },
                        opacity: a.opacity.clone(),
                        dx: a.dx.clone(),
                        dy: a.dy.clone(),
                        blur: a.blur.clone(),
                        dissolve: a.dissolve.clone(),
                        wipe: a.wipe.clone(),
                        accent: a.accent.clone(),
                        passed: a.passed,
                    }
                }
                None => WordAnim {
                    id: w.id,
                    text: w.text.clone(),
                    since: t,
                    opacity: chan(1.0, fade, Rest::UNIT),
                    dx: chan(0.0, slide, Rest::PIXELS),
                    dy: chan(0.0, slide, Rest::PIXELS),
                    blur: chan(0.0, fade, Rest::PIXELS),
                    dissolve: chan(0.0, fade, Rest::UNIT),
                    wipe: chan(0.0, fade, Rest::UNIT),
                    accent: chan(0.0, ACCENT, Rest::UNIT),
                    passed: None,
                },
            };
            if let Some(([o, x, y], [b, d, w])) = start {
                let moves = [
                    (&mut anim.opacity, o, 1.0),
                    (&mut anim.dx, x, 0.0),
                    (&mut anim.dy, y, 0.0),
                    (&mut anim.blur, b, 0.0),
                    (&mut anim.dissolve, d, 0.0),
                    (&mut anim.wipe, w, 0.0),
                ];
                for (ch, v, rest) in moves {
                    if v != rest {
                        ch.snap(t, v);
                        ch.retarget(t, rest);
                        changed = true;
                    }
                }
            }
            next.push(anim);
        }
        next.sort_by_key(|a| a.id);
        changed |= next.len() != self.words.len() || next.iter().zip(&self.words).any(|(a, b)| a.id != b.id);
        if changed {
            self.words_at = t;
        }
        self.words = next;
        changed || typo_changed
    }

    /// Live text: moves the current-word accent and mark to the word that holds the role now,
    /// using the last layout. Returns whether anything started to move.
    fn sync_mark(&mut self, t: f64, retention: f64) -> bool {
        let Kind::LiveText { words, style, current, mark, .. } = &self.kind else {
            return self.mark.take().is_some();
        };
        let current = current.filter(|id| words.iter().any(|w| w.id == *id));
        let mark = mark.as_deref();
        let tint = mark.is_some_and(|m| m.tints());
        let mut changed = false;
        for w in &mut self.words {
            let on = if tint && Some(w.id) == current { 1.0 } else { 0.0 };
            if w.accent.target() != on {
                w.accent.retarget(t, on);
                changed = true;
            }
        }
        let Some(mark) = mark else {
            return changed | self.mark.take().is_some();
        };
        let lead = mark.shape == MarkShape::Karaoke && !self.reduced;
        let reduced = self.reduced;
        let size = style.size;
        let boxes = word_boxes(&self.shaped, &word_starts(words));
        let target = current.and_then(|id| boxes.iter().find(|b| b.0 == id)).copied();
        let chan =
            |v: f32, s: Spring, rest: Rest| Channel::new(t, v as f64, s).with_rest(rest).with_retention(retention);
        let m = self.mark.get_or_insert_with(|| MarkAnim {
            word: None,
            line: 0.0,
            from: (0.0, 0.0),
            clip: NO_CLIP,
            p0: chan(1.0, MARK_MOVE, Rest::UNIT),
            p1: chan(1.0, MARK_MOVE, Rest::UNIT),
            opacity: chan(0.0, ACCENT, Rest::UNIT),
            gone: Vec::new(),
        });
        m.gone.retain(|p| !p.settled(t));
        if target.map(|b| b.0) == m.word {
            // The same word: the live piece follows its box; only the line may change.
            if let Some(b) = target {
                m.line = b.3;
            }
            if m.p0.is_settled(t) && m.p1.is_settled(t) {
                m.clip = NO_CLIP;
            }
            return changed;
        }
        let drawn = m.live_span(&self.live, t, size);
        let k = m.opacity.value(t).clamp(0.0, 1.0) as f32;
        let piece = |baseline: f32, (lo, hi): (f32, f32), (x0, x1): (f32, f32)| MarkPiece {
            baseline,
            lo,
            hi,
            x0: chan(x0, MARK_MOVE, Rest::PIXELS),
            x1: chan(x1, MARK_MOVE, Rest::PIXELS),
            opacity: chan(k, MARK_TRAIL, Rest::UNIT),
        };
        match (drawn, target) {
            (None, None) => m.word = None,
            (Some((baseline, x0, x1)), None) => {
                // No word holds the role: the mark fades where it is.
                let mut p = piece(baseline, NO_CLIP, (x0, x1));
                p.opacity.set_spring(ACCENT);
                p.opacity.retarget(t, 0.0);
                m.gone.push(p);
                m.word = None;
                m.opacity.snap(t, 0.0);
            }
            (None, Some((id, x0, x1, line))) => {
                // Nothing was showing: fade in on the word.
                (m.word, m.line, m.from, m.clip) = (Some(id), line, (x0, x1), NO_CLIP);
                m.p0.snap(t, 1.0);
                m.p1.snap(t, 1.0);
                m.opacity.retarget(t, 1.0);
            }
            (Some((baseline, x0, x1)), Some((id, tx0, tx1, line))) => {
                let right = if reduced {
                    true
                } else if line != m.line {
                    // A line jump: the old piece runs off its line's end (its start when the
                    // word is on an earlier line) and the live piece enters from the other side
                    // of the new line.
                    let down = line > m.line;
                    let old_line = line_extent(&boxes, m.line);
                    let (lo, hi) = if old_line.0 <= old_line.1 { old_line } else { (x0, x1) };
                    let w = x1 - x0;
                    let mut p = piece(baseline, (lo, hi), (x0, x1));
                    let (s0, s1) = edge_springs(down, lead);
                    p.x0.set_spring(s0);
                    p.x1.set_spring(s1);
                    let (e0, e1) = if down { (hi, hi + w) } else { (lo - w, lo) };
                    p.x0.retarget(t, e0 as f64);
                    p.x1.retarget(t, e1 as f64);
                    p.opacity.retarget(t, 0.0);
                    m.gone.push(p);
                    let (nlo, nhi) = line_extent(&boxes, line);
                    m.from = if down { (nlo - w, nlo) } else { (nhi, nhi + w) };
                    m.clip = if down { (nlo, f32::INFINITY) } else { (f32::NEG_INFINITY, nhi) };
                    down
                } else {
                    m.from = (x0, x1);
                    m.clip = NO_CLIP;
                    tx0 + tx1 >= x0 + x1
                };
                let (s0, s1) = edge_springs(right, lead);
                m.p0.set_spring(s0);
                m.p1.set_spring(s1);
                if reduced {
                    m.p0.snap(t, 1.0);
                    m.p1.snap(t, 1.0);
                } else {
                    m.p0.snap(t, 0.0);
                    m.p1.snap(t, 0.0);
                    m.p0.retarget(t, 1.0);
                    m.p1.retarget(t, 1.0);
                }
                (m.word, m.line) = (Some(id), line);
                m.opacity.retarget(t, 1.0);
            }
        }
        true
    }

    /// Live text: the final pass and its lines, when the node has one.
    fn pass_lines(&self) -> Option<PassLines> {
        let Kind::LiveText { words, pass: Some(pass), .. } = &self.kind else { return None };
        let boxes = word_boxes(&self.shaped, &word_starts(words));
        let lines = final_pass::read_lines(&boxes);
        Some((*pass, boxes, lines))
    }

    /// Live text: starts the brighten pulse of each word the final pass reached since the
    /// last call, in reading order and `final_pass::STAGGER` apart. Words a pass that moved
    /// back has not reached lose their pulse. Returns whether a pulse started.
    fn sync_pass(&mut self, t: f64) -> bool {
        let Some((pass, boxes, lines)) = self.pass_lines() else {
            self.words.iter_mut().for_each(|w| w.passed = None);
            return false;
        };
        let pos = pass.at * final_pass::total(&lines);
        let mut order: Vec<(f32, u32)> =
            boxes.iter().map(|b| (final_pass::reading_at(&lines, b.3, b.1), b.0)).collect();
        order.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut next = self.words.iter().filter_map(|w| w.passed).fold(t, |a, p| a.max(p + final_pass::STAGGER));
        let mut started = false;
        for (start, id) in order {
            let Ok(i) = self.words.binary_search_by_key(&id, |a| a.id) else { continue };
            let w = &mut self.words[i];
            if start < pos {
                if w.passed.is_none() {
                    w.passed = Some(next);
                    next += final_pass::STAGGER;
                    started = true;
                }
            } else {
                w.passed = None;
            }
        }
        started
    }

    /// Live text: how far a word's text has turned to the mark's accent colour at `t`.
    fn word_accent(&self, id: u32, t: f64) -> f32 {
        match self.words.binary_search_by_key(&id, |a| a.id) {
            Ok(i) => self.words[i].accent.value(t).clamp(0.0, 1.0) as f32,
            Err(_) => 0.0,
        }
    }

    /// Live text: when the final pass reached a word.
    fn word_passed(&self, id: u32) -> Option<f64> {
        self.words.binary_search_by_key(&id, |a| a.id).ok().and_then(|i| self.words[i].passed)
    }

    /// Live text: a word's `(opacity, dx, dy)` at `t`.
    fn word_motion(&self, id: u32, t: f64) -> (f32, f32, f32) {
        match self.words.binary_search_by_key(&id, |a| a.id) {
            Ok(i) => {
                let a = &self.words[i];
                (a.opacity.value(t).clamp(0.0, 1.0) as f32, a.dx.value(t) as f32, a.dy.value(t) as f32)
            }
            Err(_) => (1.0, 0.0, 0.0),
        }
    }

    /// Live text: a word's text effects `(blur, dissolve, wipe)` at `t`.
    fn word_fx(&self, id: u32, t: f64) -> (f32, f32, f32) {
        match self.words.binary_search_by_key(&id, |a| a.id) {
            Ok(i) => {
                let a = &self.words[i];
                let unit = |c: &Channel| c.value(t).clamp(0.0, 1.0) as f32;
                (a.blur.value(t).max(0.0) as f32, unit(&a.dissolve), unit(&a.wipe))
            }
            Err(_) => (0.0, 0.0, 0.0),
        }
    }

    /// The text effects (TY17) the node's slots hold at `t`, the wipe for the whole text.
    fn text_fx(&self, t: f64) -> GlyphFx {
        let v = |s: Slot| self.value(s as u8, t) as f32;
        GlyphFx {
            blur: v(Slot::TextBlur).max(0.0),
            dissolve: v(Slot::TextDissolve).clamp(0.0, 1.0),
            wipe: v(Slot::TextWipe).clamp(0.0, 1.0),
            chroma: v(Slot::TextChroma),
            halo: self.color(Slot::TextHalo, t),
            halo_radius: v(Slot::TextHaloRadius).max(0.0),
            halo_offset: Point::new(v(Slot::TextShadowX), v(Slot::TextShadowY)),
            outline: v(Slot::TextOutline).max(0.0),
            material: 0,
            weight: 0.0,
        }
    }

    /// The bloom shape from its slots (`Element::bloom_shape`).
    fn bloom_shape(&self, t: f64) -> BloomShape {
        let band = |i: u8| {
            let s = Slot::BloomBand0 as u8 + i * BAND_SLOTS;
            let v = |k: u8| self.value(s + k, t).max(0.0) as f32;
            let c = |k: u8| self.value(s + 3 + k, t).clamp(0.0, 1.0) as f32;
            BloomBand { weight: v(0), reach: v(1), saturation: v(2), tint: Color::rgba(c(0), c(1), c(2), c(3)) }
        };
        BloomShape {
            threshold: self.value(Slot::BloomThreshold as u8, t).max(0.0) as f32,
            bands: [band(0), band(1), band(2)],
        }
    }

    fn color(&self, slot: Slot, t: f64) -> Color {
        let s = slot as u8;
        let c = |i: u8| self.value(s + i, t).clamp(0.0, 1.0) as f32;
        Color::rgba(c(0), c(1), c(2), c(3))
    }

    /// The rim's direction at `t` in turns: its angle, plus the laps it has orbited since the
    /// node entered, counted in 30 fps steps (FX08 caps ambient loops at 30 fps).
    fn rim_angle(&self, t: f64) -> f32 {
        let angle = self.value(Slot::RimAngle as u8, t);
        let period = self.value(Slot::RimPeriod as u8, t);
        if period <= 0.0 || self.reduced {
            return angle as f32;
        }
        let step = ((t - self.since).max(0.0) * AMBIENT_FPS).floor() / AMBIENT_FPS;
        (angle + (step / period).fract()) as f32
    }

    /// When an orbiting rim next moves, after `t`.
    fn rim_wake(&self, t: f64) -> Option<f64> {
        let orbits = self.has(Slot::RimColor as u8) && self.value(Slot::RimPeriod as u8, t) > 0.0 && !self.reduced;
        orbits.then(|| self.since + (((t - self.since).max(0.0) * AMBIENT_FPS).floor() + 1.0) / AMBIENT_FPS)
    }

    /// The layer style (FX13) the node's slots hold at `t`, laid out on `rect`; `None` when
    /// none is set.
    fn style(&self, t: f64, rect: Rect) -> Option<LayerStyle> {
        if !(Slot::StyleChroma as u8..=Slot::StyleFlicker as u8).any(|s| self.has(s)) {
            return None;
        }
        let v = |s: Slot| self.value(s as u8, t) as f32;
        let age = (t - self.since).max(0.0);
        let moving = !self.reduced;
        let glitch_step = (age * GLITCH_FPS).floor() as u32;
        let flicker_step = (age * FLICKER_FPS).floor() as u32;
        // A seeded level per step, so a seek or a replay flickers the same way.
        let level = vo_core::hash::rand_f32(0x5f1c_4e2b, flicker_step, 0, 0);
        Some(LayerStyle {
            rect,
            chroma: v(Slot::StyleChroma),
            glitch: if moving { v(Slot::StyleGlitch).clamp(0.0, 1.0) } else { 0.0 },
            glitch_step,
            scanlines: v(Slot::StyleScanlines).clamp(0.0, 1.0),
            scan_period: v(Slot::StyleScanPeriod).max(1.0),
            curvature: v(Slot::StyleCurvature).clamp(0.0, 1.0),
            vignette: v(Slot::StyleVignette).clamp(0.0, 1.0),
            flicker: if moving { v(Slot::StyleFlicker).clamp(0.0, 0.2) * level } else { 0.0 },
        })
    }

    /// The displacement (EG-03) at `t`, or `None` when its strength is 0 or motion is reduced.
    fn displace(&self, d: &crate::displace::Displace, keep_reading: bool, t: f64) -> Option<Displace> {
        let strength = self.value(Slot::DisplaceStrength as u8, t) as f32;
        if self.reduced || strength <= 0.0 {
            return None;
        }
        Some(Displace { field: d.field(t, self.since, strength), strength, keep_reading, keep: None })
    }

    /// When the displacement next changes after `t`.
    fn displace_wake(&self, t: f64) -> Option<f64> {
        let Kind::Layer { displace: Some((d, _)), .. } = &self.kind else { return None };
        if self.reduced || self.value(Slot::DisplaceStrength as u8, t) <= 0.0 {
            return None;
        }
        d.wake(t)
    }

    /// When a glitch or flicker next changes, after `t`.
    fn style_wake(&self, t: f64) -> Option<f64> {
        if self.reduced {
            return None;
        }
        let next = |fps: f64| self.since + (((t - self.since).max(0.0) * fps).floor() + 1.0) / fps;
        let glitch = (self.has(Slot::StyleGlitch as u8) && self.value(Slot::StyleGlitch as u8, t) > 0.0)
            .then(|| next(GLITCH_FPS));
        let flicker = (self.has(Slot::StyleFlicker as u8) && self.value(Slot::StyleFlicker as u8, t) > 0.0)
            .then(|| next(FLICKER_FPS));
        glitch.into_iter().chain(flicker).min_by(f64::total_cmp)
    }

    /// How far a sheen has swept at `t`, 0 to 1, while it is sweeping.
    fn sheen_progress(&self, t: f64) -> Option<f32> {
        if !self.has(Slot::SheenColor as u8) || self.reduced {
            return None;
        }
        let u = (t - self.since) / self.value(Slot::SheenSecs as u8, t).max(1e-3);
        (0.0..1.0).contains(&u).then_some(u as f32)
    }

    /// Whether the node is drawn at `t`: created by then and not yet gone.
    fn alive(&self, t: f64) -> bool {
        t >= self.born && self.leaving.is_none_or(|(_, until)| t < until)
    }
}

/// A box as sampled at some `t`, in surface space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sampled {
    /// Node space to surface space.
    pub transform: Affine,
    pub size: Size,
    pub opacity: f32,
}

pub struct Scene<M = ()> {
    pub(crate) nodes: SlotMap<NodeId, Node<M>>,
    pub(crate) root: Option<NodeId>,
    last_t: f64,
    viewport: Size,
    scale: f32,
    text: Box<dyn TextLayout>,
    retention: f64,
    default_motion: Motion,
    reduced_motion: bool,
    stage: crate::stage::Stage,
    layout_dirty: bool,
    /// Something drawn or hit-tested changed during the current `apply`.
    changed: bool,
    pub(crate) pointer: PointerState,
    /// The signal bus store bound props read (`crate::bind`).
    signals: Option<SignalsCell>,
}

impl<M> Default for Scene<M> {
    fn default() -> Self {
        Scene::new()
    }
}

impl<M> Scene<M> {
    /// An empty scene with estimated text metrics and no viewport (the root takes its
    /// content size).
    pub fn new() -> Scene<M> {
        Scene {
            nodes: SlotMap::with_key(),
            root: None,
            last_t: f64::NEG_INFINITY,
            viewport: Size::ZERO,
            scale: 1.0,
            text: Box::new(EstimateText::default()),
            retention: f64::INFINITY,
            default_motion: DEFAULT_MOTION,
            reduced_motion: false,
            stage: crate::stage::Stage::default(),
            layout_dirty: false,
            changed: false,
            pointer: PointerState::default(),
            signals: None,
        }
    }

    /// Replaces the text measurer. Takes effect at the next layout.
    pub fn with_text(mut self, text: Box<dyn TextLayout>) -> Scene<M> {
        self.text = text;
        self
    }

    /// The audio feature store that bound props (`Element::bind`) and shaders read. The host
    /// pushes frames into it; sampling stays pure in `t`, since frames sit at their audio
    /// clock times. A root element with `Element::signals` sets it too.
    pub fn set_signals(&mut self, cell: SignalsCell) {
        if self.signals.as_ref() == Some(&cell) {
            return;
        }
        for n in self.nodes.values_mut().filter(|n| !n.binds.is_empty()) {
            n.signals = Some(cell.clone());
        }
        self.signals = Some(cell);
    }

    pub fn signals(&self) -> Option<&SignalsCell> {
        self.signals.as_ref()
    }

    /// How properties move that no `.motion()` call names. `DEFAULT_MOTION` (a soft spring)
    /// unless set; `Motion::Snap` makes every unnamed change jump, as in a plain toolkit.
    pub fn with_default_motion(mut self, motion: Motion) -> Scene<M> {
        self.default_motion = motion;
        self
    }

    /// Reduced motion (MO09): position, size, offset, scale and rotation jump to new targets,
    /// presence and word transitions keep only their fade, and glow, bloom and shadow follow
    /// their targets slowly so a level cannot make them flash. Fades still show every change.
    pub fn with_reduced_motion(mut self, reduced: bool) -> Scene<M> {
        self.reduced_motion = reduced;
        self
    }

    /// Keep channel history only `secs` back from each retarget. The default keeps all of
    /// it, which seeking and export want; a long-running overlay should set a few seconds.
    pub fn with_retention(mut self, secs: f64) -> Scene<M> {
        self.retention = secs;
        self
    }

    /// Sets the surface size (logical units; zero lets the root take its content size) and
    /// the scale factor, and lays out again at `t` if the size changed.
    /// The camera that `rotate_x`, `rotate_y` and `translate_z` are seen through
    /// (`crate::stage`). Default: perspective 1000 logical pixels, looking at the centre.
    pub fn set_stage(&mut self, stage: crate::stage::Stage) {
        self.stage = stage;
    }

    pub fn set_viewport(&mut self, t: f64, size: Size, scale: f32) {
        self.scale = scale;
        if size != self.viewport {
            self.viewport = size;
            if let Some(root) = self.root {
                self.mark_layout(root);
                self.run_layout(t);
            }
        }
    }

    /// Number of retained nodes, leaving ones included.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn root(&self) -> Option<NodeId> {
        self.root
    }

    /// The node at a key path from the root, root key first.
    pub fn find(&self, path: &[&str]) -> Option<NodeId> {
        let (first, rest) = path.split_first()?;
        let mut id = self.root.filter(|r| self.nodes[*r].key.0 == *first)?;
        for k in rest {
            id = *self.nodes[id].children.iter().find(|c| self.nodes[**c].key.0 == *k)?;
        }
        Some(id)
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(id)
    }

    /// The key path of a node, joined with `/`.
    pub fn path(&self, id: NodeId) -> String {
        let mut keys = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            keys.push(self.nodes[c].key.0.as_ref());
            cur = self.nodes[c].parent;
        }
        keys.reverse();
        keys.join("/")
    }

    pub fn presence(&self, id: NodeId, t: f64) -> Presence {
        let n = &self.nodes[id];
        match n.leaving {
            Some((since, until)) if t >= since => Presence::Leaving { since, until },
            _ if t < n.entered => Presence::Entering { since: n.since },
            _ => Presence::Present,
        }
    }

    /// The layout box a node is heading to (relative to its parent), from the last layout.
    pub fn layout_target(&self, id: NodeId) -> Rect {
        let l = &self.nodes[id].layout;
        Rect::new(l.location.x, l.location.y, l.size.width, l.size.height)
    }

    /// Applies a new view at `t`. Events must come in time order.
    ///
    /// Returns whether anything changed: a retarget, a node added, removed or reordered, a
    /// node starting to leave, a drawn property or hit flag, or anything that needs layout.
    /// When it returns false, `sample` gives the same lists as before for every `t`, so a
    /// host needs no new frame.
    pub fn apply(&mut self, view: Element<M>, t: f64) -> bool {
        debug_assert!(
            t >= self.last_t,
            "scene event at t = {t} after one at {}; events must be in time order",
            self.last_t
        );
        self.last_t = self.last_t.max(t);
        self.changed = false;
        self.drop_gone(t);
        if let Some(cell) = view.signals.clone() {
            self.set_signals(cell);
        }
        match self.root {
            Some(r) if self.nodes[r].key == view.key && self.nodes[r].kind.same_variant(&view.kind) => {
                self.revive(r);
                self.update(r, view, t)
            }
            old => {
                if let Some(r) = old {
                    self.remove_subtree(r);
                }
                let r = self.create(None, view, t);
                self.root = Some(r);
                self.layout_dirty = true;
            }
        }
        if self.layout_dirty {
            self.changed = true;
            self.run_layout(t);
        }
        self.changed |= crate::repeat::place_children(&mut self.nodes, t, self.retention);
        self.pointer.forget_missing(|id| self.nodes.get(id).is_some_and(|n| n.leaving.is_none()));
        self.changed
    }

    /// The time at or after `t` from which the display list may differ from `sample(t)`:
    /// `Some(t)` while anything moves, `None` once everything has settled.
    /// Whether `n`'s bindings still move at `t`: frames are arriving or their release runs.
    fn bound_moving(&self, n: &Node<M>, t: f64) -> bool {
        if n.binds.is_empty() {
            return false;
        }
        let Some(c) = &self.signals else { return false };
        c.get().active_until(crate::bind::tail(&n.binds)).is_some_and(|until| t < until)
    }

    pub fn next_wake(&self, t: f64) -> Option<f64> {
        // A node under one that has finished leaving is gone, though `drop_gone` only removes
        // it at the next `apply`: its loops, emitters and canvases must not keep the scene awake.
        let nodes: Vec<&Node<M>> = if self.nodes.values().any(|n| n.leaving.is_some_and(|(_, until)| until <= t)) {
            self.nodes.values().filter(|n| !self.gone_by(n, t)).collect()
        } else {
            self.nodes.values().collect()
        };
        let moving = nodes.iter().copied().any(|n| {
            n.chans.iter().any(|(_, c)| !c.resting_at(t))
                || (n.alive(t) && self.bound_moving(n, t))
                || n.words.iter().flat_map(|w| w.channels()).any(|c| !c.resting_at(t))
                || n.mark.iter().flat_map(|m| m.channels()).any(|c| c.settled_at() > t)
                || n.words.iter().any(|w| w.passed.is_some_and(|p| t < p + final_pass::PULSE))
                || n.pass_changed.iter().any(|c| t < c.at + final_pass::NOD.max(final_pass::GROW))
                || n.pass_since.is_some_and(|s| t < s + final_pass::SPOKEN_IN)
                || n.live.settled_at() > t
                || n.typo.settled_at() > t
                || (n.alive(t) && n.sheen_progress(t).is_some())
                || (n.alive(t)
                    && match &n.kind {
                        Kind::Wave(w) => w.moving(t),
                        Kind::Bars(b) => b.moving(t),
                        Kind::Progress(p) => p.moving(),
                        Kind::Dots(d) => d.source.moving(t),
                        Kind::Emitter(_) => true,
                        Kind::LiveText { particles: Some(_), .. } => {
                            t < n.words.iter().map(|w| w.since).fold(n.words_at, f64::max) + WORD_PARTICLES_BUSY
                        }
                        Kind::Canvas(c) => c.moving(t),
                        Kind::Gpu(g) => t < g.0.until(),
                        Kind::StrokePath(a) => a.head.is_some(),
                        Kind::Frame(f) => f.moving(),
                        Kind::Spectrum(s) => s.moving(self.signals.as_ref(), t),
                        Kind::LiveText { .. } => {
                            n.live.magnet_moving(self.signals.as_ref().map(|c| c.get()).as_deref(), t)
                        }
                        _ => false,
                    })
        });
        if moving {
            return Some(t);
        }
        // A feedback trail fades on after its content stops, for 12 half lives: to 0.02%,
        // under one 8-bit level even after sRGB encoding.
        let last = self
            .nodes
            .values()
            .flat_map(|n| n.chans.iter().map(|(_, c)| c.settled_at()).chain([n.since]))
            .fold(f64::NEG_INFINITY, f64::max);
        let fading = nodes.iter().copied().any(|n| {
            n.alive(t) && n.has(Slot::TrailHalfLife as u8) && t < last + 12.0 * n.value(Slot::TrailHalfLife as u8, t)
        });
        if fading {
            return Some(t);
        }
        // Glass animates at its own capped rate (FX08), so it wakes at a time, not now.
        self.nodes
            .values()
            .filter(|n| n.alive(t) && !n.reduced && matches!(n.kind, Kind::Panel))
            .filter_map(|n| n.glass.as_ref().and_then(|g| g.next_frame(n.since, t)))
            .chain(nodes.iter().copied().filter(|n| n.alive(t)).filter_map(|n| n.rim_wake(t)))
            .chain(nodes.iter().copied().filter(|n| n.alive(t)).filter_map(|n| n.ambient_wake(t)))
            // Asked about a moment before a recorded event (a replay or a seek): wake for it.
            .chain(nodes.iter().copied().flat_map(|n| {
                let words = n.words.iter().flat_map(|w| w.channels());
                n.chans.iter().map(|(_, c)| c).chain(words).filter_map(move |c| c.next_start(t))
            }))
            .chain(nodes.iter().copied().filter(|n| n.alive(t)).filter_map(|n| n.style_wake(t)))
            .chain(nodes.iter().copied().filter(|n| n.alive(t)).filter_map(|n| n.displace_wake(t)))
            .chain(nodes.iter().copied().filter(|n| n.alive(t)).filter_map(|n| n.live.wake(t)))
            // Materials that flicker or scroll (FX15), at their own capped rate.
            .chain(
                self.nodes
                    .values()
                    .filter(|n| n.alive(t) && !n.reduced)
                    .filter_map(|n| n.material.as_ref().and_then(|m| m.next_frame(t - n.since)).map(|d| t + d)),
            )
            .min_by(f64::total_cmp)
    }

    /// The display list at `t`. Pure: the same scene gives the same list for the same `t`.
    pub fn sample(&self, t: f64) -> DisplayList {
        let mut out = DisplayList::default();
        self.sample_into(t, &mut out);
        out
    }

    /// `sample` into an existing list, reusing its allocations.
    pub fn sample_into(&self, t: f64, out: &mut DisplayList) {
        out.clear();
        out.size = self.viewport;
        out.scale = self.scale;
        out.signals = self.signals.as_ref().map_or_else(SignalBlock::default, |c| c.get().block(t));
        out.stage = self.stage;
        out.layers.push(Layer::root());
        let mut items = Vec::new();
        if let Some(root) = self.root {
            self.emit(root, t, Affine::IDENTITY, 1.0, 0, false, &mut items, out);
        }
        out.layers[0].items = close_items(&mut items, out);
        keep_reading_line(out);
    }

    /// Live text: a word's box at `t` in the node's space, from its sprung position and
    /// width. Decorations, highlights and particle targets follow this, not the layout.
    pub fn word_bounds(&self, id: NodeId, word: u32, t: f64) -> Option<Rect> {
        let n = self.nodes.get(id)?;
        let Kind::LiveText { style, .. } = &n.kind else { return None };
        n.live.word_bounds(word, t, style.size)
    }

    /// Live text with a reading stage (`Element::reading`): the newest line's box at `t` in
    /// the node's space. Map it with `sampled(id, t).transform` to place a surface so the
    /// line being read sits where the eye is.
    pub fn reading_line(&self, id: NodeId, t: f64) -> Option<Rect> {
        self.nodes.get(id)?.live.reading_line(t)
    }

    /// Live text: the caret bar at `t` in the node's space, and its blink opacity.
    pub fn caret(&self, id: NodeId, t: f64) -> Option<(Rect, f32)> {
        self.nodes.get(id)?.live.caret(t).map(|(r, _, a)| (r, a))
    }

    /// Live text: where the final pass is, `(baseline, x)` in the node's space.
    pub fn final_pass_at(&self, id: NodeId) -> Option<(f32, f32)> {
        let (pass, _, lines) = self.nodes.get(id)?.pass_lines()?;
        final_pass::locate(&lines, pass.at)
    }

    /// Live text: whether the final pass has reached the start of `word`. Hold back a word's
    /// final text until this is true, so its revision plays as the line crosses it.
    pub fn final_pass_reached(&self, id: NodeId, word: u32) -> bool {
        let Some((pass, boxes, lines)) = self.nodes.get(id).and_then(|n| n.pass_lines()) else { return false };
        let Some(b) = boxes.iter().find(|b| b.0 == word) else { return false };
        final_pass::reading_at(&lines, b.3, b.1) < pass.at * final_pass::total(&lines)
    }

    /// Box, transform and opacity of a node at `t`, composed with its ancestors. `None` if
    /// it is not drawn at `t`.
    pub fn sampled(&self, id: NodeId, t: f64) -> Option<Sampled> {
        let mut chain = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            chain.push(c);
            cur = self.nodes[c].parent;
        }
        let (mut m, mut opacity, mut size) = (Affine::IDENTITY, 1.0, Size::ZERO);
        for &c in chain.iter().rev() {
            let n = &self.nodes[c];
            if !n.alive(t) {
                return None;
            }
            let (local, s, o) = local_transform(n, t);
            let local = crate::repeat::placed(&self.nodes, n, t, s).unwrap_or(local);
            m = local.then(&m);
            opacity *= o;
            size = s;
        }
        Some(Sampled { transform: m, size, opacity })
    }

    // Reconcile.

    /// A node's default motion: its element's own, else its parent's (already resolved,
    /// since parents are reconciled first), else the scene's.
    fn inherited_motion(&self, own: Option<Motion>, parent: Option<NodeId>) -> Motion {
        own.or_else(|| parent.and_then(|p| self.nodes.get(p)).map(|p| p.default_motion)).unwrap_or(self.default_motion)
    }

    fn create(&mut self, parent: Option<NodeId>, el: Element<M>, t: f64) -> NodeId {
        let Element {
            key,
            kind,
            style,
            targets,
            gradient,
            hairline,
            inside,
            glass,
            material,
            afterglow,
            motion,
            default_motion,
            binds,
            signals: _,
            presence,
            handlers,
            children,
            loops,
        } = el;
        self.changed = true;
        let mut node = Node {
            key,
            parent,
            kind,
            style,
            cache: taffy::Cache::new(),
            layout: taffy::Layout::with_order(0),
            hoisted: Vec::new(),
            children: Vec::new(),
            flow: Vec::new(),
            chans: Vec::new(),
            motion,
            signals: (!binds.is_empty()).then(|| self.signals.clone()).flatten(),
            binds,
            presence,
            gradient,
            hairline,
            inside,
            glass,
            material,
            afterglow,
            handlers,
            born: t,
            leaving: None,
            since: t,
            entered: t,
            shaped: ShapedText::default(),
            fitted: 0.0,
            has_layout: false,
            words: Vec::new(),
            words_at: t,
            live: Flow::default(),
            mark: None,
            pass_since: None,
            pass_changed: Vec::new(),
            default_motion: self.inherited_motion(default_motion, parent),
            reduced: self.reduced_motion,
            ambient: Default::default(),
            typo: Default::default(),
        };
        node.sync_words(t, self.retention);
        let enter = presence.map_or(Transition::NONE, |(enter, _)| enter);
        let enter = if self.reduced_motion { enter.reduced() } else { enter };
        for (slot, start) in enter.slots() {
            node.start_at(slot, start, t, self.retention);
            let target = targets.iter().find(|(s, _)| *s == slot).map_or(Slot::default_value(slot), |(_, v)| *v);
            node.set_target(slot, target, t, self.retention);
        }
        for &(slot, v) in &targets {
            if !node.has(slot) {
                node.start_at(slot, v, t, self.retention);
            }
        }
        let ripples_on = node.ripples().map(|r| r.active);
        node.ambient.sync(&loops, ripples_on, t, true, self.retention);
        node.entered = node.settled_at().max(t);
        let id = self.nodes.insert(node);
        check_keys(&children, || self.path(id));
        let ids: Vec<NodeId> = children.into_iter().map(|c| self.create(Some(id), c, t)).collect();
        let n = &mut self.nodes[id];
        n.flow.clone_from(&ids);
        n.children = ids;
        id
    }

    fn update(&mut self, id: NodeId, el: Element<M>, t: f64) {
        let Element {
            key: _,
            kind,
            style,
            targets,
            gradient,
            hairline,
            inside,
            glass,
            material,
            afterglow,
            motion,
            default_motion,
            binds,
            signals: _,
            presence,
            handlers,
            children,
            loops,
        } = el;
        let retention = self.retention;
        // Before the targets below, so they retarget with this subtree's default.
        let effective = self.inherited_motion(default_motion, self.nodes[id].parent);
        let n = &mut self.nodes[id];
        n.default_motion = effective;
        let mut kind = kind;
        release_finals(n, &mut kind, t);
        let relayout = n.style != style || n.kind.layout_differs(&kind);
        let old_size = text_size(&n.kind);
        let mut changed = n.kind != kind || n.gradient != gradient || n.hairline != hairline || n.inside != inside;
        changed |= n.glass != glass;
        changed |= n.binds != binds;
        changed |= n.material != material || n.afterglow != afterglow;
        changed |= n.handlers.hit != handlers.hit;
        n.style = style;
        n.kind = kind;
        n.motion = motion;
        n.signals = (!binds.is_empty()).then(|| self.signals.clone()).flatten();
        n.binds = binds;
        n.presence = presence;
        n.gradient = gradient;
        n.hairline = hairline;
        n.inside = inside;
        n.glass = glass;
        n.material = material;
        n.afterglow = afterglow;
        n.handlers = handlers;
        for &(slot, v) in &targets {
            changed |= n.set_target(slot, v, t, retention);
        }
        // A new font size grows from the old one when the view asks for it (FX-048): the
        // drawn size starts at old over new and springs to 1, over the new layout.
        if let (Some(from), Some(to)) = (old_size, text_size(&n.kind)) {
            if from != to && to > 0.0 && n.motion.iter().any(|(p, _)| *p == Prop::TextSize) {
                let slot = Slot::TextZoom as u8;
                let now = n.value(slot, t);
                n.start_at(slot, now * (from / to) as f64, t, retention);
                n.set_target(slot, 1.0, t, retention);
                changed = true;
            }
        }
        changed |= n.sync_words(t, retention);
        let ripples_on = n.ripples().map(|r| r.active);
        changed |= n.ambient.sync(&loops, ripples_on, t, false, retention);
        // After a relayout `run_layout` moves the mark, with the new glyph positions.
        if !relayout {
            changed |= n.sync_mark(t, retention);
            changed |= n.sync_pass(t);
        }
        // Visual slots the view no longer sets go back to their defaults.
        let unset: Vec<u8> = n
            .chans
            .iter()
            .map(|(s, _)| *s)
            .filter(|s| *s >= Slot::Opacity as u8 && !targets.iter().any(|(k, _)| k == s))
            .filter(|s| !crate::repeat::placed_slot(*s))
            .collect();
        for slot in unset {
            changed |= n.set_target(slot, Slot::default_value(slot), t, retention);
        }
        self.changed |= changed;
        if relayout {
            self.mark_layout(id);
        }
        self.reconcile_children(id, children, t);
    }

    fn reconcile_children(&mut self, id: NodeId, children: Vec<Element<M>>, t: f64) {
        check_keys(&children, || self.path(id));
        let old = std::mem::take(&mut self.nodes[id].children);
        let mut used = vec![false; old.len()];
        let mut flow = Vec::with_capacity(children.len());
        for el in children {
            let found = old.iter().enumerate().position(|(i, &c)| {
                !used[i] && self.nodes[c].key == el.key && self.nodes[c].kind.same_variant(&el.kind)
            });
            let c = match found {
                Some(i) => {
                    used[i] = true;
                    let c = old[i];
                    let revived = self.revive(c);
                    self.update(c, el, t);
                    if revived {
                        let n = &mut self.nodes[c];
                        n.since = t;
                        n.entered = n.settled_at().max(t);
                    }
                    c
                }
                None => self.create(Some(id), el, t),
            };
            flow.push(c);
        }
        // Children the view dropped: leave with their exit motion, or go now.
        let mut order = flow.clone();
        let mut anchor: Option<NodeId> = None;
        for (i, &c) in old.iter().enumerate() {
            if used[i] {
                anchor = Some(c);
                continue;
            }
            if self.start_leaving(c, t) {
                // Keep its place in paint order, after the sibling it followed.
                let at = anchor.and_then(|a| order.iter().position(|x| *x == a)).map_or(0, |p| p + 1);
                order.insert(at, c);
                anchor = Some(c);
            } else {
                self.remove_subtree(c);
            }
        }
        self.changed |= old != order;
        let n = &mut self.nodes[id];
        let changed = n.flow != flow;
        n.flow = flow;
        n.children = order;
        if changed {
            self.mark_layout(id);
        }
    }

    /// A leaving node whose key is back in the view enters again from where it is.
    /// Returns whether it was leaving.
    fn revive(&mut self, id: NodeId) -> bool {
        let revived = self.nodes[id].leaving.take().is_some();
        self.changed |= revived;
        revived
    }

    /// Whether `n` or one of its ancestors has finished leaving by `t`.
    fn gone_by(&self, n: &Node<M>, t: f64) -> bool {
        let done = |n: &Node<M>| n.leaving.is_some_and(|(_, until)| until <= t);
        let mut cur = Some(n);
        while let Some(c) = cur {
            if done(c) {
                return true;
            }
            cur = c.parent.and_then(|p| self.nodes.get(p));
        }
        false
    }

    /// Starts the exit motion. Returns false when the node should go at once.
    fn start_leaving(&mut self, id: NodeId, t: f64) -> bool {
        let retention = self.retention;
        let n = &mut self.nodes[id];
        if n.leaving.is_some() {
            return true;
        }
        let Some((_, exit)) = n.presence else { return false };
        let exit = if n.reduced { exit.reduced() } else { exit };
        for (slot, v) in exit.slots() {
            n.set_target(slot, v, t, retention);
        }
        self.changed = true;
        // An exit that fades to nothing hides the ambient loops as they settle: the node goes
        // when its exit ends. A slow spin coasting to its next whole turn would otherwise keep
        // a hidden node for minutes.
        let hides = exit.opacity.is_some_and(|o| o <= 0.0);
        self.stop_ambient(id, t);
        let until = self.subtree_settled_at(id, !hides);
        if until <= t {
            return false;
        }
        self.nodes[id].leaving = Some((t, until));
        true
    }

    /// Lets the ambient loops of `id` and everything under it settle from `t`.
    fn stop_ambient(&mut self, id: NodeId, t: f64) {
        self.nodes[id].ambient.stop(t);
        for c in self.nodes[id].children.clone() {
            self.stop_ambient(c, t);
        }
    }

    /// When `id` and everything under it are at rest: their channels, and their ambient loops
    /// when `ambient`.
    fn subtree_settled_at(&self, id: NodeId, ambient: bool) -> f64 {
        let n = &self.nodes[id];
        let own = if ambient { n.settled_at().max(n.ambient_rest_at()) } else { n.settled_at() };
        n.children.iter().map(|&c| self.subtree_settled_at(c, ambient)).fold(own, f64::max)
    }

    /// Removes leaving nodes whose exit has settled by `t`.
    fn drop_gone(&mut self, t: f64) {
        let gone: Vec<NodeId> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.leaving.is_some_and(|(_, until)| until <= t))
            .map(|(id, _)| id)
            .collect();
        for id in gone {
            if !self.nodes.contains_key(id) {
                continue;
            }
            match self.nodes[id].parent {
                Some(p) => self.nodes[p].children.retain(|c| *c != id),
                None => self.root = None,
            }
            self.changed = true;
            self.remove_subtree(id);
        }
    }

    fn remove_subtree(&mut self, id: NodeId) {
        if let Some(n) = self.nodes.remove(id) {
            for c in n.children {
                self.remove_subtree(c);
            }
        }
    }

    // Layout.

    /// Clears the layout cache of `id` and its ancestors, so the next layout recomputes them.
    fn mark_layout(&mut self, id: NodeId) {
        self.layout_dirty = true;
        let mut cur = Some(id);
        while let Some(c) = cur {
            let n = &mut self.nodes[c];
            n.cache.clear();
            cur = n.parent;
        }
    }

    /// Runs taffy and turns the boxes into position and size targets at `t`. A node laid
    /// out for the first time starts at rest at its box.
    fn run_layout(&mut self, t: f64) {
        self.layout_dirty = false;
        let Some(root) = self.root else { return };
        let avail = |v: f32| if v > 0.0 { AvailableSpace::Definite(v) } else { AvailableSpace::MaxContent };
        let space = taffy::Size { width: avail(self.viewport.w), height: avail(self.viewport.h) };
        {
            let mut view = LayoutView { nodes: &mut self.nodes, text: &*self.text };
            taffy::compute_root_layout(&mut view, root.into(), space);
        }
        let retention = self.retention;
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let n = &mut self.nodes[id];
            let l = n.layout;
            let boxes =
                [(Slot::X, l.location.x), (Slot::Y, l.location.y), (Slot::W, l.size.width), (Slot::H, l.size.height)];
            for (slot, v) in boxes {
                if n.has_layout {
                    n.set_target(slot as u8, v as f64, t, retention);
                } else {
                    n.start_at(slot as u8, v as f64, t, retention);
                }
            }
            n.has_layout = true;
            if let Kind::LiveText { words, style, live, particles, .. } = &n.kind {
                let input = FlowInput {
                    shaped: &n.shaped,
                    words,
                    motion: live,
                    size: style.size,
                    line_height: style.line_height,
                    retention,
                    reduced: n.reduced,
                    crossfade: particles.is_none(),
                    width: l.size.width,
                    entering: n
                        .words
                        .iter()
                        .filter(|a| a.opacity.target() >= 1.0 && a.opacity.value(t) < 1.0 - 1e-4)
                        .map(|a| a.id)
                        .collect(),
                };
                n.live.update(t, input);
            }
            n.sync_mark(t, retention);
            n.sync_pass(t);
            stack.extend(n.flow.iter().copied());
        }
    }

    // Sampling.

    /// Emits node `id` and its subtree. `parent_m` maps the parent's space to the current
    /// layer's space; `alpha` is the opacity inherited since that layer.
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &self,
        id: NodeId,
        t: f64,
        parent_m: Affine,
        alpha: f32,
        layer: u32,
        leaving: bool,
        items: &mut Vec<Item>,
        out: &mut DisplayList,
    ) {
        let n = &self.nodes[id];
        if !n.alive(t) {
            return;
        }
        let (local, size, opacity) = local_transform(n, t);
        let local = crate::repeat::placed(&self.nodes, n, t, size).unwrap_or(local);
        if opacity <= 0.0 {
            return;
        }
        let leaving = leaving || n.leaving.is_some_and(|(since, _)| t >= since);
        let m = local.then(&parent_m);
        let as_layer = matches!(n.kind, Kind::Layer { .. });
        // Motion blur: the distance moved while the shutter is open, in the parent's units.
        let mut smear = Point::ZERO;
        if n.has(Slot::Shutter as u8) {
            let shutter = n.value(Slot::Shutter as u8, t);
            let v = |a: Slot, b: Slot| (n.velocity(a as u8, t) + n.velocity(b as u8, t)) * shutter;
            smear = Point::new(v(Slot::X, Slot::OffsetX) as f32, v(Slot::Y, Slot::OffsetY) as f32);
            // In the node's own units, which its scale divides.
            let k = n.value(Slot::Scale as u8, t).abs().max(1e-3) as f32;
            smear = Point::new(smear.x / k, smear.y / k);
        }
        let blurred = smear.x.abs() + smear.y.abs() > 0.01;
        let tilt = [Slot::RotateX, Slot::RotateY, Slot::TranslateZ].map(|s| n.value(s as u8, t) as f32);
        let tilted = tilt != [0.0; 3];
        if !as_layer && !blurred && !tilted && m.is_translation() {
            self.emit_body(id, t, m, size, alpha * opacity, layer, leaving, items, out);
            return;
        }
        // A layer element, or a scale or rotation: the node gets a layer of its own, so
        // instances inside it stay axis aligned.
        let index = out.layers.len() as u32;
        items.push(Item { kind: ItemKind::Layer, start: index, len: 1 });
        let transform = m.then(&out.layers[layer as usize].transform);
        // Stage turns (`crate::stage`): this layer's own warp about its box as laid out,
        // then its parent's.
        let parent_warp = out.layers[layer as usize].warp;
        let own_warp = tilted
            .then(|| {
                let [x0, y0, x1, y1] = affine_bounds(&transform, size);
                let rect = Rect::new(x0, y0, x1 - x0, y1 - y0);
                self.stage.tilt(self.viewport, rect, tilt[0], tilt[1], tilt[2])
            })
            .flatten();
        let warp = match (own_warp, parent_warp) {
            (Some(o), Some(p)) => Some(o.then(&p)),
            (o, p) => o.or(p),
        };
        let style = match n.kind {
            Kind::Layer { mask: None, .. } => n.style(t, Rect::from_size(size)),
            _ => None,
        };
        let displace = match &n.kind {
            Kind::Layer { displace: Some((d, keep_reading)), mask: None, .. } => n.displace(d, *keep_reading, t),
            _ => None,
        };
        let backdrop = match n.kind {
            Kind::Layer { backdrop: Some((res, tint)), mask: None, .. } => {
                let res = if displace.is_some() { BackdropRes::Full } else { res };
                Some(Backdrop { rect: RRect::new(Rect::from_size(size), radius(n, t)), res, tint })
            }
            // A displacement reads a full-resolution backdrop it does not tint.
            Kind::Layer { mask: None, .. } if displace.is_some() => Some(Backdrop {
                rect: RRect::new(Rect::from_size(size), radius(n, t)),
                res: BackdropRes::Full,
                tint: Color::TRANSPARENT,
            }),
            _ => None,
        };
        let (blend, clip, clip_fade, target, group, inside) = match n.kind {
            Kind::Layer { blend, clip, offscreen, fade, mask, .. } => {
                let clip = clip.then(|| RRect::new(Rect::from_size(size), radius(n, t)));
                let bloom = n.has(Slot::BloomStrength as u8).then(|| Bloom {
                    strength: n.value(Slot::BloomStrength as u8, t).max(0.0) as f32,
                    radius: n.value(Slot::BloomRadius as u8, t).max(0.0) as f32,
                    shape: n.has(Slot::BloomThreshold as u8).then(|| n.bloom_shape(t)),
                });
                // A layer with a mask child is composited through the mask, so it is offscreen.
                let masked =
                    n.children.iter().any(|c| matches!(self.nodes[*c].kind, Kind::Layer { mask: Some(_), .. }));
                let target = match mask {
                    Some(m) => LayerTarget::Mask(m),
                    None if offscreen || masked => LayerTarget::Offscreen { bloom },
                    None => LayerTarget::Direct,
                };
                (blend, clip, fade, target, alpha * opacity, 1.0)
            }
            _ => (BlendMode::Normal, None, [0.0; 4], LayerTarget::Direct, 1.0, alpha * opacity),
        };
        out.layers.push(Layer {
            parent: Some(layer),
            transform,
            opacity: group,
            blend,
            clip,
            clip_fade,
            target,
            motion_blur: smear,
            trail: n.has(Slot::TrailHalfLife as u8).then(|| Trail {
                key: id.to_bits(),
                half_life: n.value(Slot::TrailHalfLife as u8, t) as f32,
                t,
                decay: n.afterglow.unwrap_or([1.0; 3]),
            }),
            style,
            warp,
            backdrop,
            displace,
            items: 0..0,
        });
        let mut own = Vec::new();
        self.emit_body(id, t, Affine::IDENTITY, size, inside, index, leaving, &mut own, out);
        out.layers[index as usize].items = close_items(&mut own, out);
    }

    /// Hit shape, visuals and children of a node whose space maps to the current layer by
    /// `m`, a translation.
    #[allow(clippy::too_many_arguments)]
    fn emit_body(
        &self,
        id: NodeId,
        t: f64,
        m: Affine,
        size: Size,
        alpha: f32,
        layer: u32,
        leaving: bool,
        items: &mut Vec<Item>,
        out: &mut DisplayList,
    ) {
        let n = &self.nodes[id];
        if n.handlers.hit && !leaving {
            let transform = m.then(&out.layers[layer as usize].transform);
            let shape = RRect::new(Rect::from_size(size), radius(n, t));
            out.hits.push(HitShape { node: id.to_bits(), transform, shape });
        }
        if let (Kind::Panel, Some(g)) = (&n.kind, &n.glass) {
            // Reduced motion (MO09): the glass stays still.
            let (sheen, orbit, orbit_strength) = if n.reduced { (None, None, 0.0) } else { g.motion(n.since, t) };
            push_item(items, ItemKind::Glass, out.glass.len() as u32);
            out.glass.push(GlassInstance {
                rect: [m.e, m.f, size.w, size.h],
                radius: radius(n, t),
                glass: *g,
                alpha,
                age: (t - n.since) as f32,
                sheen,
                orbit,
                orbit_strength,
                to_surface: out.layers[layer as usize].transform,
                surface_alpha: alpha * layer_chain_opacity(&out.layers, layer),
                outline: None,
            });
        }
        let firsts = [out.shapes.len(), out.glyphs.len(), out.vectors.len()];
        if let Kind::Canvas(c) = &n.kind {
            let mut painter = crate::canvas::Painter { list: out, items, alpha, layer };
            (c.draw)(t, Rect::new(m.e, m.f, size.w, size.h), &mut painter);
        } else if let Kind::Emitter(e) = &n.kind {
            let centre = Point::new(m.e + 0.5 * size.w, m.f + 0.5 * size.h);
            let i = crate::emitter::push_instance(out, e, id.to_bits(), n.born, t, centre, alpha);
            push_item(items, ItemKind::Emitters, i);
        } else if let Kind::Gpu(g) = &n.kind {
            out.gpu.push(crate::gpu::GpuItem {
                source: g.clone(),
                rect: Rect::new(m.e, m.f, size.w, size.h),
                t,
                opacity: alpha,
            });
            push_item(items, ItemKind::Gpu, out.gpu.len() as u32 - 1);
        } else {
            // Lines leaving live text (MO-33) go under it, their shadows over it.
            let leaving = (matches!(n.kind, Kind::LiveText { .. }) && n.live.has_extras()).then(|| {
                let zoom = n.value(Slot::TextZoom as u8, t) as f32;
                let color = n.color(Slot::Text, t).fade(alpha);
                crate::flow::overflow::Place { origin: Point::new(m.e, m.f), zoom, color, layer }
            });
            if let Some(at) = &leaving {
                crate::flow::overflow::emit(n.live.leaving(), t, at, &self.stage, self.viewport, items, out);
            }
            self.draw(n, t, Point::new(m.e, m.f), size, alpha, items, out);
            if let Some(at) = &leaving {
                crate::flow::overflow::emit_shadows(n.live.leaving(), t, at, items, out);
                crate::flow::margin::emit(&n.live, t, at, alpha, items, out);
                crate::flow::spine::emit(&n.live, t, at, alpha, items, out);
            }
        }
        for &c in &n.children {
            self.emit(c, t, m, alpha, layer, leaving, items, out);
        }
        if let Some(mat) = &n.material {
            // Reduced motion (MO09): long after any flicker-on, and still.
            let age = if n.reduced { 1e3 } else { (t - n.since).max(0.0) as f32 };
            apply_material(out, firsts, mat.at(age).in_box(Rect::new(m.e, m.f, size.w, size.h)));
        }
    }

    /// The node's own visuals at `origin` in the current layer.
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        n: &Node<M>,
        t: f64,
        origin: Point,
        size: Size,
        alpha: f32,
        items: &mut Vec<Item>,
        out: &mut DisplayList,
    ) {
        let rect = Rect::new(origin.x, origin.y, size.w, size.h);
        let fill = || {
            if !n.has(Slot::Fill0 as u8) {
                return Paint::NONE;
            }
            let c0 = n.color(Slot::Fill0, t);
            let p = if n.gradient {
                let c1 = n.color(Slot::Fill1, t);
                Paint::linear(origin, c0, Point::new(origin.x, origin.y + size.h), c1)
            } else {
                Paint::solid(c0)
            };
            p.fade(alpha)
        };
        let stroked = |mut s: ShapeInstance| {
            if n.has(Slot::Stroke as u8) {
                let paint = Paint::solid(n.color(Slot::Stroke, t)).fade(alpha);
                s = if n.hairline {
                    s.with_hairline(paint)
                } else {
                    s.with_stroke(paint, n.value(Slot::StrokeWidth as u8, t).max(0.0) as f32)
                };
            }
            s
        };
        // An inside stroke is a centred one on the outline moved in by half its width; the
        // fill reaches the stroke's centre, so fill and stroke still cover the whole box.
        let boxed = || {
            let inset = if n.inside && !n.hairline && n.has(Slot::Stroke as u8) {
                0.5 * n.value(Slot::StrokeWidth as u8, t).max(0.0) as f32
            } else {
                0.0
            };
            let r = Rect::new(
                rect.x + inset,
                rect.y + inset,
                (rect.w - 2.0 * inset).max(0.0),
                (rect.h - 2.0 * inset).max(0.0),
            );
            RRect::new(r, (radius(n, t) - inset).max(0.0))
        };
        let shape = match &n.kind {
            Kind::Panel | Kind::Layer { .. } => Some(ShapeInstance::rrect(boxed(), fill())),
            Kind::Shape(kind) => Some(match kind {
                ShapeKind::RRect => ShapeInstance::rrect(boxed(), fill()),
                ShapeKind::Circle => {
                    let centre = Point::new(origin.x + 0.5 * size.w, origin.y + 0.5 * size.h);
                    ShapeInstance::circle(centre, 0.5 * size.w.min(size.h), fill())
                }
                ShapeKind::Capsule => ShapeInstance::capsule(rect, fill()),
                ShapeKind::Line => {
                    ShapeInstance::line(origin, Point::new(rect.right(), rect.bottom()), 0.0, Paint::NONE)
                }
                ShapeKind::Band => {
                    let half = 0.5 * size.h;
                    ShapeInstance::band(origin.x, rect.right(), origin.y + half, half, half, fill())
                }
            }),
            Kind::Label { style, .. } => {
                let color = n.color(Slot::Text, t).fade(alpha);
                let size = if n.fitted > 0.0 { n.fitted } else { style.size };
                self.glyphs(n, size, origin, color, alpha, t, |_| NO_WORD, items, out);
                None
            }
            Kind::LiveText { words, style, particles, mark, inline, pass, live, .. } => {
                let color = n.color(Slot::Text, t).fade(alpha);
                let z = n.value(Slot::TextZoom as u8, t) as f32;
                if let Some(look) = particles {
                    let pw = out.particle_words.get_or_insert_with(|| ParticleWords { t, words: Vec::new() });
                    pw.words.extend(n.words.iter().map(|w| ParticleWord { word: w.id, since: w.since, look: *look }));
                }
                if let (Some(look), Some(m)) = (mark, &n.mark) {
                    let s = style.size;
                    for (baseline, x0, x1, k) in m.pieces(&n.live, t, s) {
                        let (l, r) = (Point::new(origin.x + x0, 0.0), Point::new(origin.x + x1, 0.0));
                        let c0 = look.color.fade(k * alpha);
                        let c = match look.gradient {
                            Some(c1) => Paint::linear(l, c0, r, c1.fade(k * alpha)),
                            None => Paint::solid(c0),
                        };
                        let shape = match look.shape {
                            MarkShape::Box => {
                                // The padding shrinks with a piece that is running off its line.
                                let pad = (0.18 * s).min(0.5 * (x1 - x0));
                                let r = Rect::new(
                                    origin.x + x0 - pad,
                                    origin.y + baseline - 0.92 * s,
                                    x1 - x0 + 2.0 * pad,
                                    1.22 * s,
                                );
                                ShapeInstance::rrect(RRect::new(r, 0.22 * s), c)
                            }
                            MarkShape::Underline | MarkShape::Karaoke => {
                                let h = (0.075 * s).max(1.5);
                                ShapeInstance::capsule(
                                    Rect::new(origin.x + x0, origin.y + baseline + 0.14 * s, x1 - x0, h),
                                    c,
                                )
                            }
                        };
                        push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
                        out.shapes.push(shape);
                    }
                }
                if !inline.is_empty() {
                    let mut shapes = Vec::new();
                    let place = |id: u32| {
                        let b = n.live.word_bounds(id, t, style.size)?;
                        let (k, dx, dy) = n.word_motion(id, t);
                        Some((Rect::new(origin.x + b.x + dx, origin.y + b.y + dy, b.w, b.h), k))
                    };
                    crate::inline::draw(words, inline, style.size, color, place, &mut shapes);
                    for shape in shapes {
                        push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
                        out.shapes.push(shape);
                    }
                }
                let starts = word_starts(words);
                let word_of = |cluster: u32| word_at(&starts, cluster);
                let accent = mark.as_deref().filter(|m| m.tints());
                if n.words.is_empty() || color.a <= 0.0 {
                    self.glyphs(n, style.size, origin, color, alpha, t, word_of, items, out);
                } else {
                    let fx = n.text_fx(t).faded(alpha);
                    let spans = glyph_spans(&n.shaped, style.size);
                    // Each word wipes over its own span, from its first glyph to its last.
                    // With it, for the typographic channels, how many glyphs each word has and
                    // the middle of its ink run.
                    let mut word_spans: Vec<(u32, f32, f32, usize, f32, f32)> = Vec::new();
                    for (g, &(x0, x1)) in n.shaped.glyphs.iter().zip(&spans) {
                        let word = word_of(g.cluster);
                        let (p0, p1) = (g.pos.x, g.pos.x + g.advance);
                        match word_spans.iter_mut().find(|w| w.0 == word) {
                            Some(w) => {
                                (w.1, w.2, w.3) = (w.1.min(x0), w.2.max(x1), w.3 + 1);
                                (w.4, w.5) = (w.4.min(p0), w.5.max(p1));
                            }
                            None => word_spans.push((word, x0, x1, 1, p0, p1)),
                        }
                    }
                    let mut letter = (NO_WORD, 0usize);
                    // The newest line's baseline, when breaches of the envelope are tinted.
                    let envelope_tint = match &n.kind {
                        Kind::LiveText { live, .. } => live.envelope_tint,
                        _ => None,
                    };
                    let newest_y =
                        envelope_tint.map(|_| n.shaped.glyphs.iter().map(|g| g.pos.y).fold(f32::MIN, f32::max));
                    let set = pass.as_ref().and_then(|_| n.pass_lines()).map(|(p, _, lines)| {
                        let pos = p.at * final_pass::total(&lines);
                        let newest = lines.iter().map(|l| l.baseline).fold(f32::NEG_INFINITY, f32::max);
                        let w = match n.pass_since {
                            Some(since) if !(n.reduced || p.still) => {
                                final_pass::ease_in(t - since, final_pass::SPOKEN_IN)
                            }
                            _ => 1.0,
                        };
                        (p, lines, pos, newest, w)
                    });
                    // Each word's glyph count, and how many of its glyphs are drawn, for the
                    // spoken ink's loose tracking.
                    let mut counts: Vec<(u32, u32, u32)> = Vec::new();
                    if set.is_some() {
                        for g in &n.shaped.glyphs {
                            let word = word_of(g.cluster);
                            match counts.iter_mut().find(|c| c.0 == word) {
                                Some(c) => c.1 += 1,
                                None => counts.push((word, 1, 0)),
                            }
                        }
                    }
                    for (i, (g, &span)) in n.shaped.glyphs.iter().zip(&spans).enumerate() {
                        let word = word_of(g.cluster);
                        let (k, dx, dy) = n.word_motion(word, t);
                        // Where the word and glyph are on their way to the laid-out place.
                        let m = n.live.glyph(i, t);
                        let (mut k, mut dx, mut dy) = (k * m.opacity, dx + m.dx, dy + m.dy);
                        letter = if letter.0 == word { (word, letter.1 + 1) } else { (word, 0) };
                        let ws = word_spans.iter().find(|w| w.0 == word).copied();
                        let mut weight = 0.0;
                        if let Some(v) = n.typo.sample(word, t) {
                            let v = if n.live.on_reading_line(word) { Envelope::READING.clamp(v) } else { v };
                            let (count, middle) = ws.map_or((1, g.pos.x), |w| (w.3, 0.5 * (w.4 + w.5)));
                            k *= v.letter_ink(letter.1, count);
                            // Tracking spreads the letters about the word's middle.
                            dx += (v.tracking - 1.0) * (g.pos.x + 0.5 * g.advance - middle);
                            dy -= v.baseline;
                            weight = v.weight;
                        }
                        let mut grow = 1.0;
                        let mut pop = 0.0;
                        if let Some((p, lines, pos, newest, w)) = &set {
                            // Spoken ink ahead of the front, set type behind it (MO-40).
                            let r = final_pass::reading_at(lines, g.pos.y, 0.5 * (span.0 + span.1));
                            let a = final_pass::ahead(r, *pos, p.front) * w;
                            k *= 1.0 - (1.0 - p.ink) * a;
                            if let Some(c) = counts.iter_mut().find(|c| c.0 == word) {
                                dx += p.loose * style.size * (c.2 as f32 - 0.5 * (c.1 as f32 - 1.0)) * a;
                                c.2 += 1;
                            }
                            if let Some(c) = n.pass_changed.iter().find(|c| c.id == word) {
                                let dt = t - c.at;
                                if !(n.reduced || p.still) {
                                    dy += p.nod * final_pass::nod(dt);
                                }
                                let ch = glyph_char(words, &starts, g.cluster);
                                let first = starts.iter().any(|s| s.1 == word && s.0 == g.cluster);
                                if ch.is_some_and(|ch| ch.is_ascii_punctuation()) && !c.old.contains(ch.unwrap()) {
                                    // New punctuation pops in.
                                    if !(n.reduced || p.still) {
                                        k *= final_pass::ease_in(dt, final_pass::POP);
                                        pop = 1.0 - final_pass::ease_in(dt, 2.0 * final_pass::POP);
                                    }
                                } else if first
                                    && ch.is_some_and(char::is_uppercase)
                                    && c.old.chars().next().is_some_and(char::is_lowercase)
                                    && !(n.reduced || p.still)
                                {
                                    // A new capital grows, except on the newest line.
                                    if g.pos.y < *newest {
                                        grow = 0.82 + 0.18 * final_pass::ease_in(dt, final_pass::GROW);
                                    } else {
                                        pop = 1.0 - final_pass::ease_in(dt, 2.0 * final_pass::POP);
                                    }
                                }
                            }
                        }
                        if k <= 0.0 || g.glyph == OBJECT_GLYPH || live.elsewhere.contains(&word) {
                            continue;
                        }
                        let (blur, dissolve, wipe) = n.word_fx(word, t);
                        let word_span = ws.map_or(span, |w| (w.1, w.2));
                        let e = GlyphFx {
                            weight: fx.weight + weight,
                            blur: fx.blur + blur,
                            dissolve: fx.dissolve.max(dissolve),
                            wipe: wipe_part(fx.wipe, (0.0, n.shaped.size.w), span)
                                .max(wipe_part(wipe, word_span, span)),
                            halo: fx.halo.fade(k),
                            ..fx
                        };
                        push_glyph_fx(out, e);
                        let amount = accent.map_or(0.0, |_| n.word_accent(word, t));
                        let color = match accent.filter(|_| amount > 0.0) {
                            Some(m) => {
                                // Where the glyph's centre sits across its word, for a gradient.
                                let centre = 0.5 * (span.0 + span.1);
                                let share = (centre - word_span.0) / (word_span.1 - word_span.0).max(1e-3);
                                let c = m.accent_at(share).unwrap_or(color);
                                mix(color, c.fade(alpha), amount)
                            }
                            None => color,
                        };
                        // The final pass's brighten pulse, toward white.
                        let lift = match (pass, n.word_passed(word)) {
                            (Some(p), Some(at)) => p.lift * final_pass::pulse(t - at),
                            _ => 0.0,
                        };
                        let lift = lift.max(pop);
                        let color =
                            if lift > 0.0 { mix(color, Color { a: color.a, ..Color::WHITE }, lift) } else { color };
                        push_item(items, ItemKind::Glyphs, out.glyphs.len() as u32);
                        out.glyphs.push(GlyphInstance {
                            pos: Point::new(origin.x + (g.pos.x + dx) * z, origin.y + (g.pos.y + dy) * z),
                            size: style.size * m.scale * grow * z,
                            font: g.font,
                            glyph: g.glyph,
                            word,
                            color: envelope_tint
                                .filter(|_| {
                                    newest_y.is_some_and(|y| g.pos.y >= y - 0.5)
                                        && crate::flow::breaks_envelope(k, dy, m.scale, blur)
                                })
                                .unwrap_or_else(|| m.tint(color))
                                .fade(k),
                        });
                    }
                    for gh in n.live.ghosts(t) {
                        let (k, dx, dy) = n.word_motion(gh.word, t);
                        let k = k * gh.opacity;
                        if k <= 0.0 || gh.glyph == OBJECT_GLYPH {
                            continue;
                        }
                        push_item(items, ItemKind::Glyphs, out.glyphs.len() as u32);
                        out.glyphs.push(GlyphInstance {
                            pos: Point::new(origin.x + (gh.pos.x + dx) * z, origin.y + (gh.pos.y + dy) * z),
                            size: style.size * gh.scale * z,
                            font: gh.font,
                            glyph: gh.glyph,
                            word: gh.word,
                            color: color.fade(k),
                        });
                    }
                }
                // Magnetic caret (MO-89): the field around the caret.
                let signals = self.signals.as_ref().map(|c| c.get());
                if let Some((m, bar, f)) = n.live.magnet_field(signals.as_deref(), t) {
                    for shape in m.shapes(bar, style.size, f, alpha, origin, z) {
                        push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
                        out.shapes.push(shape);
                    }
                }
                // Letter physics (MO-83): a faint motion vector on each letter in flight.
                for (pen, v, c) in n.live.letter_vectors(t) {
                    let k = (0.03 * v.x.hypot(v.y)).min(0.9 * style.size) / v.x.hypot(v.y);
                    let tip = Point::new(origin.x + pen.x * z, origin.y + (pen.y - 0.9 * style.size) * z);
                    let tail = Point::new(tip.x - v.x * k * z, tip.y - v.y * k * z);
                    push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
                    out.shapes.push(ShapeInstance::line(tail, tip, z, Paint::solid(c.fade(alpha))));
                }
                if let (Some(p), Some((baseline, x))) =
                    (pass, n.pass_lines().and_then(|(p, _, l)| final_pass::locate(&l, p.at)))
                {
                    // A bracket under each word the pass changed (MO-40).
                    if p.bracket.a > 0.0 && !n.pass_changed.is_empty() {
                        let s = style.size;
                        let boxes = word_boxes(&n.shaped, &starts);
                        for c in &n.pass_changed {
                            let Some(&(_, x0, x1, b)) = boxes.iter().find(|b| b.0 == c.id) else { continue };
                            let k = if n.reduced || p.still {
                                1.0
                            } else {
                                final_pass::ease_in(t - c.at, final_pass::BRACKET_IN)
                            };
                            let paint = Paint::solid(p.bracket.fade(alpha * k));
                            let (y, tick) = (origin.y + b + 0.2 * s, 0.14 * s);
                            let (l, r) = (origin.x + x0, origin.x + x1);
                            for (from, to) in [
                                (Point::new(l, y - tick), Point::new(l, y)),
                                (Point::new(l, y), Point::new(r, y)),
                                (Point::new(r, y), Point::new(r, y - tick)),
                            ] {
                                push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
                                out.shapes.push(ShapeInstance::line(from, to, 0.0, Paint::NONE).with_hairline(paint));
                            }
                        }
                    }
                    if p.line.a > 0.0 {
                        let s = style.size;
                        let top = Point::new(origin.x + x, origin.y + baseline - 0.95 * s);
                        let bottom = Point::new(origin.x + x, origin.y + baseline + 0.3 * s);
                        let line = ShapeInstance::line(top, bottom, 0.0, Paint::NONE)
                            .with_hairline(Paint::solid(p.line.fade(alpha)));
                        push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
                        out.shapes.push(line);
                    }
                }
                // Correction hinges and revision addresses (MO-21, MO-23).
                for m in n.live.marks(t) {
                    let r = m.rect;
                    let r = Rect::new(origin.x + r.x * z, origin.y + r.y * z, r.w * z, r.h * z);
                    push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
                    out.shapes.push(ShapeInstance::rrect(RRect::new(r, 0.0), Paint::solid(color.fade(m.alpha))));
                }
                if let Some((bar, tint, blink)) = n.live.caret(t) {
                    let c = tint.map_or(color, |c| c.fade(alpha)).fade(blink);
                    if c.a > 0.0 {
                        let r = Rect::new(origin.x + bar.x * z, origin.y + bar.y * z, bar.w * z, bar.h * z);
                        push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
                        let paint = match n.live.caret_style().and_then(|s| s.gradient) {
                            Some(g) => Paint::linear(
                                Point::new(r.x, r.y),
                                c,
                                Point::new(r.x, r.y + r.h),
                                g.fade(alpha).fade(blink),
                            ),
                            None => Paint::solid(c),
                        };
                        out.shapes.push(ShapeInstance::rrect(RRect::new(r, 0.5 * bar.w), paint));
                    }
                }
                None
            }
            Kind::Bars(b) => {
                self.timed_shapes(items, out, |v| b.shapes(rect, t, alpha, v));
                None
            }
            Kind::Wave(w) => {
                self.timed_shapes(items, out, |v| w.shapes(rect, t, alpha, v));
                None
            }
            Kind::Progress(p) => {
                self.timed_shapes(items, out, |v| p.shapes(rect, t, alpha, v));
                None
            }
            Kind::Ripples(r) => {
                if !n.reduced {
                    let runs = n.ambient.ripple_runs();
                    self.timed_shapes(items, out, |v| r.shapes(runs, rect, t, alpha, v));
                }
                None
            }
            Kind::Dots(d) => {
                self.dots(d, rect, t, alpha, items, out);
                None
            }
            Kind::StrokePath(art) => {
                self.stroke_path(art, n, t, origin, size, alpha, items, out);
                None
            }
            Kind::WavyLine(style) => {
                let v = |s: Slot| n.value(s as u8, t) as f32;
                let (amp, wavelength) = (v(Slot::WaveAmp), v(Slot::WaveLength).max(1.0));
                // A growing phase moves the crests to the right.
                let phase = -std::f32::consts::TAU * v(Slot::WavePhase);
                let y = origin.y + 0.5 * size.h;
                let line = crate::path::make::wave(origin.x, origin.x + size.w, y, amp, wavelength, phase);
                let len = line.length();
                let start = v(Slot::TrimStart).clamp(0.0, 1.0);
                let end = v(Slot::TrimEnd).clamp(0.0, 1.0);
                if end > start {
                    let line = line.trim(start * len, end * len);
                    if let Some(item) = crate::path::push_stroke(out, &line, &faded(style, alpha)) {
                        push_item(items, ItemKind::Vectors, item.start);
                    }
                }
                None
            }
            Kind::Morph(art) => {
                let fit = fit_view(art.view, origin, size);
                let v = n.value(Slot::Morph as u8, t) as f32;
                let parts = art.morph.sample(v);
                if art.filled {
                    let shapes: Vec<_> =
                        parts.iter().filter(|p| p.weight > 0.01).map(|p| p.contour.transform(&fit)).collect();
                    let style = faded(&art.style, alpha);
                    if let Some(item) = crate::path::push_fill(out, &shapes, &style) {
                        push_item(items, ItemKind::Vectors, item.start);
                    }
                } else {
                    for p in &parts {
                        let mut style = faded(&art.style, alpha * p.weight.sqrt());
                        style.width *= p.weight;
                        if let Some(item) = crate::path::push_stroke(out, &p.contour.transform(&fit), &style) {
                            push_item(items, ItemKind::Vectors, item.start);
                        }
                    }
                }
                None
            }
            // Drawn by `emit_body`, which knows the layer.
            Kind::Canvas(_) | Kind::Emitter(_) | Kind::Gpu(_) => None,
            Kind::Path { id, view } => {
                let paint = fill();
                if paint.is_visible() && view.w > 0.0 && view.h > 0.0 {
                    let k = (size.w / view.w).min(size.h / view.h);
                    let pos = Point::new(
                        origin.x + 0.5 * (size.w - view.w * k) - view.x * k,
                        origin.y + 0.5 * (size.h - view.h * k) - view.y * k,
                    );
                    push_item(items, ItemKind::Glyphs, out.glyphs.len() as u32);
                    out.glyphs.push(crate::path::fill_glyph(*id, pos, k, paint.c0));
                }
                None
            }
            Kind::Repeat(r) => {
                crate::repeat::draw(r, n, t, origin, size, alpha, items, out);
                None
            }
            Kind::Spectrum(s) => {
                crate::spectrum::draw(s, self.signals.as_ref(), t, origin, size, alpha, items, out);
                None
            }
            Kind::Frame(f) => {
                crate::frame::draw(f, n, t, origin, size, alpha, items, out);
                None
            }
            Kind::Stack => None,
        };
        if let Some(s) = shape.map(stroked) {
            // Shadow, then glow, then the shape: copies of its outline with a halo fill. The
            // stroke width stays so the halo starts at the stroke's outer edge.
            let halo = |fill: Paint| ShapeInstance { fill, stroke: Paint::NONE, flags: 0, ..s };
            let mut halos = [None, None];
            if n.has(Slot::ShadowColor as u8) {
                let offset = Point::new(n.value(Slot::ShadowX as u8, t) as f32, n.value(Slot::ShadowY as u8, t) as f32);
                let blur = n.value(Slot::ShadowBlur as u8, t).max(0.0) as f32;
                halos[0] = Some(halo(Paint::shadow(n.color(Slot::ShadowColor, t), offset, blur, 0.0).fade(alpha)));
            }
            if n.has(Slot::GlowColor as u8) {
                let radius = n.value(Slot::GlowRadius as u8, t).max(0.0) as f32;
                let strength = n.value(Slot::GlowStrength as u8, t).max(0.0) as f32;
                halos[1] = Some(halo(Paint::glow(n.color(Slot::GlowColor, t), radius, strength).fade(alpha)));
            }
            for h in halos.into_iter().flatten().filter(|h| h.fill.is_visible()) {
                push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
                out.shapes.push(h);
            }
            if s.fill.is_visible() || s.stroke.is_visible() {
                push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
                out.shapes.push(s);
            }
            // Rim and sheen (FX08): copies of the outline over the shape.
            let centre = Point::new(origin.x + 0.5 * size.w, origin.y + 0.5 * size.h);
            let mut overlays = [None, None];
            if let Some(u) = n.sheen_progress(t) {
                let c = n.color(Slot::SheenColor, t);
                // A band leaning right like a slash, a third of the height wide, from fully left of the
                // shape to fully right of it.
                let k = 0.33 * size.h.max(1.0);
                let half = Point::new(0.94 * k, 0.34 * k);
                let reach = 0.5 * size.w + 1.2 * size.h;
                let at = Point::new(centre.x - reach + 2.0 * reach * u, centre.y);
                overlays[0] = Some(ShapeInstance {
                    fill: Paint::sheen(at, half, c, c.fade(0.0)).fade(alpha),
                    stroke: Paint::NONE,
                    flags: 0,
                    ..s
                });
            }
            if n.has(Slot::RimColor as u8) {
                let c = n.color(Slot::RimColor, t);
                let span = n.value(Slot::RimSpan as u8, t) as f32;
                let paint = Paint::conic(centre, c, c.fade(0.0), n.rim_angle(t), span).fade(alpha);
                let width = n.value(Slot::RimWidth as u8, t).max(0.0) as f32;
                overlays[1] = Some(ShapeInstance { fill: Paint::NONE, flags: 0, ..s }.with_stroke(paint, width));
            }
            for o in overlays.into_iter().flatten().filter(|o| o.fill.is_visible() || o.stroke.is_visible()) {
                push_item(items, ItemKind::Shapes, out.shapes.len() as u32);
                out.shapes.push(o);
            }
        }
    }

    /// A stroked outline: optional fill, the trimmed stroke, and the light head.
    #[allow(clippy::too_many_arguments)]
    fn stroke_path(
        &self,
        art: &crate::element::StrokeArt,
        n: &Node<M>,
        t: f64,
        origin: Point,
        size: Size,
        alpha: f32,
        items: &mut Vec<Item>,
        out: &mut DisplayList,
    ) {
        use crate::path::{push_fill, push_stroke, StrokeStyle};
        let o = art.outline.transform(&fit_view(art.view, origin, size));
        let len = o.length();
        if let Some(c) = art.fill {
            if let Some(item) = push_fill(out, &o.contours, &StrokeStyle::solid(0.0, c.fade(alpha))) {
                push_item(items, ItemKind::Vectors, item.start);
            }
        }
        let start = n.value(Slot::TrimStart as u8, t).clamp(0.0, 1.0) as f32;
        let end = n.value(Slot::TrimEnd as u8, t).clamp(0.0, 1.0) as f32;
        let style = faded(&art.style, alpha);
        for c in &o.trim(start * len, end * len).contours {
            if let Some(item) = push_stroke(out, c, &style) {
                push_item(items, ItemKind::Vectors, item.start);
            }
        }
        if let (Some((tail, laps, color, glow)), Some(first)) = (art.head, o.contours.first()) {
            let l0 = first.length();
            if first.closed && l0 > 0.0 {
                let head = (t * laps as f64).fract() as f32 * l0;
                let tail = tail.clamp(0.0, 1.0) * l0;
                let c = color.fade(alpha);
                let s = StrokeStyle::solid(art.style.width, c).ramp(head - tail, c.fade(0.0), head, c).glow(glow);
                if let Some(item) = push_stroke(out, &o.window(0, head, tail), &s) {
                    push_item(items, ItemKind::Vectors, item.start);
                }
            }
        }
    }

    /// Shapes from a timed element, appended in order.
    fn timed_shapes(&self, items: &mut Vec<Item>, out: &mut DisplayList, f: impl FnOnce(&mut Vec<ShapeInstance>)) {
        let start = out.shapes.len();
        f(&mut out.shapes);
        for i in start..out.shapes.len() {
            push_item(items, ItemKind::Shapes, i as u32);
        }
    }

    /// A dot grid's dots in `rect`, one run of `ItemKind::Dots`.
    fn dots(&self, d: &Dots, rect: Rect, t: f64, alpha: f32, items: &mut Vec<Item>, out: &mut DisplayList) {
        let pitch = d.snapped_pitch(self.scale);
        let (ox, oy) = (rect.x + d.origin.0 * pitch, rect.y + d.origin.1 * pitch);
        let mut cells = Vec::new();
        d.source.dots(t, &mut cells);
        let start = out.dots.len();
        out.dots.extend(cells.iter().filter(|c| c.color.a * alpha > 0.0 && c.size > 0.0).map(|c| DotInstance {
            centre: Point::new(ox + c.x * pitch, oy + c.y * pitch),
            radius: 0.5 * c.size * pitch,
            snap: c.snap.clamp(0.0, 1.0),
            color: c.color.fade(alpha),
            ring: c.ring.max(0.0) * c.size * pitch,
        }));
        for i in start..out.dots.len() {
            push_item(items, ItemKind::Dots, i as u32);
        }
    }

    /// A text node's glyphs, with its text effects; `alpha` is the opacity from its parents.
    #[allow(clippy::too_many_arguments)]
    fn glyphs(
        &self,
        n: &Node<M>,
        size: f32,
        origin: Point,
        color: Color,
        alpha: f32,
        t: f64,
        word_of: impl Fn(u32) -> u32,
        items: &mut Vec<Item>,
        out: &mut DisplayList,
    ) {
        if color.a <= 0.0 {
            return;
        }
        let fx = n.text_fx(t).faded(alpha);
        let spans = (!fx.is_none() && fx.wipe > 0.0).then(|| glyph_spans(&n.shaped, size));
        let z = n.value(Slot::TextZoom as u8, t) as f32;
        for (i, g) in n.shaped.glyphs.iter().enumerate() {
            if g.glyph == OBJECT_GLYPH {
                continue;
            }
            if !fx.is_none() {
                let wipe = spans.as_ref().map_or(0.0, |s| wipe_part(fx.wipe, (0.0, n.shaped.size.w), s[i]));
                push_glyph_fx(out, GlyphFx { wipe, ..fx });
            }
            push_item(items, ItemKind::Glyphs, out.glyphs.len() as u32);
            out.glyphs.push(GlyphInstance {
                pos: Point::new(origin.x + g.pos.x * z, origin.y + g.pos.y * z),
                size: size * z,
                font: g.font,
                glyph: g.glyph,
                word: word_of(g.cluster),
                color,
            });
        }
    }
}

/// Records text effects for the glyph about to be pushed, unless there are none.
fn push_glyph_fx(out: &mut DisplayList, fx: GlyphFx) {
    if !fx.is_none() {
        out.glyph_fx.push((out.glyphs.len() as u32, fx));
    }
}

/// Each glyph's span along its line: from its pen position to the next glyph's on the same
/// line, or half an em for the last.
fn glyph_spans(shaped: &ShapedText, size: f32) -> Vec<(f32, f32)> {
    let g = &shaped.glyphs;
    (0..g.len())
        .map(|i| {
            let x0 = g[i].pos.x;
            let next = g.get(i + 1).filter(|n| n.pos.y == g[i].pos.y && n.pos.x > x0);
            (x0, next.map_or(x0 + 0.5 * size, |n| n.pos.x))
        })
        .collect()
}

/// How much of a glyph spanning `glyph` a wipe hides when it has hidden `w` (0 to 1) of
/// `span` from the right.
fn wipe_part(w: f32, span: (f32, f32), glyph: (f32, f32)) -> f32 {
    if w <= 0.0 {
        return 0.0;
    }
    if w >= 1.0 {
        return 1.0;
    }
    let front = span.0 + (1.0 - w) * (span.1 - span.0);
    ((glyph.1 - front) / (glyph.1 - glyph.0).max(1e-3)).clamp(0.0, 1.0)
}

fn radius<M>(n: &Node<M>, t: f64) -> f32 {
    n.value(Slot::Radius as u8, t).max(0.0) as f32
}

/// Node space to parent space at `t`, the box size, and the node's own opacity.
fn local_transform<M>(n: &Node<M>, t: f64) -> (Affine, Size, f32) {
    let v = |s: Slot| n.value(s as u8, t) as f32;
    let size = Size::new(v(Slot::W).max(0.0), v(Slot::H).max(0.0));
    let (x, y) = (v(Slot::X) + v(Slot::OffsetX), v(Slot::Y) + v(Slot::OffsetY));
    let (scale, rotation) = (v(Slot::Scale), v(Slot::Rotation));
    let opacity = v(Slot::Opacity).clamp(0.0, 1.0);
    let m = if scale == 1.0 && rotation == 0.0 {
        Affine::translate(x, y)
    } else {
        let (cx, cy) = (0.5 * size.w, 0.5 * size.h);
        Affine::translate(-cx, -cy)
            .then(&Affine::scale(scale))
            .then(&Affine::rotate(rotation))
            .then(&Affine::translate(cx + x, cy + y))
    };
    (m, size, opacity)
}

/// Appends one instance at `index` to the item list, extending the last run when it is the
/// same kind and adjacent.
/// The transform that fits `view` into the box at `origin`, centred, keeping its aspect.
fn fit_view(view: Rect, origin: Point, size: Size) -> Affine {
    if view.w <= 0.0 || view.h <= 0.0 {
        return Affine::translate(origin.x, origin.y);
    }
    let k = (size.w / view.w).min(size.h / view.h);
    Affine::scale(k).then(&Affine::translate(
        origin.x + 0.5 * (size.w - view.w * k) - view.x * k,
        origin.y + 0.5 * (size.h - view.h * k) - view.y * k,
    ))
}

/// `style` with `alpha` multiplied into both colours.
/// The font size of a label or live text.
fn text_size(kind: &Kind) -> Option<f32> {
    match kind {
        Kind::Label { style, .. } | Kind::LiveText { style, .. } => Some(style.size),
        _ => None,
    }
}

fn faded(style: &crate::path::StrokeStyle, alpha: f32) -> crate::path::StrokeStyle {
    crate::path::StrokeStyle { c0: style.c0.fade(alpha), c1: style.c1.fade(alpha), ..*style }
}

/// The box `(0, 0, size)` through `m`, as `x0, y0, x1, y1`.
fn affine_bounds(m: &Affine, size: Size) -> [f32; 4] {
    let pts =
        [Point::ZERO, Point::new(size.w, 0.0), Point::new(0.0, size.h), Point::new(size.w, size.h)].map(|p| m.apply(p));
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in pts {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    [x0, y0, x1, y1]
}

/// The product of the opacities of `layer` and every layer above it.
fn layer_chain_opacity(layers: &[Layer], layer: u32) -> f32 {
    let mut o = 1.0;
    let mut li = Some(layer);
    while let Some(i) = li {
        let l = &layers[i as usize];
        o *= l.opacity;
        li = l.parent;
    }
    o
}

/// Points the shapes, glyphs and vectors from `firsts` on that have no material yet at `m`
/// (FX15). Glows and shadows stay plain.
fn apply_material(out: &mut DisplayList, firsts: [usize; 3], m: Material) {
    let [s0, g0, v0] = firsts;
    if [out.shapes.len(), out.glyphs.len(), out.vectors.len()] == firsts {
        return;
    }
    let id = out.add_material(m);
    for s in &mut out.shapes[s0..] {
        if s.material() == 0 && !s.fill.is_halo() {
            *s = s.with_material(id);
        }
    }
    for v in &mut out.vectors[v0..] {
        if v.flags >> MATERIAL_SHIFT == 0 {
            v.flags |= id << MATERIAL_SHIFT;
        }
    }
    let (g0, g1) = (g0 as u32, out.glyphs.len() as u32);
    if g1 > g0 {
        // The effects of these glyphs are the sorted tail of `glyph_fx`: merge in an entry
        // for every glyph.
        let tail = out.glyph_fx.partition_point(|(i, _)| *i < g0);
        let old: Vec<(u32, GlyphFx)> = out.glyph_fx.drain(tail..).collect();
        let mut old = old.into_iter().peekable();
        for i in g0..g1 {
            let fx = if old.peek().is_some_and(|(j, _)| *j == i) { old.next().unwrap().1 } else { GlyphFx::default() };
            let material = if fx.material == 0 { id } else { fx.material };
            out.glyph_fx.push((i, GlyphFx { material, ..fx }));
        }
    }
}

pub(crate) fn push_item(items: &mut Vec<Item>, kind: ItemKind, index: u32) {
    if let Some(last) = items.last_mut() {
        if last.kind == kind && last.start + last.len == index {
            last.len += 1;
            return;
        }
    }
    items.push(Item { kind, start: index, len: 1 });
}

/// Fills `keep` on each displacement that keeps the reading line (EG-03): the row of the
/// newest live-text word, found as the envelope check finds it (the newest word's median
/// baseline, every glyph within half an em of it), boxed generously in surface space.
fn keep_reading_line(out: &mut DisplayList) {
    if !out.layers.iter().any(|l| l.displace.is_some_and(|d| d.keep_reading)) {
        return;
    }
    // Each live-text glyph in surface space: word, pen x, baseline y, em.
    let mut glyphs: Vec<(u32, f32, f32, f32)> = Vec::new();
    for layer in &out.layers {
        let m = layer.transform;
        let k = (m.a * m.d - m.b * m.c).abs().sqrt();
        for item in &out.items[layer.items.start as usize..layer.items.end as usize] {
            if item.kind != ItemKind::Glyphs {
                continue;
            }
            for g in &out.glyphs[item.start as usize..(item.start + item.len) as usize] {
                if g.word != NO_WORD {
                    let p = m.apply(g.pos);
                    glyphs.push((g.word, p.x, p.y, g.size * k));
                }
            }
        }
    }
    let Some(newest) = glyphs.iter().map(|g| g.0).max() else { return };
    let mut ys: Vec<f32> = glyphs.iter().filter(|g| g.0 == newest).map(|g| g.2).collect();
    ys.sort_by(f32::total_cmp);
    let y = ys[ys.len() / 2];
    let em = glyphs.iter().filter(|g| g.0 == newest).map(|g| g.3).fold(0.0, f32::max);
    let (mut x0, mut x1) = (f32::INFINITY, f32::NEG_INFINITY);
    for g in glyphs.iter().filter(|g| (g.2 - y).abs() < 0.5 * em.max(g.3)) {
        x0 = x0.min(g.1 - 0.25 * g.3);
        // The pen position is the glyph's left; its advance is under an em for text.
        x1 = x1.max(g.1 + 1.1 * g.3);
    }
    let keep = Rect::new(x0, y - 1.1 * em, x1 - x0, 1.5 * em);
    for l in &mut out.layers {
        if let Some(d) = l.displace.as_mut().filter(|d| d.keep_reading) {
            d.keep = Some(keep);
        }
    }
}

/// Moves a layer's items into the list and returns their range.
pub(crate) fn close_items(items: &mut Vec<Item>, out: &mut DisplayList) -> std::ops::Range<u32> {
    let start = out.items.len() as u32;
    out.items.append(items);
    start..out.items.len() as u32
}

/// Sibling keys must differ. Checked in debug builds only.
fn check_keys<M>(children: &[Element<M>], parent: impl FnOnce() -> String) {
    if cfg!(debug_assertions) && children.len() > 1 {
        let mut keys: Vec<&Key> = children.iter().map(|c| &c.key).collect();
        keys.sort();
        if let Some(w) = keys.windows(2).find(|w| w[0] == w[1]) {
            panic!("duplicate key '{}' among the children of {}", w[0], parent());
        }
    }
}

impl From<NodeId> for taffy::NodeId {
    fn from(id: NodeId) -> taffy::NodeId {
        taffy::NodeId::from(id.to_bits())
    }
}

impl From<taffy::NodeId> for NodeId {
    fn from(id: taffy::NodeId) -> NodeId {
        NodeId::from_bits(u64::from(id))
    }
}
