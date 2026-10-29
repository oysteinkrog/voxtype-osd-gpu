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

pub use vo_scene::emitter::{tilt_x, ColorBy, EmitterLook, EmitterSim, MaskField, Shape, Shock, SpriteStyle};

use crate::color::srgb_to_linear;
use crate::glyph_points::{GlyphPointCache, GlyphPoints};
use crate::hot::wgsl;

const COMMON_WGSL: &str = include_str!("wgsl/particles.wgsl");
const STEP_WGSL: &str = include_str!("wgsl/particles_step.wgsl");
const ANCHOR_WGSL: &str = include_str!("wgsl/particles_anchor.wgsl");
const DRAW_WGSL: &str = include_str!("wgsl/particles_draw.wgsl");

/// The compute shader that steps free particles.
pub fn step_shader_source() -> String {
    [wgsl("particles.wgsl", COMMON_WGSL), wgsl("particles_step.wgsl", STEP_WGSL)].join("\n")
}

/// The compute shader that re-anchors a slot of formed particles.
pub fn anchor_shader_source() -> String {
    [wgsl("particles.wgsl", COMMON_WGSL), wgsl("particles_anchor.wgsl", ANCHOR_WGSL)].join("\n")
}

/// The sprite shaders.
pub fn draw_shader_source() -> String {
    [wgsl("particles.wgsl", COMMON_WGSL), wgsl("particles_draw.wgsl", DRAW_WGSL)].join("\n")
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
/// Frames a [`Particles`] can draw into at once: the target plus offscreen layers.
pub const VIEWS: u32 = 8;
/// Points of a path emitter's path after resampling.
pub const PATH_POINTS: u32 = 128;
/// Segments of each ribbon along a path.
pub const RIBBON_SEGMENTS: u32 = 96;
const VIEW_STRIDE: u64 = 256;

/// What [`Particles::draw_part`] draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Emitter(EmitterId),
    Slot(u32),
}

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
/// Pixels² of glyph ink per word particle by default.
pub const DOT_AREA: f32 = 1.25;

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
    /// Shape slots after the word slots (slots `word_slots..word_slots + shape_slots`), each
    /// with `shape_capacity` particles (rounded up to [`WORKGROUP`]; 0 means `slot_capacity`):
    /// shapes can take more particles than words without every word slot growing too.
    pub shape_slots: u32,
    pub shape_capacity: u32,
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
            shape_slots: 0,
            shape_capacity: 0,
            trail_capacity: 256,
            tick_hz: 120.0,
            seed: 1,
        }
    }
}

/// Slot `slot`'s first particle in the words buffer and its particle count, for a config as
/// [`Particles::new`] rounds it.
pub(crate) fn slot_span(c: &ParticleConfig, slot: u32) -> (u32, u32) {
    assert!(slot < c.word_slots + c.shape_slots, "no slot {slot}");
    if slot < c.word_slots {
        (slot * c.slot_capacity, c.slot_capacity)
    } else {
        (c.word_slots * c.slot_capacity + (slot - c.word_slots) * c.shape_capacity, c.shape_capacity)
    }
}

/// Packs a sprite style for the shaders.
fn style_code(s: SpriteStyle) -> [f32; 4] {
    match s {
        SpriteStyle::Soft => [0.0; 4],
        SpriteStyle::Dot { pitch } => [1.0, pitch, 0.0, 0.0],
        SpriteStyle::Bokeh { focus, aperture } => [2.0, 0.0, focus, aperture],
        SpriteStyle::Spark { accents, gain, twinkle } => [3.0, accents, gain, twinkle],
    }
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
    /// Targets relative to the slot's origin. At most the slot's capacity
    /// ([`Particles::slot_capacity`]) are used.
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

    /// The perspective of an overlay target `target` pixels in size, seen through a part of
    /// it: a view `size` pixels in size whose top left is at `origin` in target pixels (an
    /// offscreen layer). World units map to target pixels by `scale`; the eye is `distance`
    /// units in front of the target's centre, so every view of one target shares one
    /// vanishing point. With the view at the origin and the target's size, this is
    /// [`Camera::overlay`] in pixels.
    pub fn overlay_view(target: [f32; 2], origin: [f32; 2], size: [f32; 2], scale: f32, distance: f32) -> Camera {
        let ([w0, h0], [ox, oy], [w, h], d) = (target, origin, size, distance);
        Camera {
            view_proj: [
                [2.0 * scale / w, 0.0, 0.0, 0.0],
                [0.0, -2.0 * scale / h, 0.0, 0.0],
                [-(w0 - 2.0 * ox - w) / (w * d), -(h - h0 + 2.0 * oy) / (h * d), -0.6 / d, -1.0 / d],
                [-1.0 - 2.0 * ox / w, 1.0 + 2.0 * oy / h, 0.55, 1.0],
            ],
            focal_px: scale,
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
    /// The pass's MSAA sample count (4 inside the vo-3d pass).
    pub samples: u32,
}

/// Draw pipelines are made per target format, blend, depth format and sample count.
type DrawKey = (wgpu::TextureFormat, ParticleBlend, Option<wgpu::TextureFormat>, u32);

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
            samples: 1,
        }
    }
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
    path: [f32; 4],
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
    /// The slot this particle belongs to, for its look; the anchor pass writes it.
    slot: u32,
    _pad: f32,
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
const T_HELD: u32 = 16;
/// The particle starts at `start` and fades in like a fresh word's (a word from a source).
const T_FROM: u32 = 32;
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
    /// The slot's first particle in the words buffer.
    base: u32,
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
    path: [f32; 4],
    ribbon: [f32; 4],
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
    /// The first tick that steps this pool; earlier ticks leave it as added.
    start: u64,
    /// The path's strand wave at the last tick, for ribbons drawn at the frame time.
    path: [f32; 4],
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
    /// The glyph's particles in its slot: start and count.
    range: (usize, usize),
    /// Held as particles until the word is set again without holding it
    /// ([`Particles::set_word_held`]).
    held: bool,
}

#[derive(Default, Clone)]
struct Slot {
    gen: u32,
    glyphs: Vec<SlotGlyph>,
    /// The current targets, relative to the slot's origin.
    points: Vec<[f32; 2]>,
    /// Pixels² of ink per particle: the light each carries when formed.
    ink: f32,
    /// Particles that may be visible: one past the highest index any form used since the
    /// last reset, so a draw skips the slot's unused tail.
    hi: usize,
}

/// A pipeline set for one target format, blend and depth format.
struct DrawPipes {
    key: DrawKey,
    free: wgpu::RenderPipeline,
    trail: wgpu::RenderPipeline,
    word: wgpu::RenderPipeline,
    ribbon: wgpu::RenderPipeline,
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
    paths: wgpu::Buffer,
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
    draw_key: Option<DrawKey>,
    /// The frame as `prepare` wrote it, for further views.
    frame_data: GpuFrame,
    view_keys: [Option<DrawKey>; VIEWS as usize],
    /// Ribbons each emitter draws this frame.
    ribbons: Vec<u32>,
    // Scratch, reused.
    step_data: Vec<u8>,
    emit_data: Vec<GpuEmitStep>,
    target_data: Vec<GpuTarget>,
    /// Where the next fresh word's particles start, relative to the word, instead of a cloud
    /// round it; taken by the next [`Particles::set_word_held`].
    word_from: Option<(Point, f32)>,
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
        let shape_capacity = match cfg.shape_capacity {
            0 => cfg.slot_capacity,
            c => c.next_multiple_of(WORKGROUP),
        };
        let cfg = ParticleConfig { shape_capacity, ..cfg };
        let all_slots = cfg.word_slots + cfg.shape_slots;
        use wgpu::BufferUsages as U;
        use wgpu::ShaderStages as S;
        let free_bytes = cfg.free_capacity as u64 * size_of::<GpuFree>() as u64;
        let word_count =
            cfg.word_slots as u64 * cfg.slot_capacity as u64 + cfg.shape_slots as u64 * cfg.shape_capacity as u64;
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
        let frame = buffer(device, "vo-particles-frame", VIEWS as u64 * VIEW_STRIDE, U::UNIFORM);
        let paths = buffer(device, "vo-particles-paths", (MAX_EMITTERS * PATH_POINTS) as u64 * 16, U::STORAGE);
        let emit_draw =
            buffer(device, "vo-particles-emit-draw", MAX_EMITTERS as u64 * size_of::<GpuEmitDraw>() as u64, U::STORAGE);
        let slot_draw = buffer(
            device,
            "vo-particles-slot-draw",
            all_slots as u64 * size_of::<GpuSlotDraw>() as u64,
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
                storage(7, true, S::COMPUTE),
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
                uniform(0, true, size_of::<GpuFrame>() as u64, vf),
                storage(1, true, S::VERTEX),
                storage(2, true, S::VERTEX),
                storage(3, true, S::VERTEX),
                storage(4, true, S::VERTEX),
                storage(5, true, S::VERTEX),
                storage(6, true, S::VERTEX),
                storage(7, true, S::VERTEX),
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
        let draw_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-particles-draw"),
            layout: &draw_layout,
            entries: &[
                entry(0, &frame, Some(size_of::<GpuFrame>() as u64)),
                entry(1, &free, Some(free_bytes)),
                entry(2, &emit_draw, None),
                entry(3, &words, Some(word_count * size_of::<GpuWord>() as u64)),
                entry(4, &slot_draw, Some(all_slots as u64 * size_of::<GpuSlotDraw>() as u64)),
                entry(5, &trail, Some(trail_bytes)),
                entry(6, &trail_owner, None),
                entry(7, &paths, None),
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
        let step_bind =
            Self::make_step_bind(device, &step_layout, &steps, &free, &emits, &trail, &masks, 1, &sampler, &paths);

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
            slots: vec![Slot::default(); all_slots as usize],
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
            paths,
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
            frame_data: GpuFrame::default(),
            view_keys: [None; VIEWS as usize],
            ribbons: Vec::new(),
            step_data: Vec::new(),
            emit_data: Vec::new(),
            target_data: Vec::new(),
            word_from: None,
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
        paths: &wgpu::Buffer,
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
                entry(7, paths, None),
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
        self.add_emitter_at(spec, self.tick)
    }

    /// Adds an emitter whose pool is stepped from tick `start` on. Ticks before leave it as
    /// added, so the result does not depend on when it was added: a pool for an element born
    /// at `t` starts at `ticks_for(t) - 1` however late the first frame that shows it comes.
    /// When `start` is before the current tick, the pools are rewound to it.
    pub fn add_emitter_at(&mut self, spec: EmitterSpec, start: u64) -> EmitterId {
        assert!((self.emitters.len() as u32) < MAX_EMITTERS, "at most {MAX_EMITTERS} emitters");
        let capacity = spec.capacity.next_multiple_of(WORKGROUP);
        let base = self.free_used;
        assert!(base + capacity <= self.cfg.free_capacity, "free pool is full");
        let trails = spec.trails.min(capacity);
        let trail_base = self.trails_used;
        assert!(trail_base + trails <= self.cfg.trail_capacity, "trail pool is full");
        let id = self.emitters.len() as u32;
        self.emitters.push(Emitter { spec, base, capacity, trail_base, start, path: [0.0; 4] });
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
        let new = &self.initial[base as usize..];
        self.queue.write_buffer(&self.free, base as u64 * size_of::<GpuFree>() as u64, bytemuck::cast_slice(new));
        if start < self.tick {
            self.restore(start);
        }
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
            for (id, e) in self.emitters.iter_mut().enumerate() {
                let s = sim(id as u32, t);
                e.path = path_wave(&s);
                self.emit_data.push(encode_sim(&s, e));
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
                // Pools added after the snapshot were not stepped before their start tick, so
                // their state at the snapshot is the state they were added with.
                let copied = buf.size().min(size);
                let mut enc = self.device.create_command_encoder(&Default::default());
                enc.copy_buffer_to_buffer(buf, 0, &self.free, 0, copied);
                self.queue.submit([enc.finish()]);
                let rest = (copied / size_of::<GpuFree>() as u64) as usize;
                if rest < self.initial.len() {
                    self.queue.write_buffer(&self.free, copied, bytemuck::cast_slice(&self.initial[rest..]));
                }
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
            &self.paths,
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
        self.set_word_dots(slot, t, glyphs, px_per_unit, DOT_AREA);
    }

    /// [`Particles::set_word`] with one particle per `dot_area` px² of ink: 1.25 makes a
    /// dense swarm, 4 to 6 distinct dots. The particles carry the solid word's light
    /// together whatever the area, so the hand-over neither dims nor flashes.
    pub fn set_word_dots(&mut self, slot: u32, t: f64, glyphs: &[WordGlyph], px_per_unit: f32, dot_area: f32) {
        self.set_word_held(slot, t, glyphs, px_per_unit, dot_area, glyphs.len());
    }

    /// [`Particles::set_word_dots`] that holds glyphs `held_from..` as particles: they form
    /// but do not hand over to solid text. Setting the word again with a larger `held_from`
    /// (the same glyphs, or a revision) releases them from `t` at the normal rate. A glyph that
    /// is already solid is not held again.
    /// The next fresh word's particles start within `spread` text sizes of `from` (relative
    /// to the word's first glyph) rather than in a cloud round the word
    /// (`WordParticles::from`).
    pub fn set_word_from(&mut self, from: Option<Point>, spread: f32) {
        self.word_from = from.map(|f| (f, spread));
    }

    pub fn set_word_held(
        &mut self,
        slot: u32,
        t: f64,
        glyphs: &[WordGlyph],
        px_per_unit: f32,
        dot_area: f32,
        held_from: usize,
    ) {
        let (base, cap) = self.slot_span(slot);
        let dot_area = dot_area.max(0.25);
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
        let source = self.word_from.take();

        // Points and budget per glyph: about one particle per 1.25 px² of ink.
        let pts: Vec<Arc<GlyphPoints>> = glyphs
            .iter()
            .map(|g| match self.face(g.font) {
                Some(face) => self.points.get(g.font, &face, g.glyph, g.size, px_per_unit),
                None => Arc::new(GlyphPoints::default()),
            })
            .collect();
        let px2 = px_per_unit * px_per_unit;
        let want: Vec<f32> = pts.iter().map(|p| p.area * px2 / dot_area).collect();
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

        // Which glyph each new glyph continues, and how it came in.
        let n_glyphs = glyphs.len();
        let info: Vec<(GlyphState, f64, f32, Option<usize>)> = glyphs
            .iter()
            .zip(&pts)
            .enumerate()
            .map(|(gi, (g, p))| {
                let old_i = if fresh {
                    None
                } else if gi < prefix {
                    Some(gi)
                } else if gi >= n_glyphs - suffix {
                    Some(old.glyphs.len() - (n_glyphs - gi))
                } else {
                    None
                };
                match old_i {
                    Some(o) => (GlyphState::Kept, old.glyphs[o].reveal_t0, old.glyphs[o].reveal_delay, Some(o)),
                    None if fresh => {
                        let cx = g.pos.x + 0.5 * (p.bounds[0] + p.bounds[2]);
                        (GlyphState::Fresh, t, SWEEP * ((cx - x0) / width).clamp(0.0, 1.0), None)
                    }
                    None => (GlyphState::Changed, t, CHANGED_DELAY, None),
                }
            })
            .collect();
        let counts: Vec<usize> = want.iter().map(|w| (w * k).round() as usize).collect();
        let ranges =
            allocate(cap as usize, &counts, |gi| info[gi].3.map(|o| old.glyphs[o].range).filter(|r| r.1 == counts[gi]));

        self.target_data.clear();
        self.target_data.resize(cap as usize, GpuTarget::default());
        let mut slot_glyphs = Vec::with_capacity(n_glyphs);
        let mut points = Vec::new();
        for (gi, (g, p)) in glyphs.iter().zip(&pts).enumerate() {
            let (state, mut reveal_t0, mut reveal_delay, old_i) = info[gi];
            let was_held = old_i.is_some_and(|o| old.glyphs[o].held);
            let held = gi >= held_from && (state != GlyphState::Kept || was_held);
            if was_held && !held {
                // Released: hand over from now.
                reveal_t0 = t;
                reveal_delay = 0.0;
            }
            let range = ranges[gi];
            slot_glyphs.push(SlotGlyph { glyph: *g, reveal_t0, reveal_delay, state, range, held });
            let n = range.1;
            let n_in = if p.interior.is_empty() { 0 } else { (n as f32 * interior_share).round() as usize };
            let n_c = if p.contour.is_empty() { 0 } else { n - n_in };
            let pick = |list: &[[f32; 2]], j: usize, n: usize| {
                list[((j as f32 + 0.5) * list.len() as f32 / n as f32) as usize]
            };
            let from = source.filter(|_| state == GlyphState::Fresh);
            let flags = T_ACTIVE
                | if state == GlyphState::Changed { T_CHANGED } else { 0 }
                | if held { T_HELD } else { 0 }
                | if from.is_some() { T_START | T_FROM } else { 0 };
            for j in 0..n_c + n_in {
                let q = if j < n_c { pick(&p.contour, j, n_c) } else { pick(&p.interior, j - n_c, n_in) };
                let pos = [g.pos.x + q[0], g.pos.y + q[1]];
                points.push(pos);
                self.target_data[range.0 + j] = GpuTarget {
                    pos,
                    flags,
                    glyph: gi as u32,
                    reveal_t0: reveal_t0 as f32,
                    reveal_delay,
                    start: from.map_or([0.0; 2], |(f, from_spread)| {
                        // A small disc round the source, the same for each particle every run.
                        let h = |k: u32| ((j as u32).wrapping_mul(2_654_435_761) ^ k).wrapping_mul(0x9e37_79b9) >> 8;
                        let a = h(0x51ed) as f32 / (1 << 24) as f32 * std::f32::consts::TAU;
                        let r = (h(0x7f4a) as f32 / (1 << 24) as f32).sqrt() * from_spread * size;
                        [f.x + r * a.cos(), f.y + r * a.sin()]
                    }),
                };
            }
        }
        let anchor = GpuAnchor {
            slot,
            capacity: cap,
            base,
            gen: old.gen + 1,
            seed: self.cfg.seed,
            t: t as f32,
            size,
            w: WORD_RATE,
            x0,
            width,
            scatter: SCATTER,
            stroke: 0.09 * size,
        };
        self.form(anchor);
        let ink_px: f32 = pts.iter().map(|p| p.area * px2).sum();
        let ink = if points.is_empty() { dot_area } else { ink_px / points.len() as f32 };
        let hi = ranges.iter().map(|r| r.0 + r.1).fold(old.hi, usize::max);
        self.slots[slot as usize] = Slot { gen: anchor.gen, glyphs: slot_glyphs, points, ink, hi };
    }

    /// Forms `form.points` in slot `slot` from time `t` (PA09 and any shape): dots that
    /// assemble into a check mark, a burst that settles into a logo, the dot grid peeling off
    /// on dismiss (empty points). Particles already formed move on from where they are.
    pub fn set_points(&mut self, slot: u32, t: f64, form: &PointForm) {
        let (base, cap) = self.slot_span(slot);
        let cap = cap as usize;
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
            base,
            gen: old.gen + 1,
            seed: self.cfg.seed,
            t: t as f32,
            size: form.size,
            w: WORD_RATE,
            x0,
            width,
            scatter: if form.scatter { SCATTER } else { 0.0 },
            stroke: 0.09 * form.size,
        };
        self.form(anchor);
        let glyph = SlotGlyph {
            glyph: WordGlyph { font: 0, glyph: 0, size: form.size, pos: Point::ZERO },
            reveal_t0: t,
            reveal_delay: form.sweep,
            state: if old.points.is_empty() { GlyphState::Fresh } else { GlyphState::Changed },
            range: (0, n),
            held: false,
        };
        self.slots[slot as usize] = Slot {
            gen: anchor.gen,
            glyphs: if n > 0 { vec![glyph] } else { Vec::new() },
            points: form.points[..n].iter().map(|p| [p.x, p.y]).collect(),
            ink: DOT_AREA,
            hi: old.hi.max(n),
        };
    }

    /// Slot `slot`'s first particle in the words buffer and its particle count: word slots
    /// hold `slot_capacity` each, the shape slots after them `shape_capacity`.
    fn slot_span(&self, slot: u32) -> (u32, u32) {
        slot_span(&self.cfg, slot)
    }

    /// The particles slot `slot` can form: `slot_capacity` for a word slot, `shape_capacity`
    /// for a shape slot.
    pub fn slot_capacity(&self, slot: u32) -> u32 {
        self.slot_span(slot).1
    }

    /// Formed particles across all slots: the length of the words buffer.
    pub fn formed_capacity(&self) -> u32 {
        let (base, cap) = slot_span(&self.cfg, self.cfg.word_slots + self.cfg.shape_slots - 1);
        base + cap
    }

    /// Uploads `self.target_data` for `anchor.slot` and runs the re-anchor pass.
    fn form(&mut self, anchor: GpuAnchor) {
        let base = anchor.base as u64 * size_of::<GpuTarget>() as u64;
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
            Some(g) if g.held => 0.0,
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
            .map(|g| if g.held { f64::INFINITY } else { g.reveal_t0 + (g.reveal_delay + x / WORD_RATE_MIN) as f64 })
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
            viewport: [p.width as f32, p.height as f32, srgb as u32 as f32, self.cfg.tick_hz as f32],
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
        self.frame_data = frame;
        self.view_keys = [None; VIEWS as usize];
        self.ribbons.clear();
        let draws: Vec<GpuEmitDraw> = self
            .emitters
            .iter()
            .enumerate()
            .map(|(id, e)| {
                let l = look(id as u32);
                let r = l.rotation;
                // Ribbons follow the path's strands, so only path emitters draw them.
                let ribbons = if e.path[0] > 0.0 { l.ribbons.min(e.path[0] as u32) } else { 0 };
                self.ribbons.push(if l.brightness > 0.0 { ribbons } else { 0 });
                GpuEmitDraw {
                    origin: [l.origin.x, l.origin.y, l.z, l.size],
                    rot: [
                        [r[0][0], r[0][1], r[0][2], l.brightness],
                        [r[1][0], r[1][1], r[1][2], 0.0],
                        [r[2][0], r[2][1], r[2][2], 0.0],
                    ],
                    c: l.ramp.map(linear),
                    look: style_code(l.style),
                    trail: match l.color_by {
                        ColorBy::Age => [l.trail_width, l.trail_alpha, 0.0, 0.0],
                        ColorBy::Angle => [l.trail_width, l.trail_alpha, 1.0, 0.0],
                        ColorBy::X { half } => [l.trail_width, l.trail_alpha, 2.0, half],
                    },
                    pool: [e.base, e.capacity, e.trail_base, if e.spec.three_d { E_3D } else { 0 }],
                    path: e.path,
                    ribbon: [ribbons as f32, l.ribbon_width, l.ribbon_alpha, 0.0],
                }
            })
            .collect();
        if !draws.is_empty() {
            self.queue.write_buffer(&self.emit_draw, 0, bytemuck::cast_slice(&draws));
        }
        let sd: Vec<GpuSlotDraw> = (0..self.slots.len())
            .map(|i| match slots.get(i) {
                Some(s) => {
                    let mut look = style_code(s.style);
                    look[2] = self.slots[i].ink;
                    GpuSlotDraw {
                        origin: [s.origin.x, s.origin.y],
                        visible: s.visible as u32 as f32,
                        size: s.size,
                        color: linear(s.color),
                        look,
                    }
                }
                None => GpuSlotDraw::default(),
            })
            .collect();
        self.queue.write_buffer(&self.slot_draw, 0, bytemuck::cast_slice(&sd));
        let key = (p.format, p.blend, p.depth, p.samples.max(1));
        self.ensure_pipelines_for(key);
        self.draw_key = Some(key);
        self.view_keys[0] = Some(key);
    }

    /// Sets up view `view` (1 to [`VIEWS`] - 1) after [`Particles::prepare`]: the same frame
    /// drawn into another target, such as an offscreen layer, with its own transform, size
    /// and format. Blend, depth and samples are the prepared frame's.
    /// `camera` replaces the prepared frame's camera for this view.
    pub fn set_view(
        &mut self,
        view: u32,
        to_px: Affine,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        camera: Option<Camera>,
    ) {
        assert!(view > 0 && view < VIEWS, "view {view} is not 1 to {}", VIEWS - 1);
        let Some((_, blend, depth, samples)) = self.draw_key else { return };
        let m = to_px;
        let srgb = !format.is_srgb() && !matches!(format, wgpu::TextureFormat::Rgba16Float);
        let px_per_unit = (m.a * m.d - m.b * m.c).abs().sqrt();
        let frame = GpuFrame {
            m: [m.a, m.b, m.c, m.d],
            t: [m.e, m.f, px_per_unit, 0.0],
            viewport: [width as f32, height as f32, srgb as u32 as f32, self.cfg.tick_hz as f32],
            ..self.frame_data
        };
        let frame = match camera {
            Some(c) => GpuFrame { view_proj: c.view_proj, cam: [1.0, c.focal_px, 0.0, 0.0], ..frame },
            None => frame,
        };
        self.queue.write_buffer(&self.frame, view as u64 * VIEW_STRIDE, bytemuck::bytes_of(&frame));
        let key = (format, blend, depth, samples);
        self.ensure_pipelines_for(key);
        self.view_keys[view as usize] = Some(key);
    }

    /// Draws one emitter's particles and ribbons, or one slot's formed particles, in view
    /// `view` (0 is the frame [`Particles::prepare`] set up).
    pub fn draw_part(&self, pass: &mut wgpu::RenderPass<'_>, view: u32, part: Part) {
        let Some(key) = self.view_keys.get(view as usize).copied().flatten() else { return };
        let Some(pipes) = self.draw_pipes.iter().find(|d| d.key == key) else { return };
        pass.set_bind_group(0, &self.draw_bind, &[view * VIEW_STRIDE as u32]);
        match part {
            Part::Emitter(id) => {
                let Some(e) = self.emitters.get(id as usize) else { return };
                let ribbons = self.ribbons.get(id as usize).copied().unwrap_or(0);
                if ribbons > 0 {
                    pass.set_pipeline(&pipes.ribbon);
                    pass.draw(0..ribbons * RIBBON_SEGMENTS * 6, id..id + 1);
                }
                pass.set_pipeline(&pipes.free);
                pass.draw(e.base * 6..(e.base + e.capacity) * 6, 0..1);
                let trails = e.spec.trails.min(e.capacity);
                if trails > 0 {
                    let per = (TRAIL - 1) * 6;
                    pass.set_pipeline(&pipes.trail);
                    pass.draw(e.trail_base * per..(e.trail_base + trails) * per, 0..1);
                }
            }
            Part::Slot(slot) => {
                if let Some(s) = self.slots.get(slot as usize).filter(|s| s.gen > 0 && s.hi > 0) {
                    let (base, _) = self.slot_span(slot);
                    pass.set_pipeline(&pipes.word);
                    pass.draw(base * 6..(base + s.hi as u32) * 6, 0..1);
                }
            }
        }
    }

    /// Gives emitter `id` the path its `Shape::Path` particles follow, in emitter space, start
    /// to end. Takes effect from the next tick.
    pub fn set_path(&mut self, id: EmitterId, points: &[Point]) {
        if id >= MAX_EMITTERS {
            return;
        }
        let data = resample_path(points);
        self.queue.write_buffer(&self.paths, (id * PATH_POINTS) as u64 * 16, bytemuck::cast_slice(&data));
    }

    /// Clears every slot's formed particles, as before the first [`Particles::set_word`].
    pub fn reset_slots(&mut self) {
        let words = self.formed_capacity() as usize;
        self.queue.write_buffer(&self.words, 0, bytemuck::cast_slice(&vec![GpuWord::default(); words]));
        for s in &mut self.slots {
            *s = Slot::default();
        }
    }

    /// Clears slot `slot`'s formed particles at once, so the next form in it starts fresh
    /// rather than from the particles of whatever the slot held before.
    pub fn clear_slot(&mut self, slot: u32) {
        let (base, cap) = self.slot_span(slot);
        let zero = vec![GpuWord::default(); cap as usize];
        self.queue.write_buffer(&self.words, base as u64 * size_of::<GpuWord>() as u64, bytemuck::cast_slice(&zero));
        self.slots[slot as usize] = Slot::default();
    }

    /// Makes the additive sprite pipelines for `format` without depth, so the first frame
    /// that draws particles does not compile them.
    pub fn ensure_pipelines(&mut self, format: wgpu::TextureFormat) {
        self.ensure_pipelines_for((format, ParticleBlend::Add, None, 1));
    }

    fn ensure_pipelines_for(&mut self, key: DrawKey) {
        if self.draw_pipes.iter().any(|d| d.key == key) {
            return;
        }
        let (format, blend, depth, samples) = key;
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
                multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
                multiview_mask: None,
                cache: None,
            })
        };
        let pipes = DrawPipes {
            key,
            free: make("vs_free"),
            trail: make("vs_trail"),
            word: make("vs_word"),
            ribbon: make("vs_ribbon"),
        };
        self.draw_pipes.push(pipes);
    }

    /// Records the sprite draws into `pass`, as [`Particles::prepare`] set them up: free
    /// particles, their ribbons, then formed particles.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(key) = self.draw_key else { return };
        let Some(pipes) = self.draw_pipes.iter().find(|d| d.key == key) else { return };
        pass.set_bind_group(0, &self.draw_bind, &[0]);
        for (id, &ribbons) in self.ribbons.iter().enumerate().filter(|r| *r.1 > 0) {
            pass.set_pipeline(&pipes.ribbon);
            pass.draw(0..ribbons * RIBBON_SEGMENTS * 6, id as u32..id as u32 + 1);
        }
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
            pass.draw(0..self.formed_capacity() * 6, 0..1);
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
        Shape::Path { width, .. } => (6, [width, 0.0, 0.0, 0.0]),
    };
    let shock = s.shock.map_or([-1.0, 0.0, 1.0, 0.0], |k| [k.age, k.speed, k.width, k.strength]);
    let a = s.attract;
    let g = s.gravity;
    GpuEmitStep {
        spawn: [kind, e.spec.seed, s.epoch, if e.spec.three_d { E_3D } else { 0 }],
        shape,
        life: [s.life, s.life_jitter, s.emit, s.spawn_speed],
        swirl: [s.swirl, s.ring_pull, s.ring_radius, s.drag],
        noise: [s.noise, s.noise_scale, s.noise_speed, e.start as f32],
        attract: [a[0], a[1], a[2], s.attract_strength],
        gravity: [g[0], g[1], g[2], s.mask],
        shock,
        pool: [e.base, e.capacity, e.trail_base, e.spec.trails.min(e.capacity)],
        path: path_wave(s),
    }
}

/// A path emitter's strand wave: strands, sway, wavelength, wave speed.
fn path_wave(s: &EmitterSim) -> [f32; 4] {
    match s.shape {
        Shape::Path { strands, sway, wavelength, wave_speed, .. } => {
            [strands.max(1) as f32, sway, wavelength, wave_speed]
        }
        _ => [0.0; 4],
    }
}

/// `points` resampled to [`PATH_POINTS`] points at equal arc length, with the length so far
/// in `z` (so the last point's `z` is the whole length).
fn resample_path(points: &[Point]) -> Vec<[f32; 4]> {
    let n = PATH_POINTS as usize;
    let mut cum = vec![0.0f32; points.len()];
    for i in 1..points.len() {
        let (a, b) = (points[i - 1], points[i]);
        cum[i] = cum[i - 1] + (b.x - a.x).hypot(b.y - a.y);
    }
    let len = cum.last().copied().unwrap_or(0.0);
    let mut out = Vec::with_capacity(n);
    let mut j = 0;
    for i in 0..n {
        let s = len * i as f32 / (n - 1) as f32;
        while j + 2 < points.len() && cum[j + 1] < s {
            j += 1;
        }
        let p = match points.len() {
            0 => Point::ZERO,
            1 => points[0],
            _ => {
                let f = ((s - cum[j]) / (cum[j + 1] - cum[j]).max(1e-6)).clamp(0.0, 1.0);
                let (a, b) = (points[j], points[j + 1]);
                Point::new(a.x + (b.x - a.x) * f, a.y + (b.y - a.y) * f)
            }
        };
        out.push([p.x, p.y, s, 0.0]);
    }
    out
}

/// Places each glyph's particles in a slot of `cap`: glyph `i` keeps the range `keep(i)`
/// gives (a glyph that stayed keeps its particles), the others go into the free gaps, each
/// preferring the first gap after the glyph before it, so a changed glyph takes over the
/// particles of the glyph it replaced. Counts are cut to fit; when the gaps are too broken
/// up, everything is laid out again from the start.
fn allocate(cap: usize, counts: &[usize], keep: impl Fn(usize) -> Option<(usize, usize)>) -> Vec<(usize, usize)> {
    let mut ranges: Vec<Option<(usize, usize)>> = (0..counts.len()).map(&keep).collect();
    let mut used: Vec<(usize, usize)> = ranges.iter().flatten().copied().collect();
    used.sort();
    let overlaps = used.windows(2).any(|w| w[0].0 + w[0].1 > w[1].0) || used.iter().any(|r| r.0 + r.1 > cap);
    let mut gaps = Vec::new();
    if !overlaps {
        let mut at = 0;
        for r in &used {
            if r.0 > at {
                gaps.push((at, r.0));
            }
            at = r.0 + r.1;
        }
        if at < cap {
            gaps.push((at, cap));
        }
        let mut prev_end = 0;
        let mut ok = true;
        for i in 0..counts.len() {
            if let Some(r) = ranges[i] {
                prev_end = r.0 + r.1;
                continue;
            }
            let n = counts[i];
            let pick = gaps
                .iter()
                .position(|g| g.1 > prev_end.max(g.0) && g.1 - prev_end.max(g.0) >= n)
                .or_else(|| gaps.iter().position(|g| g.1 - g.0 >= n));
            let Some(gi) = pick else {
                ok = false;
                break;
            };
            let g = gaps[gi];
            let start = if g.1 > prev_end.max(g.0) && g.1 - prev_end.max(g.0) >= n { prev_end.max(g.0) } else { g.0 };
            ranges[i] = Some((start, n));
            prev_end = start + n;
            // Split the gap around the taken range.
            gaps.remove(gi);
            if start + n < g.1 {
                gaps.insert(gi, (start + n, g.1));
            }
            if start > g.0 {
                gaps.insert(gi, (g.0, start));
            }
        }
        if ok {
            return ranges.into_iter().map(|r| r.unwrap()).collect();
        }
    }
    let mut at = 0;
    counts
        .iter()
        .map(|&n| {
            let n = n.min(cap - at);
            let r = (at, n);
            at += n;
            r
        })
        .collect()
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
        assert_eq!(size_of::<GpuEmitStep>(), 160);
        assert_eq!(size_of::<GpuStep>(), 64);
        assert_eq!(size_of::<GpuWord>(), 64);
        assert_eq!(size_of::<GpuTarget>(), 32);
        assert_eq!(size_of::<GpuAnchor>(), 48);
        assert_eq!(size_of::<GpuFrame>(), 160);
        assert_eq!(size_of::<GpuEmitDraw>(), 192);
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
    fn allocation_keeps_kept_glyphs_and_fills_their_gaps() {
        // "wards" to "words": glyphs 0, 2, 3, 4 keep their ranges; 1 takes the gap 1 left.
        let old = [(0, 40), (40, 30), (70, 25), (95, 30), (125, 28)];
        let counts = [40, 32, 25, 30, 28];
        let r = allocate(1024, &counts, |i| (i != 1).then_some(old[i]));
        assert_eq!(r[0], (0, 40));
        assert_eq!(r[2..], old[2..]);
        // 32 do not fit in the 30-wide gap: it goes after the last glyph.
        assert_eq!(r[1], (153, 32));
        let r = allocate(1024, &[40, 30, 25], |i| (i != 1).then_some(old[i]));
        assert_eq!(r[1], (40, 30), "the replaced glyph's own particles");
        // No room: a fresh layout.
        let r = allocate(100, &[60, 60], |i| (i == 0).then_some((20, 60)));
        assert_eq!(r, vec![(0, 60), (60, 40)]);
    }

    #[test]
    fn a_view_of_part_of_the_target_sees_the_same_perspective() {
        let project = |c: &Camera, p: [f32; 3], size: [f32; 2]| {
            let m = c.view_proj;
            let mut o = [0.0f32; 4];
            for (col, v) in m.iter().zip([p[0], p[1], p[2], 1.0]) {
                for r in 0..4 {
                    o[r] += col[r] * v;
                }
            }
            [(o[0] / o[3] * 0.5 + 0.5) * size[0], (0.5 - o[1] / o[3] * 0.5) * size[1]]
        };
        let (target, scale, d) = ([640.0, 360.0], 2.0, 500.0);
        let whole = Camera::overlay_view(target, [0.0, 0.0], target, scale, d);
        let part = Camera::overlay_view(target, [200.0, 90.0], [180.0, 120.0], scale, d);
        for p in [[150.0, 70.0, 0.0], [130.0, 60.0, 120.0], [160.0, 80.0, -300.0]] {
            let a = project(&whole, p, target);
            let b = project(&part, p, [180.0, 120.0]);
            assert!((a[0] - 200.0 - b[0]).abs() < 1e-2 && (a[1] - 90.0 - b[1]).abs() < 1e-2, "{p:?}: {a:?} {b:?}");
        }
        // The plane z = 0 lands on the pixels the 2D transform gives.
        let a = project(&whole, [150.0, 70.0, 0.0], target);
        assert!((a[0] - 300.0).abs() < 1e-3 && (a[1] - 140.0).abs() < 1e-3, "{a:?}");
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

    #[test]
    fn a_path_is_resampled_at_equal_arc_length() {
        // An L: 30 units right, then 10 down.
        let l = [Point::new(0.0, 0.0), Point::new(30.0, 0.0), Point::new(30.0, 10.0)];
        let r = resample_path(&l);
        assert_eq!(r.len(), PATH_POINTS as usize);
        assert_eq!(r[0], [0.0, 0.0, 0.0, 0.0]);
        let last = r[r.len() - 1];
        assert!((last[0] - 30.0).abs() < 1e-4 && (last[1] - 10.0).abs() < 1e-4 && (last[2] - 40.0).abs() < 1e-4);
        let step = 40.0 / (PATH_POINTS - 1) as f32;
        for (i, p) in r.iter().enumerate() {
            assert!((p[2] - step * i as f32).abs() < 1e-3, "point {i} at length {}", p[2]);
            // On the L: on the first leg while the length is under 30, then on the second.
            if p[2] <= 30.0 {
                assert!((p[0] - p[2]).abs() < 1e-3 && p[1].abs() < 1e-3);
            } else {
                assert!((p[0] - 30.0).abs() < 1e-3 && (p[1] - (p[2] - 30.0)).abs() < 1e-3);
            }
        }
        // Degenerate paths do not panic.
        assert!(resample_path(&[]).iter().all(|p| *p == [0.0; 4]));
        assert!(resample_path(&[Point::new(2.0, 3.0)]).iter().all(|p| p[0] == 2.0 && p[1] == 3.0 && p[2] == 0.0));
    }
}
