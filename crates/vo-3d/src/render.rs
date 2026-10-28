//! [`Renderer3d`]: draws a [`Frame3d`] into a colour texture in one 4x MSAA pass with its own
//! depth buffer, in the D304 order: background, meshes (depth test and write), point clouds,
//! light cones, then orbs (all three depth tested, no depth write, additive).
//!
//! With a floor (D305), a mirrored copy of the meshes, clouds and orbs comes first, right
//! after the background. Then the floor pass writes the floor plane's depth over it and adds
//! the pools of light, so the copy shows only below the floor line and the real scene draws
//! over it as usual.
//!
//! The pass resolves into the caller's view, so it is a layer like any other for whatever
//! comes after it (2D display lists, text, post effects).

use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use vo_render::color::linear_premul;
use vo_render::text::slug_gpu::PREMULTIPLIED_OVER;
use vo_scene::geom::Color;

use crate::frame::{Background, CloudShape, Frame3d, MeshId, MAX_POOLS};
use crate::math::{Mat4, Vec3};
use crate::mesh::{Mesh, Vertex};

const COMMON: &str = include_str!("wgsl/common3d.wgsl");
const MESH: &str = include_str!("wgsl/mesh.wgsl");
const POINTS: &str = include_str!("wgsl/points.wgsl");
const ORBS: &str = include_str!("wgsl/orbs.wgsl");
const BACKGROUND: &str = include_str!("wgsl/background.wgsl");
const FLOOR: &str = include_str!("wgsl/floor.wgsl");
const CONE: &str = include_str!("wgsl/cone.wgsl");

pub const SAMPLES: u32 = 4;
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Every per-draw block sits in one 256-byte slot of the draw buffer.
const SLOT: u64 = 256;

/// The shader sources, `common3d.wgsl` prepended, for naga checks and tools.
pub fn shader_sources() -> [(&'static str, String); 6] {
    [("mesh", MESH), ("points", POINTS), ("orbs", ORBS), ("background", BACKGROUND), ("floor", FLOOR), ("cone", CONE)]
        .map(|(n, s)| (n, [COMMON, s].join("\n")))
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    view: [[f32; 4]; 4],
    light_dir: [f32; 4],
    light_color: [f32; 4],
    ambient: [f32; 4],
    eye: [f32; 4],
    viewport: [f32; 4],
    focus: [f32; 4],
    fog: [f32; 4],
    bg_top: [f32; 4],
    bg_bottom: [f32; 4],
    field: [f32; 4],
    floor: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct DrawBlock {
    model: [[f32; 4]; 4],
    normal_m: [[f32; 4]; 4],
    color: [f32; 4],
    params: [f32; 4],
    /// Back-face colour, linear premultiplied.
    back: [f32; 4],
    /// Fibre lines across the width, their strength.
    fibre: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct CloudBlock {
    model: [[f32; 4]; 4],
    color: [f32; 4],
    color2: [f32; 4],
    shape: [f32; 4],
    motion: [f32; 4],
    react: [f32; 4],
    ids: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct FloorBlock {
    ray: [[f32; 4]; 6],
    pool_at: [[f32; 4]; MAX_POOLS],
    pool_color: [[f32; 4]; MAX_POOLS],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct ConeBlock {
    apex: [f32; 4],
    axis: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct OrbInstance {
    pos: [f32; 4],
    color: [f32; 4],
}

struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
    /// Vertex count.
    len: usize,
}

/// Textures that follow the target size.
struct Attachments {
    size: [u32; 2],
    format: wgpu::TextureFormat,
    color: wgpu::TextureView,
    depth: wgpu::TextureView,
}

struct Pipelines {
    format: wgpu::TextureFormat,
    background: wgpu::RenderPipeline,
    mesh: wgpu::RenderPipeline,
    points: wgpu::RenderPipeline,
    orbs: wgpu::RenderPipeline,
    floor: wgpu::RenderPipeline,
    cone: wgpu::RenderPipeline,
}

/// Where [`Renderer3d::encode_with`] resolves to.
#[derive(Clone, Copy)]
pub struct Output<'a> {
    pub view: &'a wgpu::TextureView,
    pub size: [u32; 2],
    pub format: wgpu::TextureFormat,
    /// Physical pixels per logical pixel. Pixel sizes in a frame are logical.
    pub scale: f32,
}

/// What a draw inside the 3D pass needs to match it.
#[derive(Clone, Copy, Debug)]
pub struct PassInfo {
    /// Column-major, right-handed, clip z 0 to 1.
    pub view_proj: crate::math::Mat4,
    /// Pixels per world unit at view depth 1.
    pub focal_px: f32,
    pub size: [u32; 2],
    pub format: wgpu::TextureFormat,
    /// Test against it; never write it (D304).
    pub depth: wgpu::TextureFormat,
    pub samples: u32,
}

/// A draw run inside the 3D pass, after the clouds and before the orbs.
pub type ExtraDraw<'a> = &'a mut dyn FnMut(&mut wgpu::RenderPass<'_>, &PassInfo);

/// The text field the clouds react to, or none.
pub struct FieldBinding<'a> {
    pub view: &'a wgpu::TextureView,
    /// The field texture's size in its own pixels.
    pub size: [u32; 2],
    /// Target pixels per field pixel.
    pub scale: f32,
}

pub struct Renderer3d {
    device: wgpu::Device,
    queue: wgpu::Queue,
    globals_layout: wgpu::BindGroupLayout,
    draw_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    modules: Option<[wgpu::ShaderModule; 6]>,
    pipelines: Option<Pipelines>,
    meshes: Vec<GpuMesh>,
    globals: wgpu::Buffer,
    sampler: wgpu::Sampler,
    no_field: wgpu::TextureView,
    draws: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    orbs: Option<wgpu::Buffer>,
    attachments: Option<Attachments>,
    staging: Vec<u8>,
    /// Wall time spent compiling shader modules and pipelines, for cold-start reports.
    pub pipeline_time: Duration,
}

impl Renderer3d {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Renderer3d {
        let uniform = |binding, visibility, dynamic: bool, size: u64| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: wgpu::BufferSize::new(size),
            },
            count: None,
        };
        let vf = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo3d-globals"),
            entries: &[
                uniform(0, vf, false, std::mem::size_of::<Globals>() as u64),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: vf,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: vf,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let draw_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo3d-draw"),
            entries: &[uniform(0, vf, true, SLOT)],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vo3d"),
            bind_group_layouts: &[Some(&globals_layout), Some(&draw_layout)],
            immediate_size: 0,
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo3d-globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vo3d-field"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let no_field = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("vo3d-no-field"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        Renderer3d {
            device: device.clone(),
            queue: queue.clone(),
            globals_layout,
            draw_layout,
            pipeline_layout,
            modules: None,
            pipelines: None,
            meshes: Vec::new(),
            globals,
            sampler,
            no_field,
            draws: None,
            orbs: None,
            attachments: None,
            staging: Vec::new(),
            pipeline_time: Duration::ZERO,
        }
    }

    /// Uploads a mesh once. Draws refer to it by the returned id.
    pub fn add_mesh(&mut self, mesh: &Mesh) -> MeshId {
        use wgpu::util::DeviceExt;
        let vertices = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vo3d-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        let indices = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vo3d-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        self.meshes.push(GpuMesh { vertices, indices, count: mesh.indices.len() as u32, len: mesh.vertices.len() });
        MeshId(self.meshes.len() as u32 - 1)
    }

    /// Rewrites the vertices of mesh `id` for the next submit (a ribbon that bends each
    /// frame). The count must match the mesh as added; the indices stay. Like the other
    /// buffers, the write lands at submit, so one update per mesh per submit.
    pub fn update_mesh(&mut self, id: MeshId, vertices: &[Vertex]) {
        let mesh = &self.meshes[id.0 as usize];
        assert_eq!(mesh.len, vertices.len(), "update_mesh: the vertex count must stay the same");
        self.queue.write_buffer(&mesh.vertices, 0, bytemuck::cast_slice(vertices));
    }

    /// Compiles the pipelines for `format` now instead of on the first frame.
    pub fn prepare(&mut self, format: wgpu::TextureFormat) {
        if self.pipelines.as_ref().is_some_and(|p| p.format == format) {
            return;
        }
        let start = Instant::now();
        let modules = self.modules.get_or_insert_with(|| {
            shader_sources().map(|(name, src)| {
                self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(name),
                    source: wgpu::ShaderSource::Wgsl(src.into()),
                })
            })
        });
        let add = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: PREMULTIPLIED_OVER.alpha,
        };
        let make = |module: &wgpu::ShaderModule,
                    vs: &str,
                    fs: &str,
                    buffers: &[Option<wgpu::VertexBufferLayout>],
                    topology,
                    blend,
                    depth_write: bool,
                    compare| {
            self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(vs),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers,
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState { topology, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(compare),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState { count: SAMPLES, ..Default::default() },
                multiview_mask: None,
                cache: None,
            })
        };
        use wgpu::{CompareFunction as Cmp, PrimitiveTopology as Topo};
        let orb_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<OrbInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4],
        };
        let [mesh, points, orbs, bg, floor, cone] = &*modules;
        let pipelines = Pipelines {
            format,
            background: make(bg, "vs_bg", "fs_bg", &[], Topo::TriangleList, PREMULTIPLIED_OVER, false, Cmp::Always),
            mesh: make(
                mesh,
                "vs_mesh",
                "fs_mesh",
                &[Some(Vertex::layout())],
                Topo::TriangleList,
                PREMULTIPLIED_OVER,
                true,
                Cmp::Less,
            ),
            points: make(points, "vs_point", "fs_point", &[], Topo::TriangleStrip, add, false, Cmp::LessEqual),
            orbs: make(orbs, "vs_orb", "fs_orb", &[Some(orb_layout)], Topo::TriangleStrip, add, false, Cmp::LessEqual),
            floor: make(floor, "vs_floor", "fs_floor", &[], Topo::TriangleList, add, true, Cmp::Always),
            cone: make(cone, "vs_cone", "fs_cone", &[], Topo::TriangleStrip, add, false, Cmp::LessEqual),
        };
        self.pipelines = Some(pipelines);
        self.pipeline_time += start.elapsed();
    }

    /// Records the 3D pass into `enc`, resolving into `target` (a single-sample view of
    /// `size` and `format`) at scale 1. With `field`, clouds with a reach react to the text
    /// field.
    pub fn encode(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: [u32; 2],
        format: wgpu::TextureFormat,
        frame: &Frame3d,
        field: Option<FieldBinding<'_>>,
    ) {
        self.encode_with(enc, Output { view: target, size, format, scale: 1.0 }, frame, field, None);
    }

    /// [`Renderer3d::encode`] with `extra` called inside the pass after the meshes and point
    /// clouds and before the orbs, so another system (the particle engine) can draw with this
    /// pass's depth test. It may change any pass state; the orbs set theirs again.
    pub fn encode_with(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        out: Output<'_>,
        frame: &Frame3d,
        field: Option<FieldBinding<'_>>,
        extra: Option<ExtraDraw<'_>>,
    ) {
        let Output { view: target, size, format, scale } = out;
        self.prepare(format);
        self.attachments(size, format);
        let [w, h] = size.map(|v| v as f32);
        let encode_srgb = !format.is_srgb() && !matches!(format, wgpu::TextureFormat::Rgba16Float);

        // Globals.
        let cam = &frame.camera;
        let l = &frame.light;
        let lin = |c: Color| linear_premul(c, 1.0);
        let (bg_top, bg_bottom) = match frame.background {
            Background::Transparent => ([0.0; 4], [0.0; 4]),
            Background::Solid(c) => (lin(c), lin(c)),
            Background::Vertical { top, bottom } => (lin(top), lin(bottom)),
        };
        let to_light = (-l.dir).normalize();
        let lc = lin(l.color);
        let eye = cam.eye();
        let fog = frame.fog.map_or([0.0, 0.0, 0.0, scale], |(a, b)| [a, b, 1.0, scale]);
        let globals = Globals {
            view_proj: cam.view_proj(w / h).cols,
            view: cam.view().cols,
            light_dir: [to_light.x, to_light.y, to_light.z, l.rim],
            light_color: [lc[0] * l.intensity, lc[1] * l.intensity, lc[2] * l.intensity, 1.0],
            ambient: lin(l.ambient),
            eye: [eye.x, eye.y, eye.z, 1.0],
            viewport: [w, h, cam.focal_px(h), encode_srgb as u32 as f32],
            focus: [frame.focus.distance, frame.focus.range, frame.focus.max_blur_px, field.is_some() as u32 as f32],
            fog,
            bg_top,
            bg_bottom,
            field: field.as_ref().map_or([1.0, 1.0, 1.0, 0.0], |f| [f.size[0] as f32, f.size[1] as f32, f.scale, 0.0]),
            floor: frame.floor.map_or([0.0; 4], |f| [f.y, f.fade, f.reflect, 1.0]),
        };
        self.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));
        let globals_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo3d-globals"),
            layout: &self.globals_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.globals.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(field.as_ref().map_or(&self.no_field, |f| f.view)),
                },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });

        // Per-draw blocks: meshes, clouds, then with a floor their mirrored copies and the
        // floor block, then cones.
        self.staging.clear();
        let mirror = frame.floor.map(|f| mirror_y(f.y));
        let passes = std::iter::once(None).chain(mirror.map(Some));
        for m4 in passes {
            for d in &frame.meshes {
                let m = &d.material;
                let model = m4.map_or(d.model, |r| d.model.then(&r));
                let block = DrawBlock {
                    model: model.cols,
                    normal_m: model.normal_matrix().cols,
                    color: lin(m.color),
                    params: [m.emissive, m.specular, m.shininess, m4.is_some() as u32 as f32],
                    back: linear_premul(m.back.map_or(m.color, |b| Color { a: m.color.a, ..b }), 1.0),
                    fibre: [m.fibres, m.fibre, 0.0, 0.0],
                };
                push_slot(&mut self.staging, bytemuck::bytes_of(&block));
            }
            for c in &frame.clouds {
                let straight = |col: Color| {
                    let p = lin(Color { a: 1.0, ..col });
                    [p[0], p[1], p[2], col.a]
                };
                let (kind, a, b) = match c.shape {
                    CloudShape::Torus { major, minor } => (0.0, major, minor),
                    CloudShape::Ball { radius } => (1.0, radius, 0.0),
                };
                let block = CloudBlock {
                    model: m4.map_or(c.model, |r| c.model.then(&r)).cols,
                    color: straight(c.color),
                    color2: straight(c.color2),
                    shape: [kind, a, b, c.size],
                    motion: [c.t, c.speed, c.level, c.level_amp],
                    react: [c.reach_px, c.push_px, c.glow, c.min_px],
                    ids: [c.seed, c.count, c.defocus as u32, m4.is_some() as u32],
                };
                push_slot(&mut self.staging, bytemuck::bytes_of(&block));
            }
        }
        if let Some(f) = &frame.floor {
            let ray = cam.ray_basis(w / h).map(|v| [v.x, v.y, v.z, 0.0]);
            let mut block = FloorBlock { ray, ..Default::default() };
            for (i, p) in f.pools.iter().enumerate() {
                if let Some(p) = p {
                    let c = lin(p.color);
                    block.pool_at[i] = [p.center.x, p.center.z, p.radius, 0.0];
                    block.pool_color[i] = [c[0] * p.intensity, c[1] * p.intensity, c[2] * p.intensity, 0.0];
                }
            }
            push_slot(&mut self.staging, bytemuck::bytes_of(&block));
        }
        for k in &frame.cones {
            let c = lin(k.color);
            let axis = k.dir.normalize();
            let block = ConeBlock {
                apex: [k.apex.x, k.apex.y, k.apex.z, k.length],
                axis: [axis.x, axis.y, axis.z, k.half_angle.tan()],
                color: [c[0] * k.intensity, c[1] * k.intensity, c[2] * k.intensity, 0.0],
            };
            push_slot(&mut self.staging, bytemuck::bytes_of(&block));
        }
        {
            // Always present: every pipeline's layout has group 1.
            let need = self.staging.len().max(1) as u64;
            if self.draws.as_ref().is_none_or(|(b, _)| b.size() < need) {
                let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("vo3d-draws"),
                    size: need.next_power_of_two().max(SLOT * 8),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("vo3d-draws"),
                    layout: &self.draw_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &buf,
                            offset: 0,
                            size: wgpu::BufferSize::new(SLOT),
                        }),
                    }],
                });
                self.draws = Some((buf, bind));
            }
            if !self.staging.is_empty() {
                self.queue.write_buffer(&self.draws.as_ref().unwrap().0, 0, &self.staging);
            }
        }

        // Orbs, then with a floor their mirrored copies, faded on the CPU by
        // `reflect_at` in common3d.wgsl.
        let orb = |o: &crate::frame::Orb, y: f32, k: f32| {
            let c = linear_premul(o.color, 1.0);
            let i = o.intensity * k;
            OrbInstance {
                pos: [o.pos.x, y, o.pos.z, o.radius],
                color: [c[0] * i, c[1] * i, c[2] * i, c[3] * i.min(1.0)],
            }
        };
        let mut orbs: Vec<OrbInstance> = frame.orbs.iter().map(|o| orb(o, o.pos.y, 1.0)).collect();
        let real_orbs = orbs.len() as u32;
        if let Some(f) = &frame.floor {
            for o in &frame.orbs {
                let y = 2.0 * f.y - o.pos.y;
                let below = f.y - y;
                let k = if below < 0.0 { 0.0 } else { f.reflect * (1.0 - smoothstep(0.0, f.fade.max(1e-4), below)) };
                if k > 0.0 {
                    orbs.push(orb(o, y, k));
                }
            }
        }
        if !orbs.is_empty() {
            let bytes: &[u8] = bytemuck::cast_slice(&orbs);
            if self.orbs.as_ref().is_none_or(|b| b.size() < bytes.len() as u64) {
                self.orbs = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("vo3d-orbs"),
                    size: (bytes.len() as u64).next_power_of_two(),
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
            self.queue.write_buffer(self.orbs.as_ref().unwrap(), 0, bytes);
        }

        let att = self.attachments.as_ref().unwrap();
        let p = self.pipelines.as_ref().unwrap();
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vo3d"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &att.color,
                depth_slice: None,
                resolve_target: Some(target),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Discard,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &att.depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let draws = &self.draws.as_ref().unwrap().1;
        pass.set_bind_group(0, &globals_bind, &[]);
        pass.set_bind_group(1, draws, &[0]);
        if !matches!(frame.background, Background::Transparent) {
            pass.set_pipeline(&p.background);
            pass.draw(0..3, 0..1);
        }
        let (n_mesh, n_cloud) = (frame.meshes.len() as u32, frame.clouds.len() as u32);
        let at = |slot: u32| [slot * SLOT as u32];
        let draw_scene = |pass: &mut wgpu::RenderPass<'_>, first: u32| {
            pass.set_pipeline(&p.mesh);
            for (i, d) in frame.meshes.iter().enumerate() {
                let mesh = &self.meshes[d.mesh.0 as usize];
                pass.set_bind_group(1, draws, &at(first + i as u32));
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.count, 0, 0..1);
            }
            pass.set_pipeline(&p.points);
            for (i, c) in frame.clouds.iter().enumerate() {
                pass.set_bind_group(1, draws, &at(first + n_mesh + i as u32));
                pass.draw(0..4, 0..c.count);
            }
        };
        let mut next = n_mesh + n_cloud;
        if frame.floor.is_some() {
            draw_scene(&mut pass, next);
            next += n_mesh + n_cloud;
            if let (true, Some(buf)) = (orbs.len() as u32 > real_orbs, &self.orbs) {
                pass.set_pipeline(&p.orbs);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..4, real_orbs..orbs.len() as u32);
            }
            pass.set_pipeline(&p.floor);
            pass.set_bind_group(1, draws, &at(next));
            pass.draw(0..3, 0..1);
            next += 1;
        }
        draw_scene(&mut pass, 0);
        if let Some(extra) = extra {
            let info = PassInfo {
                view_proj: cam.view_proj(w / h),
                focal_px: cam.focal_px(h),
                size,
                format,
                depth: DEPTH_FORMAT,
                samples: SAMPLES,
            };
            extra(&mut pass, &info);
            pass.set_bind_group(0, &globals_bind, &[]);
            pass.set_bind_group(1, draws, &[0]);
        }
        if !frame.cones.is_empty() {
            pass.set_pipeline(&p.cone);
            for i in 0..frame.cones.len() as u32 {
                pass.set_bind_group(1, draws, &at(next + i));
                pass.draw(0..2 * (CONE_SEGMENTS + 1), 0..1);
            }
            pass.set_bind_group(1, draws, &[0]);
        }
        if let (true, Some(buf)) = (real_orbs > 0, &self.orbs) {
            pass.set_pipeline(&p.orbs);
            pass.set_vertex_buffer(0, buf.slice(..));
            pass.draw(0..4, 0..real_orbs);
        }
    }

    fn attachments(&mut self, size: [u32; 2], format: wgpu::TextureFormat) {
        if self.attachments.as_ref().is_some_and(|a| a.size == size && a.format == format) {
            return;
        }
        let tex = |label, format| {
            self.device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: SAMPLES,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        self.attachments =
            Some(Attachments { size, format, color: tex("vo3d-msaa", format), depth: tex("vo3d-depth", DEPTH_FORMAT) });
    }
}

/// Segments around a light cone; `SEGMENTS` in cone.wgsl.
const CONE_SEGMENTS: u32 = 48;

/// Reflection in the plane `y = floor`.
fn mirror_y(floor: f32) -> Mat4 {
    Mat4::translation(Vec3::new(0.0, -floor, 0.0))
        .then(&Mat4::scale(Vec3::new(1.0, -1.0, 1.0)))
        .then(&Mat4::translation(Vec3::new(0.0, floor, 0.0)))
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn push_slot(buf: &mut Vec<u8>, bytes: &[u8]) {
    debug_assert!(bytes.len() as u64 <= SLOT);
    let start = buf.len();
    buf.extend_from_slice(bytes);
    buf.resize(start + SLOT as usize, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shaders_validate_in_naga() {
        for (name, src) in shader_sources() {
            let module =
                naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&src)));
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
                .validate(&module)
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    }

    #[test]
    fn blocks_fit_one_slot() {
        assert!(std::mem::size_of::<DrawBlock>() as u64 <= SLOT);
        assert!(std::mem::size_of::<CloudBlock>() as u64 <= SLOT);
        assert!(std::mem::size_of::<FloorBlock>() as u64 <= SLOT);
        assert!(std::mem::size_of::<ConeBlock>() as u64 <= SLOT);
    }
}
