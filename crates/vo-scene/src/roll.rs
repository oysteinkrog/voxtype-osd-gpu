//! Digit roll (FX04, TY13): a number whose changed digits roll to their new value, like an
//! odometer, while the others stay still.
//!
//! Each digit is a clipped layer one line high holding a column of the ten digits; the
//! column's offset springs to show the current one, and the layer's edges fade so a rolling
//! digit does not end at a hard cut. A digit's column is keyed by the text to its left, so
//! when a counter carries (9 to 0 with the next digit going up) the old column leaves upward,
//! fading, and a new one rolls in from below, as an odometer does, with no history kept in
//! the view.
//! Within one column a digit rolls the short way, up for a larger digit, down for a smaller.
//! When the row first appears its digits roll in from below.
//! A countdown ([`Roll::Down`]) borrows the other way: the new column drops in from above and
//! the old one leaves downward.

use vo_core::spring::Spring;

use crate::element::{Element, Key, TextStyle, Transition};
use crate::geom::Color;
use crate::taffy::prelude::AlignItems;
use crate::Prop;

/// Which way a carried digit column travels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Roll {
    /// Counting up: a new column rolls in from below, the old one leaves upward.
    #[default]
    Up,
    /// Counting down: a new column drops in from above, the old one leaves downward.
    Down,
}

/// A row showing `text` in `style` and `color`, with every ASCII digit in it rolling on
/// `spring` when it changes. Other characters are plain labels. Every digit cell is as wide
/// as the widest digit, so the figures are tabular in any font. To keep a decimal point and
/// a unit in place while the number of integer digits changes, right-align the row.
pub fn digit_roll<M>(key: impl Into<Key>, text: &str, style: TextStyle, color: Color, spring: Spring) -> Element<M> {
    digit_roll_with(key, text, style, color, spring, Roll::Up)
}

/// [`digit_roll`] for a value that counts the way `dir` says, so a carry or a borrow travels
/// the same way as the digits.
pub fn digit_roll_with<M>(
    key: impl Into<Key>,
    text: &str,
    style: TextStyle,
    color: Color,
    spring: Spring,
    dir: Roll,
) -> Element<M> {
    let key = key.into();
    let lh = style.line_height;
    let n = text.chars().count();
    let cells: Vec<Element<M>> = text
        .chars()
        .enumerate()
        .map(|(i, c)| {
            // Keyed by place from the right.
            let place = n - 1 - i;
            match c.to_digit(10) {
                Some(d) => {
                    // Sized and kept from shrinking, so the clip one line high does not squash
                    // the ten lines into it. A carry changes the prefix, so a new column
                    // rolls in from below while the old one leaves upward.
                    let prefix: String = text.chars().take(i).collect();
                    let digits = Element::column(format!("{key}-c{place}-{prefix}"))
                        .height(10.0 * lh)
                        .shrink(0.0)
                        .children((0..10).map(|v| {
                            Element::label(format!("{key}-c{place}-{prefix}-{v}"), v.to_string())
                                .text_style(style)
                                .color(color)
                                .height(lh)
                                .shrink(0.0)
                        }))
                        .offset(0.0, -(d as f32) * lh)
                        .motion(Prop::Offset, spring)
                        .presence(
                            match dir {
                                Roll::Up => Transition::NONE.offset(0.0, lh),
                                Roll::Down => Transition::NONE.offset(0.0, -(d as f32 + 1.0) * lh),
                            },
                            match dir {
                                Roll::Up => Transition::NONE.offset(0.0, -(d as f32 + 1.0) * lh).opacity(0.0),
                                Roll::Down => Transition::NONE.offset(0.0, -(d as f32 - 1.0) * lh).opacity(0.0),
                            },
                        )
                        .motion(Prop::Opacity, Spring::SNAP);
                    Element::layer(format!("{key}-d{place}"))
                        .height(lh)
                        .align_items(AlignItems::FLEX_START)
                        .fade_edges(0.0, 0.2 * lh, 0.0, 0.2 * lh)
                        .child(digits)
                }
                // A space has no ink, so it gets the width text layout would give it.
                None if c.is_whitespace() => {
                    Element::row(format!("{key}-s{place}")).width(0.26 * style.size).height(lh)
                }
                None => {
                    Element::label(format!("{key}-s{place}"), c.to_string()).text_style(style).color(color).height(lh)
                }
            }
        })
        .collect();
    Element::row(key).children(cells)
}
