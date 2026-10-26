//! wgpu side of the Slug glyph path: atlas storage buffer, pipeline, instances.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use skrifa::GlyphId;

use super::slug_encode::{EncodedGlyph, Encoder};

const HB_GPU_VERTEX: &str = include_str!("wgsl/hb-gpu-vertex.wgsl");
const HB_GPU_FRAGMENT: &str = include_str!("wgsl/hb-gpu-fragment.wgsl");
const HB_GPU_DRAW_FRAGMENT: &str = include_str!("wgsl/hb-gpu-draw-fragment.wgsl");
const SLUG_GLYPH: &str = include_str!("wgsl/slug-glyph.wgsl");

/// How the fragment shader measures the pixel footprint in em space, which
/// sets the width of every coverage ramp.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Footprint {
    /// HarfBuzz as shipped: `fwidth`, which is `|dpdx| + |dpdy|` per axis.
    /// On a glyph rotated by `a` it overstates the footprint by
    /// `|cos a| + |sin a|`, up to 1.41 at 45 degrees.
    Fwidth,
    /// Length of the screen-space gradient of each em axis,
    /// `sqrt(dpdx^2 + dpdy^2)`. Equals `fwidth` on upright glyphs and does not
    /// change under rotation.
    #[default]
    AxisLength,
}

const AXIS_LENGTH_FN: &str = "
// Added by vo-render (see slug_gpu::Footprint): em units per pixel along each
// em axis, independent of rotation. Replaces fwidth in the HarfBuzz code.
fn _vo_ems_per_pixel (renderCoord: vec2f) -> vec2f
{
  let dx = dpdx (renderCoord);
  let dy = dpdy (renderCoord);
  return sqrt (dx * dx + dy * dy);
}
";

/// Assembles the glyph shader with the [`Footprint::default`] footprint.
pub fn shader_source() -> String {
    shader_source_with(Footprint::default())
}

/// Assembles the glyph shader. The HarfBuzz files pass the atlas as a
/// `ptr<storage, ...>` function argument, which needs the WGSL extension
/// `unrestricted_pointer_parameters` that naga does not implement. This drops
/// that parameter and its arguments and reads the global `hbgpu_atlas_buf`
/// instead. With [`Footprint::AxisLength`] it also replaces the two `fwidth`
/// calls. The copied files stay byte-identical to upstream; each patch panics
/// if its target text is gone, so a HarfBuzz update that moves it fails the
/// unit tests.
pub fn shader_source_with(footprint: Footprint) -> String {
    let mut hb = [HB_GPU_VERTEX, HB_GPU_FRAGMENT, HB_GPU_DRAW_FRAGMENT].concat();
    if footprint == Footprint::AxisLength {
        for (from, to) in [
            (
                "let pixelsPerEm = 1.0 / fwidth (renderCoord);",
                "let pixelsPerEm = 1.0 / _vo_ems_per_pixel (renderCoord);",
            ),
            ("let emsPerPixel = fwidth (renderCoord);", "let emsPerPixel = _vo_ems_per_pixel (renderCoord);"),
        ] {
            assert!(hb.contains(from), "hb-gpu WGSL changed: {from}");
            hb = hb.replace(from, to);
        }
        hb = [AXIS_LENGTH_FN, &hb].concat();
    }
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
}

impl Uniforms {
    /// Orthographic camera over `w` x `h` physical pixels, Y down.
    pub fn pixels(w: f32, h: f32) -> Self {
        Self {
            mvp: [[2.0 / w, 0.0, 0.0, 0.0], [0.0, -2.0 / h, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [-1.0, 1.0, 0.0, 1.0]],
            viewport: [w, h],
            gamma: 1.0,
            stem_darkening: 0.0,
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
    /// The `vo-core` word id, or [`NO_WORD`].
    pub word: u32,
    /// Linear light, premultiplied alpha, opacity already multiplied in.
    pub color: [f32; 4],
}

/// `GlyphInstance::word` for glyphs outside live text.
pub const NO_WORD: u32 = u32::MAX;

impl GlyphInstance {
    /// A white glyph at `origin` (pixels, Y down), `px_per_unit` pixels per font
    /// unit, rotated by `angle` radians (counter-clockwise on screen).
    pub fn new(entry: &AtlasEntry, origin: [f32; 2], px_per_unit: f32, angle: f32) -> Self {
        let (s, c) = angle.sin_cos();
        let k = px_per_unit;
        // em (x right, y up) -> pixels (x right, y down), then rotate.
        let lin = [k * c, -k * s, -k * s, -k * c];
        Self::with_matrix(entry, lin, origin, [1.0; 4], NO_WORD)
    }

    /// A glyph with an explicit em-to-pixel matrix (by columns), premultiplied linear
    /// `color` and word id.
    pub fn with_matrix(entry: &AtlasEntry, lin: [f32; 4], offset: [f32; 2], color: [f32; 4], word: u32) -> Self {
        Self { ext: entry.extents, lin, offset, loc: entry.loc, word, color }
    }

    /// The vertex buffer layout of the glyph shader's instance input.
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x2, 3 => Uint32, 5 => Uint32, 4 => Float32x4
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GlyphInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AtlasEntry {
    pub loc: u32,
    pub extents: [f32; 4],
}

/// CPU copy of the atlas: blobs widened from i16 to i32 for the storage buffer.
/// It only grows, so an upload after new glyphs only sends the new texels.
#[derive(Default)]
pub struct Atlas {
    pub texels: Vec<[i32; 4]>,
    /// Keyed by caller font id and glyph id.
    pub entries: HashMap<(u32, GlyphId), Option<AtlasEntry>>,
    encoder: Encoder,
}

impl Atlas {
    /// Returns the entry for `gid` of a single-font atlas (font id 0).
    pub fn glyph(&mut self, font: &skrifa::FontRef, gid: GlyphId) -> Option<AtlasEntry> {
        self.glyph_in(0, || font.clone(), gid)
    }

    /// Returns the entry for `gid` in the font the caller calls `font_id`, encoding it on
    /// first use. `font` is only called on a miss. `None` for glyphs with no ink.
    pub fn glyph_in<'a>(
        &mut self,
        font_id: u32,
        font: impl FnOnce() -> skrifa::FontRef<'a>,
        gid: GlyphId,
    ) -> Option<AtlasEntry> {
        if let Some(e) = self.entries.get(&(font_id, gid)) {
            return *e;
        }
        let entry = self.encoder.encode_glyph(&font(), gid).and_then(|g| self.push(&g));
        self.entries.insert((font_id, gid), entry);
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

/// Blend state for premultiplied "over".
pub const PREMULTIPLIED_OVER: wgpu::BlendState = wgpu::BlendState {
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

/// The GPU side of the atlas: the uniform block and the atlas storage buffer, bound as
/// group 0 of every glyph pipeline. Shared by [`SlugRenderer`] and `Renderer`.
pub struct SlugAtlasGpu {
    layout: wgpu::BindGroupLayout,
    ubuf: wgpu::Buffer,
    atlas_buf: Option<wgpu::Buffer>,
    bind: Option<wgpu::BindGroup>,
    /// Texels already in `atlas_buf`.
    uploaded: usize,
}

impl SlugAtlasGpu {
    pub fn new(device: &wgpu::Device) -> Self {
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
        let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("slug-uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { layout, ubuf, atlas_buf: None, bind: None, uploaded: 0 }
    }

    pub fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    /// `None` until the first [`Self::upload`].
    pub fn bind_group(&self) -> Option<&wgpu::BindGroup> {
        self.bind.as_ref()
    }

    pub fn set_uniforms(&self, queue: &wgpu::Queue, u: &Uniforms) {
        queue.write_buffer(&self.ubuf, 0, bytemuck::bytes_of(u));
    }

    /// Uploads the texels added since the last upload, or the whole atlas when the buffer
    /// has to grow.
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, atlas: &Atlas) {
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
            self.uploaded = 0;
        }
        if atlas.texels.len() > self.uploaded {
            let new = &atlas.texels[self.uploaded..];
            queue.write_buffer(
                self.atlas_buf.as_ref().unwrap(),
                (self.uploaded * 16) as u64,
                bytemuck::cast_slice(new),
            );
            self.uploaded = atlas.texels.len();
        }
    }
}

pub struct SlugRenderer {
    pub pipeline: wgpu::RenderPipeline,
    gpu: SlugAtlasGpu,
    instances: Option<wgpu::Buffer>,
    count: u32,
}

impl SlugRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self::with_footprint(device, format, Footprint::default())
    }

    pub fn with_footprint(device: &wgpu::Device, format: wgpu::TextureFormat, footprint: Footprint) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("slug-glyph"),
            source: wgpu::ShaderSource::Wgsl(shader_source_with(footprint).into()),
        });
        let gpu = SlugAtlasGpu::new(device);
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("slug"),
            bind_group_layouts: &[Some(gpu.layout())],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("slug-glyph"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_glyph"),
                compilation_options: Default::default(),
                buffers: &[Some(GlyphInstance::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_glyph"),
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
        Self { pipeline, gpu, instances: None, count: 0 }
    }

    pub fn set_uniforms(&self, queue: &wgpu::Queue, u: &Uniforms) {
        self.gpu.set_uniforms(queue, u);
    }

    /// Uploads the atlas, growing the buffer when needed.
    pub fn upload_atlas(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, atlas: &Atlas) {
        self.gpu.upload(device, queue, atlas);
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
        let (Some(bind), Some(inst)) = (self.gpu.bind_group(), &self.instances) else { return };
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
        for fp in [Footprint::Fwidth, Footprint::AxisLength] {
            let module = naga::front::wgsl::parse_str(&shader_source_with(fp)).expect("parse");
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
                .validate(&module)
                .expect("validate");
        }
    }

    #[test]
    fn axis_length_replaces_every_fwidth_call() {
        let s = shader_source_with(Footprint::AxisLength);
        let calls = s.lines().filter(|l| l.contains("fwidth (") && !l.trim_start().starts_with('*')).count();
        assert_eq!(calls, 0);
    }
}
