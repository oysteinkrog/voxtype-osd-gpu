//! Offscreen rendering to CPU memory, for goldens and export (plan decision 10).
//!
//! [`Offscreen`] draws display lists into its own texture and copies each frame into a ring
//! of staging buffers mapped with `map_async`, so an export can keep the GPU busy on frame
//! `n + 1` while the CPU reads frame `n`. [`Offscreen::render_to_rgba`] is the one-shot form.
//!
//! Pixels come back as RGBA8 in an sRGB texture: linear light, premultiplied, then sRGB
//! encoded per channel. [`write_png`] (feature `png`) turns them into a straight-alpha PNG.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use vo_scene::display::DisplayList;

use crate::renderer::{Renderer, Target};

/// The format of the offscreen texture: blending in linear light, sRGB encoded in memory.
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Staging buffers in the ring.
const RING: usize = 3;

/// One frame read back from the GPU.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, rows top to bottom, no padding. Premultiplied.
    pub rgba: Vec<u8>,
}

struct Slot {
    buf: wgpu::Buffer,
    mapped: Arc<AtomicBool>,
    width: u32,
    height: u32,
    row: u32,
    submission: Option<wgpu::SubmissionIndex>,
}

pub struct Offscreen {
    pub renderer: Renderer,
    target: Option<Target>,
    slots: Vec<Slot>,
    /// Slots holding a frame not yet read, oldest first.
    pending: VecDeque<usize>,
    next: usize,
}

impl Offscreen {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Offscreen {
        Offscreen { renderer: Renderer::new(device, queue), target: None, slots: Vec::new(), pending: VecDeque::new(), next: 0 }
    }

    /// Draws `list` at `scale` into a `size` (physical pixels) image and returns its pixels.
    /// Frames still in the ring from [`Offscreen::submit`] are read and dropped.
    pub fn render_to_rgba(&mut self, list: &DisplayList, size: [u32; 2], scale: f32) -> Vec<u8> {
        self.finish();
        self.submit(list, size, scale);
        self.finish().pop().expect("one frame").rgba
    }

    /// Starts a frame. When the ring is full, waits for the oldest frame and returns it.
    pub fn submit(&mut self, list: &DisplayList, size: [u32; 2], scale: f32) -> Option<Frame> {
        let ready = if self.pending.len() == RING { self.read_oldest() } else { None };
        let [w, h] = size;
        if self.target.as_ref().is_none_or(|t| t.width() != w || t.height() != h) {
            self.target = Some(Target::new(self.renderer.device(), w, h, FORMAT));
        }
        let i = self.next;
        self.next = (self.next + 1) % RING;
        let row = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let bytes = row as u64 * h as u64;
        let device = self.renderer.device().clone();
        if self.slots.get(i).is_none_or(|s| s.buf.size() < bytes) {
            let slot = Slot {
                buf: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("vo-readback"),
                    size: bytes,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                mapped: Arc::new(AtomicBool::new(false)),
                width: w,
                height: h,
                row,
                submission: None,
            };
            if i < self.slots.len() {
                self.slots[i] = slot;
            } else {
                self.slots.push(slot);
            }
        }
        let target = self.target.as_ref().unwrap();
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vo-offscreen") });
        self.renderer.encode(list, scale, target, &mut enc);
        let slot = &mut self.slots[i];
        (slot.width, slot.height, slot.row) = (w, h, row);
        enc.copy_texture_to_buffer(
            target.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &slot.buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        slot.submission = Some(self.renderer.queue().submit([enc.finish()]));
        slot.mapped.store(false, Ordering::Release);
        let flag = slot.mapped.clone();
        slot.buf.slice(..bytes).map_async(wgpu::MapMode::Read, move |r| {
            r.expect("map readback buffer");
            flag.store(true, Ordering::Release);
        });
        self.pending.push_back(i);
        ready
    }

    /// Waits for every frame still in the ring and returns them, oldest first.
    pub fn finish(&mut self) -> Vec<Frame> {
        let mut out = Vec::with_capacity(self.pending.len());
        while let Some(f) = self.read_oldest() {
            out.push(f);
        }
        out
    }

    fn read_oldest(&mut self) -> Option<Frame> {
        let i = self.pending.pop_front()?;
        let slot = &mut self.slots[i];
        let device = self.renderer.device();
        while !slot.mapped.load(Ordering::Acquire) {
            device
                .poll(wgpu::PollType::Wait { submission_index: slot.submission.clone(), timeout: None })
                .expect("GPU readback");
        }
        let (w, h, row) = (slot.width, slot.height, slot.row);
        let bytes = row as u64 * h as u64;
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        {
            let data = slot.buf.slice(..bytes).get_mapped_range().expect("mapped");
            for y in 0..h {
                let start = (y * row) as usize;
                rgba.extend_from_slice(&data[start..start + (w * 4) as usize]);
            }
        }
        slot.buf.unmap();
        Some(Frame { width: w, height: h, rgba })
    }
}

/// Writes premultiplied sRGB-encoded pixels (as [`Offscreen`] returns them) as an 8-bit
/// straight-alpha RGBA PNG.
#[cfg(feature = "png")]
pub fn write_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
    let straight: Vec<u8> =
        rgba.chunks_exact(4).flat_map(|p| crate::color::unpremultiply_srgb8([p[0], p[1], p[2], p[3]])).collect();
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut w = enc.write_header().map_err(std::io::Error::other)?;
    w.write_image_data(&straight).map_err(std::io::Error::other)
}
