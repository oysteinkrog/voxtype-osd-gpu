//! 3D as an item inside a 2D scene: a [`Source3d`] gives the [`Frame3d`] for a time and a
//! size, and [`ItemPainter`] draws it at the item's place in a surface that already holds the
//! layers below it.
//!
//! The 3D pass clears and resolves a whole texture, so the painter draws into a scratch
//! texture the item's size and composites it over the surface with premultiplied over, times
//! the layer opacity. The item is placed by the bounds of its rect in surface pixels; a
//! rotated layer is not followed.

use std::fmt::Debug;

use bytemuck::{Pod, Zeroable};
use vo_render::text::slug_gpu::PREMULTIPLIED_OVER;
use vo_scene::geom::Size;

use crate::frame::Frame3d;
use crate::mesh::Mesh;
use crate::render::Renderer3d;
use crate::MeshId;

const ITEM_WGSL: &str = include_str!("wgsl/item.wgsl");

pub fn shader_source() -> &'static str {
    ITEM_WGSL
}

/// What a 3D item shows. `frame` must be pure in `t`: the scene samples it at any time, in
/// any order. The painter knows a source by its address, so keep one `Arc` alive across views
/// (as with `Element::canvas_shared`); a new source each view uploads its meshes again.
pub trait Source3d: Debug + Send + Sync {
    /// Meshes the frames refer to, uploaded once when the painter first meets this source.
    /// Frame `MeshDraw`s name them by index in this list, as `MeshId(i)`.
    fn meshes(&self) -> Vec<Mesh> {
        Vec::new()
    }

    /// The frame at `t` for an item `size` logical pixels big.
    fn frame(&self, t: f64, size: Size) -> Frame3d;

    /// The scene keeps waking for this item until `until`. Ambient sources return infinity.
    fn until(&self) -> f64 {
        f64::INFINITY
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ItemUniform {
    rect: [f32; 4],
    viewport: [f32; 4],
}

struct Scratch {
    size: [u32; 2],
    format: wgpu::TextureFormat,
    view: wgpu::TextureView,
}

pub struct ItemPainter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pub scene: Renderer3d,
    layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
    pipeline: Option<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    scratch: Option<Scratch>,
    /// Sources whose meshes are uploaded, by address, with the id of their first mesh.
    uploaded: Vec<(usize, u32)>,
}

/// Where an item goes on the surface.
pub struct ItemPlace {
    /// x, y, w, h in surface pixels.
    pub rect: [f32; 4],
    /// Surface size in pixels.
    pub surface: [u32; 2],
    pub opacity: f32,
    /// Logical size of the item, for the source.
    pub size: Size,
    /// x, y, w, h in surface pixels: the item is only drawn inside it.
    pub scissor: Option<[u32; 4]>,
}

impl ItemPainter {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> ItemPainter {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo3d-item"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo3d-item"),
            size: std::mem::size_of::<ItemUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ItemPainter {
            device: device.clone(),
            queue: queue.clone(),
            scene: Renderer3d::new(device, queue),
            layout,
            uniform,
            pipeline: None,
            scratch: None,
            uploaded: Vec::new(),
        }
    }

    /// Records `source` at `t` into `enc`, composited over `view` (which has `format`) at
    /// `at`. Writes queue buffers now, so at most one item per painter per submit.
    pub fn paint(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        source: &dyn Source3d,
        t: f64,
        at: &ItemPlace,
    ) {
        let [x, y, w, h] = at.rect;
        let (x0, y0) = (x.floor(), y.floor());
        let size = [(x + w).ceil() - x0, (y + h).ceil() - y0].map(|v| v.max(1.0) as u32);
        if at.opacity <= 0.0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let base = self.mesh_base(source);
        let mut frame = source.frame(t, at.size);
        for d in &mut frame.meshes {
            d.mesh = MeshId(base + d.mesh.0);
        }
        self.ensure_scratch(size, format);
        let scratch = self.scratch.as_ref().unwrap();
        let scale = if at.size.w > 0.0 { w / at.size.w } else { 1.0 };
        let out = crate::render::Output { view: &scratch.view, size, format, scale };
        self.scene.encode_with(enc, out, &frame, None, None);

        let u = ItemUniform {
            rect: [x0, y0, size[0] as f32, size[1] as f32],
            viewport: [at.surface[0] as f32, at.surface[1] as f32, at.opacity, 0.0],
        };
        self.queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&u));
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo3d-item"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&scratch.view) },
                wgpu::BindGroupEntry { binding: 1, resource: self.uniform.as_entire_binding() },
            ],
        });
        let pipeline = self.pipeline(format);
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vo3d-item"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some([x, y, w, h]) = at.scissor {
            pass.set_scissor_rect(x, y, w, h);
        }
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..4, 0..1);
    }

    /// The id of the source's first mesh, uploading its meshes the first time.
    fn mesh_base(&mut self, source: &dyn Source3d) -> u32 {
        let key = source as *const dyn Source3d as *const () as usize;
        if let Some(&(_, base)) = self.uploaded.iter().find(|(k, _)| *k == key) {
            return base;
        }
        let meshes = source.meshes();
        let mut base = u32::MAX;
        for m in &meshes {
            let id = self.scene.add_mesh(m);
            base = base.min(id.0);
        }
        let base = if meshes.is_empty() { 0 } else { base };
        self.uploaded.push((key, base));
        base
    }

    fn ensure_scratch(&mut self, size: [u32; 2], format: wgpu::TextureFormat) {
        if self.scratch.as_ref().is_some_and(|s| s.size == size && s.format == format) {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vo3d-item-scratch"),
            size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        self.scratch = Some(Scratch { size, format, view });
    }

    fn pipeline(&mut self, format: wgpu::TextureFormat) -> &wgpu::RenderPipeline {
        if self.pipeline.as_ref().is_none_or(|(f, _)| *f != format) {
            let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("vo3d-item"),
                source: wgpu::ShaderSource::Wgsl(ITEM_WGSL.into()),
            });
            let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vo3d-item"),
                bind_group_layouts: &[Some(&self.layout)],
                immediate_size: 0,
            });
            let pipeline = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("vo3d-item"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_item"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_item"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(PREMULTIPLIED_OVER),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            });
            self.pipeline = Some((format, pipeline));
        }
        &self.pipeline.as_ref().unwrap().1
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn item_shader_validates_in_naga() {
        let src = super::shader_source();
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }
}
