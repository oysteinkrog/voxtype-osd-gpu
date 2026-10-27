//! Word swap (T22, T25): a short label that changes as a state changes ("Listening" to
//! "Finishing · 4 s of audio left" to "Done · Esc to dismiss"), word by word.
//!
//! Each word is its own label, keyed by its text and which copy of that text it is, so a word
//! both labels share ("·", "Right", "Ctrl") keeps its node and slides to its new place on the
//! row's position spring instead of leaving and coming back. A word only the old label has
//! leaves upward and fades; it leaves the layout at once, so the words after it close the gap
//! as it goes ("LIVE TRANSCRIPT" to "TRANSCRIPT"). A word only the new label has rises in from
//! below. Anything laid out after the row, such as a keyboard badge, slides with it when it
//! has a position spring (L02).

use std::collections::HashMap;

use vo_core::spring::Spring;

use crate::element::{Element, Key, TextStyle, Transition};
use crate::geom::Color;
use crate::Prop;

/// Motion for [`word_swap`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Swap {
    /// How far below a new word starts and how far above an old one ends, in logical units.
    pub rise: f32,
    /// Moves new and old words up and kept words to their new place.
    pub slide: Spring,
    /// Fades words in and out.
    pub fade: Spring,
}

impl Swap {
    /// About 160 ms of travel and 120 ms of fade, with a rise of 0.6 of the font size.
    pub fn new(style: &TextStyle) -> Swap {
        Swap { rise: 0.6 * style.size, slide: Spring::critical_to_98(0.16), fade: Spring::critical_to_98(0.12) }
    }
}

/// A row of `text`'s words in `style` and `color`, swapped word by word when `text` changes.
/// Words are split at spaces and set one space apart.
pub fn word_swap<M>(key: impl Into<Key>, text: &str, style: TextStyle, color: Color, swap: Swap) -> Element<M> {
    let key = key.into();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let words: Vec<Element<M>> = text
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let copy = seen.entry(w).or_insert(0);
            *copy += 1;
            Element::label(format!("{key}-{w}-{copy}"), w)
                .text_style(style)
                .color(color)
                .shrink(0.0)
                .motion(Prop::Position, swap.slide)
                .motion(Prop::Offset, swap.slide)
                .motion(Prop::Opacity, swap.fade)
                .presence(Transition::fade().offset(0.0, swap.rise), Transition::fade().offset(0.0, -swap.rise))
        })
        .collect();
    // A space's advance in most text faces.
    Element::row(key).gap(0.26 * style.size).children(words)
}
