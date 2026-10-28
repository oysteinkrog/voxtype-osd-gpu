//! Layer blur and frost (EG-04): a separable Gaussian blur in two passes.
//!
//! The blur is a two-pass Gaussian at full resolution (`wgsl/blur.wgsl`): one pass across,
//! one down, each pairing neighbouring taps into one bilinear fetch. At the radii the looks
//! use (2 to 12 px) this costs about as much as a dual Kawase pyramid and needs no
//! downsampled textures, and it is exact: symmetric, and its weights sum to 1, so an impulse
//! keeps its energy.
//!
//! Two uses:
//! - **Layer blur** (`vo_scene::display::LayerBlur`): after an offscreen layer is drawn, the
//!   layer texture is blurred across into a pooled texture and back down into the layer
//!   texture. The kept reading line is left as drawn.
//! - **Frost** (`vo_scene::display::Layer::frost`): after a backdrop (EG-01) is captured, it is
//!   blurred across into a pooled texture, and the pass down writes straight into the surface
//!   the backdrop came from, mixing the blurred and the sharp capture by the backdrop's
//!   rounded box. The capture reaches past the box by the blur's reach, so the frost near the
//!   box's edge reads what is next to it.
//!
//! The radius is a standard deviation; the kernel reaches three of them. Each pass is one
//! 256-byte block in a uniform buffer bound with a dynamic offset. A frame without a blur
//! touches none of this.

use bytemuck::{Pod, Zeroable};

use crate::hot::wgsl;

const BLUR_WGSL: &str = include_str!("wgsl/blur.wgsl");
const COMMON_WGSL: &str = include_str!("wgsl/common.wgsl");

/// The kernel reaches this many standard deviations.
pub const REACH_SIGMAS: f32 = 3.0;

/// The largest standard deviation drawn, in physical pixels: 96 px of reach, 48 fetches
/// per side and pass. Larger radii are drawn at this one.
pub const MAX_SIGMA_PX: f32 = 32.0;

/// Byte distance between two blocks: the dynamic offset alignment every backend accepts.
const STRIDE: usize = 256;

/// The blur shader: `common.wgsl` and `blur.wgsl`.
pub fn blur_shader_source() -> String {
    [wgsl("common.wgsl", COMMON_WGSL), wgsl("blur.wgsl", BLUR_WGSL)].join("\n")
}

/// The standard deviation drawn for `sigma_px`, clamped to [`MAX_SIGMA_PX`].
pub fn sigma_px(sigma: f32) -> f32 {
    sigma.clamp(0.0, MAX_SIGMA_PX)
}

/// Whole pixels the blur of `sigma_px` reaches on each side.
pub fn reach_px(sigma: f32) -> u32 {
    (sigma_px(sigma) * REACH_SIGMAS).ceil() as u32
}

/// One pass in `blur.wgsl` (`struct Blur`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub(crate) struct BlurBlock {
    head: [f32; 4],
    at: [f32; 4],
    keep: [f32; 4],
    pad: [f32; 4],
}

impl BlurBlock {
    /// A pass along `dir` (`[1, 0]` across, `[0, 1]` down) of standard deviation `sigma` px,
    /// reading a texture whose top left sits at `src` and drawing into one at `dst`, both in
    /// target pixels. `keep` (`x0, y0, x1, y1` in target pixels) is left as it was.
    pub(crate) fn new(dir: [f32; 2], sigma: f32, src: [i32; 2], dst: [i32; 2], keep: Option<[f32; 4]>) -> BlurBlock {
        let s = self::sigma_px(sigma);
        BlurBlock {
            head: [dir[0], dir[1], s, reach_px(s) as f32],
            at: [src[0] as f32, src[1] as f32, dst[0] as f32, dst[1] as f32],
            keep: keep.unwrap_or([1.0, 1.0, 0.0, 0.0]),
            pad: [0.0; 4],
        }
    }
}

/// The blur pipelines, their bind group layouts and this frame's blocks.
pub(crate) struct BlurGpu {
    tex_layout: wgpu::BindGroupLayout,
    block_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    plain: wgpu::RenderPipeline,
    /// With the layer block at group 1; `None` when made without one (`gaussian_blur`).
    frost: Option<wgpu::RenderPipeline>,
    data: Vec<u8>,
    buf: Option<wgpu::Buffer>,
    bind: Option<wgpu::BindGroup>,
}

impl BlurGpu {
    /// Pipelines drawing into `format`. `layer_layout` is the renderer's layer block layout,
    /// for frost.
    pub(crate) fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        layer_layout: Option<&wgpu::BindGroupLayout>,
    ) -> BlurGpu {
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
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-blur-textures"),
            entries: &[
                tex(0),
                tex(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let block_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-blur"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<BlurBlock>() as u64),
                },
                count: None,
            }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vo-blur"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vo-blur"),
            source: wgpu::ShaderSource::Wgsl(blur_shader_source().into()),
        });
        let pipeline = |fs: &str, groups: &[Option<&wgpu::BindGroupLayout>]| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vo-blur"),
                bind_group_layouts: groups,
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("vo-blur"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_blur"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    // Both passes replace: each writes back what it does not blur.
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let plain = pipeline("fs_blur", &[Some(&tex_layout), None, Some(&block_layout)]);
        let frost = layer_layout.map(|l| pipeline("fs_frost", &[Some(&tex_layout), Some(l), Some(&block_layout)]));
        BlurGpu { tex_layout, block_layout, sampler, plain, frost, data: Vec::new(), buf: None, bind: None }
    }

    pub(crate) fn clear(&mut self) {
        self.data.clear();
    }

    /// Adds a block and returns its dynamic offset.
    pub(crate) fn push(&mut self, block: &BlurBlock) -> u32 {
        let at = self.data.len();
        self.data.extend_from_slice(bytemuck::bytes_of(block));
        self.data.resize(at + STRIDE, 0);
        at as u32
    }

    /// Writes this frame's blocks.
    pub(crate) fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.data.is_empty() {
            self.bind = None;
            return;
        }
        let need = self.data.len() as u64;
        if self.buf.as_ref().is_none_or(|b| b.size() < need) {
            self.buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vo-blur"),
                size: need.next_power_of_two(),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let buf = self.buf.as_ref().unwrap();
        queue.write_buffer(buf, 0, &self.data);
        self.bind = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-blur"),
            layout: &self.block_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: buf,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<BlurBlock>() as u64),
                }),
            }],
        }));
    }

    fn textures(&self, device: &wgpu::Device, src: &wgpu::TextureView, sharp: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-blur-textures"),
            layout: &self.tex_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(src) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(sharp) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }

    /// One `fs_blur` pass of block `offset` from `src` into `dst`. `load` keeps what `dst`
    /// holds where the pass discards (the kept box); otherwise `dst` is cleared first.
    pub(crate) fn pass(
        &self,
        device: &wgpu::Device,
        enc: &mut wgpu::CommandEncoder,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
        offset: u32,
        load: bool,
    ) {
        let Some(blocks) = &self.bind else { return };
        let textures = self.textures(device, src, src);
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vo-blur"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: if load { wgpu::LoadOp::Load } else { wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT) },
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.plain);
        pass.set_bind_group(0, &textures, &[]);
        pass.set_bind_group(2, blocks, &[offset]);
        pass.draw(0..3, 0..1);
    }

    /// The frost's pass down: block `offset` reads `across` (the backdrop blurred across) and
    /// `sharp` (the capture), and writes into `surface` inside `scissor` (`x, y, w, h` in its
    /// pixels), with the backdrop's layer block at `uniform` in `layers`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn frost(
        &self,
        device: &wgpu::Device,
        enc: &mut wgpu::CommandEncoder,
        across: &wgpu::TextureView,
        sharp: &wgpu::TextureView,
        surface: &wgpu::TextureView,
        scissor: [u32; 4],
        layers: (&wgpu::BindGroup, u32),
        offset: u32,
    ) {
        let (Some(blocks), Some(pipeline)) = (&self.bind, &self.frost) else { return };
        let textures = self.textures(device, across, sharp);
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vo-frost"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: surface,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let [x, y, w, h] = scissor;
        pass.set_scissor_rect(x, y, w, h);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &textures, &[]);
        pass.set_bind_group(1, layers.0, &[layers.1]);
        pass.set_bind_group(2, blocks, &[offset]);
        pass.draw(0..3, 0..1);
    }
}

/// Blurs `src` (a texture of `format`, filterable, with `TEXTURE_BINDING`) by a Gaussian of
/// standard deviation `sigma_px` texels, with the renderer's passes, and returns the result
/// in a new texture of the same size and format (`RENDER_ATTACHMENT`, `TEXTURE_BINDING`,
/// `COPY_SRC`). For tests and tools; edges clamp.
pub fn gaussian_blur(device: &wgpu::Device, queue: &wgpu::Queue, src: &wgpu::Texture, sigma_px: f32) -> wgpu::Texture {
    let format = src.format();
    let mut gpu = BlurGpu::new(device, format, None);
    let make = |label| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: src.size(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    };
    let (tmp, out) = (make("vo-blur-across"), make("vo-blur-out"));
    let across = gpu.push(&BlurBlock::new([1.0, 0.0], sigma_px, [0, 0], [0, 0], None));
    let down = gpu.push(&BlurBlock::new([0.0, 1.0], sigma_px, [0, 0], [0, 0], None));
    gpu.upload(device, queue);
    let view = |t: &wgpu::Texture| t.create_view(&Default::default());
    let mut enc = device.create_command_encoder(&Default::default());
    gpu.pass(device, &mut enc, &view(src), &view(&tmp), across, false);
    gpu.pass(device, &mut enc, &view(&tmp), &view(&out), down, false);
    queue.submit([enc.finish()]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_fits_its_stride() {
        assert!(std::mem::size_of::<BlurBlock>() <= STRIDE);
        assert_eq!(std::mem::size_of::<BlurBlock>() % 16, 0);
    }

    #[test]
    fn reach_is_three_sigmas_and_capped() {
        assert_eq!(reach_px(4.0), 12);
        assert_eq!(reach_px(0.5), 2);
        assert_eq!(reach_px(1000.0), (MAX_SIGMA_PX * REACH_SIGMAS) as u32);
    }

    #[test]
    fn shader_validates() {
        let src = blur_shader_source();
        let module = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }
}
