//! The glass pass: one pipeline and one uniform block, drawn after the scene with
//! "destination over" blending so the panel lands behind the text. Self-contained for the
//! spike; it moves onto the effect path once the render graph has a slot for it.

use vo::scene::geom::Color;

/// What one glass panel looks like at one instant. Lengths are logical pixels.
#[derive(Clone, Copy, Debug)]
pub struct GlassParams {
    /// Panel rect x, y, w, h.
    pub rect: [f32; 4],
    pub radius: f32,
    /// Seconds, for anything that moves.
    pub time: f32,
    /// The compositor blurs behind the panel.
    pub blur_behind: bool,
    /// Interior tint and its alpha; the alpha is what the contrast gate checks.
    pub tint: Color,
    /// Alpha of the tint right at the edge, below the interior alpha.
    pub edge_alpha: f32,
    pub accent: Color,
    /// Orbit phase 0..1, or `None` when the rim is still.
    pub orbit: Option<f32>,
    pub orbit_strength: f32,
    /// Sheen sweep progress 0..1, or `None` outside the sweep.
    pub sheen: Option<f32>,
    pub dark: bool,
}

const UNIFORM_SIZE: u64 = 6 * 16;

pub struct GlassPass {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    bind: wgpu::BindGroup,
    encode_srgb: bool,
}

fn linear(c: Color) -> [f32; 3] {
    let f = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    [f(c.r), f(c.g), f(c.b)]
}

impl GlassPass {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> GlassPass {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glass"),
            source: wgpu::ShaderSource::Wgsl(include_str!("glass.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glass"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(UNIFORM_SIZE),
                },
                count: None,
            }],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glass"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        // Destination over: the panel goes behind whatever the scene already drew.
        let over = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glass"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_glass"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_glass"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState { color: over, alpha: over }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glass"),
            size: UNIFORM_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glass"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
        });
        GlassPass { pipeline, buffer, bind, encode_srgb: !format.is_srgb() }
    }

    /// Writes the uniforms and records the pass into `view`, which keeps its contents.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        size: [u32; 2],
        scale: f32,
        p: &GlassParams,
        timestamps: Option<wgpu::RenderPassTimestampWrites<'_>>,
    ) {
        let s = scale;
        let tint = linear(p.tint);
        let accent = linear(p.accent);
        let u: [f32; 24] = [
            p.rect[0] * s,
            p.rect[1] * s,
            p.rect[2] * s,
            p.rect[3] * s,
            p.radius * s,
            s,
            p.time,
            if p.blur_behind { 1.0 } else { 0.0 },
            tint[0],
            tint[1],
            tint[2],
            p.tint.a,
            p.orbit.unwrap_or(-1.0),
            p.orbit_strength,
            p.sheen.unwrap_or(-1.0),
            if p.dark { 1.0 } else { 0.0 },
            accent[0],
            accent[1],
            accent[2],
            p.edge_alpha,
            size[0] as f32,
            size[1] as f32,
            if self.encode_srgb { 1.0 } else { 0.0 },
            0.0,
        ];
        let bytes: Vec<u8> = u.iter().flat_map(|v| v.to_le_bytes()).collect();
        queue.write_buffer(&self.buffer, 0, &bytes);
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("glass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: timestamps,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..4, 0..1);
    }
}
