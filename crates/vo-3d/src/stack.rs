//! [`Stack`]: one frame in the fixed depth order of D304 (CAPABILITIES section 3, item 8).
//!
//! 1. **Text field** (TY19), from the text list, so the scene below can react to the words.
//! 2. **3D pass**: background, meshes with depth test and write, point clouds and bokeh with
//!    depth test and no depth write ([`Renderer3d`]).
//! 3. **2D vector**: a display list drawn by the normal renderer, composited over.
//! 4. **Text**: a display list, composited over; then text planes, Slug glyphs placed in the
//!    3D scene by a model matrix and drawn with depth off, sharp under perspective (D301).
//! 5. **Post**: a caller hook on the finished target (bloom and the like live in the effect
//!    graph).
//!
//! Each stage is its own command buffer, because the 2D renderer reuses its buffers for
//! every list it draws. With a [`GpuTimer`] each stage is timed.

use std::time::Duration;

use skrifa::GlyphId;
use vo_render::color::linear_premul;
use vo_render::text::slug_gpu::{Atlas, GlyphInstance, SlugRenderer, Uniforms, NO_WORD, PREMULTIPLIED_OVER};
use vo_render::{Renderer, Target};
use vo_scene::display::{DisplayList, ItemKind};

use crate::field::TextField;
use crate::frame::Frame3d;
use crate::math::{Mat4, Vec3};
use crate::render::Renderer3d;
use crate::timer::GpuTimer;

const STACK_WGSL: &str = include_str!("wgsl/stack.wgsl");

/// Flat Slug text placed in the 3D scene.
pub struct TextPlane<'a> {
    /// Glyphs in the list's own logical units, y down. Shapes are ignored.
    pub list: &'a DisplayList,
    /// Plane to world. The plane's origin is the list's origin.
    pub model: Mat4,
    /// World units per list unit.
    pub units_per_px: f32,
    /// Multiplied into every glyph's alpha.
    pub opacity: f32,
}

/// What one frame draws, bottom to top.
#[derive(Default)]
pub struct Layers<'a> {
    pub scene: Option<&'a Frame3d>,
    pub vector: Option<&'a DisplayList>,
    pub text: Option<&'a DisplayList>,
    /// Build the text field from `text` and bind it for the scene's point clouds.
    pub field_from_text: bool,
    /// Drawn after `text`, with the scene's camera. Ignored without a scene.
    pub planes: &'a [TextPlane<'a>],
}

pub type PostFn<'a> = &'a mut dyn FnMut(&wgpu::Device, &mut wgpu::CommandEncoder, &Target);

pub struct Stack {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pub scene: Renderer3d,
    pub flat: Renderer,
    pub field: TextField,
    pub timer: Option<GpuTimer>,
    layer: Option<Target>,
    over_layout: wgpu::BindGroupLayout,
    over: Option<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    slug: Option<(wgpu::TextureFormat, SlugRenderer)>,
    atlas: Atlas,
    pub pipeline_time: Duration,
}

impl Stack {
    /// A stack whose 2D renderer reads font ids from `fonts`, the scene's table.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, fonts: vo_scene::font::FontRegistry) -> Stack {
        let over_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo3d-over"),
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
        Stack {
            device: device.clone(),
            queue: queue.clone(),
            scene: Renderer3d::new(device, queue),
            flat: Renderer::with_fonts(device, queue, fonts),
            field: TextField::new(device, queue),
            timer: GpuTimer::new(device, queue),
            layer: None,
            over_layout,
            over: None,
            slug: None,
            atlas: Atlas::default(),
            pipeline_time: Duration::ZERO,
        }
    }

    /// Compiles every pipeline for `format` now. Returns the time it took.
    pub fn prepare(&mut self, format: wgpu::TextureFormat) -> Duration {
        let start = std::time::Instant::now();
        self.scene.prepare(format);
        self.over_pipeline(format);
        self.slug_renderer(format);
        let spent = start.elapsed();
        self.pipeline_time += spent;
        spent
    }

    /// Draws `layers` into `target` at `scale` physical pixels per logical unit, then runs
    /// `post`. With a timer, the stage times are read with [`Stack::stage_times`].
    pub fn render(&mut self, layers: &Layers<'_>, target: &Target, scale: f32, post: Option<PostFn<'_>>) {
        let size = [target.width(), target.height()];
        let format = target.format();
        let view = target.texture().create_view(&Default::default());
        if let Some(t) = &mut self.timer {
            t.begin();
        }
        let mut enc = self.encoder("start");

        // 1. Text field.
        let field = match (layers.field_from_text, layers.text, layers.scene.is_some()) {
            (true, Some(text), true) => {
                self.field.update(&mut self.flat, text, size, scale, &mut enc);
                self.mark(&mut enc, "field");
                true
            }
            _ => false,
        };
        self.queue.submit([enc.finish()]);

        // 2. 3D, or a clear when there is no scene.
        let mut enc = self.device.create_command_encoder(&Default::default());
        match layers.scene {
            Some(frame) => {
                let binding = if field { self.field.binding() } else { None };
                self.scene.encode(&mut enc, &view, size, format, frame, binding);
            }
            None => clear(&mut enc, &view),
        }
        self.mark(&mut enc, "scene");
        self.queue.submit([enc.finish()]);

        // 3 and 4. 2D lists.
        for (list, label) in [(layers.vector, "vector"), (layers.text, "text")] {
            let Some(list) = list else { continue };
            let mut enc = self.device.create_command_encoder(&Default::default());
            self.flat_over(list, scale, target, &view, &mut enc);
            self.mark(&mut enc, label);
            self.queue.submit([enc.finish()]);
        }
        if let Some(frame) = layers.scene {
            for plane in layers.planes {
                let mut enc = self.device.create_command_encoder(&Default::default());
                self.plane(plane, frame, size, format, &view, &mut enc);
                self.mark(&mut enc, "plane");
                self.queue.submit([enc.finish()]);
            }
        }

        // 5. Post.
        if let Some(post) = post {
            let mut enc = self.device.create_command_encoder(&Default::default());
            post(&self.device, &mut enc, target);
            self.mark(&mut enc, "post");
            self.queue.submit([enc.finish()]);
        }
    }

    /// `(stage, milliseconds)` of the last [`Stack::render`], waiting for the GPU. Empty
    /// without a timer.
    pub fn stage_times(&mut self) -> Vec<(&'static str, f64)> {
        match &mut self.timer {
            Some(t) => t.read(&self.device, &self.queue),
            None => Vec::new(),
        }
    }

    fn encoder(&mut self, first_mark: &'static str) -> wgpu::CommandEncoder {
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.mark(&mut enc, first_mark);
        enc
    }

    fn mark(&mut self, enc: &mut wgpu::CommandEncoder, label: &'static str) {
        if let Some(t) = &mut self.timer {
            t.mark(enc, label);
        }
    }

    /// Draws `list` into the scratch layer and composites it over the target.
    fn flat_over(
        &mut self,
        list: &DisplayList,
        scale: f32,
        target: &Target,
        view: &wgpu::TextureView,
        enc: &mut wgpu::CommandEncoder,
    ) {
        let (w, h, format) = (target.width(), target.height(), target.format());
        if self.layer.as_ref().is_none_or(|l| l.width() != w || l.height() != h || l.format() != format) {
            self.layer = Some(Target::new(&self.device, w, h, format));
        }
        let layer = self.layer.as_ref().unwrap();
        self.flat.encode(list, scale, layer, enc);
        let layer_view = layer.texture().create_view(&Default::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo3d-over"),
            layout: &self.over_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&layer_view) }],
        });
        let pipeline = self.over_pipeline(format);
        let mut pass = load_pass(enc, view, "vo3d-over");
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    fn over_pipeline(&mut self, format: wgpu::TextureFormat) -> &wgpu::RenderPipeline {
        if self.over.as_ref().is_none_or(|(f, _)| *f != format) {
            let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("vo3d-over"),
                source: wgpu::ShaderSource::Wgsl(STACK_WGSL.into()),
            });
            let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vo3d-over"),
                bind_group_layouts: &[Some(&self.over_layout)],
                immediate_size: 0,
            });
            let pipeline = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("vo3d-over"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_full"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_over"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(PREMULTIPLIED_OVER),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            });
            self.over = Some((format, pipeline));
        }
        &self.over.as_ref().unwrap().1
    }

    fn slug_renderer(&mut self, format: wgpu::TextureFormat) -> &mut SlugRenderer {
        if self.slug.as_ref().is_none_or(|(f, _)| *f != format) {
            self.slug = Some((format, SlugRenderer::new(&self.device, format)));
        }
        &mut self.slug.as_mut().unwrap().1
    }

    /// Draws a text plane's glyphs with the scene camera, depth off.
    fn plane(
        &mut self,
        plane: &TextPlane<'_>,
        frame: &Frame3d,
        size: [u32; 2],
        format: wgpu::TextureFormat,
        view: &wgpu::TextureView,
        enc: &mut wgpu::CommandEncoder,
    ) {
        let list = plane.list;
        let mut glyphs = Vec::new();
        for (li, layer) in list.layers.iter().enumerate() {
            for item in list.layer_items(li as u32) {
                if item.kind != ItemKind::Glyphs {
                    continue;
                }
                let m = &layer.transform;
                for g in &list.glyphs[item.start as usize..(item.start + item.len) as usize] {
                    let color = linear_premul(g.color, layer.opacity * plane.opacity);
                    let Some(face) = self.flat.fonts().get(g.font) else { continue };
                    let Some(entry) = self.atlas.glyph_in(g.font, || face.skrifa(), GlyphId::new(g.glyph)) else {
                        continue;
                    };
                    let k = g.size / face.units_per_em() as f32;
                    // Em space has y up; the list has y down.
                    let lin = [m.a * k, m.b * k, -m.c * k, -m.d * k];
                    let o = m.apply(g.pos);
                    glyphs.push(GlyphInstance::with_matrix(&entry, lin, [o.x, o.y], color, NO_WORD));
                }
            }
        }
        let [w, h] = size.map(|v| v as f32);
        // List units (y down) to plane units (y up), to world, to clip.
        let s = plane.units_per_px;
        let mvp = Mat4::scale(Vec3::new(s, -s, s)).then(&plane.model).then(&frame.camera.view_proj(w / h));
        let (device, queue) = (self.device.clone(), self.queue.clone());
        let atlas = std::mem::take(&mut self.atlas);
        let slug = self.slug_renderer(format);
        slug.upload_atlas(&device, &queue, &atlas);
        slug.set_uniforms(&queue, &Uniforms { mvp: mvp.cols, viewport: [w, h], gamma: 1.0, stem_darkening: 0.0 });
        slug.set_instances(&device, &queue, &glyphs);
        let mut pass = load_pass(enc, view, "vo3d-plane");
        self.slug.as_ref().unwrap().1.draw(&mut pass);
        drop(pass);
        self.atlas = atlas;
    }
}

fn load_pass<'e>(enc: &'e mut wgpu::CommandEncoder, view: &wgpu::TextureView, label: &str) -> wgpu::RenderPass<'e> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
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
    })
}

fn clear(enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("vo3d-clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}
