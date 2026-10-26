//! Draws a `vo_scene` display list into a texture.
//!
//! [`Renderer::render`] walks the layers in paint order and records one command per run of
//! shapes or glyphs, with a 256-byte layer block (transform, clip, opacity) bound at a dynamic
//! offset. Shapes go through the SDF uber-shader in one instanced draw per run; glyphs go
//! through the Slug path. Then it encodes one render pass per offscreen layer, deepest first,
//! and one pass on the target.
//!
//! Layer rules in this first cut:
//! - **Opacity.** `Layer::opacity` is the layer's own value; the renderer multiplies it down
//!   the tree. A direct layer folds it into each instance's alpha. An offscreen layer draws
//!   its content at full opacity and applies the product once, when it is composited.
//! - **Blend.** A direct layer's instances use its blend mode. An offscreen layer's own
//!   instances use `Normal` inside its texture and the layer's mode when composited. `Multiply`
//!   is exact only over an opaque destination (fixed-function blending cannot add the
//!   `src * (1 - dst alpha)` term); `Normal`, `Add` and `Screen` are exact.
//! - **Clip.** A layer's rounded-rect clip is applied by SDF coverage in every fragment. A
//!   layer without its own clip uses its nearest ancestor's. Where clips nest, the outer ones
//!   only act through a scissor of their bounding boxes, so an inner layer is cut square, not
//!   round, at an outer clip's corners.
//! - **Offscreen layers** get a texture the size of the target, so the layer's own transform
//!   still applies and compositing is a 1:1 copy. `effect` is ignored for now.
//! - **Emitters** are not drawn (M4).

use std::ops::Range;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use skrifa::GlyphId;
use vo_scene::font::{Face, FontRegistry};
use vo_scene::display::{BlendMode, DisplayList, GlyphInstance as DlGlyph, ItemKind, LayerTarget};
use vo_scene::geom::{Affine, RRect};

use crate::color::linear_premul;
use crate::shapes::{pixels_per_unit, GpuShape};
use crate::text::slug_gpu::{self, Atlas, GlyphInstance, SlugAtlasGpu, Uniforms, PREMULTIPLIED_OVER};

const COMMON_WGSL: &str = include_str!("wgsl/common.wgsl");
const SHAPES_WGSL: &str = include_str!("wgsl/shapes.wgsl");
const GLYPHS_WGSL: &str = include_str!("wgsl/glyphs.wgsl");
const COMPOSITE_WGSL: &str = include_str!("wgsl/composite.wgsl");

/// The shape shader: `common.wgsl` and `shapes.wgsl`.
pub fn shape_shader_source() -> String {
    [COMMON_WGSL, SHAPES_WGSL].join("\n")
}

/// The display-list glyph shader: the Slug shader, `common.wgsl` and `glyphs.wgsl`.
pub fn glyph_shader_source() -> String {
    [slug_gpu::shader_source().as_str(), COMMON_WGSL, GLYPHS_WGSL].join("\n")
}

/// The offscreen layer composite shader: `common.wgsl` and `composite.wgsl`.
pub fn composite_shader_source() -> String {
    [COMMON_WGSL, COMPOSITE_WGSL].join("\n")
}

/// A texture the renderer draws into: an offscreen texture, or later a swapchain image.
pub struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Target {
    /// Usage of a texture made by [`Target::new`].
    pub const USAGE: wgpu::TextureUsages = wgpu::TextureUsages::RENDER_ATTACHMENT
        .union(wgpu::TextureUsages::COPY_SRC)
        .union(wgpu::TextureUsages::TEXTURE_BINDING);

    /// A new `width` x `height` texture. An sRGB or float `format` blends in linear light; a
    /// plain UNORM format gets sRGB-encoded values from the shaders and blends in that space.
    pub fn new(device: &wgpu::Device, width: u32, height: u32, format: wgpu::TextureFormat) -> Target {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vo-target"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: Self::USAGE,
            view_formats: &[],
        });
        Target::from_texture(texture)
    }

    /// Wraps an existing texture, for example a swapchain image. It needs `RENDER_ATTACHMENT`.
    pub fn from_texture(texture: wgpu::Texture) -> Target {
        let view = texture.create_view(&Default::default());
        Target { texture, view }
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub fn width(&self) -> u32 {
        self.texture.width()
    }

    pub fn height(&self) -> u32 {
        self.texture.height()
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.texture.format()
    }
}

/// One layer block as `common.wgsl` reads it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct LayerUniform {
    m: [f32; 4],
    t: [f32; 4],
    clip_rect: [f32; 4],
    clip_radii: [f32; 4],
    clip_m: [f32; 4],
    clip_t: [f32; 4],
    viewport: [f32; 4],
}

#[derive(Clone, Copy)]
struct ClipState {
    rect: RRect,
    /// Physical pixels to the clip owner's layer space.
    from_px: Affine,
    ppu: f32,
}

#[derive(Clone, Copy)]
struct LayerState {
    to_px: Affine,
    ppu: f32,
    /// Multiplied into this layer's own instances.
    opacity: f32,
    clip: Option<ClipState>,
    /// `x0, y0, x1, y1` in physical pixels.
    scissor: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pipe {
    Shape,
    Glyph,
    Composite,
}

#[derive(Clone, Debug)]
struct Cmd {
    pipe: Pipe,
    /// Instance range for shapes and glyphs; the surface index for a composite.
    range: Range<u32>,
    uniform: u32,
    blend: BlendMode,
    scissor: [f32; 4],
}

/// A render pass: the target (surface 0) or one offscreen layer.
#[derive(Default)]
struct Surface {
    cmds: Vec<Cmd>,
}

struct LayerTex {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}


/// Draws display lists. Holds the pipelines, the glyph atlas and the per-frame buffers, all
/// reused from frame to frame.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// The shared font table, and this renderer's own copy of it so drawing a glyph takes
    /// no lock. The copy catches up when a glyph names a font it does not have yet.
    fonts: FontRegistry,
    faces: Vec<Arc<Face>>,
    atlas: Atlas,
    atlas_gpu: SlugAtlasGpu,
    layer_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    modules: [Option<wgpu::ShaderModule>; 3],
    pipelines: Vec<((wgpu::TextureFormat, Pipe, BlendMode), wgpu::RenderPipeline)>,
    uniform_stride: u32,
    // Per frame, cleared each time.
    shapes: Vec<GpuShape>,
    glyphs: Vec<GlyphInstance>,
    uniforms: Vec<u8>,
    surfaces: Vec<Surface>,
    // GPU buffers, grown when needed.
    shape_buf: Option<wgpu::Buffer>,
    glyph_buf: Option<wgpu::Buffer>,
    uniform_buf: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    layer_texs: Vec<LayerTex>,
}

impl Renderer {
    /// A renderer with a font table of its own. Use [`Renderer::with_fonts`] to share the
    /// scene's table.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Renderer {
        Renderer::with_fonts(device, queue, FontRegistry::new())
    }

    /// A renderer that reads glyph font ids from `fonts`, the same table the scene's text
    /// layout uses.
    pub fn with_fonts(device: &wgpu::Device, queue: &wgpu::Queue, fonts: FontRegistry) -> Renderer {
        let uniform_stride = (std::mem::size_of::<LayerUniform>() as u32)
            .next_multiple_of(device.limits().min_uniform_buffer_offset_alignment);
        let layer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-layer"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<LayerUniform>() as u64),
                },
                count: None,
            }],
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-composite"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        Renderer {
            device: device.clone(),
            queue: queue.clone(),
            fonts,
            faces: Vec::new(),
            atlas: Atlas::default(),
            atlas_gpu: SlugAtlasGpu::new(device),
            layer_layout,
            composite_layout,
            modules: [None, None, None],
            pipelines: Vec::new(),
            uniform_stride,
            shapes: Vec::new(),
            glyphs: Vec::new(),
            uniforms: Vec::new(),
            surfaces: Vec::new(),
            shape_buf: None,
            glyph_buf: None,
            uniform_buf: None,
            layer_texs: Vec::new(),
        }
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Adds a font file (face 0) to the font table and returns the id that
    /// `GlyphInstance::font` uses. Ids count up from 0 in the order fonts are added, across
    /// everyone that shares the table.
    pub fn add_font(&mut self, data: Vec<u8>) -> Result<u32, skrifa::raw::ReadError> {
        self.fonts.add(data)
    }

    /// The font table this renderer reads.
    pub fn fonts(&self) -> &FontRegistry {
        &self.fonts
    }

    /// Draws `list` into `target`, cleared to transparent first, at `list.scale` physical
    /// pixels per logical unit.
    pub fn render(&mut self, list: &DisplayList, target: &mut Target) {
        self.render_at(list, list.scale, target);
    }

    /// [`Renderer::render`] with the scale given here instead of `list.scale`, so one list
    /// can be drawn at several scales.
    pub fn render_at(&mut self, list: &DisplayList, scale: f32, target: &mut Target) {
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vo-render") });
        self.encode(list, scale, target, &mut enc);
        self.queue.submit([enc.finish()]);
    }

    /// Records the frame into `enc` without submitting, so a caller can add a copy after it.
    pub fn encode(&mut self, list: &DisplayList, scale: f32, target: &Target, enc: &mut wgpu::CommandEncoder) {
        let (w, h) = (target.width(), target.height());
        let format = target.format();
        let encode_srgb = !format.is_srgb() && !matches!(format, wgpu::TextureFormat::Rgba16Float);
        self.shapes.clear();
        self.glyphs.clear();
        self.uniforms.clear();
        self.surfaces.clear();
        self.surfaces.push(Surface::default());

        if !list.layers.is_empty() {
            let full = [0.0, 0.0, w as f32, h as f32];
            let top = LayerState { to_px: Affine::IDENTITY, ppu: 1.0, opacity: 1.0, clip: None, scissor: full };
            let frame = Frame { list, scale, viewport: [w as f32, h as f32, encode_srgb as u32 as f32, 0.0] };
            self.visit(&frame, 0, &top, 0);
        }
        self.upload(format, w, h);
        self.encode_passes(target, enc);
    }

    fn visit(&mut self, f: &Frame, li: u32, parent: &LayerState, surface: usize) {
        let layer = &f.list.layers[li as usize];
        let to_px = layer.transform.then(&Affine::scale(f.scale));
        let ppu = pixels_per_unit(&to_px);
        let mut scissor = parent.scissor;
        let clip = match layer.clip {
            Some(rect) => {
                let Some(from_px) = to_px.invert() else { return };
                scissor = intersect(scissor, bounds(&rect, &to_px));
                Some(ClipState { rect, from_px, ppu })
            }
            None => parent.clip,
        };
        if scissor[2] <= scissor[0] || scissor[3] <= scissor[1] {
            return;
        }
        let (draw_surface, opacity, blend) = match layer.target {
            LayerTarget::Direct => (surface, parent.opacity * layer.opacity, layer.blend),
            LayerTarget::Offscreen { .. } => {
                let opacity = parent.opacity * layer.opacity;
                if opacity <= 0.0 {
                    return;
                }
                let uniform = self.push_uniform(f, parent, opacity);
                let child = self.surfaces.len();
                self.surfaces.push(Surface::default());
                self.surfaces[surface].cmds.push(Cmd {
                    pipe: Pipe::Composite,
                    range: child as u32..child as u32 + 1,
                    uniform,
                    blend: layer.blend,
                    scissor: parent.scissor,
                });
                (child, 1.0, BlendMode::Normal)
            }
        };
        if opacity <= 0.0 {
            return;
        }
        let state = LayerState { to_px, ppu, opacity, clip, scissor };
        let uniform = self.push_uniform(f, &state, opacity);
        for item in f.list.layer_items(li) {
            let range = item.start as usize..(item.start + item.len) as usize;
            match item.kind {
                ItemKind::Shapes => {
                    let start = self.shapes.len() as u32;
                    for s in &f.list.shapes[range] {
                        self.shapes.extend(GpuShape::new(s, &to_px, ppu, opacity));
                    }
                    self.push_cmd(draw_surface, Pipe::Shape, start..self.shapes.len() as u32, uniform, blend, scissor);
                }
                ItemKind::Glyphs => {
                    let start = self.glyphs.len() as u32;
                    for g in &f.list.glyphs[range] {
                        if let Some(inst) = self.glyph(g, &state) {
                            self.glyphs.push(inst);
                        }
                    }
                    self.push_cmd(draw_surface, Pipe::Glyph, start..self.glyphs.len() as u32, uniform, blend, scissor);
                }
                ItemKind::Emitters => {}
                ItemKind::Layer => self.visit(f, item.start, &state, draw_surface),
            }
        }
    }

    /// Adds a draw, or extends the previous one when it continues the same run.
    fn push_cmd(
        &mut self,
        surface: usize,
        pipe: Pipe,
        range: Range<u32>,
        uniform: u32,
        blend: BlendMode,
        scissor: [f32; 4],
    ) {
        if range.is_empty() {
            return;
        }
        let cmds = &mut self.surfaces[surface].cmds;
        if let Some(last) = cmds.last_mut() {
            if last.pipe == pipe && last.uniform == uniform && last.blend == blend && last.range.end == range.start {
                last.range.end = range.end;
                return;
            }
        }
        cmds.push(Cmd { pipe, range, uniform, blend, scissor });
    }

    fn glyph(&mut self, g: &DlGlyph, st: &LayerState) -> Option<GlyphInstance> {
        let color = linear_premul(g.color, st.opacity);
        if color[3] <= 0.0 {
            return None;
        }
        if g.font as usize >= self.faces.len() {
            let more = self.fonts.faces_from(self.faces.len());
            self.faces.extend(more);
        }
        let face = self.faces.get(g.font as usize)?;
        let entry = self.atlas.glyph_in(g.font, || face.skrifa(), GlyphId::new(g.glyph))?;
        let k = g.size / face.units_per_em() as f32;
        let m = &st.to_px;
        // Em space has y up; layer space has y down.
        let lin = [m.a * k, m.b * k, -m.c * k, -m.d * k];
        let o = m.apply(g.pos);
        Some(GlyphInstance::with_matrix(&entry, lin, [o.x, o.y], color, g.word))
    }

    /// Appends a layer block and returns its byte offset.
    fn push_uniform(&mut self, f: &Frame, st: &LayerState, opacity: f32) -> u32 {
        let m = &st.to_px;
        let mut u = LayerUniform {
            m: [m.a, m.b, m.c, m.d],
            t: [m.e, m.f, st.ppu, opacity],
            viewport: f.viewport,
            ..Default::default()
        };
        if let Some(c) = &st.clip {
            let (r, k) = (c.rect.rect, &c.from_px);
            u.clip_rect = [r.x, r.y, r.w, r.h];
            u.clip_radii = c.rect.radii;
            u.clip_m = [k.a, k.b, k.c, k.d];
            u.clip_t = [k.e, k.f, c.ppu, 1.0];
        }
        let offset = self.uniforms.len() as u32;
        self.uniforms.extend_from_slice(bytemuck::bytes_of(&u));
        self.uniforms.resize((offset + self.uniform_stride) as usize, 0);
        offset
    }

    fn upload(&mut self, format: wgpu::TextureFormat, w: u32, h: u32) {
        let (device, queue) = (&self.device, &self.queue);
        self.atlas_gpu.upload(device, queue, &self.atlas);
        self.atlas_gpu.set_uniforms(queue, &Uniforms::pixels(w as f32, h as f32));
        write_grow(
            device,
            queue,
            &mut self.shape_buf,
            "vo-shapes",
            wgpu::BufferUsages::VERTEX,
            bytemuck::cast_slice(&self.shapes),
        );
        write_grow(
            device,
            queue,
            &mut self.glyph_buf,
            "vo-glyphs",
            wgpu::BufferUsages::VERTEX,
            bytemuck::cast_slice(&self.glyphs),
        );
        let need = self.uniforms.len().max(self.uniform_stride as usize) as u64;
        if self.uniform_buf.as_ref().is_none_or(|(b, _)| b.size() < need) {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vo-layers"),
                size: need.next_power_of_two(),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vo-layers"),
                layout: &self.layer_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &buf,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<LayerUniform>() as u64),
                    }),
                }],
            });
            self.uniform_buf = Some((buf, bind));
        }
        if !self.uniforms.is_empty() {
            queue.write_buffer(&self.uniform_buf.as_ref().unwrap().0, 0, &self.uniforms);
        }
        // One texture per offscreen layer, the size and format of the target.
        let layers = self.surfaces.len() - 1;
        for i in 0..layers {
            let fits = self
                .layer_texs
                .get(i)
                .is_some_and(|t| t.texture.width() == w && t.texture.height() == h && t.texture.format() == format);
            if !fits {
                let tex = self.layer_texture(w, h, format);
                if i < self.layer_texs.len() {
                    self.layer_texs[i] = tex;
                } else {
                    self.layer_texs.push(tex);
                }
            }
        }
        // Pipelines this frame needs.
        let mut needed: Vec<(Pipe, BlendMode)> = Vec::new();
        for s in &self.surfaces {
            for c in &s.cmds {
                if !needed.contains(&(c.pipe, c.blend)) {
                    needed.push((c.pipe, c.blend));
                }
            }
        }
        for (pipe, blend) in needed {
            self.pipeline(format, pipe, blend);
        }
    }

    fn layer_texture(&self, w: u32, h: u32, format: wgpu::TextureFormat) -> LayerTex {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vo-layer"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-layer"),
            layout: &self.composite_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }],
        });
        LayerTex { texture, view, bind }
    }

    fn encode_passes(&self, target: &Target, enc: &mut wgpu::CommandEncoder) {
        // Surfaces are numbered in pre-order, so a child always has a higher number than its
        // parent: going from the last to the first draws every layer before it is composited.
        for i in (0..self.surfaces.len()).rev() {
            let view = if i == 0 { &target.view } else { &self.layer_texs[i - 1].view };
            let att = [Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })];
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(if i == 0 { "vo-target" } else { "vo-layer" }),
                color_attachments: &att,
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.run(&mut pass, &self.surfaces[i], target);
        }
    }

    fn run(&self, pass: &mut wgpu::RenderPass<'_>, surface: &Surface, target: &Target) {
        let (w, h) = (target.width(), target.height());
        let format = target.format();
        let layers = &self.uniform_buf.as_ref().expect("uploaded").1;
        for c in &surface.cmds {
            let x0 = (c.scissor[0].floor().max(0.0) as u32).min(w);
            let y0 = (c.scissor[1].floor().max(0.0) as u32).min(h);
            let x1 = (c.scissor[2].ceil().max(0.0) as u32).min(w);
            let y1 = (c.scissor[3].ceil().max(0.0) as u32).min(h);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
            pass.set_pipeline(self.cached_pipeline(format, c.pipe, c.blend));
            pass.set_bind_group(1, layers, &[c.uniform]);
            match c.pipe {
                Pipe::Shape => {
                    pass.set_vertex_buffer(0, self.shape_buf.as_ref().unwrap().slice(..));
                    pass.draw(0..4, c.range.clone());
                }
                Pipe::Glyph => {
                    let Some(atlas) = self.atlas_gpu.bind_group() else { continue };
                    pass.set_bind_group(0, atlas, &[]);
                    pass.set_vertex_buffer(0, self.glyph_buf.as_ref().unwrap().slice(..));
                    pass.draw(0..4, c.range.clone());
                }
                Pipe::Composite => {
                    pass.set_bind_group(0, &self.layer_texs[c.range.start as usize - 1].bind, &[]);
                    pass.draw(0..3, 0..1);
                }
            }
        }
    }

    fn cached_pipeline(&self, format: wgpu::TextureFormat, pipe: Pipe, blend: BlendMode) -> &wgpu::RenderPipeline {
        let key = (format, pipe, blend);
        &self.pipelines.iter().find(|(k, _)| *k == key).expect("created in upload").1
    }

    /// Creates the pipeline for `format`, `pipe` and `blend` on first use.
    fn pipeline(&mut self, format: wgpu::TextureFormat, pipe: Pipe, blend: BlendMode) {
        let key = (format, pipe, blend);
        if self.pipelines.iter().any(|(k, _)| *k == key) {
            return;
        }
        let slot = pipe as usize;
        if self.modules[slot].is_none() {
            let (label, source) = match pipe {
                Pipe::Shape => ("vo-shapes", shape_shader_source()),
                Pipe::Glyph => ("vo-glyphs", glyph_shader_source()),
                Pipe::Composite => ("vo-composite", composite_shader_source()),
            };
            self.modules[slot] = Some(self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            }));
        }
        let module = self.modules[slot].as_ref().unwrap();
        let group0 = match pipe {
            Pipe::Shape => None,
            Pipe::Glyph => Some(self.atlas_gpu.layout()),
            Pipe::Composite => Some(&self.composite_layout),
        };
        let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[group0, Some(&self.layer_layout)],
            immediate_size: 0,
        });
        let (vs, fs, buffers) = match pipe {
            Pipe::Shape => ("vs_shape", "fs_shape", vec![Some(GpuShape::layout())]),
            Pipe::Glyph => ("vs_glyph", "fs_dl_glyph", vec![Some(GlyphInstance::layout())]),
            Pipe::Composite => ("vs_composite", "fs_composite", vec![]),
        };
        let pipeline = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vo"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some(vs),
                compilation_options: Default::default(),
                buffers: &buffers,
            },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some(fs),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(blend_state(blend)),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: match pipe {
                    Pipe::Composite => wgpu::PrimitiveTopology::TriangleList,
                    _ => wgpu::PrimitiveTopology::TriangleStrip,
                },
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.push((key, pipeline));
    }
}

/// What every layer of one frame shares.
struct Frame<'a> {
    list: &'a DisplayList,
    scale: f32,
    viewport: [f32; 4],
}

/// Fixed-function blend states for premultiplied colours. See the module notes on `Multiply`.
fn blend_state(mode: BlendMode) -> wgpu::BlendState {
    use wgpu::BlendFactor::*;
    let comp =
        |src_factor, dst_factor| wgpu::BlendComponent { src_factor, dst_factor, operation: wgpu::BlendOperation::Add };
    let alpha = comp(One, OneMinusSrcAlpha);
    match mode {
        BlendMode::Normal => PREMULTIPLIED_OVER,
        BlendMode::Add => wgpu::BlendState { color: comp(One, One), alpha },
        BlendMode::Multiply => wgpu::BlendState { color: comp(Dst, OneMinusSrcAlpha), alpha },
        BlendMode::Screen => wgpu::BlendState { color: comp(One, OneMinusSrc), alpha },
    }
}

/// Bounding box in physical pixels of `r` mapped through `m`.
fn bounds(r: &RRect, m: &Affine) -> [f32; 4] {
    let r = r.rect;
    let pts = [(r.x, r.y), (r.right(), r.y), (r.right(), r.bottom()), (r.x, r.bottom())]
        .map(|(x, y)| m.apply(vo_scene::geom::Point::new(x, y)));
    let mut b = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
    for p in pts {
        b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
    }
    b
}

fn intersect(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

/// Writes `data` into `buf`, replacing the buffer with a larger one when it does not fit.
fn write_grow(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buf: &mut Option<wgpu::Buffer>,
    label: &str,
    usage: wgpu::BufferUsages,
    data: &[u8],
) {
    let size = (data.len().max(256) as u64).next_power_of_two();
    if buf.as_ref().is_none_or(|b| b.size() < size) {
        *buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
    }
    if !data.is_empty() {
        queue.write_buffer(buf.as_ref().unwrap(), 0, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(src: &str) {
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }

    #[test]
    fn shaders_validate_in_naga() {
        validate(&shape_shader_source());
        validate(&glyph_shader_source());
        validate(&composite_shader_source());
    }

    #[test]
    fn layer_block_fits_one_slot() {
        assert!(std::mem::size_of::<LayerUniform>() <= 256);
    }
}
