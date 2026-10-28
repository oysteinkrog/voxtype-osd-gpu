//! Displacement (EG-03): a layer's backdrop drawn back with each pixel moved by a field.
//!
//! The scene emits `vo_scene::display::Displace` on a layer that also has a full-resolution
//! backdrop (EG-01). Where the backdrop would be drawn back tinted, the renderer draws it
//! displaced instead (`wgsl/displace.wgsl`): one full-screen triangle cut to the backdrop's
//! box, replacing what the surface holds there. Outside the layer's rounded box, and inside
//! the kept reading line, it writes back exactly what was captured, so those pixels are the
//! same as without the pass.
//!
//! Each displacement is one 256-byte block in a uniform buffer bound with a dynamic offset,
//! next to the four image slots the texture source reads (`Renderer::set_material_image`).
//! A frame without a displacement touches none of this.

use bytemuck::{Pod, Zeroable};
use vo_scene::display::{Displace, DisplaceField, DISPLACE_RINGS};
use vo_scene::geom::{Affine, Point};

/// One displacement block in `displace.wgsl` (`struct Displace`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub(crate) struct DisplaceBlock {
    head: [f32; 4],
    a: [f32; 4],
    b: [f32; 4],
    keep: [f32; 4],
    rect: [f32; 4],
    valid: [f32; 4],
    rings: [[f32; 4]; DISPLACE_RINGS],
}

/// Byte distance between two blocks: the dynamic offset alignment every backend accepts.
const STRIDE: usize = 256;

impl DisplaceBlock {
    /// The block for `d` on a layer laid out by `to_px` (layer space to target pixels), with
    /// `ppu` pixels per layer unit, `scale` pixels per surface unit, `rect` the layer's box
    /// and `valid` the part of it the backdrop captured, both in target pixels
    /// (`x0, y0, x1, y1`).
    pub(crate) fn new(
        d: &Displace,
        to_px: &Affine,
        ppu: f32,
        scale: f32,
        rect: [f32; 4],
        valid: [f32; 4],
    ) -> DisplaceBlock {
        let mut b = DisplaceBlock { rect, valid, keep: [1.0, 1.0, 0.0, 0.0], ..Default::default() };
        if let Some(k) = d.keep {
            b.keep = [k.x * scale, k.y * scale, (k.x + k.w) * scale, (k.y + k.h) * scale];
        }
        let px = |p: Point| to_px.apply(p);
        let (kind, count, seed) = match d.field {
            DisplaceField::Ripples { rings, count, wavelength, width } => {
                let n = (count as usize).min(DISPLACE_RINGS);
                for (out, r) in b.rings.iter_mut().zip(&rings[..n]) {
                    let c = px(r.centre);
                    *out = [c.x, c.y, r.radius * ppu, r.amp * ppu];
                }
                b.a = [wavelength.max(1e-3) * ppu, width.max(1e-3) * ppu, 0.0, 0.0];
                (0.0, n as f32, 0)
            }
            DisplaceField::Noise { scale: cell, amount, seed, turn } => {
                b.a = [cell.max(1e-3) * ppu, amount * ppu, turn, 0.0];
                (1.0, 0.0, seed)
            }
            DisplaceField::Curl { scale: cell, amount, seed, turn } => {
                b.a = [cell.max(1e-3) * ppu, amount * ppu, turn, 0.0];
                (2.0, 0.0, seed)
            }
            DisplaceField::PageCurl { line, dir, radius } => {
                let o = px(line);
                // Directions map by the linear part alone.
                let e = px(Point::new(line.x + dir.x, line.y + dir.y));
                let (dx, dy) = (e.x - o.x, e.y - o.y);
                let len = (dx * dx + dy * dy).sqrt().max(1e-6);
                b.a = [o.x, o.y, dx / len, dy / len];
                b.b = [radius.max(1e-3) * ppu, 0.0, 0.0, 0.0];
                (3.0, 0.0, 0)
            }
            DisplaceField::Texture { slot, amount, bias } => {
                b.a = [amount[0] * ppu, amount[1] * ppu, bias[0], bias[1]];
                b.b = [0.0, slot as f32, 0.0, 0.0];
                (4.0, 0.0, 0)
            }
        };
        b.head = [kind, d.strength, count, f32::from_bits(seed)];
        b
    }
}

/// The displacement pipeline's group 2 and this frame's blocks.
pub(crate) struct DisplaceGpu {
    pub(crate) layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    empty: wgpu::TextureView,
    data: Vec<u8>,
    buf: Option<wgpu::Buffer>,
    /// Made in `upload` for a frame with blocks.
    pub(crate) bind: Option<wgpu::BindGroup>,
}

impl DisplaceGpu {
    pub(crate) fn new(device: &wgpu::Device) -> DisplaceGpu {
        let tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-displace"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<DisplaceBlock>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                tex(2),
                tex(3),
                tex(4),
                tex(5),
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vo-displace"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vo-displace-empty"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            // wgpu zeroes it: an empty slot reads transparent black.
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let empty = texture.create_view(&Default::default());
        DisplaceGpu { layout, sampler, empty, data: Vec::new(), buf: None, bind: None }
    }

    pub(crate) fn clear(&mut self) {
        self.data.clear();
    }

    /// Adds a block and returns its dynamic offset.
    pub(crate) fn push(&mut self, block: &DisplaceBlock) -> u32 {
        let at = self.data.len();
        self.data.extend_from_slice(bytemuck::bytes_of(block));
        self.data.resize(at + STRIDE, 0);
        at as u32
    }

    /// Writes this frame's blocks and binds them with the image slots.
    pub(crate) fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, images: &[Option<wgpu::TextureView>]) {
        if self.data.is_empty() {
            self.bind = None;
            return;
        }
        let need = self.data.len() as u64;
        if self.buf.as_ref().is_none_or(|b| b.size() < need) {
            self.buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vo-displace"),
                size: need.next_power_of_two(),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let buf = self.buf.as_ref().unwrap();
        queue.write_buffer(buf, 0, &self.data);
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: buf,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<DisplaceBlock>() as u64),
                }),
            },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
        ];
        for i in 0..4 {
            let view = images.get(i).and_then(|v| v.as_ref()).unwrap_or(&self.empty);
            entries.push(wgpu::BindGroupEntry {
                binding: 2 + i as u32,
                resource: wgpu::BindingResource::TextureView(view),
            });
        }
        self.bind = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-displace"),
            layout: &self.layout,
            entries: &entries,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_fits_its_stride() {
        assert!(std::mem::size_of::<DisplaceBlock>() <= STRIDE);
        assert_eq!(std::mem::size_of::<DisplaceBlock>() % 16, 0);
    }
}
