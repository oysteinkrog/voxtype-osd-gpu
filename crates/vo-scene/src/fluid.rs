//! Ink and smoke (EG-12): a fluid layer that dictation seeds and the dye draws.
//!
//! [`FluidLayer`] places a fluid (the EG-11 simulation, `vo_sim::fluid`) behind or around the
//! text. The words seed it as recorded events ([`InkWord`]): dye where a word lands, taken from
//! its laid-out box, a push from its onset, and both scaled by the voice level at that moment.
//! [`FluidLayer::splats`] turns the words into the splat list ([`InkSplat`]); the fluid at time
//! `t` is a function of `t` and that list only, so a seek or an export draws the same frame.
//!
//! The layer builds up to three things:
//!
//! - a GPU element ([`FluidSource`]) whose painter steps the simulation to the frame's time
//!   before the dye is drawn. `vo_sim::fluid_render` holds the painter, the WGSL and the
//!   install call; the scene knows nothing about the simulation;
//! - a panel over the layer's box whose material ([`FLUID_MATERIAL`]) draws the dye texture in
//!   one of three styles ([`InkStyle`]): ink in water, luminous smoke (added light, which a
//!   bloom picks up: [`FluidLayer::glow`]) and watercolour (soft edges, a darker rim, a little
//!   paper grain);
//! - optionally ([`FluidLayer::smear`]), a displacement layer (EG-03) that moves what is drawn
//!   under it along the flow, read from the velocity texture. Older lines smear; the newest
//!   (reading) line is kept as captured, so it never moves.
//!
//! The reading line: [`FluidLayer::keep_clear`] takes the band of the newest line (its top and
//! bottom in the layer's space). No dye is drawn in that band, and it fades in over a few units
//! outside it, so the line being read sits on the plain background and its pixels are what
//! they would be without the layer. Dye seeded there blooms above and below it, and shows in
//! full once the line moves up.
//!
//! [`FluidLayer::view`] is the dye alone (put it before the text to draw it behind),
//! [`FluidLayer::smear_view`] the displacement (put it after the text), and
//! [`FluidLayer::around`] both around some content.
//!
//! Reduced motion (`VO_REDUCED_MOTION=1` by default): pushes are dropped and the fluid does not
//! step, so dye appears where words land and does not drift; nothing is smeared. Light mode
//! (`VO_LIGHT=1` by default): every style darkens the paper instead of glowing.
//!
//! Parameter blocks of the material: slot 0 is the box in layer units, slot 1 the style, the
//! dye image slot, the light flag and the opacity, slot 2 the softening (layer units), rim,
//! grain and density, slot 3 the clear band (top, bottom, fade; bottom at or above top for
//! none).

use std::any::Any;
use std::sync::Arc;

use crate::displace::Displace;
use crate::element::{Element, Key};
use crate::geom::{Color, Point, Rect, Size};
use crate::gpu::GpuSource;
use crate::material::Material;
use crate::text::ShapedText;

/// The material function id of the dye. `vo_sim::fluid_render::install` registers it.
pub const FLUID_MATERIAL: u32 = crate::material::CUSTOM_FIRST + 193;

/// Seconds the fluid keeps moving after its last splat (the default 1.5 s velocity fade takes
/// a push to under 2 % in 6 s).
pub const SETTLE_SECS: f64 = 6.0;

/// The default image slot of the dye texture.
pub const DEFAULT_DYE_SLOT: u32 = 1;
/// The default image slot of the velocity texture.
pub const DEFAULT_VELOCITY_SLOT: u32 = 2;

/// Layer units over which the dye fades in outside the clear band.
pub const CLEAR_FADE: f32 = 6.0;

/// How the dye is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InkStyle {
    /// Ink in water: dense colour with a soft falloff, painted over what is under it.
    #[default]
    Ink,
    /// Luminous smoke: soft, added light. Glows under a bloom.
    Smoke,
    /// Watercolour: soft edges, pigment pooling darker at the rim, a little paper grain.
    Watercolour,
}

impl InkStyle {
    fn index(self) -> f32 {
        match self {
            InkStyle::Ink => 0.0,
            InkStyle::Smoke => 1.0,
            InkStyle::Watercolour => 2.0,
        }
    }
}

/// A word that landed, as a recorded event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InkWord {
    /// Scene time the word arrived (its onset), in seconds.
    pub t: f64,
    /// The word's laid-out box, in the fluid layer's space (from its top left).
    pub rect: Rect,
    /// Voice level at the onset, 0 to 1: the source's strength.
    pub level: f32,
}

/// One splat of the fluid at a recorded time: a smooth bump of push and dye. Places are uv over
/// the layer's box, as `vo_sim::fluid::FluidSplat` takes them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InkSplat {
    pub t: f64,
    /// Centre, 0 to 1 across the box.
    pub x: f32,
    /// Centre, 0 to 1 down the box.
    pub y: f32,
    /// Radius as a fraction of the box's width.
    pub radius: f32,
    /// Push in box widths (x) and heights (y) per second.
    pub velocity: [f32; 2],
    /// Dye colour, linear light, 0 to 1.
    pub color: [f32; 3],
    /// Dye amount at the centre. 1 is full.
    pub amount: f32,
}

/// What the layer's GPU element hands its painter: the splats and the motion setting.
#[derive(Clone, Debug, PartialEq)]
pub struct FluidSource {
    pub splats: Vec<InkSplat>,
    pub reduced_motion: bool,
}

impl GpuSource for FluidSource {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn until(&self) -> f64 {
        let last = self.splats.iter().map(|s| s.t).fold(f64::NEG_INFINITY, f64::max);
        if self.reduced_motion {
            last
        } else {
            last + SETTLE_SECS
        }
    }
}

/// A fluid layer. See the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct FluidLayer {
    /// The layer's box size.
    pub area: Size,
    /// Its top left in the parent.
    pub at: Point,
    pub words: Vec<InkWord>,
    /// Extra splats, after the words'.
    pub extra: Vec<InkSplat>,
    pub style: InkStyle,
    /// Word `i` takes colour `i % len`. Straight sRGB. Default: a blue, a violet and a teal.
    pub palette: Vec<Color>,
    /// The dye's opacity, 0 to 1. Default 1.
    pub opacity: f32,
    /// How far the flow moves what is drawn under [`FluidLayer::smear_view`]: seconds of
    /// travel at the local speed. 0 (the default) draws no smear.
    pub smear: f32,
    /// Bloom strength on the dye, for smoke. 0 (the default) adds no bloom.
    pub glow: f32,
    /// How strongly pushes move the fluid, times the default. Default 1.
    pub push: f32,
    /// The band kept clear of dye, top and bottom in layer space: the reading line.
    pub clear: Option<(f32, f32)>,
    pub dye_slot: u32,
    pub velocity_slot: u32,
    pub light: bool,
    pub reduced_motion: bool,
}

impl FluidLayer {
    /// A fluid layer of size `area` at `at` in its parent. Light mode and reduced motion
    /// default from `VO_LIGHT=1` and `VO_REDUCED_MOTION=1`.
    pub fn new(area: Size, at: Point) -> FluidLayer {
        let env = |k: &str| std::env::var(k).is_ok_and(|v| v == "1");
        FluidLayer {
            area,
            at,
            words: Vec::new(),
            extra: Vec::new(),
            style: InkStyle::Ink,
            palette: vec![Color::hex(0x3a7bff, 1.0), Color::hex(0x9b5cff, 1.0), Color::hex(0x19c2b0, 1.0)],
            opacity: 1.0,
            smear: 0.0,
            glow: 0.0,
            push: 1.0,
            clear: None,
            dye_slot: DEFAULT_DYE_SLOT,
            velocity_slot: DEFAULT_VELOCITY_SLOT,
            light: env("VO_LIGHT"),
            reduced_motion: env("VO_REDUCED_MOTION"),
        }
    }

    pub fn words(mut self, words: impl IntoIterator<Item = InkWord>) -> FluidLayer {
        self.words.extend(words);
        self
    }

    pub fn word(mut self, w: InkWord) -> FluidLayer {
        self.words.push(w);
        self
    }

    /// Adds a splat of its own, drawn after the words' splats.
    pub fn splat(mut self, s: InkSplat) -> FluidLayer {
        self.extra.push(s);
        self
    }

    pub fn style(self, style: InkStyle) -> FluidLayer {
        FluidLayer { style, ..self }
    }

    pub fn palette(self, palette: impl IntoIterator<Item = Color>) -> FluidLayer {
        let palette: Vec<Color> = palette.into_iter().collect();
        if palette.is_empty() {
            return self;
        }
        FluidLayer { palette, ..self }
    }

    pub fn opacity(self, opacity: f32) -> FluidLayer {
        FluidLayer { opacity: opacity.clamp(0.0, 1.0), ..self }
    }

    pub fn smear(self, secs: f32) -> FluidLayer {
        FluidLayer { smear: secs.max(0.0), ..self }
    }

    pub fn glow(self, strength: f32) -> FluidLayer {
        FluidLayer { glow: strength.max(0.0), ..self }
    }

    pub fn push(self, push: f32) -> FluidLayer {
        FluidLayer { push: push.max(0.0), ..self }
    }

    /// Keeps the band from `top` to `bottom` (layer space) clear of dye: the reading line. The
    /// dye fades in over [`CLEAR_FADE`] units outside it.
    pub fn keep_clear(self, top: f32, bottom: f32) -> FluidLayer {
        FluidLayer { clear: Some((top, bottom)), ..self }
    }

    /// The image slots of the dye and velocity textures (`Renderer::set_material_image`).
    pub fn slots(self, dye: u32, velocity: u32) -> FluidLayer {
        FluidLayer { dye_slot: dye, velocity_slot: velocity, ..self }
    }

    pub fn light(self, light: bool) -> FluidLayer {
        FluidLayer { light, ..self }
    }

    pub fn reduced_motion(self, reduced_motion: bool) -> FluidLayer {
        FluidLayer { reduced_motion, ..self }
    }

    /// The splats the words make, in word order, then the extra ones. Each word gives:
    ///
    /// - at its onset, dye at two points along its box (a quarter in from each end), as wide
    ///   as about a third of the line height, and as much as `0.35 + 0.45 * level`;
    /// - 0.12 s later, half as much again at the box's centre: the source lingering;
    /// - at its onset, a push from the box's centre, up and to one side (alternating by word),
    ///   faster with the level.
    pub fn splats(&self) -> Vec<InkSplat> {
        let (aw, ah) = (self.area.w.max(1.0), self.area.h.max(1.0));
        let mut out = Vec::with_capacity(self.words.len() * 4 + self.extra.len());
        for (i, w) in self.words.iter().enumerate() {
            let level = w.level.clamp(0.0, 1.0);
            let c = self.palette[i % self.palette.len()];
            let color = [linear(c.r), linear(c.g), linear(c.b)];
            let cx = (w.rect.x + 0.5 * w.rect.w) / aw;
            let cy = (w.rect.y + 0.5 * w.rect.h) / ah;
            let quarter = 0.25 * w.rect.w / aw;
            let radius = (0.35 * w.rect.h / aw).max(0.012) * (0.8 + 0.4 * level);
            let amount = 0.35 + 0.45 * level;
            for dx in [-quarter, quarter] {
                out.push(InkSplat { t: w.t, x: cx + dx, y: cy, radius, velocity: [0.0; 2], color, amount });
            }
            let side = if i % 2 == 0 { 1.0 } else { -1.0 };
            let speed = self.push * (0.4 + 0.8 * level);
            out.push(InkSplat {
                t: w.t,
                x: cx,
                y: cy,
                radius: radius * 1.4,
                velocity: [0.12 * side * speed, -0.3 * speed],
                color,
                amount: 0.0,
            });
            out.push(InkSplat { t: w.t + 0.12, x: cx, y: cy, radius, velocity: [0.0; 2], color, amount: 0.5 * amount });
        }
        out.extend(self.extra.iter().copied());
        out
    }

    /// The dye's material.
    pub fn material(&self) -> Material {
        let (soften, rim, grain, density) = match self.style {
            InkStyle::Ink => (1.5, 0.0, 0.0, 1.8),
            InkStyle::Smoke => (4.0, 0.0, 0.0, 1.2),
            InkStyle::Watercolour => (3.0, 0.55, 0.06, 1.4),
        };
        Material::custom(FLUID_MATERIAL)
            .param(0, [0.0, 0.0, self.area.w, self.area.h])
            .param(1, [self.style.index(), self.dye_slot as f32, if self.light { 1.0 } else { 0.0 }, self.opacity])
            .param(2, [soften, rim, grain, density])
            .param(3, match self.clear {
                Some((top, bottom)) if bottom > top => [top, bottom, CLEAR_FADE, 0.0],
                _ => [0.0, -1.0, CLEAR_FADE, 0.0],
            })
            .reads_images()
    }

    /// The GPU source, for the painter.
    pub fn source(&self) -> FluidSource {
        FluidSource { splats: self.splats(), reduced_motion: self.reduced_motion }
    }

    fn place<M>(&self, e: Element<M>) -> Element<M> {
        let at = self.at;
        e.layout(|s| {
            s.position = taffy::style::Position::Absolute;
            s.inset.left = taffy::style::LengthPercentageAuto::length(at.x);
            s.inset.top = taffy::style::LengthPercentageAuto::length(at.y);
        })
    }

    /// The dye: the simulation's drive and the dye panel, in a layer the size of the box,
    /// absolutely placed at [`FluidLayer::at`]. Put it before the text to draw it behind.
    pub fn view<M>(&self, key: impl Into<Key>) -> Element<M> {
        let key = key.into();
        let drive = Element::gpu(format!("{key}-drive"), Arc::new(self.source())).size(self.area.w, self.area.h);
        let dye = Element::panel(format!("{key}-dye"))
            .size(self.area.w, self.area.h)
            .fill(Color::WHITE)
            .material(self.material());
        let mut layer = Element::layer(format!("{key}-layer")).size(self.area.w, self.area.h).child(dye);
        if self.glow > 0.0 && !self.light {
            layer = layer.bloom(self.glow, 0.04 * self.area.w.min(self.area.h * 2.0));
        }
        self.place(Element::column(key).size(self.area.w, self.area.h)).child(absolute(drive)).child(absolute(layer))
    }

    /// The smear: a layer that draws what is under it moved along the flow. Put it after the
    /// text. It moves only the box above the clear band's top (the older lines over the
    /// reading line), or the whole box with no band; the newest line of live text is kept as
    /// drawn either way. With no smear set, or no room above the band, an empty box.
    pub fn smear_view<M>(&self, key: impl Into<Key>) -> Element<M> {
        let key = key.into();
        let (w, h) = (self.area.w, self.area.h);
        let top = self.clear.map_or(h, |(top, _)| top.min(h));
        if self.smear <= 0.0 || top <= 0.0 {
            return self.place(Element::layer(key).size(w, h));
        }
        // The displacement spans the whole box, so the velocity texture lines up with it; a
        // clip above it keeps the band and what is below it as drawn.
        let moved = Element::layer(format!("{key}-moved")).size(w, h).displace(Displace::Texture {
            slot: self.velocity_slot,
            amount: [-self.smear * w, -self.smear * h],
            bias: [0.0; 2],
        });
        self.place(Element::layer(key).size(w, top).clip().child(absolute(moved)))
    }

    /// `content` with the dye behind it and the smear over it, in a box the layer's size at
    /// [`FluidLayer::at`]. `content` is placed at the box's top left.
    pub fn around<M>(&self, key: impl Into<Key>, content: Element<M>) -> Element<M> {
        let key = key.into();
        let inner = FluidLayer { at: Point::ZERO, ..self.clone() };
        self.place(Element::column(key.clone()).size(self.area.w, self.area.h))
            .child(inner.view(format!("{key}-ink")))
            .child(absolute(content))
            .child(inner.smear_view(format!("{key}-smear")))
    }
}

fn absolute<M>(e: Element<M>) -> Element<M> {
    e.layout(|s| {
        s.position = taffy::style::Position::Absolute;
        s.inset.left = taffy::style::LengthPercentageAuto::length(0.0);
        s.inset.top = taffy::style::LengthPercentageAuto::length(0.0);
    })
}

/// Each space-separated word's box in `text` as `shaped` laid it out (in the text box's space),
/// in order: from its first glyph's pen position to its last glyph's advance, one `size` tall
/// with its baseline at 0.78 of the height, as live text places words. Hand these, moved to the
/// fluid layer's space, to [`InkWord`]. A word with no glyphs (all fallback-less) is skipped.
pub fn word_boxes(text: &str, shaped: &ShapedText, size: f32) -> Vec<Rect> {
    let mut out = Vec::new();
    let mut at = 0;
    for word in text.split(' ') {
        let range = at as u32..(at + word.len()) as u32;
        at += word.len() + 1;
        if word.is_empty() {
            continue;
        }
        let mut glyphs = shaped.glyphs.iter().filter(|g| range.contains(&g.cluster));
        let Some(first) = glyphs.next() else { continue };
        let (mut x0, mut x1) = (first.pos.x, first.pos.x + first.advance);
        for g in glyphs {
            x0 = x0.min(g.pos.x);
            x1 = x1.max(g.pos.x + g.advance);
        }
        out.push(Rect::new(x0, first.pos.y - 0.78 * size, x1 - x0, size));
    }
    out
}

/// sRGB to linear light.
fn linear(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer() -> FluidLayer {
        FluidLayer::new(Size::new(600.0, 300.0), Point::ZERO).reduced_motion(false).light(false).words([
            InkWord { t: 0.2, rect: Rect::new(30.0, 30.0, 80.0, 30.0), level: 0.5 },
            InkWord { t: 0.5, rect: Rect::new(120.0, 30.0, 60.0, 30.0), level: 1.0 },
        ])
    }

    #[test]
    fn words_make_dye_a_push_and_a_lingering_source() {
        let s = layer().splats();
        assert_eq!(s.len(), 8);
        // Dye a quarter in from each end of the first word, at its centre line.
        assert!((s[0].x - 50.0 / 600.0).abs() < 1e-6 && (s[1].x - 90.0 / 600.0).abs() < 1e-6);
        assert!((s[0].y - 45.0 / 300.0).abs() < 1e-6);
        assert_eq!(s[0].velocity, [0.0; 2]);
        // The push goes up, to alternating sides, faster when louder.
        assert!(s[2].amount == 0.0 && s[2].velocity[1] < 0.0 && s[2].velocity[0] > 0.0);
        assert!(s[6].velocity[0] < 0.0 && s[6].velocity[1] < s[2].velocity[1]);
        // Louder words carry more dye.
        assert!(s[4].amount > s[0].amount);
        assert!((s[3].t - 0.32).abs() < 1e-9 && s[3].amount < s[0].amount);
    }

    #[test]
    fn a_new_word_extends_the_splat_list() {
        let a = layer();
        let b = a.clone().word(InkWord { t: 0.8, rect: Rect::new(200.0, 30.0, 40.0, 30.0), level: 0.2 });
        assert!(b.splats().starts_with(&a.splats()));
    }

    #[test]
    fn source_settles_after_the_last_splat() {
        let l = layer();
        assert!((l.source().until() - (0.62 + SETTLE_SECS)).abs() < 1e-9);
        assert!((l.clone().reduced_motion(true).source().until() - 0.62).abs() < 1e-9);
        assert_eq!(FluidLayer::new(l.area, Point::ZERO).source().until(), f64::NEG_INFINITY);
    }

    #[test]
    fn word_boxes_follow_the_layout() {
        use crate::text::{EstimateText, TextLayout};
        let style = crate::element::TextStyle::new(0, 20.0, 26.0);
        let text = "ink blooms where words land";
        let shaped = EstimateText::default().layout(text, &style, Some(120.0));
        let boxes = word_boxes(text, &shaped, 20.0);
        assert_eq!(boxes.len(), 5);
        assert!(boxes[1].x > boxes[0].x + boxes[0].w);
        // "words" wraps to the next line.
        assert!(boxes[3].y > boxes[0].y && boxes[3].x < boxes[2].x);
        assert!((boxes[0].w - 3.0 * 0.55 * 20.0).abs() < 1e-3, "{:?}", boxes[0]);
    }

    #[test]
    fn material_carries_style_slot_and_mode() {
        let m = layer().style(InkStyle::Watercolour).light(true).slots(0, 3).material();
        assert_eq!(m.params[1][..3], [2.0, 0.0, 1.0]);
        assert!(m.reads_textures() && m.params[2][1] > 0.0);
        assert_eq!(layer().style(InkStyle::Smoke).material().params[1][0], 1.0);
        assert_eq!(layer().keep_clear(40.0, 70.0).material().params[3][..2], [40.0, 70.0]);
        assert!(layer().material().params[3][1] < layer().material().params[3][0]);
    }
}
