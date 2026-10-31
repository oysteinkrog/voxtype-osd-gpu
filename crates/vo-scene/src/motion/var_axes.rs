//! Variable font breathing (MO-81): a word's variable font axes from how it was said.
//!
//! Loudness sets the weight, tempo the width and pitch the slant; the optical size follows
//! the text size. [`Axes::from_voice`] maps a word's reading over the font's full ranges, for
//! a specimen. [`Axes::reading`] clamps it to what the reading line allows (rule 1 of the
//! motion language): a weight that keeps the ink close to the regular's, a width within
//! 5 %, and a slant of a few degrees, which is a shear of the outline and never a rotation
//! of the word. [`Axes::quantized`] rounds to steps, so a breathing word reuses a small set
//! of instances (`FontRegistry::add_instance`) instead of making one per frame.
//!
//! [`breathe`] keeps those instances and makes the axes smooth on top of the nearest one
//! (EG-23): the weight left over becomes edge growth, measured so a whole step grows the
//! word's ink as much as the next instance has; the width left over moves the advances that
//! share of the way to the next width instance and stretches the letters to match; the slant
//! left over is a lean. A slow loudness ramp then thickens the word frame by frame instead of
//! in jumps of 20 weight.

use crate::font::{FontId, FontRegistry, GlyphInk};

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

/// One step of each axis in `Axes::quantized`.
pub const STEP: Axes = Axes { wght: 20.0, wdth: 1.0, opsz: 2.0, slnt: 1.0 };

/// Ink the glyph shader adds per pixel of weight, as a share of the outline length: its
/// distance to a curve runs a little long, so a curved edge grows a little less than the
/// weight. Measured on Roboto Flex at 40 and 80 px (vo-render `tests/gpu_var_axes.rs`).
pub const EDGE_GROWTH: f32 = 0.98;

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

    /// Rounded to steps of 20 weight, 1 width, 2 optical size and 1 degree of slant (`STEP`).
    pub fn quantized(self) -> Axes {
        let q = |v: f32, step: f32| (v / step).round() * step;
        Axes {
            wght: q(self.wght, STEP.wght),
            wdth: q(self.wdth, STEP.wdth),
            opsz: q(self.opsz, STEP.opsz),
            slnt: q(self.slnt, STEP.slnt),
        }
    }

    /// What `quantized` leaves out: each axis within half a step of zero.
    pub fn remainder(self) -> Axes {
        let q = self.quantized();
        Axes { wght: self.wght - q.wght, wdth: self.wdth - q.wdth, opsz: self.opsz - q.opsz, slnt: self.slnt - q.slnt }
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

/// A word's axes drawn as its nearest quantised instance plus what lies between that and the
/// axes (EG-23), from [`breathe`]. Live text takes it through `Element::word_breath`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Breath {
    /// The nearest quantised instance: the word's glyphs and, apart from `toward`, its advances.
    pub font: FontId,
    /// The instance one weight step and the one one width step toward the axes, and the share
    /// of the way there (0 to 0.5) that the word's advances take, so they move smoothly too.
    pub toward: [Option<(FontId, f32)>; 2],
    /// Edge growth in ems for the weight between the instance and the axes, or thinning when
    /// negative (`GlyphFx::weight` at the text size).
    pub weight: f32,
    /// Horizontal scale less one for the width between them (`GlyphXform::stretch`).
    pub stretch: f32,
    /// Lean in degrees for the slant between them (`GlyphXform::skew_deg`).
    pub skew_deg: f32,
}

impl Breath {
    /// The word in `font` with nothing on top: what a quantised instance alone draws.
    pub fn plain(font: FontId) -> Breath {
        Breath { font, toward: [None; 2], weight: 0.0, stretch: 0.0, skew_deg: 0.0 }
    }
}

/// The breath of `text` in variable font `base` at `axes`. The instances it uses are the
/// quantised ones (`Axes::quantized`): the nearest, and the next one along the weight and the
/// width when the axes lie between. The edge growth and the stretch are measured on this
/// word's own letters (`FontRegistry::glyph_ink`, cached), so at half a step either side of a
/// switch the two instances draw the same ink and the same width. Text the font has no
/// letters for gets the plain instance. `None` when `base` is not a font.
pub fn breathe(fonts: &FontRegistry, base: FontId, text: &str, axes: Axes) -> Option<Breath> {
    let q = axes.quantized();
    let r = axes.remainder();
    let font = fonts.add_instance(base, &q.settings())?;
    let face = fonts.get(font)?;
    let map = {
        use skrifa::MetadataProvider;
        face.skrifa().charmap()
    };
    let glyphs: Vec<u32> =
        text.chars().filter(|c| !c.is_whitespace()).filter_map(|c| map.map(c)).map(|g| g.to_u32()).collect();
    let ink = |id: FontId| {
        glyphs.iter().filter_map(|&g| fonts.glyph_ink(id, g)).fold(GlyphInk::default(), |a, b| GlyphInk {
            area: a.area + b.area,
            perimeter: a.perimeter + b.perimeter,
            advance: a.advance + b.advance,
        })
    };
    // The next instance along one axis, toward the axes; `None` on a step.
    let next = |share: f32, side: Axes| (share > 1e-4).then(|| fonts.add_instance(base, &side.settings())).flatten();
    let mut b = Breath { skew_deg: -r.slnt, ..Breath::plain(font) };
    if glyphs.is_empty() {
        return Some(b);
    }
    let here = ink(font);
    let share = r.wght.abs() / STEP.wght;
    if let Some(id) = next(share, Axes { wght: q.wght + STEP.wght * r.wght.signum(), ..q }) {
        b.toward[0] = Some((id, share));
        if here.perimeter > 0.0 {
            b.weight = share * (ink(id).area - here.area) / (EDGE_GROWTH * here.perimeter);
        }
    }
    let share = r.wdth.abs() / STEP.wdth;
    if let Some(id) = next(share, Axes { wdth: q.wdth + STEP.wdth * r.wdth.signum(), ..q }) {
        b.toward[1] = Some((id, share));
        if here.advance > 0.0 {
            b.stretch = share * (ink(id).advance / here.advance - 1.0);
        }
    }
    Some(b)
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
    fn the_remainder_is_within_half_a_step_and_adds_back() {
        for (w, d, s) in [(531.0, 102.4, -3.6), (409.9, 95.5, -0.49), (600.0, 100.0, 0.0), (590.1, 104.51, -4.5)] {
            let a = Axes { wght: w, wdth: d, opsz: 14.0, slnt: s };
            let (q, r) = (a.quantized(), a.remainder());
            assert!(r.wght.abs() <= 10.0 + 1e-3 && r.wdth.abs() <= 0.5 + 1e-3 && r.slnt.abs() <= 0.5 + 1e-3, "{r:?}");
            assert!((q.wght + r.wght - w).abs() < 1e-3 && (q.wdth + r.wdth - d).abs() < 1e-3);
            assert!((q.slnt + r.slnt - s).abs() < 1e-3);
        }
        let r = Axes { wght: 531.0, wdth: 102.4, opsz: 14.0, slnt: -3.6 }.remainder();
        assert!((r.wght + 9.0).abs() < 1e-3 && (r.wdth - 0.4).abs() < 1e-3 && (r.slnt - 0.4).abs() < 1e-3, "{r:?}");
    }

    fn flex() -> (FontRegistry, FontId) {
        let fonts = FontRegistry::new();
        let id = fonts.asset("flex").unwrap();
        (fonts, id)
    }

    #[test]
    fn on_a_step_the_breath_is_the_plain_instance() {
        let (fonts, base) = flex();
        let a = Axes { wght: 440.0, wdth: 101.0, opsz: 14.0, slnt: -2.0 };
        let b = breathe(&fonts, base, "breathe", a).unwrap();
        assert_eq!(b, Breath::plain(fonts.add_instance(base, &a.settings()).unwrap()));
        assert!(breathe(&fonts, 999, "x", a).is_none());
    }

    /// Half a step past one instance and half a step short of the next draw the same ink and
    /// the same width: the calibration that makes the switch between them invisible.
    #[test]
    fn half_a_step_either_side_of_a_switch_meets() {
        let (fonts, base) = flex();
        let word = "breathing";
        let at = |a: Axes| breathe(&fonts, base, word, a).unwrap();
        let glyphs = |font: FontId| {
            use skrifa::MetadataProvider;
            let face = fonts.get(font).unwrap();
            let map = face.skrifa().charmap();
            word.chars()
                .filter_map(|c| map.map(c))
                .map(|g| fonts.glyph_ink(font, g.to_u32()).unwrap())
                .collect::<Vec<_>>()
        };
        // Ink grown by `weight` ems on every edge, and advance stretched.
        let drawn = |b: Breath| {
            let g = glyphs(b.font);
            let area: f32 = g.iter().map(|g| g.area + g.perimeter * EDGE_GROWTH * b.weight).sum();
            let width: f32 = g.iter().map(|g| g.advance).sum::<f32>() * (1.0 + b.stretch);
            (area, width)
        };
        let rest = Axes { wght: 400.0, wdth: 100.0, opsz: 14.0, slnt: 0.0 };
        let (lo, hi) = (at(Axes { wght: 509.999, ..rest }), at(Axes { wght: 510.0, ..rest }));
        assert_ne!(lo.font, hi.font);
        assert!(lo.weight > 0.0 && hi.weight < 0.0, "{lo:?} {hi:?}");
        let ((a0, _), (a1, _)) = (drawn(lo), drawn(hi));
        assert!((a0 - a1).abs() < 1e-3 * a0, "{a0} {a1}");
        let (lo, hi) = (at(Axes { wdth: 102.4999, ..rest }), at(Axes { wdth: 102.5, ..rest }));
        assert_ne!(lo.font, hi.font);
        assert!(lo.stretch > 0.0 && hi.stretch < 0.0, "{lo:?} {hi:?}");
        let ((_, w0), (_, w1)) = (drawn(lo), drawn(hi));
        assert!((w0 - w1).abs() < 1e-3 * w0, "{w0} {w1}");
        assert_eq!(lo.toward[1].unwrap().0, hi.font);
        assert_eq!(hi.toward[1].unwrap().0, lo.font);
        let s = at(Axes { slnt: -3.3, ..rest });
        assert!((s.skew_deg - 0.3).abs() < 1e-3, "{s:?}");
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
