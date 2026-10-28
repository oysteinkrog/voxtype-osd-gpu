//! The margin bell (MO-31): as the newest line nears the right edge of the text box, its
//! letters tighten a little (at most 5 %, within the reading-line envelope), so the reader
//! feels the wrap coming. When the text wraps, the line that was full relaxes back to normal
//! spacing while the new line starts at normal spacing. With a carriage return, the new line
//! also arrives a few pixels to the right and settles with a small bounce, as a typewriter
//! carriage does.
//!
//! Tightening moves each letter toward the start of its line in proportion to how far along
//! it is, so the line keeps its start and its end comes in. Under reduced motion there is no
//! tightening and no carriage return.

use vo_core::channel::{Channel, Rest};
use vo_core::spring::Spring;
use vo_core::words::WordId;

use super::{Flow, FlowWord};

/// The margin bell's settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bell {
    /// How full the line is, as a share of the box width, when it starts to tighten.
    pub from: f32,
    /// Letter spacing factor of a full line, 0.95 to 1.
    pub tight: f32,
    /// How far right the new line starts after a wrap, in logical pixels; 0 for none.
    pub carriage: f32,
    /// The spring of the tightening and the relaxing.
    pub travel: Spring,
}

impl Bell {
    /// Tightens from 80 % full to 4 % tighter when full, on a 260 ms spring; no carriage.
    pub const DEFAULT: Bell =
        Bell { from: 0.8, tight: 0.96, carriage: 0.0, travel: Spring::critical_to_98(0.26) };

    /// The same bell with a carriage return of `px` on each wrap.
    pub fn carriage(self, px: f32) -> Bell {
        Bell { carriage: px, ..self }
    }

    /// The spacing factor for a line that fills `fill` of the box.
    pub fn factor(&self, fill: f32) -> f32 {
        let tight = self.tight.clamp(0.95, 1.0);
        let k = ((fill - self.from) / (1.0 - self.from).max(1e-3)).clamp(0.0, 1.0);
        1.0 - (1.0 - tight) * k
    }
}

/// The carriage settles with a small bounce in about 300 ms.
const CARRIAGE: Spring = Spring::from_frequency(9.0, 0.45);

/// The margin bell's state on one live text node.
#[derive(Clone, Debug, Default)]
pub(crate) struct Bells {
    style: Option<Bell>,
    /// The first word of the newest line, and that line's spacing factor.
    first: Option<WordId>,
    tight: Option<Channel>,
    /// The line before, relaxing: its first word and its factor.
    prev: Option<(WordId, Channel)>,
    /// The new line's carriage offset.
    carriage: Option<Channel>,
}

impl Flow {
    /// Follows the newest line's fill after an update at `t`, in a box `width` wide.
    pub(super) fn update_bell(&mut self, t: f64, style: Option<Bell>, width: f32, reduced: bool) {
        let (Some(style), false) = (style, reduced) else {
            self.bell = Bells::default();
            return;
        };
        self.bell.style = Some(style);
        let Some(last) = self.words.last() else {
            self.bell.first = None;
            return;
        };
        let line = last.line;
        let first = self.words.iter().find(|w| w.line == line).map_or(last.id, |w| w.id);
        let fill = match self.spans.get(line as usize) {
            Some(&(x0, x1, _)) if width > 0.0 => (x1 - x0) / width,
            _ => 0.0,
        };
        let target = style.factor(fill) as f64;
        let chan = |v: f64, spring: Spring| Channel::new(t, v, spring).with_rest(Rest::UNIT);
        let wrapped = self.bell.first.is_some_and(|old| {
            old != first && self.words.iter().any(|w| w.id == old && w.line < line)
        });
        if wrapped {
            // The full line relaxes; the new one starts at its own spacing.
            let old = self.bell.first.unwrap_or(first);
            let from = self.bell.tight.as_ref().map_or(1.0, |c| c.value(t));
            let mut relax = chan(from, style.travel);
            relax.retarget(t, 1.0);
            self.bell.prev = Some((old, relax));
            self.bell.tight = Some(chan(target, style.travel));
            if style.carriage != 0.0 {
                let mut c = Channel::new(t, style.carriage as f64, CARRIAGE).with_rest(Rest::PIXELS);
                c.retarget(t, 0.0);
                self.bell.carriage = Some(c);
            }
        } else {
            match &mut self.bell.tight {
                Some(c) => c.retarget(t, target),
                None => self.bell.tight = Some(chan(target, style.travel)),
            }
        }
        self.bell.first = Some(first);
        if self.bell.prev.as_ref().is_some_and(|(_, c)| c.settled_at() <= t && c.target() == 1.0) {
            self.bell.prev = None;
        }
    }

    /// When the bell's springs come to rest.
    pub(super) fn bell_settled_at(&self) -> f64 {
        let b = &self.bell;
        b.tight
            .iter()
            .chain(b.prev.iter().map(|(_, c)| c))
            .chain(&b.carriage)
            .map(|c| c.settled_at())
            .fold(f64::NEG_INFINITY, f64::max)
    }

    /// How far the bell moves a pen position `x` (as laid out) of word `w` along its line at
    /// `t`: toward the line's start by the spacing factor, plus the carriage on the newest
    /// line.
    pub(super) fn bell_dx(&self, w: &FlowWord, x: f32, t: f64) -> f32 {
        let b = &self.bell;
        if b.style.is_none() {
            return 0.0;
        }
        let line_of = |id: WordId| self.words.iter().find(|v| v.id == id).map(|v| v.line);
        let x0 = self.spans.get(w.line as usize).map_or(0.0, |s| s.0);
        let newest = b.first.and_then(line_of) == Some(w.line);
        let k = if newest {
            b.tight.as_ref().map_or(1.0, |c| c.value(t) as f32)
        } else {
            match &b.prev {
                Some((id, c)) if line_of(*id) == Some(w.line) => c.value(t) as f32,
                _ => 1.0,
            }
        };
        let carriage = if newest { b.carriage.as_ref().map_or(0.0, |c| c.value(t) as f32) } else { 0.0 };
        (k - 1.0) * (x - x0) + carriage
    }

    /// The bell's move for the caret at pen position `x` on the newest line.
    pub(super) fn bell_caret_dx(&self, x: f32, t: f64) -> f32 {
        match self.words.last() {
            Some(w) => self.bell_dx(w, x, t),
            None => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tightens_only_near_the_edge_and_at_most_five_percent() {
        let b = Bell::DEFAULT;
        assert_eq!(b.factor(0.5), 1.0);
        assert_eq!(b.factor(0.8), 1.0);
        assert!((b.factor(0.9) - 0.98).abs() < 1e-5);
        assert!((b.factor(1.0) - 0.96).abs() < 1e-5);
        let hard = Bell { tight: 0.5, ..b };
        assert!((hard.factor(1.0) - 0.95).abs() < 1e-5, "clamped to the envelope");
    }
}
