//! 3D as an item inside a 2D scene: a [`Source3d`] gives the [`Frame3d`] for a time and a
//! size, and [`ItemPainter`] draws it at the item's place in a surface that already holds the
//! layers below it.
//!
//! The 3D pass clears and resolves a whole texture, so the painter draws into a scratch
//! texture the item's size and composites it over the surface with premultiplied over, times
//! the layer opacity. The item is placed by the bounds of its rect in surface pixels; a
//! rotated layer is not followed.
//!
//! **Two halves** (EG-20). An item can be drawn as one of two halves split at the text plane
//! ([`Part::Behind`], [`Part::Front`]): the plane through the camera's target that faces the
//! camera, which is z = 0 in stage space for a frame drawn [`crate::OnStage`]. The 3D pass
//! drops what lies on the other side ([`Renderer3d::part`]), so one [`Source3d`] drawn twice
//! covers its whole drawing once. A front half is composited everywhere but inside
//! [`ItemPlace::cut`], where the surface stays exactly as it was (the reading line).

use std::fmt::Debug;

use bytemuck::{Pod, Zeroable};
use vo_render::text::slug_gpu::PREMULTIPLIED_OVER;
use vo_scene::geom::Size;

use crate::camera::Camera;
use crate::frame::Frame3d;
use crate::mesh::{Mesh, Vertex};
use crate::render::Renderer3d;
use crate::MeshId;

pub use vo_scene::gpu::GpuPart as Part;

/// Most boxes a front half leaves alone; more are merged into the last one.
pub const MAX_CUTS: usize = 16;

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

    /// Meshes that bend over time (a [`crate::Ribbon3d`]): push `(i, vertices at t)` for
    /// each, `i` its index in [`Source3d::meshes`]. The count must stay what `meshes` gave.
    /// Pure in `t`, like `frame`. Most sources have none.
    fn vertices(&self, _t: f64, _out: &mut Vec<(u32, Vec<Vertex>)>) {}

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
    /// The region's top left in the scratch texture.
    src: [f32; 4],
    /// x0, y0, x1, y1 in surface pixels; the first `cut_count` are used.
    cuts: [[f32; 4]; MAX_CUTS],
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
    /// The front half's composite buffer, apart from the back half's (one submit).
    uniform_front: wgpu::Buffer,
    /// A front half already drawn beside its back half, waiting for its composite.
    front: Option<Front>,
}

/// A front half drawn in the back half's pass (EG-20): where it is in the scratch texture.
struct Front {
    key: usize,
    t: f64,
    /// `x, y, w, h` in surface pixels; `w` 0 when the half is empty.
    rect: [f32; 4],
    /// Its top left in the scratch texture.
    src: [f32; 2],
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
    /// The stage camera in surface pixels and the item's centre as seen on the surface, for
    /// frames drawn [`crate::OnStage`].
    pub stage: Option<StageAt>,
    /// Which half to draw (EG-20), `Part::Whole` for all of it.
    pub part: Part,
    /// Boxes `x0, y0, x1, y1` in surface pixels where the item leaves the surface exactly as
    /// it was: a pixel whose centre is inside one is not touched.
    pub cut: Vec<[f32; 4]>,
}

/// Where the stage camera is, for an item: `origin` and `centre` in surface pixels, and the
/// eye `perspective` pixels in front of the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StageAt {
    pub origin: [f32; 2],
    pub perspective: f32,
    pub centre: [f32; 2],
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
            uniform_front: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vo3d-item-front"),
                size: std::mem::size_of::<ItemUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            front: None,
        }
    }

    /// Records `source` at `t` into `enc`, composited over `view` (which has `format`) at
    /// `at`. Writes queue buffers now, so at most one item per painter per submit, except for
    /// the two halves of one split element (EG-20): a back half on the stage camera with
    /// meshes alone draws its front half in the same pass, and the front half's own call then
    /// only composites it ([`ItemPainter::has_front`]).
    pub fn paint(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        source: &dyn Source3d,
        t: f64,
        at: &ItemPlace,
    ) {
        let key = source_key(source);
        if at.part == Part::Front {
            if let Some(f) = self.front.take().filter(|f| f.key == key && f.t == t) {
                if f.rect[2] > 0.0 {
                    self.composite(enc, view, format, f.rect, f.src, at, true);
                }
                return;
            }
        }
        self.front = None;
        let [_, _, w, h] = at.rect;
        if at.opacity <= 0.0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let scale = if at.size.w > 0.0 { w / at.size.w } else { 1.0 };
        let base = self.mesh_base(source);
        let mut moving = Vec::new();
        source.vertices(t, &mut moving);
        for (i, v) in &moving {
            self.scene.update_mesh(MeshId(base + i), v);
        }
        self.scene.part = at.part;
        let mut frame = source.frame(t, at.size);
        for d in &mut frame.meshes {
            d.mesh = MeshId(base + d.mesh.0);
        }
        // On the stage the drawing covers the item's box grown by the overscan, about where
        // its centre lands, cut to the scissor.
        let [x, y, w, h] = match (frame.on_stage, at.stage) {
            (Some(on), Some(st)) => {
                let (gw, gh) = (w * (1.0 + 2.0 * on.overscan.max(0.0)), h * (1.0 + 2.0 * on.overscan.max(0.0)));
                let mut r = [
                    st.centre[0] - 0.5 * gw,
                    st.centre[1] - 0.5 * gh,
                    st.centre[0] + 0.5 * gw,
                    st.centre[1] + 0.5 * gh,
                ];
                if let Some([sx, sy, sw, sh]) = at.scissor {
                    let s = [sx as f32, sy as f32, (sx + sw) as f32, (sy + sh) as f32];
                    r = [r[0].max(s[0]), r[1].max(s[1]), r[2].min(s[2]), r[3].min(s[3])];
                }
                r = [r[0].floor(), r[1].floor(), r[2].ceil(), r[3].ceil()];
                if r[2] <= r[0] || r[3] <= r[1] {
                    return;
                }
                let rect = [r[0], r[1], r[2] - r[0], r[3] - r[1]];
                let camera = |rect: [f32; 4]| Camera::stage(st.perspective / scale, st.origin, st.centre, scale, rect);
                frame.camera = camera(rect);
                // Draw only the box the meshes (or this half of them) cover: the same
                // projection on a smaller window, so the pixels inside are the same.
                let tight = |scene: &mut Renderer3d, frame: &Frame3d, part: Part| {
                    scene.part = part;
                    let b = scene.draw_bounds_px(frame, [rect[2], rect[3]])?;
                    let t = [
                        r[0].max((r[0] + b[0]).floor()),
                        r[1].max((r[1] + b[1]).floor()),
                        r[2].min((r[0] + b[2]).ceil()),
                        r[3].min((r[1] + b[3]).ceil()),
                    ];
                    Some(if t[2] <= t[0] || t[3] <= t[1] {
                        [t[0], t[1], 0.0, 0.0]
                    } else {
                        [t[0], t[1], t[2] - t[0], t[3] - t[1]]
                    })
                };
                if at.part == Part::Behind {
                    let back = tight(&mut self.scene, &frame, Part::Behind);
                    let front = tight(&mut self.scene, &frame, Part::Front);
                    self.scene.part = Part::Behind;
                    if let (Some(back), Some(front)) = (back, front) {
                        self.paint_halves(enc, view, format, &frame, back, front, &camera, scale, at, key, t);
                        return;
                    }
                }
                match tight(&mut self.scene, &frame, at.part) {
                    Some(t) if t[2] == 0.0 => return,
                    Some(t) => {
                        frame.camera = camera(t);
                        t
                    }
                    None => rect,
                }
            }
            _ => at.rect,
        };
        let (x0, y0) = (x.floor(), y.floor());
        let size = [(x + w).ceil() - x0, (y + h).ceil() - y0].map(|v| v.max(1.0) as u32);
        self.ensure_scratch(size, format);
        let scratch = self.scratch.as_ref().unwrap();
        let out = crate::render::Output { view: &scratch.view, size, format, scale };
        self.scene.encode_with(enc, out, &frame, None, None);
        self.composite(enc, view, format, [x0, y0, size[0] as f32, size[1] as f32], [0.0; 2], at, false);
    }

    /// Whether a front half of `source` at `t` is drawn and waits for its composite.
    pub fn has_front(&self, source: &dyn Source3d, t: f64) -> bool {
        self.front.as_ref().is_some_and(|f| f.key == source_key(source) && f.t == t)
    }

    /// Both halves in one pass, side by side in the scratch texture: composites the back
    /// half now and keeps the front half for its own call. `back` and `front` are the halves'
    /// boxes in surface pixels, whole pixels, `w` 0 when empty.
    #[allow(clippy::too_many_arguments)]
    fn paint_halves(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        frame: &Frame3d,
        back: [f32; 4],
        front: [f32; 4],
        camera: &dyn Fn([f32; 4]) -> Camera,
        scale: f32,
        at: &ItemPlace,
        key: usize,
        t: f64,
    ) {
        let (wb, wf) = (back[2] as u32, front[2] as u32);
        let (hb, hf) = (back[3] as u32, front[3] as u32);
        if wb + wf > 0 {
            let size = [wb + wf, hb.max(hf)];
            self.ensure_scratch(size, format);
            let scratch = self.scratch.as_ref().unwrap();
            let out = crate::render::Output { view: &scratch.view, size, format, scale };
            if wb > 0 && wf > 0 {
                let halves = [
                    crate::render::Half { camera: camera(back), part: Part::Behind, viewport: [0, 0, wb, hb] },
                    crate::render::Half { camera: camera(front), part: Part::Front, viewport: [wb, 0, wf, hf] },
                ];
                self.scene.encode_halves(enc, out, frame, halves);
            } else {
                // One half is empty: an ordinary pass for the other.
                let (r, part) = if wb > 0 { (back, Part::Behind) } else { (front, Part::Front) };
                let mut f = frame.clone();
                f.camera = camera(r);
                self.scene.part = part;
                self.scene.encode_with(enc, out, &f, None, None);
                self.scene.part = Part::Behind;
            }
        }
        if wb > 0 {
            self.composite(enc, view, format, back, [0.0; 2], at, false);
        }
        let src = if wb > 0 { [wb as f32, 0.0] } else { [0.0; 2] };
        self.front = Some(Front { key, t, rect: if wf > 0 { front } else { [0.0; 4] }, src });
    }

    /// Composites `rect` (surface pixels) of the scratch texture, from `src` in it, over
    /// `view`, leaving the cut boxes alone.
    #[allow(clippy::too_many_arguments)]
    fn composite(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        rect: [f32; 4],
        src: [f32; 2],
        at: &ItemPlace,
        front: bool,
    ) {
        let mut cuts = [[0.0f32; 4]; MAX_CUTS];
        for (i, c) in at.cut.iter().enumerate() {
            let k = i.min(MAX_CUTS - 1);
            cuts[k] = if i < MAX_CUTS {
                *c
            } else {
                let m = cuts[k];
                [m[0].min(c[0]), m[1].min(c[1]), m[2].max(c[2]), m[3].max(c[3])]
            };
        }
        let u = ItemUniform {
            rect,
            viewport: [at.surface[0] as f32, at.surface[1] as f32, at.opacity, at.cut.len().min(MAX_CUTS) as f32],
            src: [src[0], src[1], 0.0, 0.0],
            cuts,
        };
        let uniform = if front { &self.uniform_front } else { &self.uniform };
        self.queue.write_buffer(uniform, 0, bytemuck::bytes_of(&u));
        let scratch = self.scratch.as_ref().unwrap();
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo3d-item"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&scratch.view) },
                wgpu::BindGroupEntry { binding: 1, resource: uniform.as_entire_binding() },
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
        let key = source_key(source);
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

/// A source's identity: its address.
fn source_key(source: &dyn Source3d) -> usize {
    source as *const dyn Source3d as *const () as usize
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
