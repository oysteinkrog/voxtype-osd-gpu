//! GPU particles (CAPABILITIES PA01 to PA09, D302, FX16; ROADMAP 4.7).
//!
//! [`Particles`] holds two kinds of particle, both in storage buffers that never come back to
//! the CPU:
//!
//! - **Free particles** in fixed-slot pools, one pool per emitter ([`Particles::add_emitter`]).
//!   They step at a fixed tick (`ParticleConfig::tick_hz`), one compute dispatch per tick.
//!   A tick's inputs come from [`EmitterSim`] values the caller computes at the tick's own
//!   time, and every random number is `rand(seed, emitter, slot, spawn)`. So the state after
//!   tick `n` is the same however the ticks were batched into frames: a 144 Hz live run and a
//!   60 fps export run the same ticks with the same inputs. The draw interpolates between the
//!   last two ticks at the frame time. Positions are 3D; a 2D emitter keeps z at 0, a 3D one
//!   is drawn through [`Camera`] with depth.
//! - **Formed particles** ([`Particles::set_word`], [`Particles::set_points`]): a fixed pool per
//!   slot. They do not step. Each holds an anchor and its position is a closed-form critically
//!   damped spring, evaluated at the frame time. A change re-anchors the slot at the change's
//!   own time, by evaluating the old spring exactly there, so the result does not depend on
//!   when frames happen. The CPU knows each glyph's convergence in closed form, so
//!   [`Particles::glyph_alpha`] gives the solid glyph's opacity with no readback.
//!
//! Emitter inputs cover the force library (PA02: swirl, ring pull, curl noise, attractor,
//! gravity and wind, drag), bursts and shockwaves (PA05: a burst is a new `epoch`, which
//! respawns the whole pool on that tick) and the text mask (PA08, [`MaskField`]). Looks cover
//! soft additive sprites with a colour ramp and velocity stretch (PA06), ribbons for a few
//! hero particles (PA07), hard dots snapped to a grid (PA09) and depth-of-field discs (FX16).
//!
//! Drawing: [`Particles::prepare`] writes the frame's uniforms, then [`Particles::draw`] records
//! three vertex-pulled draws (6 vertices per sprite, no vertex buffers) into any render pass.
//! A layer that wants particles (an offscreen layer with bloom, for example) draws them into
//! its own pass with its own transform.
//!
//! Seek: going back in time restores the last snapshot at or before the target tick
//! ([`Particles::snapshot`]), or the start state, then steps forward. Formed particles need no
//! seek of their own, but the changes before the target time must be applied again.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use vo_core::hash::rand_f32;
use vo_scene::font::{Face, FontRegistry};
use vo_scene::geom::{Affine, Color, Point};

use crate::color::srgb_to_linear;
use crate::glyph_points::{GlyphPointCache, GlyphPoints};

const COMMON_WGSL: &str = include_str!("wgsl/particles.wgsl");
const STEP_WGSL: &str = include_str!("wgsl/particles_step.wgsl");
const ANCHOR_WGSL: &str = include_str!("wgsl/particles_anchor.wgsl");
const DRAW_WGSL: &str = include_str!("wgsl/particles_draw.wgsl");

/// The compute shader that steps free particles.
pub fn step_shader_source() -> String {
    [COMMON_WGSL, STEP_WGSL].join("\n")
}

/// The compute shader that re-anchors a slot of formed particles.
pub fn anchor_shader_source() -> String {
    [COMMON_WGSL, ANCHOR_WGSL].join("\n")
}

/// The sprite shaders.
pub fn draw_shader_source() -> String {
    [COMMON_WGSL, DRAW_WGSL].join("\n")
}

/// Workgroup size of the compute passes, and the alignment of every pool.
pub const WORKGROUP: u32 = 64;
/// Most ticks recorded into one submission.
pub const MAX_BATCH: u32 = 128;
/// Most emitters.
pub const MAX_EMITTERS: u32 = 16;
/// Ticks of history per hero particle, for ribbons.
pub const TRAIL: u32 = 16;
const STEP_STRIDE: u64 = 256;

/// Spring rate of a word particle in 1/s, before its ±20 % jitter.
pub const WORD_RATE: f32 = 36.0;
/// The slowest particle's rate; the reveal follows it, so no straggler is left behind.
pub const WORD_RATE_MIN: f32 = WORD_RATE * 0.8;
/// The solid glyph fades in while the slowest particle's convergence goes from the first
/// value to the second.
pub const REVEAL: (f32, f32) = (0.5, 0.85);
/// A fresh word forms left to right over this many seconds.
pub const SWEEP: f32 = 0.04;
/// A changed glyph's reveal waits this long, for the scatter to spread.
pub const CHANGED_DELAY: f32 = 0.04;
/// Scatter speed of a changed glyph, in font sizes per second. The peak excursion of a
/// critically damped spring kicked from rest is `v0 / (w e)`, about a quarter of the size.
pub const SCATTER: f32 = 22.0;

/// The solid glyph's opacity `tau` seconds into its reveal. The sprite shader uses the same
/// formula (`reveal` in `particles_draw.wgsl`), so particle light and glyph opacity add up.
pub fn reveal(tau: f32) -> f32 {
    let x = WORD_RATE_MIN * tau.max(0.0);
    let conv = 1.0 - (1.0 + x) * (-x).exp();
    smoothstep(REVEAL.0, REVEAL.1, conv)
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Pool sizes. Fixed for the life of a [`Particles`].
#[derive(Clone, Copy, Debug)]
pub struct ParticleConfig {
    /// Free particles across all emitters.
    pub free_capacity: u32,
    /// Slots of formed particles, and particles per slot (rounded up to [`WORKGROUP`]).
    pub word_slots: u32,
    pub slot_capacity: u32,
    /// Hero particles with ribbons, across all emitters.
    pub trail_capacity: u32,
    /// Fixed tick of the free particles.
    pub tick_hz: f64,
    pub seed: u32,
}

impl Default for ParticleConfig {
    fn default() -> Self {
        ParticleConfig {
            free_capacity: 20_480,
            word_slots: 32,
            slot_capacity: 1024,
            trail_capacity: 256,
            tick_hz: 120.0,
            seed: 1,
        }
    }
}

/// Where an emitter spawns, in its own space. Parameters may change from tick to tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Point,
    /// A circle in the xy plane; `spread` is the band width as a share of the radius.
    Ring {
        radius: f32,
        spread: f32,
    },
    Disc {
        radius: f32,
    },
    /// A segment through the origin, `angle` in radians from +x.
    Line {
        half_length: f32,
        angle: f32,
    },
    Box {
        half: [f32; 3],
    },
    /// A torus about z: `major` to the tube's centre, `minor` the tube's radius (D302).
    Torus {
        major: f32,
        minor: f32,
    },
}

/// A shockwave: a front leaving the emitter's origin (PA05).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shock {
    /// Seconds since it started.
    pub age: f32,
    /// Front speed in units per second, and its width in units.
    pub speed: f32,
    pub width: f32,
    /// Push in units per second squared at the front.
    pub strength: f32,
}

/// An emitter's simulation inputs at one tick time. Everything that should follow a level, a
/// state or a burst goes here, computed from the tick's own time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmitterSim {
    pub shape: Shape,
    /// Share of deaths that respawn, 0 to 1. 0 lets the pool die out.
    pub emit: f32,
    /// Mean life in seconds and its jitter as a share of it.
    pub life: f32,
    pub life_jitter: f32,
    /// Speed outward from the shape at spawn.
    pub spawn_speed: f32,
    /// Swirl about z in radians per second.
    pub swirl: f32,
    /// Pull towards the circle of `ring_radius` (the torus core in 3D), per second squared.
    pub ring_pull: f32,
    pub ring_radius: f32,
    /// Velocity damping per second.
    pub drag: f32,
    /// Curl noise strength (units per second squared), scale (cells per unit) and time speed.
    pub noise: f32,
    pub noise_scale: f32,
    pub noise_speed: f32,
    /// A point attractor and its strength per second squared per unit of distance (negative
    /// repels).
    pub attract: [f32; 3],
    pub attract_strength: f32,
    /// Gravity plus wind, units per second squared.
    pub gravity: [f32; 3],
    pub shock: Option<Shock>,
    /// Text mask force (PA08): positive pushes out of the text, negative hugs glyph edges.
    pub mask: f32,
    /// Burst counter: when it changes, the whole pool respawns on that tick.
    pub epoch: u32,
    /// The level, 0 to 1. Only recorded; the other fields carry its effect.
    pub level: f32,
}

impl Default for EmitterSim {
    fn default() -> Self {
        EmitterSim {
            shape: Shape::Point,
            emit: 1.0,
            life: 1.0,
            life_jitter: 0.5,
            spawn_speed: 0.0,
            swirl: 0.0,
            ring_pull: 0.0,
            ring_radius: 0.0,
            drag: 1.5,
            noise: 0.0,
            noise_scale: 0.02,
            noise_speed: 0.7,
            attract: [0.0; 3],
            attract_strength: 0.0,
            gravity: [0.0; 3],
            shock: None,
            mask: 0.0,
            epoch: 0,
            level: 0.0,
        }
    }
}

impl EmitterSim {
    /// A ring of `radius` that holds its particles on it (the voice ring, PA04).
    pub fn ring(radius: f32, spread: f32) -> EmitterSim {
        EmitterSim { shape: Shape::Ring { radius, spread }, ring_pull: 40.0, ring_radius: radius, ..Default::default() }
    }

    /// A torus about z that holds its particles near its core (D302).
    pub fn torus(major: f32, minor: f32) -> EmitterSim {
        EmitterSim { shape: Shape::Torus { major, minor }, ring_pull: 30.0, ring_radius: major, ..Default::default() }
    }
}

/// How a sprite is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpriteStyle {
    /// A soft glow.
    Soft,
    /// A hard disc, snapped to a grid of `pitch` pixels when `pitch` is above 0 (PA09).
    Dot { pitch: f32 },
    /// Out of focus by depth (FX16): sprites at clip depth `focus` are sharp, others grow by
    /// `aperture` pixels per unit of depth into flat discs with the same light. 3D only.
    Bokeh { focus: f32, aperture: f32 },
}

impl SpriteStyle {
    fn encode(self) -> [f32; 4] {
        match self {
            SpriteStyle::Soft => [0.0; 4],
            SpriteStyle::Dot { pitch } => [1.0, pitch, 0.0, 0.0],
            SpriteStyle::Bokeh { focus, aperture } => [2.0, 0.0, focus, aperture],
        }
    }
}

/// An emitter's look at the frame time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmitterLook {
    /// Where the emitter's origin is in layer space, and its z (3D only).
    pub origin: Point,
    pub z: f32,
    /// Rows of a rotation applied to emitter space before the origin: a tilt for a 3D ring.
    pub rotation: [[f32; 3]; 3],
    /// Sprite radius in logical units.
    pub size: f32,
    pub brightness: f32,
    /// Colour at birth, in mid-life and at death (sRGB, straight alpha).
    pub ramp: [Color; 3],
    pub style: SpriteStyle,
    /// Ribbon width as a share of the sprite, and its opacity (PA07; hero particles only).
    pub trail_width: f32,
    pub trail_alpha: f32,
}

impl Default for EmitterLook {
    fn default() -> Self {
        EmitterLook {
            origin: Point::ZERO,
            z: 0.0,
            rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            size: 1.5,
            brightness: 1.0,
            ramp: [Color::WHITE, Color::WHITE, Color::hex(0xffffff, 0.0)],
            style: SpriteStyle::Soft,
            trail_width: 0.8,
            trail_alpha: 0.6,
        }
    }
}

/// A rotation about x by `a` radians, as rows, for [`EmitterLook::rotation`].
pub fn tilt_x(a: f32) -> [[f32; 3]; 3] {
    let (s, c) = a.sin_cos();
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}

/// What an emitter is made of. Fixed once added.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmitterSpec {
    /// Pool size, rounded up to [`WORKGROUP`].
    pub capacity: u32,
    pub seed: u32,
    /// Births are spread over the first `warmup` seconds.
    pub warmup: f32,
    /// The first `trails` particles of the pool keep a ribbon (PA07).
    pub trails: u32,
    /// Simulate in 3D (curl noise in z) and draw through the frame's [`Camera`].
    pub three_d: bool,
}

impl EmitterSpec {
    pub fn new(capacity: u32, seed: u32) -> EmitterSpec {
        EmitterSpec { capacity, seed, warmup: 1.0, trails: 0, three_d: false }
    }
}

/// One glyph of a word, as [`Particles::set_word`] takes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordGlyph {
    pub font: u32,
    pub glyph: u32,
    /// Em size in logical units.
    pub size: f32,
    /// Pen position on the baseline, relative to the word's origin.
    pub pos: Point,
}

/// Points for a slot to form, as [`Particles::set_points`] takes them (PA09 and any shape).
#[derive(Clone, Copy, Debug)]
pub struct PointForm<'a> {
    /// Targets relative to the slot's origin. At most `slot_capacity` are used.
    pub points: &'a [Point],
    /// Where each particle starts, when it should not start from where it is (or from a
    /// cloud): dots peeling off a grid, for example.
    pub starts: Option<&'a [Point]>,
    /// Size of the shape, for the cloud and the scatter.
    pub size: f32,
    /// The particles stay visible once formed. Without it they hand over to solid content
    /// through [`Particles::glyph_alpha`] like a word.
    pub persist: bool,
    /// A left to right sweep of start delays across the points, in seconds.
    pub sweep: f32,
    /// Particles that were formed already scatter before they move on.
    pub scatter: bool,
}

/// A slot's look at the frame time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SlotLook {
    /// Where the slot's origin is in layer space this frame.
    pub origin: Point,
    pub color: Color,
    pub visible: bool,
    /// Sprite radius in logical units; 0 takes it from the word's stroke.
    pub size: f32,
    pub style: SpriteStyle,
}

impl SlotLook {
    pub fn new(origin: Point, color: Color) -> SlotLook {
        SlotLook { origin, color, visible: true, size: 0.0, style: SpriteStyle::Soft }
    }
}

/// A camera for 3D emitters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// World (layer units) to wgpu clip space, column-major as WGSL reads a `mat4x4f`.
    pub view_proj: [[f32; 4]; 4],
    /// Pixels per logical unit at clip `w` 1: sprite sizes divide by `w`.
    pub focal_px: f32,
}

impl Camera {
    /// A perspective camera for an overlay `width` by `height` logical units: the z = 0
    /// plane lands exactly on the layer's pixels, z comes towards the viewer, and the eye is
    /// `distance` units in front of the centre. Depth runs from 0.1 just in front of the eye
    /// to about 0.58 at `distance` behind the plane.
    pub fn overlay(width: f32, height: f32, distance: f32, px_per_unit: f32) -> Camera {
        let d = distance;
        Camera {
            view_proj: [
                [2.0 / width, 0.0, 0.0, 0.0],
                [0.0, -2.0 / height, 0.0, 0.0],
                [0.0, 0.0, -0.6 / d, -1.0 / d],
                [-1.0, 1.0, 0.55, 1.0],
            ],
            focal_px: px_per_unit,
        }
    }
}

/// How sprites combine with what is below.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParticleBlend {
    /// Light adds up: glows on dark themes. Needs no sorting.
    #[default]
    Add,
    /// Premultiplied source over: dark particles on light themes (PA06). Unsorted, so dense
    /// overlapping particles of different colours can show their draw order.
    Over,
}

/// What [`Particles::prepare`] needs about the frame.
#[derive(Clone, Copy, Debug)]
pub struct FrameParams {
    pub t: f64,
    /// Layer space (logical units) to target pixels, for 2D emitters and slots.
    pub to_px: Affine,
    /// Physical pixels per logical unit along the transform (sprite sizes).
    pub px_per_unit: f32,
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
    /// Velocity stretch: a sprite streaks over this many seconds of its motion (PA07).
    pub stretch: f32,
    pub camera: Option<Camera>,
    pub blend: ParticleBlend,
    /// The depth attachment's format when the pass has one: sprites test against it and do
    /// not write it.
    pub depth: Option<wgpu::TextureFormat>,
}

impl FrameParams {
    /// A 2D frame at `t` with layer units mapped to pixels by `to_px`.
    pub fn new(t: f64, to_px: Affine, width: u32, height: u32, format: wgpu::TextureFormat) -> FrameParams {
        let px_per_unit = (to_px.a * to_px.d - to_px.b * to_px.c).abs().sqrt();
        FrameParams {
            t,
            to_px,
            px_per_unit,
            width,
            height,
            format,
            stretch: 1.0 / 60.0,
            camera: None,
            blend: ParticleBlend::Add,
            depth: None,
        }
    }
}

/// A coverage field of text (or any shape) for the PA08 mask force, in emitter space.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskField {
    pub width: u32,
    pub height: u32,
    /// The field's box in emitter space: x, y, w, h.
    pub rect: [f32; 4],
    /// Coverage 0 to 255, rows top to bottom.
    pub data: Vec<u8>,
}

impl MaskField {
    /// A field over `rect` with cells of `cell` units, from points that fill shapes (as
    /// glyph points do), blurred by `blur` cells so the force reaches past the edges.
    pub fn from_points(points: &[[f32; 2]], rect: [f32; 4], cell: f32, blur: u32) -> MaskField {
        let w = ((rect[2] / cell).ceil() as u32).clamp(1, 512);
        let h = ((rect[3] / cell).ceil() as u32).clamp(1, 512);
        let mut acc = vec![0f32; (w * h) as usize];
        for p in points {
            let x = ((p[0] - rect[0]) / rect[2] * w as f32).floor();
            let y = ((p[1] - rect[1]) / rect[3] * h as f32).floor();
            if x >= 0.0 && y >= 0.0 && (x as u32) < w && (y as u32) < h {
                acc[(y as u32 * w + x as u32) as usize] += 1.0;
            }
        }
        // Coverage: a cell with points counts as ink, then a box blur twice.
        for v in &mut acc {
            *v = if *v > 0.0 { 1.0 } else { 0.0 };
        }
        for _ in 0..2 {
            acc = box_blur(&acc, w, h, blur);
        }
        let data = acc.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
        MaskField { width: w, height: h, rect, data }
    }
}

fn box_blur(src: &[f32], w: u32, h: u32, r: u32) -> Vec<f32> {
    if r == 0 {
        return src.to_vec();
    }
    let (w, h, r) = (w as i32, h as i32, r as i32);
    let n = (2 * r + 1) as f32;
    let mut tmp = vec![0f32; src.len()];
    for y in 0..h {
        for x in 0..w {
            let s: f32 = (-r..=r).map(|d| src[(y * w + (x + d).clamp(0, w - 1)) as usize]).sum();
            tmp[(y * w + x) as usize] = s / n;
        }
    }
    let mut out = vec![0f32; src.len()];
    for y in 0..h {
        for x in 0..w {
            let s: f32 = (-r..=r).map(|d| tmp[((y + d).clamp(0, h - 1) * w + x) as usize]).sum();
            out[(y * w + x) as usize] = s / n;
        }
    }
    out
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuFree {
    pos: [f32; 4],
    prev: [f32; 4],
    vel: [f32; 4],
    info: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuEmitStep {
    spawn: [u32; 4],
    shape: [f32; 4],
    life: [f32; 4],
    swirl: [f32; 4],
    noise: [f32; 4],
    attract: [f32; 4],
    gravity: [f32; 4],
    shock: [f32; 4],
    pool: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuStep {
    index: u32,
    count: u32,
    emitters: u32,
    tick: u32,
    t: f32,
    dt: f32,
    mask_switch: u32,
    _pad: u32,
    rect_a: [f32; 4],
    rect_b: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuWord {
    start: [f32; 2],
    v0: [f32; 2],
    goal: [f32; 2],
    t0: f32,
    w: f32,
    delay: f32,
    gen: u32,
    flags: u32,
    stroke: f32,
    reveal_t0: f32,
    reveal_delay: f32,
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuTarget {
    pos: [f32; 2],
    flags: u32,
    glyph: u32,
    reveal_t0: f32,
    reveal_delay: f32,
    start: [f32; 2],
}

const T_ACTIVE: u32 = 1;
const T_CHANGED: u32 = 2;
const T_PERSIST: u32 = 4;
const T_START: u32 = 8;
const E_3D: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuAnchor {
    slot: u32,
    capacity: u32,
    gen: u32,
    seed: u32,
    t: f32,
    size: f32,
    w: f32,
    _pad: f32,
    x0: f32,
    width: f32,
    scatter: f32,
    stroke: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuFrame {
    m: [f32; 4],
    t: [f32; 4],
    viewport: [f32; 4],
    time: f32,
    alpha: f32,
    stretch: f32,
    w_min: f32,
    reveal0: f32,
    reveal1: f32,
    tick: u32,
    trail_len: u32,
    view_proj: [[f32; 4]; 4],
    cam: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuEmitDraw {
    origin: [f32; 4],
    rot: [[f32; 4]; 3],
    c: [[f32; 4]; 3],
    look: [f32; 4],
    trail: [f32; 4],
    pool: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuSlotDraw {
    origin: [f32; 2],
    visible: f32,
    size: f32,
    color: [f32; 4],
    look: [f32; 4],
}

/// Index of an emitter in a [`Particles`].
pub type EmitterId = u32;

struct Emitter {
    spec: EmitterSpec,
    base: u32,
    capacity: u32,
    trail_base: u32,
}

/// How a glyph came into the current text of its slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphState {
    /// New in a fresh word: forms from a cloud.
    Fresh,
    /// Replaced a different glyph: scatters and re-forms.
    Changed,
    /// The same glyph as before (maybe moved): stays solid.
    Kept,
}

#[derive(Clone)]
struct SlotGlyph {
    glyph: WordGlyph,
    /// When this glyph's reveal starts, and its delay.
    reveal_t0: f64,
    reveal_delay: f32,
    state: GlyphState,
}

#[derive(Default, Clone)]
struct Slot {
    gen: u32,
    glyphs: Vec<SlotGlyph>,
    /// The current targets, relative to the slot's origin.
    points: Vec<[f32; 2]>,
}

/// A pipeline set for one target format, blend and depth format.
struct DrawPipes {
    key: (wgpu::TextureFormat, ParticleBlend, Option<wgpu::TextureFormat>),
    free: wgpu::RenderPipeline,
    trail: wgpu::RenderPipeline,
    word: wgpu::RenderPipeline,
}

struct MaskTex {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    rect: [f32; 4],
}

/// Fixed-step GPU particles and formed particles on one device.
pub struct Particles {
    device: wgpu::Device,
    queue: wgpu::Queue,
    cfg: ParticleConfig,
    fonts: FontRegistry,
    faces: Vec<Arc<Face>>,
    points: GlyphPointCache,
    emitters: Vec<Emitter>,
    /// Free particles in use, a multiple of [`WORKGROUP`].
    free_used: u32,
    trails_used: u32,
    /// The free pool has been stepped to this tick.
    tick: u64,
    initial: Vec<GpuFree>,
    snapshot: Option<(u64, wgpu::Buffer)>,
    slots: Vec<Slot>,
    /// Mask A is sampled before `mask_switch`, mask B from it on.
    masks: [MaskTex; 2],
    mask_b: usize,
    mask_switch: u64,

    free: wgpu::Buffer,
    emits: wgpu::Buffer,
    steps: wgpu::Buffer,
    trail: wgpu::Buffer,
    trail_owner: wgpu::Buffer,
    words: wgpu::Buffer,
    targets: wgpu::Buffer,
    anchor: wgpu::Buffer,
    frame: wgpu::Buffer,
    emit_draw: wgpu::Buffer,
    slot_draw: wgpu::Buffer,
    sampler: wgpu::Sampler,

    step_layout: wgpu::BindGroupLayout,
    step_bind: wgpu::BindGroup,
    anchor_bind: wgpu::BindGroup,
    draw_bind: wgpu::BindGroup,
    step_pipe: wgpu::ComputePipeline,
    anchor_pipe: wgpu::ComputePipeline,
    draw_layout: wgpu::PipelineLayout,
    draw_module: wgpu::ShaderModule,
    draw_pipes: Vec<DrawPipes>,
    draw_key: Option<(wgpu::TextureFormat, ParticleBlend, Option<wgpu::TextureFormat>)>,
    // Scratch, reused.
    step_data: Vec<u8>,
    emit_data: Vec<GpuEmitStep>,
    target_data: Vec<GpuTarget>,
}

fn storage(binding: u32, read_only: bool, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform(binding: u32, dynamic: bool, size: u64, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: dynamic,
            min_binding_size: wgpu::BufferSize::new(size),
        },
        count: None,
    }
}

fn buffer(device: &wgpu::Device, label: &str, size: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size.max(256),
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn entry(binding: u32, buf: &wgpu::Buffer, size: Option<u64>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
            buffer: buf,
            offset: 0,
            size: size.and_then(wgpu::BufferSize::new),
        }),
    }
}

fn linear(c: Color) -> [f32; 4] {
    [srgb_to_linear(c.r), srgb_to_linear(c.g), srgb_to_linear(c.b), c.a]
}

fn mask_texture(device: &wgpu::Device, w: u32, h: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("vo-particles-mask"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    (tex, view)
}

impl Particles {
    /// Creates the buffers and the two compute pipelines. Sprite pipelines are made on the
    /// first [`Particles::prepare`] for a target format, or by [`Particles::ensure_pipelines`].
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, fonts: FontRegistry, cfg: ParticleConfig) -> Particles {
        let cfg = ParticleConfig {
            free_capacity: cfg.free_capacity.next_multiple_of(WORKGROUP).max(WORKGROUP),
            slot_capacity: cfg.slot_capacity.next_multiple_of(WORKGROUP).max(WORKGROUP),
            word_slots: cfg.word_slots.max(1),
            trail_capacity: cfg.trail_capacity.max(1),
            ..cfg
        };
        use wgpu::BufferUsages as U;
        use wgpu::ShaderStages as S;
        let free_bytes = cfg.free_capacity as u64 * size_of::<GpuFree>() as u64;
        let word_count = cfg.word_slots as u64 * cfg.slot_capacity as u64;
        let trail_bytes = cfg.trail_capacity as u64 * TRAIL as u64 * 16;
        let free = buffer(device, "vo-particles-free", free_bytes, U::STORAGE | U::COPY_SRC);
        let emits = buffer(
            device,
            "vo-particles-emits",
            (MAX_BATCH * MAX_EMITTERS) as u64 * size_of::<GpuEmitStep>() as u64,
            U::STORAGE,
        );
        let steps = buffer(device, "vo-particles-steps", MAX_BATCH as u64 * STEP_STRIDE, U::UNIFORM);
        let trail = buffer(device, "vo-particles-trail", trail_bytes, U::STORAGE | U::COPY_SRC);
        let trail_owner = buffer(device, "vo-particles-trail-owner", cfg.trail_capacity as u64 * 4, U::STORAGE);
        let words = buffer(device, "vo-particles-words", word_count * size_of::<GpuWord>() as u64, U::STORAGE);
        let targets = buffer(device, "vo-particles-targets", word_count * size_of::<GpuTarget>() as u64, U::STORAGE);
        let anchor = buffer(device, "vo-particles-anchor", size_of::<GpuAnchor>() as u64, U::UNIFORM);
        let frame = buffer(device, "vo-particles-frame", size_of::<GpuFrame>() as u64, U::UNIFORM);
        let emit_draw =
            buffer(device, "vo-particles-emit-draw", MAX_EMITTERS as u64 * size_of::<GpuEmitDraw>() as u64, U::STORAGE);
        let slot_draw = buffer(
            device,
            "vo-particles-slot-draw",
            cfg.word_slots as u64 * size_of::<GpuSlotDraw>() as u64,
            U::STORAGE,
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vo-particles-mask"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let masks = [0, 1].map(|_| {
            let (tex, view) = mask_texture(device, 1, 1);
            MaskTex { tex, view, rect: [0.0; 4] }
        });

        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: S::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let step_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-particles-step"),
            entries: &[
                uniform(0, true, size_of::<GpuStep>() as u64, S::COMPUTE),
                storage(1, false, S::COMPUTE),
                storage(2, true, S::COMPUTE),
                storage(3, false, S::COMPUTE),
                tex_entry(4),
                tex_entry(5),
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: S::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let anchor_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-particles-anchor"),
            entries: &[
                uniform(0, false, size_of::<GpuAnchor>() as u64, S::COMPUTE),
                storage(1, false, S::COMPUTE),
                storage(2, true, S::COMPUTE),
            ],
        });
        let vf = S::VERTEX_FRAGMENT;
        let draw_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-particles-draw"),
            entries: &[
                uniform(0, false, size_of::<GpuFrame>() as u64, vf),
                storage(1, true, S::VERTEX),
                storage(2, true, S::VERTEX),
                storage(3, true, S::VERTEX),
                storage(4, true, S::VERTEX),
                storage(5, true, S::VERTEX),
                storage(6, true, S::VERTEX),
            ],
        });
        let anchor_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-particles-anchor"),
            layout: &anchor_layout,
            entries: &[
                entry(0, &anchor, Some(size_of::<GpuAnchor>() as u64)),
                entry(1, &words, Some(word_count * size_of::<GpuWord>() as u64)),
                entry(2, &targets, Some(word_count * size_of::<GpuTarget>() as u64)),
            ],
        });
        // Exact sizes: the word shader finds the slot capacity from the two array lengths.
        let draw_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-particles-draw"),
            layout: &draw_layout,
            entries: &[
                entry(0, &frame, Some(size_of::<GpuFrame>() as u64)),
                entry(1, &free, Some(free_bytes)),
                entry(2, &emit_draw, None),
                entry(3, &words, Some(word_count * size_of::<GpuWord>() as u64)),
                entry(4, &slot_draw, Some(cfg.word_slots as u64 * size_of::<GpuSlotDraw>() as u64)),
                entry(5, &trail, Some(trail_bytes)),
                entry(6, &trail_owner, None),
            ],
        });

        let compute = |label: &str, source: String, layout: &wgpu::BindGroupLayout, entry: &str| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let step_pipe = compute("vo-particles-step", step_shader_source(), &step_layout, "cs_step");
        let anchor_pipe = compute("vo-particles-anchor", anchor_shader_source(), &anchor_layout, "cs_anchor");
        let draw_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vo-particles-draw"),
            source: wgpu::ShaderSource::Wgsl(draw_shader_source().into()),
        });
        let draw_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vo-particles-draw"),
            bind_group_layouts: &[Some(&draw_layout)],
            immediate_size: 0,
        });
        // Formed particles start empty: zeroed flags are "not in use".
        let mut enc = device.create_command_encoder(&Default::default());
        enc.clear_buffer(&words, 0, None);
        enc.clear_buffer(&free, 0, None);
        queue.submit([enc.finish()]);
        let step_bind = Self::make_step_bind(device, &step_layout, &steps, &free, &emits, &trail, &masks, 1, &sampler);

        Particles {
            device: device.clone(),
            queue: queue.clone(),
            cfg,
            fonts,
            faces: Vec::new(),
            points: GlyphPointCache::new(),
            emitters: Vec::new(),
            free_used: 0,
            trails_used: 0,
            tick: 0,
            initial: Vec::new(),
            snapshot: None,
            slots: vec![Slot::default(); cfg.word_slots as usize],
            masks,
            mask_b: 1,
            mask_switch: 0,
            free,
            emits,
            steps,
            trail,
            trail_owner,
            words,
            targets,
            anchor,
            frame,
            emit_draw,
            slot_draw,
            sampler,
            step_layout,
            step_bind,
            anchor_bind,
            draw_bind,
            step_pipe,
            anchor_pipe,
            draw_layout: draw_pl,
            draw_module,
            draw_pipes: Vec::new(),
            draw_key: None,
            step_data: Vec::new(),
            emit_data: Vec::new(),
            target_data: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn make_step_bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        steps: &wgpu::Buffer,
        free: &wgpu::Buffer,
        emits: &wgpu::Buffer,
        trail: &wgpu::Buffer,
        masks: &[MaskTex; 2],
        b: usize,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-particles-step"),
            layout,
            entries: &[
                entry(0, steps, Some(size_of::<GpuStep>() as u64)),
                entry(1, free, None),
                entry(2, emits, None),
                entry(3, trail, None),
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&masks[1 - b].view) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&masks[b].view) },
                wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        })
    }

    pub fn config(&self) -> &ParticleConfig {
        &self.cfg
    }

    /// Bytes of GPU buffers this system holds, snapshots included.
    pub fn gpu_bytes(&self) -> u64 {
        [
            &self.free,
            &self.emits,
            &self.steps,
            &self.trail,
            &self.trail_owner,
            &self.words,
            &self.targets,
            &self.anchor,
            &self.frame,
            &self.emit_draw,
            &self.slot_draw,
        ]
        .iter()
        .map(|b| b.size())
        .sum::<u64>()
            + self.snapshot.as_ref().map_or(0, |(_, b)| b.size())
    }

    /// Free particles in use.
    pub fn free_count(&self) -> u32 {
        self.free_used
    }

    /// The tick the free pool is at.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    pub fn tick_time(&self, tick: u64) -> f64 {
        tick as f64 / self.cfg.tick_hz
    }

    /// Adds an emitter with its own pool. Must be called before the first tick.
    pub fn add_emitter(&mut self, spec: EmitterSpec) -> EmitterId {
        assert_eq!(self.tick, 0, "emitters are added before the first tick");
        assert!((self.emitters.len() as u32) < MAX_EMITTERS, "at most {MAX_EMITTERS} emitters");
        let capacity = spec.capacity.next_multiple_of(WORKGROUP);
        let base = self.free_used;
        assert!(base + capacity <= self.cfg.free_capacity, "free pool is full");
        let trails = spec.trails.min(capacity);
        let trail_base = self.trails_used;
        assert!(trail_base + trails <= self.cfg.trail_capacity, "trail pool is full");
        let id = self.emitters.len() as u32;
        self.emitters.push(Emitter { spec, base, capacity, trail_base });
        self.free_used += capacity;
        self.trails_used += trails;
        // Not yet born: negative age, staggered by a hash of the slot. Ages sit on the tick
        // grid so a birth happens on a tick, not between two.
        let dt = (1.0 / self.cfg.tick_hz) as f32;
        for i in base..base + capacity {
            let wait = (rand_f32(spec.seed, id, i, 0) * spec.warmup / dt).floor() * dt;
            self.initial.push(GpuFree {
                pos: [0.0, 0.0, 0.0, -wait - dt * 0.5],
                info: [0, id, 0, 0],
                ..Default::default()
            });
        }
        self.queue.write_buffer(&self.free, 0, bytemuck::cast_slice(&self.initial));
        if trails > 0 {
            let owners: Vec<u32> = (base..base + trails).collect();
            self.queue.write_buffer(&self.trail_owner, trail_base as u64 * 4, bytemuck::cast_slice(&owners));
        }
        id
    }

    /// A 2D ring emitter (PA04), the same as `add_emitter(EmitterSpec { warmup, .. })`.
    pub fn add_ring(&mut self, capacity: u32, seed: u32, warmup: f32) -> EmitterId {
        self.add_emitter(EmitterSpec { warmup, ..EmitterSpec::new(capacity, seed) })
    }

    /// Takes a GPU-side copy of the free pool at the current tick, for seeking back. Ribbon
    /// history is not kept, so ribbons are short for one ribbon length after a restore.
    pub fn snapshot(&mut self) {
        let size = self.free_used as u64 * size_of::<GpuFree>() as u64;
        if size == 0 {
            return;
        }
        let buf = self.snapshot.take().map(|(_, b)| b).filter(|b| b.size() >= size).unwrap_or_else(|| {
            buffer(
                &self.device,
                "vo-particles-snapshot",
                size,
                wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            )
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(&self.free, 0, &buf, 0, size);
        self.queue.submit([enc.finish()]);
        self.snapshot = Some((self.tick, buf));
    }

    /// The number of ticks that must have run for a frame at `t`: the state at tick `n` is
    /// the state at time `n / tick_hz`, and the frame interpolates between ticks `n - 1` and
    /// `n` with `(n - 1) / tick_hz <= t < n / tick_hz`.
    pub fn ticks_for(&self, t: f64) -> u64 {
        (t * self.cfg.tick_hz).floor().max(0.0) as u64 + 1
    }

    /// Steps the free pool to the tick a frame at `t` needs, seeking back first when `t` is
    /// before the current tick. `sim(emitter, tick_time)` gives each emitter's inputs at a
    /// tick's start time. Submits one command buffer per [`MAX_BATCH`] ticks. Returns the
    /// number of ticks run.
    pub fn advance(&mut self, t: f64, sim: &dyn Fn(EmitterId, f64) -> EmitterSim) -> u64 {
        let want = self.ticks_for(t);
        if want < self.tick {
            self.restore(want);
        }
        let mut ran = 0;
        while self.tick < want {
            let n = (want - self.tick).min(MAX_BATCH as u64) as u32;
            let mut enc =
                self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vo-particles") });
            self.encode_ticks(&mut enc, n, sim);
            self.queue.submit([enc.finish()]);
            ran += n as u64;
        }
        ran
    }

    /// Records the next `n` ticks (at most [`MAX_BATCH`]) into `enc`. Writes the tick inputs
    /// with `queue.write_buffer`, so `enc` must be submitted before the next call.
    pub fn encode_ticks(&mut self, enc: &mut wgpu::CommandEncoder, n: u32, sim: &dyn Fn(EmitterId, f64) -> EmitterSim) {
        assert!(n <= MAX_BATCH);
        if n == 0 {
            return;
        }
        let dt = 1.0 / self.cfg.tick_hz;
        let ne = self.emitters.len().max(1) as u32;
        self.step_data.clear();
        self.emit_data.clear();
        let (ra, rb) = (self.masks[1 - self.mask_b].rect, self.masks[self.mask_b].rect);
        for k in 0..n {
            let tick = self.tick + k as u64;
            let t = tick as f64 * dt;
            let step = GpuStep {
                index: k,
                count: self.free_used,
                emitters: ne,
                tick: tick as u32,
                t: t as f32,
                dt: dt as f32,
                mask_switch: self.mask_switch as u32,
                _pad: 0,
                rect_a: ra,
                rect_b: rb,
            };
            self.step_data.extend_from_slice(bytemuck::bytes_of(&step));
            self.step_data.resize(((k + 1) as u64 * STEP_STRIDE) as usize, 0);
            for (id, e) in self.emitters.iter().enumerate() {
                self.emit_data.push(encode_sim(&sim(id as u32, t), e));
            }
            if self.emitters.is_empty() {
                self.emit_data.push(GpuEmitStep::default());
            }
        }
        self.queue.write_buffer(&self.steps, 0, &self.step_data);
        self.queue.write_buffer(&self.emits, 0, bytemuck::cast_slice(&self.emit_data));
        if self.free_used > 0 {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vo-particles-step"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.step_pipe);
            for k in 0..n {
                pass.set_bind_group(0, &self.step_bind, &[(k as u64 * STEP_STRIDE) as u32]);
                pass.dispatch_workgroups(self.free_used / WORKGROUP, 1, 1);
            }
        }
        self.tick += n as u64;
    }

    fn restore(&mut self, want: u64) {
        let size = self.free_used as u64 * size_of::<GpuFree>() as u64;
        match &self.snapshot {
            Some((tick, buf)) if *tick <= want => {
                let mut enc = self.device.create_command_encoder(&Default::default());
                enc.copy_buffer_to_buffer(buf, 0, &self.free, 0, size);
                self.queue.submit([enc.finish()]);
                self.tick = *tick;
            }
            _ => {
                self.queue.write_buffer(&self.free, 0, bytemuck::cast_slice(&self.initial));
                self.tick = 0;
            }
        }
    }

    /// Sets the text mask (PA08) from time `t` on. Ticks that start before `t` keep the
    /// previous mask, so the switch lands on the same tick however frames are paced. Only
    /// the last two masks are kept: set them in time order, and not further back than the
    /// last tick that has run.
    pub fn set_mask(&mut self, t: f64, mask: &MaskField) {
        let switch = ((t * self.cfg.tick_hz).ceil().max(0.0) as u64).max(self.tick);
        // The current B becomes A; the new mask goes into the other texture.
        let new_b = 1 - self.mask_b;
        let fits = {
            let m = &self.masks[new_b];
            m.tex.width() == mask.width && m.tex.height() == mask.height
        };
        if !fits {
            let (tex, view) = mask_texture(&self.device, mask.width, mask.height);
            self.masks[new_b] = MaskTex { tex, view, rect: mask.rect };
        }
        self.masks[new_b].rect = mask.rect;
        self.queue.write_texture(
            self.masks[new_b].tex.as_image_copy(),
            &mask.data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(mask.width), rows_per_image: None },
            wgpu::Extent3d { width: mask.width, height: mask.height, depth_or_array_layers: 1 },
        );
        self.mask_b = new_b;
        self.mask_switch = switch;
        self.step_bind = Self::make_step_bind(
            &self.device,
            &self.step_layout,
            &self.steps,
            &self.free,
            &self.emits,
            &self.trail,
            &self.masks,
            self.mask_b,
            &self.sampler,
        );
    }

    fn face(&mut self, font: u32) -> Option<Arc<Face>> {
        if font as usize >= self.faces.len() {
            let more = self.fonts.faces_from(self.faces.len());
            self.faces.extend(more);
        }
        self.faces.get(font as usize).cloned()
    }

    /// Sets the text of slot `slot` at time `t` (PA03). The first call forms the word from a
    /// cloud; a later call scatters and re-forms only the glyphs that changed (by a common
    /// prefix and suffix match), slides the rest, and scatters away glyphs that went. An
    /// empty `glyphs` clears the slot. `px_per_unit` sets the particle density.
    ///
    /// Records and submits the re-anchor pass at once, so calls must come in time order.
    pub fn set_word(&mut self, slot: u32, t: f64, glyphs: &[WordGlyph], px_per_unit: f32) {
        let cap = self.cfg.slot_capacity;
        let old = std::mem::take(&mut self.slots[slot as usize]);
        let same = |a: &WordGlyph, b: &WordGlyph| a.font == b.font && a.glyph == b.glyph && a.size == b.size;
        let prefix = old.glyphs.iter().zip(glyphs).take_while(|(a, b)| same(&a.glyph, b)).count();
        let max_suffix = old.glyphs.len().min(glyphs.len()) - prefix;
        let suffix = old
            .glyphs
            .iter()
            .rev()
            .zip(glyphs.iter().rev())
            .take(max_suffix)
            .take_while(|(a, b)| same(&a.glyph, b))
            .count();
        let fresh = old.glyphs.is_empty();

        // Points and budget per glyph: about one particle per 1.25 px² of ink.
        let pts: Vec<Arc<GlyphPoints>> = glyphs
            .iter()
            .map(|g| match self.face(g.font) {
                Some(face) => self.points.get(g.font, &face, g.glyph, g.size, px_per_unit),
                None => Arc::new(GlyphPoints::default()),
            })
            .collect();
        let px2 = px_per_unit * px_per_unit;
        let want: Vec<f32> = pts.iter().map(|p| p.area * px2 / 1.25).collect();
        let total: f32 = want.iter().sum();
        let k = if total > cap as f32 { cap as f32 / total } else { 1.0 };
        let (x0, x1) = glyphs.iter().zip(&pts).fold((f32::MAX, f32::MIN), |(a, b), (g, p)| {
            if p.contour.is_empty() {
                (a, b)
            } else {
                (a.min(g.pos.x + p.bounds[0]), b.max(g.pos.x + p.bounds[2]))
            }
        });
        let (x0, width) = if x1 > x0 { (x0, x1 - x0) } else { (0.0, 1.0) };
        let size = glyphs.first().map_or(16.0, |g| g.size);
        let size_px = size * px_per_unit;
        // At small sizes strokes are a pixel or two wide and the outline is the whole glyph.
        let interior_share = ((size_px - 18.0) / 10.0).clamp(0.0, 1.0) * 0.3;

        self.target_data.clear();
        self.target_data.resize(cap as usize, GpuTarget::default());
        let mut slot_glyphs = Vec::with_capacity(glyphs.len());
        let mut points = Vec::new();
        let mut next = 0usize;
        for (gi, (g, p)) in glyphs.iter().zip(&pts).enumerate() {
            let kept = !fresh && (gi < prefix || gi >= glyphs.len() - suffix);
            let (state, reveal_t0, reveal_delay) = if kept {
                let o = if gi < prefix { &old.glyphs[gi] } else { &old.glyphs[old.glyphs.len() - (glyphs.len() - gi)] };
                (GlyphState::Kept, o.reveal_t0, o.reveal_delay)
            } else if fresh {
                let cx = g.pos.x + 0.5 * (p.bounds[0] + p.bounds[2]);
                (GlyphState::Fresh, t, SWEEP * ((cx - x0) / width).clamp(0.0, 1.0))
            } else {
                (GlyphState::Changed, t, CHANGED_DELAY)
            };
            slot_glyphs.push(SlotGlyph { glyph: *g, reveal_t0, reveal_delay, state });
            let n = ((want[gi] * k).round() as usize).min(cap as usize - next);
            let n_in = if p.interior.is_empty() { 0 } else { (n as f32 * interior_share).round() as usize };
            let n_c = if p.contour.is_empty() { 0 } else { n - n_in };
            let pick = |list: &[[f32; 2]], j: usize, n: usize| {
                list[((j as f32 + 0.5) * list.len() as f32 / n as f32) as usize]
            };
            let flags = T_ACTIVE | if state == GlyphState::Changed { T_CHANGED } else { 0 };
            for j in 0..n_c + n_in {
                let q = if j < n_c { pick(&p.contour, j, n_c) } else { pick(&p.interior, j - n_c, n_in) };
                let pos = [g.pos.x + q[0], g.pos.y + q[1]];
                points.push(pos);
                self.target_data[next] = GpuTarget {
                    pos,
                    flags,
                    glyph: gi as u32,
                    reveal_t0: reveal_t0 as f32,
                    reveal_delay,
                    start: [0.0; 2],
                };
                next += 1;
            }
        }
        let anchor = GpuAnchor {
            slot,
            capacity: cap,
            gen: old.gen + 1,
            seed: self.cfg.seed,
            t: t as f32,
            size,
            w: WORD_RATE,
            x0,
            width,
            scatter: SCATTER,
            stroke: 0.09 * size,
            ..Default::default()
        };
        self.form(anchor);
        self.slots[slot as usize] = Slot { gen: anchor.gen, glyphs: slot_glyphs, points };
    }

    /// Forms `form.points` in slot `slot` from time `t` (PA09 and any shape): dots that
    /// assemble into a check mark, a burst that settles into a logo, the dot grid peeling off
    /// on dismiss (empty points). Particles already formed move on from where they are.
    pub fn set_points(&mut self, slot: u32, t: f64, form: &PointForm) {
        let cap = self.cfg.slot_capacity as usize;
        let old = std::mem::take(&mut self.slots[slot as usize]);
        let n = form.points.len().min(cap);
        let (x0, x1) = form.points[..n].iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.x), b.max(p.x)));
        let (x0, width) = if x1 > x0 { (x0, x1 - x0) } else { (0.0, 1.0) };
        let mut flags = T_ACTIVE;
        if form.persist {
            flags |= T_PERSIST;
        }
        if form.scatter {
            flags |= T_CHANGED;
        }
        if form.starts.is_some() {
            flags |= T_START;
        }
        self.target_data.clear();
        self.target_data.resize(cap, GpuTarget::default());
        for (i, p) in form.points[..n].iter().enumerate() {
            let start = form.starts.and_then(|s| s.get(i)).map_or([0.0; 2], |s| [s.x, s.y]);
            self.target_data[i] = GpuTarget {
                pos: [p.x, p.y],
                flags,
                glyph: 0,
                reveal_t0: t as f32,
                reveal_delay: form.sweep * ((p.x - x0) / width).clamp(0.0, 1.0),
                start,
            };
        }
        let anchor = GpuAnchor {
            slot,
            capacity: cap as u32,
            gen: old.gen + 1,
            seed: self.cfg.seed,
            t: t as f32,
            size: form.size,
            w: WORD_RATE,
            x0,
            width,
            scatter: if form.scatter { SCATTER } else { 0.0 },
            stroke: 0.09 * form.size,
            ..Default::default()
        };
        self.form(anchor);
        let glyph = SlotGlyph {
            glyph: WordGlyph { font: 0, glyph: 0, size: form.size, pos: Point::ZERO },
            reveal_t0: t,
            reveal_delay: form.sweep,
            state: if old.points.is_empty() { GlyphState::Fresh } else { GlyphState::Changed },
        };
        self.slots[slot as usize] = Slot {
            gen: anchor.gen,
            glyphs: if n > 0 { vec![glyph] } else { Vec::new() },
            points: form.points[..n].iter().map(|p| [p.x, p.y]).collect(),
        };
    }

    /// Uploads `self.target_data` for `anchor.slot` and runs the re-anchor pass.
    fn form(&mut self, anchor: GpuAnchor) {
        let cap = self.cfg.slot_capacity as u64;
        let base = anchor.slot as u64 * cap * size_of::<GpuTarget>() as u64;
        self.queue.write_buffer(&self.targets, base, bytemuck::cast_slice(&self.target_data));
        self.queue.write_buffer(&self.anchor, 0, bytemuck::bytes_of(&anchor));
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vo-anchor") });
        {
            let mut pass = enc
                .begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vo-anchor"), timestamp_writes: None });
            pass.set_pipeline(&self.anchor_pipe);
            pass.set_bind_group(0, &self.anchor_bind, &[]);
            pass.dispatch_workgroups(anchor.capacity / WORKGROUP, 1, 1);
        }
        self.queue.submit([enc.finish()]);
    }

    /// The current targets of `slot`, relative to its origin: the points a mask of the
    /// slot's text is made from ([`MaskField::from_points`]).
    pub fn slot_points(&self, slot: u32) -> &[[f32; 2]] {
        &self.slots[slot as usize].points
    }

    /// The opacity of glyph `index` of slot `slot`'s solid text at `t`: 0 while its
    /// particles form, 1 once they have handed over. Glyphs past the slot's text give 1.
    pub fn glyph_alpha(&self, slot: u32, index: usize, t: f64) -> f32 {
        match self.slots.get(slot as usize).and_then(|s| s.glyphs.get(index)) {
            Some(g) => reveal((t - g.reveal_t0) as f32 - g.reveal_delay),
            None => 1.0,
        }
    }

    /// How each glyph of a slot came in, for tests and tools.
    pub fn glyph_states(&self, slot: u32) -> Vec<GlyphState> {
        self.slots[slot as usize].glyphs.iter().map(|g| g.state).collect()
    }

    /// The time at which every glyph of `slot` is fully solid.
    pub fn settled_at(&self, slot: u32) -> f64 {
        let x = settle_x();
        self.slots[slot as usize]
            .glyphs
            .iter()
            .map(|g| g.reveal_t0 + (g.reveal_delay + x / WORD_RATE_MIN) as f64)
            .fold(f64::NEG_INFINITY, f64::max)
    }

    /// Writes the frame's uniforms and makes the sprite pipelines it needs. `look(emitter)`
    /// gives each emitter's look at `p.t`; `slots[i]` is slot `i`'s (missing slots are
    /// hidden).
    pub fn prepare(&mut self, p: &FrameParams, look: &dyn Fn(EmitterId) -> EmitterLook, slots: &[SlotLook]) {
        let m = &p.to_px;
        let alpha = if self.tick == 0 {
            1.0
        } else {
            ((p.t - self.tick_time(self.tick - 1)) * self.cfg.tick_hz).clamp(0.0, 1.0) as f32
        };
        let srgb = !p.format.is_srgb() && !matches!(p.format, wgpu::TextureFormat::Rgba16Float);
        let (view_proj, cam) = match p.camera {
            Some(c) => (c.view_proj, [1.0, c.focal_px, 0.0, 0.0]),
            None => {
                ([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]], [0.0; 4])
            }
        };
        let frame = GpuFrame {
            m: [m.a, m.b, m.c, m.d],
            t: [m.e, m.f, p.px_per_unit, 0.0],
            viewport: [p.width as f32, p.height as f32, srgb as u32 as f32, 0.0],
            time: p.t as f32,
            alpha,
            stretch: p.stretch,
            w_min: WORD_RATE_MIN,
            reveal0: REVEAL.0,
            reveal1: REVEAL.1,
            tick: self.tick as u32,
            trail_len: TRAIL,
            view_proj,
            cam,
        };
        self.queue.write_buffer(&self.frame, 0, bytemuck::bytes_of(&frame));
        let draws: Vec<GpuEmitDraw> = self
            .emitters
            .iter()
            .enumerate()
            .map(|(id, e)| {
                let l = look(id as u32);
                let r = l.rotation;
                GpuEmitDraw {
                    origin: [l.origin.x, l.origin.y, l.z, l.size],
                    rot: [
                        [r[0][0], r[0][1], r[0][2], l.brightness],
                        [r[1][0], r[1][1], r[1][2], 0.0],
                        [r[2][0], r[2][1], r[2][2], 0.0],
                    ],
                    c: l.ramp.map(linear),
                    look: l.style.encode(),
                    trail: [l.trail_width, l.trail_alpha, 0.0, 0.0],
                    pool: [e.base, e.capacity, e.trail_base, if e.spec.three_d { E_3D } else { 0 }],
                }
            })
            .collect();
        if !draws.is_empty() {
            self.queue.write_buffer(&self.emit_draw, 0, bytemuck::cast_slice(&draws));
        }
        let sd: Vec<GpuSlotDraw> = (0..self.cfg.word_slots as usize)
            .map(|i| match slots.get(i) {
                Some(s) => GpuSlotDraw {
                    origin: [s.origin.x, s.origin.y],
                    visible: s.visible as u32 as f32,
                    size: s.size,
                    color: linear(s.color),
                    look: s.style.encode(),
                },
                None => GpuSlotDraw::default(),
            })
            .collect();
        self.queue.write_buffer(&self.slot_draw, 0, bytemuck::cast_slice(&sd));
        let key = (p.format, p.blend, p.depth);
        self.ensure_pipelines_for(key);
        self.draw_key = Some(key);
    }

    /// Makes the additive sprite pipelines for `format` without depth, so the first frame
    /// that draws particles does not compile them.
    pub fn ensure_pipelines(&mut self, format: wgpu::TextureFormat) {
        self.ensure_pipelines_for((format, ParticleBlend::Add, None));
    }

    fn ensure_pipelines_for(&mut self, key: (wgpu::TextureFormat, ParticleBlend, Option<wgpu::TextureFormat>)) {
        if self.draw_pipes.iter().any(|d| d.key == key) {
            return;
        }
        let (format, blend, depth) = key;
        use wgpu::BlendFactor::*;
        let color = match blend {
            ParticleBlend::Add => One,
            ParticleBlend::Over => OneMinusSrcAlpha,
        };
        let blend = wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: One, dst_factor: color, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent {
                src_factor: One,
                dst_factor: OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let make = |vs: &str| {
            self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(vs),
                layout: Some(&self.draw_layout),
                vertex: wgpu::VertexState {
                    module: &self.draw_module,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.draw_module,
                    entry_point: Some("fs_sprite"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: depth.map(|format| wgpu::DepthStencilState {
                    format,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipes = DrawPipes { key, free: make("vs_free"), trail: make("vs_trail"), word: make("vs_word") };
        self.draw_pipes.push(pipes);
    }

    /// Records the sprite draws into `pass`, as [`Particles::prepare`] set them up: free
    /// particles, their ribbons, then formed particles.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(key) = self.draw_key else { return };
        let Some(pipes) = self.draw_pipes.iter().find(|d| d.key == key) else { return };
        pass.set_bind_group(0, &self.draw_bind, &[]);
        if self.free_used > 0 {
            pass.set_pipeline(&pipes.free);
            pass.draw(0..self.free_used * 6, 0..1);
        }
        if self.trails_used > 0 {
            pass.set_pipeline(&pipes.trail);
            pass.draw(0..self.trails_used * (TRAIL - 1) * 6, 0..1);
        }
        if self.slots.iter().any(|s| s.gen > 0) {
            pass.set_pipeline(&pipes.word);
            pass.draw(0..self.cfg.word_slots * self.cfg.slot_capacity * 6, 0..1);
        }
    }

    /// Records a render pass on `view` that loads what is there and draws the particles on
    /// top. For a pass with depth, open the pass yourself and call [`Particles::draw`].
    pub fn draw_over(&self, enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
        let att = [Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
        })];
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vo-particles"),
            color_attachments: &att,
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.draw(&mut pass);
    }
}

fn encode_sim(s: &EmitterSim, e: &Emitter) -> GpuEmitStep {
    let (kind, shape) = match s.shape {
        Shape::Point => (0, [0.0; 4]),
        Shape::Ring { radius, spread } => (1, [radius, spread, 0.0, 0.0]),
        Shape::Disc { radius } => (2, [radius, 0.0, 0.0, 0.0]),
        Shape::Line { half_length, angle } => (3, [half_length, angle, 0.0, 0.0]),
        Shape::Box { half } => (4, [half[0], half[1], half[2], 0.0]),
        Shape::Torus { major, minor } => (5, [major, minor, 0.0, 0.0]),
    };
    let shock = s.shock.map_or([-1.0, 0.0, 1.0, 0.0], |k| [k.age, k.speed, k.width, k.strength]);
    let a = s.attract;
    let g = s.gravity;
    GpuEmitStep {
        spawn: [kind, e.spec.seed, s.epoch, if e.spec.three_d { E_3D } else { 0 }],
        shape,
        life: [s.life, s.life_jitter, s.emit, s.spawn_speed],
        swirl: [s.swirl, s.ring_pull, s.ring_radius, s.drag],
        noise: [s.noise, s.noise_scale, s.noise_speed, 0.0],
        attract: [a[0], a[1], a[2], s.attract_strength],
        gravity: [g[0], g[1], g[2], s.mask],
        shock,
        pool: [e.base, e.capacity, e.trail_base, e.spec.trails.min(e.capacity)],
    }
}

/// `w tau` at which the slowest particle's convergence reaches the end of the reveal.
fn settle_x() -> f32 {
    let mut x = 0.0f32;
    while 1.0 - (1.0 + x) * (-x).exp() < REVEAL.1 {
        x += 0.001;
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(src: &str) {
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }

    #[test]
    fn shaders_validate_in_naga() {
        validate(&step_shader_source());
        validate(&anchor_shader_source());
        validate(&draw_shader_source());
    }

    #[test]
    fn struct_sizes_match_wgsl() {
        assert_eq!(size_of::<GpuFree>(), 64);
        assert_eq!(size_of::<GpuEmitStep>(), 144);
        assert_eq!(size_of::<GpuStep>(), 64);
        assert_eq!(size_of::<GpuWord>(), 64);
        assert_eq!(size_of::<GpuTarget>(), 32);
        assert_eq!(size_of::<GpuAnchor>(), 48);
        assert_eq!(size_of::<GpuFrame>(), 160);
        assert_eq!(size_of::<GpuEmitDraw>(), 160);
        assert_eq!(size_of::<GpuSlotDraw>(), 48);
    }

    #[test]
    fn reveal_is_readable_within_200_ms() {
        assert_eq!(reveal(0.0), 0.0);
        // The last glyph of a fresh word starts SWEEP late and is solid by 200 ms.
        let solid_at = settle_x() / WORD_RATE_MIN + SWEEP;
        assert!(solid_at < 0.2, "{solid_at}");
        assert!(reveal(solid_at - SWEEP) > 0.999);
        let mut last = 0.0;
        for i in 0..200 {
            let r = reveal(i as f32 * 0.001);
            assert!(r >= last);
            last = r;
        }
    }

    #[test]
    fn hash_matches_vo_core() {
        // The WGSL `pcg` is a transcription of vo_core::hash::pcg; pin the constants.
        assert!(COMMON_WGSL.contains("747796405u") && COMMON_WGSL.contains("2891336453u"));
        assert!(COMMON_WGSL.contains("277803737u"));
        assert_eq!(vo_core::hash::pcg(0), 129_708_002);
    }

    #[test]
    fn mask_field_covers_points_and_spreads() {
        let pts: Vec<[f32; 2]> =
            (0..20).flat_map(|x| (0..10).map(move |y| [40.0 + x as f32, 20.0 + y as f32])).collect();
        let m = MaskField::from_points(&pts, [0.0, 0.0, 100.0, 50.0], 2.0, 2);
        assert_eq!((m.width, m.height), (50, 25));
        let at = |x: f32, y: f32| m.data[((y / 2.0) as u32 * m.width + (x / 2.0) as u32) as usize];
        assert!(at(50.0, 25.0) > 150, "inside");
        assert_eq!(at(5.0, 5.0), 0, "far outside");
        let edge = at(38.0, 25.0);
        assert!(edge > 0 && edge < at(50.0, 25.0), "blurred edge {edge}");
    }

    #[test]
    fn overlay_camera_maps_the_plane_onto_pixels() {
        let c = Camera::overlay(800.0, 400.0, 1000.0, 1.0);
        let m = c.view_proj;
        let clip = |p: [f32; 4]| -> [f32; 4] {
            let mut o = [0.0; 4];
            for (col, v) in m.iter().zip(p) {
                for r in 0..4 {
                    o[r] += col[r] * v;
                }
            }
            o
        };
        let px = |p: [f32; 4]| {
            let c = clip(p);
            [(c[0] / c[3] * 0.5 + 0.5) * 800.0, (0.5 - c[1] / c[3] * 0.5) * 400.0, c[2] / c[3]]
        };
        let a = px([100.0, 50.0, 0.0, 1.0]);
        assert!((a[0] - 100.0).abs() < 1e-3 && (a[1] - 50.0).abs() < 1e-3, "{a:?}");
        // Closer to the eye: further from the centre and a smaller depth.
        let b = px([100.0, 50.0, 500.0, 1.0]);
        assert!(b[0] < a[0] && b[1] < a[1] && b[2] < a[2], "{a:?} {b:?}");
        let far = px([400.0, 200.0, -1000.0, 1.0]);
        assert!(far[2] > a[2] && far[2] <= 1.0);
        assert!(px([400.0, 200.0, 900.0, 1.0])[2] >= 0.0);
    }

    #[test]
    fn frame_params_take_the_scale_from_the_transform() {
        let p = FrameParams::new(0.0, Affine::scale(2.0), 10, 10, wgpu::TextureFormat::Rgba8UnormSrgb);
        assert!((p.px_per_unit - 2.0).abs() < 1e-6);
    }
}
