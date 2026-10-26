//! GPU particles (CAPABILITIES PA01, PA03, PA04, PA06, PA07; ROADMAP 4.7).
//!
//! [`Particles`] holds two kinds of particle, both in storage buffers that never come back to
//! the CPU:
//!
//! - **Free particles** in fixed-slot pools, one pool per emitter ([`Particles::add_ring`]).
//!   They step at a fixed tick (`ParticleConfig::tick_hz`), one compute dispatch per tick.
//!   A tick's inputs come from [`EmitterSim`] values the caller computes at the tick's own
//!   time, and every random number is `rand(seed, emitter, slot, spawn)`. So the state after
//!   tick `n` is the same however the ticks were batched into frames: a 144 Hz live run and a
//!   60 fps export run the same ticks with the same inputs. The draw interpolates between the
//!   last two ticks at the frame time.
//! - **Word particles** ([`Particles::set_word`]): a fixed pool per word slot. A word's
//!   particles do not step. Each holds an anchor and its position is a closed-form critically
//!   damped spring, evaluated at the frame time. A change of text re-anchors the slot at the
//!   change's own time, by evaluating the old spring exactly there, so the result does not
//!   depend on when frames happen. The CPU knows each glyph's convergence in closed form, so
//!   [`Particles::glyph_alpha`] gives the solid glyph's opacity with no readback.
//!
//! Drawing: [`Particles::prepare`] writes the frame's uniforms, then [`Particles::draw`] records
//! two vertex-pulled draws (6 vertices per particle, no vertex buffers) into any render pass,
//! with additive blending. A layer that wants particles (an offscreen layer with bloom, for
//! example) draws them into its own pass with its own transform.
//!
//! Seek: going back in time restores the last snapshot at or before the target tick
//! ([`Particles::snapshot`]), or the start state, then steps forward. Word particles need no
//! seek of their own, but the text changes before the target time must be applied again.

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

/// The compute shader that re-anchors a word slot.
pub fn anchor_shader_source() -> String {
    [COMMON_WGSL, ANCHOR_WGSL].join("\n")
}

/// The sprite shaders for both kinds of particle.
pub fn draw_shader_source() -> String {
    [COMMON_WGSL, DRAW_WGSL].join("\n")
}

/// Workgroup size of the compute passes, and the alignment of every pool.
pub const WORKGROUP: u32 = 64;
/// Most ticks recorded into one submission.
pub const MAX_BATCH: u32 = 128;
/// Most emitters.
pub const MAX_EMITTERS: u32 = 16;
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
    /// Word slots, and particles per slot (rounded up to [`WORKGROUP`]).
    pub word_slots: u32,
    pub slot_capacity: u32,
    /// Fixed tick of the free particles.
    pub tick_hz: f64,
    pub seed: u32,
}

impl Default for ParticleConfig {
    fn default() -> Self {
        ParticleConfig { free_capacity: 20_480, word_slots: 32, slot_capacity: 1024, tick_hz: 120.0, seed: 1 }
    }
}

/// A ring emitter's simulation inputs at one tick time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmitterSim {
    /// Ring radius in logical units.
    pub radius: f32,
    /// Swirl along the ring, radians per second.
    pub swirl: f32,
    /// The level, 0 to 1. Only recorded; `radius`, `swirl` and `noise` carry its effect.
    pub level: f32,
    /// Curl noise strength in units per second squared.
    pub noise: f32,
    /// Mean particle life in seconds.
    pub life: f32,
    /// Spawn band as a fraction of the radius.
    pub spread: f32,
    /// Curl noise scale: noise cells per logical unit.
    pub scale: f32,
}

/// A ring emitter's look at the frame time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmitterLook {
    /// Centre of the ring in layer space.
    pub origin: Point,
    /// Sprite radius in logical units.
    pub size: f32,
    pub brightness: f32,
    /// Colour at birth, in mid-life and at death (sRGB, straight alpha).
    pub ramp: [Color; 3],
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

/// A word slot's look at the frame time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SlotLook {
    /// Where the word's origin is in layer space this frame.
    pub origin: Point,
    pub color: Color,
    pub visible: bool,
}

/// What [`Particles::prepare`] needs about the frame.
#[derive(Clone, Copy, Debug)]
pub struct FrameParams {
    pub t: f64,
    /// Layer space (logical units) to target pixels.
    pub to_px: Affine,
    /// Physical pixels per logical unit along the transform (sprite sizes).
    pub px_per_unit: f32,
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
    /// Velocity stretch: a sprite streaks over this many seconds of its motion (PA07).
    pub stretch: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuFree {
    pos: [f32; 2],
    prev: [f32; 2],
    vel: [f32; 2],
    age: f32,
    life: f32,
    spawn: u32,
    emitter: u32,
    r: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuEmitStep {
    radius: f32,
    swirl: f32,
    level: f32,
    noise: f32,
    life: f32,
    spread: f32,
    scale: f32,
    seed: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuStep {
    index: u32,
    count: u32,
    emitters: u32,
    _pad: u32,
    t: f32,
    dt: f32,
    _pad2: [f32; 2],
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
    _pad: [f32; 2],
}

const T_ACTIVE: u32 = 1;
const T_CHANGED: u32 = 2;

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
    reveal0: f32,
    reveal1: f32,
    w_min: f32,
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuEmitDraw {
    origin: [f32; 2],
    size: f32,
    brightness: f32,
    c: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GpuSlotDraw {
    origin: [f32; 2],
    visible: f32,
    _pad: f32,
    color: [f32; 4],
}

/// Index of an emitter in a [`Particles`].
pub type EmitterId = u32;

struct Emitter {
    seed: u32,
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
}

/// Fixed-step GPU particles and word particles on one device.
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
    /// The free pool has been stepped to this tick.
    tick: u64,
    initial: Vec<GpuFree>,
    snapshot: Option<(u64, wgpu::Buffer)>,
    slots: Vec<Slot>,

    free: wgpu::Buffer,
    emits: wgpu::Buffer,
    steps: wgpu::Buffer,
    words: wgpu::Buffer,
    targets: wgpu::Buffer,
    anchor: wgpu::Buffer,
    frame: wgpu::Buffer,
    emit_draw: wgpu::Buffer,
    slot_draw: wgpu::Buffer,

    step_bind: wgpu::BindGroup,
    anchor_bind: wgpu::BindGroup,
    draw_bind: wgpu::BindGroup,
    step_pipe: wgpu::ComputePipeline,
    anchor_pipe: wgpu::ComputePipeline,
    draw_layout: wgpu::PipelineLayout,
    draw_module: wgpu::ShaderModule,
    draw_pipes: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline, wgpu::RenderPipeline)>,
    draw_format: Option<wgpu::TextureFormat>,
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

fn premul_linear(c: Color) -> [f32; 4] {
    [srgb_to_linear(c.r), srgb_to_linear(c.g), srgb_to_linear(c.b), c.a]
}

impl Particles {
    /// Creates the buffers and the two compute pipelines. Sprite pipelines are made on the
    /// first [`Particles::prepare`] for a target format.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, fonts: FontRegistry, cfg: ParticleConfig) -> Particles {
        let cfg = ParticleConfig {
            free_capacity: cfg.free_capacity.next_multiple_of(WORKGROUP).max(WORKGROUP),
            slot_capacity: cfg.slot_capacity.next_multiple_of(WORKGROUP).max(WORKGROUP),
            word_slots: cfg.word_slots.max(1),
            ..cfg
        };
        use wgpu::BufferUsages as U;
        use wgpu::ShaderStages as S;
        let free_bytes = cfg.free_capacity as u64 * size_of::<GpuFree>() as u64;
        let word_count = cfg.word_slots as u64 * cfg.slot_capacity as u64;
        let free = buffer(device, "vo-particles-free", free_bytes, U::STORAGE | U::COPY_SRC);
        let emits = buffer(
            device,
            "vo-particles-emits",
            (MAX_BATCH * MAX_EMITTERS) as u64 * size_of::<GpuEmitStep>() as u64,
            U::STORAGE,
        );
        let steps = buffer(device, "vo-particles-steps", MAX_BATCH as u64 * STEP_STRIDE, U::UNIFORM);
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

        let step_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-particles-step"),
            entries: &[
                uniform(0, true, size_of::<GpuStep>() as u64, S::COMPUTE),
                storage(1, false, S::COMPUTE),
                storage(2, true, S::COMPUTE),
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
            ],
        });
        let step_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-particles-step"),
            layout: &step_layout,
            entries: &[
                entry(0, &steps, Some(size_of::<GpuStep>() as u64)),
                entry(1, &free, Some(free_bytes)),
                entry(2, &emits, None),
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
        // Word particles start empty: zeroed flags are "not in use".
        let mut enc = device.create_command_encoder(&Default::default());
        enc.clear_buffer(&words, 0, None);
        enc.clear_buffer(&free, 0, None);
        queue.submit([enc.finish()]);

        Particles {
            device: device.clone(),
            queue: queue.clone(),
            cfg,
            fonts,
            faces: Vec::new(),
            points: GlyphPointCache::new(),
            emitters: Vec::new(),
            free_used: 0,
            tick: 0,
            initial: Vec::new(),
            snapshot: None,
            slots: vec![Slot::default(); cfg.word_slots as usize],
            free,
            emits,
            steps,
            words,
            targets,
            anchor,
            frame,
            emit_draw,
            slot_draw,
            step_bind,
            anchor_bind,
            draw_bind,
            step_pipe,
            anchor_pipe,
            draw_layout: draw_pl,
            draw_module,
            draw_pipes: Vec::new(),
            draw_format: None,
            step_data: Vec::new(),
            emit_data: Vec::new(),
            target_data: Vec::new(),
        }
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

    /// Adds a ring emitter (PA04) with a pool of `capacity` free particles (rounded up to
    /// [`WORKGROUP`]). Births are spread over the first `warmup` seconds. Must be called
    /// before the first [`Particles::advance`].
    pub fn add_ring(&mut self, capacity: u32, seed: u32, warmup: f32) -> EmitterId {
        assert_eq!(self.tick, 0, "emitters are added before the first tick");
        assert!((self.emitters.len() as u32) < MAX_EMITTERS, "at most {MAX_EMITTERS} emitters");
        let capacity = capacity.next_multiple_of(WORKGROUP);
        let base = self.free_used;
        assert!(base + capacity <= self.cfg.free_capacity, "free pool is full");
        let id = self.emitters.len() as u32;
        self.emitters.push(Emitter { seed });
        self.free_used += capacity;
        // Not yet born: negative age, staggered by a hash of the slot. Ages sit on the tick
        // grid so a birth happens on a tick, not between two.
        let dt = (1.0 / self.cfg.tick_hz) as f32;
        for i in base..base + capacity {
            let wait = (rand_f32(seed, id, i, 0) * warmup / dt).floor() * dt;
            self.initial.push(GpuFree { age: -wait - dt * 0.5, emitter: id, ..Default::default() });
        }
        self.queue.write_buffer(&self.free, 0, bytemuck::cast_slice(&self.initial));
        id
    }

    /// Takes a GPU-side copy of the free pool at the current tick, for seeking back.
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
        for k in 0..n {
            let tick = self.tick + k as u64;
            let t = tick as f64 * dt;
            let step = GpuStep {
                index: k,
                count: self.free_used,
                emitters: ne,
                t: t as f32,
                dt: dt as f32,
                ..Default::default()
            };
            self.step_data.extend_from_slice(bytemuck::bytes_of(&step));
            self.step_data.resize(((k + 1) as u64 * STEP_STRIDE) as usize, 0);
            for (id, e) in self.emitters.iter().enumerate() {
                let s = sim(id as u32, t);
                self.emit_data.push(GpuEmitStep {
                    radius: s.radius,
                    swirl: s.swirl,
                    level: s.level,
                    noise: s.noise,
                    life: s.life,
                    spread: s.spread,
                    scale: s.scale,
                    seed: e.seed,
                });
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

    fn face(&mut self, font: u32) -> Option<Arc<Face>> {
        if font as usize >= self.faces.len() {
            let more = self.fonts.faces_from(self.faces.len());
            self.faces.extend(more);
        }
        self.faces.get(font as usize).cloned()
    }

    /// Sets the text of word slot `slot` at time `t` (PA03). The first call forms the word
    /// from a cloud; a later call scatters and re-forms only the glyphs that changed (by a
    /// common prefix and suffix match), slides the rest, and scatters away glyphs that went.
    /// An empty `glyphs` clears the slot. `px_per_unit` sets the particle density.
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
                self.target_data[next] = GpuTarget {
                    pos: [g.pos.x + q[0], g.pos.y + q[1]],
                    flags,
                    glyph: gi as u32,
                    reveal_t0: reveal_t0 as f32,
                    reveal_delay,
                    _pad: [0.0; 2],
                };
                next += 1;
            }
        }
        let gen = old.gen + 1;
        let base = slot as u64 * cap as u64 * size_of::<GpuTarget>() as u64;
        self.queue.write_buffer(&self.targets, base, bytemuck::cast_slice(&self.target_data));
        let anchor = GpuAnchor {
            slot,
            capacity: cap,
            gen,
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
        self.queue.write_buffer(&self.anchor, 0, bytemuck::bytes_of(&anchor));
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vo-anchor") });
        {
            let mut pass = enc
                .begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vo-anchor"), timestamp_writes: None });
            pass.set_pipeline(&self.anchor_pipe);
            pass.set_bind_group(0, &self.anchor_bind, &[]);
            pass.dispatch_workgroups(cap / WORKGROUP, 1, 1);
        }
        self.queue.submit([enc.finish()]);
        self.slots[slot as usize] = Slot { gen, glyphs: slot_glyphs };
    }

    /// The opacity of glyph `index` of word slot `slot`'s solid text at `t`: 0 while its
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

    /// Writes the frame's uniforms and makes the sprite pipelines for `p.format` if needed.
    /// `look(emitter)` gives each emitter's look at `p.t`; `slots[i]` is word slot `i`'s
    /// (missing slots are hidden).
    pub fn prepare(&mut self, p: &FrameParams, look: &dyn Fn(EmitterId) -> EmitterLook, slots: &[SlotLook]) {
        let m = &p.to_px;
        let alpha = if self.tick == 0 {
            1.0
        } else {
            ((p.t - self.tick_time(self.tick - 1)) * self.cfg.tick_hz).clamp(0.0, 1.0) as f32
        };
        let srgb = !p.format.is_srgb() && !matches!(p.format, wgpu::TextureFormat::Rgba16Float);
        let frame = GpuFrame {
            m: [m.a, m.b, m.c, m.d],
            t: [m.e, m.f, p.px_per_unit, 0.0],
            viewport: [p.width as f32, p.height as f32, srgb as u32 as f32, 0.0],
            time: p.t as f32,
            alpha,
            stretch: p.stretch,
            reveal0: REVEAL.0,
            reveal1: REVEAL.1,
            w_min: WORD_RATE_MIN,
            _pad: [0.0; 2],
        };
        self.queue.write_buffer(&self.frame, 0, bytemuck::bytes_of(&frame));
        let draws: Vec<GpuEmitDraw> = (0..self.emitters.len() as u32)
            .map(|id| {
                let l = look(id);
                GpuEmitDraw {
                    origin: [l.origin.x, l.origin.y],
                    size: l.size,
                    brightness: l.brightness,
                    c: l.ramp.map(premul_linear),
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
                    _pad: 0.0,
                    color: premul_linear(s.color),
                },
                None => GpuSlotDraw::default(),
            })
            .collect();
        self.queue.write_buffer(&self.slot_draw, 0, bytemuck::cast_slice(&sd));
        self.ensure_pipelines(p.format);
        self.draw_format = Some(p.format);
    }

    /// Makes the sprite pipelines for `format`, so the first frame that draws particles does
    /// not compile them. Returns at once when they exist.
    pub fn ensure_pipelines(&mut self, format: wgpu::TextureFormat) {
        if self.draw_pipes.iter().any(|(f, _, _)| *f == format) {
            return;
        }
        use wgpu::BlendFactor::*;
        let blend = wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: One, dst_factor: One, operation: wgpu::BlendOperation::Add },
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
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let (free, word) = (make("vs_free"), make("vs_word"));
        self.draw_pipes.push((format, free, word));
    }

    /// Records the sprite draws into `pass`, as [`Particles::prepare`] set them up.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(format) = self.draw_format else { return };
        let Some((_, free, word)) = self.draw_pipes.iter().find(|(f, _, _)| *f == format) else { return };
        pass.set_bind_group(0, &self.draw_bind, &[]);
        if self.free_used > 0 {
            pass.set_pipeline(free);
            pass.draw(0..self.free_used * 6, 0..1);
        }
        if self.slots.iter().any(|s| s.gen > 0) {
            pass.set_pipeline(word);
            pass.draw(0..self.cfg.word_slots * self.cfg.slot_capacity * 6, 0..1);
        }
    }

    /// Records a render pass on `view` that loads what is there and draws the particles on
    /// top.
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
        assert_eq!(size_of::<GpuFree>(), 48);
        assert_eq!(size_of::<GpuEmitStep>(), 32);
        assert_eq!(size_of::<GpuStep>(), 32);
        assert_eq!(size_of::<GpuWord>(), 64);
        assert_eq!(size_of::<GpuTarget>(), 32);
        assert_eq!(size_of::<GpuAnchor>(), 48);
        assert_eq!(size_of::<GpuFrame>(), 80);
        assert_eq!(size_of::<GpuEmitDraw>(), 64);
        assert_eq!(size_of::<GpuSlotDraw>(), 32);
    }

    #[test]
    fn reveal_is_readable_within_200_ms() {
        assert_eq!(reveal(0.0), 0.0);
        // The last glyph of a fresh word starts SWEEP late and is solid by 200 ms.
        let solid_at = settle_x() / WORD_RATE_MIN + SWEEP;
        assert!(solid_at < 0.2, "{solid_at}");
        assert!(reveal(solid_at - SWEEP) > 0.999);
        // Monotonic.
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
}
