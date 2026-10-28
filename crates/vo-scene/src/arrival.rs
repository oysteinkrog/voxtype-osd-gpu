//! How a new live-text word arrives (MO-08 follow-up): readable in the frame it arrives.
//!
//! The reading-line envelope of the motion plan (section 2, rule 1) allows a new word at
//! least 70 % ink, at most 3 px off its baseline, and no blur or scale. By default a word's
//! enter transition (`Element::word_enter`) is pulled inside that: a fade starts at 70 %, a
//! rise starts at 3 px, a dissolve starts at most 30 % in, and a blur or wipe is dropped (a
//! wipe hides the end of the word outright, so no part of it is readable).
//! The look keeps the direction and the springs of its arrival; only the distance shrinks.
//! A sideways slide is left alone: the envelope says nothing about it.
//!
//! `Arrival::Free` (`Element::word_arrival`) takes the transition as given, for text that is
//! not read as it arrives, such as a title card.

use crate::element::Transition;

/// Ink a word starts at, at least.
pub const INK: f32 = 0.7;
/// Baseline offset a word starts at, at most, in logical pixels.
pub const RISE: f32 = 3.0;
/// Dissolve a word starts at, at most (it takes ink away).
pub const COVER: f32 = 1.0 - INK;

/// How a new word's enter transition is applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Arrival {
    /// Pulled inside the reading-line envelope (see the module docs).
    #[default]
    Readable,
    /// As given.
    Free,
}

impl Arrival {
    /// The enter transition a new word starts from.
    pub fn apply(self, enter: Transition) -> Transition {
        match self {
            Arrival::Free => enter,
            Arrival::Readable => readable(enter),
        }
    }
}

/// `enter` pulled inside the reading-line envelope.
pub fn readable(enter: Transition) -> Transition {
    Transition {
        opacity: enter.opacity.map(|o| o.max(INK)),
        offset: enter.offset.map(|(x, y)| (x, y.clamp(-RISE, RISE))),
        scale: None,
        blur: None,
        dissolve: enter.dissolve.map(|d| d.min(COVER)),
        wipe: None,
        ..enter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fade_and_rise_keeps_its_direction_inside_the_envelope() {
        let t = readable(Transition::fade().offset(4.0, 10.0).blur(6.0).dissolve(1.0).wipe(0.2).scale(0.8));
        assert_eq!(t.opacity, Some(INK));
        assert_eq!(t.offset, Some((4.0, RISE)));
        assert_eq!((t.blur, t.scale), (None, None));
        assert_eq!(t.dissolve, Some(COVER));
        assert_eq!(t.wipe, None);
        assert_eq!(readable(Transition::NONE.offset(0.0, -8.0)).offset, Some((0.0, -RISE)));
    }

    #[test]
    fn what_is_already_inside_is_kept() {
        let t = Transition::fade().opacity(0.85).offset(0.0, 2.0);
        assert_eq!(readable(t), t);
        assert_eq!(readable(Transition::NONE), Transition::NONE);
        assert_eq!(Arrival::Free.apply(Transition::fade()), Transition::fade());
    }
}
