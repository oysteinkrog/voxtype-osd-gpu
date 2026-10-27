//! Hearing before reading (MO-11, MO-12, MO-30): what a dictation popup can show from the
//! audio while the recogniser has not caught up. Audio leads the words by 100 to 400 ms, so
//! the popup reacts to the voice first and to the words when they come.
//!
//! [`Hearing`] takes the audio one frame at a time ([`Hearing::hear`]: the frame's time in
//! the recording, its level from 0 to 1 and, when the daemon measures one, an onset
//! strength) and how far into the recording the words on screen reach
//! ([`Hearing::read_until`]). From that it keeps:
//!
//! - **Pre-echo blobs** (MO-11): one per onset heard past the last recognised word, as wide
//!   as its burst of voice lasted. When the words that cover a blob arrive, the blob is
//!   read: it turns into its word's box and fades ([`pre_echo`]).
//! - **The pause** (MO-30): after [`PAUSE_TICKS_AFTER`] of quiet a tick per
//!   [`PAUSE_TICK_EVERY`], gathering into a hollow comma at [`PAUSE_COMMA_AT`] and a hollow
//!   full stop at [`PAUSE_STOP_AT`] ([`Pause`], [`pause_marks`]). On the next onset the ticks
//!   squeeze into a space.
//! - **The exhale** (MO-12): after [`EXHALE_AFTER`] of quiet, a 0 to 1 amount over
//!   [`EXHALE_SECS`] for the chrome around the text to rest by; on the next onset it is 0
//!   again at once, and the looks move back on [`SNAP_BACK`].
//!
//! All of it is plain state on the audio clock, so a look that rebuilds its view on each
//! message reads it from there, and a test feeds it frames. Nothing here touches the text:
//! blobs and ticks sit on the baseline beside the words at well under text contrast, and
//! the exhale is for everything but the text (rule 1, the reading-line envelope).
//!
//! Without onset strengths (a daemon that sends levels only, or a scripted demo) an onset
//! is a rise of [`HearingTune::rise`] in level from the last dip.

use vo_core::spring::Spring;

use crate::element::{Element, Transition};
use crate::geom::{Color, Rect};
use crate::path::{Outline, StrokeStyle};
use crate::{Motion, Prop};

/// Quiet before the first pause tick, in seconds.
pub const PAUSE_TICKS_AFTER: f64 = 0.6;
/// One more tick per this much quiet.
pub const PAUSE_TICK_EVERY: f64 = 0.4;
/// Quiet at which the ticks gather into a hollow comma.
pub const PAUSE_COMMA_AT: f64 = 1.5;
/// Quiet at which the comma becomes a hollow full stop.
pub const PAUSE_STOP_AT: f64 = 2.6;
/// Quiet before the chrome starts to exhale.
pub const EXHALE_AFTER: f64 = 3.0;
/// How long the exhale takes.
pub const EXHALE_SECS: f64 = 1.2;
/// A read blob stays this long while it turns into its word's box and fades.
pub const HANDOFF_SECS: f64 = 0.3;
/// The blob turns into its word's box in about this long.
pub const HANDOFF_MORPH: f64 = 0.12;
/// The ticks squeeze into a space in this long.
pub const SQUEEZE_SECS: f64 = 0.09;
/// One fast spring back from the exhale, slightly underdamped so it lands with life.
pub const SNAP_BACK: Spring = Spring::from_frequency(34.0, 0.8);

/// Thresholds for reading voice and onsets from the levels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HearingTune {
    /// A frame at or above this level is voice.
    pub voice: f32,
    /// A frame below this level is quiet. Between the two the state holds.
    pub quiet: f32,
    /// A daemon onset strength at or above this starts a blob.
    pub onset: f32,
    /// Without onset strengths: a rise of this much from the last dip starts a blob, and a
    /// fall of this much from the peak arms the next one.
    pub rise: f32,
    /// Onsets closer than this to the last one are one onset (at most a few a second).
    pub min_gap: f64,
    /// Blobs older than this that no word has read fade anyway, so a clock that does not
    /// line up with the word times never leaves blobs behind.
    pub max_age: f64,
    /// Most unread blobs kept; the oldest go first.
    pub max_blobs: usize,
}

impl Default for HearingTune {
    fn default() -> HearingTune {
        HearingTune {
            voice: 0.12,
            quiet: 0.06,
            onset: vo_core::signals::ONSET_THRESHOLD,
            rise: 0.1,
            min_gap: 0.12,
            max_age: 2.0,
            max_blobs: 12,
        }
    }
}

/// One onset heard and the burst of voice after it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blob {
    /// Unique within one [`Hearing`], for element keys.
    pub id: u64,
    /// The onset, in seconds of the recording.
    pub start: f64,
    /// The last voiced frame of the burst so far.
    pub end: f64,
    /// When a word read it, on the audio clock; `None` while it is heard, not yet read.
    pub read_at: Option<f64>,
    /// Which of the words that read it the blob turns into, counted from the first of them.
    pub word: usize,
}

impl Blob {
    /// How long the burst lasted, at least one frame.
    pub fn secs(&self) -> f64 {
        (self.end - self.start).max(0.04)
    }
}

/// The mark a pause gathers into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mark {
    Comma,
    Stop,
}

impl Mark {
    /// The punctuation the mark stands for.
    pub fn char(self) -> char {
        match self {
            Mark::Comma => ',',
            Mark::Stop => '.',
        }
    }
}

/// The current pause, from the end of speech.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pause {
    /// Seconds of quiet.
    pub quiet: f64,
    /// Ticks laid so far, before they gather.
    pub ticks: u32,
    /// The hollow mark, once the ticks have gathered.
    pub mark: Option<Mark>,
}

impl Pause {
    /// The pause after `quiet` seconds of quiet.
    pub fn after(quiet: f64) -> Pause {
        let ticks =
            if quiet < PAUSE_TICKS_AFTER { 0 } else { 1 + ((quiet - PAUSE_TICKS_AFTER) / PAUSE_TICK_EVERY) as u32 };
        let mark = if quiet >= PAUSE_STOP_AT {
            Some(Mark::Stop)
        } else if quiet >= PAUSE_COMMA_AT {
            Some(Mark::Comma)
        } else {
            None
        };
        // Once gathered, the ticks are spent: as many as had been laid at the comma.
        let laid = 1 + ((PAUSE_COMMA_AT - PAUSE_TICKS_AFTER) / PAUSE_TICK_EVERY) as u32;
        Pause { quiet, ticks: ticks.min(laid), mark }
    }
}

/// The exhale amount after `quiet` seconds of quiet: 0 until [`EXHALE_AFTER`], then eased
/// to 1 over [`EXHALE_SECS`]. Under reduced motion it steps to 1 at the start.
pub fn exhale_after(quiet: f64, reduced: bool) -> f32 {
    let x = ((quiet - EXHALE_AFTER) / EXHALE_SECS).clamp(0.0, 1.0) as f32;
    if reduced {
        return if quiet >= EXHALE_AFTER { 1.0 } else { 0.0 };
    }
    x * x * (3.0 - 2.0 * x)
}

/// What the audio says before the words do. See the module docs.
#[derive(Clone, Debug)]
pub struct Hearing {
    tune: HearingTune,
    /// The newest frame heard, in seconds of the recording.
    clock: Option<f64>,
    /// Voice is on (between the thresholds it holds).
    voiced: bool,
    /// Frames in a row at or above the voice level, for waking without an onset.
    loud_run: u32,
    /// The level-rise detector: the lowest level since the last peak fell away, and the
    /// highest since the last onset. `armed` once the level has fallen from that peak.
    dip: f32,
    peak: f32,
    armed: bool,
    /// The previous frame's onset strength, for rising edges.
    last_strength: f32,
    last_onset: Option<f64>,
    /// When the quiet began; `None` while speaking.
    quiet_from: Option<f64>,
    /// Voice was heard at least once.
    spoke: bool,
    blobs: Vec<Blob>,
    next_id: u64,
    /// The words on screen reach this far into the recording.
    read: f64,
}

impl Default for Hearing {
    fn default() -> Hearing {
        Hearing::new(HearingTune::default())
    }
}

impl Hearing {
    pub fn new(tune: HearingTune) -> Hearing {
        Hearing {
            tune,
            clock: None,
            voiced: false,
            loud_run: 0,
            dip: 0.0,
            peak: 0.0,
            armed: true,
            last_strength: 0.0,
            last_onset: None,
            quiet_from: Some(0.0),
            spoke: false,
            blobs: Vec::new(),
            next_id: 0,
            read: f64::NEG_INFINITY,
        }
    }

    pub fn tune(&self) -> &HearingTune {
        &self.tune
    }

    /// Starts again for a new recording.
    pub fn reset(&mut self) {
        let next_id = self.next_id;
        *self = Hearing::new(self.tune);
        self.next_id = next_id;
    }

    /// The newest frame heard, in seconds of the recording.
    pub fn clock(&self) -> Option<f64> {
        self.clock
    }

    /// One audio frame at `clock` seconds into the recording, with its `level` from 0 to 1
    /// and the daemon's onset `strength` when it measures one. Frames at or before the last
    /// one are ignored.
    pub fn hear(&mut self, clock: f64, level: f32, strength: Option<f32>) {
        if self.clock.is_some_and(|c| clock <= c) {
            return;
        }
        self.clock = Some(clock);
        let t = self.tune;
        let gap_ok = self.last_onset.is_none_or(|o| clock - o >= t.min_gap);
        let onset = match strength {
            Some(s) => {
                let edge = s >= t.onset && self.last_strength < t.onset;
                self.last_strength = s;
                edge && level >= t.quiet && gap_ok
            }
            None => {
                self.peak = self.peak.max(level);
                if self.peak - level >= t.rise {
                    self.armed = true;
                }
                if self.armed {
                    self.dip = self.dip.min(level);
                }
                let rose = self.armed && level >= t.voice && level - self.dip >= t.rise;
                rose && gap_ok
            }
        };
        if level >= t.voice {
            self.voiced = true;
            self.loud_run += 1;
        } else {
            self.loud_run = 0;
            if level < t.quiet {
                self.voiced = false;
            }
        }
        if onset {
            self.onset(clock, level);
        } else if self.loud_run >= 3 && self.quiet_from.is_some() {
            // A steady voice without a clear onset still ends the quiet.
            self.quiet_from = None;
            self.spoke = true;
        }
        if self.voiced {
            if let Some(b) = self.blobs.iter_mut().rev().find(|b| b.read_at.is_none()) {
                if b.end >= clock - 0.15 {
                    b.end = clock;
                }
            }
        } else if self.quiet_from.is_none() {
            self.quiet_from = Some(clock);
        }
        let max_age = t.max_age;
        for b in &mut self.blobs {
            if b.read_at.is_none() && clock - b.start > max_age {
                b.read_at = Some(clock);
                b.word = usize::MAX;
            }
        }
        self.blobs.retain(|b| b.read_at.is_none_or(|r| clock - r <= HANDOFF_SECS));
    }

    fn onset(&mut self, clock: f64, level: f32) {
        self.last_onset = Some(clock);
        self.armed = false;
        self.peak = level;
        self.dip = level;
        self.quiet_from = None;
        self.spoke = true;
        // A word may already cover this audio: then nothing is waiting to be read.
        if clock < self.read {
            return;
        }
        // The open burst ends where this one starts.
        if let Some(b) = self.blobs.iter_mut().rev().find(|b| b.read_at.is_none()) {
            b.end = b.end.min(clock - 0.02).max(b.start);
        }
        self.blobs.push(Blob { id: self.next_id, start: clock, end: clock, read_at: None, word: 0 });
        self.next_id += 1;
        let unread = self.blobs.iter().filter(|b| b.read_at.is_none()).count();
        if unread > self.tune.max_blobs {
            if let Some(old) = self.blobs.iter_mut().find(|b| b.read_at.is_none()) {
                old.read_at = Some(clock);
                old.word = usize::MAX;
            }
        }
    }

    /// The words on screen now reach `until` seconds into the recording, and `new_words`
    /// of them arrived since the last call. Blobs that start before `until` are read, each
    /// handed to one of the new words in order (several blobs can share a word). Returns
    /// how many blobs were read.
    pub fn read_until(&mut self, until: f64, new_words: usize) -> usize {
        if until <= self.read {
            return 0;
        }
        self.read = until;
        let now = self.clock.unwrap_or(until);
        let fresh: Vec<usize> =
            (0..self.blobs.len()).filter(|&i| self.blobs[i].read_at.is_none() && self.blobs[i].start < until).collect();
        let n = fresh.len();
        for (k, &i) in fresh.iter().enumerate() {
            let b = &mut self.blobs[i];
            b.read_at = Some(now);
            b.word = if new_words == 0 { usize::MAX } else { k * new_words / n };
        }
        n
    }

    /// Words arrived and the engine gives no times: everything heard more than `lag`
    /// seconds ago is read.
    pub fn read_untimed(&mut self, new_words: usize, lag: f64) -> usize {
        match self.clock {
            Some(c) => self.read_until(c - lag, new_words),
            None => 0,
        }
    }

    /// The blobs on screen: heard and not yet read, then the ones still turning into
    /// their word.
    pub fn blobs(&self) -> &[Blob] {
        &self.blobs
    }

    /// Seconds of quiet up to the newest frame; 0 while speaking.
    pub fn quiet(&self) -> f64 {
        match (self.quiet_from, self.clock) {
            (Some(q), Some(c)) => (c - q).max(0.0),
            _ => 0.0,
        }
    }

    /// The pause after speech, once there was speech; `None` while speaking.
    pub fn pause(&self) -> Option<Pause> {
        (self.spoke && self.quiet_from.is_some()).then(|| Pause::after(self.quiet()))
    }

    /// When the current quiet began, on the audio clock.
    pub fn quiet_from(&self) -> Option<f64> {
        self.quiet_from
    }

    /// When the last onset came, on the audio clock.
    pub fn last_onset(&self) -> Option<f64> {
        self.last_onset
    }

    /// How far the chrome has exhaled, 0 to 1 ([`exhale_after`]).
    pub fn exhale(&self, reduced: bool) -> f32 {
        exhale_after(self.quiet(), reduced)
    }
}

/// How pre-echo blobs and pause marks look.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HearingLook {
    /// Ink of blobs, ticks and marks: the text colour or an accent.
    pub ink: Color,
    /// Blob opacity, well under the text's ("heard, not yet read").
    pub blob_ink: f32,
    /// Blob width per second of voice, clamped to `blob_min..blob_max`.
    pub px_per_sec: f32,
    pub blob_min: f32,
    pub blob_max: f32,
    /// Blob height, and the gap between blobs.
    pub blob_height: f32,
    pub blob_gap: f32,
    /// Pause tick size and spacing.
    pub tick_height: f32,
    pub tick_gap: f32,
    pub tick_ink: f32,
    /// The hollow mark's height (about the font's comma) and stroke.
    pub mark_height: f32,
    pub mark_stroke: f32,
}

impl HearingLook {
    /// Blobs and marks for a transcript `font_px` tall in `ink`.
    pub fn for_text(font_px: f32, ink: Color) -> HearingLook {
        HearingLook {
            ink,
            blob_ink: 0.15,
            px_per_sec: font_px * 4.5,
            blob_min: font_px * 0.45,
            blob_max: font_px * 3.0,
            blob_height: (font_px * 0.22).max(3.0),
            blob_gap: (font_px * 0.25).max(3.0),
            tick_height: (font_px * 0.25).max(3.0),
            tick_gap: (font_px * 0.2).max(3.0),
            tick_ink: 0.55,
            mark_height: font_px * 0.42,
            mark_stroke: 1.0,
        }
    }
}

fn place<M>(e: Element<M>, x: f32, y: f32, w: f32, h: f32) -> Element<M> {
    use taffy::style::{LengthPercentageAuto, Position};
    e.size(w, h).layout(|s| {
        s.position = Position::Absolute;
        s.inset = taffy::Rect {
            left: LengthPercentageAuto::length(x),
            top: LengthPercentageAuto::length(y),
            right: LengthPercentageAuto::auto(),
            bottom: LengthPercentageAuto::auto(),
        };
    })
}

/// The pre-echo blobs as absolutely placed capsules for a text box. `caret` is where the
/// next word would start, on the baseline of the reading line, and `room` the width left
/// on that line; unread blobs run right from there, squeezed to fit. A read blob moves to
/// `word_box(blob)` (the box of the word it was handed to, in the same coordinates) in about
/// [`HANDOFF_MORPH`] and fades; without a box it fades where it is. Keys are
/// `pre-echo-<id>`, so a blob keeps its node from growth to hand-off.
pub fn pre_echo<M>(
    hearing: &Hearing,
    look: &HearingLook,
    caret: (f32, f32),
    room: f32,
    word_box: impl Fn(&Blob) -> Option<Rect>,
) -> Vec<Element<M>> {
    let unread: Vec<&Blob> = hearing.blobs().iter().filter(|b| b.read_at.is_none()).collect();
    let want: f32 = unread
        .iter()
        .map(|b| (b.secs() as f32 * look.px_per_sec).clamp(look.blob_min, look.blob_max) + look.blob_gap)
        .sum();
    let squeeze = if want > room && want > 0.0 { (room / want).max(0.0) } else { 1.0 };
    let morph = Motion::Spring(Spring::critical_to_98(HANDOFF_MORPH));
    let h = look.blob_height;
    let y = caret.1 + 1.0;
    let mut x = caret.0;
    let mut out = Vec::new();
    let mut slots = std::collections::HashMap::new();
    for b in &unread {
        let w = (b.secs() as f32 * look.px_per_sec).clamp(look.blob_min, look.blob_max) * squeeze;
        slots.insert(b.id, (x, w));
        x += w + look.blob_gap * squeeze;
    }
    for b in hearing.blobs() {
        let key = format!("pre-echo-{}", b.id);
        let shape = Element::shape(key, crate::display::ShapeKind::Capsule)
            .fill(look.ink)
            .motion(Prop::Position, morph)
            .motion(Prop::Size, morph)
            .motion(Prop::Radius, morph)
            .motion(Prop::Opacity, Motion::Spring(Spring::critical_to_98(HANDOFF_SECS)));
        let e = match (b.read_at, slots.get(&b.id)) {
            (None, Some(&(bx, w))) => place(shape.opacity(look.blob_ink), bx, y, w.max(1.0), h)
                // A new blob grows from nothing at the caret.
                .presence(Transition::fade().opacity(0.0), Transition::fade()),
            _ => match word_box(b) {
                Some(r) => place(shape.opacity(0.0).radius(r.h * 0.2), r.x, r.y, r.w.max(1.0), r.h.max(1.0)),
                // Read with no word to go to: fade in place.
                None => place(shape.opacity(0.0), caret.0, y, look.blob_min, h),
            },
        };
        out.push(e);
    }
    out
}

/// A hollow comma or full stop `h` tall with its dot's bottom on y = `h` * 0.62, in a box
/// `h` * 0.5 wide and `h` tall, as a stroke outline.
pub fn mark_outline(mark: Mark, h: f32) -> Outline {
    use kurbo::{BezPath, Circle, Shape};
    let r = (h * 0.16) as f64;
    let cx = (h * 0.25) as f64;
    let cy = (h * 0.62) as f64 - r;
    let mut p = BezPath::new();
    match mark {
        Mark::Stop => p.extend(Circle::new((cx, cy), r).path_elements(0.05)),
        Mark::Comma => {
            // The dot, then a tail curling down and left from its right side.
            let h = h as f64;
            p.move_to((cx + r, cy));
            p.curve_to((cx + r, cy - r * 1.4), (cx - r, cy - r * 1.4), (cx - r, cy));
            p.curve_to((cx - r, cy + r * 0.8), (cx, cy + r), (cx + r * 0.2, cy + r * 0.9));
            p.curve_to((cx + r * 0.1, cy + r * 1.8), (cx - r * 0.6, h * 0.92), (cx - r * 1.3, h * 0.98));
            p.curve_to((cx + r * 0.2, h * 0.9), (cx + r * 1.1, cy + r * 1.6), (cx + r, cy));
            p.close_path();
        }
    }
    Outline::new(&p, 0.05)
}

/// The pause at the caret as absolutely placed elements: ticks along the baseline while
/// the quiet grows, then the hollow mark at `caret` (just after the last word) once they
/// gather. `filled` fills a placed mark (the final pass agreed with it). Keys start with
/// `pause-<n>` for pause number `n`, so each pause keeps its own nodes.
pub fn pause_marks<M>(n: u64, pause: &Pause, look: &HearingLook, caret: (f32, f32), filled: bool) -> Vec<Element<M>> {
    let mut out = Vec::new();
    let tick_motion = Motion::Spring(Spring::critical_to_98(SQUEEZE_SECS));
    let (x0, base) = caret;
    let gathered = pause.mark.is_some();
    for i in 0..pause.ticks {
        // Ticks stand on the baseline; once gathered they pull into the mark and fade.
        let x = if gathered { x0 + look.mark_height * 0.25 } else { x0 + 2.0 + i as f32 * look.tick_gap };
        let tick = Element::shape(format!("pause-{n}-tick-{i}"), crate::display::ShapeKind::RRect)
            .fill(look.ink)
            .opacity(if gathered { 0.0 } else { look.tick_ink })
            .motion(Prop::Position, tick_motion)
            .motion(Prop::Opacity, tick_motion)
            .motion(Prop::Size, tick_motion)
            // A new tick grows up from the baseline; a squeezed one shrinks into the space.
            .presence(Transition::fade().opacity(0.0).offset(0.0, look.tick_height), Transition::fade().opacity(0.0));
        out.push(place(tick, x, base - look.tick_height, 1.0, look.tick_height));
    }
    if let Some(mark) = pause.mark {
        let h = look.mark_height;
        let outline = std::sync::Arc::new(mark_outline(mark, h));
        let view = Rect::new(0.0, 0.0, h * 0.5, h);
        let mut e = Element::stroke_path(
            format!("pause-{n}-{}", if mark == Mark::Comma { "comma" } else { "stop" }),
            outline,
            view,
            StrokeStyle::solid(look.mark_stroke, look.ink),
        )
        .opacity(look.tick_ink.max(0.7))
        .motion(Prop::Trim, Motion::Spring(Spring::critical_to_98(0.2)))
        .motion(Prop::Opacity, Motion::Spring(Spring::critical_to_98(0.2)))
        .presence(Transition::NONE.trim(0.0, 0.0), Transition::fade().opacity(0.0));
        if filled {
            e = e.path_fill(look.ink);
        }
        out.push(place(e, x0, base - h * 0.62, h * 0.5, h));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `secs` of frames at 100 Hz from `from`, with the level from `level(t)`.
    fn feed(h: &mut Hearing, from: f64, secs: f64, level: impl Fn(f64) -> f32) {
        let n = (secs * 100.0).round() as usize;
        for i in 0..n {
            let t = from + i as f64 * 0.01;
            h.hear(t, level(t), None);
        }
    }

    /// Syllables at 4 a second: 150 ms of voice, 100 ms of near silence.
    fn syllables(t: f64) -> f32 {
        if (t * 4.0).fract() < 0.6 {
            0.5
        } else {
            0.02
        }
    }

    #[test]
    fn one_blob_per_onset_with_its_duration() {
        let mut h = Hearing::default();
        feed(&mut h, 0.0, 1.0, syllables);
        let blobs = h.blobs();
        assert_eq!(blobs.len(), 4, "{blobs:?}");
        for b in blobs {
            assert!(b.read_at.is_none());
            assert!((b.secs() - 0.14).abs() < 0.03, "{b:?}");
        }
    }

    #[test]
    fn onset_strengths_from_the_daemon_start_blobs() {
        let mut h = Hearing::default();
        for i in 0..100 {
            let t = i as f64 * 0.01;
            let s = if i % 30 == 0 { 0.9 } else { 0.1 };
            h.hear(t, 0.4, Some(s));
        }
        assert_eq!(h.blobs().len(), 4);
        // A steady voice keeps the open burst going until the next onset.
        assert!((h.blobs()[0].secs() - 0.28).abs() < 0.02, "{:?}", h.blobs()[0]);
    }

    #[test]
    fn words_read_the_blobs_they_cover_in_order() {
        let mut h = Hearing::default();
        feed(&mut h, 0.0, 1.0, syllables);
        // Two words covering the first 0.55 s: the first three onsets (0, 0.25, 0.5).
        assert_eq!(h.read_until(0.55, 2), 3);
        let words: Vec<usize> = h.blobs().iter().filter(|b| b.read_at.is_some()).map(|b| b.word).collect();
        assert_eq!(words, vec![0, 0, 1]);
        assert_eq!(h.blobs().iter().filter(|b| b.read_at.is_none()).count(), 1);
        // Read blobs leave after the hand-off.
        feed(&mut h, 1.0, 0.5, |_| 0.0);
        assert_eq!(h.blobs().len(), 1);
        // Audio a word already covers makes no blob.
        let mut h = Hearing::default();
        h.read_until(5.0, 0);
        feed(&mut h, 0.0, 1.0, syllables);
        assert!(h.blobs().is_empty());
    }

    #[test]
    fn unread_blobs_fade_after_max_age() {
        let mut h = Hearing::default();
        feed(&mut h, 0.0, 0.3, syllables);
        feed(&mut h, 0.3, 2.5, |_| 0.0);
        assert!(h.blobs().is_empty());
    }

    #[test]
    fn pause_ticks_then_comma_then_stop() {
        assert_eq!(Pause::after(0.5), Pause { quiet: 0.5, ticks: 0, mark: None });
        assert_eq!(Pause::after(0.6).ticks, 1);
        assert_eq!(Pause::after(1.0).ticks, 2);
        assert_eq!(Pause::after(1.45).ticks, 3);
        assert_eq!(Pause::after(1.5).mark, Some(Mark::Comma));
        assert_eq!(Pause::after(3.0).mark, Some(Mark::Stop));
        assert_eq!(Pause::after(9.0).ticks, 3);
        let mut h = Hearing::default();
        // Silence before any speech is not a pause.
        feed(&mut h, 0.0, 2.0, |_| 0.0);
        assert_eq!(h.pause(), None);
        feed(&mut h, 2.0, 0.5, syllables);
        feed(&mut h, 2.5, 1.0, |_| 0.0);
        let p = h.pause().expect("a pause after speech");
        assert!(p.ticks >= 2 && p.mark.is_none(), "{p:?}");
        // The next onset ends it.
        feed(&mut h, 3.5, 0.05, |_| 0.5);
        assert_eq!(h.pause(), None);
    }

    #[test]
    fn exhale_after_three_seconds_and_back_on_the_onset() {
        assert_eq!(exhale_after(2.9, false), 0.0);
        assert!((exhale_after(3.6, false) - 0.5).abs() < 1e-6);
        assert_eq!(exhale_after(4.2, false), 1.0);
        assert_eq!(exhale_after(3.01, true), 1.0);
        let mut h = Hearing::default();
        feed(&mut h, 0.0, 0.5, syllables);
        feed(&mut h, 0.5, 5.0, |_| 0.0);
        assert_eq!(h.exhale(false), 1.0);
        feed(&mut h, 5.5, 0.03, |_| 0.5);
        assert_eq!(h.exhale(false), 0.0);
    }

    #[test]
    fn a_steady_noise_floor_between_thresholds_holds_the_state() {
        let mut h = Hearing::default();
        feed(&mut h, 0.0, 0.5, syllables);
        feed(&mut h, 0.5, 2.0, |_| 0.08);
        // 0.08 is between quiet and voice: the quiet that began at the last dip holds.
        assert!(h.pause().is_some());
    }

    #[test]
    fn blobs_fit_the_room_left_on_the_line() {
        let mut h = Hearing::default();
        feed(&mut h, 0.0, 2.0, syllables);
        let look = HearingLook::for_text(20.0, Color::WHITE);
        let els: Vec<Element<()>> = pre_echo(&h, &look, (100.0, 30.0), 60.0, |_| None);
        assert_eq!(els.len(), h.blobs().len());
        assert!(!els.is_empty());
    }

    #[test]
    fn marks_have_outlines() {
        for m in [Mark::Comma, Mark::Stop] {
            assert!(mark_outline(m, 10.0).length() > 5.0);
        }
        let look = HearingLook::for_text(20.0, Color::WHITE);
        let p = Pause::after(1.0);
        assert_eq!(pause_marks::<()>(0, &p, &look, (0.0, 20.0), false).len(), 2);
        let p = Pause::after(2.0);
        assert_eq!(pause_marks::<()>(0, &p, &look, (0.0, 20.0), true).len(), 4);
    }
}
