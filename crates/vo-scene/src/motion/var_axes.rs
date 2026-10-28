//! Variable font breathing (MO-81): a word's variable font axes from how it was said.
//!
//! Loudness sets the weight, tempo the width and pitch the slant; the optical size follows
//! the text size. [`Axes::from_voice`] maps a word's reading over the font's full ranges, for
//! a specimen. [`Axes::reading`] clamps it to what the reading line allows (rule 1 of the
//! motion language): a weight that keeps the ink close to the regular's, a width within
//! 5 %, and a slant of a few degrees, which is a shear of the outline and never a rotation
//! of the word. [`Axes::quantized`] rounds to steps, so a breathing word reuses a small set
//! of instances (`FontRegistry::add_instance`) instead of making one per frame.

/// A variable font location in user units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axes {
    pub wght: f32,
    pub wdth: f32,
    pub opsz: f32,
    pub slnt: f32,
}

/// What the reading line allows: weight, width and slant ranges.
pub const READING_WGHT: (f32, f32) = (340.0, 620.0);
pub const READING_WDTH: (f32, f32) = (95.0, 105.0);
pub const READING_SLNT: (f32, f32) = (-5.0, 0.0);

/// A word said at this many seconds per letter has normal width.
pub const NORMAL_SECS_PER_LETTER: f32 = 0.07;

impl Axes {
    /// The regular: weight 400, width 100, optical size 14, upright.
    pub const REST: Axes = Axes { wght: 400.0, wdth: 100.0, opsz: 14.0, slnt: 0.0 };

    /// The axes for a word said at `level` (0 to 1, as `Sig::Level`), at `secs_per_letter`,
    /// `pitch` semitones above the speaker's running pitch, set at `size` pixels. Full
    /// ranges: weight 300 to 900, width 75 (slow) to 125 (fast), slant 0 to -10.
    pub fn from_voice(level: f32, secs_per_letter: f32, pitch: f32, size: f32) -> Axes {
        let wght = 300.0 + 600.0 * level.clamp(0.0, 1.0).powf(0.8);
        let tempo = if secs_per_letter > 0.0 {
            ((NORMAL_SECS_PER_LETTER - secs_per_letter) / 0.04).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let wdth = 100.0 + 25.0 * tempo;
        let slnt = -10.0 * (pitch / 5.0).clamp(0.0, 1.0);
        Axes { wght, wdth, opsz: optical(size), slnt }
    }

    /// The same axes kept within the reading-line envelope.
    pub fn reading(self) -> Axes {
        Axes {
            wght: self.wght.clamp(READING_WGHT.0, READING_WGHT.1),
            wdth: self.wdth.clamp(READING_WDTH.0, READING_WDTH.1),
            opsz: self.opsz,
            slnt: self.slnt.clamp(READING_SLNT.0, READING_SLNT.1),
        }
    }

    /// Whether the axes are inside the reading-line envelope.
    pub fn holds(&self) -> bool {
        let inside = |v: f32, (lo, hi): (f32, f32)| v >= lo - 1e-3 && v <= hi + 1e-3;
        inside(self.wght, READING_WGHT) && inside(self.wdth, READING_WDTH) && inside(self.slnt, READING_SLNT)
    }

    /// Rounded to steps of 20 weight, 1 width, 2 optical size and 1 degree of slant.
    pub fn quantized(self) -> Axes {
        let q = |v: f32, step: f32| (v / step).round() * step;
        Axes { wght: q(self.wght, 20.0), wdth: q(self.wdth, 1.0), opsz: q(self.opsz, 2.0), slnt: q(self.slnt, 1.0) }
    }

    /// `self` moved toward `other` by `k` (0 keeps `self`).
    pub fn mix(self, other: Axes, k: f32) -> Axes {
        let m = |a: f32, b: f32| a + (b - a) * k;
        Axes {
            wght: m(self.wght, other.wght),
            wdth: m(self.wdth, other.wdth),
            opsz: m(self.opsz, other.opsz),
            slnt: m(self.slnt, other.slnt),
        }
    }

    /// The settings for `FontRegistry::add_instance`.
    pub fn settings(&self) -> [([u8; 4], f32); 4] {
        [(*b"wght", self.wght), (*b"wdth", self.wdth), (*b"opsz", self.opsz), (*b"slnt", self.slnt)]
    }
}

/// The optical size for text set at `size` pixels: points at 96 dpi, within 8 to 72.
pub fn optical(size: f32) -> f32 {
    (size * 0.75).clamp(8.0, 72.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loud_fast_high_is_heavy_wide_and_slanted() {
        let soft = Axes::from_voice(0.1, 0.12, 0.0, 20.0);
        let loud = Axes::from_voice(0.95, 0.03, 6.0, 20.0);
        assert!(loud.wght > 800.0 && soft.wght < 400.0, "{soft:?} {loud:?}");
        assert_eq!((soft.wdth, loud.wdth), (75.0, 125.0));
        assert_eq!((soft.slnt, loud.slnt), (0.0, -10.0));
        assert_eq!(loud.opsz, 15.0);
    }

    #[test]
    fn the_reading_form_holds_the_envelope() {
        for level in [0.0, 0.3, 0.7, 1.0] {
            for spl in [0.0, 0.02, 0.07, 0.2] {
                for pitch in [-5.0, 0.0, 3.0, 12.0] {
                    let a = Axes::from_voice(level, spl, pitch, 20.0).reading();
                    assert!(a.holds(), "{a:?}");
                    assert!(a.quantized().holds() || a.quantized().reading().holds());
                }
            }
        }
        assert!(!Axes::from_voice(1.0, 0.01, 9.0, 20.0).holds());
        assert!(Axes::REST.holds());
    }

    #[test]
    fn quantized_steps_and_settings() {
        let a = Axes { wght: 531.0, wdth: 102.4, opsz: 15.2, slnt: -3.6 }.quantized();
        assert_eq!(a, Axes { wght: 540.0, wdth: 102.0, opsz: 16.0, slnt: -4.0 });
        assert_eq!(a.settings()[0], (*b"wght", 540.0));
        assert_eq!(Axes::REST.mix(a, 0.0), Axes::REST);
        assert_eq!(Axes::REST.mix(a, 1.0), a);
    }
}
