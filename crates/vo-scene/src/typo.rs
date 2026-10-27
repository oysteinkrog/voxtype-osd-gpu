//! Per-word typographic channels (MO-03): ink, weight, tracking, baseline and a left to right
//! fill on live text, each on its own spring, clamped on the reading line.
//!
//! A look describes what each word should look like now with a [`WordTypo`]: where it rests,
//! where it starts when it first appears (`arrive`), and an optional `kick` that throws it to
//! a value and lets it spring back. The scene keeps one set of channels per word and springs
//! them toward the targets, so a view can set new targets every frame without jumps. Words
//! the view does not name spring back to [`Typo::REST`].
//!
//! The channels:
//! - `ink`: opacity factor, 1 is full ink.
//! - `weight`: how far each edge of the glyph grows, in logical pixels (negative thins). Drawn
//!   by dilating the glyph's coverage in the shader, so the advance does not change.
//! - `tracking`: the word's letters spread about its middle by this factor; 1 is normal.
//! - `baseline`: the word moves up by this many logical pixels.
//! - `fill`: the share of the word's letters, from the left, at full ink; letters past it are
//!   at `ink * FILL_FLOOR`, with a soft front one letter wide.
//!
//! Rule 1 of the motion language, the reading-line envelope, is enforced at draw time: a word
//! on the newest line draws with its values clamped by [`Envelope::READING`] (baseline within
//! 3 px, tracking within 5 %, ink at least 70 %), so no look can break it. Older lines are not
//! clamped. Under reduced motion every channel jumps to its target and kicks do nothing, so
//! the rest values (the state cue) still show.

use vo_core::channel::{Channel, Rest};
use vo_core::spring::Spring;
use vo_core::words::WordId;

/// Letters past a word's fill draw at this share of its ink.
pub const FILL_FLOOR: f32 = 0.35;

/// One word's typographic values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Typo {
    pub ink: f32,
    pub weight: f32,
    pub tracking: f32,
    pub baseline: f32,
    pub fill: f32,
}

impl Typo {
    /// Plain type: full ink, normal weight and spacing, on the baseline, filled.
    pub const REST: Typo = Typo { ink: 1.0, weight: 0.0, tracking: 1.0, baseline: 0.0, fill: 1.0 };

    pub fn ink(self, ink: f32) -> Typo {
        Typo { ink, ..self }
    }

    pub fn weight(self, weight: f32) -> Typo {
        Typo { weight, ..self }
    }

    pub fn tracking(self, tracking: f32) -> Typo {
        Typo { tracking, ..self }
    }

    pub fn baseline(self, baseline: f32) -> Typo {
        Typo { baseline, ..self }
    }

    pub fn fill(self, fill: f32) -> Typo {
        Typo { fill, ..self }
    }

    fn get(&self, c: usize) -> f32 {
        [self.ink, self.weight, self.tracking, self.baseline, self.fill][c]
    }

    fn from_array(a: [f32; CHANNELS]) -> Typo {
        Typo { ink: a[0], weight: a[1], tracking: a[2], baseline: a[3], fill: a[4] }
    }

    /// Opacity factor of letter `i` of `n` from `ink` and `fill`.
    pub fn letter_ink(&self, i: usize, n: usize) -> f32 {
        let ink = self.ink.clamp(0.0, 1.0);
        if self.fill >= 1.0 || n == 0 {
            return ink;
        }
        // The front sits at fill * n letters; the letter it crosses is part filled.
        let front = self.fill.max(0.0) * n as f32;
        let k = (front - i as f32).clamp(0.0, 1.0);
        ink * (FILL_FLOOR + (1.0 - FILL_FLOOR) * k)
    }
}

impl Default for Typo {
    fn default() -> Typo {
        Typo::REST
    }
}

const CHANNELS: usize = 5;

/// What a look asks of one word.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordTypo {
    /// Where the word rests. A change springs there.
    pub rest: Typo,
    /// Where a word starts when it first appears, before springing to `rest`. `None` starts it
    /// at `rest`.
    pub arrive: Option<Typo>,
    /// A throw: when the number changes, the word jumps to the values and springs back to
    /// `rest`. Use a counter the look bumps per event (an onset, a stable hypothesis).
    pub kick: Option<(u32, Typo)>,
    /// Springs for this word's channels, instead of the element's.
    pub motion: Option<TypoMotion>,
}

impl WordTypo {
    pub fn rest(rest: Typo) -> WordTypo {
        WordTypo { rest, arrive: None, kick: None, motion: None }
    }

    pub fn arrive(self, from: Typo) -> WordTypo {
        WordTypo { arrive: Some(from), ..self }
    }

    pub fn kick(self, n: u32, to: Typo) -> WordTypo {
        WordTypo { kick: Some((n, to)), ..self }
    }

    pub fn motion(self, m: TypoMotion) -> WordTypo {
        WordTypo { motion: Some(m), ..self }
    }
}

impl Default for WordTypo {
    fn default() -> WordTypo {
        WordTypo::rest(Typo::REST)
    }
}

/// Springs for the channels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TypoMotion {
    pub ink: Spring,
    pub weight: Spring,
    pub tracking: Spring,
    pub baseline: Spring,
    pub fill: Spring,
}

impl TypoMotion {
    /// One spring for every channel.
    pub const fn all(s: Spring) -> TypoMotion {
        TypoMotion { ink: s, weight: s, tracking: s, baseline: s, fill: s }
    }

    fn get(&self, c: usize) -> Spring {
        [self.ink, self.weight, self.tracking, self.baseline, self.fill][c]
    }
}

impl Default for TypoMotion {
    /// Ink 300 ms, weight 180 ms, tracking 260 ms, baseline 220 ms, fill 160 ms.
    fn default() -> TypoMotion {
        TypoMotion {
            ink: Spring::critical_to_98(0.30),
            weight: Spring::critical_to_98(0.18),
            tracking: Spring::critical_to_98(0.26),
            baseline: Spring::SOFT,
            fill: Spring::critical_to_98(0.16),
        }
    }
}

/// Limits on how far a word may draw from plain type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Envelope {
    /// Most baseline shift either way, in logical pixels.
    pub baseline: f32,
    /// Most tracking change either way, as a share (0.05 is 5 %).
    pub tracking: f32,
    /// Least ink.
    pub ink: f32,
    /// Most weight change either way, in logical pixels.
    pub weight: f32,
}

impl Envelope {
    /// Rule 1, on the newest line: baseline 3 px, tracking 5 %, ink 70 %, weight 0.6 px. Fill
    /// keeps every letter at least at the ink floor.
    pub const READING: Envelope = Envelope { baseline: 3.0, tracking: 0.05, ink: 0.7, weight: 0.6 };
    /// No limit.
    pub const FREE: Envelope =
        Envelope { baseline: f32::INFINITY, tracking: f32::INFINITY, ink: 0.0, weight: f32::INFINITY };

    /// `v` within the limits.
    pub fn clamp(&self, v: Typo) -> Typo {
        Typo {
            ink: v.ink.clamp(self.ink, 1.0),
            weight: v.weight.clamp(-self.weight, self.weight),
            tracking: v.tracking.clamp(1.0 - self.tracking, 1.0 + self.tracking),
            baseline: v.baseline.clamp(-self.baseline, self.baseline),
            // A letter past the fill must still reach the ink floor.
            fill: if self.ink > 0.0 && v.ink.clamp(self.ink, 1.0) * FILL_FLOOR < self.ink { 1.0 } else { v.fill },
        }
    }

    /// Whether `v` is within the limits (with a small tolerance).
    pub fn holds(&self, v: Typo) -> bool {
        let e = 1e-3;
        v.baseline.abs() <= self.baseline + e
            && (v.tracking - 1.0).abs() <= self.tracking + e
            && v.ink >= self.ink - e
            && v.weight.abs() <= self.weight + e
    }
}

/// The element's channels: every word's [`WordTypo`] and the default springs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Typos {
    pub words: Vec<(WordId, WordTypo)>,
    pub motion: TypoMotion,
    /// What a word that has no entry and is new arrives as (before springing to rest).
    pub arrive: Option<Typo>,
}

impl Typos {
    pub fn new() -> Typos {
        Typos::default()
    }

    /// Sets word `id`'s channels; a later call for the same id wins.
    pub fn word(mut self, id: WordId, w: WordTypo) -> Typos {
        self.set(id, w);
        self
    }

    pub fn set(&mut self, id: WordId, w: WordTypo) {
        match self.words.iter_mut().find(|(i, _)| *i == id) {
            Some(e) => e.1 = w,
            None => self.words.push((id, w)),
        }
    }

    pub fn motion(mut self, m: TypoMotion) -> Typos {
        self.motion = m;
        self
    }

    fn get(&self, id: WordId) -> Option<&WordTypo> {
        self.words.iter().find(|(i, _)| *i == id).map(|(_, w)| w)
    }
}

/// One word's channels.
struct WordChannels {
    id: WordId,
    ch: [Channel; CHANNELS],
    kick: Option<u32>,
}

/// The channels the scene keeps for one live text node.
#[derive(Default)]
pub(crate) struct TypoState {
    words: Vec<WordChannels>,
}

impl TypoState {
    /// Matches the channels to `words` (the node's word ids, in order) and `typos` at `t`.
    /// Returns whether any motion started.
    pub fn sync(&mut self, t: f64, ids: &[WordId], typos: Option<&Typos>, reduced: bool, retention: f64) -> bool {
        let Some(typos) = typos else {
            let had = !self.words.is_empty();
            self.words.clear();
            return had;
        };
        let mut changed = false;
        let mut next = Vec::with_capacity(ids.len());
        let rest = WordTypo::default();
        for &id in ids {
            let want = typos.get(id).copied().unwrap_or(WordTypo { arrive: typos.arrive, ..rest });
            let m = want.motion.unwrap_or(typos.motion);
            let old = self.words.iter().position(|w| w.id == id).map(|i| self.words.swap_remove(i));
            let mut w = match old {
                Some(w) => w,
                None => {
                    let from = if reduced { want.rest } else { want.arrive.unwrap_or(want.rest) };
                    let ch = std::array::from_fn(|c| {
                        let rest = if c == 3 || c == 1 { Rest::PIXELS } else { Rest::UNIT };
                        Channel::new(t, from.get(c) as f64, m.get(c)).with_rest(rest).with_retention(retention)
                    });
                    changed = true;
                    // A word that arrives with a kick takes it as already seen.
                    WordChannels { id, ch, kick: want.kick.map(|k| k.0) }
                }
            };
            let kick = want.kick.filter(|k| Some(k.0) != w.kick);
            if let Some((n, _)) = want.kick {
                w.kick = Some(n);
            }
            for c in 0..CHANNELS {
                let ch = &mut w.ch[c];
                let target = want.rest.get(c) as f64;
                if reduced {
                    if ch.target() != target || !ch.is_settled(t) {
                        ch.snap(t, target);
                        changed = true;
                    }
                    continue;
                }
                ch.set_spring(m.get(c));
                if let Some((_, to)) = kick {
                    let v = to.get(c) as f64;
                    if v != target {
                        ch.snap(t, v);
                        ch.retarget(t, target);
                        changed = true;
                        continue;
                    }
                }
                if ch.target() != target {
                    ch.retarget(t, target);
                    changed = true;
                }
            }
            next.push(w);
        }
        changed |= !self.words.is_empty();
        self.words = next;
        changed
    }

    /// Word `id`'s values at `t`, before any envelope; plain type for a word without channels.
    pub fn sample(&self, id: WordId, t: f64) -> Option<Typo> {
        let w = self.words.iter().find(|w| w.id == id)?;
        Some(Typo::from_array(std::array::from_fn(|c| w.ch[c].value(t) as f32)))
    }

    pub fn settled_at(&self) -> f64 {
        self.words.iter().flat_map(|w| w.ch.iter()).map(Channel::settled_at).fold(f64::NEG_INFINITY, f64::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrive_springs_to_rest() {
        let mut s = TypoState::default();
        let typos = Typos::new().word(1, WordTypo::rest(Typo::REST).arrive(Typo::REST.ink(0.5).weight(0.6)));
        assert!(s.sync(0.0, &[1], Some(&typos), false, 1.0));
        let a = s.sample(1, 0.0).unwrap();
        assert!((a.ink - 0.5).abs() < 1e-4 && (a.weight - 0.6).abs() < 1e-4);
        let b = s.sample(1, 0.1).unwrap();
        assert!(b.ink > 0.5 && b.weight < 0.6);
        let c = s.sample(1, 1.0).unwrap();
        assert!((c.ink - 1.0).abs() < 1e-3 && c.weight.abs() < 1e-3);
    }

    #[test]
    fn kick_fires_once_per_number() {
        let mut s = TypoState::default();
        let w = WordTypo::rest(Typo::REST);
        s.sync(0.0, &[7], Some(&Typos::new().word(7, w)), false, 1.0);
        let kicked = Typos::new().word(7, w.kick(1, Typo::REST.baseline(2.0)));
        assert!(s.sync(1.0, &[7], Some(&kicked), false, 1.0));
        assert!((s.sample(7, 1.0).unwrap().baseline - 2.0).abs() < 1e-4);
        // The same number again does not throw it again.
        s.sync(1.05, &[7], Some(&kicked), false, 1.0);
        assert!(s.sample(7, 1.05).unwrap().baseline < 2.0);
        assert!(s.sample(7, 2.0).unwrap().baseline.abs() < 1e-3);
    }

    #[test]
    fn reduced_motion_jumps_and_ignores_kicks() {
        let mut s = TypoState::default();
        let typos = Typos::new().word(
            1,
            WordTypo::rest(Typo::REST.ink(0.75)).arrive(Typo::REST.ink(0.2)).kick(3, Typo::REST.baseline(3.0)),
        );
        s.sync(0.0, &[1], Some(&typos), true, 1.0);
        let v = s.sample(1, 0.0).unwrap();
        assert_eq!(v.ink, 0.75);
        assert_eq!(v.baseline, 0.0);
    }

    #[test]
    fn words_without_entry_rest_and_removed_words_go() {
        let mut s = TypoState::default();
        let typos = Typos::new().word(1, WordTypo::rest(Typo::REST.baseline(-2.0)));
        s.sync(0.0, &[1, 2], Some(&typos), false, 1.0);
        assert_eq!(s.sample(2, 0.0), Some(Typo::REST));
        // Dropping the entry springs word 1 back.
        s.sync(0.5, &[1], Some(&Typos::new()), false, 1.0);
        assert!(s.sample(1, 2.0).unwrap().baseline.abs() < 1e-3);
        assert!(s.sample(2, 2.0).is_none());
    }

    #[test]
    fn envelope_clamps_the_reading_line() {
        let wild = Typo { ink: 0.2, weight: 2.0, tracking: 1.3, baseline: -8.0, fill: 0.0 };
        let c = Envelope::READING.clamp(wild);
        assert!(Envelope::READING.holds(c));
        assert_eq!(c.baseline, -3.0);
        assert_eq!(c.tracking, 1.05);
        assert_eq!(c.ink, 0.7);
        // An unfilled letter would fall under the ink floor, so the fill is dropped.
        assert_eq!(c.fill, 1.0);
        assert_eq!(Envelope::FREE.clamp(wild), wild);
    }

    #[test]
    fn letter_ink_fills_left_to_right() {
        let v = Typo::REST.fill(0.5);
        assert_eq!(v.letter_ink(0, 4), 1.0);
        assert_eq!(v.letter_ink(1, 4), 1.0);
        assert!((v.letter_ink(3, 4) - FILL_FLOOR).abs() < 1e-6);
        assert_eq!(Typo::REST.letter_ink(3, 4), 1.0);
    }
}
