//! What the last frame cost and what it drew, for budgets, cost tables and the preview's
//! inspection overlays.
//!
//! [`Renderer::set_profiling`](crate::Renderer::set_profiling) turns on GPU timestamps: the
//! renderer writes one into the command encoder before the frame and after each section
//! (a surface's passes, a bloom pyramid, the effect path's last pass), and resolves them at
//! the end of the frame. [`Renderer::last_timings`](crate::Renderer::last_timings) reads them
//! back once the frame is submitted. It needs `TIMESTAMP_QUERY` and
//! `TIMESTAMP_QUERY_INSIDE_ENCODERS` on the device; [`timestamp_features`] gives both.
//!
//! [`Renderer::frame_report`](crate::Renderer::frame_report) lists the surfaces of the last
//! frame: which layer each offscreen texture belongs to, what it covers, and how far its
//! bloom reaches. It costs nothing until it is asked for.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Timestamps at most per frame. A frame with more sections times the first ones only.
const MAX_MARKS: u32 = 256;

/// The device features GPU timing needs.
pub fn timestamp_features() -> wgpu::Features {
    wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS
}

/// A part of the frame with a time of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    /// The passes that draw one surface, pass breaks included: the target or the effect
    /// path's scene (surface 0), or an offscreen layer or mask.
    Draw { surface: usize },
    /// One surface's bloom pyramid, down and up.
    Bloom { surface: usize },
    /// The effect path's last pass: tone map, encode and dither into the target.
    Final,
}

/// One section's GPU time.
#[derive(Clone, Debug, PartialEq)]
pub struct PassTime {
    pub section: Section,
    /// The layer the section draws, when it is an offscreen layer or its bloom.
    pub layer: Option<u32>,
    /// `target`, `scene`, `layer 3`, `layer 3 bloom`, `mask 4` or `final`.
    pub label: String,
    pub ms: f64,
}

/// GPU times of one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameTimings {
    pub passes: Vec<PassTime>,
    /// From the first timestamp to the last.
    pub total_ms: f64,
}

impl FrameTimings {
    /// The sum over sections that `keep` accepts.
    pub fn sum(&self, keep: impl Fn(&PassTime) -> bool) -> f64 {
        self.passes.iter().filter(|p| keep(p)).map(|p| p.ms).sum()
    }
}

/// What a surface of the last frame is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceKind {
    /// Surface 0 on the direct path: the caller's target.
    Target,
    /// Surface 0 on the effect path: the linear scene texture.
    Scene,
    /// An offscreen layer's texture.
    Layer,
    /// A mask layer's coverage texture.
    Mask,
}

/// One surface of the last frame. Rectangles are in target pixels, `x0, y0, x1, y1`.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceReport {
    pub kind: SurfaceKind,
    /// The display list layer, for offscreen layers and masks.
    pub layer: Option<u32>,
    /// What the surface's content covers, before any bloom.
    pub content: [f32; 4],
    /// The texture's rectangle: the content plus the bloom's reach, rounded up to the pool's
    /// size steps. Empty when the layer drew nothing.
    pub texture: [f32; 4],
    pub bloom: Option<BloomReport>,
    /// The layer keeps a feedback trail (it covers the whole target).
    pub trail: bool,
    /// The layer is composited with a style (FX13).
    pub style: bool,
    /// The layer is composited through a mask.
    pub masked: bool,
    /// Draw commands recorded for the surface.
    pub draws: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BloomReport {
    pub levels: u32,
    pub strength: f32,
    /// How far the bloom visibly reaches past the content, in pixels: three times its radius.
    /// The texture is padded further, so the faint tail is kept.
    pub reach: f32,
}

/// The last frame: its path, its surfaces and how many instances it drew.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameReport {
    /// The frame took the effect path (bloom or trails).
    pub effects: bool,
    /// The target's size in pixels.
    pub size: [u32; 2],
    pub surfaces: Vec<SurfaceReport>,
    pub shapes: usize,
    pub glyphs: usize,
    pub dots: usize,
    pub vectors: usize,
    pub glass: usize,
    pub gpu_items: usize,
    /// Bytes in pooled layer, scene and bloom textures.
    pub pool_bytes: u64,
}

impl SurfaceReport {
    /// A short name: `target`, `scene`, `layer 3` or `mask 4`.
    pub fn label(&self) -> String {
        match (self.kind, self.layer) {
            (SurfaceKind::Target, _) => "target".into(),
            (SurfaceKind::Scene, _) => "scene".into(),
            (SurfaceKind::Layer, Some(l)) => format!("layer {l}"),
            (SurfaceKind::Mask, Some(l)) => format!("mask {l}"),
            (SurfaceKind::Layer, None) => "layer".into(),
            (SurfaceKind::Mask, None) => "mask".into(),
        }
    }
}

/// The query set and buffers, made when profiling is turned on.
pub(crate) struct Profiler {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    /// Nanoseconds per timestamp tick.
    period: f64,
    /// The section that ends at timestamp `i + 1`.
    marks: RefCell<Vec<Section>>,
    /// Timestamps written into the frame encoded last, 0 when none.
    written: u32,
}

impl Profiler {
    /// `None` when the device lacks [`timestamp_features`].
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Profiler> {
        if !device.features().contains(timestamp_features()) {
            return None;
        }
        let set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("vo-profile"),
            ty: wgpu::QueryType::Timestamp,
            count: MAX_MARKS,
        });
        let size = MAX_MARKS as u64 * 8;
        let buffer = |usage, label| {
            device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false })
        };
        Some(Profiler {
            set,
            resolve: buffer(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC, "vo-profile-resolve"),
            read: buffer(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, "vo-profile-read"),
            period: queue.get_timestamp_period() as f64,
            marks: RefCell::new(Vec::new()),
            written: 0,
        })
    }

    /// The first timestamp of a frame.
    pub(crate) fn begin(&self, enc: &mut wgpu::CommandEncoder) {
        self.marks.borrow_mut().clear();
        enc.write_timestamp(&self.set, 0);
    }

    /// The timestamp that ends `section`.
    pub(crate) fn mark(&self, enc: &mut wgpu::CommandEncoder, section: Section) {
        let mut marks = self.marks.borrow_mut();
        let i = marks.len() as u32 + 1;
        if i < MAX_MARKS {
            enc.write_timestamp(&self.set, i);
            marks.push(section);
        }
    }

    /// Resolves the frame's timestamps into the read-back buffer.
    pub(crate) fn finish(&mut self, enc: &mut wgpu::CommandEncoder) {
        let n = self.marks.borrow().len() as u32 + 1;
        enc.resolve_query_set(&self.set, 0..n, &self.resolve, 0);
        enc.copy_buffer_to_buffer(&self.resolve, 0, &self.read, 0, n as u64 * 8);
        self.written = n;
    }

    /// Reads the last frame's times. Blocks until the GPU has run it, so the frame must have
    /// been submitted. `label` names a section.
    pub(crate) fn read(
        &mut self,
        device: &wgpu::Device,
        label: impl Fn(Section) -> (Option<u32>, String),
    ) -> Option<FrameTimings> {
        let n = std::mem::take(&mut self.written);
        if n < 2 {
            return None;
        }
        let done = Arc::new(AtomicBool::new(false));
        let ok = Arc::new(AtomicBool::new(false));
        let (d, o) = (done.clone(), ok.clone());
        let slice = self.read.slice(..n as u64 * 8);
        slice.map_async(wgpu::MapMode::Read, move |r| {
            o.store(r.is_ok(), Ordering::Release);
            d.store(true, Ordering::Release);
        });
        while !done.load(Ordering::Acquire) {
            if device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).is_err() {
                return None;
            }
        }
        if !ok.load(Ordering::Acquire) {
            return None;
        }
        let ticks: Vec<u64> = {
            let data = slice.get_mapped_range().ok()?;
            bytemuck::cast_slice::<u8, u64>(&data).to_vec()
        };
        self.read.unmap();
        let ms = |a: u64, b: u64| b.saturating_sub(a) as f64 * self.period * 1e-6;
        let marks = self.marks.borrow();
        let passes = marks
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let (layer, label) = label(*s);
                PassTime { section: *s, layer, label, ms: ms(ticks[i], ticks[i + 1]) }
            })
            .collect();
        Some(FrameTimings { passes, total_ms: ms(ticks[0], ticks[n as usize - 1]) })
    }
}
