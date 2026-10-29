//! Checks that keep motion honest (MO-08): the reading-line envelope, frame by frame, and the
//! GPU budget of a look.
//!
//! The envelope (motion plan, section 2, rule 1) holds on the newest line of live text: the
//! glyphs of the newest word and every glyph that comes to rest on the same row.
//!
//! - baseline shift at most 3 px from where the glyph comes to rest, and no glyph off its row
//!   (more than half an em away);
//! - tracking change at most 5 %: a word's width from its first to its last glyph against
//!   its width at rest;
//! - ink at least 70 % of the glyph's ink at rest, from the first frame it is drawn, so text
//!   is readable in the frame it arrives;
//! - no scale, rotation or blur beyond what the glyph has at rest.
//!
//! [`Tracker`] does the work on display lists. It follows each live-text glyph (one with a
//! word id) from frame to frame, finds where it comes to rest, and compares every frame
//! with that rest. A glyph that disappears without coming to rest again is leaving (a
//! correction's ghost, an exit) and is not checked; [`Report::leaving`] counts those frames.
//! Feed it lists yourself with [`Tracker::push`], or run a whole scenario with
//! [`check_scenario`], which needs no GPU.
//!
//! [`measure_budget`] renders a scenario offline with GPU timestamps and returns per-frame and
//! per-section times ([`Budget`]).
//!
//! Lengths are logical pixels (layout units), the same at any surface scale.

use std::collections::HashMap;
use std::fmt;

use vo_scene::display::{DisplayList, ItemKind, NO_WORD};
use vo_scene::geom::Point;

use crate::app::App;
use crate::offline::{self, OfflineError, Scenario};
use crate::Options;

/// The limits. The defaults are the motion plan's rule 1.
#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    /// Largest baseline shift from rest, in logical pixels.
    pub baseline_px: f32,
    /// Largest change of a word's width against rest, as a fraction (0.05 is 5 %).
    pub tracking: f32,
    /// Smallest ink against rest, as a fraction.
    pub ink: f32,
    /// Largest change of the em size against rest, as a fraction. Scale is not allowed; this
    /// only absorbs rounding.
    pub scale: f32,
    /// Largest turn against rest, in degrees. A glyph's own turn (`GlyphXform::rot_deg`)
    /// counts in full, also at rest: a letter left tilted on the reading line is a breach.
    pub rotation_deg: f32,
    /// Largest lean of a glyph (`GlyphXform::skew_deg`) either way, in degrees. The default is
    /// the reading range of the slant axis (`READING_SLNT`, 0 to 5 degrees).
    pub skew_deg: f32,
    /// Largest blur beyond rest, in logical pixels (glyph blur plus layer motion blur).
    pub blur_px: f32,
    /// Only frames sampled in `from..=until` are checked; the rest still count for finding
    /// where glyphs come to rest. Use `until` to leave out an ending that removes the text.
    pub from: f64,
    pub until: f64,
    /// A glyph is at rest from a frame on when it and the next `still_frames - 1` frames
    /// differ by less than `still_px` in position and size and 0.5 % in ink.
    pub still_frames: usize,
    pub still_px: f32,
    /// The word ids of the transcript. Other live text, such as a status row whose words
    /// carry made-up ids, never sets the reading line. All ids by default.
    pub transcript: std::ops::Range<u32>,
    /// A word that first shows within this many seconds after a frame is already the newest
    /// word in that frame. A look that holds a new word back while the line before scrolls
    /// out of its row (MO-33) has its reading line where that word will be, not on the row
    /// that is moving up. The default covers holds up to about 0.2 s (the flow's 120 ms drop hold,
    /// karaoke's 160 ms line wait), their fade in and a frame at 30 fps.
    pub pending: f64,
}

impl Default for Envelope {
    fn default() -> Envelope {
        Envelope {
            baseline_px: 3.0,
            tracking: 0.05,
            ink: 0.7,
            scale: 0.01,
            rotation_deg: 0.5,
            skew_deg: -vo_scene::font::var_axes::READING_SLNT.0,
            blur_px: 0.1,
            from: f64::NEG_INFINITY,
            until: f64::INFINITY,
            still_frames: 3,
            still_px: 0.05,
            transcript: 0..u32::MAX,
            pending: 0.25,
        }
    }
}

/// What a glyph on the reading line did wrong, with the measured value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Breach {
    /// Pixels from its rest baseline (more than the limit, less than half an em).
    Baseline(f32),
    /// Pixels from its rest baseline, half an em or more: the glyph is off its row.
    OffRow(f32),
    /// The word's width over its width at rest.
    Tracking(f32),
    /// Ink over ink at rest.
    Ink(f32),
    /// Em size over em size at rest.
    Scale(f32),
    /// Degrees turned against rest.
    Rotation(f32),
    /// Degrees of lean.
    Skew(f32),
    /// Pixels of blur beyond rest.
    Blur(f32),
}

impl Breach {
    pub fn kind(&self) -> &'static str {
        match self {
            Breach::Baseline(_) => "baseline",
            Breach::OffRow(_) => "off-row",
            Breach::Tracking(_) => "tracking",
            Breach::Ink(_) => "ink",
            Breach::Scale(_) => "scale",
            Breach::Rotation(_) => "rotation",
            Breach::Skew(_) => "skew",
            Breach::Blur(_) => "blur",
        }
    }

    pub fn value(&self) -> f32 {
        match *self {
            Breach::Baseline(v)
            | Breach::OffRow(v)
            | Breach::Tracking(v)
            | Breach::Ink(v)
            | Breach::Scale(v)
            | Breach::Rotation(v)
            | Breach::Skew(v)
            | Breach::Blur(v) => v,
        }
    }

    /// How far past rest the value is, for picking the worst one of a kind.
    fn badness(&self) -> f32 {
        match *self {
            Breach::Baseline(v) | Breach::OffRow(v) | Breach::Rotation(v) | Breach::Blur(v) => v,
            Breach::Skew(v) => v.abs(),
            Breach::Tracking(v) | Breach::Scale(v) => (v - 1.0).abs(),
            Breach::Ink(v) => 1.0 - v,
        }
    }
}

/// One breach in one frame. Tracking breaches are per word; the others per glyph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Violation {
    pub frame: u64,
    pub t: f64,
    /// The `vo-core` word id.
    pub word: u32,
    pub breach: Breach,
}

/// The result of a check.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// Frames pushed.
    pub frames: u64,
    /// Frames inside the window that had a reading line.
    pub frames_checked: u64,
    /// Glyph-frames checked on the reading line.
    pub glyphs_checked: u64,
    /// Glyph-frames of live text that never came to rest again (leaving), not checked.
    pub leaving: u64,
    pub violations: Vec<Violation>,
}

impl Report {
    pub fn is_ok(&self) -> bool {
        self.violations.is_empty()
    }

    /// Per kind of breach: how many, how many words, the worst one.
    pub fn by_kind(&self) -> Vec<KindSummary> {
        let mut out: Vec<KindSummary> = Vec::new();
        for v in &self.violations {
            let kind = v.breach.kind();
            let i = match out.iter().position(|s| s.kind == kind) {
                Some(i) => i,
                None => {
                    out.push(KindSummary { kind, count: 0, words: Vec::new(), worst: *v });
                    out.len() - 1
                }
            };
            let s = &mut out[i];
            s.count += 1;
            if !s.words.contains(&v.word) {
                s.words.push(v.word);
            }
            if v.breach.badness() > s.worst.breach.badness() {
                s.worst = *v;
            }
        }
        out
    }

    /// Panics with the summary when anything breached.
    pub fn assert_ok(&self) {
        assert!(self.is_ok(), "reading-line envelope breached:\n{self}");
    }
}

/// One kind of breach in a [`Report`].
#[derive(Clone, Debug, PartialEq)]
pub struct KindSummary {
    pub kind: &'static str,
    /// Violations of this kind.
    pub count: usize,
    /// The words that had one, in the order first seen.
    pub words: Vec<u32>,
    pub worst: Violation,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{} frames, {} checked, {} glyph-frames on the reading line, {} leaving",
            self.frames, self.frames_checked, self.glyphs_checked, self.leaving
        )?;
        if self.is_ok() {
            return writeln!(f, "envelope: ok");
        }
        for s in self.by_kind() {
            let w = s.worst;
            let worst = match w.breach {
                Breach::Baseline(v) | Breach::OffRow(v) => format!("{v:.2} px"),
                Breach::Blur(v) => format!("{v:.2} px blur"),
                Breach::Rotation(v) | Breach::Skew(v) => format!("{v:.2} deg"),
                Breach::Tracking(v) | Breach::Ink(v) | Breach::Scale(v) => format!("{:.0} %", v * 100.0),
            };
            writeln!(
                f,
                "{:9} {:5} times, {:3} words, worst {} (word {} at {:.3} s, frame {})",
                s.kind,
                s.count,
                s.words.len(),
                worst,
                w.word,
                w.t,
                w.frame
            )?;
        }
        Ok(())
    }
}

/// A live-text glyph as drawn, in surface space.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    /// Pen position on the baseline.
    pos: Point,
    /// Em size after every transform.
    em: f32,
    /// Degrees of the glyph's x axis, from the layers.
    rot: f32,
    /// The glyph's own turn and lean in degrees (EG-15).
    tilt: f32,
    skew: f32,
    /// Alpha after layer opacity, dissolve and wipe.
    ink: f32,
    blur: f32,
}

impl Sample {
    fn still(&self, next: &Sample, px: f32) -> bool {
        (self.pos.x - next.pos.x).abs() <= px
            && (self.pos.y - next.pos.y).abs() <= px
            && (self.em - next.em).abs() <= px
            && (self.ink - next.ink).abs() <= 0.005
            && (self.rot - next.rot).abs() <= 0.05
            && (self.blur - next.blur).abs() <= px
    }
}

/// `(word, font, glyph)`.
type Key = (u32, u32, u32);

struct Track {
    key: Key,
    first: usize,
    samples: Vec<Sample>,
}

impl Track {
    fn last_frame(&self) -> usize {
        self.first + self.samples.len() - 1
    }
}

struct FrameRec {
    t: f64,
    /// Tracks drawn in this frame, by index.
    tracks: Vec<usize>,
}

/// Follows live-text glyphs through frames and checks them against an [`Envelope`].
#[derive(Default)]
pub struct Tracker {
    frames: Vec<FrameRec>,
    tracks: Vec<Track>,
    /// Tracks drawn in the last frame, by key.
    open: HashMap<Key, Vec<usize>>,
}

impl Tracker {
    pub fn new() -> Tracker {
        Tracker::default()
    }

    /// Records one frame sampled at `t`. Frames must come in time order, one per sample.
    pub fn push(&mut self, t: f64, list: &DisplayList) {
        let n = self.frames.len();
        let mut now: Vec<(Key, Sample)> = Vec::new();
        collect(list, &mut now);
        let mut open: HashMap<Key, Vec<usize>> = HashMap::new();
        let mut drawn = Vec::with_capacity(now.len());
        for (key, s) in now {
            // The nearest glyph with the same key in the last frame, within an em and a half.
            let best = self.open.get(&key).and_then(|cands| {
                cands
                    .iter()
                    .copied()
                    .filter(|&i| self.tracks[i].last_frame() + 1 == n)
                    .map(|i| {
                        let p = self.tracks[i].samples.last().unwrap();
                        (i, (p.pos.x - s.pos.x).hypot(p.pos.y - s.pos.y), p.em)
                    })
                    .filter(|&(_, d, em)| d <= 1.5 * em.max(s.em))
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(i, _, _)| i)
            });
            let i = match best {
                Some(i) => {
                    self.tracks[i].samples.push(s);
                    i
                }
                None => {
                    self.tracks.push(Track { key, first: n, samples: vec![s] });
                    self.tracks.len() - 1
                }
            };
            open.entry(key).or_default().push(i);
            drawn.push(i);
        }
        self.open = open;
        self.frames.push(FrameRec { t, tracks: drawn });
    }

    /// Checks every recorded frame.
    pub fn check(&self, env: &Envelope) -> Report {
        let mut report = Report { frames: self.frames.len() as u64, ..Report::default() };
        let last_frame = self.frames.len().saturating_sub(1);
        // Per track, per sample: the sample index where it next comes to rest.
        let rests: Vec<Vec<Option<usize>>> = self.tracks.iter().map(|tr| rests_of(tr, env, last_frame)).collect();
        // The same with the scroll of the glyph's row ([`Tracker::row_scroll`]) taken out, so
        // a word that scrolls with its line rests on that line. Text that scrolls a line every
        // few frames is never still long enough to rest where it is drawn.
        let scroll = self.row_scroll(env);
        let level: Vec<Track> = self
            .tracks
            .iter()
            .zip(&scroll)
            .map(|(tr, sc)| Track {
                key: tr.key,
                first: tr.first,
                samples: tr
                    .samples
                    .iter()
                    .zip(sc)
                    .map(|(s, d)| Sample { pos: Point::new(s.pos.x, s.pos.y - d), ..*s })
                    .collect(),
            })
            .collect();
        let level_rests: Vec<Vec<Option<usize>>> = level.iter().map(|tr| rests_of(tr, env, last_frame)).collect();
        // Where sample `k` of track `ti` rests on its row, carried along by the row's scroll to
        // that sample's frame.
        let on_row = |ti: usize, k: usize| {
            level_rests[ti][k].map(|r| {
                let mut at = level[ti].samples[r];
                at.pos.y += scroll[ti][k];
                at
            })
        };

        for (n, fr) in self.frames.iter().enumerate() {
            // (track, sample, rest sample)
            let mut glyphs: Vec<(usize, &Sample, Sample)> = Vec::new();
            for &ti in &fr.tracks {
                let tr = &self.tracks[ti];
                let k = n - tr.first;
                match rests[ti][k] {
                    Some(r) => glyphs.push((ti, &tr.samples[k], tr.samples[r])),
                    None => report.leaving += 1,
                }
            }
            glyphs.retain(|g| env.transcript.contains(&self.tracks[g.0].key.0));
            // Words drawn now, and words about to show, with where each glyph comes to rest.
            let soon = self.frames[n + 1..].iter().enumerate().take_while(|(_, f)| f.t <= fr.t + env.pending);
            let pending = soon.flat_map(|(i, f)| {
                f.tracks.iter().filter(move |&&ti| self.tracks[ti].first == n + 1 + i).filter_map(|&ti| {
                    let tr = &self.tracks[ti];
                    let r = rests[ti][0]?;
                    env.transcript.contains(&tr.key.0).then(|| (tr.key.0, tr.samples[r].pos.y))
                })
            });
            let rests_now = glyphs.iter().map(|g| (self.tracks[g.0].key.0, g.2.pos.y));
            let all: Vec<(u32, f32)> = rests_now.chain(pending).collect();
            let Some(newest) = all.iter().map(|a| a.0).max() else {
                continue;
            };
            let mut ys: Vec<f32> = all.iter().filter(|a| a.0 == newest).map(|a| a.1).collect();
            ys.sort_by(f32::total_cmp);
            let line_y = ys[ys.len() / 2];
            let inside = fr.t >= env.from && fr.t <= env.until;
            if !inside {
                continue;
            }
            report.frames_checked += 1;
            let mut words: HashMap<u32, Vec<(&Sample, Sample)>> = HashMap::new();
            for &(ti, s, r) in &glyphs {
                if (r.pos.y - line_y).abs() >= 0.5 * r.em {
                    continue;
                }
                // A glyph whose rest is off its row from here, while its row scrolls on the way
                // there, is measured against its rest on the row instead. Nearer than that, the
                // scroll itself is still reported, as for text that rests between scrolls.
                let off = (s.pos.y - r.pos.y).abs();
                let r = match on_row(ti, n - self.tracks[ti].first) {
                    Some(o) if off >= 0.5 * r.em && (s.pos.y - o.pos.y).abs() < off => o,
                    _ => r,
                };
                let word = self.tracks[ti].key.0;
                report.glyphs_checked += 1;
                let mut flag = |breach| report.violations.push(Violation { frame: n as u64, t: fr.t, word, breach });
                let dy = (s.pos.y - r.pos.y).abs();
                if dy >= 0.5 * r.em {
                    flag(Breach::OffRow(dy));
                } else if dy > env.baseline_px + REST_SLACK_PX {
                    flag(Breach::Baseline(dy));
                }
                if r.ink > 1e-3 && s.ink < env.ink * r.ink {
                    flag(Breach::Ink(s.ink / r.ink));
                }
                if r.em > 0.0 && (s.em / r.em - 1.0).abs() > env.scale {
                    flag(Breach::Scale(s.em / r.em));
                }
                let turn = angle_diff(s.rot + s.tilt, r.rot);
                if turn > env.rotation_deg {
                    flag(Breach::Rotation(turn));
                }
                if s.skew.abs() > env.skew_deg {
                    flag(Breach::Skew(s.skew));
                }
                if s.blur - r.blur > env.blur_px {
                    flag(Breach::Blur(s.blur - r.blur));
                }
                words.entry(word).or_default().push((s, r));
            }
            let mut ids: Vec<u32> = words.keys().copied().collect();
            ids.sort_unstable();
            for word in ids {
                let gs = &words[&word];
                if gs.len() < 2 {
                    continue;
                }
                let span = |f: fn(&(&Sample, Sample)) -> f32| {
                    let (lo, hi) = gs.iter().map(f).fold((f32::MAX, f32::MIN), |(lo, hi), x| (lo.min(x), hi.max(x)));
                    hi - lo
                };
                let now = span(|g| g.0.pos.x);
                let rest = span(|g| g.1.pos.x);
                if rest > 0.5 * gs[0].1.em && (now / rest - 1.0).abs() > env.tracking {
                    report.violations.push(Violation {
                        frame: n as u64,
                        t: fr.t,
                        word,
                        breach: Breach::Tracking(now / rest),
                    });
                }
            }
        }
        report
    }

    /// Per track, per sample: how far the glyph's row has scrolled down since the track's first
    /// sample (negative is up). The row scrolls in a frame by a move that at least
    /// [`ROW_MATES`] glyphs on other rows share, within [`ROW_TOGETHER_PX`], and that the row
    /// shares too: as many glyphs of other words on it, or the glyph itself (a word whose only
    /// row-mate jumps still scrolls with the block). A glyph with no other word on its row (it
    /// left the row, or starts one) takes the block's move. Of several such moves, the one
    /// nearest the glyph's own (a word put in place while the block still eases up moves
    /// apart from the words that scroll). The glyph's own word never counts, so a word that
    /// leaves its row while the row holds still keeps all of its move, and so does a row that
    /// rises on its own (an arrival, not a scroll).
    fn row_scroll(&self, env: &Envelope) -> Vec<Vec<f32>> {
        let mut out: Vec<Vec<f32>> = self.tracks.iter().map(|tr| vec![0.0; tr.samples.len()]).collect();
        for n in 1..self.frames.len() {
            // Transcript glyphs drawn in this frame and the last: (track, word, height in the
            // last frame, move, em).
            let both: Vec<(usize, u32, f32, f32, f32)> = self.frames[n]
                .tracks
                .iter()
                .filter_map(|&ti| {
                    let tr = &self.tracks[ti];
                    if tr.first >= n || !env.transcript.contains(&tr.key.0) {
                        return None;
                    }
                    let (a, b) = (&tr.samples[n - tr.first - 1], &tr.samples[n - tr.first]);
                    Some((ti, tr.key.0, a.pos.y, b.pos.y - a.pos.y, a.em))
                })
                .collect();
            for &(ti, word, y, own, em) in &both {
                let near = |m: f32, row: bool| {
                    both.iter()
                        .filter(|o| {
                            o.1 != word && ((o.2 - y).abs() < 0.5 * em) == row && (o.3 - m).abs() <= ROW_TOGETHER_PX
                        })
                        .count()
                };
                let alone = !both.iter().any(|o| o.1 != word && (o.2 - y).abs() < 0.5 * em);
                let mut step = 0.0;
                let mut best = f32::MAX;
                // The moves of the other words, each once: glyphs that scroll together move alike.
                let mut moves: Vec<f32> = Vec::new();
                for o in both.iter().filter(|o| o.1 != word) {
                    if !moves.iter().any(|m| (m - o.3).abs() <= 0.01) {
                        moves.push(o.3);
                    }
                }
                for m in moves {
                    let off = (m - own).abs();
                    let row = near(m, true) >= ROW_MATES || off <= ROW_TOGETHER_PX || alone;
                    if off < best && row && near(m, false) >= ROW_MATES {
                        best = off;
                        step = m;
                    }
                }
                let k = n - self.tracks[ti].first;
                out[ti][k] = out[ti][k - 1] + step;
            }
        }
        out
    }
}

/// Glyphs of other words a row needs before its move counts as a scroll ([`Tracker::row_scroll`]).
const ROW_MATES: usize = 2;

/// How far a glyph's move can be from its row's and still move with the row, in logical pixels.
const ROW_TOGETHER_PX: f32 = 0.5;

/// Per sample, where the glyph comes to rest. That is the next still sample ([`rest_of`]),
/// unless the sample is part of a hold on one row (at least `still_frames` samples, each
/// within [`ROW_STILL_PX`] of the next in height) whose stillest sample is half an em or more
/// from that rest. A word still settling onto its row when the text scrolls a line, or on a
/// centred line that keeps gliding along its row until it rolls away, rests on that row
/// where it is stillest, not on the row the text takes it to.
fn rests_of(tr: &Track, env: &Envelope, last_frame: usize) -> Vec<Option<usize>> {
    let mut rests = rest_of(tr, env, last_frame);
    let len = tr.samples.len();
    // Only the height counts: a centred line glides along its row as words arrive.
    let holds = |j: usize| (tr.samples[j].pos.y - tr.samples[j + 1].pos.y).abs() <= ROW_STILL_PX;
    let mut i = 0;
    while i < len {
        let mut e = i;
        while e + 1 < len && holds(e) {
            e += 1;
        }
        if e + 1 - i >= env.still_frames.max(1) {
            // The stillest sample of the hold in height, the latest of equals: the word has
            // settled most there, before the scroll that ends the hold sets off.
            let speed = |j: usize| match j {
                _ if j == i || tr.samples[j].ink < REST_INK => f32::MAX,
                _ => (tr.samples[j].pos.y - tr.samples[j - 1].pos.y).abs(),
            };
            let calm = (i..=e).fold(i, |b, j| if speed(j) <= speed(b) + 0.01 { j } else { b });
            let end = &tr.samples[calm];
            for rest in &mut rests[i..=e] {
                if rest.is_some_and(|r| (tr.samples[r].pos.y - end.pos.y).abs() >= 0.5 * end.em) {
                    *rest = Some(calm);
                }
            }
        }
        i = e + 1;
    }
    rests
}

/// Vertical movement per frame under which a glyph holds its row, in logical pixels.
const ROW_STILL_PX: f32 = 1.5;

/// How far a measured rest can be from where the glyph would settle, in logical pixels. A
/// spring that overshoots creeps back by a few hundredths a frame, which counts as still, and
/// the text often moves on before it is done.
const REST_SLACK_PX: f32 = 0.1;

/// Ink under which a glyph is gone to the eye. The faded tail of an exit is still, but the
/// glyph is leaving, not resting there.
const REST_INK: f32 = 0.02;

/// For each sample of `tr`, where the glyph next comes to rest (the end of the next run of
/// still samples), or `None` when it disappears before resting again. A glyph never rests
/// below [`REST_INK`]. A glyph still drawn in the last frame rests at its last sample if
/// nowhere sooner, so a short scenario does not hide arrivals.
fn rest_of(tr: &Track, env: &Envelope, last_frame: usize) -> Vec<Option<usize>> {
    let len = tr.samples.len();
    let k = env.still_frames.max(1);
    let mut still = vec![false; len];
    for (i, flag) in still.iter_mut().enumerate() {
        *flag = i + k <= len
            && tr.samples[i].ink >= REST_INK
            && (i..i + k - 1).all(|j| tr.samples[j].still(&tr.samples[j + 1], env.still_px));
    }
    let mut out = vec![None; len];
    let mut next = (tr.last_frame() == last_frame && tr.samples[len - 1].ink >= REST_INK).then_some(len - 1);
    for i in (0..len).rev() {
        // A run of still samples rests at its end: a spring that overshoots is still for a
        // moment at its turn, and then creeps back to where it settles.
        if still[i] && !still.get(i + 1).copied().unwrap_or(false) {
            next = Some(i);
        }
        out[i] = next;
    }
    out
}

fn angle_diff(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// The live-text glyphs of `list`, in surface space.
fn collect(list: &DisplayList, out: &mut Vec<(Key, Sample)>) {
    // Drawn opacity and motion blur of each layer, down the parent chain.
    let mut opacity = vec![1.0f32; list.layers.len()];
    let mut blur = vec![0.0f32; list.layers.len()];
    for (i, l) in list.layers.iter().enumerate() {
        let (po, pb) = l.parent.map(|p| (opacity[p as usize], blur[p as usize])).unwrap_or((1.0, 0.0));
        opacity[i] = po * l.opacity;
        let t = &l.transform;
        let mb = (t.a * l.motion_blur.x + t.c * l.motion_blur.y).hypot(t.b * l.motion_blur.x + t.d * l.motion_blur.y);
        blur[i] = pb + mb;
    }
    for (li, l) in list.layers.iter().enumerate() {
        let map = |p: Point| {
            let q = l.transform.apply(p);
            match &l.warp {
                Some(w) => w.apply(q),
                None => q,
            }
        };
        for item in &list.items[l.items.start as usize..l.items.end as usize] {
            if item.kind != ItemKind::Glyphs {
                continue;
            }
            for gi in item.start..item.start + item.len {
                let g = &list.glyphs[gi as usize];
                if g.word == NO_WORD {
                    continue;
                }
                let fx = list
                    .glyph_fx
                    .binary_search_by_key(&gi, |(i, _)| *i)
                    .ok()
                    .map(|k| list.glyph_fx[k].1)
                    .unwrap_or_default();
                let ink =
                    g.color.a * opacity[li] * (1.0 - fx.dissolve.clamp(0.0, 1.0)) * (1.0 - fx.wipe.clamp(0.0, 1.0));
                if ink <= 1e-3 {
                    continue;
                }
                // The glyph's own move counts; its turn and lean are kept apart (`tilt`,
                // `skew`), so the axes below are the layers' only.
                let at = Point::new(g.pos.x + g.xform.offset.x, g.pos.y + g.xform.offset.y);
                let p = map(at);
                let px = map(Point::new(at.x + g.size, at.y));
                let py = map(Point::new(at.x, at.y + g.size));
                let (ax, ay) = (px.x - p.x, px.y - p.y);
                let em = ((ax.hypot(ay)) * ((py.x - p.x).hypot(py.y - p.y))).sqrt();
                let s = Sample {
                    pos: p,
                    em,
                    rot: ay.atan2(ax).to_degrees(),
                    tilt: g.xform.rot_deg,
                    skew: g.xform.skew_deg,
                    ink,
                    blur: fx.blur * em / g.size.max(1e-6) + blur[li],
                };
                out.push(((g.word, g.font, g.glyph), s));
            }
        }
    }
}

/// Runs `app` through `scenario` at `fps` on the CPU and hands each frame's display list to
/// `sink` with its frame number and time. Stops after `scenario.duration` or once the app
/// exits and settles. Returns the number of frames. Nothing is drawn, so no GPU is needed.
pub fn sample_scenario<A: App>(
    app: A,
    scenario: &Scenario<A::Msg>,
    fps: f64,
    mut options: Options<A::Msg>,
    sink: &mut dyn FnMut(u64, f64, &DisplayList),
) -> u64 {
    let mut driver = offline::start(app, scenario, &mut options);
    let mut next_input = 0;
    let mut list = DisplayList::default();
    let mut n = 0u64;
    loop {
        let ft = vo_core::time::FrameTime::at_rate(n, fps);
        if ft.t > scenario.duration {
            break;
        }
        offline::advance(&mut driver, &scenario.inputs, &mut next_input, ft.t);
        driver.scene.sample_into(ft.t, &mut list);
        sink(n, ft.t, &list);
        n += 1;
        if driver.done(ft.t) {
            break;
        }
    }
    n
}

/// Runs `scenario` on the CPU ([`sample_scenario`]) and checks the reading-line envelope.
pub fn check_scenario<A: App>(
    app: A,
    scenario: &Scenario<A::Msg>,
    fps: f64,
    options: Options<A::Msg>,
    env: &Envelope,
) -> Report {
    let mut tracker = Tracker::new();
    sample_scenario(app, scenario, fps, options, &mut |_, t, list| tracker.push(t, list));
    tracker.check(env)
}

/// GPU time of a scenario's main surface.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Budget {
    /// Physical size of the main surface.
    pub size: [u32; 2],
    /// Frames with timestamps.
    pub frames: u64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    /// Per section label (`target`, `scene`, `layer 3`, `layer 3 bloom`, `final`): its 95th
    /// percentile and largest time over the frames it appears in, slowest first.
    pub sections: Vec<SectionCost>,
}

/// See [`Budget::sections`].
#[derive(Clone, Debug, PartialEq)]
pub struct SectionCost {
    pub label: String,
    pub frames: u64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

impl Budget {
    /// `Err` with the reason when the 95th percentile of the whole frame is over `frame_ms`,
    /// or that of any one section is over `section_ms`. Percentiles, not maxima, because
    /// other work on the GPU makes single frames slow.
    pub fn within(&self, frame_ms: f64, section_ms: f64) -> Result<(), String> {
        let mut why = Vec::new();
        if self.p95_ms > frame_ms {
            why.push(format!("frame p95 {:.3} ms over {frame_ms} ms", self.p95_ms));
        }
        for s in self.sections.iter().filter(|s| s.p95_ms > section_ms) {
            why.push(format!("{} p95 {:.3} ms over {section_ms} ms", s.label, s.p95_ms));
        }
        if why.is_empty() {
            Ok(())
        } else {
            Err(why.join("; "))
        }
    }
}

impl fmt::Display for Budget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "GPU {}x{}, {} frames: mean {:.3} ms, p50 {:.3}, p95 {:.3}, max {:.3}",
            self.size[0], self.size[1], self.frames, self.mean_ms, self.p50_ms, self.p95_ms, self.max_ms
        )?;
        for s in self.sections.iter().take(6) {
            writeln!(f, "  {:16} p95 {:.3} ms, max {:.3} ms ({} frames)", s.label, s.p95_ms, s.max_ms, s.frames)?;
        }
        Ok(())
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// Renders `scenario` offline with GPU timestamps and measures the main surface. `Ok(None)`
/// when the adapter has no timestamps. Any `options.frame_timings` is replaced.
pub fn measure_budget<A: App>(
    app: A,
    scenario: &Scenario<A::Msg>,
    fps: f64,
    mut options: Options<A::Msg>,
) -> Result<Option<Budget>, OfflineError> {
    use std::sync::{Arc, Mutex};
    type Seen = (Vec<f64>, HashMap<String, Vec<f64>>, [u32; 2]);
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let sink = seen.clone();
    options.frame_timings = Some(Arc::new(move |f: &crate::FrameTiming| {
        if f.surface.is_some() {
            return;
        }
        let mut s = sink.lock().unwrap();
        s.0.push(f.timings.total_ms);
        for p in &f.timings.passes {
            s.1.entry(p.label.clone()).or_default().push(p.ms);
        }
        s.2 = f.size;
    }));
    offline::drive::<A, OfflineError>(
        app,
        scenario,
        fps,
        offline::Frames::UntilDone(scenario.duration),
        options,
        &mut |_| Ok(()),
    )?;
    let (mut totals, by_label, size) = std::mem::take(&mut *seen.lock().unwrap());
    if totals.is_empty() {
        return Ok(None);
    }
    let mean_ms = totals.iter().sum::<f64>() / totals.len() as f64;
    totals.sort_by(f64::total_cmp);
    let mut sections: Vec<SectionCost> = by_label
        .into_iter()
        .map(|(label, mut ms)| {
            ms.sort_by(f64::total_cmp);
            SectionCost { label, frames: ms.len() as u64, p95_ms: percentile(&ms, 0.95), max_ms: *ms.last().unwrap() }
        })
        .collect();
    sections.sort_by(|a, b| b.p95_ms.total_cmp(&a.p95_ms));
    Ok(Some(Budget {
        size,
        frames: totals.len() as u64,
        mean_ms,
        p50_ms: percentile(&totals, 0.5),
        p95_ms: percentile(&totals, 0.95),
        max_ms: *totals.last().unwrap(),
        sections,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vo_scene::display::{GlyphInstance, Item, Layer};
    use vo_scene::geom::Color;

    /// A list with one word of three glyphs at `(x, y)`, `gap` apart, with `alpha`.
    fn word(id: u32, x: f32, y: f32, gap: f32, alpha: f32) -> Vec<GlyphInstance> {
        (0..3)
            .map(|k| GlyphInstance {
                pos: Point::new(x + k as f32 * gap, y),
                size: 16.0,
                font: 0,
                glyph: 10 + k,
                word: id,
                color: Color::rgba(1.0, 1.0, 1.0, alpha),
                xform: vo_scene::display::GlyphXform::IDENTITY,
            })
            .collect()
    }

    fn list(glyphs: Vec<GlyphInstance>) -> DisplayList {
        let mut root = Layer::root();
        root.items = 0..1;
        DisplayList {
            layers: vec![root],
            items: vec![Item { kind: ItemKind::Glyphs, start: 0, len: glyphs.len() as u32 }],
            glyphs,
            ..DisplayList::default()
        }
    }

    /// Frames of word 1 resting, then word 2 arriving with `arrive(k)` giving its
    /// `(dy, alpha, gap)` in its k-th frame; it rests from frame 6 on.
    fn run(arrive: impl Fn(usize) -> (f32, f32, f32)) -> Report {
        let mut tr = Tracker::new();
        for n in 0..20 {
            let mut g = word(1, 10.0, 40.0, 9.0, 1.0);
            if n >= 4 {
                let k = n - 4;
                let (dy, a, gap) = if k < 6 { arrive(k) } else { (0.0, 1.0, 9.0) };
                g.extend(word(2, 50.0, 40.0 + dy, gap, a));
            }
            tr.push(n as f64 / 60.0, &list(g));
        }
        tr.check(&Envelope::default())
    }

    #[test]
    fn a_word_that_arrives_in_place_passes() {
        let r = run(|_| (0.0, 1.0, 9.0));
        r.assert_ok();
        assert!(r.glyphs_checked > 0);
    }

    #[test]
    fn a_small_rise_inside_the_envelope_passes() {
        run(|k| (2.5 - k as f32 * 0.5, 0.8 + k as f32 * 0.04, 9.0)).assert_ok();
    }

    #[test]
    fn a_rise_and_fade_breaks_baseline_and_ink() {
        let r = run(|k| (8.0 - k as f32 * 1.6, 0.1 + k as f32 * 0.15, 9.0));
        let kinds: Vec<_> = r.by_kind().iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&"baseline"), "{r}");
        assert!(kinds.contains(&"ink"), "{r}");
        assert!(r.violations.iter().all(|v| v.word == 2), "{r}");
        let ink = r.by_kind().into_iter().find(|s| s.kind == "ink").unwrap();
        assert!((ink.worst.breach.value() - 0.1).abs() < 1e-4);
        assert_eq!(ink.worst.frame, 4, "the frame it arrives in");
    }

    /// The line before scrolls up a row from frame 4 while the new word is held back, and
    /// the new word shows at its place in frame 4 + `hold`.
    fn held(hold: usize, env: &Envelope) -> Report {
        let mut tr = Tracker::new();
        for n in 0..20usize {
            let up = (n.saturating_sub(4).min(6) as f32) * 4.0;
            let mut g = word(1, 10.0, 64.0 - up, 9.0, 1.0);
            if n >= 4 + hold {
                g.extend(word(2, 10.0, 64.0, 9.0, 1.0));
            }
            tr.push(n as f64 / 60.0, &list(g));
        }
        tr.check(env)
    }

    #[test]
    fn a_held_word_keeps_the_reading_line_off_the_row_that_scrolls_up() {
        held(5, &Envelope::default()).assert_ok();
        let r = held(5, &Envelope { pending: 0.0, ..Envelope::default() });
        assert!(r.by_kind().iter().any(|s| s.kind == "off-row"), "without pending the scrolling row is checked: {r}");
    }

    #[test]
    fn a_word_still_settling_when_the_text_scrolls_rests_on_its_row() {
        // Word 2 lands a row below word 1, settles by a pixel a frame, and the block then
        // scrolls up a row before it is quite still.
        let run = |settle: f32| {
            let mut tr = Tracker::new();
            for n in 0..30usize {
                let up = (n.saturating_sub(10).min(6) as f32) * 4.0;
                let mut g = word(1, 10.0, 40.0 - up, 9.0, 1.0);
                if n >= 4 {
                    let s = (10usize.saturating_sub(n).min(4)) as f32 * settle;
                    g.extend(word(2, 10.0, 64.0 + s - up, 9.0, 1.0));
                }
                tr.push(n as f64 / 60.0, &list(g));
            }
            tr.check(&Envelope { pending: 0.0, ..Envelope::default() })
        };
        // Before the scroll (frame 10) the word is on its row. The scroll itself moves the
        // reading line, which this check still reports.
        let before =
            |r: &Report| r.violations.iter().filter(|v| v.frame < 10 && matches!(v.breach, Breach::OffRow(_))).count();
        let r = run(0.5);
        assert_eq!(before(&r), 0, "it was on its row: {r}");
        // A word that slides a whole row while it arrives is still off its row.
        let r = run(6.0);
        assert!(before(&r) > 0, "{r}");
    }

    /// Word 1 resting, then word 2 drawn from frame 4 on at `at(k)` = `(dy, alpha)` in its
    /// k-th frame, for `frames` frames in all.
    fn track(frames: usize, at: impl Fn(usize) -> (f32, f32)) -> Report {
        let mut tr = Tracker::new();
        for n in 0..frames {
            let mut g = word(1, 10.0, 40.0, 9.0, 1.0);
            if n >= 4 {
                let (dy, a) = at(n - 4);
                g.extend(word(2, 50.0, 40.0 + dy, 9.0, a));
            }
            tr.push(n as f64 / 60.0, &list(g));
        }
        tr.check(&Envelope::default())
    }

    /// Two rows of two words each, the block scrolling up a row (24 px) every 8 frames for
    /// five scrolls, so no glyph is ever still for three frames until it stops. Word 4, the
    /// newest, is drawn `own(n)` px below its place in frame `n`.
    fn scrolling(own: impl Fn(usize) -> f32) -> Report {
        scrolling_into(&mut Tracker::new(), own)
    }

    fn scrolling_into(tr: &mut Tracker, own: impl Fn(usize) -> f32) -> Report {
        // One scroll: fast, then a creep that never settles before the next one.
        const STEP: [f32; 8] = [9.0, 7.0, 4.0, 2.4, 1.0, 0.3, 0.2, 0.1];
        for n in 0..56usize {
            let up: f32 = (0..n.min(40)).map(|i| STEP[i % 8]).sum();
            let mut g = word(1, 10.0, 40.0 - up, 9.0, 1.0);
            g.extend(word(2, 50.0, 40.0 - up, 9.0, 1.0));
            g.extend(word(3, 10.0, 64.0 - up, 9.0, 1.0));
            g.extend(word(4, 50.0, 64.0 - up + own(n), 9.0, 1.0));
            tr.push(n as f64 / 60.0, &list(g));
        }
        tr.check(&Envelope::default())
    }

    #[test]
    fn a_line_that_scrolls_every_few_frames_rests_on_its_row() {
        // The last scroll ends where the text rests, and its own move is reported as for any
        // scroll; the four before it never come to rest and are clean.
        let mut tr = Tracker::new();
        let r = scrolling_into(&mut tr, |_| 0.0);
        let before_last = r.violations.iter().filter(|v| v.frame < 32).count();
        assert_eq!(before_last, 0, "{r}");
        assert!(!r.by_kind().iter().any(|s| s.kind == "off-row" || s.kind == "tracking"), "{r}");
        let env = Envelope { until: 31.5 / 60.0, ..Envelope::default() };
        let r = tr.check(&env);
        r.assert_ok();
        assert!(r.glyphs_checked > 0);
    }

    #[test]
    fn a_word_that_leaves_its_scrolling_row_is_still_off_its_row() {
        // Sinks 4 px a frame to 16 px below its row during the second scroll and comes back.
        let away = |n: usize| (4.0 * (4.0 - (n as f32 - 13.0).abs())).max(0.0);
        let r = scrolling(away);
        assert!(r.violations.iter().any(|v| v.word == 4 && matches!(v.breach, Breach::OffRow(_))), "{r}");
        // Nothing else before the last scroll.
        assert!(r.violations.iter().all(|v| v.frame >= 32 || (v.word == 4 && (10..17).contains(&v.frame))), "{r}");
        // The same in text that holds still is caught the same way.
        let mut tr = Tracker::new();
        for n in 0..30usize {
            let mut g = word(1, 10.0, 40.0, 9.0, 1.0);
            g.extend(word(2, 50.0, 40.0 + away(n), 9.0, 1.0));
            tr.push(n as f64 / 60.0, &list(g));
        }
        let still = tr.check(&Envelope::default());
        let mut kinds: Vec<_> = still.by_kind().iter().map(|s| s.kind).collect();
        kinds.sort_unstable();
        assert_eq!(kinds, vec!["baseline", "off-row"], "{still}");
    }

    #[test]
    fn a_word_held_below_its_scrolling_row_breaks_the_baseline() {
        // Held about 6 px below its row, creeping up 0.1 px a frame, through the second and
        // third scrolls, then back on its row.
        let r = scrolling(|n| if (8..24).contains(&n) { 6.0 - 0.1 * (n - 8) as f32 } else { 0.0 });
        let base = r.by_kind().into_iter().find(|s| s.kind == "baseline").expect("baseline");
        assert!(base.worst.word == 4 && base.worst.breach.value() > 5.9, "{r}");
        assert!(r.violations.iter().all(|v| v.frame >= 32 || (v.word == 4 && (8..24).contains(&v.frame))), "{r}");
    }

    #[test]
    fn a_spring_that_overshoots_rests_where_it_creeps_back_to() {
        // Arrives 3 px low, overshoots by 0.3 px, then creeps back 0.04 px a frame.
        const PATH: [f32; 12] = [3.0, 1.2, -0.2, -0.3, -0.3, -0.26, -0.22, -0.18, -0.14, -0.1, -0.06, -0.02];
        let r = track(30, |k| (PATH.get(k).copied().unwrap_or(0.0), 1.0));
        // Against the turn of the overshoot it would be 3.3 px off its rest.
        r.assert_ok();
    }

    #[test]
    fn a_word_still_rising_when_the_scroll_sets_off_rests_where_it_was_stillest() {
        // Settles from 3 px low, then the block scrolls up a row; its first step is small.
        const PATH: [f32; 8] = [3.0, 2.0, 1.2, 0.6, 0.25, 0.1, 0.05, -1.15];
        let r = track(30, |k| (PATH.get(k).copied().unwrap_or(-24.0), 1.0));
        let before = r.violations.iter().filter(|v| v.frame < 11).count();
        assert_eq!(before, 0, "{r}");
    }

    #[test]
    fn the_faded_tail_of_an_exit_is_not_a_rest() {
        // Rests, then rises 8 px while it fades out, and lingers almost clear.
        let r = track(40, |k| match k {
            0..=8 => (0.0, 1.0),
            9..=14 => (-(k as f32 - 8.0) * 8.0 / 6.0, 1.0 - (k as f32 - 8.0) / 6.2),
            _ => (-8.0, 0.004 - (k as f32 - 15.0) * 0.0001),
        });
        r.assert_ok();
    }

    #[test]
    fn a_status_row_outside_the_transcript_is_not_the_reading_line() {
        let mut tr = Tracker::new();
        for n in 0..20 {
            let mut g = word(1, 10.0, 40.0, 9.0, 1.0);
            // A status row with a made-up id that moves down a row as the panel grows.
            g.extend(word(1 << 30, 10.0, 80.0 + n.min(6) as f32 * 5.0, 9.0, 1.0));
            tr.push(n as f64 / 60.0, &list(g));
        }
        assert!(!tr.check(&Envelope::default()).is_ok());
        tr.check(&Envelope { transcript: 0..1 << 20, ..Envelope::default() }).assert_ok();
    }

    #[test]
    fn a_word_that_drops_from_a_row_above_is_off_its_row() {
        let r = run(|k| (-20.0 + k as f32 * 3.0, 1.0, 9.0));
        assert!(r.by_kind().iter().any(|s| s.kind == "off-row"), "{r}");
    }

    #[test]
    fn loose_tracking_on_arrival_is_caught() {
        let r = run(|_| (0.0, 1.0, 10.0));
        let t = r.by_kind().into_iter().find(|s| s.kind == "tracking").expect("tracking");
        assert!((t.worst.breach.value() - 20.0 / 18.0).abs() < 1e-3, "{r}");
        let r = run(|_| (0.0, 1.0, 9.3));
        r.assert_ok();
    }

    #[test]
    fn a_glyph_that_leaves_is_not_checked() {
        let mut tr = Tracker::new();
        for n in 0..12 {
            let mut g = word(1, 10.0, 40.0, 9.0, 1.0);
            // A ghost of word 2 that sinks and fades away.
            if n < 6 {
                g.extend(word(2, 50.0, 40.0 + n as f32 * 2.0, 9.0, 1.0 - n as f32 * 0.15));
            }
            tr.push(n as f64 / 60.0, &list(g));
        }
        let r = tr.check(&Envelope::default());
        r.assert_ok();
        assert_eq!(r.leaving, 18);
    }

    #[test]
    fn layer_opacity_rotation_and_size_count() {
        let mut tr = Tracker::new();
        for n in 0..10 {
            let mut l = list(word(1, 10.0, 40.0, 9.0, 1.0));
            if n < 3 {
                l.glyphs[1].size = 18.0;
                l.layers[0].opacity = 0.5;
                l.layers[0].transform = vo_scene::geom::Affine::rotate(0.05);
            }
            tr.push(n as f64 / 60.0, &l);
        }
        let r = tr.check(&Envelope::default());
        let kinds: Vec<_> = r.by_kind().iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&"ink") && kinds.contains(&"rotation") && kinds.contains(&"scale"), "{r}");
    }

    #[test]
    fn the_window_limits_what_is_checked() {
        let mut tr = Tracker::new();
        for n in 0..10 {
            let alpha = if n < 3 { 0.2 } else { 1.0 };
            tr.push(n as f64 / 60.0, &list(word(1, 10.0, 40.0, 9.0, alpha)));
        }
        let env = Envelope { from: 3.0 / 60.0, ..Envelope::default() };
        tr.check(&env).assert_ok();
    }

    /// Kinds of breach for word 1 held still with every glyph given `xform`.
    fn with_xform(xform: vo_scene::display::GlyphXform) -> Vec<&'static str> {
        let mut tr = Tracker::new();
        for n in 0..10 {
            let mut g = word(1, 10.0, 40.0, 9.0, 1.0);
            for g in &mut g {
                g.xform = xform;
            }
            tr.push(n as f64 / 60.0, &list(g));
        }
        let r = tr.check(&Envelope::default());
        assert!(r.glyphs_checked > 0);
        r.by_kind().iter().map(|s| s.kind).collect()
    }

    #[test]
    fn a_glyph_turned_on_its_own_breaks_rotation_even_at_rest() {
        let kinds = with_xform(vo_scene::display::GlyphXform::rotate(2.0));
        assert_eq!(kinds, vec!["rotation"]);
    }

    #[test]
    fn a_lean_inside_the_slant_range_passes_and_past_it_breaks() {
        let lean = |deg| vo_scene::display::GlyphXform { skew_deg: deg, ..Default::default() };
        assert!(with_xform(lean(4.0)).is_empty());
        assert!(with_xform(lean(-4.0)).is_empty());
        assert_eq!(with_xform(lean(6.0)), vec!["skew"]);
    }

    #[test]
    fn a_glyph_moved_on_its_own_counts_for_the_baseline() {
        let mut tr = Tracker::new();
        for n in 0..10 {
            let mut g = word(1, 10.0, 40.0, 9.0, 1.0);
            if n < 3 {
                g[1].xform.offset = Point::new(0.0, 5.0);
            }
            tr.push(n as f64 / 60.0, &list(g));
        }
        let r = tr.check(&Envelope::default());
        let kinds: Vec<_> = r.by_kind().iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec!["baseline"], "{r}");
    }
}
