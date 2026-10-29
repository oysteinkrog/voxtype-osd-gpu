//! Kinetic lattice (MO-85): the words of a sentence joined by a faint lattice of thin lines
//! that encode timing, in a technical drawing style.
//!
//! Each word has a node a little under its baseline, at its left edge. Between two words
//! runs a bundle of strands that bow apart into a lens and meet at the nodes:
//!
//! - **pause is length**: the longer the pause before a word, the further its strands bow,
//!   so a slow gap draws a long slack curve and a quick one a short taut line;
//! - **loudness is thickness**: a strand is as heavy as the word it leads into was loud;
//! - **arrival tightens the lattice**: when a word arrives its strands draw on from the
//!   previous node, and every strand pulls taut and relaxes back on a damped swing.
//!
//! Faint arcs that skip one word tie the bundles into a lattice. Under each node a dashed
//! drop line ends in a small label with the pause in milliseconds.
//!
//! The words themselves are drawn by the caller with the same layout, so the reading line
//! never moves (rule 1). [`anchors`] places the nodes from that layout.
//!
//! Plucked strings (LT-02): [`LatticeLook::plucks`] lists [`Pluck`] events. Each bundle
//! rings as a damped sine summed over the last four plucks, and a pluck spreads to the
//! neighbouring bundles, 40 ms later and half as strong per bundle. A live tension can come
//! from a [`LevelTrack`] instead of the arrival swing: under [`LatticeLook::level_floor`] the
//! strands hang slack, above it they pull taut with the level.
//!
//! Sentences, revisions, pitch and the live strand (LT-03): a [`LatticeWord`] carries its
//! [`Boundary`], pitch and revision time. Skip arcs stop at sentence ends, a sentence-end node
//! gets a second ring, a faint arc spans each finished sentence, and a comma adds slack to the
//! next bundle. A revision flashes the node's ring, plucks its bundle again and dashes it for
//! [`REVISION_DASH`] seconds. Pitch picks the bundle's colour on the `color` to `accent` ramp.
//! [`LatticeLook::live`] draws a strand from the newest node to the caret that slackens over
//! [`LIVE_SLACK`] seconds of silence and trembles with the level.
//!
//! The drawing is a pure function of `t`, the words' arrival times, the plucks and the
//! level: there is no simulation state, so any `t` can be drawn in any order. Under reduced
//! motion strands are drawn whole and still, at their resting bow, and plucks and the level
//! are ignored.

use std::sync::Arc;

use crate::canvas::Painter;
use crate::display::{Paint, ShapeInstance};
use crate::element::{Element, Key, TextStyle};
use crate::emitter::LevelTrack;
use crate::geom::{Color, Point, Rect, Size};
use crate::path::{Contour, StrokeStyle};
use crate::taffy;
use crate::taffy::prelude::{auto, length};
use crate::text::TextLayout;

/// A word in the lattice. Build it with [`LatticeWord::new`] or `..LatticeWord::default()`
/// so new fields keep their defaults.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LatticeWord {
    /// The pause before the word, in seconds (0 for the first word).
    pub pause: f32,
    /// How loud the word was, 0 to 1.
    pub loud: f32,
    /// Scene time the word arrived on screen.
    pub at: f64,
    /// The word's pitch, 0 (low) to 1 (high): its bundle's colour on the `color` to `accent`
    /// ramp. `None` keeps `color`.
    pub pitch: Option<f32>,
    /// Scene time the word was last revised: its node flashes, its bundle is plucked again
    /// and drawn dashed for [`REVISION_DASH`] seconds.
    pub changed: Option<f64>,
    /// The punctuation after the word.
    pub boundary: Boundary,
}

impl LatticeWord {
    pub fn new(pause: f32, loud: f32, at: f64) -> LatticeWord {
        LatticeWord { pause, loud, at, ..LatticeWord::default() }
    }
}

/// What ends a word: nothing, a comma or a sentence end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Boundary {
    #[default]
    None,
    Comma,
    Sentence,
}

/// The strand from the newest node to the caret while the speaker pauses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveStrand {
    /// Index of the word whose node the strand starts at.
    pub from_node: usize,
    /// The caret's x in the text box; the strand ends there at the node's height.
    pub caret_x: f32,
    /// Scene time of the last word: the strand slackens with the time since.
    pub since: f64,
}

/// Seconds a revised word's bundle is drawn dashed.
pub const REVISION_DASH: f64 = 0.5;
/// Seconds a revised word's node ring flashes.
pub const REVISION_FLASH: f64 = 0.4;
/// Seconds of silence over which the live strand slackens to its full bow.
pub const LIVE_SLACK: f64 = 3.0;

/// A pluck of one bundle. Bundle `i` is the bundle of strands that leads into word `i`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pluck {
    /// Scene time of the pluck.
    pub at: f64,
    pub bundle: usize,
    /// Peak swing of the plucked bundle, in logical pixels.
    pub amp: f32,
}

/// Seconds a pluck takes to reach the next bundle.
pub const PLUCK_SPREAD: f64 = 0.04;
/// How much of a pluck reaches the next bundle.
pub const PLUCK_FALLOFF: f32 = 0.5;
/// How many of the latest plucks ring at once.
pub const PLUCK_RING: usize = 4;
/// A pluck is still within this many seconds after it.
pub const PLUCK_SETTLE: f64 = 2.5;
/// The swing, in pixels, under which the strings count as still.
pub const PLUCK_STILL: f32 = 0.05;
/// Each ringing pluck settles under this, so all of them together stay under [`PLUCK_STILL`].
const PLUCK_EPS: f32 = PLUCK_STILL / PLUCK_RING as f32;

/// Where a word's node sits: its left edge, its baseline and its line, in the text box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub x: f32,
    pub width: f32,
    pub baseline: f32,
    pub line: usize,
}

/// The strip under one text line that strands may use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineGutter {
    /// Top of the strip, in the text box (just under the line's descenders).
    pub top: f32,
    /// Height of the strip. Under 6 px bundles thin to one strand.
    pub height: f32,
    /// The newest line: nodes sit at baseline + `descent` + `drop` and every strand bows down only.
    pub newest: bool,
    /// How far the line's glyphs reach under the baseline.
    pub descent: f32,
}

/// How the lattice looks.
#[derive(Clone, Debug, PartialEq)]
pub struct LatticeLook {
    /// Strands per bundle.
    pub strands: usize,
    /// Logical pixels of bow per second of pause, and the least and most bow.
    pub bow_per_sec: f32,
    pub bow: (f32, f32),
    /// Strand width for a silent word and for the loudest one.
    pub width: (f32, f32),
    pub color: Color,
    /// The newest node and its strands.
    pub accent: Color,
    /// How far under the baseline the nodes sit.
    pub drop: f32,
    pub node_radius: f32,
    /// How much an arrival pulls the strands taut (0 none, 1 straight).
    pub tighten: f32,
    /// Seconds a strand takes to draw on from the previous node.
    pub draw_on: f64,
    /// Labels under the nodes with the pause in ms, or `None` for no labels.
    pub label: Option<TextStyle>,
    pub label_color: Color,
    /// Length of the dashed drop line from a node to its label.
    pub stem: f32,
    /// Per text line (indexed by `Anchor::line`) the gutter its strands stay in. Empty, or a
    /// line past the end, draws as before.
    pub gutters: Vec<LineGutter>,
    /// Plucks in time order. Empty: no bundle rings on its own.
    pub plucks: Arc<[Pluck]>,
    /// Swings per second of a plucked bundle, and seconds for its swing to fall to 1/e
    /// (shorter for a big pluck, so it still settles within [`PLUCK_SETTLE`]).
    pub pluck_hz: f32,
    pub pluck_decay: f32,
    /// The live tension. `None` keeps the swing after each arrival ([`LatticeLook::tension`]).
    pub level: Option<LevelTrack>,
    /// Level under which the strands hang slack; from there to 1 they pull taut by up to
    /// `tighten`.
    pub level_floor: f32,
    /// Extra bow, in pixels, of the bundle after a comma.
    pub comma_slack: f32,
    /// Peak swing of the pluck a revision gives its bundle, in pixels.
    pub revision_amp: f32,
    /// Peak tremble of the live strand at full level, in pixels.
    pub live_tremble: f32,
    /// The live strand to the caret, or `None` for none.
    pub live: Option<LiveStrand>,
}

impl Default for LatticeLook {
    fn default() -> LatticeLook {
        LatticeLook {
            strands: 4,
            bow_per_sec: 30.0,
            bow: (2.0, 14.0),
            width: (0.6, 2.2),
            color: Color::rgba(0.82, 0.86, 0.9, 0.55),
            accent: Color::hex(0x2de2c8, 1.0),
            drop: 7.0,
            node_radius: 2.0,
            tighten: 0.6,
            draw_on: 0.18,
            label: None,
            label_color: Color::rgba(0.7, 0.74, 0.8, 0.7),
            stem: 10.0,
            gutters: Vec::new(),
            plucks: Arc::from([]),
            pluck_hz: 6.0,
            pluck_decay: 0.35,
            level: None,
            level_floor: 0.08,
            comma_slack: 4.0,
            revision_amp: 5.0,
            live_tremble: 1.2,
            live: None,
        }
    }
}

/// The nodes of `words` set in `style` at `width` by `layout`, as the caller's text is laid
/// out when it shows the words joined by single spaces.
pub fn anchors(layout: &dyn TextLayout, words: &[&str], style: &TextStyle, width: Option<f32>) -> Vec<Anchor> {
    let text = words.join(" ");
    let shaped = layout.layout(&text, style, width);
    let mut starts = Vec::with_capacity(words.len());
    let mut at = 0usize;
    for w in words {
        starts.push(at..at + w.len());
        at += w.len() + 1;
    }
    let line_of = |gi: usize| shaped.lines.iter().position(|l| l.glyphs.contains(&gi)).unwrap_or(0);
    starts
        .iter()
        .map(|range| {
            let mut lo = f32::INFINITY;
            let mut hi = f32::NEG_INFINITY;
            let mut line = None;
            for (gi, g) in shaped.glyphs.iter().enumerate() {
                if range.contains(&(g.cluster as usize)) {
                    lo = lo.min(g.pos.x);
                    hi = hi.max(g.pos.x + g.advance);
                    line.get_or_insert(line_of(gi));
                }
            }
            let line = line.unwrap_or(shaped.lines.len().saturating_sub(1));
            let baseline = shaped.lines.get(line).map_or(style.size, |l| l.baseline);
            if lo.is_finite() {
                Anchor { x: lo, width: hi - lo, baseline, line }
            } else {
                Anchor { x: 0.0, width: 0.0, baseline, line }
            }
        })
        .collect()
}

impl LatticeLook {
    /// The tension multiplier on every bow at `t`: 1 at rest, pulled down by the newest
    /// arrival at `last` and swinging back a little past rest before it settles.
    pub fn tension(&self, last: Option<f64>, t: f64) -> f32 {
        let Some(at) = last else { return 1.0 };
        let u = t - at;
        if u < 0.0 {
            return 1.0;
        }
        (1.0 - self.tighten as f64 * (-u / 0.3).exp() * (u * 7.0).cos()) as f32
    }

    /// The tension multiplier at `t` from [`LatticeLook::level`]: 1 (slack, at rest) while the
    /// level is under the floor, down to `1 - tighten` at full level. Without a level, the
    /// arrival swing of [`LatticeLook::tension`] for the newest arrival at `last`.
    pub fn live_tension(&self, last: Option<f64>, t: f64) -> f32 {
        let Some(level) = &self.level else { return self.tension(last, t) };
        let floor = self.level_floor.clamp(0.0, 0.99);
        let k = ((level.at(t) - floor) / (1.0 - floor)).clamp(0.0, 1.0);
        1.0 - self.tighten * k
    }

    /// The swing of one pluck's share at `d` bundles away: `(start, amp, decay)`, or `None`
    /// when it never reaches.
    fn ring(&self, p: &Pluck, d: usize) -> Option<(f64, f32, f32)> {
        let delay = PLUCK_SPREAD * d as f64;
        if delay >= PLUCK_SETTLE {
            return None;
        }
        let amp = p.amp.abs() * PLUCK_FALLOFF.powi(d.min(64) as i32);
        if amp <= 0.0 {
            return None;
        }
        let mut decay = self.pluck_decay.max(1e-3);
        if amp > PLUCK_EPS {
            // Fall under PLUCK_EPS by PLUCK_SETTLE after the pluck at the latest.
            decay = decay.min(((PLUCK_SETTLE - delay) as f32) / (amp / PLUCK_EPS).ln());
        }
        Some((p.at + delay, amp * p.amp.signum(), decay))
    }

    /// How far bundle `bundle` swings off its bow at `t`, in pixels: a damped sine per pluck,
    /// summed over the last [`PLUCK_RING`] plucks at or before `t`.
    pub fn pluck_offset(&self, bundle: usize, t: f64) -> f32 {
        self.offset_in(&self.plucks, bundle, t)
    }

    fn offset_in(&self, plucks: &[Pluck], bundle: usize, t: f64) -> f32 {
        let end = plucks.partition_point(|p| p.at <= t);
        let w = std::f32::consts::TAU * self.pluck_hz;
        plucks[end.saturating_sub(PLUCK_RING)..end]
            .iter()
            .filter_map(|p| self.ring(p, bundle.abs_diff(p.bundle)))
            .map(|(start, amp, decay)| {
                let u = (t - start) as f32;
                if u < 0.0 {
                    0.0
                } else {
                    amp * (-u / decay).exp() * (w * u).sin()
                }
            })
            .sum()
    }

    /// [`LatticeLook::plucks`] with a pluck of [`LatticeLook::revision_amp`] on each revised
    /// word's bundle at its revision time, in time order.
    pub fn plucks_with_revisions(&self, words: &[LatticeWord]) -> Arc<[Pluck]> {
        if words.iter().all(|w| w.changed.is_none()) {
            return self.plucks.clone();
        }
        let mut all: Vec<Pluck> = self.plucks.to_vec();
        all.extend(
            words
                .iter()
                .enumerate()
                .filter_map(|(i, w)| w.changed.map(|at| Pluck { at, bundle: i, amp: self.revision_amp })),
        );
        all.sort_by(|a, b| a.at.total_cmp(&b.at));
        all.into()
    }

    /// Whether `w`'s bundle is drawn dashed at `t`: from its revision for [`REVISION_DASH`]
    /// seconds.
    pub fn dashed(&self, w: &LatticeWord, t: f64) -> bool {
        w.changed.is_some_and(|c| t >= c && t < c + REVISION_DASH)
    }

    /// A strand colour for `pitch` on the ramp from `color` (0) to `accent` (1).
    pub fn pitch_color(&self, pitch: Option<f32>) -> Color {
        let Some(k) = pitch else { return self.color };
        let k = k.clamp(0.0, 1.0);
        let (a, b) = (self.color, self.accent);
        let l = |x: f32, y: f32| x * (1.0 - k) + y * k;
        Color::rgba(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b), l(a.a, b.a))
    }

    /// The arcs that skip a word, as `[a, b, c]` over the shown words: never across a
    /// sentence end, so a sentence's lattice stops at its last word.
    pub fn skip_arcs(words: &[LatticeWord], nodes: &[Anchor], shown: &[usize]) -> Vec<[usize; 3]> {
        shown
            .windows(3)
            .filter(|w| nodes[w[0]].line == nodes[w[2]].line)
            .filter(|w| (w[0]..w[2]).all(|i| words[i].boundary != Boundary::Sentence))
            .map(|w| [w[0], w[1], w[2]])
            .collect()
    }

    /// Each finished sentence as `(first, last)` word, for those of two or more words whose
    /// ends share a line, among the first `n` words.
    pub fn sentences(words: &[LatticeWord], nodes: &[Anchor], n: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut start = 0;
        for i in 0..n.min(words.len()).min(nodes.len()) {
            if words[i].boundary == Boundary::Sentence {
                if i > start && nodes[start].line == nodes[i].line {
                    out.push((start, i));
                }
                start = i + 1;
            }
        }
        out
    }

    /// The live strand's bow at `t`: from the least bow at its last word up to the most over
    /// [`LIVE_SLACK`] seconds of silence. Under reduced motion it stays at the most.
    pub fn live_bow(&self, live: &LiveStrand, t: f64, reduced: bool) -> f32 {
        if reduced {
            return self.bow.1;
        }
        let k = ((t - live.since) / LIVE_SLACK).clamp(0.0, 1.0) as f32;
        let k = k * k * (3.0 - 2.0 * k);
        self.bow.0 + (self.bow.1 - self.bow.0) * k
    }

    /// How far the live strand trembles off its bow at `t`: with the level over its floor.
    pub fn live_tremble_at(&self, t: f64) -> f32 {
        let Some(level) = &self.level else { return 0.0 };
        let floor = self.level_floor.clamp(0.0, 0.99);
        let k = ((level.at(t) - floor) / (1.0 - floor)).clamp(0.0, 1.0);
        self.live_tremble * k * (std::f64::consts::TAU * 9.0 * t).sin() as f32
    }

    /// Seconds after `p` until every bundle it reaches swings under [`PLUCK_STILL`] (at most
    /// [`PLUCK_SETTLE`]).
    pub fn pluck_settle(&self, p: &Pluck) -> f64 {
        (0..)
            .map_while(|d| self.ring(p, d).map(|r| (d, r)))
            .take_while(|&(d, (_, amp, _))| d == 0 || amp.abs() > PLUCK_EPS)
            .map(|(d, (_, amp, decay))| {
                let delay = PLUCK_SPREAD * d as f64;
                let a = amp.abs();
                if a <= PLUCK_EPS {
                    delay
                } else {
                    (delay + (decay * (a / PLUCK_EPS).ln()) as f64).min(PLUCK_SETTLE)
                }
            })
            .fold(0.0, f64::max)
    }

    /// The resting bow of a bundle after a pause of `pause` seconds.
    pub fn rest_bow(&self, pause: f32) -> f32 {
        (pause.max(0.0) * self.bow_per_sec).clamp(self.bow.0, self.bow.1)
    }

    fn gutter(&self, a: &Anchor) -> Option<LineGutter> {
        self.gutters.get(a.line).copied()
    }

    /// Where the node of `a` sits vertically.
    pub fn node_y(&self, a: &Anchor) -> f32 {
        match self.gutter(a) {
            None => a.baseline + self.drop,
            Some(g) if g.newest => a.baseline + g.descent + self.drop,
            Some(g) => g.top + g.height * 0.5,
        }
    }

    /// The strands between nodes `a` and `b` (same line): `(curve, edge)` with edge 1 for the
    /// middle strand and less for outer ones. `bow` is the unsigned bow before the gutter
    /// clamp; `skip` draws a single arc.
    pub fn strands(&self, a: &Anchor, b: &Anchor, bow: f32, skip: bool) -> Vec<(Contour, f32)> {
        let (pa, pb) = (Point::new(a.x, self.node_y(a)), Point::new(b.x, self.node_y(b)));
        let g = self.gutter(a);
        let (lo, hi) = match g {
            None => (-bow.abs(), bow.abs()),
            Some(g) if g.newest => (0.0, bow.abs().min((g.top + g.height - pa.y.max(pb.y)).max(0.0))),
            Some(g) => {
                let room = (pa.y.min(pb.y) - g.top).min(g.top + g.height - pa.y.max(pb.y)).max(0.0);
                (-bow.abs().min(room), bow.abs().min(room))
            }
        };
        let s = if skip || g.is_some_and(|g| g.height < 6.0) { 1 } else { self.strands.max(1) };
        let mut out = Vec::with_capacity(s);
        for k in 0..s {
            if skip {
                // Arcs rise (negative) as before; in a gutter the newest line sends them down.
                let off = if g.is_some_and(|g| g.newest) {
                    hi
                } else if g.is_some() {
                    -hi.abs()
                } else {
                    -bow.abs()
                };
                out.push((lens(pa, pb, off, 20), 1.0));
                continue;
            }
            let f = if s == 1 { 0.0 } else { 2.0 * k as f32 / (s - 1) as f32 - 1.0 };
            let (off, edge) = if g.is_some_and(|g| g.newest) {
                let u = if s == 1 { 1.0 } else { k as f32 / (s - 1) as f32 };
                (hi * u, 1.0 - 0.45 * (1.0 - u))
            } else if g.is_some() {
                (if f >= 0.0 { hi * f } else { lo * -f }, 1.0 - 0.45 * f.abs())
            } else {
                (bow * f, 1.0 - 0.45 * f.abs())
            };
            out.push((lens(pa, pb, off, 16), edge));
        }
        out
    }

    /// When the drawing stops changing: 1.2 s after the newest arrival, once the last pluck
    /// (revisions included) settles, a revision's dash and flash end, the live strand is fully
    /// slack, and the level has settled after its newest entry, whichever is latest. A level
    /// held up keeps the live strand trembling past that: a caller that pushes a live level
    /// rebuilds the view as it goes.
    pub fn until(&self, words: &[LatticeWord]) -> f64 {
        let arrivals = words.iter().map(|w| w.at).fold(f64::NEG_INFINITY, f64::max) + 1.2;
        let plucks = self.plucks_with_revisions(words);
        let revised = words.iter().filter_map(|w| w.changed).map(|c| c + REVISION_DASH.max(REVISION_FLASH));
        let live = self.live.map(|l| l.since + LIVE_SLACK);
        let level = self.level.as_ref().and_then(|l| l.last().map(|at| at + 6.0 / l.rate.max(1e-3) as f64));
        plucks
            .iter()
            .map(|p| p.at + self.pluck_settle(p))
            .chain(revised)
            .chain(live)
            .chain(level)
            .fold(arrivals, f64::max)
    }

    /// Draws the lattice of `words` at `nodes` (same order) at `t`.
    pub fn draw(&self, words: &[LatticeWord], nodes: &[Anchor], t: f64, reduced: bool, p: &mut Painter) {
        let n = words.len().min(nodes.len());
        let shown: Vec<usize> = (0..n).filter(|&i| reduced || words[i].at <= t).collect();
        let Some(&newest) = shown.last() else { return };
        let last = (!reduced).then(|| words[newest].at);
        let tension = if reduced { 1.0 } else { self.live_tension(last, t) };
        let plucks = if reduced { Arc::from([]) } else { self.plucks_with_revisions(&words[..n]) };
        let swing = |bundle: usize| if reduced { 0.0 } else { self.offset_in(&plucks, bundle, t) };
        let node = |i: usize| Point::new(nodes[i].x, self.node_y(&nodes[i]));

        for w in shown.windows(2) {
            let (a, b) = (w[0], w[1]);
            if nodes[a].line != nodes[b].line {
                continue;
            }
            let grow = if reduced { 1.0 } else { ease(((t - words[b].at) / self.draw_on).clamp(0.0, 1.0)) };
            if grow <= 0.0 {
                continue;
            }
            let slack = if words[a].boundary == Boundary::Comma { self.comma_slack } else { 0.0 };
            let bow = (self.rest_bow(words[b].pause) + slack) * tension + swing(b);
            let width = self.width.0 + (self.width.1 - self.width.0) * words[b].loud.clamp(0.0, 1.0);
            let base = self.pitch_color(words[b].pitch);
            let color = if b == newest { mix(base, self.accent, 0.8) } else { base };
            let dashed = !reduced && self.dashed(&words[b], t);
            for (c, edge) in self.strands(&nodes[a], &nodes[b], bow, false) {
                let c = c.trim(0.0, grow * c.length());
                let style = StrokeStyle::solid(width * edge, color.fade(edge));
                if dashed {
                    for d in c.dash(&[3.0, 2.0], 0.0) {
                        p.stroke(&d, &style);
                    }
                } else {
                    p.stroke(&c, &style);
                }
            }
        }
        // Arcs that skip a word tie the bundles of a sentence into a lattice.
        for [a, b, c] in Self::skip_arcs(words, nodes, &shown) {
            let grow = if reduced { 1.0 } else { ease(((t - words[c].at) / (2.0 * self.draw_on)).clamp(0.0, 1.0)) };
            let bow = (self.rest_bow(words[c].pause) + self.rest_bow(words[b].pause)) * tension + swing(c) + swing(b);
            let arc = self.strands(&nodes[a], &nodes[c], bow, true).remove(0).0;
            let arc = arc.trim(0.0, grow * arc.length());
            p.stroke(&arc, &StrokeStyle::solid(0.6, self.color.fade(0.5)));
        }
        // A faint arc spans each finished sentence.
        for (a, b) in Self::sentences(words, nodes, newest + 1) {
            let grow = if reduced { 1.0 } else { ease(((t - words[b].at) / (3.0 * self.draw_on)).clamp(0.0, 1.0)) };
            let arc = self.strands(&nodes[a], &nodes[b], self.bow.1 * 1.6, true).remove(0).0;
            let arc = arc.trim(0.0, grow * arc.length());
            p.stroke(&arc, &StrokeStyle::solid(0.5, self.color.fade(0.3)));
        }
        // The live strand from the newest node to the caret.
        if let Some(live) = self.live.filter(|l| l.from_node <= newest) {
            let a = nodes[live.from_node];
            if live.caret_x > a.x + 1.0 {
                let b = Anchor { x: live.caret_x, width: 0.0, ..a };
                let tremble = if reduced { 0.0 } else { self.live_tremble_at(t) };
                let bow = self.live_bow(&live, t, reduced) + tremble;
                for (c, edge) in self.strands(&a, &b, bow, false) {
                    p.stroke(&c, &StrokeStyle::solid(self.width.0 * edge, self.accent.fade(0.6 * edge)));
                }
            }
        }
        // Nodes and their dashed drop lines.
        for &i in &shown {
            let at = node(i);
            let fresh = if reduced { 1.0 } else { ease(((t - words[i].at) / self.draw_on).clamp(0.0, 1.0)) };
            let c = if i == newest { self.accent } else { mix(self.color, Color::WHITE, 0.4) };
            p.shape(ShapeInstance::circle(at, self.node_radius * (0.6 + 0.4 * fresh), Paint::solid(c)));
            if words[i].boundary == Boundary::Sentence {
                let ring = crate::path::make::arc(at, self.node_radius * 2.0, 0.0, std::f32::consts::TAU);
                p.stroke(&ring, &StrokeStyle::solid(0.6, c.fade(fresh)));
            }
            if let Some(changed) = words[i].changed.filter(|_| !reduced) {
                let u = (t - changed) / REVISION_FLASH;
                if (0.0..1.0).contains(&u) {
                    let u = u as f32;
                    let ring = crate::path::make::arc(
                        at,
                        self.node_radius * (1.5 + 3.0 * ease(u as f64)),
                        0.0,
                        std::f32::consts::TAU,
                    );
                    p.stroke(&ring, &StrokeStyle::solid(1.0, self.accent.fade(1.0 - u)));
                }
            }
            let stem = Contour::polyline(&[at, Point::new(at.x, at.y + self.stem * fresh)], false);
            for dash in stem.dash(&[1.5, 2.0], 0.0) {
                p.stroke(&dash, &StrokeStyle::solid(0.6, self.color.fade(0.8)));
            }
        }
    }

    /// The lattice as an element `size` big over text laid out from its top left: a canvas
    /// for the strands and, with [`LatticeLook::label`], a label under each node.
    pub fn view<M>(
        &self,
        key: impl Into<Key>,
        size: Size,
        words: Arc<[LatticeWord]>,
        nodes: Arc<[Anchor]>,
        reduced: bool,
    ) -> Element<M> {
        let key: Key = key.into();
        let look = Arc::new(self.clone());
        let until = if reduced { 0.0 } else { self.until(&words) };
        let (w2, n2, l2) = (words.clone(), nodes.clone(), look.clone());
        let canvas = Element::canvas(format!("{}-strands", key.0), until, move |t, _: Rect, p: &mut Painter| {
            l2.draw(&w2, &n2, t, reduced, p)
        })
        .size(size.w, size.h)
        .layout(absolute(0.0, 0.0));
        let mut root = Element::layer(key.clone()).size(size.w, size.h).child(canvas);
        if let Some(style) = &self.label {
            for (i, (w, a)) in words.iter().zip(nodes.iter()).enumerate() {
                let ms = (w.pause * 1000.0).round() as i32;
                let text = if i == 0 { "+0".to_string() } else { format!("+{ms}") };
                let y = self.node_y(a) + self.stem + 1.0;
                root = root.child(
                    Element::label(format!("{}-t{i}", key.0), text)
                        .text_style(*style)
                        .color(self.label_color)
                        .layout(absolute(a.x - 2.0, y)),
                );
            }
        }
        root
    }
}

fn absolute(x: f32, y: f32) -> impl FnOnce(&mut taffy::Style) {
    move |s: &mut taffy::Style| {
        s.position = taffy::Position::Absolute;
        s.inset = taffy::Rect { left: length(x), top: length(y), right: auto(), bottom: auto() };
    }
}

/// A curve from `a` to `b` that bows `bow` pixels off the straight line at its middle
/// (positive: down), as a cubic with both handles a third of the way along.
fn lens(a: Point, b: Point, bow: f32, n: usize) -> Contour {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    // Unit normal pointing down for a left-to-right segment.
    let (nx, ny) = (-dy / len, dx / len);
    // A cubic with handles offset by h bows 0.75 h at its middle.
    let h = bow / 0.75;
    let c1 = Point::new(a.x + dx / 3.0 + nx * h, a.y + dy / 3.0 + ny * h);
    let c2 = Point::new(a.x + 2.0 * dx / 3.0 + nx * h, a.y + 2.0 * dy / 3.0 + ny * h);
    let pts: Vec<Point> = (0..=n)
        .map(|i| {
            let u = i as f32 / n as f32;
            let v = 1.0 - u;
            let (k0, k1, k2, k3) = (v * v * v, 3.0 * v * v * u, 3.0 * v * u * u, u * u * u);
            Point::new(k0 * a.x + k1 * c1.x + k2 * c2.x + k3 * b.x, k0 * a.y + k1 * c1.y + k2 * c2.y + k3 * b.y)
        })
        .collect();
    Contour::polyline(&pts, false)
}

fn ease(k: f64) -> f32 {
    (1.0 - (1.0 - k).powi(3)) as f32
}

fn mix(a: Color, b: Color, k: f32) -> Color {
    Color::rgba(a.r + (b.r - a.r) * k, a.g + (b.g - a.g) * k, a.b + (b.b - a.b) * k, a.a + (b.a - a.a) * k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::DisplayList;
    use crate::text::EstimateText;

    #[test]
    fn a_longer_pause_bows_further_up_to_the_most() {
        let l = LatticeLook::default();
        assert!(l.rest_bow(0.4) > l.rest_bow(0.2));
        assert_eq!(l.rest_bow(0.0), l.bow.0);
        assert_eq!(l.rest_bow(30.0), l.bow.1);
        let c = lens(Point::new(0.0, 0.0), Point::new(90.0, 0.0), 6.0, 16);
        let mid = c.resample(3)[1];
        assert!((mid.y - 6.0).abs() < 0.2, "{mid:?}");
    }

    #[test]
    fn an_arrival_pulls_taut_then_settles() {
        let l = LatticeLook::default();
        assert_eq!(l.tension(None, 3.0), 1.0);
        assert!((l.tension(Some(1.0), 1.0) - (1.0 - l.tighten)).abs() < 1e-5);
        assert!((l.tension(Some(1.0), 3.0) - 1.0).abs() < 1e-3);
        assert!(l.tension(Some(1.0), 0.5) == 1.0, "nothing before the arrival");
    }

    #[test]
    fn anchors_follow_the_words_and_their_lines() {
        let style = TextStyle::default();
        let layout = EstimateText::default();
        let words = ["the", "future", "feels"];
        let a = anchors(&layout, &words, &style, None);
        assert_eq!(a.len(), 3);
        assert_eq!(a[0].x, 0.0);
        assert!(a[1].x > a[0].x + a[0].width && a[2].x > a[1].x + a[1].width, "{a:?}");
        assert!(a.iter().all(|n| n.line == 0));
    }

    #[test]
    fn drawing_is_pure_in_time_and_hides_words_not_yet_arrived() {
        let l = LatticeLook::default();
        let words: Vec<LatticeWord> = (0..3).map(|i| LatticeWord::new(0.3, 0.5, i as f64)).collect();
        let nodes: Vec<Anchor> =
            (0..3).map(|i| Anchor { x: 60.0 * i as f32, width: 40.0, baseline: 20.0, line: 0 }).collect();
        let count = |t: f64| {
            let mut scene: crate::Scene<()> = crate::Scene::new();
            scene.set_viewport(0.0, Size::new(300.0, 80.0), 1.0);
            let view = l.view("lat", Size::new(300.0, 80.0), words.clone().into(), nodes.clone().into(), false);
            scene.apply(Element::column("root").child(view), 0.0);
            let list = scene.sample(t);
            list.shapes.len()
        };
        assert_eq!(count(1.5), count(1.5));
        assert!(count(0.5) < count(2.5), "a node per arrived word");
    }

    #[test]
    fn newest_line_strands_stay_below_the_glyphs_and_in_the_gutter() {
        let (baseline, descent) = (20.0, 6.0);
        let gutters = vec![LineGutter { top: baseline + descent, height: 14.0, newest: true, descent }];
        let l = LatticeLook { gutters, ..LatticeLook::default() };
        let a = Anchor { x: 0.0, width: 40.0, baseline, line: 0 };
        let b = Anchor { x: 60.0, width: 40.0, baseline, line: 0 };
        for ms in (0..=30).map(|i| i as f32 * 0.1) {
            for tension in [0.4, 1.0, 1.6] {
                let bow = l.rest_bow(ms) * tension;
                for skip in [false, true] {
                    for (c, _) in l.strands(&a, &b, bow * if skip { 2.0 } else { 1.0 }, skip) {
                        for pt in c.resample(24) {
                            assert!(pt.y >= baseline + descent - 1e-3, "{ms} {pt:?}");
                            assert!(pt.y <= 20.0 + 6.0 + 14.0 + 1e-3, "{ms} {pt:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn older_line_bow_is_clamped_and_thin_gutters_use_one_strand() {
        let gutters = vec![LineGutter { top: 26.0, height: 8.0, newest: false, descent: 6.0 }];
        let mut l = LatticeLook { gutters, ..LatticeLook::default() };
        let a = Anchor { x: 0.0, width: 40.0, baseline: 20.0, line: 0 };
        let b = Anchor { x: 60.0, width: 40.0, baseline: 20.0, line: 0 };
        for bow in [0.0, 5.0, 40.0] {
            for skip in [false, true] {
                for (c, _) in l.strands(&a, &b, bow, skip) {
                    for pt in c.resample(24) {
                        assert!(pt.y >= 26.0 - 1e-3 && pt.y <= 34.0 + 1e-3, "{pt:?}");
                    }
                }
            }
        }
        assert_eq!(l.strands(&a, &b, 5.0, false).len(), 4);
        l.gutters[0].height = 5.0;
        assert_eq!(l.strands(&a, &b, 5.0, false).len(), 1);
    }

    fn plucked() -> (LatticeLook, Vec<LatticeWord>, Vec<Anchor>) {
        let words: Vec<LatticeWord> = (0..5).map(|i| LatticeWord::new(0.3, 0.5, 0.2 * i as f64)).collect();
        let nodes: Vec<Anchor> =
            (0..5).map(|i| Anchor { x: 60.0 * i as f32, width: 40.0, baseline: 20.0, line: 0 }).collect();
        let plucks: Vec<Pluck> = vec![
            Pluck { at: 0.4, bundle: 2, amp: 6.0 },
            Pluck { at: 0.6, bundle: 3, amp: 3.0 },
            Pluck { at: 0.9, bundle: 1, amp: 8.0 },
            Pluck { at: 1.1, bundle: 4, amp: 5.0 },
            Pluck { at: 1.3, bundle: 2, amp: 12.0 },
        ];
        let mut level = LevelTrack::new(12.0);
        level.push(0.0, 0.0);
        level.push(0.5, 0.9);
        level.push(1.0, 0.02);
        let look = LatticeLook { plucks: plucks.into(), level: Some(level), ..LatticeLook::default() };
        (look, words, nodes)
    }

    fn draw_list(l: &LatticeLook, words: &[LatticeWord], nodes: &[Anchor], t: f64, reduced: bool) -> DisplayList {
        let mut list = DisplayList::default();
        let mut items = Vec::new();
        let mut p = Painter { list: &mut list, items: &mut items, alpha: 1.0, layer: 0 };
        l.draw(words, nodes, t, reduced, &mut p);
        list
    }

    #[test]
    fn plucked_drawing_is_pure_in_time() {
        let (l, words, nodes) = plucked();
        for (t1, t2) in [(0.45, 1.4), (0.95, 3.0), (1.32, 1.33)] {
            let fresh = draw_list(&l, &words, &nodes, t1, false);
            let _ = draw_list(&l, &words, &nodes, t2, false);
            assert_eq!(draw_list(&l, &words, &nodes, t1, false), fresh, "{t1} after {t2}");
        }
        assert_ne!(draw_list(&l, &words, &nodes, 1.35, false), draw_list(&l, &words, &nodes, 1.4, false));
    }

    #[test]
    fn plucks_settle_under_the_still_swing_within_the_settle_time() {
        let (l, words, _) = plucked();
        let last = l.plucks.last().unwrap().at;
        for amp in [0.01, 1.0, 12.0, 400.0] {
            let one = LatticeLook { plucks: Arc::from([Pluck { at: 0.0, bundle: 3, amp }]), ..l.clone() };
            assert!(one.pluck_settle(&one.plucks[0]) <= PLUCK_SETTLE);
            for b in 0..12 {
                assert!(one.pluck_offset(b, PLUCK_SETTLE).abs() < PLUCK_STILL, "{amp} {b}");
            }
        }
        for b in 0..8 {
            for k in 0..50 {
                let t = last + PLUCK_SETTLE + 0.1 * k as f64;
                assert!(l.pluck_offset(b, t).abs() < PLUCK_STILL, "{b} {t}");
            }
        }
        let until = l.until(&words);
        assert!(until <= last + PLUCK_SETTLE + 1e-9 && until >= words[4].at + 1.2, "{until}");
        assert!(l.pluck_offset(2, last + 0.02).abs() > 0.5, "a fresh pluck rings");
    }

    #[test]
    fn a_pluck_reaches_its_neighbours_later_and_weaker() {
        let l = LatticeLook { plucks: Arc::from([Pluck { at: 1.0, bundle: 3, amp: 6.0 }]), ..LatticeLook::default() };
        // The first time a bundle moves, and its biggest swing.
        let probe = |b: usize| {
            let mut first = None;
            let mut peak = 0.0f32;
            for k in 0..2000 {
                let t = 1.0 + k as f64 * 0.001;
                let y = l.pluck_offset(b, t).abs();
                if y > 1e-4 && first.is_none() {
                    first = Some(t);
                }
                peak = peak.max(y);
            }
            (first.unwrap(), peak)
        };
        let (t3, p3) = probe(3);
        for b in [2, 4] {
            let (tb, pb) = probe(b);
            assert!(tb > t3 + 0.03, "bundle {b} starts later: {tb} vs {t3}");
            assert!(pb < p3 * 0.6, "bundle {b} swings less: {pb} vs {p3}");
        }
        assert_eq!(l.pluck_offset(3, 0.99), 0.0, "nothing before the pluck");
    }

    #[test]
    fn only_the_last_four_plucks_ring() {
        let p = |at: f64| Pluck { at, bundle: 0, amp: 5.0 };
        let five =
            LatticeLook { plucks: Arc::from([p(0.0), p(0.01), p(0.02), p(0.03), p(0.04)]), ..LatticeLook::default() };
        let four = LatticeLook { plucks: Arc::from([p(0.01), p(0.02), p(0.03), p(0.04)]), ..LatticeLook::default() };
        assert_eq!(five.pluck_offset(0, 0.1), four.pluck_offset(0, 0.1));
    }

    #[test]
    fn the_level_pulls_taut_only_over_its_floor() {
        let (l, _, _) = plucked();
        assert_eq!(l.live_tension(Some(0.0), 0.0), 1.0, "quiet start is slack");
        assert!(l.live_tension(None, 0.95) < 0.6, "loud is taut");
        assert_eq!(l.live_tension(None, 3.0), 1.0, "under the floor hangs at rest");
        let none = LatticeLook::default();
        assert_eq!(none.live_tension(Some(1.0), 1.1), none.tension(Some(1.0), 1.1));
    }

    #[test]
    fn reduced_motion_is_still_with_plucks_and_level() {
        let (l, words, nodes) = plucked();
        for t in [0.0, 0.5, 1.3, 2.0] {
            assert_eq!(draw_list(&l, &words, &nodes, t, true), draw_list(&l, &words, &nodes, t + 1.0, true), "{t}");
        }
    }

    #[test]
    fn without_plucks_or_level_the_drawing_is_unchanged() {
        let (l, words, _) = plucked();
        let plain = LatticeLook { plucks: Arc::from([]), level: None, ..l.clone() };
        let base = LatticeLook::default();
        assert_eq!(plain, base);
        assert_eq!(base.until(&words), words[4].at + 1.2);
        for t in [0.3, 0.9, 1.5] {
            assert_eq!(base.pluck_offset(2, t), 0.0);
            assert_eq!(base.live_tension(Some(0.8), t), base.tension(Some(0.8), t));
        }
    }

    fn sentences_words() -> (Vec<LatticeWord>, Vec<Anchor>) {
        let b = [
            Boundary::None,
            Boundary::Comma,
            Boundary::Sentence,
            Boundary::None,
            Boundary::None,
            Boundary::Sentence,
            Boundary::None,
        ];
        let words: Vec<LatticeWord> = b
            .iter()
            .enumerate()
            .map(|(i, &boundary)| LatticeWord { boundary, ..LatticeWord::new(0.3, 0.5, 0.2 * i as f64) })
            .collect();
        let nodes: Vec<Anchor> =
            (0..b.len()).map(|i| Anchor { x: 50.0 * i as f32, width: 40.0, baseline: 20.0, line: 0 }).collect();
        (words, nodes)
    }

    #[test]
    fn no_arc_joins_nodes_across_a_sentence_end() {
        let (words, nodes) = sentences_words();
        let shown: Vec<usize> = (0..words.len()).collect();
        let arcs = LatticeLook::skip_arcs(&words, &nodes, &shown);
        assert!(!arcs.is_empty());
        for [a, _, c] in &arcs {
            assert!((*a..*c).all(|i| words[i].boundary != Boundary::Sentence), "{a}..{c}");
        }
        assert_eq!(arcs, vec![[0, 1, 2], [3, 4, 5]]);
        assert_eq!(LatticeLook::sentences(&words, &nodes, words.len()), vec![(0, 2), (3, 5)]);
        assert_eq!(LatticeLook::sentences(&words, &nodes, 4), vec![(0, 2)], "an unfinished sentence has no arc");
    }

    #[test]
    fn sentence_ends_and_commas_change_the_drawing() {
        let (words, nodes) = sentences_words();
        let l = LatticeLook::default();
        let plain: Vec<LatticeWord> =
            words.iter().map(|w| LatticeWord { boundary: Boundary::None, ..w.clone() }).collect();
        let a = draw_list(&l, &words, &nodes, 3.0, false);
        let b = draw_list(&l, &plain, &nodes, 3.0, false);
        assert_ne!(a, b);
    }

    #[test]
    fn pitch_ends_are_exactly_color_and_accent() {
        let l = LatticeLook { color: Color::rgba(0.13, 0.37, 0.71, 0.55), ..LatticeLook::default() };
        assert_eq!(l.pitch_color(Some(0.0)), l.color);
        assert_eq!(l.pitch_color(Some(1.0)), l.accent);
        assert_eq!(l.pitch_color(None), l.color);
        assert_eq!(l.pitch_color(Some(-2.0)), l.color);
        assert_eq!(l.pitch_color(Some(9.0)), l.accent);
        let mid = l.pitch_color(Some(0.5));
        assert!(mid != l.color && mid != l.accent);
    }

    #[test]
    fn the_live_strand_slackens_over_silence_and_freezes_under_reduced_motion() {
        let live = LiveStrand { from_node: 0, caret_x: 120.0, since: 2.0 };
        let l = LatticeLook { live: Some(live), ..LatticeLook::default() };
        let mut prev = l.live_bow(&live, 2.0, false);
        assert_eq!(prev, l.bow.0);
        for k in 1..=300 {
            let b = l.live_bow(&live, 2.0 + k as f64 * 0.01, false);
            assert!(b >= prev, "{k}: {b} < {prev}");
            prev = b;
        }
        assert_eq!(prev, l.bow.1);
        assert!(l.live_bow(&live, 3.5, false) > l.live_bow(&live, 2.5, false));
        let frozen = l.live_bow(&live, 2.0, true);
        for t in [0.0, 2.5, 4.0, 9.0] {
            assert_eq!(l.live_bow(&live, t, true), frozen);
        }
        let words = vec![LatticeWord::new(0.0, 0.5, 0.0)];
        let nodes = vec![Anchor { x: 0.0, width: 40.0, baseline: 20.0, line: 0 }];
        let mut level = LevelTrack::new(12.0);
        level.push(0.0, 0.9);
        let shaking = LatticeLook { level: Some(level), ..l.clone() };
        assert!(
            (0..20).any(|k| shaking.live_tremble_at(1.0 + k as f64 * 0.013).abs() > 0.3),
            "trembles with the level"
        );
        assert_eq!(draw_list(&shaking, &words, &nodes, 2.5, true), draw_list(&shaking, &words, &nodes, 4.0, true));
        assert_ne!(draw_list(&shaking, &words, &nodes, 2.5, false), draw_list(&shaking, &words, &nodes, 4.0, false));
        assert!(shaking.until(&words) >= live.since + LIVE_SLACK);
    }

    #[test]
    fn a_revision_dashes_until_half_a_second_and_plucks_again() {
        let mut words: Vec<LatticeWord> = (0..3).map(|i| LatticeWord::new(0.3, 0.5, 0.2 * i as f64)).collect();
        words[2].changed = Some(2.0);
        let l = LatticeLook::default();
        assert!(!l.dashed(&words[2], 1.99));
        assert!(l.dashed(&words[2], 2.0));
        assert!(l.dashed(&words[2], 2.0 + REVISION_DASH - 1e-6));
        assert!(!l.dashed(&words[2], 2.0 + REVISION_DASH));
        assert!(!l.dashed(&words[1], 2.2));
        let plucks = l.plucks_with_revisions(&words);
        assert_eq!(plucks.len(), 1);
        assert_eq!(plucks[0], Pluck { at: 2.0, bundle: 2, amp: l.revision_amp });
        assert!(l.until(&words) >= 2.0 + REVISION_DASH);
        let nodes: Vec<Anchor> =
            (0..3).map(|i| Anchor { x: 60.0 * i as f32, width: 40.0, baseline: 20.0, line: 0 }).collect();
        let mut plain = words.clone();
        plain[2].changed = None;
        assert_ne!(draw_list(&l, &words, &nodes, 2.1, false), draw_list(&l, &plain, &nodes, 2.1, false));
        assert!(l.offset_in(&plucks, 2, 2.0 + PLUCK_SETTLE).abs() < PLUCK_STILL, "the re-pluck settles");
    }

    #[test]
    fn level_track_last_lets_until_see_a_moving_level() {
        let mut level = LevelTrack::new(12.0);
        assert_eq!(level.last(), None);
        level.push(0.0, 0.2);
        level.push(9.0, 0.8);
        assert_eq!(level.last(), Some(9.0));
        let words = vec![LatticeWord::new(0.0, 0.5, 0.0)];
        let l = LatticeLook { level: Some(level), ..LatticeLook::default() };
        assert!(l.until(&words) > 9.0);
    }
}
