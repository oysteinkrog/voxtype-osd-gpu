//! Particle words for live text (PA03): [`ParticleText`] keeps one particle slot per live
//! word and feeds it from the glyphs a scene samples, so an app with a `LiveText` element needs
//! no shaping of its own.
//!
//! Use, with `p` a [`Particles`]:
//!
//! 1. When the words change (in `update`, at the event's time `t`), sample the scene at `t` and
//!    call [`ParticleText::update`] with its glyphs. Words are matched by `GlyphInstance::word`;
//!    a new id forms from particles, a changed glyph run scatters and re-forms, a word that went
//!    scatters away.
//! 2. Every frame, after sampling the display list at the frame time, call
//!    [`ParticleText::apply`]: it fades each live glyph by its hand-over opacity and returns
//!    the slot looks for [`Particles::prepare`], with each word's origin taken from where the
//!    frame drew it, so word motion carries its particles.
//!
//! Glyphs are taken in display-list order, which is the order the word was shaped in.

use vo_scene::display::{GlyphInstance, NO_WORD};
use vo_scene::geom::{Color, Point};

use crate::particles::{Particles, SlotLook, SpriteStyle, WordGlyph};

/// One word's glyphs as a display list holds them.
#[derive(Clone, Debug, PartialEq)]
pub struct WordRun {
    pub word: u32,
    /// The first glyph's pen position in layer space.
    pub origin: Point,
    /// Relative to `origin`.
    pub glyphs: Vec<WordGlyph>,
}

/// Groups the live-text glyphs of `glyphs` by word id, in order of first appearance.
pub fn word_runs(glyphs: &[GlyphInstance]) -> Vec<WordRun> {
    let mut runs: Vec<WordRun> = Vec::new();
    for g in glyphs.iter().filter(|g| g.word != NO_WORD) {
        let run = match runs.iter_mut().position(|r| r.word == g.word) {
            Some(i) => &mut runs[i],
            None => {
                runs.push(WordRun { word: g.word, origin: g.pos, glyphs: Vec::new() });
                runs.last_mut().unwrap()
            }
        };
        let pos = Point::new(g.pos.x - run.origin.x, g.pos.y - run.origin.y);
        run.glyphs.push(WordGlyph { font: g.font, glyph: g.glyph, size: g.size, pos });
    }
    runs
}

#[derive(Clone, Debug, Default)]
struct Held {
    word: Option<u32>,
    glyphs: Vec<WordGlyph>,
    /// Where the word was at its last update, for the scatter of a word that went.
    origin: Point,
    /// When the slot was emptied, for reuse after its particles have faded.
    freed_at: f64,
}

/// One particle slot per live word.
pub struct ParticleText {
    slots: Vec<Held>,
    /// Particle density: pixels per logical unit.
    pub px_per_unit: f32,
    /// Colour of forming particles.
    pub color: Color,
    pub size: f32,
    pub style: SpriteStyle,
    /// Pixels² of ink per particle (`Particles::set_word_dots`).
    pub dot_area: f32,
}

/// A freed slot is reused only after its scatter has faded.
const REUSE_AFTER: f64 = 0.2;

impl ParticleText {
    /// Uses slots `0..slots` of the [`Particles`] it is given.
    pub fn new(slots: u32, px_per_unit: f32, color: Color) -> ParticleText {
        ParticleText {
            slots: vec![Held { freed_at: f64::NEG_INFINITY, ..Default::default() }; slots as usize],
            px_per_unit,
            color,
            size: 0.0,
            style: SpriteStyle::Soft,
            dot_area: crate::particles::DOT_AREA,
        }
    }

    /// The slot that holds `word`.
    pub fn slot_of(&self, word: u32) -> Option<u32> {
        self.slots.iter().position(|h| h.word == Some(word)).map(|i| i as u32)
    }

    /// Applies the words in `glyphs` (the scene sampled at `t`) at time `t`. Words that did
    /// not change cost nothing. When every slot is busy, a new word shows as solid text with
    /// no particles.
    pub fn update(&mut self, p: &mut Particles, t: f64, glyphs: &[GlyphInstance]) {
        let runs = word_runs(glyphs);
        let gone: Vec<u32> =
            self.slots.iter().filter_map(|h| h.word).filter(|w| !runs.iter().any(|r| r.word == *w)).collect();
        for w in gone {
            self.remove(p, w, t);
        }
        for run in &runs {
            self.set(p, t, run);
        }
    }

    /// Sets one word at `t`: a new word forms, a changed one re-forms its changed glyphs, an
    /// unchanged one only moves. Returns false when no slot was free.
    pub fn set(&mut self, p: &mut Particles, t: f64, run: &WordRun) -> bool {
        let slot = match self.slot_of(run.word) {
            Some(s) => s,
            None => match self.slots.iter().position(|h| h.word.is_none() && t - h.freed_at >= REUSE_AFTER) {
                Some(s) => s as u32,
                None => return false,
            },
        };
        let h = &mut self.slots[slot as usize];
        if h.word == Some(run.word) && h.glyphs == run.glyphs {
            h.origin = run.origin;
            return true;
        }
        p.set_word_dots(slot, t, &run.glyphs, self.px_per_unit, self.dot_area);
        *h = Held { word: Some(run.word), glyphs: run.glyphs.clone(), origin: run.origin, freed_at: f64::NEG_INFINITY };
        true
    }

    /// Scatters `word` away at `t` and frees its slot.
    pub fn remove(&mut self, p: &mut Particles, word: u32, t: f64) {
        if let Some(i) = self.slot_of(word) {
            p.set_word(i, t, &[], self.px_per_unit);
            let h = &mut self.slots[i as usize];
            *h = Held { word: None, glyphs: Vec::new(), origin: h.origin, freed_at: t };
        }
    }

    /// Forgets every word, for a seek back. Call [`Particles::reset_slots`] with it.
    pub fn clear(&mut self) {
        for h in &mut self.slots {
            *h = Held { freed_at: f64::NEG_INFINITY, ..Default::default() };
        }
    }

    /// Slots whose word went less than the reuse time before `t`: their scatter still shows,
    /// at the word's last place.
    pub fn fading(&self, t: f64) -> impl Iterator<Item = (u32, Point)> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter(move |(_, h)| h.word.is_none() && t - h.freed_at < REUSE_AFTER)
            .map(|(i, h)| (i as u32, h.origin))
    }

    /// Fades each live glyph in `glyphs` (the frame's display list) by its hand-over opacity
    /// at `t` and returns the slot looks for [`Particles::prepare`].
    pub fn apply(&self, p: &Particles, t: f64, glyphs: &mut [GlyphInstance]) -> Vec<SlotLook> {
        let mut looks = vec![SlotLook { visible: false, ..SlotLook::new(Point::ZERO, self.color) }; self.slots.len()];
        let mut seen: Vec<(u32, usize)> = Vec::new();
        for g in glyphs.iter_mut().filter(|g| g.word != NO_WORD) {
            let Some(slot) = self.slot_of(g.word) else { continue };
            let index = match seen.iter_mut().find(|(w, _)| *w == g.word) {
                Some((_, n)) => {
                    *n += 1;
                    *n
                }
                None => {
                    seen.push((g.word, 0));
                    looks[slot as usize] =
                        SlotLook { size: self.size, style: self.style, ..SlotLook::new(g.pos, self.color) };
                    0
                }
            };
            g.color.a *= p.glyph_alpha(slot, index, t);
        }
        // Slots whose word has gone keep drawing their scatter at its last place.
        for (i, origin) in self.fading(t) {
            looks[i as usize] = SlotLook { size: self.size, style: self.style, ..SlotLook::new(origin, self.color) };
        }
        looks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(word: u32, glyph: u32, x: f32) -> GlyphInstance {
        GlyphInstance { pos: Point::new(x, 20.0), size: 16.0, font: 0, glyph, word, color: Color::WHITE }
    }

    #[test]
    fn runs_group_by_word_relative_to_the_first_glyph() {
        let runs = word_runs(&[g(3, 10, 5.0), g(3, 11, 14.0), g(NO_WORD, 1, 0.0), g(7, 12, 40.0)]);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].word, 3);
        assert_eq!(runs[0].origin, Point::new(5.0, 20.0));
        assert_eq!(runs[0].glyphs[1].pos, Point::new(9.0, 0.0));
        assert_eq!(runs[1].glyphs.len(), 1);
    }
}
