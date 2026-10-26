//! Puts the preview window together on the GPU from two textures the renderer drew: the stage
//! (the app's display list at the stage's scale) and the chrome (timeline and overlays).
//!
//! The renderer clears its target, and display lists from other lanes grow new arenas often, so
//! the preview never merges lists. It draws each into a texture of its own and composites:
//! background and checkerboard, then the stage at whole pixels with `textureLoad` (so the stage
//! shows exactly the pixels an export writes at that scale), then the chrome.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const WGSL: &str = r#"
struct Blit {
    // Where to draw, clip space: x, y of the top-left corner, then w, h (h negative).
    clip: vec4<f32>,
    // The same rect in target pixels: x, y, w, h.
    px: vec4<f32>,
    // The stage in target pixels, for the checkerboard.
    stage: vec4<f32>,
    bg: vec4<f32>,
    c0: vec4<f32>,
    c1: vec4<f32>,
    // 0: background and checkerboard, 1: the texture at `px`, 1:1.
    mode: u32,
    cell: f32,
    pad0: u32,
    pad1: u32,
};

@group(0) @binding(0) var<uniform> u: Blit;
@group(0) @binding(1) var src: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    return vec4<f32>(u.clip.xy + corners[i] * u.clip.zw, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    if u.mode == 0u {
        let s = u.stage;
        let inside = pos.x >= s.x && pos.y >= s.y && pos.x < s.x + s.z && pos.y < s.y + s.w;
        if !inside {
            return u.bg;
        }
        let cell = vec2<i32>(floor((pos.xy - s.xy) / u.cell));
        if ((cell.x + cell.y) & 1) == 0 {
            return u.c0;
        }
        return u.c1;
    }
    let size = vec2<i32>(textureDimensions(src, 0));
    let p = clamp(vec2<i32>(floor(pos.xy - u.px.xy)), vec2<i32>(0), size - 1);
    return textureLoad(src, p, 0);
}
"#;

/// One draw's parameters, as the shader reads them.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Blit {
    clip: [f32; 4],
    px: [f32; 4],
    stage: [f32; 4],
    bg: [f32; 4],
    c0: [f32; 4],
    c1: [f32; 4],
    mode: u32,
    cell: f32,
    pad: [u32; 2],
}

/// A rectangle in target pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PxRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub struct Compositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    module: wgpu::ShaderModule,
    uniforms: [wgpu::Buffer; 3],
}

/// Linear premultiplied colours of the window.
const BG: [f32; 4] = [0.010, 0.011, 0.014, 1.0];
const CHECK0: [f32; 4] = [0.030, 0.030, 0.034, 1.0];
const CHECK1: [f32; 4] = [0.045, 0.045, 0.050, 1.0];

impl Compositor {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Compositor {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vo-preview-composite"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-preview-composite"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let uniform = || {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vo-preview-blit"),
                size: std::mem::size_of::<Blit>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        Compositor {
            device: device.clone(),
            queue: queue.clone(),
            layout,
            pipelines: Vec::new(),
            module,
            uniforms: [uniform(), uniform(), uniform()],
        }
    }

    fn pipeline(&mut self, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
        if let Some((_, p)) = self.pipelines.iter().find(|(f, _)| *f == format) {
            return p.clone();
        }
        let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&self.layout)],
            immediate_size: 0,
        });
        let p = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vo-preview-composite"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &self.module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.push((format, p.clone()));
        p
    }

    /// Records the window into `target`: background, checkerboard under `stage`, the stage
    /// texture at `stage`, then `chrome` over the whole target. Both textures must be
    /// `TEXTURE_BINDING`; `stage` is drawn 1:1 from its top-left pixel.
    pub fn encode(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::Texture,
        stage_tex: &wgpu::Texture,
        stage: PxRect,
        chrome: &wgpu::Texture,
    ) {
        let (tw, th) = (target.width() as f32, target.height() as f32);
        let full = PxRect { x: 0.0, y: 0.0, w: tw, h: th };
        let blit = |r: PxRect, mode: u32| Blit {
            // Pixels to clip space, y up.
            clip: [2.0 * r.x / tw - 1.0, 1.0 - 2.0 * r.y / th, 2.0 * r.w / tw, -2.0 * r.h / th],
            px: [r.x, r.y, r.w, r.h],
            stage: [stage.x, stage.y, stage.w, stage.h],
            bg: BG,
            c0: CHECK0,
            c1: CHECK1,
            mode,
            cell: 12.0,
            pad: [0; 2],
        };
        let draws: [(Blit, &wgpu::Texture); 3] =
            [(blit(full, 0), chrome), (blit(stage, 1), stage_tex), (blit(full, 1), chrome)];
        let pipeline = self.pipeline(target.format());
        let mut groups = Vec::with_capacity(3);
        for (i, (b, tex)) in draws.iter().enumerate() {
            self.queue.write_buffer(&self.uniforms[i], 0, bytemuck::bytes_of(b));
            let view = tex.create_view(&Default::default());
            groups.push(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.uniforms[i].as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                ],
            }));
        }
        let view = target.create_view(&Default::default());
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vo-preview-composite"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        for g in &groups {
            pass.set_bind_group(0, g, &[]);
            pass.draw(0..6, 0..1);
        }
    }
}

/// Copies `tex` (RGBA8, sRGB encoded, premultiplied) back to the CPU. Blocks.
pub fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture) -> Vec<u8> {
    let (w, h) = (tex.width(), tex.height());
    let row = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vo-preview-shot"),
        size: row as u64 * h as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    let index = queue.submit([enc.finish()]);
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        r.expect("map the screenshot buffer");
        flag.store(true, Ordering::Release);
    });
    while !done.load(Ordering::Acquire) {
        device
            .poll(wgpu::PollType::Wait { submission_index: Some(index.clone()), timeout: None })
            .expect("GPU readback");
    }
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    {
        let data = buf.slice(..).get_mapped_range().expect("mapped");
        for y in 0..h {
            let start = (y * row) as usize;
            rgba.extend_from_slice(&data[start..start + (w * 4) as usize]);
        }
    }
    buf.unmap();
    rgba
}
