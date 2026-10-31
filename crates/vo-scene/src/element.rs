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

use crate::ambient::{Loop, LoopProp, Ripples};
use crate::bars::Bars;
use crate::canvas::{CanvasDraw, Painter};
use crate::displace::Displace;
use crate::display::{BackdropRes, BlendMode, BloomShape, Mask, ShapeKind, TrailDrift};
use crate::dots::Dots;
use crate::emitter::{Emitter, WordParticles};
use crate::final_pass::FinalPass;
use crate::flow::{Caret, CaseChange, EditDecor, LiveMotion, Revise, Rewrap, SettleInk};
use crate::geom::{Color, Rect};
use crate::glass::{Glass, GlassShape};
use crate::material::Material;
use crate::path::{Morph, Outline, StrokeStyle};
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
    /// Bloom shape (bloom v2, `Element::bloom_shape`): the emissive threshold, then per band
    /// (core, mid, tail) its weight, reach, saturation and tint (four slots).
    BloomThreshold = 120,
    /// Band k starts at `BloomBand0 + k * BAND_SLOTS`, so slots 121 to 141 are taken.
    BloomBand0 = 121,
    Shutter = 42,
    TrailHalfLife = 43,
    /// Rim (FX08): colour, stroke width, band width in turns, angle in turns, seconds a lap.
    RimColor = 44,
    RimWidth = 48,
    RimSpan = 49,
    RimAngle = 50,
    RimPeriod = 51,
    /// Sheen (FX08): colour and the seconds one sweep takes.
    SheenColor = 52,
    SheenSecs = 56,
    /// Path trim (`Element::stroke_path`): start and end as fractions of the length.
    TrimStart = 57,
    TrimEnd = 58,
    /// Morph progress (`Element::morph`), 0 to 1.
    Morph = 59,
    /// Text effects (TY17) on a label or live text: blur and chromatic offset in logical
    /// units, dissolve and wipe from 0 to 1, then a halo colour and radius.
    TextBlur = 60,
    TextDissolve = 61,
    TextWipe = 62,
    TextChroma = 63,
    TextHalo = 64,
    TextHaloRadius = 68,
    /// Layer style (FX13): chromatic aberration (logical units), glitch, scanline depth and
    /// period, CRT curvature, vignette and flicker.
    StyleChroma = 69,
    StyleGlitch = 70,
    StyleScanlines = 71,
    StyleScanPeriod = 72,
    StyleCurvature = 73,
    StyleVignette = 74,
    StyleFlicker = 75,
    /// A wavy line (`Element::wavy_line`, T06): amplitude and wavelength in logical units,
    /// and the phase in waves.
    WaveAmp = 76,
    WaveLength = 77,
    WavePhase = 78,
    /// Text shadow offset (FX-013) in logical units, x then y, and the outline width
    /// (FX-018). Part of the text effects.
    TextShadowX = 79,
    TextShadowY = 80,
    TextOutline = 81,
    /// Text size morph (FX-048): the drawn size as a factor of the laid-out size, 1 at rest.
    /// Set by the scene when the font size changes, not by elements.
    TextZoom = 82,
    /// Displacement strength (EG-03, `Element::displace_strength`), 0 for none.
    DisplaceStrength = 83,
    /// Layer blur (EG-04, `Element::layer_blur`) and frost (`Element::frost`): standard
    /// deviations in logical units, 0 for none. Under reduced motion they jump.
    LayerBlur = 84,
    Frost = 85,
    /// Stage turns in radians and the push toward the viewer in logical units
    /// (`crate::stage`).
    RotateX = 100,
    RotateY = 101,
    TranslateZ = 102,
    /// Ink outside the box that the app declares (`Element::declare_reach`): left, top,
    /// right and bottom, in logical units. High numbers, clear of the effect slots.
    ReachLeft = 200,
    ReachTop = 201,
    ReachRight = 202,
    ReachBottom = 203,
    /// A repeater's child: its place on the ring (an angle) or path (a fraction), and its
    /// distance from it. Set by the repeater (`crate::repeat`).
    ItemAt = 204,
    ItemRadius = 205,
    /// A repeater's ring: radius, start and sweep; or path: offset, from and to.
    RepeatA = 206,
    RepeatB = 207,
    RepeatC = 208,
    /// A repeater's child moved out from the ring (`Element::radial`).
    Radial = 209,
}

/// Slots per bloom band: weight, reach, saturation and a four-slot tint.
pub(crate) const BAND_SLOTS: u8 = 7;

impl Slot {
    /// Value of a slot no element has set.
    pub(crate) fn default_value(slot: u8) -> f64 {
        if slot == Slot::Opacity as u8 || slot == Slot::Scale as u8 || slot == Slot::TextZoom as u8 {
            1.0
        } else {
            0.0
        }
    }

    pub(crate) fn prop(slot: u8) -> Prop {
        match slot {
            s if s >= Slot::ReachLeft as u8 => Slot::hud_prop(s),
            s if (Slot::RotateX as u8..=Slot::TranslateZ as u8).contains(&s) => Prop::Tilt,
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
            s if (Slot::BloomThreshold as u8..Slot::BloomBand0 as u8 + 3 * BAND_SLOTS).contains(&s) => Prop::Bloom,
            s if s < Slot::BloomStrength as u8 => Prop::Shadow,
            s if s < Slot::Shutter as u8 => Prop::Bloom,
            s if s < Slot::RimColor as u8 => Prop::MotionBlur,
            s if s < Slot::SheenColor as u8 => Prop::Rim,
            s if s < Slot::TrimStart as u8 => Prop::Sheen,
            s if s <= Slot::TrimEnd as u8 => Prop::Trim,
            s if s == Slot::Morph as u8 => Prop::Morph,
            s if s <= Slot::TextHaloRadius as u8 => Prop::TextFx,
            s if (Slot::TextShadowX as u8..=Slot::TextOutline as u8).contains(&s) => Prop::TextFx,
            s if s == Slot::TextZoom as u8 => Prop::TextSize,
            s if s == Slot::DisplaceStrength as u8 => Prop::Style,
            s if s == Slot::LayerBlur as u8 || s == Slot::Frost as u8 => Prop::Style,
            s if s <= Slot::StyleFlicker as u8 => Prop::Style,
            _ => Prop::Wave,
        }
    }

    /// Whether the slot holds a value from 0 to 1 (opacity, scale, colour, angle), which
    /// settles at a finer threshold than pixels.
    pub(crate) fn is_unit(slot: u8) -> bool {
        if slot >= Slot::ReachLeft as u8 {
            return Slot::hud_unit(slot);
        }
        !(slot <= Slot::H as u8
            || slot == Slot::OffsetX as u8
            || slot == Slot::OffsetY as u8
            || slot == Slot::Radius as u8
            || slot == Slot::StrokeWidth as u8
            || slot == Slot::GlowRadius as u8
            || (Slot::ShadowX as u8..=Slot::ShadowBlur as u8).contains(&slot)
            || slot == Slot::BloomRadius as u8
            || slot == Slot::RimWidth as u8
            || slot == Slot::TextBlur as u8
            || slot == Slot::LayerBlur as u8
            || slot == Slot::Frost as u8
            || slot == Slot::TextChroma as u8
            || slot == Slot::TextHaloRadius as u8
            || (Slot::TextShadowX as u8..=Slot::TextOutline as u8).contains(&slot)
            || slot == Slot::StyleChroma as u8
            || slot == Slot::StyleScanPeriod as u8
            || slot == Slot::WaveAmp as u8
            || slot == Slot::WaveLength as u8
            || slot == Slot::TranslateZ as u8)
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
    /// The motion blur shutter and the trail's half life.
    MotionBlur,
    /// Rim colour, width, span, angle and lap time.
    Rim,
    /// Sheen colour and sweep time.
    Sheen,
    /// A path's trim start and end (`Element::stroke_path`).
    Trim,
    /// A morph's progress (`Element::morph`).
    Morph,
    /// Text blur, dissolve, wipe, chromatic offset and halo.
    TextFx,
    /// A label's or live text's font size (FX-048). Name it in `.motion()` and a new size
    /// grows or shrinks from the old one, with the new size's line breaks from the start;
    /// without it a new size jumps.
    TextSize,
    /// A layer's style: chromatic aberration, glitch, scanlines, curvature, vignette and
    /// flicker.
    Style,
    /// A wavy line's amplitude, wavelength and phase (`Element::wavy_line`).
    Wave,
    /// A repeater's ring radius, start and sweep, or its path offset and range
    /// (`Element::polar`, `Element::along_path`).
    Ring,
    /// Stage turns and the push in z (`Element::rotate_x`, `rotate_y`, `translate_z`).
    Tilt,
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
    /// A stroked path's trim start and end: `(0, 0)` on enter draws it on, `(1, 1)` on exit
    /// draws it off from the start.
    pub trim: Option<(f32, f32)>,
    /// A morph's progress.
    pub morph: Option<f32>,
    /// Text blur in logical units: `blur(8.0)` on enter blurs text in.
    pub blur: Option<f32>,
    /// Text dissolve, 0 to 1: `dissolve(1.0)` on enter forms text out of noise.
    pub dissolve: Option<f32>,
    /// Text wipe, 0 to 1: `wipe(1.0)` on enter reveals text from the left.
    pub wipe: Option<f32>,
}

impl Transition {
    pub const NONE: Transition = Transition {
        opacity: None,
        offset: None,
        scale: None,
        trim: None,
        morph: None,
        blur: None,
        dissolve: None,
        wipe: None,
    };

    pub fn fade() -> Transition {
        Transition { opacity: Some(0.0), ..Transition::NONE }
    }

    pub fn opacity(mut self, v: f32) -> Transition {
        self.opacity = Some(v);
        self
    }

    /// Enter from (or exit to) this trim: `trim(0.0, 0.0)` draws a path on.
    pub fn trim(mut self, start: f32, end: f32) -> Transition {
        self.trim = Some((start, end));
        self
    }

    /// Enter from (or exit to) this morph progress.
    pub fn morph(mut self, v: f32) -> Transition {
        self.morph = Some(v);
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

    /// Enter from (or exit to) text blurred over `v` logical units.
    pub fn blur(mut self, v: f32) -> Transition {
        self.blur = Some(v);
        self
    }

    /// Enter from (or exit to) text dissolved this far, 0 to 1.
    pub fn dissolve(mut self, v: f32) -> Transition {
        self.dissolve = Some(v);
        self
    }

    /// Enter from (or exit to) text wiped this far, 0 to 1. Entering from 1 reveals the text
    /// from the left.
    pub fn wipe(mut self, v: f32) -> Transition {
        self.wipe = Some(v);
        self
    }

    /// The transition with reduced motion: its opacity, blur and dissolve, with no travel,
    /// scale or wipe.
    pub(crate) fn reduced(self) -> Transition {
        Transition { opacity: self.opacity, blur: self.blur, dissolve: self.dissolve, ..Transition::NONE }
    }

    /// The slot values this transition sets.
    pub(crate) fn slots(&self) -> impl Iterator<Item = (u8, f64)> {
        let o = self.opacity.map(|v| (Slot::Opacity as u8, v as f64));
        let x = self.offset.map(|(x, _)| (Slot::OffsetX as u8, x as f64));
        let y = self.offset.map(|(_, y)| (Slot::OffsetY as u8, y as f64));
        let s = self.scale.map(|v| (Slot::Scale as u8, v as f64));
        let t0 = self.trim.map(|(a, _)| (Slot::TrimStart as u8, a as f64));
        let t1 = self.trim.map(|(_, b)| (Slot::TrimEnd as u8, b as f64));
        let m = self.morph.map(|v| (Slot::Morph as u8, v as f64));
        let b = self.blur.map(|v| (Slot::TextBlur as u8, v as f64));
        let d = self.dissolve.map(|v| (Slot::TextDissolve as u8, v as f64));
        let w = self.wipe.map(|v| (Slot::TextWipe as u8, v as f64));
        [o, x, y, s, t0, t1, m, b, d, w].into_iter().flatten()
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
    /// Where each line sits across the text box (FX-002).
    pub align: crate::text::Align,
    /// Wrap to near-equal line lengths, as CSS `text-wrap: balance` (TY25): the narrowest
    /// width that keeps the greedy line count.
    pub balance: bool,
    /// One column, top to bottom, every character upright (FX-005, EG-30): shaped top to
    /// bottom with the font's vertical forms and metrics; see
    /// [`ShapedLayout::vertical`](crate::text::ShapedLayout::vertical). For labels, not live
    /// text. Wrapping, alignment, balance and line height do not apply.
    pub vertical: bool,
    /// Autofit (FX-049) as `(min size, step)`: a label is drawn at the largest of its size,
    /// size minus step, and so on down to the min size, that fits its box. See
    /// [`fit_size`](crate::text::fit_size). Labels only.
    pub fit: Option<(f32, f32)>,
    /// Build caret stops and line byte ranges ([`ShapedText::stops`](crate::text::ShapedText),
    /// [`Line::bytes`](crate::text::Line)) for editable text. Off by default; layout costs
    /// nothing extra then.
    pub stops: bool,
}

impl TextStyle {
    pub const fn new(font: u32, size: f32, line_height: f32) -> TextStyle {
        TextStyle {
            font,
            size,
            line_height,
            max_lines: 0,
            ellipsis: false,
            tracking: 0.0,
            align: crate::text::Align::Start,
            balance: false,
            vertical: false,
            fit: None,
            stops: false,
        }
    }

    /// With caret stops and line byte ranges, for editable text.
    pub fn with_stops(self) -> TextStyle {
        TextStyle { stops: true, ..self }
    }

    /// Each line placed across the box by `align`.
    pub fn align(self, align: crate::text::Align) -> TextStyle {
        TextStyle { align, ..self }
    }

    /// Each line centred in the box.
    pub fn centered(self) -> TextStyle {
        self.align(crate::text::Align::Center)
    }

    /// Lines of near-equal length.
    pub fn balanced(self) -> TextStyle {
        TextStyle { balance: true, ..self }
    }

    /// Set top to bottom in one column, every character upright (FX-005, EG-30).
    pub fn vertical(self) -> TextStyle {
        TextStyle { vertical: true, ..self }
    }

    /// Shrunk in `step`s, down to `min_size`, until the text fits its box (FX-049).
    pub fn fit(self, min_size: f32, step: f32) -> TextStyle {
        TextStyle { fit: Some((min_size, step)), ..self }
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
        /// Every glyph's lean in degrees, as an italic (`Element::glyph_lean`).
        lean: f32,
    },
    Shape(ShapeKind),
    /// A container drawn as its own display-list layer.
    Layer {
        blend: BlendMode,
        clip: bool,
        offscreen: bool,
        /// Edge fades of the clip: left, top, right, bottom.
        fade: [f32; 4],
        /// A mask for the parent layer element (`Element::mask`).
        mask: Option<Mask>,
        /// A backdrop (EG-01, `Element::backdrop`): its resolution and tint.
        backdrop: Option<(BackdropRes, Color)>,
        /// A displacement of the backdrop (EG-03, `Element::displace`) and whether it keeps
        /// the reading line still.
        displace: Option<(Box<Displace>, bool)>,
    },
    /// Live dictation text: lays out the words like a label, tags each glyph with its word
    /// id, and moves each word by its own channels. A new id enters from `enter`; a kept id
    /// whose text changed jumps to `change` and springs back.
    LiveText {
        words: Vec<Word>,
        style: TextStyle,
        enter: Transition,
        change: Transition,
        /// Words form from particles (`Element::word_particles`).
        particles: Option<WordParticles>,
        /// Caret and other live text motion (`crate::flow`). Boxed: it is the largest part of
        /// the largest `Kind`.
        live: Box<LiveMotion>,
        /// The word that holds the "current word" role (`Element::current_word`).
        current: Option<u32>,
        /// The decoration that follows the current word (`Element::word_mark`). Boxed to keep
        /// `Kind` small.
        mark: Option<Box<WordMark>>,
        /// The final-pass sweep (`Element::final_pass`).
        pass: Option<FinalPass>,
        /// Final text that each word takes once the pass reaches it (`Element::final_words`).
        finals: Vec<Word>,
        /// Inline objects by word id (`Element::inline_objects`, `crate::inline`).
        inline: Vec<(u32, crate::inline::Inline)>,
        /// Caret, selection and edit marks (`Element::edit`). Boxed to keep `Kind` small.
        edit: Option<Box<EditDecor>>,
    },
    /// A scrolling waveform filling the box.
    Wave(Wave),
    /// A bar-strip waveform over the level history, filling the box (`crate::bars`).
    Bars(Bars),
    /// A progress bar filling the box.
    Progress(Progress),
    /// Rings growing from the centre of the box (`crate::ambient`).
    Ripples(Ripples),
    /// A dot grid filling the box.
    Dots(Dots),
    /// A particle emitter at the centre of the box, simulated by the renderer.
    Emitter(Emitter),
    /// A filled path from `FontRegistry::add_path`: `view` (path units) fits the box,
    /// centred, keeping its aspect.
    Path {
        id: u32,
        view: Rect,
    },
    /// App drawing code run from the frame time (`crate::canvas`).
    Canvas(CanvasDraw),
    /// Content an app draws with its own GPU code (`crate::gpu`); `true` when it is split at
    /// the text plane (`Element::gpu_split`).
    Gpu(crate::gpu::GpuRef, bool),
    /// A stroked outline fitted to the box, trimmed by the trim channels.
    StrokePath(StrokeArt),
    /// A stroked morph fitted to the box at the morph channel's progress.
    Morph(MorphArt),
    /// A sine across the middle of the box, from the wave and trim channels.
    WavyLine(StrokeStyle),
    /// Children and ticks around a ring or along a path (`crate::repeat`).
    Repeat(crate::repeat::Repeat),
    /// A cut panel frame filling the box (`crate::frame`).
    Frame(crate::frame::Frame),
    /// Bars round a ring from band energies (`crate::spectrum`).
    Spectrum(crate::spectrum::Spectrum),
}

/// What `Element::stroke_path` draws. Compared by identity of the outline.
#[derive(Clone, Debug)]
pub struct StrokeArt {
    pub outline: std::sync::Arc<Outline>,
    /// The outline's own box, fitted to the element's box, centred, aspect kept.
    pub view: Rect,
    pub style: StrokeStyle,
    /// A light head running around the first contour: its tail length as a fraction of the
    /// contour, laps per second, colour and glow radius.
    pub head: Option<(f32, f32, Color, f32)>,
    /// With `fill`, the inside of the outline is filled in this colour under the stroke.
    pub fill: Option<Color>,
}

impl PartialEq for StrokeArt {
    fn eq(&self, o: &StrokeArt) -> bool {
        std::sync::Arc::ptr_eq(&self.outline, &o.outline)
            && self.view == o.view
            && self.style == o.style
            && self.head == o.head
            && self.fill == o.fill
    }
}

/// What `Element::morph` draws. Compared by identity of the morph.
#[derive(Clone, Debug)]
pub struct MorphArt {
    pub morph: std::sync::Arc<Morph>,
    pub view: Rect,
    pub style: StrokeStyle,
    /// Fill the shape instead of stroking it.
    pub filled: bool,
}

impl PartialEq for MorphArt {
    fn eq(&self, o: &MorphArt) -> bool {
        std::sync::Arc::ptr_eq(&self.morph, &o.morph)
            && self.view == o.view
            && self.style == o.style
            && self.filled == o.filled
    }
}

impl Kind {
    /// Same variant, so a node can be updated in place rather than replaced.
    pub(crate) fn same_variant(&self, other: &Kind) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    pub(crate) fn is_text(&self) -> bool {
        matches!(self, Kind::Label { .. } | Kind::LiveText { .. })
    }

    /// Whether `other` lays out differently from `self`. A live text's current-word mark,
    /// final pass and edit decor do not change its layout (only whether it has a decor does,
    /// for the caret stops); its live motion does, since the layout feeds it to the flow.
    pub(crate) fn layout_differs(&self, other: &Kind) -> bool {
        match (self, other) {
            (
                Kind::LiveText {
                    words: a,
                    style: sa,
                    inline: ia,
                    live: la,
                    enter: ea,
                    change: ca,
                    particles: pa,
                    edit: da,
                    ..
                },
                Kind::LiveText {
                    words: b,
                    style: sb,
                    inline: ib,
                    live: lb,
                    enter: eb,
                    change: cb,
                    particles: pb,
                    edit: db,
                    ..
                },
            ) => {
                a != b
                    || sa != sb
                    || la != lb
                    || ea != eb
                    || ca != cb
                    || pa != pb
                    || da.is_some() != db.is_some()
                    || ia.len() != ib.len()
                    || ia.iter().zip(ib).any(|(x, y)| x.0 != y.0 || x.1.width != y.1.width)
            }
            _ => other.is_text() && self != other,
        }
    }
}

/// A decoration that belongs to the "current word" role of a live text, not to a fixed word
/// id: when the role moves to another word, the mark moves with it (T04, T05).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordMark {
    pub shape: MarkShape,
    pub color: Color,
    /// With a second colour the mark runs from `color` on the left to this on the right.
    pub gradient: Option<Color>,
    /// The colour the current word's text takes, or `None` to keep the text colour. It
    /// leaves the old word and reaches the new one in about 150 ms.
    pub accent: Option<Color>,
    /// A gradient for the current word's text instead of one accent colour: left, middle
    /// and right (`WordMark::accent_gradient`).
    pub accent_ramp: Option<[Color; 3]>,
}

impl WordMark {
    pub fn new(shape: MarkShape, color: Color) -> WordMark {
        WordMark { shape, color, gradient: None, accent: None, accent_ramp: None }
    }

    /// Runs the mark from its colour on the left to `c` on the right.
    pub fn gradient(self, c: Color) -> WordMark {
        WordMark { gradient: Some(c), ..self }
    }

    /// Also tints the current word's text with `c`.
    pub fn accent(self, c: Color) -> WordMark {
        WordMark { accent: Some(c), ..self }
    }

    /// Fills the current word's text with a gradient instead: `c0` at the word's left, `mid`
    /// in its middle and `c1` at its right, by each glyph's centre. It moves from word to
    /// word like `accent`.
    pub fn accent_gradient(self, c0: Color, mid: Color, c1: Color) -> WordMark {
        WordMark { accent_ramp: Some([c0, mid, c1]), ..self }
    }

    pub(crate) fn tints(&self) -> bool {
        self.accent.is_some() || self.accent_ramp.is_some()
    }

    /// The accent colour at `share` (0 at the word's left, 1 at its right), or `None`.
    pub(crate) fn accent_at(&self, share: f32) -> Option<Color> {
        match self.accent_ramp {
            Some([c0, mid, c1]) => {
                let mix = |a: Color, b: Color, k: f32| {
                    let l = |x: f32, y: f32| x + (y - x) * k;
                    Color::rgba(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b), l(a.a, b.a))
                };
                let s = share.clamp(0.0, 1.0);
                Some(if s < 0.5 { mix(c0, mid, 2.0 * s) } else { mix(mid, c1, 2.0 * s - 1.0) })
            }
            None => self.accent,
        }
    }
}

/// How a [`WordMark`] looks and moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkShape {
    /// A rounded box behind the word. Both edges spring to the next word together.
    Box,
    /// A line under the word. Both edges spring to the next word together.
    Underline,
    /// A line under the word whose leading edge springs to the next word first and whose
    /// trailing edge follows, so it stretches over both words and then contracts.
    Karaoke,
}

/// Pointer and key handlers. Message values and plain functions, not closures, so elements
/// stay cheap to rebuild.
#[derive(Clone, Debug)]
pub(crate) struct Handlers<M> {
    pub hit: bool,
    pub click: Option<M>,
    pub hover: Option<(M, M)>,
    /// Makes the message for a key while this node has keyboard focus.
    pub key: Option<fn(crate::input::KeyEvent) -> M>,
}

impl<M> Default for Handlers<M> {
    fn default() -> Self {
        Handlers { hit: false, click: None, hover: None, key: None }
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
    /// Drawn as a glass panel under the content (FX06).
    pub(crate) glass: Option<Glass>,
    /// Asks the compositor to blur the desktop behind this outline in the box (EG-07).
    pub(crate) blur_behind: Option<GlassShape>,
    /// The material (FX15) of this element's content and of its children's, unless they set
    /// their own.
    pub(crate) material: Option<Material>,
    /// Phosphor persistence (FX15): the trail's half-life multiplier per channel.
    pub(crate) afterglow: Option<[f32; 3]>,
    /// How a trail's afterglow turns, grows and slides (EG-34).
    pub(crate) trail_drift: Option<TrailDrift>,
    pub(crate) motion: Vec<(Prop, Motion)>,
    /// How this node and its descendants move when no `.motion()` names a property
    /// (`Element::default_motion`); `None` takes the parent's.
    pub(crate) default_motion: Option<Motion>,
    /// Signal bus bindings (`crate::bind`).
    pub(crate) binds: Vec<crate::bind::Bound>,
    /// The signal store for the scene, on the root element (`Element::signals`).
    pub(crate) signals: Option<vo_core::signals::SignalsCell>,
    pub(crate) presence: Option<(Transition, Transition)>,
    pub(crate) handlers: Handlers<M>,
    pub(crate) children: Vec<Element<M>>,
    /// Ambient loops by slot (`Element::ambient`).
    pub(crate) loops: Vec<(u8, Loop)>,
}

impl<M> Element<M> {
    pub(crate) fn new(key: impl Into<Key>, kind: Kind, style: taffy::Style) -> Element<M> {
        Element {
            key: key.into(),
            kind,
            style,
            targets: Vec::new(),
            gradient: false,
            hairline: false,
            inside: false,
            glass: None,
            blur_behind: None,
            material: None,
            afterglow: None,
            trail_drift: None,
            motion: Vec::new(),
            default_motion: None,
            binds: Vec::new(),
            signals: None,
            presence: None,
            handlers: Handlers::default(),
            children: Vec::new(),
            loops: Vec::new(),
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
        let kind = Kind::Label { text: text.into(), style: TextStyle::default(), lean: 0.0 };
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
            particles: None,
            live: Box::default(),
            current: None,
            mark: None,
            pass: None,
            finals: Vec::new(),
            inline: Vec::new(),
            edit: None,
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

    /// A bar-strip waveform filling the box (`crate::bars`). Give it a size: it has no
    /// content size.
    pub fn bars(key: impl Into<Key>, bars: Bars) -> Element<M> {
        Element::new(key, Kind::Bars(bars), taffy::Style::default())
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

    /// Content drawn by the app's own GPU code in the box (`crate::gpu`), such as a 3D scene.
    /// The renderer needs a painter for the source's type. Give it a size: it has no content
    /// size.
    pub fn gpu(key: impl Into<Key>, source: std::sync::Arc<dyn crate::gpu::GpuSource>) -> Element<M> {
        Element::new(key, Kind::Gpu(crate::gpu::GpuRef(source), false), taffy::Style::default())
    }

    /// [`Element::gpu`] drawn in two halves cut at the text plane (EG-20, `crate::gpu`): what
    /// lies behind it is drawn here, what lies in front of it after the live text, so a 3D
    /// ring can pass behind the text and then in front of older lines. The front half never
    /// covers the newest line of live text. The painter must know the parts (`vo_3d` does).
    pub fn gpu_split(key: impl Into<Key>, source: std::sync::Arc<dyn crate::gpu::GpuSource>) -> Element<M> {
        Element::new(key, Kind::Gpu(crate::gpu::GpuRef(source), true), taffy::Style::default())
    }

    /// A stroked outline fitted to the box (`view` is its own box). Its trim is two animated
    /// fractions, 0 to 1 by default: `.trim(start, end)` moves them on the `Prop::Trim`
    /// motion, and `Transition::trim(0.0, 0.0)` in `.presence` draws it on as it enters.
    /// Stroke width is in logical units, not scaled with the fit.
    pub fn stroke_path(
        key: impl Into<Key>,
        outline: std::sync::Arc<Outline>,
        view: Rect,
        style: StrokeStyle,
    ) -> Element<M> {
        let art = StrokeArt { outline, view, style, head: None, fill: None };
        let mut e = Element::new(key, Kind::StrokePath(art), taffy::Style::default());
        e.set(Slot::TrimEnd as u8, 1.0);
        e
    }

    /// A wavy line across the middle of the box (T06): a sine with amplitude `amp` and
    /// `wavelength` in logical units, starting at the left edge. Trim draws it on and off
    /// (`.trim`, `Prop::Trim`, `Transition::trim`). `Prop::Wave` springs the amplitude,
    /// wavelength and phase, so `.wave_shape(0.0, wavelength)` straightens it.
    pub fn wavy_line(key: impl Into<Key>, style: StrokeStyle, amp: f32, wavelength: f32) -> Element<M> {
        let mut e = Element::new(key, Kind::WavyLine(style), taffy::Style::default());
        e.set(Slot::TrimEnd as u8, 1.0);
        e.wave_shape(amp, wavelength)
    }

    /// A wavy line's amplitude and wavelength in logical units. The wavelength is at least
    /// one unit.
    pub fn wave_shape(mut self, amp: f32, wavelength: f32) -> Element<M> {
        self.set(Slot::WaveAmp as u8, [amp, wavelength.max(1.0)]);
        self
    }

    /// A wavy line's phase in waves: 1 moves the crests one wavelength to the right.
    pub fn wave_phase(mut self, waves: f32) -> Element<M> {
        self.set(Slot::WavePhase as u8, waves);
        self
    }

    /// A stroked (or with `.morph_filled()`, filled) morph fitted to the box. Its progress is
    /// animated: `.morph_to(v)` moves it on the `Prop::Morph` motion. 0 shows `from`, 1 `to`;
    /// an underdamped spring overshoots past the shape.
    pub fn morph(key: impl Into<Key>, morph: std::sync::Arc<Morph>, view: Rect, style: StrokeStyle) -> Element<M> {
        Element::new(key, Kind::Morph(MorphArt { morph, view, style, filled: false }), taffy::Style::default())
    }

    /// The trim targets of a `stroke_path`, fractions of its whole length.
    pub fn trim(mut self, start: f32, end: f32) -> Element<M> {
        self.set(Slot::TrimStart as u8, [start, end]);
        self
    }

    /// The progress target of a `morph`.
    pub fn morph_to(mut self, v: f32) -> Element<M> {
        self.set(Slot::Morph as u8, v);
        self
    }

    /// Fills a `morph` instead of stroking it.
    pub fn morph_filled(mut self) -> Element<M> {
        if let Kind::Morph(m) = &mut self.kind {
            m.filled = true;
        }
        self
    }

    /// A light head on a `stroke_path`: a stretch `tail` (fraction of the first contour)
    /// long, fading out behind, running `laps` times a second, with a glow. The node then
    /// keeps the scene awake.
    pub fn light_head(mut self, tail: f32, laps: f32, color: Color, glow: f32) -> Element<M> {
        if let Kind::StrokePath(a) = &mut self.kind {
            a.head = Some((tail, laps, color, glow));
        }
        self
    }

    /// Fills a `stroke_path`'s inside under its stroke.
    pub fn path_fill(mut self, color: Color) -> Element<M> {
        if let Kind::StrokePath(a) = &mut self.kind {
            a.fill = Some(color);
        }
        self
    }

    /// A dot grid filling the box (`crate::dots`). Give it a size: it has no content size.
    pub fn dots(key: impl Into<Key>, dots: Dots) -> Element<M> {
        Element::new(key, Kind::Dots(dots), taffy::Style::default())
    }

    /// A particle emitter at the centre of the box (`crate::emitter`). The renderer runs its
    /// simulation; put it in a layer with bloom for a glow. Give it a size: particles outside
    /// the box are drawn, but layout only reserves the box.
    pub fn emitter(key: impl Into<Key>, emitter: Emitter) -> Element<M> {
        Element::new(key, Kind::Emitter(emitter), taffy::Style::default())
    }

    /// A container drawn as its own layer: group transform, blend mode, optional clip to
    /// its rounded box, optional offscreen target. Laid out as a flex column.
    pub fn layer(key: impl Into<Key>) -> Element<M> {
        let kind = Kind::Layer {
            blend: BlendMode::Normal,
            clip: false,
            offscreen: false,
            fade: [0.0; 4],
            mask: None,
            backdrop: None,
            displace: None,
        };
        Element::new(key, kind, Element::<M>::flex(FlexDirection::Column))
    }

    /// A mask (FX04) for the parent element, which must be a [`Element::layer`]: the parent
    /// shows only where the mask's children draw, by their alpha. The mask itself is not
    /// drawn, and its children may move, scale and fade like any others. It covers the
    /// parent's box and takes no space in its layout; `.layout(..)` changes that. Use
    /// [`Element::luma_mask`] and [`Element::invert_mask`] for the other modes. Only shapes,
    /// text, vectors and dots count in a mask.
    pub fn mask(key: impl Into<Key>) -> Element<M> {
        use taffy::style::{LengthPercentageAuto, Position};
        let kind = Kind::Layer {
            blend: BlendMode::Normal,
            clip: false,
            offscreen: false,
            fade: [0.0; 4],
            mask: Some(Mask::default()),
            backdrop: None,
            displace: None,
        };
        let mut style = Element::<M>::flex(FlexDirection::Column);
        style.position = Position::Absolute;
        style.inset = taffy::Rect {
            left: LengthPercentageAuto::length(0.0),
            top: LengthPercentageAuto::length(0.0),
            right: LengthPercentageAuto::length(0.0),
            bottom: LengthPercentageAuto::length(0.0),
        };
        Element::new(key, kind, style)
    }

    /// On a mask: mask by luminance, so white shows the parent and black hides it.
    pub fn luma_mask(mut self) -> Element<M> {
        match &mut self.kind {
            Kind::Layer { mask: Some(m), .. } => m.luma = true,
            _ => debug_assert!(false, "luma_mask on a node that is not a mask: {}", self.key),
        }
        self
    }

    /// On a mask: show the parent where the mask is empty and hide it where the mask draws,
    /// as a knockout.
    pub fn invert_mask(mut self) -> Element<M> {
        match &mut self.kind {
            Kind::Layer { mask: Some(m), .. } => m.invert = true,
            _ => debug_assert!(false, "invert_mask on a node that is not a mask: {}", self.key),
        }
        self
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

    pub fn child(mut self, mut child: Element<M>) -> Element<M> {
        self.adopt(&child);
        crate::repeat::adopt(&self.kind, &mut child.style);
        self.children.push(child);
        self
    }

    /// `child` painted behind the children so far, as the first child. For absolutely
    /// placed marks under text; an in-flow child would also move to the front of the flow.
    pub fn child_behind(mut self, mut child: Element<M>) -> Element<M> {
        self.adopt(&child);
        crate::repeat::adopt(&self.kind, &mut child.style);
        self.children.insert(0, child);
        self
    }

    pub fn children(mut self, children: impl IntoIterator<Item = Element<M>>) -> Element<M> {
        for c in children {
            self = self.child(c);
        }
        self
    }

    /// A mask is placed over its parent's box: the parent becomes its containing block, as
    /// `position: relative` does in CSS, which leaves the parent's own place unchanged.
    fn adopt(&mut self, child: &Element<M>) {
        let is_mask = matches!(child.kind, Kind::Layer { mask: Some(_), .. });
        if is_mask && self.style.position == taffy::style::Position::Static {
            self.style.position = taffy::style::Position::Relative;
        }
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

    /// Turns the element about its horizontal centre line in the stage camera
    /// (`crate::stage`): positive takes the top edge away from the viewer. The element gets a
    /// layer of its own, seen in perspective. A spring channel (`Prop::Tilt`).
    pub fn rotate_x(mut self, radians: f32) -> Element<M> {
        self.set(Slot::RotateX as u8, radians);
        self
    }

    /// Turns the element about its vertical centre line: positive takes the right edge away.
    pub fn rotate_y(mut self, radians: f32) -> Element<M> {
        self.set(Slot::RotateY as u8, radians);
        self
    }

    /// Moves the element toward the viewer by `z` logical pixels (negative pushes it back),
    /// so it shrinks or grows about the stage origin and moves with parallax.
    pub fn translate_z(mut self, z: f32) -> Element<M> {
        self.set(Slot::TranslateZ as u8, z);
        self
    }

    pub fn radius(mut self, r: f32) -> Element<M> {
        self.set(Slot::Radius as u8, r);
        self
    }

    /// Draws this panel as glass (FX06) under its content, with the sheen on show and, when
    /// `Glass::orbit` is set, the orbiting rim (FX08). Use it instead of a fill. With
    /// `Glass::blur_behind` the live host asks the compositor to blur the desktop under the
    /// panel (FX07) where it can. Only panels draw glass.
    pub fn glass(mut self, g: Glass) -> Element<M> {
        self.glass = Some(g);
        self
    }

    /// Asks the live host to have the compositor blur the desktop behind this element's
    /// `shape` in its box (EG-07), for a translucent fill or frame that is not glass.
    /// [`GlassShape::Rounded`] follows the element's corner radius. The region follows
    /// rotated and tilted layers to within one physical pixel, and holds the element only
    /// from [`crate::glass::BLUR_MIN_ALPHA`] opacity up. Where the compositor has no blur the
    /// element draws as it is and the host logs "no compositor blur" once.
    pub fn blur_behind(mut self, shape: GlassShape) -> Element<M> {
        self.blur_behind = Some(shape);
        self
    }

    /// Draws this element's shapes, glyphs and strokes with a material (FX15): neon,
    /// hologram, chrome, a gradient across a word, or an app's own (`vo_scene::material`).
    /// Children that set no material of their own get it too, so a material on a container
    /// covers everything in it. Its time starts when the element appears; with reduced motion
    /// (MO09) it stays still.
    pub fn material(mut self, m: impl Into<Material>) -> Element<M> {
        self.material = Some(m.into());
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

    /// Label: every glyph leans `deg` degrees about its own baseline origin, the top to the
    /// right for positive, as an italic does (`GlyphXform::skew_deg`). Layout is unchanged.
    /// For a counter whose digits lean with pace (EG-16, `flow::letter_motion::digit_lean`).
    pub fn glyph_lean(mut self, deg: f32) -> Element<M> {
        match &mut self.kind {
            Kind::Label { lean, .. } => *lean = deg,
            _ => debug_assert!(false, "glyph_lean on a node that is not a label: {}", self.key),
        }
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

    /// Live text: a caret after the last word (TY14). It jumps to a new end further on along
    /// its line, so it never stands inside the text, and glides back or to another line in
    /// about 120 ms. It stays solid while words arrive and for 400 ms after, then blinks at about
    /// 1 Hz.
    pub fn caret(mut self, c: Caret) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.caret = Some(c),
            _ => debug_assert!(false, "caret on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: the active-line reading stage (T14). The newest line holds its centre at
    /// `reading.line_y` from the top of the box, older lines recede upward and take the look
    /// of their age, and a word that rewraps onto the new line glides along the stage. Give
    /// the element a fixed height and clip it, since older lines move above the box.
    /// `Scene::reading_line` gives the newest line's box for placing the surface.
    pub fn reading(mut self, reading: crate::flow::Reading) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.reading = Some(reading),
            _ => debug_assert!(false, "reading on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: how the top line leaves when the text overflows and a line cap drops it
    /// (MO-33): it scrolls up and fades by default, or folds under or thins away where it is
    /// while the lines below move up. See [`crate::flow::overflow`].
    pub fn overflow(mut self, o: crate::flow::overflow::Overflow) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.overflow = o,
            _ => debug_assert!(false, "overflow on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: words set in a font of their own, by word id, such as a variable font
    /// instance from `FontRegistry::add_instance` (MO-81). The words keep the style's size
    /// and line height; a changed font springs the words after it to their new places.
    pub fn word_fonts(mut self, fonts: Vec<(vo_core::words::WordId, crate::font::FontId)>) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.word_fonts = fonts,
            _ => debug_assert!(false, "word_fonts on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: as [`Element::word_fonts`], with each word keeping the width the style's font
    /// gives it: its own font changes how its letters are drawn, never where it or the words
    /// after it stand, nor where lines break. For a voice that breathes through a variable
    /// font without moving the text (MO-81).
    pub fn word_fonts_in_place(mut self, fonts: Vec<(vo_core::words::WordId, crate::font::FontId)>) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => {
                live.word_fonts = fonts;
                live.word_fonts_in_place = true;
            }
            _ => debug_assert!(false, "word_fonts_in_place on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: draws a caret, a selection and edit marks with the words, at their sprung
    /// places (`flow::EditDecor`). Pass the buffer with `Element::live_source` so the decor's
    /// bytes match the text. While a node has a decor it lays out with caret stops and does
    /// not draw its end-of-text caret, unless the decor has no caret and sets `end_caret`.
    /// Changing the decor alone does not lay the text out again.
    pub fn edit(mut self, decor: EditDecor) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { edit, .. } => *edit = Some(Box::new(decor)),
            _ => debug_assert!(false, "edit on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: the still profile for editing (`LiveMotion::still`). Words that enter,
    /// change or leave only change ink, within 80 ms; reflow and scroll take at most 150 ms.
    /// Turning it off brings the node's own motion back for the next change.
    pub fn still_motion(mut self, on: bool) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.still = on,
            _ => debug_assert!(false, "still_motion on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: the text as typed, with runs of spaces and hard newlines kept (the edit
    /// layer's buffer). Each word is found in it in order; it is used only when nothing but
    /// whitespace lies around and between the words, else the words are joined by single
    /// spaces as without it (`flow::live_spans`).
    pub fn live_source(mut self, text: impl Into<String>) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.source = Some(text.into()),
            _ => debug_assert!(false, "live_source on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: the magnetic caret (MO-89). New words snap to the caret along their line,
    /// and the caret draws a field whose strength follows the voice level and which relaxes
    /// into concentric rings in silence. Needs a `caret`. See [`crate::flow::magnet`].
    pub fn magnetic_caret(mut self, m: crate::flow::magnet::Magnet) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.magnet = Some(m),
            _ => debug_assert!(false, "magnetic_caret on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: letter physics (MO-83). New letters drop into the line as small rigid
    /// bodies, bounce, nudge their neighbours and lock into place, within the reading-line
    /// envelope on the newest line. See [`crate::flow::letter_physics`].
    pub fn letter_physics(mut self, p: crate::flow::letter_physics::LetterPhysics) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.letters = Some(p),
            _ => debug_assert!(false, "letter_physics on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: per-letter motions (EG-16): tumbling arrivals, an italic arrival and the
    /// older lines on a wave. See [`crate::flow::letter_motion`].
    pub fn letter_motion(mut self, m: crate::flow::letter_motion::LetterMotions) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.letter_motion = m,
            _ => debug_assert!(false, "letter_motion on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: a mark in the left margin beside the newest line that splits when the
    /// text wraps, one half staying with the previous line and one going to the new line
    /// (MO-32). See [`crate::flow::margin`].
    pub fn margin_mark(mut self, m: crate::flow::margin::MarginMark) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.margin = Some(m),
            _ => debug_assert!(false, "margin_mark on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: the overflow spine (MO-34). Each line that a line cap drops leaves a strip
    /// of word-wide blocks in the margin above the text, which close into one history
    /// tick. See [`crate::flow::spine`].
    pub fn overflow_spine(mut self, s: crate::flow::spine::Spine) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.spine = Some(s),
            _ => debug_assert!(false, "overflow_spine on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: draws glyphs on the newest line in `tint` while they break the
    /// reading-line envelope (MO-08: ink under 70 %, more than 3 px off the baseline, scaled
    /// or blurred), so the check is visible live. For the station and tests, not for looks.
    pub fn envelope_tint(mut self, tint: Color) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.envelope_tint = Some(tint),
            _ => debug_assert!(false, "envelope_tint on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: how a new word's enter transition (`word_enter`) is applied.
    /// `Arrival::Readable` by default pulls it inside the reading-line envelope, so the word
    /// is readable in the frame it arrives; `Arrival::Free` takes it as given.
    pub fn word_arrival(mut self, a: crate::arrival::Arrival) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.arrival = a,
            _ => debug_assert!(false, "word_arrival on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: the margin bell (MO-31). The newest line's letters tighten a little as
    /// it nears the right edge and relax when the text wraps; with a carriage return the new
    /// line arrives a few pixels right and settles. See [`crate::flow::bell`].
    pub fn margin_bell(mut self, b: crate::flow::bell::Bell) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.bell = Some(b),
            _ => debug_assert!(false, "margin_bell on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: what a word whose text changes does with its glyphs: swap, crossfade, or a
    /// grapheme diff that moves only the changed glyphs. `Revise::Palimpsest` (MO-20) by default.
    pub fn word_revise(mut self, r: Revise) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.revise = r,
            _ => debug_assert!(false, "word_revise on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: what a revision does around the changed word (MO-21 to MO-23): width
    /// escrow, the correction hinge and the revision address. All off by default. See
    /// [`crate::flow::revisions`].
    pub fn revision_motion(mut self, r: crate::flow::revisions::RevisionMotion) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.revisions = r,
            _ => debug_assert!(false, "revision_motion on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: how a word moves to another line when the text reflows. `Rewrap::Fade` by
    /// default; `Rewrap::Glide` glides it there (T11).
    pub fn word_rewrap(mut self, r: Rewrap) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.rewrap = r,
            _ => debug_assert!(false, "word_rewrap on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: how `Revise::Glyphs` shows a letter whose case changed. `CaseChange::Roll`
    /// by default.
    pub fn word_case(mut self, c: CaseChange) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.case = c,
            _ => debug_assert!(false, "word_case on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: settling ink (T03). The newest words arrive lighter and a little looser and
    /// settle to full ink in 300 to 600 ms, slower for words revised often. A revision starts
    /// the settle again.
    pub fn settling_ink(mut self, ink: SettleInk) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.settle = Some(ink),
            _ => debug_assert!(false, "settling_ink on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: per-word typographic channels (MO-03): ink, weight, tracking, baseline and
    /// fill per word, each on a spring, clamped by the reading-line envelope on the newest
    /// line. See [`crate::typo`].
    pub fn word_typo(mut self, typos: crate::typo::Typos) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.typo = Some(typos),
            _ => debug_assert!(false, "word_typo on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: the words the recogniser marks stable. With settling ink, a word not in the
    /// list keeps its arrival ink until it is. Without this call every word settles by age.
    pub fn stable_words(mut self, ids: &[u32]) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.stable = Some(ids.to_vec()),
            _ => debug_assert!(false, "stable_words on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: these words keep their place in the line but draw no glyphs, because
    /// something else draws them for now: a GPU element that morphs a revised word, for
    /// example (`vo_3d::glyph_fx`).
    pub fn words_drawn_elsewhere(mut self, ids: &[u32]) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { live, .. } => live.elsewhere = ids.to_vec(),
            _ => debug_assert!(false, "words_drawn_elsewhere on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: each new or changed word forms from particles and hands over to the solid
    /// glyphs. A word is readable about 110 ms after it arrives; on a revision only the
    /// changed glyphs scatter and re-form. Drawn by the renderer; word ids must be unique
    /// among the live texts in a scene that use it.
    pub fn word_particles(mut self, look: WordParticles) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { particles, .. } => *particles = Some(look),
            _ => debug_assert!(false, "word_particles on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: gives the "current word" role to word `id`, or to no word. A
    /// [`Element::word_mark`] follows the role from word to word. An id that is not among the
    /// words counts as no word.
    pub fn current_word(mut self, id: Option<u32>) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { current, .. } => *current = id,
            _ => debug_assert!(false, "current_word on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: a decoration on the current word. Its edges spring from word to word, and
    /// when the next word is on another line it runs off the end of the old line and enters
    /// at the start of the new one (off the start and in from the end when it goes back).
    pub fn word_mark(mut self, m: WordMark) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { mark, .. } => *mark = Some(Box::new(m)),
            _ => debug_assert!(false, "word_mark on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: a reading pass at `p.at` (0 to 1) of the text, in reading order (T10). A
    /// hairline marks it, and each word brightens briefly as the pass reaches it. Feed it the
    /// finishing fraction; see `crate::final_pass`.
    pub fn final_pass(mut self, p: FinalPass) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { pass, .. } => *pass = Some(p),
            _ => debug_assert!(false, "final_pass on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: the final text of each word, by id. A word shows its final text from the
    /// moment the final pass reaches its start, so the revision (with `Revise::Glyphs`, the
    /// casing roll and the punctuation pop) plays as the line crosses the word. Words not
    /// listed keep their text.
    pub fn final_words(mut self, words: &[Word]) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { finals, .. } => *finals = words.to_vec(),
            _ => debug_assert!(false, "final_words on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: the words given to `live_text` are the final text; show `live` instead,
    /// each word until the final pass reaches it (MO-40). The same as `final_words` with the
    /// two lists the other way round, for an app whose view is built from the final words.
    pub fn final_from(mut self, live: &[Word]) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { words, finals, .. } => {
                *finals = std::mem::replace(words, live.to_vec());
            }
            _ => debug_assert!(false, "final_from on a node that is not live text: {}", self.key),
        }
        self
    }

    /// Live text: the words it shows. `None` for any other element.
    pub fn live_words(&self) -> Option<&[Word]> {
        match &self.kind {
            Kind::LiveText { words, .. } => Some(words),
            _ => None,
        }
    }

    /// Runs `f` on every live text in this tree, children first, and keeps what it returns.
    /// For an app that adds one behaviour to every live text its views build.
    pub fn map_live_texts(mut self, f: &mut dyn FnMut(Element<M>) -> Element<M>) -> Element<M> {
        self.children = std::mem::take(&mut self.children).into_iter().map(|c| c.map_live_texts(f)).collect();
        if matches!(self.kind, Kind::LiveText { .. }) {
            f(self)
        } else {
            self
        }
    }

    /// Live text: inline objects (FX-063). Each id names a word whose text is
    /// [`OBJECT_TEXT`](crate::inline::OBJECT_TEXT) (make one with `Inline::word`); the word
    /// takes the object's width in the line, wraps like a word, springs when the width
    /// changes, and draws the object's mark instead of a glyph.
    pub fn inline_objects(mut self, objects: Vec<(u32, crate::inline::Inline)>) -> Element<M> {
        match &mut self.kind {
            Kind::LiveText { inline, .. } => *inline = objects,
            _ => debug_assert!(false, "inline_objects on a node that is not live text: {}", self.key),
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

    /// Clip the layer and fade its content out toward the edges (FX04): over `left`, `top`,
    /// `right` and `bottom` logical units from each edge of the box, 0 for a hard edge. For
    /// tickers and scrolling text, whose words should not end at a cut.
    pub fn fade_edges(mut self, left: f32, top: f32, right: f32, bottom: f32) -> Element<M> {
        if let Kind::Layer { clip, fade, .. } = &mut self.kind {
            *clip = true;
            *fade = [left, top, right, bottom].map(|v| v.max(0.0));
        } else {
            debug_assert!(false, "fade_edges on a node that is not a layer: {}", self.key);
        }
        self
    }

    /// A rim light on the element's outline (FX08): a band of `c`, `span` turns wide each
    /// side, on a stroke `width` wide, centred on the direction `angle` (turns, clockwise
    /// from +x: -0.125 is up and right, -0.375 up and left). With `period` above 0 the band
    /// orbits the outline once every `period` seconds, drawn at 30 fps; 0 keeps it still, as
    /// a specular edge. Reduced motion stops the orbit. For panels, layers and shapes.
    pub fn rim(mut self, c: Color, width: f32, span: f32, angle: f32, period: f32) -> Element<M> {
        self.set(Slot::RimColor as u8, c);
        self.set(Slot::RimWidth as u8, width.max(0.0));
        self.set(Slot::RimSpan as u8, span.max(1e-3));
        self.set(Slot::RimAngle as u8, angle);
        self.set(Slot::RimPeriod as u8, period.max(0.0));
        self.motion.push((Prop::Rim, Motion::Snap));
        self
    }

    /// A sheen (FX08): a soft slanted band of `c` sweeps once across the element's shape,
    /// left to right over `secs` seconds, when the element enters. Reduced motion leaves it
    /// out. For panels, layers and shapes.
    pub fn sheen(mut self, c: Color, secs: f32) -> Element<M> {
        self.set(Slot::SheenColor as u8, c);
        self.set(Slot::SheenSecs as u8, secs.max(1e-3));
        self.motion.push((Prop::Sheen, Motion::Snap));
        self
    }

    /// Text blur (TY17) over `v` logical units, for labels and live text. Animate it with
    /// `Prop::TextFx`, or blur text in with `Transition::blur`.
    pub fn text_blur(mut self, v: f32) -> Element<M> {
        self.set(Slot::TextBlur as u8, v.max(0.0));
        self
    }

    /// Text dissolve (TY17), 0 to 1: holes from a noise fixed to each glyph grow until the
    /// text is gone at 1.
    pub fn text_dissolve(mut self, v: f32) -> Element<M> {
        self.set(Slot::TextDissolve as u8, v.clamp(0.0, 1.0));
        self
    }

    /// Text wipe (TY17), 0 to 1: the part of each line hidden from the right, with a soft
    /// front. Going from 1 to 0 reveals the text from the left.
    pub fn text_wipe(mut self, v: f32) -> Element<M> {
        self.set(Slot::TextWipe as u8, v.clamp(0.0, 1.0));
        self
    }

    /// Chromatic offset (TY17): the red of the text moves `v` logical units left and the
    /// blue the same right.
    pub fn text_chroma(mut self, v: f32) -> Element<M> {
        self.set(Slot::TextChroma as u8, v);
        self
    }

    /// A halo (TY17) of `c` under the text, reaching about `radius` logical units past the
    /// glyphs. Drawn in the glyph shader, with no offscreen pass.
    pub fn text_halo(mut self, c: Color, radius: f32) -> Element<M> {
        self.set(Slot::TextHalo as u8, c);
        self.set(Slot::TextHaloRadius as u8, radius.max(0.0));
        self
    }

    /// A drop shadow (FX-013) of `c` under the text, moved by `dx`, `dy` and softened over
    /// about `blur`, in logical units, as CSS `text-shadow: dx dy blur c`. It is the halo
    /// moved, so it takes the place of [`Element::text_halo`]. Drawn in the glyph shader,
    /// with no offscreen pass.
    pub fn text_shadow(mut self, c: Color, dx: f32, dy: f32, blur: f32) -> Element<M> {
        self.set(Slot::TextHalo as u8, c);
        self.set(Slot::TextHaloRadius as u8, (0.5 * blur).max(0.5));
        self.set(Slot::TextShadowX as u8, [dx, dy]);
        self
    }

    /// Outlined text (FX-018): each glyph's edge stroked `width` logical units wide, in the
    /// text colour, with the inside left empty. For labels and live text; the width
    /// animates with `Prop::TextFx`. A halo or shadow still follows the filled shape.
    pub fn text_outline(mut self, width: f32) -> Element<M> {
        self.set(Slot::TextOutline as u8, width.max(0.0));
        self
    }

    /// A glow around the element's shape: `c` fading out over about `radius` logical units,
    /// at `strength` (1 is the colour at the shape's edge; above 1 is brighter than the
    /// colour and lights a bloom around it). For elements that draw a shape: panels, layers
    /// and shapes. Text glows with [`Element::text_halo`], or through a [`Element::bloom`]
    /// layer around it.
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

    /// Sets a style slot on a layer and makes it offscreen, where the style is composited.
    fn style(mut self, slot: Slot, v: f32) -> Element<M> {
        if let Kind::Layer { offscreen, .. } = &mut self.kind {
            *offscreen = true;
        } else {
            debug_assert!(false, "a layer style on a node that is not a layer: {}", self.key);
        }
        self.set(slot as u8, v);
        self
    }

    /// Chromatic aberration (FX13) on a layer: its red and blue move apart, outward from the
    /// centre, by `v` logical units at the edge.
    pub fn chromatic(self, v: f32) -> Element<M> {
        self.style(Slot::StyleChroma, v)
    }

    /// Glitch (FX13) on a layer, 0 to 1: horizontal slices jump sideways, a new pattern 15
    /// times a second. Reduced motion turns it off.
    pub fn glitch(self, v: f32) -> Element<M> {
        self.style(Slot::StyleGlitch, v.clamp(0.0, 1.0))
    }

    /// Scanlines (FX13) on a layer: dark lines `period` logical units apart, `depth` 0 to 1.
    pub fn scanlines(self, depth: f32, period: f32) -> Element<M> {
        self.style(Slot::StyleScanPeriod, period.max(1.0)).style(Slot::StyleScanlines, depth.clamp(0.0, 1.0))
    }

    /// CRT curvature (FX13) on a layer, 0 to about 0.3: the content bows out as on a curved
    /// screen.
    pub fn crt(self, curvature: f32) -> Element<M> {
        self.style(Slot::StyleCurvature, curvature.clamp(0.0, 1.0))
    }

    /// A vignette (FX13) on a layer, 0 to 1: its corners darken.
    pub fn vignette(self, v: f32) -> Element<M> {
        self.style(Slot::StyleVignette, v.clamp(0.0, 1.0))
    }

    /// Flicker (FX13) on a layer: its brightness dips by up to `v` (at most 0.2), a new level
    /// six times a second, so it flashes at most three times a second. Reduced motion turns
    /// it off.
    pub fn flicker(self, v: f32) -> Element<M> {
        self.style(Slot::StyleFlicker, v.clamp(0.0, 0.2))
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

    /// The shape of a layer's bloom (bloom v2): an emissive threshold and three bands, a tight
    /// hot core, a saturated middle and a wide faint tail, instead of one blur. Use with
    /// [`Element::bloom`], whose radius is the widest band's spread.
    /// `BloomShape::NEON` is the concepts' neon. Every value animates, as `Prop::Bloom`.
    pub fn bloom_shape(mut self, shape: BloomShape) -> Element<M> {
        self.set(Slot::BloomThreshold as u8, shape.threshold.max(0.0));
        for (i, b) in shape.bands.iter().enumerate() {
            let s = Slot::BloomBand0 as u8 + i as u8 * BAND_SLOTS;
            self.set(s, b.weight.max(0.0));
            self.set(s + 1, b.reach.max(0.0));
            self.set(s + 2, b.saturation.max(0.0));
            self.set(s + 3, b.tint);
        }
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

    /// A feedback trail on a layer (FX11): what the layer drew stays and fades to half every
    /// `half_life` seconds, so moving content leaves a comet tail. Makes the layer offscreen
    /// at the size of the surface. Seeking back or a gap over half a second starts it fresh.
    pub fn trail(mut self, half_life: f32) -> Element<M> {
        if let Kind::Layer { offscreen, .. } = &mut self.kind {
            *offscreen = true;
        } else {
            debug_assert!(false, "trail on a node that is not a layer: {}", self.key);
        }
        self.set(Slot::TrailHalfLife as u8, half_life.max(1e-3));
        self.motion.push((Prop::MotionBlur, Motion::Snap));
        self
    }

    /// Trails that drift and turn (EG-34): the trail's afterglow turns, grows and slides about
    /// `drift.pivot` (in this element's own space) while it fades, so a still dot off the
    /// pivot leaves a curved streak and moving content leaves swirls. Frame rate independent.
    /// A drifting trail takes a second texture the size of the surface, and keeps the scene
    /// drawing every frame while its layer is alive. Under reduced motion, and with a drift
    /// that moves nothing, the trail stays in place. Call after `trail`.
    pub fn trail_drift(mut self, drift: TrailDrift) -> Element<M> {
        debug_assert!(
            self.targets.iter().any(|(k, _)| *k == Slot::TrailHalfLife as u8),
            "trail_drift without trail: {}",
            self.key
        );
        self.trail_drift = Some(drift);
        self
    }

    /// Phosphor persistence (FX15) on a layer: a feedback trail whose channels fade at their
    /// own rate, the colours `afterglow` lacks first, so a white trace or wave cools to the
    /// phosphor's colour (green for P1, amber for P3) as it fades over about `half_life`
    /// seconds. Fading can only take colour away: a trace already in the afterglow's hue keeps
    /// it. Add `bloom` for the beam's glow. Only on a layer element.
    pub fn phosphor(mut self, half_life: f32, afterglow: Color) -> Element<M> {
        let lin = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        let c = [afterglow.r, afterglow.g, afterglow.b].map(lin);
        let top = c[0].max(c[1]).max(c[2]).max(1e-4);
        self.afterglow = Some(c.map(|v| 0.2 + 0.8 * v / top));
        self.trail(half_life)
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

    /// A backdrop (EG-01) on a layer: before the layer draws, what is already drawn under its
    /// rounded box is captured into a texture at `res`, and drawn back multiplied by `tint`
    /// (premultiplied, so its alpha fades it). A transparent tint only captures, for effects
    /// that read the backdrop. Only this surface's own drawing is captured, never the desktop.
    /// The frame takes the effect path; a mask or a lower quality tier has no backdrop.
    pub fn backdrop(mut self, res: BackdropRes, tint: Color) -> Element<M> {
        if let Kind::Layer { backdrop, mask: None, .. } = &mut self.kind {
            *backdrop = Some((res, tint));
        } else {
            debug_assert!(false, "backdrop on a node that is not a layer: {}", self.key);
        }
        self
    }

    /// A displacement (EG-03, `crate::displace`) on a layer: what is already drawn under its
    /// rounded box is captured (a full-resolution backdrop) and drawn back with each pixel
    /// moved by `d`, before the layer's own content. Replaces a backdrop's tint. Strength
    /// starts at 1 (`displace_strength`). The newest line of live text stays sharp and still
    /// unless `displace_reading_line(true)`. Reduced motion, a mask or a lower quality tier
    /// draws the layer undisplaced.
    pub fn displace(mut self, d: Displace) -> Element<M> {
        if let Kind::Layer { displace, mask: None, .. } = &mut self.kind {
            *displace = Some((Box::new(d), true));
        } else {
            debug_assert!(false, "displace on a node that is not a layer: {}", self.key);
            return self;
        }
        if !self.targets.iter().any(|(k, _)| *k == Slot::DisplaceStrength as u8) {
            self.set(Slot::DisplaceStrength as u8, 1.0);
        }
        self
    }

    /// How strongly the displacement moves pixels: 1 as described, 0 not at all (and no
    /// pass at all). Animated like a style; bind it with `Param::DisplaceStrength`.
    pub fn displace_strength(mut self, v: f32) -> Element<M> {
        self.set(Slot::DisplaceStrength as u8, v.max(0.0));
        self
    }

    /// Lets the displacement move the newest line of live text too. Only for text that is
    /// not read: the reading-line envelope holds there by default.
    pub fn displace_reading_line(mut self, moves: bool) -> Element<M> {
        if let Kind::Layer { displace: Some((_, keep)), .. } = &mut self.kind {
            *keep = !moves;
        }
        self
    }

    /// Blurs the layer's own content (EG-04): a Gaussian of standard deviation `radius`
    /// logical units, reaching three times as far. Animated like a style; under reduced
    /// motion it jumps to its target. 0 draws the layer as without it, with no pass. The
    /// layer is drawn offscreen. The newest line of live text inside the layer stays sharp
    /// (the reading-line envelope). Only on the full quality tier; `minimal` and `no-bloom`
    /// draw the layer sharp.
    pub fn layer_blur(mut self, radius: f32) -> Element<M> {
        if !matches!(self.kind, Kind::Layer { mask: None, .. }) {
            debug_assert!(false, "layer_blur on a node that is not a layer: {}", self.key);
            return self;
        }
        self.set(Slot::LayerBlur as u8, radius.max(0.0));
        self
    }

    /// Frosts what is under the layer (EG-04): its backdrop (EG-01) is captured at full
    /// resolution and drawn back blurred by `radius` logical units (standard deviation) inside
    /// the layer's rounded box, before the layer's own content, so a sheet frosts the lines
    /// behind it. Replaces a backdrop's tint; tint with the layer's own fill. Animated like
    /// `layer_blur`; 0 captures and draws nothing. Only on the full quality tier.
    pub fn frost(mut self, radius: f32) -> Element<M> {
        if !matches!(self.kind, Kind::Layer { mask: None, .. }) {
            debug_assert!(false, "frost on a node that is not a layer: {}", self.key);
            return self;
        }
        self.set(Slot::Frost as u8, radius.max(0.0));
        self
    }

    // Motion.

    /// How properties of this node and of every node below it move when no `.motion()` names
    /// them, in place of the scene's default (`Scene::with_default_motion`). A descendant's
    /// own `default_motion` wins inside its subtree. For several designs in one scene, such
    /// as the cells of a theme wall: one cell snaps, the next springs.
    pub fn default_motion(mut self, motion: impl Into<Motion>) -> Element<M> {
        self.default_motion = Some(motion.into());
        self
    }

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
    /// Runs an ambient loop on `prop` while the view declares it (FX-203): its offset is
    /// added to the property's value. When a later view leaves it out, or the node leaves,
    /// the loop settles to its rest, so the scene can go idle. One loop per property; a
    /// second call for the same property replaces the first.
    pub fn ambient(mut self, prop: LoopProp, l: Loop) -> Element<M> {
        let slot = prop.slot();
        self.loops.retain(|(s, _)| *s != slot);
        self.loops.push((slot, l));
        self
    }

    /// Rings that grow from the centre of the box and fade, a new one every `every` seconds
    /// while `active` is set (FX-101). Turning `active` off lets the rings on screen finish.
    pub fn ripples(key: impl Into<Key>, ripples: Ripples) -> Element<M> {
        Element::new(key, Kind::Ripples(ripples), taffy::Style::default())
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

    /// Makes the node focusable: while the app has keyboard focus, keys go to the focused
    /// node first, as `msg(key)`, and reach `App::key` only when no node is focused. The
    /// scene focuses the first focusable node in tree order (one per scene for now), and a
    /// node that is removed or leaves loses focus. `msg` is usually a message constructor,
    /// such as `Msg::Key`.
    pub fn on_key(mut self, msg: fn(crate::input::KeyEvent) -> M) -> Element<M> {
        self.handlers.key = Some(msg);
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
