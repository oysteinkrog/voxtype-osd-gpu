//! Signal-to-glyph binding (MO-04): what the voice did while each word was said, sampled
//! once per word from the signal bus, for the typographic channels (`crate::typo`).
//!
//! [`WordVoices::update`] runs with each new hypothesis. For a word with times from the
//! engine (only the parakeet live preview sends them), it reads the frames whose audio clock
//! lies in the word's span: the recording clock and the bus clock are the same axis, samples
//! since the recording started. A word without times is read when it first appears, over the
//! audio since the word before it appeared (at most [`UNTIMED_WINDOW`] seconds), and its
//! onset is the strongest one in that window. A word read without times is read again once
//! its times arrive, while the frames are still in the bus history; after that its reading
//! is fixed, so a word keeps its look while the speaker goes on.
//!
//! [`VoiceTypo`] turns a reading into channel values: onset into arrival weight (MO-13),
//! pitch against the speaker's running pitch into baseline (MO-14), and spoken duration per
//! letter into tracking (MO-15).

use std::collections::HashMap;

use vo_core::signals::{Sig, SignalFrame, Signals};
use vo_core::words::{WordId, WordModel};

use crate::font::Fnv;
use crate::typo::Typo;

/// Without word times, the window read for a word reaches back at most this far.
pub const UNTIMED_WINDOW: f64 = 1.2;
/// A word said this long per letter has normal tracking.
pub const SECS_PER_LETTER: f32 = 0.07;

/// What the voice did while one word was said.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordVoice {
    /// The strongest onset strength at the word's start (0 to 1).
    pub onset: f32,
    /// Mean and peak level, 0 to 1 as `Sig::Level`.
    pub level: f32,
    pub peak: f32,
    /// Mean pitch of the voiced frames, in semitones from the speaker's running pitch at the
    /// time; 0 when the word had no voiced frame.
    pub pitch: f32,
    /// Pitch at the end of the word less pitch at its start, in semitones.
    pub glide: f32,
    /// Seconds the word took to say: from its times, or the gap since the word before
    /// arrived, clamped to the window.
    pub duration: f32,
    /// Whether the reading comes from the engine's word times.
    pub timed: bool,
    /// Scene time the reading was taken.
    pub at: f64,
}

impl WordVoice {
    pub const QUIET: WordVoice =
        WordVoice { onset: 0.0, level: 0.0, peak: 0.0, pitch: 0.0, glide: 0.0, duration: 0.0, timed: false, at: 0.0 };
}

/// Every word's reading, by id.
#[derive(Clone, Debug, Default)]
pub struct WordVoices {
    voices: HashMap<WordId, WordVoice, Fnv>,
    /// The speaker's running pitch in semitones from 60 Hz, smoothed over words.
    pitch_ref: Option<f32>,
    /// When the newest word first appeared, for the untimed window.
    last_arrival: Option<f64>,
}

fn semitones(hz: f32) -> f32 {
    12.0 * (hz.max(1.0) / 60.0).log2()
}

/// Frames of `signals` whose audio clock is in `lo..=hi`.
fn by_clock(signals: &Signals, lo: f64, hi: f64) -> Vec<SignalFrame> {
    signals.frames().map(|(_, f)| *f).filter(|f| f.clock >= lo && f.clock <= hi).collect()
}

/// Frames of `signals` placed (in scene time, as read `playout` behind) in `lo..=hi`.
fn by_scene(signals: &Signals, lo: f64, hi: f64) -> Vec<SignalFrame> {
    let p = signals.playout();
    signals.frames().filter(|(at, _)| at + p >= lo && at + p <= hi).map(|(_, f)| *f).collect()
}

/// Mean level, peak, mean pitch and glide (semitones, absolute) of `frames`, and the strongest
/// onset in the first `head` of them.
fn read(frames: &[SignalFrame], head: usize) -> Option<(f32, f32, Option<f32>, f32, f32)> {
    if frames.is_empty() {
        return None;
    }
    let level = |db: f32| ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let n = frames.len() as f32;
    let mean = frames.iter().map(|f| level(f.rms_dbfs)).sum::<f32>() / n;
    let peak = frames.iter().map(|f| level(f.peak_dbfs)).fold(0.0, f32::max);
    let onset = frames.iter().take(head).map(|f| f.onset).fold(0.0, f32::max).clamp(0.0, 1.0);
    let voiced: Vec<f32> = frames.iter().filter(|f| f.voiced && f.f0_hz > 0.0).map(|f| semitones(f.f0_hz)).collect();
    let pitch = (!voiced.is_empty()).then(|| voiced.iter().sum::<f32>() / voiced.len() as f32);
    // Glide: the last third of the voiced frames against the first third.
    let glide = if voiced.len() >= 6 {
        let k = voiced.len() / 3;
        let head = voiced[..k].iter().sum::<f32>() / k as f32;
        let tail = voiced[voiced.len() - k..].iter().sum::<f32>() / k as f32;
        tail - head
    } else {
        0.0
    };
    Some((mean, peak, pitch, glide, onset))
}

impl WordVoices {
    pub fn new() -> WordVoices {
        WordVoices::default()
    }

    /// Reads new words and words whose times arrived, at scene time `t`. Drops words that
    /// are gone.
    pub fn update(&mut self, t: f64, model: &WordModel, signals: &Signals) {
        let words = model.words();
        self.voices.retain(|id, _| words.iter().any(|w| w.id == *id));
        for w in words {
            let info = model.info(w.id);
            let times = info.and_then(|i| Some((i.start? as f64, i.end? as f64)));
            let have = self.voices.get(&w.id).copied();
            if have.is_some_and(|v| v.timed || times.is_none()) {
                continue;
            }
            let reading = match times {
                Some((start, end)) => {
                    let frames = by_clock(signals, start - 0.03, end.max(start));
                    read(&frames, 12).map(|r| (r, (end - start).max(0.0) as f32, true))
                }
                None => {
                    let from = self.last_arrival.unwrap_or(t - UNTIMED_WINDOW).max(t - UNTIMED_WINDOW);
                    let frames = by_scene(signals, from, t);
                    let duration = (t - from) as f32;
                    Some(
                        read(&frames, frames.len())
                            .map_or(((0.0, 0.0, None, 0.0, 0.0), duration, false), |r| (r, duration, false)),
                    )
                }
            };
            if have.is_none() {
                self.last_arrival = Some(t);
            }
            let Some(((level, peak, pitch, glide, onset), duration, timed)) = reading else {
                // Timed, but the frames are gone or not here yet: keep any untimed reading.
                if have.is_none() {
                    self.voices.insert(w.id, WordVoice { at: t, ..WordVoice::QUIET });
                }
                continue;
            };
            let rel = match (pitch, self.pitch_ref) {
                (Some(p), Some(r)) => p - r,
                _ => 0.0,
            };
            if let Some(p) = pitch {
                // About a five-word memory.
                self.pitch_ref = Some(self.pitch_ref.map_or(p, |r| r + 0.2 * (p - r)));
            }
            self.voices.insert(w.id, WordVoice { onset, level, peak, pitch: rel, glide, duration, timed, at: t });
        }
    }

    /// Word `id`'s reading, once it has one.
    pub fn get(&self, id: WordId) -> Option<WordVoice> {
        self.voices.get(&id).copied()
    }

    /// The live pitch at `t` in semitones from the speaker's running pitch, while voiced; 0
    /// when not voiced or before any pitch is known. For a baseline that follows the word
    /// being said (MO-14).
    pub fn live_pitch(&self, signals: &Signals, t: f64) -> f32 {
        let Some(r) = self.pitch_ref else { return 0.0 };
        if signals.sample(Sig::Voiced, t) < 0.5 {
            return 0.0;
        }
        let hz = signals.sample(Sig::Raw(vo_core::signals::Field::F0Hz), t);
        if hz <= 0.0 {
            return 0.0;
        }
        semitones(hz) - r
    }
}

/// Most pitch baseline either way, in px: half the reading line's baseline limit
/// ([`Envelope::READING`]). A word follows the pitch while it is said, so it can swing from
/// one end to the other, and the whole swing must stay within the limit of where it rests.
///
/// [`Envelope::READING`]: crate::typo::Envelope::READING
pub const PITCH_REACH: f32 = crate::typo::Envelope::READING.baseline / 2.0;

/// How a reading becomes channel values. Each part can be turned off with a zero gain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceTypo {
    /// Arrival weight in px at full onset (MO-13); a soft start (onset under `soft`) arrives
    /// thin by up to the same amount and with its fill at `soft_fill`.
    pub onset_weight: f32,
    pub soft: f32,
    pub soft_fill: f32,
    /// Baseline in px per semitone (MO-14).
    pub pitch_px: f32,
    /// Tracking from spoken duration (MO-15): the factor is `(duration / (letters *
    /// SECS_PER_LETTER))` raised to this power, clamped to `tracking_range`.
    pub tracking_gamma: f32,
    pub tracking_range: (f32, f32),
}

impl Default for VoiceTypo {
    /// 0.6 px at a hard onset, 0.5 px per semitone, tracking from 0.8 to 1.3.
    fn default() -> VoiceTypo {
        VoiceTypo {
            onset_weight: 0.6,
            soft: 0.25,
            soft_fill: 0.3,
            pitch_px: 0.5,
            tracking_gamma: 0.5,
            tracking_range: (0.8, 1.3),
        }
    }
}

impl VoiceTypo {
    /// Where a word with reading `v` and `letters` letters arrives: heavier after a hard
    /// onset, light and unfilled after a soft one, spaced by how long it took to say.
    pub fn arrive(&self, v: &WordVoice, letters: usize) -> Typo {
        let mut out = Typo::REST.tracking(self.tracking(v, letters));
        if v.onset >= self.soft {
            let k = ((v.onset - self.soft) / (1.0 - self.soft).max(1e-3)).clamp(0.0, 1.0);
            out.weight = self.onset_weight * k;
        } else if self.onset_weight > 0.0 {
            let k = 1.0 - v.onset / self.soft.max(1e-3);
            out.weight = -0.5 * self.onset_weight * k;
            out.fill = self.soft_fill + (1.0 - self.soft_fill) * (1.0 - k);
        }
        out
    }

    /// Spoken-time tracking factor (MO-15).
    pub fn tracking(&self, v: &WordVoice, letters: usize) -> f32 {
        if self.tracking_gamma == 0.0 || v.duration <= 0.0 || letters == 0 {
            return 1.0;
        }
        let ratio = v.duration / (letters as f32 * SECS_PER_LETTER);
        ratio.powf(self.tracking_gamma).clamp(self.tracking_range.0, self.tracking_range.1)
    }

    /// Baseline in px for `semitones` from the speaker's running pitch (MO-14), within
    /// [`PITCH_REACH`] either way.
    pub fn baseline(&self, semitones: f32) -> f32 {
        (semitones * self.pitch_px).clamp(-PITCH_REACH, PITCH_REACH)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vo_core::signals::SignalFrame;
    use vo_core::words::WordInfo;

    fn frame(clock: f64, db: f32, hz: f32, onset: f32) -> SignalFrame {
        SignalFrame {
            rms_dbfs: db,
            peak_dbfs: db + 3.0,
            f0_hz: hz,
            voiced: hz > 0.0,
            onset,
            ..SignalFrame::silent(clock)
        }
    }

    /// One second of speech: a hard onset at 0.2 s, 150 Hz to 0.5 s, then 200 Hz to 0.9 s.
    fn speech() -> Signals {
        let frames = (1..=100).map(|i| {
            let c = i as f64 * 0.01;
            let hz = if c < 0.2 {
                0.0
            } else if c < 0.5 {
                150.0
            } else if c < 0.9 {
                200.0
            } else {
                0.0
            };
            let onset = if (c - 0.2).abs() < 0.005 { 0.9 } else { 0.05 };
            frame(c, if hz > 0.0 { -20.0 } else { -55.0 }, hz, onset)
        });
        Signals::recorded(frames, 10.0)
    }

    #[test]
    fn timed_words_read_their_span() {
        let mut m = WordModel::new();
        m.update("hello there");
        let info = |s: f32, e: f32| WordInfo { start: Some(s), end: Some(e), ..Default::default() };
        m.set_info(&[info(0.2, 0.5), info(0.5, 0.9)]);
        let mut v = WordVoices::new();
        v.update(12.0, &m, &speech());
        let ids: Vec<_> = m.words().iter().map(|w| w.id).collect();
        let (a, b) = (v.get(ids[0]).unwrap(), v.get(ids[1]).unwrap());
        assert!(a.timed && b.timed);
        assert!(a.onset > 0.8, "{a:?}");
        assert!(b.onset < 0.1, "{b:?}");
        assert!((a.duration - 0.3).abs() < 1e-4);
        assert!(a.level > 0.5);
        // The second word is higher than the running pitch the first one set (5 semitones).
        assert!(b.pitch > 3.0, "{b:?}");
    }

    #[test]
    fn untimed_words_read_since_the_last_arrival_and_upgrade() {
        let mut m = WordModel::new();
        m.update("hello");
        let s = speech();
        let mut v = WordVoices::new();
        // Frames sit at scene 10.01 to 11.0; read at 11.1 with the 50 ms playout.
        v.update(11.1, &m, &s);
        let id = m.words()[0].id;
        let a = v.get(id).unwrap();
        assert!(!a.timed && a.onset > 0.8 && (a.duration - UNTIMED_WINDOW as f32).abs() < 1e-4, "{a:?}");
        m.set_info(&[WordInfo { start: Some(0.5), end: Some(0.9), ..Default::default() }]);
        v.update(11.2, &m, &s);
        let b = v.get(id).unwrap();
        assert!(b.timed && b.onset < 0.1);
        // Fixed from now on.
        v.update(11.3, &m, &Signals::new());
        assert_eq!(v.get(id), Some(b));
    }

    #[test]
    fn voice_typo_maps_onset_pitch_and_duration() {
        let vt = VoiceTypo::default();
        let hard = WordVoice { onset: 1.0, duration: 0.35, ..WordVoice::QUIET };
        let a = vt.arrive(&hard, 5);
        assert!((a.weight - 0.6).abs() < 1e-4 && a.fill == 1.0);
        assert!((a.tracking - 1.0).abs() < 1e-4);
        let soft = WordVoice { onset: 0.0, duration: 1.4, ..WordVoice::QUIET };
        let b = vt.arrive(&soft, 5);
        assert!(b.weight < 0.0 && b.fill < 0.5);
        assert_eq!(b.tracking, 1.3);
        assert_eq!(vt.tracking(&WordVoice { duration: 0.05, ..WordVoice::QUIET }, 5), 0.8);
        assert_eq!(vt.baseline(2.0), 1.0);
    }

    #[test]
    fn a_pitch_swing_from_one_end_to_the_other_stays_within_the_reading_line() {
        let vt = VoiceTypo { pitch_px: 1.0, ..VoiceTypo::default() };
        let swing = vt.baseline(12.0) - vt.baseline(-12.0);
        assert!(swing > 0.0 && swing <= crate::typo::Envelope::READING.baseline, "{swing}");
    }
}
