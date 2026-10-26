//! Elements: the keyed tree that `fn view(&State, &Tokens) -> Element` returns.
//!
//! An element is a description, rebuilt each time app state changes. Every numeric value on
//! it is a target: the scene moves the node's channel toward it with the property's motion
//! (a spring by default). Layout fields go to taffy, and the boxes taffy computes become
//! position and size targets too.
//!
//! Every element has a key. The scene matches elements to retained nodes by key path (the
//! keys from the root down), so two siblings must not share a key. A key that moves to
//! another parent is a new node there.

use std::borrow::Cow;

use taffy::prelude::{
    auto, length, AlignItems, Dimension, FlexDirection, JustifyContent, LengthPercentage, LengthPercentageAuto,
};
use vo_core::spring::Spring;
use vo_core::words::Word;

use crate::canvas::{CanvasDraw, Painter};
use crate::display::{BlendMode, ShapeKind};
use crate::dots::Dots;
use crate::geom::{Color, Rect};
use crate::timed::{Progress, Wave};

/// A node's key among its siblings.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(pub Cow<'static, str>);

impl From<&'static str> for Key {
    fn from(s: &'static str) -> Key {
        Key(Cow::Borrowed(s))
    }
}

impl From<String> for Key {
    fn from(s: String) -> Key {
        Key(Cow::Owned(s))
    }
}

impl From<u64> for Key {
    fn from(n: u64) -> Key {
        Key(Cow::Owned(n.to_string()))
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An animated scalar on a node. Each has its own channel; colours use four slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub(crate) enum Slot {
    X,
    Y,
    W,
    H,
    Opacity,
    OffsetX,
    OffsetY,
    Scale,
    Rotation,
    Radius,
    StrokeWidth,
    /// Four slots each, red to alpha.
    Fill0 = 11,
    Fill1 = 15,
    Stroke = 19,
    Text = 23,
    GlowColor = 27,
    GlowRadius = 31,
    GlowStrength = 32,
    ShadowColor = 33,
    ShadowX = 37,
    ShadowY = 38,
    ShadowBlur = 39,
    BloomStrength = 40,
    BloomRadius = 41,
    Shutter = 42,
}

impl Slot {
    /// Value of a slot no element has set.
    pub(crate) fn default_value(slot: u8) -> f64 {
        if slot == Slot::Opacity as u8 || slot == Slot::Scale as u8 {
            1.0
        } else {
            0.0
        }
    }

    pub(crate) fn prop(slot: u8) -> Prop {
        match slot {
            s if s <= Slot::Y as u8 => Prop::Position,
            s if s <= Slot::H as u8 => Prop::Size,
            s if s == Slot::Opacity as u8 => Prop::Opacity,
            s if s <= Slot::OffsetY as u8 => Prop::Offset,
            s if s == Slot::Scale as u8 => Prop::Scale,
            s if s == Slot::Rotation as u8 => Prop::Rotation,
            s if s == Slot::Radius as u8 => Prop::Radius,
            s if s == Slot::StrokeWidth as u8 => Prop::StrokeWidth,
            s if s < Slot::Stroke as u8 => Prop::Fill,
            s if s < Slot::Text as u8 => Prop::Stroke,
            s if s < Slot::GlowColor as u8 => Prop::TextColor,
            s if s < Slot::ShadowColor as u8 => Prop::Glow,
            s if s < Slot::BloomStrength as u8 => Prop::Shadow,
            s if s < Slot::Shutter as u8 => Prop::Bloom,
            _ => Prop::MotionBlur,
        }
    }

    /// Whether the slot holds a value from 0 to 1 (opacity, scale, colour, angle), which
    /// settles at a finer threshold than pixels.
    pub(crate) fn is_unit(slot: u8) -> bool {
        !(slot <= Slot::H as u8
            || slot == Slot::OffsetX as u8
            || slot == Slot::OffsetY as u8
            || slot == Slot::Radius as u8
            || slot == Slot::StrokeWidth as u8
            || slot == Slot::GlowRadius as u8
            || (Slot::ShadowX as u8..=Slot::ShadowBlur as u8).contains(&slot)
            || slot == Slot::BloomRadius as u8)
    }
}

/// A group of animated properties that share one motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Prop {
    /// The layout position.
    Position,
    /// The layout size.
    Size,
    Opacity,
    /// The offset added to the layout position.
    Offset,
    Scale,
    Rotation,
    Radius,
    StrokeWidth,
    /// Both fill colours (a solid fill uses the first).
    Fill,
    Stroke,
    TextColor,
    /// Glow colour, radius and strength.
    Glow,
    /// Shadow colour, offset and blur.
    Shadow,
    /// Bloom strength and radius.
    Bloom,
    /// The motion blur shutter.
    MotionBlur,
}

/// How a property moves to a new target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    Spring(Spring),
    /// Jumps to the target at once.
    Snap,
}

impl From<Spring> for Motion {
    fn from(s: Spring) -> Motion {
        Motion::Spring(s)
    }
}

/// The motion of a property no `.motion()` call names.
pub const DEFAULT_MOTION: Motion = Motion::Spring(Spring::SOFT);

/// The state a node enters from or exits to. Unset fields stay at the element's value.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Transition {
    pub opacity: Option<f32>,
    pub offset: Option<(f32, f32)>,
    pub scale: Option<f32>,
}

impl Transition {
    pub const NONE: Transition = Transition { opacity: None, offset: None, scale: None };

    pub fn fade() -> Transition {
        Transition { opacity: Some(0.0), ..Transition::NONE }
    }

    pub fn opacity(mut self, v: f32) -> Transition {
        self.opacity = Some(v);
        self
    }

    pub fn offset(mut self, x: f32, y: f32) -> Transition {
        self.offset = Some((x, y));
        self
    }

    pub fn scale(mut self, s: f32) -> Transition {
        self.scale = Some(s);
        self
    }

    /// The transition with reduced motion: its opacity only, no travel or scale.
    pub(crate) fn reduced(self) -> Transition {
        Transition { opacity: self.opacity, ..Transition::NONE }
    }

    /// The slot values this transition sets.
    pub(crate) fn slots(&self) -> impl Iterator<Item = (u8, f64)> {
        let o = self.opacity.map(|v| (Slot::Opacity as u8, v as f64));
        let x = self.offset.map(|(x, _)| (Slot::OffsetX as u8, x as f64));
        let y = self.offset.map(|(_, y)| (Slot::OffsetY as u8, y as f64));
        let s = self.scale.map(|v| (Slot::Scale as u8, v as f64));
        [o, x, y, s].into_iter().flatten()
    }
}

/// Font, size and line limits for a label. `font` is an id in the shared
/// [`FontRegistry`](crate::font::FontRegistry).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub font: u32,
    /// Em size in logical units.
    pub size: f32,
    /// Line height in logical units.
    pub line_height: f32,
    /// Most lines shown; 0 means no limit. Lines past the limit are dropped.
    pub max_lines: u32,
    /// End the last line with an ellipsis when text was dropped, either lines past
    /// `max_lines` or the end of a word too wide for its line.
    pub ellipsis: bool,
    /// Extra space after every character in logical units, as CSS `letter-spacing`. Negative
    /// tightens.
    pub tracking: f32,
}

impl TextStyle {
    pub fn new(font: u32, size: f32, line_height: f32) -> TextStyle {
        TextStyle { font, size, line_height, max_lines: 0, ellipsis: false, tracking: 0.0 }
    }

    /// With `tracking` logical units of extra space after every character.
    pub fn tracking(self, tracking: f32) -> TextStyle {
        TextStyle { tracking, ..self }
    }

    /// At most `n` lines, the last one ending in an ellipsis when text was cut.
    pub fn clamp_lines(self, n: u32) -> TextStyle {
        TextStyle { max_lines: n, ellipsis: true, ..self }
    }
}

impl Default for TextStyle {
    fn default() -> TextStyle {
        TextStyle::new(0, 14.0, 18.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    /// A container with an optional rounded background and border.
    Panel,
    /// A container that draws nothing itself.
    Stack,
    Label {
        text: String,
        style: TextStyle,
    },
    Shape(ShapeKind),
    /// A container drawn as its own display-list layer.
    Layer {
        blend: BlendMode,
        clip: bool,
        offscreen: bool,
    },
    /// Live dictation text: lays out the words like a label, tags each glyph with its word
    /// id, and moves each word by its own channels. A new id enters from `enter`; a kept id
    /// whose text changed jumps to `change` and springs back.
    LiveText {
        words: Vec<Word>,
        style: TextStyle,
        enter: Transition,
        change: Transition,
    },
    /// A scrolling waveform filling the box.
    Wave(Wave),
    /// A progress bar filling the box.
    Progress(Progress),
    /// A dot grid filling the box.
    Dots(Dots),
    /// A filled path from `FontRegistry::add_path`: `view` (path units) fits the box,
    /// centred, keeping its aspect.
    Path { id: u32, view: Rect },
    /// App drawing code run from the frame time (`crate::canvas`).
    Canvas(CanvasDraw),
}

impl Kind {
    /// Same variant, so a node can be updated in place rather than replaced.
    pub(crate) fn same_variant(&self, other: &Kind) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    pub(crate) fn is_text(&self) -> bool {
        matches!(self, Kind::Label { .. } | Kind::LiveText { .. })
    }
}

/// Pointer handlers. Message values, not closures, so elements stay cheap to rebuild.
#[derive(Clone, Debug)]
pub(crate) struct Handlers<M> {
    pub hit: bool,
    pub click: Option<M>,
    pub hover: Option<(M, M)>,
}

impl<M> Default for Handlers<M> {
    fn default() -> Self {
        Handlers { hit: false, click: None, hover: None }
    }
}

/// One element of a view. `M` is the app's message type for pointer handlers.
#[derive(Clone, Debug)]
pub struct Element<M = ()> {
    pub(crate) key: Key,
    pub(crate) kind: Kind,
    pub(crate) style: taffy::Style,
    /// Targets set on this element, by slot.
    pub(crate) targets: Vec<(u8, f64)>,
    pub(crate) gradient: bool,
    pub(crate) hairline: bool,
    /// The stroke lies inside the box, as a CSS border does, rather than centred on its
    /// edge.
    pub(crate) inside: bool,
    pub(crate) motion: Vec<(Prop, Motion)>,
    pub(crate) presence: Option<(Transition, Transition)>,
    pub(crate) handlers: Handlers<M>,
    pub(crate) children: Vec<Element<M>>,
}

impl<M> Element<M> {
    fn new(key: impl Into<Key>, kind: Kind, style: taffy::Style) -> Element<M> {
        Element {
            key: key.into(),
            kind,
            style,
            targets: Vec::new(),
            gradient: false,
            hairline: false,
            inside: false,
            motion: Vec::new(),
            presence: None,
            handlers: Handlers::default(),
            children: Vec::new(),
        }
    }

    fn flex(direction: FlexDirection) -> taffy::Style {
        taffy::Style { display: taffy::Display::Flex, flex_direction: direction, ..Default::default() }
    }

    /// A container with a background, laid out as a flex column.
    pub fn panel(key: impl Into<Key>) -> Element<M> {
        Element::new(key, Kind::Panel, Element::<M>::flex(FlexDirection::Column))
    }

    /// An invisible flex row.
    pub fn row(key: impl Into<Key>) -> Element<M> {
        Element::new(key, Kind::Stack, Element::<M>::flex(FlexDirection::Row))
    }

    /// An invisible flex column.
    pub fn column(key: impl Into<Key>) -> Element<M> {
        Element::new(key, Kind::Stack, Element::<M>::flex(FlexDirection::Column))
    }

    /// One run of text, measured by the scene's `TextLayout`. Wraps at its width.
    pub fn label(key: impl Into<Key>, text: impl Into<String>) -> Element<M> {
        let kind = Kind::Label { text: text.into(), style: TextStyle::default() };
        let mut e = Element::new(key, kind, taffy::Style::default());
        e.set(Slot::Text as u8, Color::WHITE);
        e
    }

    /// Live dictation words, with ids from `vo-core`'s word model. Words have no motion of
    /// their own until [`Element::word_enter`] or [`Element::word_change`] sets one.
    pub fn live_text(key: impl Into<Key>, words: &[Word]) -> Element<M> {
        let kind = Kind::LiveText {
            words: words.to_vec(),
            style: TextStyle::default(),
            enter: Transition::NONE,
            change: Transition::NONE,
        };
        let mut e = Element::new(key, kind, taffy::Style::default());
        e.set(Slot::Text as u8, Color::WHITE);
        e
    }

    /// An SDF shape filling the element's box. A circle sits centred with the shorter side
    /// as its diameter; a line runs from the top-left to the bottom-right corner.
    pub fn shape(key: impl Into<Key>, kind: ShapeKind) -> Element<M> {
        Element::new(key, Kind::Shape(kind), taffy::Style::default())
    }

    /// A filled waveform that fills the element's box and scrolls by the frame time.
    pub fn wave(key: impl Into<Key>, wave: Wave) -> Element<M> {
        Element::new(key, Kind::Wave(wave), taffy::Style::default())
    }

    /// A progress bar that fills the element's box, with an indeterminate sweep when the
    /// fraction is unknown.
    pub fn progress(key: impl Into<Key>, progress: Progress) -> Element<M> {
        Element::new(key, Kind::Progress(progress), taffy::Style::default())
    }

    /// A filled path from `FontRegistry::add_path`, drawn through the glyph shader in the
    /// fill colour (white until `.fill`). `view` is the path's own box in path units, for an
    /// SVG icon its `viewBox`; it is scaled to fit the element's box and centred.
    pub fn path(key: impl Into<Key>, id: u32, view: Rect) -> Element<M> {
        let mut e = Element::new(key, Kind::Path { id, view }, taffy::Style::default());
        e.set(Slot::Fill0 as u8, Color::WHITE);
        e
    }

    /// Drawn by `draw(t, rect, painter)` each time the scene is sampled (`crate::canvas`).
    /// `draw` must be pure in `t`. The scene keeps waking until `until`. Give it a size: it
    /// has no content size.
    pub fn canvas(
        key: impl Into<Key>,
        until: f64,
        draw: impl Fn(f64, Rect, &mut Painter) + Send + Sync + 'static,
    ) -> Element<M> {
        Element::new(key, Kind::Canvas(CanvasDraw::new(until, std::sync::Arc::new(draw))), taffy::Style::default())
    }

    /// [`Element::canvas`] with a drawing that already exists, so a view rebuilt each time
    /// keeps the same node content.
    pub fn canvas_shared(key: impl Into<Key>, draw: CanvasDraw) -> Element<M> {
        Element::new(key, Kind::Canvas(draw), taffy::Style::default())
    }

    /// A dot grid filling the box (`crate::dots`). Give it a size: it has no content size.
    pub fn dots(key: impl Into<Key>, dots: Dots) -> Element<M> {
        Element::new(key, Kind::Dots(dots), taffy::Style::default())
    }

    /// A container drawn as its own layer: group transform, blend mode, optional clip to
    /// its rounded box, optional offscreen target. Laid out as a flex column.
    pub fn layer(key: impl Into<Key>) -> Element<M> {
        let kind = Kind::Layer { blend: BlendMode::Normal, clip: false, offscreen: false };
        Element::new(key, kind, Element::<M>::flex(FlexDirection::Column))
    }

    pub fn key(&self) -> &Key {
        &self.key
    }

    fn set(&mut self, slot: u8, value: impl Into<Target>) {
        for (i, v) in value.into().0.into_iter().enumerate().take_while(|(_, v)| v.is_some()) {
            let s = slot + i as u8;
            let v = v.expect("checked above");
            match self.targets.iter_mut().find(|(k, _)| *k == s) {
                Some(t) => t.1 = v,
                None => self.targets.push((s, v)),
            }
        }
    }

    // Children.

    pub fn child(mut self, child: Element<M>) -> Element<M> {
        self.children.push(child);
        self
    }

    pub fn children(mut self, children: impl IntoIterator<Item = Element<M>>) -> Element<M> {
        self.children.extend(children);
        self
    }

    // Layout. Lengths are logical units.

    pub fn width(mut self, w: f32) -> Element<M> {
        self.style.size.width = Dimension::length(w);
        self
    }

    pub fn height(mut self, h: f32) -> Element<M> {
        self.style.size.height = Dimension::length(h);
        self
    }

    pub fn size(self, w: f32, h: f32) -> Element<M> {
        self.width(w).height(h)
    }

    pub fn max_width(mut self, w: f32) -> Element<M> {
        self.style.max_size.width = LengthPercentageAuto::length(w);
        self
    }

    pub fn min_width(mut self, w: f32) -> Element<M> {
        self.style.min_size.width = LengthPercentageAuto::length(w);
        self
    }

    pub fn pad(self, p: f32) -> Element<M> {
        self.pad_xy(p, p)
    }

    pub fn pad_xy(mut self, x: f32, y: f32) -> Element<M> {
        let (x, y) = (LengthPercentage::length(x), LengthPercentage::length(y));
        self.style.padding = taffy::Rect { left: x, right: x, top: y, bottom: y };
        self
    }

    /// Padding per edge: top, right, bottom, left.
    pub fn pad_edges(mut self, top: f32, right: f32, bottom: f32, left: f32) -> Element<M> {
        let l = LengthPercentage::length;
        self.style.padding = taffy::Rect { left: l(left), right: l(right), top: l(top), bottom: l(bottom) };
        self
    }

    /// Margin per edge: top, right, bottom, left.
    pub fn margin_edges(mut self, top: f32, right: f32, bottom: f32, left: f32) -> Element<M> {
        self.style.margin =
            taffy::Rect { left: length(left), right: length(right), top: length(top), bottom: length(bottom) };
        self
    }

    pub fn margin(mut self, m: f32) -> Element<M> {
        let m = length(m);
        self.style.margin = taffy::Rect { left: m, right: m, top: m, bottom: m };
        self
    }

    /// Space between children.
    pub fn gap(mut self, g: f32) -> Element<M> {
        self.style.gap = taffy::Size { width: length(g), height: length(g) };
        self
    }

    pub fn direction_row(mut self) -> Element<M> {
        self.style.flex_direction = FlexDirection::Row;
        self
    }

    pub fn align_items(mut self, a: AlignItems) -> Element<M> {
        self.style.align_items = a;
        self
    }

    pub fn justify(mut self, j: JustifyContent) -> Element<M> {
        self.style.justify_content = j;
        self
    }

    pub fn grow(mut self, g: f32) -> Element<M> {
        self.style.flex_grow = g;
        self
    }

    pub fn shrink(mut self, s: f32) -> Element<M> {
        self.style.flex_shrink = s;
        self
    }

    /// Block layout instead of flex for this container.
    pub fn block(mut self) -> Element<M> {
        self.style.display = taffy::Display::Block;
        self
    }

    /// Any other taffy field.
    pub fn layout(mut self, f: impl FnOnce(&mut taffy::Style)) -> Element<M> {
        f(&mut self.style);
        self
    }

    /// Clears a fixed size back to auto.
    pub fn auto_size(mut self) -> Element<M> {
        self.style.size = taffy::Size { width: auto(), height: auto() };
        self
    }

    // Visual targets.

    pub fn opacity(mut self, o: f32) -> Element<M> {
        self.set(Slot::Opacity as u8, o);
        self
    }

    pub fn offset(mut self, x: f32, y: f32) -> Element<M> {
        self.set(Slot::OffsetX as u8, [x, y]);
        self
    }

    /// Scale around the box centre.
    pub fn scale(mut self, s: f32) -> Element<M> {
        self.set(Slot::Scale as u8, s);
        self
    }

    /// Rotation in radians around the box centre, clockwise on screen.
    pub fn rotate(mut self, radians: f32) -> Element<M> {
        self.set(Slot::Rotation as u8, radians);
        self
    }

    pub fn radius(mut self, r: f32) -> Element<M> {
        self.set(Slot::Radius as u8, r);
        self
    }

    pub fn fill(mut self, c: Color) -> Element<M> {
        self.set(Slot::Fill0 as u8, c);
        self.gradient = false;
        self
    }

    /// A vertical gradient from `top` to `bottom`.
    pub fn fill_gradient(mut self, top: Color, bottom: Color) -> Element<M> {
        self.set(Slot::Fill0 as u8, top);
        self.set(Slot::Fill1 as u8, bottom);
        self.gradient = true;
        self
    }

    /// A stroke `width` wide centred on the outline.
    pub fn stroke(mut self, c: Color, width: f32) -> Element<M> {
        self.set(Slot::Stroke as u8, c);
        self.set(Slot::StrokeWidth as u8, width);
        self.hairline = false;
        self.inside = false;
        self
    }

    /// A stroke `width` wide inside the box, as a CSS border: the shape's outer edge stays
    /// on the box edge. For panels, layers and rounded rects; other shapes centre it.
    pub fn border(mut self, c: Color, width: f32) -> Element<M> {
        self = self.stroke(c, width);
        self.inside = true;
        self
    }

    /// A stroke one physical pixel wide at any scale.
    pub fn hairline(mut self, c: Color) -> Element<M> {
        self.set(Slot::Stroke as u8, c);
        self.hairline = true;
        self.inside = false;
        self
    }

    /// Text colour for labels and live text.
    pub fn color(mut self, c: Color) -> Element<M> {
        self.set(Slot::Text as u8, c);
        self
    }

    /// Font and size for labels and live text. Changes layout, so it is not animated.
    pub fn text_style(mut self, s: TextStyle) -> Element<M> {
        match &mut self.kind {
            Kind::Label { style, .. } | Kind::LiveText { style, .. } => *style = s,
            _ => debug_assert!(false, "text_style on a node without text: {}", self.key),
        }
        self
    }

    /// Live text: where a word with a new id starts from. Opacity and offset are used; scale
    /// is ignored. The words move with this element's `Prop::Opacity` and `Prop::Offset`
    /// motion.
    pub fn word_enter(mut self, t: Transition) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { enter, .. } => *enter = t,
            _ => debug_assert!(false, "word_enter on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: where a kept word whose text changed ("confi" to "configuration", "hello" to
    /// "Hello,") jumps to before it springs back.
    pub fn word_change(mut self, t: Transition) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { change, .. } => *change = t,
            _ => debug_assert!(false, "word_change on a node that is not live text: {}", self.key),
        }
        self
    }

    pub fn blend(mut self, b: BlendMode) -> Element<M> {
        if let Kind::Layer { blend, .. } = &mut self.kind {
            *blend = b;
        } else {
            debug_assert!(false, "blend on a node that is not a layer: {}", self.key);
        }
        self
    }

    /// Clip the layer's content to its rounded box.
    pub fn clip(mut self) -> Element<M> {
        if let Kind::Layer { clip, .. } = &mut self.kind {
            *clip = true;
        } else {
            debug_assert!(false, "clip on a node that is not a layer: {}", self.key);
        }
        self
    }

    /// A glow around the element's shape: `c` fading out over about `radius` logical units,
    /// at `strength` (1 is the colour at the shape's edge; above 1 is brighter than the
    /// colour and lights a bloom around it). For elements that draw a shape: panels, layers
    /// and shapes. Text glows through a [`Element::bloom`] layer around it instead.
    pub fn glow(mut self, c: Color, radius: f32, strength: f32) -> Element<M> {
        self.set(Slot::GlowColor as u8, c);
        self.set(Slot::GlowRadius as u8, radius);
        self.set(Slot::GlowStrength as u8, strength);
        self
    }

    /// A soft drop shadow of the element's shape, moved by `dx`, `dy` and blurred with a
    /// standard deviation of `blur`, in logical units. For panels, layers and shapes.
    pub fn shadow(mut self, c: Color, dx: f32, dy: f32, blur: f32) -> Element<M> {
        self.set(Slot::ShadowColor as u8, c);
        self.set(Slot::ShadowX as u8, [dx, dy]);
        self.set(Slot::ShadowBlur as u8, blur);
        self
    }

    /// Bloom on a layer: a blurred copy of everything in it, spread over about `radius`
    /// logical units, is added `strength` times on top. Makes the layer offscreen and puts
    /// the frame on the renderer's effect path. The halo is not cut by the layer's clip.
    pub fn bloom(mut self, strength: f32, radius: f32) -> Element<M> {
        if let Kind::Layer { offscreen, .. } = &mut self.kind {
            *offscreen = true;
        } else {
            debug_assert!(false, "bloom on a node that is not a layer: {}", self.key);
        }
        self.set(Slot::BloomStrength as u8, strength);
        self.set(Slot::BloomRadius as u8, radius);
        self
    }

    /// Motion blur (FX12): while the element moves, its content is smeared along the distance it
    /// travels in `shutter` seconds (1/60 is a light blur at 60 fps, 1/30 a strong one). The
    /// speed comes from its position and offset springs. Text in it blurs as a whole.
    pub fn motion_blur(mut self, shutter: f32) -> Element<M> {
        self.set(Slot::Shutter as u8, shutter.max(0.0));
        self.motion.push((Prop::MotionBlur, Motion::Snap));
        self
    }

    /// Draw the layer into its own texture, for exact group opacity and effects.
    pub fn offscreen(mut self) -> Element<M> {
        if let Kind::Layer { offscreen, .. } = &mut self.kind {
            *offscreen = true;
        } else {
            debug_assert!(false, "offscreen on a node that is not a layer: {}", self.key);
        }
        self
    }

    // Motion.

    /// How `prop` moves to new targets on this node.
    pub fn motion(mut self, prop: Prop, motion: impl Into<Motion>) -> Element<M> {
        let motion = motion.into();
        match self.motion.iter_mut().find(|(p, _)| *p == prop) {
            Some(m) => m.1 = motion,
            None => self.motion.push((prop, motion)),
        }
        self
    }

    /// The node enters from `enter` and, once removed from the view, exits to `exit` and
    /// stays until that motion settles.
    pub fn presence(mut self, enter: Transition, exit: Transition) -> Element<M> {
        self.presence = Some((enter, exit));
        self
    }

    // Input (docs/INPUT-MODEL.md).

    /// A hit target with no handler: blocks click-through over its shape.
    pub fn interactive(mut self) -> Element<M> {
        self.handlers.hit = true;
        self
    }

    pub fn on_click(mut self, msg: M) -> Element<M> {
        self.handlers.hit = true;
        self.handlers.click = Some(msg);
        self
    }

    pub fn on_hover(mut self, enter: M, leave: M) -> Element<M> {
        self.handlers.hit = true;
        self.handlers.hover = Some((enter, leave));
        self
    }
}

/// Up to four slot values from a scalar, a pair or a colour.
struct Target([Option<f64>; 4]);

impl From<f32> for Target {
    fn from(v: f32) -> Target {
        Target([Some(v as f64), None, None, None])
    }
}

impl From<[f32; 2]> for Target {
    fn from(v: [f32; 2]) -> Target {
        Target([Some(v[0] as f64), Some(v[1] as f64), None, None])
    }
}

impl From<Color> for Target {
    fn from(c: Color) -> Target {
        Target([Some(c.r as f64), Some(c.g as f64), Some(c.b as f64), Some(c.a as f64)])
    }
}
