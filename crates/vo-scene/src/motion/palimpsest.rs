//! Palimpsest correction (MO-20): the default revision of live text.
//!
//! A revised word is diffed by grapheme against its old text: the shared start and end are
//! kept, and what lies between is replaced (`diff_ends`). Letters that survive keep their
//! glyph, ink and place, so the letters before the change never move and those after it
//! only follow the width change; none dips or fades. Removed letters stay where they
//! were, sink `SINK_PX` below the baseline and fade to a `GHOST_INK` ghost that is
//! readable for `GHOST_HOLD`, then dissolve. New letters arrive in place at `ARRIVE_INK`
//! or more and fill in, with no travel, so the reading line keeps its envelope (rule 1:
//! no scale, ink at least 70 %).
//!
//! Shaping can break the one glyph per letter picture. A kept letter whose glyph changed
//! (a ligature that forms or splits, a contextual form) crossfades in place, and so does
//! a spare glyph of a kept letter. A word in a joined script (Arabic, Syriac, N'Ko,
//! Mongolian, the Indic scripts) does not sink its removed letters: they crossfade in
//! place, since a ghost cut out of a joined word reads as a broken word. A word that keeps
//! no letter at all crossfades in place too: a whole ghost word under it is noise.
//!
//! Under reduced motion the ghost does not sink; it still fades to `GHOST_INK`, holds and
//! dissolves, so the reader can still see what changed.

use super::*;

/// How far a removed letter sinks below the baseline, in pixels.
pub const SINK_PX: f32 = 4.0;
/// The opacity a removed letter sinks to.
pub const GHOST_INK: f32 = 0.2;
/// How long the ghost stays readable from the revision, in seconds.
pub const GHOST_HOLD: f64 = 0.5;
/// A new letter's opacity in the frame it arrives.
pub const ARRIVE_INK: f32 = 0.75;
/// The sink and fade to the ghost take about 80 ms.
pub const SINK: Spring = Spring::critical_to_98(0.080);
/// The ghost dissolves in about 300 ms after its hold.
pub const DISSOLVE: Spring = Spring::critical_to_98(0.300);
/// A new letter fills to full ink in about 120 ms.
pub const ARRIVE: Spring = Spring::critical_to_98(0.120);

/// Diffs two words by their shared ends: the op for each new grapheme. The common prefix
/// and the common suffix are kept and everything between is replaced, so the change reads
/// as one place in the word and the letters before it never move ("there" to "their"
/// keeps "the" and replaces "re" with "ir"). A case change counts as a new letter.
pub fn diff_ends(old: &[&str], new: &[&str]) -> Vec<GraphemeOp> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let room = old.len().min(new.len()) - prefix;
    let suffix = old.iter().rev().zip(new.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
    let (n, m) = (old.len(), new.len());
    (0..m)
        .map(|j| {
            if j < prefix {
                GraphemeOp::Kept(j)
            } else if j >= m - suffix {
                GraphemeOp::Kept(j + n - m)
            } else {
                GraphemeOp::Enter
            }
        })
        .collect()
}

/// Whether `text` has a letter of a script whose letters join, where a single letter cut
/// out of the word changes the shape of its neighbours.
pub fn joined_script(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(c as u32,
            0x0600..=0x08FF      // Arabic, Syriac, Arabic supplement, Thaana, N'Ko, Samaritan, Mandaic
            | 0x0900..=0x0DFF    // Devanagari to Sinhala
            | 0x1800..=0x18AF    // Mongolian
            | 0xA840..=0xA87F    // Phags-pa
            | 0xFB50..=0xFDFF    // Arabic presentation forms A
            | 0xFE70..=0xFEFF    // Arabic presentation forms B
        )
    })
}

impl Flow {
    /// MO-20: the palimpsest revision of word `o` to `w`, placed at `p`.
    pub(super) fn palimpsest(
        &mut self,
        o: &mut FlowWord,
        w: &Word,
        p: &PlacedWord,
        graphemes: &[u32],
        make: &Make,
        reduced: bool,
    ) {
        let t = make.t;
        let old_g: Vec<&str> = o.text.graphemes(true).collect();
        let new_g: Vec<&str> = w.text.graphemes(true).collect();
        let ops = diff_ends(&old_g, &new_g);
        let joined = joined_script(&o.text) || joined_script(&w.text);
        // A word with nothing in common is a new word: it crossfades, with no ghost under it.
        let shared = ops.iter().any(|op| matches!(op, GraphemeOp::Kept(_)));
        let mut kept_old = vec![false; old_g.len()];
        for op in &ops {
            if let GraphemeOp::Kept(i) = op {
                kept_old[*i] = true;
            }
        }
        let s = stretch(o, t);
        let word = o.id;
        let mut old_glyphs: Vec<Option<FlowGlyph>> = std::mem::take(&mut o.glyphs).into_iter().map(Some).collect();
        let mut glyphs = Vec::with_capacity(p.glyphs.len());
        for (n, &gr) in p.glyphs.iter().zip(graphemes) {
            let op = ops.get(gr as usize).copied().unwrap_or(GraphemeOp::Enter);
            let claimed = match op {
                GraphemeOp::Kept(from) => old_glyphs
                    .iter_mut()
                    .find(|g| g.as_ref().is_some_and(|g| g.grapheme as usize == from))
                    .and_then(Option::take),
                _ => None,
            };
            let g = match claimed {
                Some(mut g) if g.glyph == n.glyph && g.font == n.font => {
                    // A survivor: it keeps its ink and only follows the layout.
                    let x = g.x.value(t) as f32 * s;
                    g.x = if reduced { make.ch(n.x, FLIP, Rest::PIXELS) } else { make.to(x, n.x, FLIP, Rest::PIXELS) };
                    FlowGlyph { grapheme: gr, advance: n.advance, y: n.y, ..g }
                }
                Some(g) => {
                    // Same letter, another shape: a local crossfade.
                    self.palimpsest_ghost(word, &g, s, make, false, false);
                    make.enter(n, gr, (0.0, 0.0, 1.0), CROSSFADE)
                }
                None => make.enter(n, gr, (ARRIVE_INK, 0.0, 1.0), ARRIVE),
            };
            glyphs.push(g);
        }
        for g in old_glyphs.into_iter().flatten() {
            let removed = !kept_old.get(g.grapheme as usize).copied().unwrap_or(false);
            let sink = removed && !joined && shared;
            // Reduced motion: the ghost holds in place instead of sinking.
            self.palimpsest_ghost(word, &g, s, make, sink && !reduced, sink);
        }
        o.glyphs = glyphs;
        o.spread = false;
    }

    /// Leaves old glyph `g` of word `word` as a ghost where it is drawn now. It sinks
    /// `SINK_PX` with `sink`, and with `hold` it fades to `GHOST_INK` and holds before it
    /// dissolves; otherwise it fades out in place.
    fn palimpsest_ghost(&mut self, word: WordId, g: &FlowGlyph, s: f32, make: &Make, sink: bool, hold: bool) {
        let t = make.t;
        let k = g.opacity.value(t) as f32;
        if k <= 0.0 {
            return;
        }
        let rise = g.rise.value(t) as f32;
        let spring = if sink { SINK } else { CROSSFADE };
        self.ghosts.push(Ghost {
            glyph: g.glyph,
            font: g.font,
            word,
            advance: g.advance,
            fixed: None,
            x: g.x.value(t) as f32 * s,
            y: g.y,
            width: 0.0,
            opacity: if hold { ghost_ink(k, t, make) } else { make.to(k, 0.0, CROSSFADE, Rest::UNIT) },
            rise: make.to(rise, if sink { rise + SINK_PX } else { rise }, spring, Rest::PIXELS),
            scale: make.ch(1.0, GLYPH_POP, Rest::UNIT),
            slide: make.ch(0.0, GLYPH_ROLL, Rest::PIXELS),
        });
    }
}

/// A removed letter's opacity: from `from` to `GHOST_INK` (never above `from`), held until
/// `GHOST_HOLD` after `t`, then dissolved.
fn ghost_ink(from: f32, t: f64, make: &Make) -> Channel {
    let mut c = make.to(from, GHOST_INK.min(from), SINK, Rest::UNIT);
    c.set_spring(DISSOLVE);
    c.retarget(t + GHOST_HOLD, 0.0);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(s: &str) -> Vec<&str> {
        s.graphemes(true).collect()
    }

    #[test]
    fn the_change_is_one_place_between_the_shared_ends() {
        use GraphemeOp::*;
        assert_eq!(diff_ends(&g("there"), &g("their")), vec![Kept(0), Kept(1), Kept(2), Enter, Enter]);
        assert_eq!(diff_ends(&g("cream"), &g("cream,")), vec![Kept(0), Kept(1), Kept(2), Kept(3), Kept(4), Enter]);
        assert_eq!(diff_ends(&g("scream"), &g("cream")), vec![Kept(1), Kept(2), Kept(3), Kept(4), Kept(5)]);
        assert_eq!(diff_ends(&g("ice"), &g("Ice")), vec![Enter, Kept(1), Kept(2)]);
        // A repeated letter is not kept twice.
        assert_eq!(diff_ends(&g("aa"), &g("a")), vec![Kept(0)]);
    }

    #[test]
    fn joined_scripts_are_found() {
        assert!(joined_script("\u{0645}\u{0631}\u{062d}\u{0628}\u{0627}"));
        assert!(joined_script("\u{0928}\u{092e}\u{0938}\u{094d}\u{0924}\u{0947}"));
        assert!(!joined_script("their"));
        assert!(!joined_script("caf\u{e9}"));
    }

    #[test]
    fn the_ghost_holds_then_dissolves() {
        let make = Make { t: 1.0, retention: f64::INFINITY, size: 20.0 };
        let c = ghost_ink(1.0, 1.0, &make);
        assert!((c.value(1.0 + 0.3) - GHOST_INK as f64).abs() < 0.01);
        assert!((c.value(1.0 + GHOST_HOLD - 0.01) - GHOST_INK as f64).abs() < 0.01);
        assert!(c.value(1.0 + GHOST_HOLD + 0.5) < 0.005);
    }
}
