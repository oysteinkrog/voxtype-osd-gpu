//! Materials (FX15): how a shape, a glyph or a vector stroke turns its coverage into colour.
//!
//! One instance points at one [`Material`] in `DisplayList::materials` by a 1-based id (0 is
//! no material). A material is a function id plus a small parameter block, so a new look is a
//! new WGSL function and a constructor here, never a new instance field. The block is shared:
//! one chrome material covers every glyph of a word.
//!
//! Built in:
//!
//! - [`Material::neon`]: a neon tube on a stroke. A white-hot core, the stroke's colour as the
//!   tube, a hue shift toward the tube's edge, a soft bloom outside it, and an optional
//!   flicker as the tube comes on.
//! - [`Material::hologram`]: scan lines, a tint, light added rather than painted over, and a
//!   slight flicker.
//! - [`Material::chrome`]: an 80s chrome fill keyed to the glyph's em height, so the horizon
//!   lines up across a word whatever the glyph: sky above, a sharp horizon with a bright
//!   line, warm ground below.
//! - [`Material::gradient`]: two or three colour stops along a line in layer space, for one
//!   gradient across a whole word.
//! - [`Material::aurora`]: a mesh gradient of up to four soft colour blobs drifting over the
//!   element's box, brighter and larger with the app's energy. [`Material::haze`] is the same
//!   function as a warm glow that fades out between its blobs.
//!
//! Phosphor persistence is a layer property, not a material: see `Element::phosphor`.
//!
//! Adding a material: pick an id from [`CUSTOM_FIRST`] up, build the block with
//! [`Material::custom`], and register a WGSL function with the renderer
//! (`Renderer::add_material`). See `vo_render::material` for the function's inputs.
//!
//! Textures (EG-02): an app material can read two kinds of texture. [`Material::reads_backdrop`]
//! gives it the backdrop (EG-01) of the layer it draws in: what that layer's surface drew under
//! it, captured by `Element::backdrop` (a transparent tint captures without drawing anything
//! back). [`Material::reads_images`] gives it the renderer's image slots
//! ([`IMAGE_SLOTS`] of them, filled by `Renderer::set_material_image`). Both are bits in
//! [`Material::flags`] ([`READS_BACKDROP`], [`READS_IMAGES`]), so the rule above holds: no
//! instance field, and a material that reads no texture costs what it did before. The WGSL
//! side is `tex_backdrop` and `tex_image` in `vo_render`'s `materials.wgsl`.
//!
//! Colours in a block are straight sRGB like every display colour; the renderer turns the
//! slots marked as colours into OKLab before the shader sees them. A material's own colours
//! take their opacity from the instance's colour, so a fading label fades its chrome too.

use serde::Serialize;

use crate::geom::{Color, Point, Rect};

/// Parameter slots in a block, four floats each.
pub const PARAM_SLOTS: usize = 6;

/// The first function id free for app materials. Ids below are built in.
pub const CUSTOM_FIRST: u32 = 64;

/// Built-in material functions.
pub mod func {
    pub const NEON: u32 = 1;
    pub const HOLOGRAM: u32 = 2;
    pub const CHROME: u32 = 3;
    pub const GRADIENT: u32 = 4;
    pub const AURORA: u32 = 5;
}

/// `Material::flags` bit for chrome: key the bands to layer y instead of the glyph's em y.
pub const CHROME_LAYER: u32 = 1;

/// `Material::flags` bit for a gradient or an aurora: its geometry is in fractions of the
/// element's box, which the scene maps to layer space ([`Material::in_box`]).
pub const GRADIENT_BOX: u32 = 2;

/// `Material::flags` bit for an aurora: a glow that fades out between the blobs (a haze)
/// instead of a mesh that fills the whole box.
pub const AURORA_GLOW: u32 = 4;

/// `Material::flags` bit for any material: it reads the backdrop of the layer it draws in
/// (EG-02, `tex_backdrop` in WGSL). Reserved: app functions must not use it for their own.
pub const READS_BACKDROP: u32 = 1 << 30;

/// `Material::flags` bit for any material: it reads the renderer's image slots (EG-02,
/// `tex_image` in WGSL). Reserved like [`READS_BACKDROP`].
pub const READS_IMAGES: u32 = 1 << 31;

/// `Material::flags` bit for any material: it reads the text field and the word table (EG-31,
/// `text_dist` and `word_near` in WGSL). Reserved like [`READS_BACKDROP`].
pub const READS_TEXT: u32 = 1 << 29;

/// Every texture bit: a material with any of them draws with the textured pipelines.
pub const READS_TEXTURES: u32 = READS_BACKDROP | READS_IMAGES | READS_TEXT;

/// Image slots a material can read with `tex_image (slot, uv)`.
pub const IMAGE_SLOTS: usize = 4;

/// One material: a function and its parameters.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Material {
    /// Which WGSL function draws it: one of [`func`], or an app's from [`CUSTOM_FIRST`] up.
    pub func: u32,
    /// Function-specific bits.
    pub flags: u32,
    /// How far the material draws outside the shape, in logical units (a neon bloom). The
    /// renderer grows the instance's quad by this much.
    pub reach: f32,
    /// Seconds since the material's element appeared, written by the scene when it samples.
    /// Flicker and scrolling read it, so a frame stays a pure function of the scene time.
    pub time: f32,
    pub params: [[f32; 4]; PARAM_SLOTS],
    /// Bit `i` set: slot `i` holds a straight sRGB colour, which the renderer turns into
    /// OKLab plus alpha.
    pub colors: u32,
    /// Frames a second an app material needs while it is shown, 0 for a still one. Built-in
    /// materials work out their own (see [`Material::next_frame`]).
    pub fps: f32,
}

impl Material {
    /// An app material for function `func` (at least [`CUSTOM_FIRST`]), all parameters zero.
    pub fn custom(func: u32) -> Material {
        debug_assert!(func >= CUSTOM_FIRST, "custom material ids start at {CUSTOM_FIRST}");
        Material { func, flags: 0, reach: 0.0, time: 0.0, params: [[0.0; 4]; PARAM_SLOTS], colors: 0, fps: 0.0 }
    }

    fn builtin(func: u32) -> Material {
        Material { func, flags: 0, reach: 0.0, time: 0.0, params: [[0.0; 4]; PARAM_SLOTS], colors: 0, fps: 0.0 }
    }

    /// Sets parameter slot `i`.
    pub fn param(mut self, i: usize, v: [f32; 4]) -> Material {
        self.params[i] = v;
        self.colors &= !(1 << i);
        self
    }

    /// Sets parameter slot `i` to a colour, which the shader receives as OKLab plus alpha.
    pub fn color(mut self, i: usize, c: Color) -> Material {
        self.params[i] = [c.r, c.g, c.b, c.a];
        self.colors |= 1 << i;
        self
    }

    /// The same material at `time` seconds since its element appeared.
    pub fn at(self, time: f32) -> Material {
        Material { time, ..self }
    }

    /// The material reads the backdrop of the layer it draws in (EG-02). The layer, or one
    /// above it that draws into the same surface, needs `Element::backdrop`; without one the
    /// material samples transparent black and the renderer logs one warning.
    pub fn reads_backdrop(mut self) -> Material {
        self.flags |= READS_BACKDROP;
        self
    }

    /// The material reads the renderer's image slots (EG-02). An empty slot samples
    /// transparent black.
    pub fn reads_images(mut self) -> Material {
        self.flags |= READS_IMAGES;
        self
    }

    /// The material reads the text field and the word table (EG-31): how far each pixel is
    /// from the ink, and the words near it with their ages. The app builds the field with
    /// `vo_render::text_field::TextField::update_words`, which does nothing for a frame where
    /// no material reads text.
    pub fn reads_text(mut self) -> Material {
        self.flags |= READS_TEXT;
        self
    }

    /// Whether the material reads any texture: the renderer then draws it with the textured
    /// pipelines.
    pub fn reads_textures(&self) -> bool {
        self.flags & READS_TEXTURES != 0
    }

    /// An app material that changes over time: frames come `fps` times a second while it is
    /// shown. Ambient loops are capped at 30 (FX08).
    pub fn animated(self, fps: f32) -> Material {
        Material { fps: fps.clamp(0.0, 30.0), ..self }
    }

    /// When the material next changes, `age` seconds after its element appeared, as a delay in
    /// seconds; `None` when it is still. Neon flickers at 20 steps a second while it comes on
    /// and at 30 with a steady flicker; a hologram that scrolls or flickers is an ambient
    /// loop at 30.
    pub fn next_frame(&self, age: f64) -> Option<f64> {
        let p = &self.params;
        match self.func {
            func::NEON if age < p[1][2] as f64 => Some(1.0 / 20.0),
            func::NEON if p[1][3] > 0.0 => Some(1.0 / 30.0),
            func::HOLOGRAM if p[1][2] != 0.0 || p[2][0] > 0.0 => Some(1.0 / 30.0),
            func::AURORA if p[5][1] > 0.0 => Some(1.0 / 30.0),
            f if f >= CUSTOM_FIRST && self.fps > 0.0 => Some(1.0 / self.fps as f64),
            _ => None,
        }
    }

    /// A neon tube for a stroke in the stroke's colour. `bloom` is how far the soft light
    /// reaches outside the tube, in logical units. Defaults: a core a third of the tube wide,
    /// tinted most of the way to white; a bloom of strength 0.9; a hue shift of 0.06 turns at
    /// the tube's edge; no flicker.
    ///
    /// On a glyph, which has coverage but no distance, the tube degrades to the core colour
    /// over the glyph with no bloom; put neon text on a bloom layer, or stroke its outline.
    pub fn neon(bloom: f32) -> Neon {
        Neon { bloom: bloom.max(0.0), strength: 0.9, core: 0.35, white: 0.8, hue: 0.06, on: 0.0, steady: 0.0 }
    }

    /// Hologram scan lines `period` logical units apart, tinted toward `tint` by its alpha.
    /// Defaults: lines 55% deep scrolling down at 1.5 lines a second, 70% of the light added
    /// rather than painted over, a 6% flicker at 24 steps a second, edges brightened.
    pub fn hologram(tint: Color, period: f32) -> Hologram {
        Hologram {
            tint,
            period: period.max(1e-3),
            depth: 0.55,
            scroll: 1.5,
            additive: 0.7,
            flicker: 0.06,
            rate: 24.0,
            edge: 0.6,
        }
    }

    /// 80s chrome keyed to the glyph's em height (baseline 0, up positive): the sky runs from
    /// `top` at em height 0.75 to `sky` at the horizon at 0.36, the ground from `ground` just
    /// under it to `bottom` at the baseline.
    pub fn chrome(top: Color, sky: Color, ground: Color, bottom: Color) -> Chrome {
        Chrome {
            colors: [top, sky, ground, bottom],
            heights: [0.75, 0.36, 0.0],
            soft: 0.012,
            line: 0.9,
            line_width: 0.02,
            layer: false,
        }
    }

    /// The classic look: deep blue to pale sky, a white horizon line, brown to hot orange.
    pub fn chrome_classic() -> Chrome {
        Material::chrome(
            Color::hex(0x1b3a8c, 1.0),
            Color::hex(0xd8ecff, 1.0),
            Color::hex(0x3a1c10, 1.0),
            Color::hex(0xffb347, 1.0),
        )
    }

    /// `c0` at `p0` to `c1` at `p1` (layer space), mixed in OKLab, clamped outside.
    pub fn gradient(p0: Point, c0: Color, p1: Point, c1: Color) -> Material {
        // Two stops: the shader skips the middle one.
        Material::gradient3(p0, c0, 0.5, c0, p1, c1).param(4, [0.5, 2.0, 0.0, 0.0])
    }

    /// Three stops across the element it is on, left to right: `c0` at its left edge, `c1` at
    /// fraction `mid` of its width, `c2` at its right edge. On a label that is one gradient
    /// across the whole text, wherever layout puts it.
    pub fn gradient_across(c0: Color, mid: f32, c1: Color, c2: Color) -> Material {
        let mut m = Material::gradient3(Point::new(0.0, 0.5), c0, mid, c1, Point::new(1.0, 0.5), c2);
        m.flags |= GRADIENT_BOX;
        m
    }

    /// The material with geometry given as fractions of a box mapped onto `rect` (layer
    /// space). Other materials come back unchanged. The scene calls this with the element's
    /// box.
    pub fn in_box(self, rect: Rect) -> Material {
        if self.flags & GRADIENT_BOX == 0 {
            return self;
        }
        if self.func == func::AURORA {
            let mut m = self.param(4, [rect.x, rect.y, rect.w, rect.h]);
            m.flags &= !GRADIENT_BOX;
            return m;
        }
        if self.func != func::GRADIENT {
            return self;
        }
        let [x0, y0, x1, y1] = self.params[3];
        let at = |fx: f32, fy: f32| [rect.x + fx * rect.w, rect.y + fy * rect.h];
        let ([ax, ay], [bx, by]) = (at(x0, y0), at(x1, y1));
        let mut m = self.param(3, [ax, ay, bx, by]);
        m.flags &= !GRADIENT_BOX;
        m
    }

    /// A mesh gradient over the element's box: one soft blob per colour (two to four), each
    /// drifting on its own slow path, blended in OKLab. Defaults: blobs a third of the box
    /// wide, drifting at 0.12 cycles a second, energy 0, painted over (not added).
    pub fn aurora(colors: &[Color]) -> Aurora {
        let colors = if colors.is_empty() { &[Color::WHITE][..] } else { &colors[..colors.len().min(4)] };
        let c = std::array::from_fn(|i| colors[i % colors.len()]);
        Aurora { colors: c, energy: 0.0, speed: 0.12, size: 0.34, additive: 0.0, glow: false }
    }

    /// A warm haze: two soft glows of `color` drifting slowly inside the element's box, light
    /// added rather than painted over, fading to nothing between them. The colour's alpha is
    /// its strength.
    pub fn haze(color: Color) -> Aurora {
        let warm = Color { a: color.a * 0.7, ..color };
        Aurora {
            colors: [color, warm, Color::TRANSPARENT, Color::TRANSPARENT],
            energy: 0.0,
            speed: 0.05,
            size: 0.3,
            additive: 0.85,
            glow: true,
        }
    }

    /// Three stops: `c0` at `p0`, `c1` at fraction `mid` of the way, `c2` at `p1`.
    pub fn gradient3(p0: Point, c0: Color, mid: f32, c1: Color, p1: Point, c2: Color) -> Material {
        Material::builtin(func::GRADIENT)
            .color(0, c0)
            .color(1, c1)
            .color(2, c2)
            .param(3, [p0.x, p0.y, p1.x, p1.y])
            .param(4, [mid.clamp(1e-3, 1.0 - 1e-3), 3.0, 0.0, 0.0])
    }
}

/// A neon tube, built with [`Material::neon`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Neon {
    pub bloom: f32,
    /// How bright the bloom is next to the tube; above 1 is allowed on a float target.
    pub strength: f32,
    /// The white-hot core's width as a fraction of the tube's.
    pub core: f32,
    /// How far the core goes toward white, 0 to 1.
    pub white: f32,
    /// Hue shift at the tube's edge, in turns of OKLab hue.
    pub hue: f32,
    /// Seconds the tube flickers as it comes on; 0 lights it at once.
    pub on: f32,
    /// A steady flicker after that, 0 to 1.
    pub steady: f32,
}

impl Neon {
    pub fn strength(self, strength: f32) -> Neon {
        Neon { strength: strength.max(0.0), ..self }
    }
    pub fn core(self, core: f32, white: f32) -> Neon {
        Neon { core: core.clamp(0.0, 1.0), white: white.clamp(0.0, 1.0), ..self }
    }
    pub fn hue(self, turns: f32) -> Neon {
        Neon { hue: turns, ..self }
    }
    /// Flickers for `secs` as the element appears, then a steady flicker of `steady`.
    pub fn flicker_on(self, secs: f32, steady: f32) -> Neon {
        Neon { on: secs.max(0.0), steady: steady.clamp(0.0, 1.0), ..self }
    }
}

impl From<Neon> for Material {
    fn from(n: Neon) -> Material {
        let mut m = Material::builtin(func::NEON)
            .param(0, [n.core, n.white, n.hue, 0.0])
            .param(1, [n.bloom, n.strength, n.on, n.steady]);
        // The bloom falls under 1/256 at about 5.5 radii.
        m.reach = n.bloom * (256.0 * n.strength.max(1e-3)).max(1.0).ln();
        m
    }
}

/// A hologram look, built with [`Material::hologram`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hologram {
    pub tint: Color,
    pub period: f32,
    /// How dark the gaps between lines get, 0 to 1.
    pub depth: f32,
    /// Lines a second the pattern scrolls down; negative scrolls up.
    pub scroll: f32,
    /// How much of the light is added instead of painted over, 0 to 1.
    pub additive: f32,
    /// Brightness dips by up to this much, a new amount `rate` times a second.
    pub flicker: f32,
    pub rate: f32,
    /// How much brighter the shape's edge is, on shapes and strokes.
    pub edge: f32,
}

impl Hologram {
    pub fn depth(self, depth: f32) -> Hologram {
        Hologram { depth: depth.clamp(0.0, 1.0), ..self }
    }
    pub fn scroll(self, lines_per_sec: f32) -> Hologram {
        Hologram { scroll: lines_per_sec, ..self }
    }
    pub fn additive(self, additive: f32) -> Hologram {
        Hologram { additive: additive.clamp(0.0, 1.0), ..self }
    }
    pub fn flicker(self, amount: f32, rate: f32) -> Hologram {
        Hologram { flicker: amount.clamp(0.0, 1.0), rate: rate.max(0.0), ..self }
    }
    pub fn edge(self, edge: f32) -> Hologram {
        Hologram { edge: edge.max(0.0), ..self }
    }
}

impl From<Hologram> for Material {
    fn from(h: Hologram) -> Material {
        Material::builtin(func::HOLOGRAM)
            .color(0, h.tint)
            .param(1, [h.period, h.depth, h.scroll, h.additive])
            .param(2, [h.flicker, h.rate, h.edge, 0.0])
    }
}

/// A mesh gradient or haze, built with [`Material::aurora`] or [`Material::haze`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aurora {
    /// One blob per colour; fewer than four colours repeat.
    pub colors: [Color; 4],
    /// 0 to 1, from the app (a level or a signal): blobs grow, brighten and swirl harder.
    pub energy: f32,
    /// Drift in cycles a second; 0 holds the blobs still.
    pub speed: f32,
    /// A blob's radius as a fraction of the box's shorter side.
    pub size: f32,
    /// How much of the light is added instead of painted over, 0 to 1.
    pub additive: f32,
    /// Fade out between blobs instead of filling the box.
    pub glow: bool,
}

impl Aurora {
    /// Sets the energy for this frame, 0 to 1. The drift keeps its speed, so a changing
    /// energy never makes the blobs jump.
    pub fn energy(self, energy: f32) -> Aurora {
        Aurora { energy: energy.clamp(0.0, 1.0), ..self }
    }
    pub fn speed(self, cycles_per_sec: f32) -> Aurora {
        Aurora { speed: cycles_per_sec.max(0.0), ..self }
    }
    pub fn size(self, size: f32) -> Aurora {
        Aurora { size: size.clamp(0.02, 2.0), ..self }
    }
    pub fn additive(self, additive: f32) -> Aurora {
        Aurora { additive: additive.clamp(0.0, 1.0), ..self }
    }
    pub fn glow(self, glow: bool) -> Aurora {
        Aurora { glow, ..self }
    }
}

impl From<Aurora> for Material {
    fn from(a: Aurora) -> Material {
        let mut m = Material::builtin(func::AURORA)
            .color(0, a.colors[0])
            .color(1, a.colors[1])
            .color(2, a.colors[2])
            .color(3, a.colors[3])
            .param(4, [0.0, 0.0, 1.0, 1.0])
            .param(5, [a.energy, a.speed, a.size, a.additive]);
        m.flags |= GRADIENT_BOX;
        if a.glow {
            m.flags |= AURORA_GLOW;
        }
        m
    }
}

/// A chrome fill, built with [`Material::chrome`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chrome {
    /// Sky top, sky at the horizon, ground at the horizon, ground at the bottom.
    pub colors: [Color; 4],
    /// Heights of the sky's top, the horizon and the ground's bottom: em units up from the
    /// baseline, or layer y (down) with [`Chrome::layer`].
    pub heights: [f32; 3],
    /// How soft the horizon is, in the same units.
    pub soft: f32,
    /// The bright line on the horizon: strength and half width.
    pub line: f32,
    pub line_width: f32,
    pub layer: bool,
}

impl Chrome {
    /// Where the sky's top, the horizon and the ground's bottom are, in em units up from the
    /// baseline.
    pub fn heights(self, top: f32, horizon: f32, bottom: f32) -> Chrome {
        Chrome { heights: [top, horizon, bottom], layer: false, ..self }
    }
    /// Keys the bands to layer y (down) for shapes and strokes: the sky's top, the horizon
    /// and the ground's bottom at these layer y values.
    pub fn layer(self, top: f32, horizon: f32, bottom: f32) -> Chrome {
        Chrome { heights: [top, horizon, bottom], layer: true, ..self }
    }
    pub fn horizon_line(self, strength: f32, half_width: f32) -> Chrome {
        Chrome { line: strength.max(0.0), line_width: half_width.max(1e-4), ..self }
    }
    pub fn soft(self, soft: f32) -> Chrome {
        Chrome { soft: soft.max(1e-4), ..self }
    }
}

impl From<Chrome> for Material {
    fn from(c: Chrome) -> Material {
        // The shader measures height upward; layer y runs down, so it is negated here.
        let h = if c.layer { c.heights.map(|v| -v) } else { c.heights };
        let mut m = Material::builtin(func::CHROME)
            .color(0, c.colors[0])
            .color(1, c.colors[1])
            .color(2, c.colors[2])
            .color(3, c.colors[3])
            .param(4, [h[0], h[1], h[2], c.soft])
            .param(5, [c.line, c.line_width, 0.0, 0.0]);
        if c.layer {
            m.flags |= CHROME_LAYER;
        }
        m
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn texture_bits_are_flags() {
        let m = super::Material::custom(super::CUSTOM_FIRST);
        assert!(!m.reads_textures());
        let b = m.reads_backdrop();
        assert_eq!(b.flags, super::READS_BACKDROP);
        assert!(b.reads_textures() && m.reads_images().reads_textures());
        assert_eq!(b.reads_images().flags, super::READS_BACKDROP | super::READS_IMAGES);
        assert!(Material::custom(super::CUSTOM_FIRST).reads_text().reads_textures());
    }

    use super::*;

    #[test]
    fn color_slots_are_marked_and_unmarked() {
        let m = Material::custom(CUSTOM_FIRST).color(2, Color::hex(0xff0000, 1.0)).param(3, [1.0; 4]);
        assert_eq!(m.colors, 1 << 2);
        assert_eq!(m.color(3, Color::WHITE).param(2, [0.0; 4]).colors, 1 << 3);
    }

    #[test]
    fn neon_reach_covers_its_bloom() {
        let m: Material = Material::neon(6.0).into();
        assert_eq!(m.func, func::NEON);
        // exp(-reach / bloom) * strength is under 1/256.
        assert!(0.9 * (-m.reach / 6.0).exp() <= 1.0 / 255.0);
        let none: Material = Material::neon(0.0).into();
        assert_eq!(none.reach, 0.0);
    }

    #[test]
    fn chrome_in_layer_space_measures_up() {
        let m: Material = Material::chrome_classic().layer(10.0, 30.0, 50.0).into();
        assert_eq!(m.flags & CHROME_LAYER, CHROME_LAYER);
        assert_eq!(&m.params[4][..3], &[-10.0, -30.0, -50.0]);
        let em: Material = Material::chrome_classic().into();
        assert_eq!(em.flags, 0);
        assert!(em.params[4][0] > em.params[4][1] && em.params[4][1] > em.params[4][2]);
    }

    #[test]
    fn only_moving_materials_ask_for_frames() {
        let still: Material = Material::neon(4.0).into();
        assert_eq!(still.next_frame(0.0), None);
        let on: Material = Material::neon(4.0).flicker_on(0.6, 0.0).into();
        assert_eq!(on.next_frame(0.3), Some(0.05));
        assert_eq!(on.next_frame(0.7), None);
        let humming: Material = Material::neon(4.0).flicker_on(0.6, 0.1).into();
        assert_eq!(humming.next_frame(9.0), Some(1.0 / 30.0));
        let holo: Material = Material::hologram(Color::hex(0x40e0ff, 0.6), 3.0).into();
        assert_eq!(holo.next_frame(5.0), Some(1.0 / 30.0));
        let frozen: Material = Material::hologram(Color::WHITE, 3.0).scroll(0.0).flicker(0.0, 0.0).into();
        assert_eq!(frozen.next_frame(5.0), None);
        assert_eq!(Material::from(Material::chrome_classic()).next_frame(0.0), None);
        assert_eq!(Material::custom(CUSTOM_FIRST).animated(60.0).next_frame(0.0), Some(1.0 / 30.0));
    }

    #[test]
    fn a_box_gradient_spans_the_box() {
        let m = Material::gradient_across(Color::WHITE, 0.4, Color::hex(0x38e8ff, 1.0), Color::hex(0x9b5cff, 1.0));
        let placed = m.in_box(Rect::new(10.0, 20.0, 200.0, 40.0));
        assert_eq!(placed.params[3], [10.0, 40.0, 210.0, 40.0]);
        assert_eq!(placed.flags & GRADIENT_BOX, 0);
        // Mapped once: a second call leaves it.
        assert_eq!(placed.in_box(Rect::new(0.0, 0.0, 1.0, 1.0)), placed);
        let neon: Material = Material::neon(3.0).into();
        assert_eq!(neon.in_box(Rect::new(1.0, 2.0, 3.0, 4.0)), neon);
    }

    #[test]
    fn an_aurora_fills_its_box_and_drifts() {
        let m: Material = Material::aurora(&[Color::hex(0x2be0c8, 1.0), Color::hex(0x7a3cff, 1.0)]).energy(2.0).into();
        assert_eq!(m.func, func::AURORA);
        // Two colours repeat over the four blobs.
        assert_eq!(m.params[2], m.params[0]);
        assert_eq!(m.params[5][0], 1.0);
        assert_eq!(m.colors, 0b1111);
        let placed = m.in_box(Rect::new(10.0, 20.0, 300.0, 100.0));
        assert_eq!(placed.params[4], [10.0, 20.0, 300.0, 100.0]);
        assert_eq!(placed.flags & GRADIENT_BOX, 0);
        assert_eq!(m.next_frame(0.0), Some(1.0 / 30.0));
        let still: Material = Material::aurora(&[Color::WHITE]).speed(0.0).into();
        assert_eq!(still.next_frame(0.0), None);
        let haze: Material = Material::haze(Color::hex(0xffb070, 0.5)).into();
        assert_eq!(haze.flags & AURORA_GLOW, AURORA_GLOW);
        assert!(haze.params[5][3] > 0.5);
    }

    #[test]
    fn two_stop_gradient_puts_the_middle_stop_halfway() {
        let (a, b) = (Color::hex(0xffffff, 1.0), Color::hex(0x7a3cff, 1.0));
        let m = Material::gradient(Point::new(0.0, 0.0), a, Point::new(100.0, 0.0), b);
        assert_eq!(m.params[4][0], 0.5);
        assert_eq!(m.colors, 0b111);
        assert_eq!(m.params[3], [0.0, 0.0, 100.0, 0.0]);
    }
}
