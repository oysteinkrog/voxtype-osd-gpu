//! [`Renderer3d`]: draws a [`Frame3d`] into a colour texture in one 4x MSAA pass with its own
//! depth buffer, in the D304 order: background, meshes (depth test and write), point clouds,
//! light cones, then orbs (all three depth tested, no depth write, additive).
//!
//! With a floor (D305), a mirrored copy of the meshes, clouds and orbs comes first, right
//! after the background. Then the floor pass writes the floor plane's depth over it and adds
//! the pools of light, so the copy shows only below the floor line and the real scene draws
//! over it as usual.
//!
//! With [`crate::Shadows`] (EG-18), a depth-only pass from the key light draws the meshes
//! into a shadow map first; the meshes and the floor then read it (see [`crate::shadow`]).
//!
//! With [`Renderer3d::part`] set to `Behind` or `Front` (EG-20), every fragment, point and
//! orb on the other side of the text plane is dropped: the plane through the camera's target
//! that faces the camera (z = 0 on the stage camera), so a split element's two halves cover
//! the whole drawing once. The front half draws no background; the shadow map still holds
//! every mesh, so each half is shadowed by the other.
//!
//! The pass resolves into the caller's view, so it is a layer like any other for whatever
//! comes after it (2D display lists, text, post effects).

use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use vo_render::color::linear_premul;
use vo_render::text::slug_gpu::PREMULTIPLIED_OVER;
use vo_scene::geom::Color;

use crate::frame::{Background, CloudShape, Frame3d, MeshId, TextureId, MAX_POOLS};
use crate::math::{Mat4, Vec3};
use crate::mesh::{Mesh, Vertex};
use crate::shadow::{fit_light, world_box, SHADOW_FORMAT};

const COMMON: &str = include_str!("wgsl/common3d.wgsl");
const MESH: &str = include_str!("wgsl/mesh.wgsl");
const POINTS: &str = include_str!("wgsl/points.wgsl");
const ORBS: &str = include_str!("wgsl/orbs.wgsl");
const BACKGROUND: &str = include_str!("wgsl/background.wgsl");
const FLOOR: &str = include_str!("wgsl/floor.wgsl");
const CONE: &str = include_str!("wgsl/cone.wgsl");
/// The shadow caster pass; standalone, not prepended with `common3d.wgsl`.
pub const SHADOW_CASTER: &str = include_str!("wgsl/shadow.wgsl");

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
    light_view_proj: [[f32; 4]; 4],
    shadow: [f32; 4],
    shadow_k: [f32; 4],
    clip: [f32; 4],
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
    /// Local bounds, for fitting the shadow map.
    bounds: (Vec3, Vec3),
}

/// The key light's shadow map (EG-18), made on the first frame with shadows.
struct ShadowMap {
    size: u32,
    view: wgpu::TextureView,
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
    /// Meshes with [`crate::Material::texture`] (EG-14): `fs_mesh_tex`, group 2 bound.
    mesh_tex: wgpu::RenderPipeline,
    points: wgpu::RenderPipeline,
    orbs: wgpu::RenderPipeline,
    floor: wgpu::RenderPipeline,
    cone: wgpu::RenderPipeline,
}

/// The pipelines only shadows use (EG-18), made on the first frame with shadows.
struct ShadowPipelines {
    format: wgpu::TextureFormat,
    /// Depth only, into the shadow map.
    caster: wgpu::RenderPipeline,
    /// The floor's shadow: `fs_floor_shadow`, premultiplied over, no depth write.
    floor: wgpu::RenderPipeline,
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
    texture_layout: wgpu::BindGroupLayout,
    /// The textured mesh pipeline's layout: the two groups above plus the texture.
    texture_pipeline_layout: wgpu::PipelineLayout,
    texture_sampler: wgpu::Sampler,
    /// One bind group per [`TextureId`].
    textures: Vec<wgpu::BindGroup>,
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
    shadow_sampler: wgpu::Sampler,
    /// Bound in place of the shadow map when shadows are off.
    no_shadow: wgpu::TextureView,
    shadow: Option<ShadowMap>,
    shadow_pipelines: Option<ShadowPipelines>,
    /// The caster pass: its layout, the light matrix buffer and its bind group.
    caster_layout: wgpu::PipelineLayout,
    light_buf: wgpu::Buffer,
    light_bind: wgpu::BindGroup,
    /// Wall time spent compiling shader modules and pipelines, for cold-start reports.
    pub pipeline_time: Duration,
    /// Which half of the scene the next passes draw (EG-20); `Whole` by default.
    pub part: crate::item::Part,
    /// The second half's globals in [`Renderer3d::encode_halves`].
    globals_b: wgpu::Buffer,
}

/// One half of a split frame drawn by [`Renderer3d::encode_halves`]: its camera, which half,
/// and its place in the target, `x, y, w, h` in pixels.
#[derive(Clone, Copy, Debug)]
pub struct Half {
    pub camera: crate::camera::Camera,
    pub part: crate::item::Part,
    pub viewport: [u32; 4],
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
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: vf,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: vf,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
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
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo3d-texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let texture_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vo3d-textured"),
            bind_group_layouts: &[Some(&globals_layout), Some(&draw_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let texture_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vo3d-texture"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
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
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vo3d-shadow"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        // Never drawn into or read while shadows are off: `shadow.x` is 0 then.
        let no_shadow = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("vo3d-no-shadow"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: SHADOW_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let light_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo3d-light"),
            entries: &[uniform(0, wgpu::ShaderStages::VERTEX, false, 64)],
        });
        let caster_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vo3d-caster"),
            bind_group_layouts: &[Some(&light_layout), Some(&draw_layout)],
            immediate_size: 0,
        });
        let light_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo3d-light"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let light_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo3d-light"),
            layout: &light_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: light_buf.as_entire_binding() }],
        });
        Renderer3d {
            device: device.clone(),
            queue: queue.clone(),
            globals_layout,
            draw_layout,
            pipeline_layout,
            texture_layout,
            texture_pipeline_layout,
            texture_sampler,
            textures: Vec::new(),
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
            shadow_sampler,
            no_shadow,
            shadow: None,
            shadow_pipelines: None,
            caster_layout,
            light_buf,
            light_bind,
            pipeline_time: Duration::ZERO,
            part: crate::item::Part::Whole,
            globals_b: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vo3d-globals-b"),
                size: std::mem::size_of::<Globals>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
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
        self.meshes.push(GpuMesh {
            vertices,
            indices,
            count: mesh.indices.len() as u32,
            len: mesh.vertices.len(),
            bounds: bounds_of(&mesh.vertices),
        });
        MeshId(self.meshes.len() as u32 - 1)
    }

    /// Rewrites the vertices of mesh `id` for the next submit (a ribbon that bends each
    /// frame). The count must match the mesh as added; the indices stay. Like the other
    /// buffers, the write lands at submit, so one update per mesh per submit.
    pub fn update_mesh(&mut self, id: MeshId, vertices: &[Vertex]) {
        let mesh = &mut self.meshes[id.0 as usize];
        assert_eq!(mesh.len, vertices.len(), "update_mesh: the vertex count must stay the same");
        self.queue.write_buffer(&mesh.vertices, 0, bytemuck::cast_slice(vertices));
        mesh.bounds = bounds_of(vertices);
    }

    /// The pixel box `x0, y0, x1, y1` of what `frame` draws on a target `size` pixels big in
    /// the current [`Renderer3d::part`], grown by 2 pixels for antialiasing: the meshes' boxes
    /// cut at the text plane and projected. `None` when it cannot tell: a background, a floor,
    /// point clouds, orbs, light cones, or a box reaching behind the eye. An empty box (x1 <=
    /// x0) when nothing is drawn.
    pub fn draw_bounds_px(&self, frame: &Frame3d, size: [f32; 2]) -> Option<[f32; 4]> {
        let busy = !matches!(frame.background, Background::Transparent)
            || frame.floor.is_some()
            || !frame.clouds.is_empty()
            || !frame.orbs.is_empty()
            || !frame.cones.is_empty();
        if busy {
            return None;
        }
        let cam = &frame.camera;
        let vp = cam.view_proj(size[0] / size[1]);
        let fwd = (cam.target - cam.eye()).normalize();
        let k = match self.part {
            crate::item::Part::Whole => 0.0,
            crate::item::Part::Behind => 1.0,
            crate::item::Part::Front => -1.0,
        };
        // Signed distance from the plane on the kept side.
        let side = |p: Vec3| k * fwd.dot(p - cam.target);
        let mut b = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
        let mut add = |p: Vec3| -> bool {
            let c = vp.transform_point(p);
            if c[3] <= 1e-4 {
                return false;
            }
            let (x, y) = ((c[0] / c[3] + 1.0) * 0.5 * size[0], (1.0 - c[1] / c[3]) * 0.5 * size[1]);
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            true
        };
        for d in &frame.meshes {
            let (lo, hi) = self.meshes.get(d.mesh.0 as usize)?.bounds;
            let corner = |i: usize| {
                let p = Vec3::new(
                    if i & 1 == 0 { lo.x } else { hi.x },
                    if i & 2 == 0 { lo.y } else { hi.y },
                    if i & 4 == 0 { lo.z } else { hi.z },
                );
                let q = d.model.transform_point(p);
                Vec3::new(q[0] / q[3], q[1] / q[3], q[2] / q[3])
            };
            let corners: [Vec3; 8] = std::array::from_fn(corner);
            for (i, &p) in corners.iter().enumerate() {
                if side(p) >= 0.0 && !add(p) {
                    return None;
                }
                // Where the box's edges from this corner cross the plane.
                for bit in [1, 2, 4] {
                    let j = i | bit;
                    if j == i {
                        continue;
                    }
                    let (sa, sb) = (side(p), side(corners[j]));
                    if (sa < 0.0) != (sb < 0.0) {
                        let q = p + (corners[j] - p) * (sa / (sa - sb));
                        if !add(q) {
                            return None;
                        }
                    }
                }
            }
        }
        if b[0] > b[2] {
            return Some([0.0; 4]);
        }
        Some([b[0] - 2.0, b[1] - 2.0, b[2] + 2.0, b[3] + 2.0])
    }

    /// The shadow map's size in texels, or `None` while no frame has asked for shadows.
    pub fn shadow_map_size(&self) -> Option<u32> {
        self.shadow.as_ref().map(|s| s.size)
    }

    /// Registers a texture for [`crate::Material::texture`]: a 2D view of premultiplied,
    /// sRGB-encoded bytes in a plain UNORM format (what the 2D renderer writes into an
    /// `Rgba8Unorm` target), with any number of mip levels. [`crate::SurfaceText::view`]
    /// gives one.
    pub fn add_texture(&mut self, view: &wgpu::TextureView) -> TextureId {
        let bind = self.texture_bind(view);
        self.textures.push(bind);
        TextureId(self.textures.len() as u32 - 1)
    }

    /// Points `id` at another view, for a texture that was made again at a new size.
    pub fn set_texture(&mut self, id: TextureId, view: &wgpu::TextureView) {
        self.textures[id.0 as usize] = self.texture_bind(view);
    }

    fn texture_bind(&self, view: &wgpu::TextureView) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo3d-texture"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.texture_sampler) },
            ],
        })
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
        let make_in = |layout: &wgpu::PipelineLayout,
                       module: &wgpu::ShaderModule,
                       vs: &str,
                       fs: &str,
                       buffers: &[Option<wgpu::VertexBufferLayout>],
                       topology,
                       blend,
                       depth_write: bool,
                       compare| {
            self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(fs),
                layout: Some(layout),
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
        let make = |module: &wgpu::ShaderModule,
                    vs: &str,
                    fs: &str,
                    buffers: &[Option<wgpu::VertexBufferLayout>],
                    topology,
                    blend,
                    depth_write: bool,
                    compare| {
            make_in(&self.pipeline_layout, module, vs, fs, buffers, topology, blend, depth_write, compare)
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
            mesh_tex: make_in(
                &self.texture_pipeline_layout,
                mesh,
                "vs_mesh",
                "fs_mesh_tex",
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

    /// Makes the shadow map of `size` texels and the shadow pipelines, once.
    fn prepare_shadows(&mut self, size: u32, format: wgpu::TextureFormat) {
        if self.shadow.as_ref().is_none_or(|s| s.size != size) {
            let view = self
                .device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("vo3d-shadow"),
                    size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: SHADOW_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default());
            self.shadow = Some(ShadowMap { size, view });
        }
        if self.shadow_pipelines.as_ref().is_some_and(|p| p.format == format) {
            return;
        }
        let start = Instant::now();
        let caster_module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow"),
            source: wgpu::ShaderSource::Wgsl(SHADOW_CASTER.into()),
        });
        let caster = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vo3d-caster"),
            layout: Some(&self.caster_layout),
            vertex: wgpu::VertexState {
                module: &caster_module,
                entry_point: Some("vs_caster"),
                compilation_options: Default::default(),
                buffers: &[Some(Vertex::layout())],
            },
            fragment: None,
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: SHADOW_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                // Keeps a surface from shading itself where it slopes away from the light.
                bias: wgpu::DepthBiasState { constant: 2, slope_scale: 2.0, clamp: 0.0 },
            }),
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let floor_module = &self.modules.as_ref().expect("prepare first")[4];
        let floor = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vo3d-floor-shadow"),
            layout: Some(&self.pipeline_layout),
            vertex: wgpu::VertexState {
                module: floor_module,
                entry_point: Some("vs_floor"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: floor_module,
                entry_point: Some("fs_floor_shadow"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(PREMULTIPLIED_OVER),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState { count: SAMPLES, ..Default::default() },
            multiview_mask: None,
            cache: None,
        });
        self.shadow_pipelines = Some(ShadowPipelines { format, caster, floor });
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
        self.encode_inner(enc, out, frame, field, extra, None);
    }

    /// Both halves of a split frame (EG-20) in one pass, side by side in `out`: each half
    /// drawn with its own camera and part into its own viewport, which must not overlap. One
    /// pass instead of two saves a pass's fixed cost. Only for frames of meshes alone (see
    /// [`Renderer3d::draw_bounds_px`]); `frame.camera` is ignored. Leaves
    /// [`Renderer3d::part`] as it was.
    pub fn encode_halves(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        out: Output<'_>,
        frame: &Frame3d,
        halves: [Half; 2],
    ) {
        debug_assert!(self.draw_bounds_px(frame, [1.0, 1.0]).is_some(), "encode_halves: meshes only");
        self.encode_inner(enc, out, frame, None, None, Some(halves));
    }

    fn encode_inner(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        out: Output<'_>,
        frame: &Frame3d,
        field: Option<FieldBinding<'_>>,
        extra: Option<ExtraDraw<'_>>,
        halves: Option<[Half; 2]>,
    ) {
        let Output { view: target, size, format, scale } = out;
        self.prepare(format);
        self.attachments(size, format);
        let [w, h] = size.map(|v| v as f32);
        let encode_srgb = !format.is_srgb() && !matches!(format, wgpu::TextureFormat::Rgba16Float);

        // Globals.
        let cam = &frame.camera;
        let l = &frame.light;
        let full = Half { camera: frame.camera, part: self.part, viewport: [0, 0, size[0], size[1]] };
        let lin = |c: Color| linear_premul(c, 1.0);
        let (bg_top, bg_bottom) = match frame.background {
            Background::Transparent => ([0.0; 4], [0.0; 4]),
            Background::Solid(c) => (lin(c), lin(c)),
            Background::Vertical { top, bottom } => (lin(top), lin(bottom)),
        };
        let to_light = (-l.dir).normalize();
        let lc = lin(l.color);
        let fog = frame.fog.map_or([0.0, 0.0, 0.0, scale], |(a, b)| [a, b, 1.0, scale]);

        // Shadows (EG-18): fit the light's box to the casters' bounds in this pose.
        let casters: Vec<usize> =
            (0..frame.meshes.len()).filter(|&i| frame.meshes[i].material.color.a >= 0.5).collect();
        let fit = frame.shadows.and_then(|sh| {
            let boxes: Vec<(Vec3, Vec3)> = casters
                .iter()
                .map(|&i| {
                    let d = &frame.meshes[i];
                    world_box(&d.model, self.meshes[d.mesh.0 as usize].bounds)
                })
                .collect();
            fit_light(l.dir, &boxes, sh.size.max(64)).map(|f| (sh, f))
        });
        if let Some((sh, _)) = fit {
            self.prepare_shadows(sh.size.max(64), format);
        }
        let focal = cam.focal_px(h);
        let (shadow, shadow_k, light_view_proj) = match &fit {
            Some((sh, f)) => {
                let size = sh.size.max(64) as f32;
                let per_px = match cam.projection {
                    crate::camera::Projection::Orthographic { .. } => [1.0 / focal, 0.0],
                    _ => [0.0, 1.0 / focal],
                };
                (
                    [1.0, 1.0 / size, sh.softness.max(0.0) * scale, sh.strength.clamp(0.0, 1.0)],
                    [per_px[0], per_px[1], f.texel, (f.margin as f32 - 1.5) / size],
                    f.view_proj.cols,
                )
            }
            None => ([0.0; 4], [0.0; 4], Mat4::IDENTITY.cols),
        };
        let globals_of = |half: &Half| {
            let cam = &half.camera;
            let eye = cam.eye();
            let [w, h] = [half.viewport[2] as f32, half.viewport[3] as f32];
            // The text plane through the camera's target, facing the camera (EG-20): keep
            // where dot(n, p) + d >= 0.
            let fwd = (cam.target - eye).normalize();
            let behind = [fwd.x, fwd.y, fwd.z, -fwd.dot(cam.target)];
            let clip = match half.part {
                crate::item::Part::Whole => [0.0; 4],
                crate::item::Part::Behind => behind,
                crate::item::Part::Front => behind.map(|v| -v),
            };
            Globals {
                view_proj: cam.view_proj(w / h).cols,
                view: cam.view().cols,
                light_dir: [to_light.x, to_light.y, to_light.z, l.rim],
                light_color: [lc[0] * l.intensity, lc[1] * l.intensity, lc[2] * l.intensity, 1.0],
                ambient: lin(l.ambient),
                eye: [eye.x, eye.y, eye.z, 1.0],
                viewport: [w, h, cam.focal_px(h), encode_srgb as u32 as f32],
                focus: [
                    frame.focus.distance,
                    frame.focus.range,
                    frame.focus.max_blur_px,
                    field.is_some() as u32 as f32,
                ],
                fog,
                bg_top,
                bg_bottom,
                field: field
                    .as_ref()
                    .map_or([1.0, 1.0, 1.0, 0.0], |f| [f.size[0] as f32, f.size[1] as f32, f.scale, 0.0]),
                floor: frame.floor.map_or([0.0; 4], |f| [f.y, f.fade, f.reflect, 1.0]),
                light_view_proj,
                shadow,
                shadow_k,
                clip,
            }
        };
        let halves_or_full = halves.unwrap_or([full, full]);
        self.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals_of(&halves_or_full[0])));
        if halves.is_some() {
            self.queue.write_buffer(&self.globals_b, 0, bytemuck::bytes_of(&globals_of(&halves_or_full[1])));
        }
        let bind_globals = |buf: &wgpu::Buffer| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vo3d-globals"),
                layout: &self.globals_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(field.as_ref().map_or(&self.no_field, |f| f.view)),
                    },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(match (&fit, &self.shadow) {
                            (Some(_), Some(s)) => &s.view,
                            _ => &self.no_shadow,
                        }),
                    },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.shadow_sampler) },
                ],
            })
        };
        let globals_bind = bind_globals(&self.globals);
        let globals_b_bind = halves.map(|_| bind_globals(&self.globals_b));

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

        // The caster pass: the meshes' depth from the key light, into the shadow map.
        if let (Some((_, f)), Some(map), Some(sp)) = (&fit, &self.shadow, &self.shadow_pipelines) {
            self.queue.write_buffer(&self.light_buf, 0, bytemuck::bytes_of(&f.view_proj.cols));
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vo3d-shadow"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &map.view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&sp.caster);
            pass.set_bind_group(0, &self.light_bind, &[]);
            let draws = &self.draws.as_ref().unwrap().1;
            for &i in &casters {
                let mesh = &self.meshes[frame.meshes[i].mesh.0 as usize];
                pass.set_bind_group(1, draws, &[i as u32 * SLOT as u32]);
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.count, 0, 0..1);
            }
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
        let (n_mesh, n_cloud) = (frame.meshes.len() as u32, frame.clouds.len() as u32);
        let at = |slot: u32| [slot * SLOT as u32];
        let draw_scene = |pass: &mut wgpu::RenderPass<'_>, first: u32, globals_bind: &wgpu::BindGroup| {
            pass.set_pipeline(&p.mesh);
            let mut textured = false;
            for (i, d) in frame.meshes.iter().enumerate() {
                let mesh = &self.meshes[d.mesh.0 as usize];
                let tex = d.material.texture.and_then(|t| self.textures.get(t.0 as usize));
                if tex.is_some() != textured {
                    textured = tex.is_some();
                    pass.set_pipeline(if textured { &p.mesh_tex } else { &p.mesh });
                    pass.set_bind_group(0, globals_bind, &[]);
                }
                if let Some(bind) = tex {
                    pass.set_bind_group(2, bind, &[]);
                }
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
        // Two halves side by side (EG-20): meshes only, each in its own viewport.
        if let (Some(hs), Some(bind_b)) = (halves, &globals_b_bind) {
            for (half, bind) in hs.iter().zip([&globals_bind, bind_b]) {
                let [x, y, w, h] = half.viewport;
                pass.set_viewport(x as f32, y as f32, w as f32, h as f32, 0.0, 1.0);
                pass.set_scissor_rect(x, y, w, h);
                pass.set_bind_group(0, bind, &[]);
                draw_scene(&mut pass, 0, bind);
            }
            return;
        }
        if !matches!(frame.background, Background::Transparent) && self.part != crate::item::Part::Front {
            pass.set_pipeline(&p.background);
            pass.draw(0..3, 0..1);
        }
        let mut next = n_mesh + n_cloud;
        if frame.floor.is_some() {
            draw_scene(&mut pass, next, &globals_bind);
            next += n_mesh + n_cloud;
            if let (true, Some(buf)) = (orbs.len() as u32 > real_orbs, &self.orbs) {
                pass.set_pipeline(&p.orbs);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..4, real_orbs..orbs.len() as u32);
            }
            pass.set_pipeline(&p.floor);
            pass.set_bind_group(1, draws, &at(next));
            pass.draw(0..3, 0..1);
            if let (Some(_), Some(sp)) = (&fit, &self.shadow_pipelines) {
                pass.set_pipeline(&sp.floor);
                pass.draw(0..3, 0..1);
            }
            next += 1;
        }
        draw_scene(&mut pass, 0, &globals_bind);
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

fn bounds_of(vertices: &[Vertex]) -> (Vec3, Vec3) {
    let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for v in vertices {
        for k in 0..3 {
            lo[k] = lo[k].min(v.pos[k]);
            hi[k] = hi[k].max(v.pos[k]);
        }
    }
    if vertices.is_empty() {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    (Vec3::new(lo[0], lo[1], lo[2]), Vec3::new(hi[0], hi[1], hi[2]))
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
        for (name, src) in shader_sources().into_iter().chain([("shadow", SHADOW_CASTER.to_string())]) {
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
