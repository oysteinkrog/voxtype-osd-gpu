//! wgpu side of the Slug glyph path: atlas storage buffer, pipeline, instances.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use skrifa::GlyphId;

use super::slug_encode::{EncodedGlyph, Encoder};

const HB_GPU_VERTEX: &str = include_str!("wgsl/hb-gpu-vertex.wgsl");
const HB_GPU_FRAGMENT: &str = include_str!("wgsl/hb-gpu-fragment.wgsl");
const HB_GPU_DRAW_FRAGMENT: &str = include_str!("wgsl/hb-gpu-draw-fragment.wgsl");
const SLUG_GLYPH: &str = include_str!("wgsl/slug-glyph.wgsl");

/// Assembles the glyph shader. The HarfBuzz files pass the atlas as a
/// `ptr<storage, ...>` function argument, which needs the WGSL extension
/// `unrestricted_pointer_parameters` that naga does not implement. This drops
/// that parameter and its arguments and reads the global `hbgpu_atlas_buf`
/// instead. The copied files stay byte-identical to upstream.
pub fn shader_source() -> String {
    let hb = [HB_GPU_VERTEX, HB_GPU_FRAGMENT, HB_GPU_DRAW_FRAGMENT].concat();
    let hb = hb
        .replace("(*hb_gpu_atlas)", "hbgpu_atlas_buf")
        .replace("hb_gpu_atlas: ptr<storage, array<vec4<i32>>, read>", "@@")
        .replace("&hb_gpu_atlas", "@@")
        .replace("hb_gpu_atlas", "@@");
    [strip_marker(&hb), SLUG_GLYPH.to_string()].join("\n")
}

/// Removes each `@@` with the comma that joins it to its neighbour.
fn strip_marker(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("@@") {
        let before = rest[..i].trim_end();
        let after = &rest[i + 2..];
        if let Some(b) = before.strip_suffix(',') {
            out.push_str(b);
            rest = after;
        } else {
            out.push_str(&rest[..i]);
            rest = after.trim_start().strip_prefix(',').map(str::trim_start).unwrap_or(after);
        }
    }
    out.push_str(rest);
    out
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Uniforms {
    pub mvp: [[f32; 4]; 4],
    pub viewport: [f32; 2],
    pub gamma: f32,
    pub stem_darkening: f32,
    pub foreground: [f32; 4],
}

impl Uniforms {
    /// Orthographic camera over `w` x `h` physical pixels, Y down.
    pub fn pixels(w: f32, h: f32) -> Self {
        Self {
            mvp: [
                [2.0 / w, 0.0, 0.0, 0.0],
                [0.0, -2.0 / h, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [-1.0, 1.0, 0.0, 1.0],
            ],
            viewport: [w, h],
            gamma: 1.0,
            stem_darkening: 0.0,
            foreground: [1.0; 4],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct GlyphInstance {
    /// Ink box in font units.
    pub ext: [f32; 4],
    /// Em-to-pixel 2x2 matrix by columns.
    pub lin: [f32; 4],
    /// Pixel position of the glyph origin.
    pub offset: [f32; 2],
    pub loc: u32,
    pub _pad: u32,
}

impl GlyphInstance {
    /// A glyph at `origin` (pixels, Y down), `px_per_unit` pixels per font
    /// unit, rotated by `angle` radians (counter-clockwise on screen).
    pub fn new(entry: &AtlasEntry, origin: [f32; 2], px_per_unit: f32, angle: f32) -> Self {
        let (s, c) = angle.sin_cos();
        let k = px_per_unit;
        // em (x right, y up) -> pixels (x right, y down), then rotate.
        let lin = [k * c, -k * s, -k * s, -k * c];
        Self { ext: entry.extents, lin, offset: origin, loc: entry.loc, _pad: 0 }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AtlasEntry {
    pub loc: u32,
    pub extents: [f32; 4],
}

/// CPU copy of the atlas: blobs widened from i16 to i32 for the storage buffer.
#[derive(Default)]
pub struct Atlas {
    pub texels: Vec<[i32; 4]>,
    pub entries: HashMap<GlyphId, Option<AtlasEntry>>,
    encoder: Encoder,
}

impl Atlas {
    /// Returns the entry for `gid`, encoding it on first use. `None` for
    /// glyphs with no ink.
    pub fn glyph(&mut self, font: &skrifa::FontRef, gid: GlyphId) -> Option<AtlasEntry> {
        if let Some(e) = self.entries.get(&gid) {
            return *e;
        }
        let entry = self.encoder.encode_glyph(font, gid).and_then(|g| self.push(&g));
        self.entries.insert(gid, entry);
        entry
    }

    fn push(&mut self, g: &EncodedGlyph) -> Option<AtlasEntry> {
        if g.texels.is_empty() {
            return None;
        }
        let loc = self.texels.len() as u32;
        self.texels.extend(g.texels.iter().map(|t| t.map(i32::from)));
        Some(AtlasEntry { loc, extents: g.extents })
    }

    pub fn bytes(&self) -> usize {
        self.texels.len() * 16
    }
}

pub struct SlugRenderer {
    pub pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    ubuf: wgpu::Buffer,
    atlas_buf: Option<wgpu::Buffer>,
    bind: Option<wgpu::BindGroup>,
    instances: Option<wgpu::Buffer>,
    count: u32,
}

impl SlugRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("slug-glyph"),
            source: wgpu::ShaderSource::Wgsl(shader_source().into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("slug"),
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
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("slug"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x2, 3 => Uint32];
        let blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("slug-glyph"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_glyph"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<GlyphInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attrs,
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_glyph"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("slug-uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { pipeline, layout, ubuf, atlas_buf: None, bind: None, instances: None, count: 0 }
    }

    pub fn set_uniforms(&self, queue: &wgpu::Queue, u: &Uniforms) {
        queue.write_buffer(&self.ubuf, 0, bytemuck::bytes_of(u));
    }

    /// Uploads the whole atlas, growing the buffer when needed.
    pub fn upload_atlas(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, atlas: &Atlas) {
        let size = (atlas.bytes().max(16) as u64).next_power_of_two();
        if self.atlas_buf.as_ref().is_none_or(|b| b.size() < size) {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("slug-atlas"),
                size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.bind = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("slug"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.ubuf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: buf.as_entire_binding() },
                ],
            }));
            self.atlas_buf = Some(buf);
        }
        if !atlas.texels.is_empty() {
            queue.write_buffer(self.atlas_buf.as_ref().unwrap(), 0, bytemuck::cast_slice(&atlas.texels));
        }
    }

    pub fn set_instances(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, glyphs: &[GlyphInstance]) {
        let size = (std::mem::size_of_val(glyphs).max(64) as u64).next_power_of_two();
        if self.instances.as_ref().is_none_or(|b| b.size() < size) {
            self.instances = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("slug-instances"),
                size,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        if !glyphs.is_empty() {
            queue.write_buffer(self.instances.as_ref().unwrap(), 0, bytemuck::cast_slice(glyphs));
        }
        self.count = glyphs.len() as u32;
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let (Some(bind), Some(inst)) = (&self.bind, &self.instances) else { return };
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.set_vertex_buffer(0, inst.slice(..));
        pass.draw(0..4, 0..self.count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patched_shader_has_no_pointer_params() {
        let s = shader_source();
        assert!(!s.contains("hb_gpu_atlas"));
        assert!(!s.contains("@@"));
        assert!(s.contains("fn hb_gpu_fetch (offset: i32)"));
        assert!(s.contains("_hb_gpu_decode_glyph (renderCoord, glyphLoc_)"));
    }

    #[test]
    fn shader_validates_in_naga() {
        let module = naga::front::wgsl::parse_str(&shader_source()).expect("parse");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .expect("validate");
    }
}
