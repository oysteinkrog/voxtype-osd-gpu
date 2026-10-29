//! [`Stack`]: one frame in the fixed depth order of D304 (CAPABILITIES section 3, item 8).
//!
//! 1. **Text field** (TY19), from the text list, so the scene below can react to the words.
//! 2. **3D pass**: background, meshes with depth test and write, point clouds and bokeh with
//!    depth test and no depth write ([`Renderer3d`]).
//! 3. **2D vector**: a display list drawn by the normal renderer over what is there
//!    (`Renderer::encode_with` with `LoadOp::Load`).
//! 4. **Text**: a display list, drawn the same way; then text planes, Slug glyphs placed in the
//!    3D scene by a model matrix and drawn with depth off, sharp under perspective (D301).
//! 5. **Post**: a caller hook on the finished target (bloom and the like live in the effect
//!    graph).
//!
//! The order is fixed: the whole 3D scene is under the text. Splitting 3D around the text
//! plane (EG-20, [`crate::Part`]) is for 3D elements in a vo scene (`Element::gpu_split`);
//! the stack always draws its scene with `Part::Whole`.
//!
//! Each stage is its own command buffer, because the 2D renderer reuses its buffers for
//! every list it draws. With a [`GpuTimer`] each stage is timed.

use std::time::Duration;

use skrifa::GlyphId;
use vo_render::color::linear_premul;
use vo_render::text::slug_gpu::{Atlas, GlyphInstance, SlugRenderer, Uniforms, NO_WORD};
use vo_render::{Renderer, Target};
use vo_scene::display::{DisplayList, ItemKind};

use crate::field::TextField;
use crate::frame::Frame3d;
use crate::math::{Mat4, Vec3};
use crate::render::Renderer3d;
use crate::timer::GpuTimer;

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
    slug: Option<(wgpu::TextureFormat, SlugRenderer)>,
    atlas: Atlas,
    pub pipeline_time: Duration,
}

impl Stack {
    /// A stack whose 2D renderer reads font ids from `fonts`, the scene's table.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, fonts: vo_scene::font::FontRegistry) -> Stack {
        Stack {
            device: device.clone(),
            queue: queue.clone(),
            scene: Renderer3d::new(device, queue),
            flat: Renderer::with_fonts(device, queue, fonts),
            field: TextField::new(device, queue),
            timer: GpuTimer::new(device, queue),
            slug: None,
            atlas: Atlas::default(),
            pipeline_time: Duration::ZERO,
        }
    }

    /// Compiles every pipeline for `format` now. Returns the time it took.
    pub fn prepare(&mut self, format: wgpu::TextureFormat) -> Duration {
        let start = std::time::Instant::now();
        self.scene.prepare(format);
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
                let out = crate::render::Output { view: &view, size, format, scale };
                self.scene.encode_with(&mut enc, out, frame, binding, None);
            }
            None => clear(&mut enc, &view),
        }
        self.mark(&mut enc, "scene");
        self.queue.submit([enc.finish()]);

        // 3 and 4. 2D lists.
        for (list, label) in [(layers.vector, "vector"), (layers.text, "text")] {
            let Some(list) = list else { continue };
            let mut enc = self.device.create_command_encoder(&Default::default());
            self.flat.encode_with(list, scale, target, wgpu::LoadOp::Load, &mut enc);
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
                    let (lin, o) = vo_render::text::slug_gpu::place(m, g, k, entry.extents);
                    glyphs.push(GlyphInstance::with_matrix(&entry, lin, o, color, NO_WORD));
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
