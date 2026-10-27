//! Lines by age (T14, the teleprompter): live text broken into lines, each line its own node
//! whose look depends on how many lines came after it.
//!
//! [`break_lines`] breaks words into lines with the scene's own [`TextLayout`], so the breaks
//! match what a label of that width would show. [`aged_lines`] stacks the lines in a column,
//! newest at the bottom. Each line is keyed by its first word's id, so when a new line starts
//! the older ones keep their nodes: they move up on their position spring and ease to the look
//! for their new age (scale and opacity). Lines older than the last look leave: they fade out
//! upward and give up their space. Put the column at the bottom of a box to make the text roll
//! upward.

use vo_core::spring::Spring;
use vo_core::words::Word;

use crate::element::{Element, Key, TextStyle, Transition};
use crate::geom::Color;
use crate::taffy::prelude::AlignItems;
use crate::text::{TextLayout, ELLIPSIS_CLUSTER};
use crate::Prop;

/// One line of words and its width at the style it was broken with.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub words: Vec<Word>,
    pub width: f32,
}

/// How a line looks at one age. Age 0 is the newest line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineLook {
    pub scale: f32,
    pub opacity: f32,
}

impl LineLook {
    pub const fn new(scale: f32, opacity: f32) -> LineLook {
        LineLook { scale, opacity }
    }
}

/// Motion and looks for [`aged_lines`].
#[derive(Clone, Debug, PartialEq)]
pub struct Ages {
    /// The look of each age, newest first. A line older than the last entry leaves.
    pub looks: Vec<LineLook>,
    /// Where on the line scaling is centred, from 0 (left end) to 1 (right end).
    pub origin: f32,
    /// Moves lines to their new place and scale.
    pub travel: Spring,
    /// Fades lines to their new opacity.
    pub fade: Spring,
    /// Where a new word starts from; see `Element::word_enter`.
    pub word_enter: Transition,
}

impl Ages {
    /// The teleprompter: the newest line at full size, the one before at 0.88 scale and 0.45
    /// opacity, the one before that fading out; lines move in 260 ms.
    pub fn teleprompter() -> Ages {
        Ages {
            looks: vec![LineLook::new(1.0, 1.0), LineLook::new(0.88, 0.45), LineLook::new(0.88, 0.0)],
            origin: 0.0,
            travel: Spring::critical_to_98(0.26),
            fade: Spring::critical_to_98(0.26),
            word_enter: Transition::fade().offset(0.0, 6.0),
        }
    }
}

/// Breaks `words` into lines no wider than `width`, the way `layout` breaks the joined text.
pub fn break_lines(layout: &dyn TextLayout, words: &[Word], style: &TextStyle, width: f32) -> Vec<Line> {
    let joined = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    let style = TextStyle { max_lines: 0, ellipsis: false, ..*style };
    let shaped = layout.layout(&joined, &style, Some(width));
    // The baseline of each word's first glyph says which line it is on.
    let mut starts = Vec::with_capacity(words.len());
    let mut at = 0u32;
    for w in words {
        starts.push(at);
        at += w.text.len() as u32 + 1;
    }
    let mut first_y: Vec<Option<f32>> = vec![None; words.len()];
    for g in shaped.glyphs.iter().filter(|g| g.cluster != ELLIPSIS_CLUSTER) {
        let i = starts.partition_point(|s| *s <= g.cluster).saturating_sub(1);
        first_y[i].get_or_insert(g.pos.y);
    }
    let mut lines: Vec<(f32, Vec<Word>)> = Vec::new();
    for (w, y) in words.iter().zip(first_y) {
        // A word with no glyphs (empty text) stays on the line before.
        match (lines.last_mut(), y) {
            (Some(last), Some(y)) if y > last.0 + 0.5 => lines.push((y, vec![w.clone()])),
            (Some(last), _) => last.1.push(w.clone()),
            (None, y) => lines.push((y.unwrap_or(0.0), vec![w.clone()])),
        }
    }
    lines
        .into_iter()
        .map(|(_, words)| {
            let text = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
            let width = layout.layout(&text, &style, None).size.w;
            Line { words, width }
        })
        .collect()
}

/// A column of `lines`, newest last, each line looking its age by `ages`.
pub fn aged_lines<M>(key: impl Into<Key>, lines: &[Line], style: TextStyle, color: Color, ages: &Ages) -> Element<M> {
    let key = key.into();
    let n = lines.len();
    let kept = n.min(ages.looks.len());
    let children: Vec<Element<M>> = lines[n - kept..]
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let age = kept - 1 - i;
            let look = ages.looks[age];
            let id = line.words.first().map_or(0, |w| w.id);
            let s = look.scale;
            // Scaling is about the box centre; move the box so `origin` stays put.
            let dx = (0.5 - ages.origin) * line.width * (s - 1.0);
            let dy = 0.5 * style.line_height * (s - 1.0);
            Element::live_text(format!("{key}-l{id}"), &line.words)
                .text_style(style)
                .color(color)
                .shrink(0.0)
                .word_enter(ages.word_enter)
                .scale(s)
                .opacity(look.opacity)
                .offset(dx, dy)
                .motion(Prop::Position, ages.travel)
                .motion(Prop::Scale, ages.travel)
                .motion(Prop::Offset, ages.travel)
                .motion(Prop::Opacity, ages.fade)
                .presence(Transition::NONE, Transition::fade().offset(dx, dy - 0.5 * style.line_height))
        })
        .collect();
    // Each line keeps its own width, so its scale centre is where `origin` says.
    Element::column(key).align_items(AlignItems::FLEX_START).children(children)
}
