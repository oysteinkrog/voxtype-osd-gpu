//! One shader quad drawn straight over a GPU element's surface, premultiplied over: the pass
//! the hero object (`sdf`) and the grid floor (`grid`) share. The shader has one uniform at
//! group 0 binding 0, a vertex entry that places the quad from the uniform, and a fragment
//! entry; the quad is a 4-vertex strip.

use vo_render::text::slug_gpu::PREMULTIPLIED_OVER;
use vo_render::PaintCx;

pub(crate) struct QuadPass {
    label: &'static str,
    wgsl: &'static str,
    entries: (&'static str, &'static str),
    pipeline: Option<(wgpu::TextureFormat, wgpu::BindGroupLayout, wgpu::RenderPipeline)>,
}

impl QuadPass {
    pub(crate) const fn new(label: &'static str, wgsl: &'static str, vs: &'static str, fs: &'static str) -> QuadPass {
        QuadPass { label, wgsl, entries: (vs, fs), pipeline: None }
    }

    fn prepare(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        if self.pipeline.as_ref().is_some_and(|(f, ..)| *f == format) {
            return;
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(self.label),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(self.label),
            source: wgpu::ShaderSource::Wgsl(self.wgsl.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(self.label),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(self.label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some(self.entries.0),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(self.entries.1),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(PREMULTIPLIED_OVER),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        self.pipeline = Some((format, layout, pipeline));
    }

    /// Draws the quad with `uniform` over `cx`'s surface, scissored to `q` (`x0, y0, x1, y1`
    /// in surface pixels, whole and not empty). The uniform gets its own buffer, so several
    /// draws can go into one submit.
    pub(crate) fn draw(&mut self, cx: &mut PaintCx<'_>, q: [f32; 4], uniform: &[u8]) {
        self.prepare(cx.device, cx.format);
        let (_, layout, pipeline) = self.pipeline.as_ref().unwrap();
        let buffer = cx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(self.label),
            size: uniform.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: true,
        });
        buffer.slice(..).get_mapped_range_mut().unwrap().copy_from_slice(uniform);
        buffer.unmap();
        let bind = cx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(self.label),
            layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
        });
        let mut pass = cx.enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(self.label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: cx.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_scissor_rect(q[0] as u32, q[1] as u32, (q[2] - q[0]) as u32, (q[3] - q[1]) as u32);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..4, 0..1);
    }
}

/// `q` (`x0, y0, x1, y1`) cut to `cx`'s scissor and rounded out to whole pixels; `None` when
/// nothing is left.
pub(crate) fn clip_to_scissor(cx: &PaintCx<'_>, q: [f32; 4]) -> Option<[f32; 4]> {
    let [sx, sy, sw, sh] = cx.scissor;
    let q = [q[0].max(sx as f32), q[1].max(sy as f32), q[2].min((sx + sw) as f32), q[3].min((sy + sh) as f32)];
    let q = [q[0].floor(), q[1].floor(), q[2].ceil(), q[3].ceil()];
    (q[2] > q[0] && q[3] > q[1]).then_some(q)
}
