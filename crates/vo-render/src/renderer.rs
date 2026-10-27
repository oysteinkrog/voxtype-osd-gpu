//! Draws a `vo_scene` display list into a texture.
//!
//! [`Renderer::render`] walks the layers in paint order and records one command per run of
//! shapes or glyphs, with a layer block (transform, clip, opacity, stage warp) bound at a dynamic
//! offset. Shapes go through the SDF uber-shader in one instanced draw per run; glyphs go
//! through the Slug path. Then it encodes one render pass per offscreen layer, deepest first,
//! and one pass on the target.
//!
//! Two fixed pass lists (ROADMAP 4.3), picked per frame:
//! - **Direct path**, for a frame with no layer effect: everything draws straight into the
//!   target, offscreen layers in the target's format.
//! - **Effect path**, when any layer has bloom: the root draws into a linear `Rgba16Float`
//!   scene texture and every offscreen layer into an `Rgba16Float` texture; a bloom layer runs
//!   its pyramid after it is drawn and adds the result when it is composited; a last pass
//!   tone maps, encodes and dithers the scene into the target.
//!
//! Glow and drop shadow (`PaintKind::Glow`, `PaintKind::Shadow`) are computed in the shape
//! shader on both paths. On the direct path a glow's strength is capped at 1.
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
//!   round, at an outer clip's corners. A bloom's halo reaches past its own layer's clip.
//! - **Offscreen layers** get a texture that covers what they draw, plus the bloom's reach,
//!   at a whole-pixel offset so compositing stays a 1:1 copy. Sizes round up to 64 pixels so
//!   textures are reused from a pool while a layer moves or grows.
//! - **GPU elements** (`vo_scene::gpu`) break the surface's pass: the pass ends, the painters
//!   draw, and a new pass resumes with `LoadOp::Load` (`crate::gpu`).
//! - **Dots** have a pipeline of their own with 16-byte instances (`dots.rs`).

use std::ops::Range;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use skrifa::GlyphId;
use vo_scene::display::{BlendMode, Bloom, DisplayList, GlyphInstance as DlGlyph, ItemKind, LayerTarget, Mask};
use vo_scene::font::{Face, FontRegistry};
use vo_scene::geom::{Affine, Point, RRect};
use vo_scene::path::PATH_FONT;
use vo_scene::stage::Warp;

use crate::color::linear_premul;
use crate::dots::GpuDot;
use crate::glass::{glass_layer_shader_source, GlassGpu};
use crate::gpu::{GpuPainter, PaintCx, StagePx};
use crate::hot::wgsl;
use crate::material::{materials_source, GpuMaterial, MaterialFn};
use crate::particle_layer::ParticleLayer;
use crate::profile::{BloomReport, FrameReport, FrameTimings, Profiler, Section, SurfaceKind, SurfaceReport};
use crate::shapes::{pixels_per_unit, GpuShape};
use crate::text::slug_gpu::{self, Atlas, GlyphInstance, SlugAtlasGpu, Uniforms, PREMULTIPLIED_OVER};
use crate::vector::{vector_shader_source_with, GpuVector};

const COMMON_WGSL: &str = include_str!("wgsl/common.wgsl");
const SHAPES_WGSL: &str = include_str!("wgsl/shapes.wgsl");
const GLYPHS_WGSL: &str = include_str!("wgsl/glyphs.wgsl");
const COMPOSITE_WGSL: &str = include_str!("wgsl/composite.wgsl");
const DOTS_WGSL: &str = include_str!("wgsl/dots.wgsl");
const BLOOM_WGSL: &str = include_str!("wgsl/bloom.wgsl");
const NOISE_WGSL: &str = include_str!("wgsl/noise.wgsl");
const TRAIL_WGSL: &str = include_str!("wgsl/trail.wgsl");

/// The format of the scene and of every layer texture on the effect path.
pub const EFFECT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Layer texture sizes round up to this, so a layer that moves or grows a little keeps its
/// pooled texture, and every bloom level is an exact half of the one above.
pub(crate) const ALIGN: u32 = 64;
/// Frames a pooled texture may go unused before it is freed.
const POOL_KEEP_FRAMES: u64 = 120;
/// Bloom levels at most, below the layer texture.
const MAX_LEVELS: u32 = 6;
/// How much of the lower level each bloom upsample mixes in.
pub(crate) const BLOOM_MIX: f32 = 0.65;
/// A trail starts fresh after a gap between frames longer than this, in seconds.
const TRAIL_MAX_GAP: f64 = 0.5;

/// Quality tiers (MO09). Every tier draws a correct picture; a lower one leaves out the costly
/// effects. `Full` is the default. The renderer drops one tier by itself when a texture for the
/// effect path cannot be allocated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quality {
    /// No bloom, no glow, no drop shadow.
    Minimal,
    /// Glow and drop shadow from the shape shader, no bloom: every frame takes the direct path.
    NoBloom,
    #[default]
    Full,
}

impl Quality {
    /// `full`, `no-bloom` or `minimal`.
    pub fn parse(s: &str) -> Option<Quality> {
        match s.trim() {
            "full" => Some(Quality::Full),
            "no-bloom" => Some(Quality::NoBloom),
            "minimal" => Some(Quality::Minimal),
            _ => None,
        }
    }

    /// From `VO_QUALITY`, when it is set to a known tier.
    pub fn from_env() -> Option<Quality> {
        std::env::var("VO_QUALITY").ok().and_then(|v| Quality::parse(&v))
    }

    fn lower(self) -> Quality {
        match self {
            Quality::Full => Quality::NoBloom,
            _ => Quality::Minimal,
        }
    }
}

/// The shape shader: `common.wgsl`, `shapes.wgsl` and `materials.wgsl`.
pub fn shape_shader_source() -> String {
    shape_shader_source_with(&[])
}

/// [`shape_shader_source`] with app material functions (FX15).
pub fn shape_shader_source_with(materials: &[MaterialFn]) -> String {
    [wgsl("common.wgsl", COMMON_WGSL), wgsl("shapes.wgsl", SHAPES_WGSL), materials_source(materials).into()].join("\n")
}

/// The display-list glyph shader: the Slug shader, `common.wgsl`, `noise.wgsl`,
/// `glyphs.wgsl` and `materials.wgsl`.
pub fn glyph_shader_source() -> String {
    glyph_shader_source_with(&[])
}

/// [`glyph_shader_source`] with app material functions (FX15).
pub fn glyph_shader_source_with(materials: &[MaterialFn]) -> String {
    [
        slug_gpu::shader_source().into(),
        wgsl("common.wgsl", COMMON_WGSL),
        noise_shader_source(),
        wgsl("glyphs.wgsl", GLYPHS_WGSL),
        materials_source(materials).into(),
    ]
    .join("\n")
}

/// The noise library (FX10, `noise.wgsl`), for a shader to append: seeded hashes, value and
/// simplex noise, fbm, curl and domain warp, all named `noise_*`. `vo_core::noise` is the
/// same in Rust.
pub fn noise_shader_source() -> std::borrow::Cow<'static, str> {
    wgsl("noise.wgsl", NOISE_WGSL)
}

/// The offscreen layer composite shader: `common.wgsl`, `noise.wgsl` and `composite.wgsl`.
pub fn composite_shader_source() -> String {
    [wgsl("common.wgsl", COMMON_WGSL), noise_shader_source(), wgsl("composite.wgsl", COMPOSITE_WGSL)].join("\n")
}

/// The dot shader: `common.wgsl` and `dots.wgsl`.
pub fn dot_shader_source() -> String {
    [wgsl("common.wgsl", COMMON_WGSL), wgsl("dots.wgsl", DOTS_WGSL)].join("\n")
}

/// The bloom pyramid and the effect path's last pass.
pub fn bloom_shader_source() -> String {
    wgsl("bloom.wgsl", BLOOM_WGSL).into_owned()
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
    origin: [f32; 4],
    src: [f32; 4],
    smear: [f32; 4],
    clip_fade: [f32; 4],
    mask: [f32; 4],
    style_box: [f32; 4],
    style_a: [f32; 4],
    style_b: [f32; 4],
    /// The stage warp in target pixels (`vo_scene::stage`): rows of the 3x3, and of its
    /// inverse. `warp0[3]` is 1 when the layer is warped.
    warp0: [f32; 4],
    warp1: [f32; 4],
    warp2: [f32; 4],
    unwarp0: [f32; 4],
    unwarp1: [f32; 4],
    unwarp2: [f32; 4],
}

/// One block of `Fx` in `bloom.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct FxUniform {
    texel: [f32; 2],
    weight: f32,
    mode: f32,
}

#[derive(Clone, Copy)]
struct ClipState {
    rect: RRect,
    /// Physical pixels to the clip owner's layer space.
    from_px: Affine,
    ppu: f32,
    /// Edge fades in clip units: left, top, right, bottom.
    fade: [f32; 4],
}

#[derive(Clone, Copy)]
struct LayerState {
    to_px: Affine,
    ppu: f32,
    /// Multiplied into this layer's own instances.
    opacity: f32,
    clip: Option<ClipState>,
    /// `x0, y0, x1, y1` in target pixels.
    scissor: [f32; 4],
    /// Motion blur smear in target pixels, this layer's own plus its parents'.
    smear: [f32; 2],
    /// Inside a mask's content (FX04): 1 writes coverage, 2 luminance; 0 elsewhere.
    mask_out: f32,
    /// The stage warp in target pixels, composed with every parent's.
    warp: Option<Warp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pipe {
    Shape,
    Glyph,
    Composite,
    Dot,
    Vector,
    Glass,
    CompositeBloom,
    /// Composites a masked layer through its mask's texture (FX04).
    CompositeMask,
    /// Composites a layer with a style (FX13).
    CompositeStyle,
    Down,
    Up,
    Final,
    /// Fades a feedback trail's last frame by the blend constant.
    Fade,
    /// Merges a trail's new frame into its faded pixels by the larger value per channel.
    TrailMax,
    /// Particles (`crate::particle_layer`): the range is of its placements.
    Particles,
    /// GPU items drawn by painters (`crate::gpu`): the range is of `Renderer::gpu_items`. A
    /// pass break: the pass ends before it and resumes with `LoadOp::Load` after it.
    Gpu,
}

impl Pipe {
    /// Commands that run outside a render pass, between two passes on one surface.
    fn breaks_pass(self) -> bool {
        matches!(self, Pipe::Gpu)
    }
}

/// A GPU item placed for this frame.
struct GpuCmd {
    item: vo_scene::gpu::GpuItem,
    /// The layer's space to target pixels.
    to_px: Affine,
    /// The stage warp in target pixels.
    warp: Option<Warp>,
    /// The stage camera in target pixels.
    stage: StagePx,
    opacity: f32,
}

#[derive(Clone, Debug)]
struct Cmd {
    pipe: Pipe,
    /// Instance range for shapes and glyphs; the surface index for a composite.
    range: Range<u32>,
    uniform: u32,
    blend: BlendMode,
    /// In target pixels.
    scissor: [f32; 4],
}

const EMPTY: [f32; 4] = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];

/// A render pass: the target (surface 0) or one offscreen layer.
struct Surface {
    cmds: Vec<Cmd>,
    /// What the surface draws, in target pixels, `x0, y0, x1, y1`.
    bounds: [f32; 4],
    /// The texture's top left in target pixels, and its size. A size of 0 draws nothing.
    origin: [i32; 2],
    size: [u32; 2],
    /// Pool index of the texture; `None` for the target itself.
    tex: Option<usize>,
    bloom: Option<BloomPlan>,
    trail: Option<TrailPlan>,
    /// The surface of this layer's mask, and how it masks.
    mask: Option<(usize, Mask)>,
    /// A mask's own surface: an R8 texture of coverage, never composited.
    r8: bool,
    /// The display list layer, for an offscreen layer or a mask (`frame_report`).
    layer: Option<u32>,
    /// Composited with a layer style (`frame_report`).
    style: bool,
}

impl Surface {
    fn new() -> Surface {
        Surface {
            cmds: Vec::new(),
            bounds: EMPTY,
            origin: [0, 0],
            size: [0, 0],
            tex: None,
            bloom: None,
            trail: None,
            mask: None,
            r8: false,
            layer: None,
            style: false,
        }
    }

    /// The texture format the surface draws into, given the frame's layer format.
    fn format(&self, layers: wgpu::TextureFormat) -> wgpu::TextureFormat {
        if self.r8 {
            wgpu::TextureFormat::R8Unorm
        } else {
            layers
        }
    }

    /// The texture's rectangle in target pixels.
    fn rect(&self) -> [f32; 4] {
        let [x, y] = self.origin.map(|v| v as f32);
        [x, y, x + self.size[0] as f32, y + self.size[1] as f32]
    }
}

/// One layer's feedback trail for this frame (FX11). The layer covers the whole target and
/// keeps its texture from frame to frame, found again by `key`.
struct TrailPlan {
    key: u64,
    half_life: f32,
    /// The time the frame shows.
    t: f64,
    /// What last frame's pixels are multiplied by before this frame draws. `None` clears
    /// instead: the first frame, a seek backward, a gap over `TRAIL_MAX_GAP` or a new size.
    fade: Option<f32>,
    /// Half-life multiplier per channel (phosphor, FX15): a channel keeps `fade^(1 / d)`.
    decay: [f32; 3],
    /// With a fade: the pool index of the cleared texture this frame draws into before it
    /// merges into the trail. Drawing straight over the faded pixels would add the frame
    /// onto its own afterglow, so still content would brighten with every frame.
    fresh: Option<usize>,
}

/// One layer's bloom for this frame.
struct BloomPlan {
    strength: f32,
    /// Levels used, 1 to `MAX_LEVELS`.
    levels: u32,
    /// How much the deepest level mixes in, 0 to 1, so the radius changes smoothly.
    top: f32,
    /// Pool indices: level 1 to `levels`, then the upsamples of levels 1 to `levels - 1`.
    down: Vec<usize>,
    up: Vec<usize>,
    /// Byte offsets of the Fx blocks: one per downsample, then one per upsample.
    fx: Vec<u32>,
}

impl BloomPlan {
    /// The pool index holding the finished bloom, at half the layer's size.
    fn result(&self) -> usize {
        if self.levels == 1 {
            self.down[0]
        } else {
            self.up[0]
        }
    }
}

struct PoolTex {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// For `fs_composite`: the texture alone.
    bind: wgpu::BindGroup,
    used: bool,
    last_used: u64,
    /// Set for a feedback trail's texture, which keeps its pixels and is never handed out by
    /// `acquire`.
    key: Option<u64>,
    /// The time a trail's pixels show.
    t: f64,
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
    /// The signal bus block (`vo_core::signals`), group 3 of every layer pipeline.
    signals_layout: wgpu::BindGroupLayout,
    signals_buf: wgpu::Buffer,
    signals_bind: wgpu::BindGroup,
    composite_layout: wgpu::BindGroupLayout,
    /// A masked layer's composite: its texture and its mask's.
    mask_layout: wgpu::BindGroupLayout,
    /// Made on the first effect frame: sampled textures and the Fx block.
    fx: Option<FxShared>,
    modules: [Option<wgpu::ShaderModule>; 9],
    pipelines: Vec<((wgpu::TextureFormat, Pipe, BlendMode), wgpu::RenderPipeline)>,
    uniform_stride: u32,
    // Per frame, cleared each time.
    shapes: Vec<GpuShape>,
    glyphs: Vec<GlyphInstance>,
    dots: Vec<GpuDot>,
    vectors: Vec<GpuVector>,
    /// Vector segments, two texels each, in target pixels.
    vector_segs: Vec<[f32; 4]>,
    uniforms: Vec<u8>,
    fx_uniforms: Vec<u8>,
    surfaces: Vec<Surface>,
    /// The frame is on the effect path; the pool index of the scene texture.
    scene: Option<usize>,
    frame: u64,
    // GPU buffers, grown when needed.
    shape_buf: Option<wgpu::Buffer>,
    glyph_buf: Option<wgpu::Buffer>,
    dot_buf: Option<wgpu::Buffer>,
    glass: Vec<GlassGpu>,
    glass_buf: Option<wgpu::Buffer>,
    vector_buf: Option<wgpu::Buffer>,
    vector_layout: wgpu::BindGroupLayout,
    vector_segs_buf: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    /// Material blocks (FX15) for this frame, their storage buffer at group 2 of the shape,
    /// glyph and vector pipelines, and the app's material functions.
    materials: Vec<GpuMaterial>,
    material_layout: wgpu::BindGroupLayout,
    material_buf: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    material_fns: Vec<MaterialFn>,
    uniform_buf: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    fx_buf: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    pool: Vec<PoolTex>,
    quality: Quality,
    /// Light-mode shading (FX14).
    light: bool,
    /// Emitter elements and particle words, which live from frame to frame.
    particles: ParticleLayer,
    /// How this frame's pass on the target starts (`encode_with`).
    load: wgpu::LoadOp<wgpu::Color>,
    /// Draw GPU items (`add_painter`).
    painters: Vec<Box<dyn GpuPainter>>,
    gpu_items: Vec<GpuCmd>,
    /// GPU timestamps per section, when profiling is on.
    profiler: Option<Profiler>,
}

struct FxShared {
    /// `src`, `low`, a sampler: the pyramid passes, the final pass and the bloom composite.
    sampled_layout: wgpu::BindGroupLayout,
    fx_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
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
        let signals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-signals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let signals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-signals"),
            size: (vo_core::signals::SIGNAL_BLOCK_FLOATS * 4) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let signals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-signals"),
            layout: &signals_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: signals_buf.as_entire_binding() }],
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-composite"),
            entries: &[texture_entry(0, false)],
        });
        let mask_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-composite-mask"),
            entries: &[texture_entry(0, false), texture_entry(1, false)],
        });
        Renderer {
            device: device.clone(),
            queue: queue.clone(),
            particles: ParticleLayer::new(fonts.clone()),
            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            painters: Vec::new(),
            gpu_items: Vec::new(),
            profiler: None,
            fonts,
            faces: Vec::new(),
            atlas: Atlas::default(),
            atlas_gpu: SlugAtlasGpu::new(device),
            layer_layout,
            signals_layout,
            signals_buf,
            signals_bind,
            composite_layout,
            mask_layout,
            fx: None,
            modules: [None, None, None, None, None, None, None, None, None],
            pipelines: Vec::new(),
            uniform_stride,
            shapes: Vec::new(),
            glyphs: Vec::new(),
            dots: Vec::new(),
            vectors: Vec::new(),
            vector_segs: Vec::new(),
            uniforms: Vec::new(),
            fx_uniforms: Vec::new(),
            surfaces: Vec::new(),
            scene: None,
            frame: 0,
            shape_buf: None,
            glyph_buf: None,
            dot_buf: None,
            glass: Vec::new(),
            glass_buf: None,
            vector_buf: None,
            vector_layout: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("vo-vector-segs"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            }),
            vector_segs_buf: None,
            materials: Vec::new(),
            material_layout: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("vo-materials"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            }),
            material_buf: None,
            material_fns: Vec::new(),
            uniform_buf: None,
            fx_buf: None,
            pool: Vec::new(),
            quality: Quality::from_env().unwrap_or_default(),
            light: false,
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

    /// Bytes of GPU memory in pooled layer, scene and bloom textures.
    pub fn pool_bytes(&self) -> u64 {
        self.pool
            .iter()
            .map(|p| {
                let t = &p.texture;
                let texel = t.format().block_copy_size(None).unwrap_or(4) as u64;
                t.width() as u64 * t.height() as u64 * texel
            })
            .sum()
    }

    /// The quality tier. Starts from `VO_QUALITY`, else `Full`.
    pub fn quality(&self) -> Quality {
        self.quality
    }

    pub fn set_quality(&mut self, q: Quality) {
        self.quality = q;
    }

    /// Light-mode shading (FX14): a bloom becomes a soft dark shadow under its layer instead of
    /// added light, because light added to a light backdrop washes out. Off by default.
    pub fn set_light(&mut self, light: bool) {
        self.light = light;
    }

    /// Whether the last frame took the effect path.
    pub fn last_frame_had_effects(&self) -> bool {
        self.scene.is_some()
    }

    /// Turns GPU timestamps per section on or off (`crate::profile`). Returns whether they
    /// are on: the device needs `profile::timestamp_features()`.
    pub fn set_profiling(&mut self, on: bool) -> bool {
        self.profiler =
            if on { self.profiler.take().or_else(|| Profiler::new(&self.device, &self.queue)) } else { None };
        self.profiler.is_some()
    }

    /// The GPU times of the frame encoded last, once it is submitted. Blocks until the GPU
    /// has run it. `None` without profiling, and for a frame already read.
    pub fn last_timings(&mut self) -> Option<FrameTimings> {
        let report = self.frame_report();
        let label = |s: Section| match s {
            Section::Draw { surface } => {
                report.surfaces.get(surface).map_or((None, "?".into()), |r| (r.layer, r.label()))
            }
            Section::Bloom { surface } => report
                .surfaces
                .get(surface)
                .map_or((None, "? bloom".into()), |r| (r.layer, format!("{} bloom", r.label()))),
            Section::Final => (None, "final".into()),
        };
        self.profiler.as_mut()?.read(&self.device, label)
    }

    /// The surfaces and instance counts of the frame encoded last (`crate::profile`).
    pub fn frame_report(&self) -> FrameReport {
        let surfaces = self
            .surfaces
            .iter()
            .enumerate()
            .map(|(i, s)| SurfaceReport {
                kind: match (i, s.r8, self.scene.is_some()) {
                    (0, _, false) => SurfaceKind::Target,
                    (0, _, true) => SurfaceKind::Scene,
                    (_, true, _) => SurfaceKind::Mask,
                    _ => SurfaceKind::Layer,
                },
                layer: s.layer,
                content: s.bounds,
                texture: if s.size[0] == 0 { [0.0; 4] } else { s.rect() },
                bloom: s.bloom.as_ref().map(|b| BloomReport {
                    levels: b.levels,
                    strength: b.strength,
                    // Three times the radius the pyramid stands for (`bloom_levels`).
                    reach: 3.0 * 2f32.powf(b.levels as f32 - 1.0 + b.top),
                }),
                trail: s.trail.is_some(),
                style: s.style,
                masked: s.mask.is_some(),
                draws: s.cmds.len(),
            })
            .collect();
        FrameReport {
            effects: self.scene.is_some(),
            size: self.surfaces.first().map_or([0, 0], |s| s.size),
            surfaces,
            shapes: self.shapes.len(),
            glyphs: self.glyphs.len(),
            dots: self.dots.len(),
            vectors: self.vectors.len(),
            glass: self.glass.len(),
            gpu_items: self.gpu_items.len(),
            pool_bytes: self.pool_bytes(),
        }
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

    /// Adds an app material function (FX15): `wgsl` defines `fn name (m: Material, i: MatIn) ->
    /// vec4f` (see `wgsl/materials.wgsl` for the inputs), drawn for every material whose
    /// `func` is `func`, which must be at least `vo_scene::material::CUSTOM_FIRST`. A later
    /// function with the same id replaces it. The shape, glyph and vector shaders are rebuilt
    /// on their next use, so add materials before the first frame; WGSL that does not compile
    /// fails there as any shader would.
    pub fn add_material(&mut self, func: u32, name: &str, wgsl: &str) {
        assert!(
            func >= vo_scene::material::CUSTOM_FIRST,
            "material ids below {} are built in",
            vo_scene::material::CUSTOM_FIRST
        );
        self.material_fns.retain(|f| f.func != func);
        self.material_fns.push(MaterialFn { func, name: name.into(), source: wgsl.into() });
        for slot in [0, 1, 5] {
            self.modules[slot] = None;
        }
        self.pipelines.retain(|((_, p, _), _)| !matches!(p, Pipe::Shape | Pipe::Glyph | Pipe::Vector));
    }

    /// Adds a painter for GPU elements (`vo_scene::gpu`). Painters are asked in the order they
    /// were added; an item no painter knows is skipped.
    pub fn add_painter(&mut self, painter: Box<dyn GpuPainter>) {
        self.painters.push(painter);
    }

    /// Records the frame into `enc` without submitting, so a caller can add a copy after it.
    /// The layout and bind group of the signal bus block, for app GPU code (`crate::gpu`) that
    /// wants audio in its own shaders: bind it as group 3 and paste `SIGNALS_WGSL`
    /// (`vo_core::signals`) with `@group(3) @binding(0) var<uniform> signals: Signals;`. It holds
    /// the block of the list being encoded.
    pub fn signals_binding(&self) -> (&wgpu::BindGroupLayout, &wgpu::BindGroup) {
        (&self.signals_layout, &self.signals_bind)
    }

    pub fn encode(&mut self, list: &DisplayList, scale: f32, target: &Target, enc: &mut wgpu::CommandEncoder) {
        self.encode_with(list, scale, target, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), enc);
    }

    /// [`Renderer::encode`] with the target's load op. With `LoadOp::Load` the frame is drawn
    /// premultiplied over what the target already holds, on both paths: the direct path loads
    /// the target, and the effect path's last pass blends the scene over it.
    pub fn encode_with(
        &mut self,
        list: &DisplayList,
        scale: f32,
        target: &Target,
        load: wgpu::LoadOp<wgpu::Color>,
        enc: &mut wgpu::CommandEncoder,
    ) {
        self.load = load;
        // One block per encode: a second encode before the submit overwrites it.
        self.queue.write_buffer(&self.signals_buf, 0, bytemuck::cast_slice(&list.signals.as_floats()));
        let (w, h) = (target.width(), target.height());
        let effects = self.quality == Quality::Full
            && list.layers.iter().any(|l| bloom_of(&l.target).is_some() || l.trail.is_some());
        let format = if effects { EFFECT_FORMAT } else { target.format() };
        let encode_srgb = !format.is_srgb() && !matches!(format, wgpu::TextureFormat::Rgba16Float);
        self.shapes.clear();
        self.glyphs.clear();
        self.dots.clear();
        self.glass.clear();
        self.vectors.clear();
        self.materials.clear();
        self.materials.extend(list.materials.iter().map(GpuMaterial::new));
        self.vector_segs.clear();
        self.uniforms.clear();
        self.fx_uniforms.clear();
        self.surfaces.clear();
        self.gpu_items.clear();
        self.frame += 1;
        for p in &mut self.pool {
            p.used = false;
        }
        let mut root = Surface::new();
        root.size = [w, h];
        root.bounds = [0.0, 0.0, w as f32, h as f32];
        self.surfaces.push(root);
        self.scene = None;
        self.particles.advance(&self.device, &self.queue, list);
        // An effect frame allocates its textures inside an error scope, so a failed allocation
        // drops a quality tier and redoes the frame instead of drawing with a broken texture.
        let scope = effects.then(|| self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory));
        if effects {
            self.ensure_fx();
            self.scene = Some(self.acquire(w, h, EFFECT_FORMAT));
            self.surfaces[0].tex = self.scene;
        }

        if !list.layers.is_empty() {
            let full = [0.0, 0.0, w as f32, h as f32];
            let top = LayerState {
                to_px: Affine::IDENTITY,
                ppu: 1.0,
                opacity: 1.0,
                clip: None,
                scissor: full,
                smear: [0.0; 2],
                mask_out: 0.0,
                warp: None,
            };
            let frame = Frame { list, scale, effects, encode_srgb: encode_srgb as u32 as f32, target: [w, h] };
            self.visit(&frame, 0, &top, 0);
            if let Some((r, _)) = self.particles.place_fading(0) {
                self.push_cmd(0, Pipe::Particles, r, 0, BlendMode::Normal, [0.0, 0.0, w as f32, h as f32]);
            }
        }
        self.place_surfaces(format);
        if let Some(scope) = scope {
            let mut pop = std::pin::pin!(scope.pop());
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            if let std::task::Poll::Ready(Some(e)) = std::future::Future::poll(pop.as_mut(), &mut cx) {
                eprintln!("vo-render: effect textures failed ({e}); dropping to quality {:?}", self.quality.lower());
                self.quality = self.quality.lower();
                self.pool.clear();
                return self.encode_with(list, scale, target, load, enc);
            }
        }
        self.upload(format, target.format(), w, h);
        let surfaces = &self.surfaces;
        self.particles.prepare(w, h, format, &|i| (surfaces[i].origin, surfaces[i].size));
        let mut painters = std::mem::take(&mut self.painters);
        if !self.gpu_items.is_empty() {
            painters.iter_mut().for_each(|p| p.begin_frame());
        }
        if let Some(p) = &self.profiler {
            p.begin(enc);
        }
        self.encode_passes(target, format, enc, &mut painters);
        if let Some(p) = &mut self.profiler {
            p.finish(enc);
        }
        self.painters = painters;
        self.pool.retain(|p| p.used || self.frame - p.last_used < POOL_KEEP_FRAMES);
    }

    fn visit(&mut self, f: &Frame, li: u32, parent: &LayerState, surface: usize) {
        let layer = &f.list.layers[li as usize];
        let to_px = layer.transform.then(&Affine::scale(f.scale));
        let ppu = pixels_per_unit(&to_px);
        // A turned layer (`vo_scene::stage`): bounds as laid out, then as seen.
        let warp = layer.warp.map(|w| w.scaled(f.scale)).filter(|w| w.invert().is_some());
        let seen = |b: [f32; 4]| match warp {
            Some(w) if b[2] > b[0] && b[3] > b[1] => w.bounds(b),
            _ => b,
        };
        let mut scissor = parent.scissor;
        let clip = match layer.clip {
            Some(rect) => {
                let Some(from_px) = to_px.invert() else { return };
                scissor = intersect(scissor, seen(bounds(&rect, &to_px)));
                Some(ClipState { rect, from_px, ppu, fade: layer.clip_fade })
            }
            None => parent.clip,
        };
        if scissor[2] <= scissor[0] || scissor[3] <= scissor[1] {
            return;
        }
        let in_mask = parent.mask_out != 0.0;
        let mut mask_out = parent.mask_out;
        // Where this layer's composite went, to send it through a mask found later.
        let mut composite_at = None;
        let (draw_surface, opacity, blend) = match layer.target {
            LayerTarget::Direct => (surface, parent.opacity * layer.opacity, layer.blend),
            // Inside a mask everything draws straight into the mask.
            LayerTarget::Offscreen { .. } if in_mask => (surface, parent.opacity * layer.opacity, BlendMode::Normal),
            LayerTarget::Mask(m) => {
                let masks_parent = layer
                    .parent
                    .is_some_and(|p| matches!(f.list.layers[p as usize].target, LayerTarget::Offscreen { .. }));
                if in_mask || !masks_parent || self.surfaces[surface].mask.is_some() {
                    return;
                }
                // The mask gets its surface even when it draws nothing: an empty alpha mask
                // hides its layer.
                let child = self.surfaces.len();
                self.surfaces.push(Surface { r8: true, layer: Some(li), ..Surface::new() });
                self.surfaces[surface].mask = Some((child, m));
                mask_out = if m.luma { 2.0 } else { 1.0 };
                (child, parent.opacity * layer.opacity, BlendMode::Normal)
            }
            LayerTarget::Offscreen { bloom } => {
                let opacity = parent.opacity * layer.opacity;
                if opacity <= 0.0 {
                    return;
                }
                let uniform = self.push_uniform(f, parent, opacity);
                let child = self.surfaces.len();
                let mut s = Surface { layer: Some(li), style: layer.style.is_some(), ..Surface::new() };
                if let Some(b) = bloom.filter(|b| f.effects && b.strength > 0.0) {
                    s.bloom = Some(bloom_plan(b, ppu));
                }
                if let Some(t) = layer.trail.filter(|_| f.effects) {
                    s.trail = Some(TrailPlan {
                        key: t.key,
                        half_life: t.half_life,
                        t: t.t,
                        fade: None,
                        decay: t.decay,
                        fresh: None,
                    });
                }
                self.surfaces.push(s);
                let pipe = match (&self.surfaces[child].bloom, layer.style) {
                    (Some(_), _) => Pipe::CompositeBloom,
                    (None, Some(s)) => {
                        self.set_style(uniform, &s, &to_px, ppu);
                        Pipe::CompositeStyle
                    }
                    (None, None) => Pipe::Composite,
                };
                composite_at = Some(self.surfaces[surface].cmds.len());
                self.surfaces[surface].cmds.push(Cmd {
                    pipe,
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
        // The layer's smear in target pixels: its own, through its linear part, plus its parents'.
        let mb = layer.motion_blur;
        let smear =
            [parent.smear[0] + (to_px.a * mb.x + to_px.c * mb.y), parent.smear[1] + (to_px.b * mb.x + to_px.d * mb.y)];
        let state = LayerState { to_px, ppu, opacity, clip, scissor, smear, mask_out, warp };
        // Content reaches half the smear further either way, and is seen through the warp.
        let reach = |b: [f32; 4]| {
            let (hx, hy) = (0.5 * smear[0].abs(), 0.5 * smear[1].abs());
            seen([b[0] - hx, b[1] - hy, b[2] + hx, b[3] + hy])
        };
        let uniform = self.push_uniform(f, &state, opacity);
        for item in f.list.layer_items(li) {
            let range = item.start as usize..(item.start + item.len) as usize;
            match item.kind {
                ItemKind::Shapes => {
                    let start = self.shapes.len() as u32;
                    for s in &f.list.shapes[range] {
                        if self.quality == Quality::Minimal && s.fill.is_halo() {
                            continue;
                        }
                        if let Some(mut g) = GpuShape::new(s, &to_px, ppu, opacity) {
                            if !f.effects {
                                g.clamp_glow();
                            }
                            if let Some(m) = f.list.material(s.material()).filter(|_| !s.fill.is_halo()) {
                                g.material = s.material();
                                g.reach = m.reach.max(0.0);
                            }
                            let q = g.quad(ppu);
                            let b = bounds(
                                &RRect::new(vo_scene::geom::Rect::new(q[0], q[1], q[2] - q[0], q[3] - q[1]), 0.0),
                                &to_px,
                            );
                            grow(&mut self.surfaces[draw_surface].bounds, intersect(reach(b), scissor));
                            self.shapes.push(g);
                        }
                    }
                    self.push_cmd(draw_surface, Pipe::Shape, start..self.shapes.len() as u32, uniform, blend, scissor);
                }
                ItemKind::Glyphs => {
                    let start = self.glyphs.len() as u32;
                    let fx = &f.list.glyph_fx;
                    let mut at = fx.partition_point(|(i, _)| (*i as usize) < range.start);
                    for (gi, g) in f.list.glyphs[range.clone()].iter().enumerate() {
                        // A word forming from particles fades its glyphs in as they converge.
                        let g = &DlGlyph { color: g.color.fade(self.particles.glyph_alpha(g)), ..*g };
                        let index = (range.start + gi) as u32;
                        while at < fx.len() && fx[at].0 < index {
                            at += 1;
                        }
                        let effects = (at < fx.len() && fx[at].0 == index).then(|| fx[at].1);
                        if let Some(mut inst) = self.glyph(g, &state) {
                            if let Some(e) = effects {
                                apply_glyph_fx(&mut inst, &e, &state);
                                if f.list.material(e.material).is_some() {
                                    // The id, a seed per glyph and em units per outline unit,
                                    // as floats: exact below 2^24.
                                    inst.fx2[1] = e.material as f32;
                                    inst.fx2[2] = (index % (1 << 20)) as f32;
                                    inst.fx2[3] = self.em_per_unit(g.font);
                                }
                            }
                            grow(
                                &mut self.surfaces[draw_surface].bounds,
                                intersect(reach(glyph_bounds(&inst)), scissor),
                            );
                            self.glyphs.push(inst);
                        }
                    }
                    self.push_cmd(draw_surface, Pipe::Glyph, start..self.glyphs.len() as u32, uniform, blend, scissor);
                    let words = &f.list.glyphs[range];
                    let placed = if mask_out == 0.0 {
                        self.particles.place_words(words, &to_px, opacity, draw_surface)
                    } else {
                        None
                    };
                    if let Some((r, b)) = placed {
                        grow(&mut self.surfaces[draw_surface].bounds, intersect(b, scissor));
                        self.push_cmd(draw_surface, Pipe::Particles, r, uniform, blend, scissor);
                    }
                }
                ItemKind::Vectors => {
                    let start = self.vectors.len() as u32;
                    for v in &f.list.vectors[range] {
                        if let Some(g) = GpuVector::new(f.list, v, &to_px, ppu, opacity, &mut self.vector_segs) {
                            grow(&mut self.surfaces[draw_surface].bounds, intersect(seen(g.bbox), scissor));
                            self.vectors.push(g);
                        }
                    }
                    self.push_cmd(
                        draw_surface,
                        Pipe::Vector,
                        start..self.vectors.len() as u32,
                        uniform,
                        blend,
                        scissor,
                    );
                }
                // Particles, glass and GPU items have their own pipelines: not in a mask.
                ItemKind::Emitters | ItemKind::Glass | ItemKind::Gpu if mask_out != 0.0 => {}
                ItemKind::Emitters => {
                    let mut placed: Option<Range<u32>> = None;
                    for i in range {
                        let at = self.particles.place_emitter(f.list, i, &to_px, opacity, draw_surface);
                        if let Some((r, b)) = at {
                            grow(&mut self.surfaces[draw_surface].bounds, intersect(b, scissor));
                            placed = Some(placed.map_or(r.clone(), |p| p.start..r.end));
                        }
                    }
                    if let Some(r) = placed {
                        self.push_cmd(draw_surface, Pipe::Particles, r, uniform, blend, scissor);
                    }
                }
                ItemKind::Dots => {
                    let start = self.dots.len() as u32;
                    let mut b = EMPTY;
                    for d in &f.list.dots[range] {
                        if let Some(g) = GpuDot::new(d, &to_px, ppu, opacity) {
                            let r = g.radius + 1.0 / ppu;
                            let [x, y] = g.centre;
                            grow(
                                &mut b,
                                bounds(
                                    &RRect::new(vo_scene::geom::Rect::new(x - r, y - r, 2.0 * r, 2.0 * r), 0.0),
                                    &to_px,
                                ),
                            );
                            self.dots.push(g);
                        }
                    }
                    grow(&mut self.surfaces[draw_surface].bounds, intersect(seen(b), scissor));
                    self.push_cmd(draw_surface, Pipe::Dot, start..self.dots.len() as u32, uniform, blend, scissor);
                }
                ItemKind::Glass => {
                    let start = self.glass.len() as u32;
                    let mut b = EMPTY;
                    for g in &f.list.glass[range] {
                        let [x0, y0, x1, y1] = GlassGpu::reach(g);
                        let r = vo_scene::geom::Rect::new(x0, y0, x1 - x0, y1 - y0);
                        grow(&mut b, bounds(&RRect::new(r, 0.0), &to_px));
                        // The minimal tier draws no drop shadow, glass included.
                        let mut g = *g;
                        if self.quality == Quality::Minimal {
                            g.glass.shadow[0] = 0.0;
                        }
                        self.glass.push(GlassGpu::new(&g, opacity));
                    }
                    grow(&mut self.surfaces[draw_surface].bounds, intersect(seen(b), scissor));
                    self.push_cmd(draw_surface, Pipe::Glass, start..self.glass.len() as u32, uniform, blend, scissor);
                }
                ItemKind::Gpu => {
                    let start = self.gpu_items.len() as u32;
                    for g in &f.list.gpu[range] {
                        let b = seen(bounds(&RRect::new(g.rect, 0.0), &to_px));
                        grow(&mut self.surfaces[draw_surface].bounds, intersect(b, scissor));
                        let o = f.list.stage.origin_in(f.list.size);
                        let stage = StagePx {
                            origin: [o.x * f.scale, o.y * f.scale],
                            perspective: f.list.stage.perspective * f.scale,
                        };
                        self.gpu_items.push(GpuCmd {
                            item: g.clone(),
                            to_px,
                            warp,
                            stage,
                            opacity: opacity * g.opacity,
                        });
                    }
                    let end = self.gpu_items.len() as u32;
                    self.push_cmd(draw_surface, Pipe::Gpu, start..end, uniform, BlendMode::Normal, scissor);
                }
                ItemKind::Layer => self.visit(f, item.start, &state, draw_surface),
            }
        }
        if draw_surface != surface {
            if let Some((mask, _)) = self.surfaces[draw_surface].mask {
                // A masked layer draws no bloom: put the bloom on a layer around it. An empty
                // inverted mask hides nothing, so the plain composite stays.
                self.surfaces[draw_surface].bloom = None;
                if let Some(ci) = composite_at {
                    let drew = self.surfaces[mask].size[0] != 0;
                    self.surfaces[surface].cmds[ci].pipe = if drew { Pipe::CompositeMask } else { Pipe::Composite };
                }
            }
            self.close_surface(draw_surface, surface, parent.scissor, f.target);
        }
    }

    /// Sizes an offscreen layer's texture from what it drew, and adds the part its parent
    /// shows to the parent's bounds.
    fn close_surface(&mut self, child: usize, parent: usize, parent_scissor: [f32; 4], target: [u32; 2]) {
        // An alpha mask hides everything outside what it draws, so the layer needs no texture
        // there.
        let mask_bounds = match self.surfaces[child].mask {
            Some((m, mask)) if !mask.invert => Some(self.surfaces[m].bounds),
            _ => None,
        };
        let s = &mut self.surfaces[child];
        let b = match mask_bounds {
            Some(m) if s.trail.is_none() => intersect(s.bounds, m),
            _ => s.bounds,
        };
        let [w, h] = if s.trail.is_some() {
            // A trail keeps what earlier frames drew anywhere, so it covers the whole target
            // even when nothing draws this frame.
            s.origin = [0, 0];
            target
        } else {
            if b[2] <= b[0] || b[3] <= b[1] {
                s.size = [0, 0];
                return;
            }
            let pad = s.bloom.as_ref().map_or(0.0, |p| (3 << p.levels) as f32);
            // Content past the target only matters through the bloom's reach.
            let limit = [-pad, -pad, target[0] as f32 + pad, target[1] as f32 + pad];
            let b = intersect([b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad], limit);
            let (x0, y0) = (b[0].floor() as i32, b[1].floor() as i32);
            s.origin = [x0, y0];
            [
                ((b[2].ceil() as i32 - x0).max(1) as u32).next_multiple_of(ALIGN),
                ((b[3].ceil() as i32 - y0).max(1) as u32).next_multiple_of(ALIGN),
            ]
        };
        s.size = [w, h];
        if let Some(p) = &mut s.bloom {
            // Keep the smallest level at least 4 pixels.
            let fit = (w.min(h) / 4).max(1).ilog2().max(1);
            if p.levels > fit {
                (p.levels, p.top) = (fit, 1.0);
            }
        }
        if s.r8 {
            // A mask is read by its layer's composite, not drawn into the parent.
            return;
        }
        let shown = intersect(s.rect(), parent_scissor);
        grow(&mut self.surfaces[parent].bounds, shown);
    }

    /// Writes a layer style (FX13) into the composite's layer block at `uniform`, its box and
    /// lengths in target pixels.
    fn set_style(&mut self, uniform: u32, s: &vo_scene::display::LayerStyle, to_px: &Affine, ppu: f32) {
        let size = std::mem::size_of::<LayerUniform>();
        let at = uniform as usize;
        let mut u: LayerUniform = bytemuck::pod_read_unaligned(&self.uniforms[at..at + size]);
        u.style_box = bounds(&RRect::new(s.rect, 0.0), to_px);
        u.style_a = [s.chroma * ppu, s.glitch, s.scanlines, s.scan_period * ppu];
        u.style_b = [s.curvature, s.vignette, 1.0 - s.flicker, s.glitch_step as f32];
        self.uniforms[at..at + size].copy_from_slice(bytemuck::bytes_of(&u));
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

    /// Em units per glyph outline unit, for materials keyed to em height (FX15).
    fn em_per_unit(&self, font: u32) -> f32 {
        if font == PATH_FONT {
            return 1.0 / slug_gpu::PATH_UNITS as f32;
        }
        self.faces.get(font as usize).map_or(1.0, |f| 1.0 / f.units_per_em() as f32)
    }

    fn glyph(&mut self, g: &DlGlyph, st: &LayerState) -> Option<GlyphInstance> {
        let color = linear_premul(g.color, st.opacity);
        if color[3] <= 0.0 {
            return None;
        }
        if g.font == PATH_FONT {
            let fonts = &self.fonts;
            let entry = self.atlas.path_in(g.font, g.glyph, || fonts.path(g.glyph))?;
            let k = g.size / slug_gpu::PATH_UNITS as f32;
            let m = &st.to_px;
            let lin = [m.a * k, m.b * k, -m.c * k, -m.d * k];
            let o = m.apply(g.pos);
            return Some(GlyphInstance::with_matrix(&entry, lin, [o.x, o.y], color, g.word));
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

    /// Appends a layer block and returns its byte offset. The texture size and origin are
    /// filled in by [`Renderer::place_surfaces`] once every surface is sized.
    fn push_uniform(&mut self, f: &Frame, st: &LayerState, opacity: f32) -> u32 {
        let m = &st.to_px;
        // The smear back in layer units, through the inverse of the linear part.
        let det = m.a * m.d - m.b * m.c;
        let [sx, sy] = st.smear;
        let smear = if (sx != 0.0 || sy != 0.0) && det != 0.0 {
            [(m.d * sx - m.c * sy) / det, (m.a * sy - m.b * sx) / det, sx, sy]
        } else {
            [0.0; 4]
        };
        let rows = |w: &Warp, flag: f32| {
            let m = w.0;
            [[m[0][0], m[0][1], m[0][2], flag], [m[1][0], m[1][1], m[1][2], 0.0], [m[2][0], m[2][1], m[2][2], 0.0]]
        };
        let (warp, unwarp) = match st.warp.and_then(|w| Some((w, w.invert()?))) {
            Some((w, inv)) => (rows(&w, 1.0), rows(&inv, 0.0)),
            None => ([[0.0; 4]; 3], [[0.0; 4]; 3]),
        };
        let mut u = LayerUniform {
            warp0: warp[0],
            warp1: warp[1],
            warp2: warp[2],
            unwarp0: unwarp[0],
            unwarp1: unwarp[1],
            unwarp2: unwarp[2],
            smear,
            m: [m.a, m.b, m.c, m.d],
            t: [m.e, m.f, st.ppu, opacity],
            viewport: [0.0, 0.0, f.encode_srgb, st.mask_out],
            ..Default::default()
        };
        if let Some(c) = &st.clip {
            let (r, k) = (c.rect.rect, &c.from_px);
            u.clip_rect = [r.x, r.y, r.w, r.h];
            u.clip_radii = c.rect.radii;
            u.clip_m = [k.a, k.b, k.c, k.d];
            u.clip_t = [k.e, k.f, c.ppu, 1.0];
            u.clip_fade = c.fade;
        }
        let offset = self.uniforms.len() as u32;
        self.uniforms.extend_from_slice(bytemuck::bytes_of(&u));
        self.uniforms.resize((offset + self.uniform_stride) as usize, 0);
        offset
    }

    /// Writes each surface's texture size and origin into the layer blocks of its draws,
    /// and takes its textures from the pool.
    fn place_surfaces(&mut self, format: wgpu::TextureFormat) {
        let size = std::mem::size_of::<LayerUniform>();
        for si in 0..self.surfaces.len() {
            let (origin, tex_size) = (self.surfaces[si].origin, self.surfaces[si].size);
            for ci in 0..self.surfaces[si].cmds.len() {
                let c = &self.surfaces[si].cmds[ci];
                let at = c.uniform as usize;
                let mut u: LayerUniform = bytemuck::pod_read_unaligned(&self.uniforms[at..at + size]);
                u.viewport[0] = tex_size[0] as f32;
                u.viewport[1] = tex_size[1] as f32;
                u.origin = [origin[0] as f32, origin[1] as f32, 0.0, 0.0];
                if matches!(c.pipe, Pipe::Composite | Pipe::CompositeBloom | Pipe::CompositeMask | Pipe::CompositeStyle)
                {
                    let child = &self.surfaces[c.range.start as usize];
                    if let (Pipe::CompositeMask, Some((m, mask))) = (c.pipe, child.mask) {
                        let o = self.surfaces[m].origin;
                        u.mask = [o[0] as f32, o[1] as f32, mask.invert as u32 as f32, 0.0];
                    }
                    u.origin[2] = child.origin[0] as f32;
                    u.origin[3] = child.origin[1] as f32;
                    u.src = [
                        child.size[0] as f32,
                        child.size[1] as f32,
                        child.bloom.as_ref().map_or(0.0, |b| b.strength),
                        self.light as u32 as f32,
                    ];
                }
                self.uniforms[at..at + size].copy_from_slice(bytemuck::bytes_of(&u));
            }
        }
        for si in 1..self.surfaces.len() {
            let [w, h] = self.surfaces[si].size;
            if w == 0 {
                continue;
            }
            let tex = match &self.surfaces[si].trail {
                Some(t) => {
                    let (tex, fade) = self.acquire_trail(t.key, t.half_life, t.t, w, h, format);
                    let fresh = fade.map(|_| self.acquire(w, h, format));
                    let plan = self.surfaces[si].trail.as_mut().unwrap();
                    (plan.fade, plan.fresh) = (fade, fresh);
                    tex
                }
                None => self.acquire(w, h, self.surfaces[si].format(format)),
            };
            self.surfaces[si].tex = Some(tex);
            if self.surfaces[si].bloom.is_none() {
                continue;
            }
            let levels = self.surfaces[si].bloom.as_ref().unwrap().levels;
            let mut down = Vec::new();
            let mut up = Vec::new();
            for l in 1..=levels {
                down.push(self.acquire(w >> l, h >> l, EFFECT_FORMAT));
            }
            for l in 1..levels {
                up.push(self.acquire(w >> l, h >> l, EFFECT_FORMAT));
            }
            let top = self.surfaces[si].bloom.as_ref().unwrap().top;
            let mut fx = Vec::new();
            for l in 1..=levels {
                let src = [w >> (l - 1), h >> (l - 1)];
                fx.push(self.push_fx(FxUniform {
                    texel: [1.0 / src[0] as f32, 1.0 / src[1] as f32],
                    weight: 0.0,
                    mode: (l == 1) as u32 as f32,
                }));
            }
            for l in 1..levels {
                // The lower level read by the tent.
                let low = [w >> (l + 1), h >> (l + 1)];
                let weight = if l == levels - 1 { BLOOM_MIX * top } else { BLOOM_MIX };
                fx.push(self.push_fx(FxUniform {
                    texel: [1.0 / low[0] as f32, 1.0 / low[1] as f32],
                    weight,
                    mode: 0.0,
                }));
            }
            let plan = self.surfaces[si].bloom.as_mut().unwrap();
            (plan.down, plan.up, plan.fx) = (down, up, fx);
        }
    }

    fn push_fx(&mut self, u: FxUniform) -> u32 {
        let offset = self.fx_uniforms.len() as u32;
        self.fx_uniforms.extend_from_slice(bytemuck::bytes_of(&u));
        self.fx_uniforms.resize((offset + self.uniform_stride) as usize, 0);
        offset
    }

    /// A pooled texture of this size and format not yet used this frame.
    fn acquire(&mut self, w: u32, h: u32, format: wgpu::TextureFormat) -> usize {
        let (w, h) = (w.max(1), h.max(1));
        let frame = self.frame;
        if let Some(i) = self.pool.iter().position(|p| {
            !p.used
                && p.key.is_none()
                && p.texture.width() == w
                && p.texture.height() == h
                && p.texture.format() == format
        }) {
            (self.pool[i].used, self.pool[i].last_used) = (true, frame);
            return i;
        }
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
        self.pool.push(PoolTex { texture, view, bind, used: true, last_used: frame, key: None, t: 0.0 });
        self.pool.len() - 1
    }

    /// The texture of trail `key`, and how much of last frame's pixels it keeps: `None` when
    /// the trail starts fresh. A trail whose size or format changed lets its old texture go
    /// back to the pool.
    fn acquire_trail(
        &mut self,
        key: u64,
        half_life: f32,
        t: f64,
        w: u32,
        h: u32,
        format: wgpu::TextureFormat,
    ) -> (usize, Option<f32>) {
        let frame = self.frame;
        if let Some(i) = self.pool.iter().position(|p| p.key == Some(key)) {
            let p = &mut self.pool[i];
            if p.texture.width() == w && p.texture.height() == h && p.texture.format() == format {
                let dt = t - p.t;
                let fade = (0.0..=TRAIL_MAX_GAP).contains(&dt).then(|| 0.5f32.powf((dt / half_life as f64) as f32));
                (p.used, p.last_used, p.t) = (true, frame, t);
                return (i, fade);
            }
            p.key = None;
        }
        let i = self.acquire(w, h, format);
        (self.pool[i].key, self.pool[i].t) = (Some(key), t);
        (i, None)
    }

    fn ensure_fx(&mut self) {
        if self.fx.is_some() {
            return;
        }
        let d = &self.device;
        let sampled_layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-fx-sampled"),
            entries: &[
                texture_entry(0, true),
                texture_entry(1, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let fx_layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-fx"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<FxUniform>() as u64),
                },
                count: None,
            }],
        });
        let sampler = d.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vo-fx"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        self.fx = Some(FxShared { sampled_layout, fx_layout, sampler });
    }

    fn upload(&mut self, format: wgpu::TextureFormat, target_format: wgpu::TextureFormat, w: u32, h: u32) {
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
        write_grow(
            device,
            queue,
            &mut self.dot_buf,
            "vo-dots",
            wgpu::BufferUsages::VERTEX,
            bytemuck::cast_slice(&self.dots),
        );
        write_grow(
            device,
            queue,
            &mut self.glass_buf,
            "vo-glass",
            wgpu::BufferUsages::VERTEX,
            bytemuck::cast_slice(&self.glass),
        );
        write_grow(
            device,
            queue,
            &mut self.vector_buf,
            "vo-vectors",
            wgpu::BufferUsages::VERTEX,
            bytemuck::cast_slice(&self.vectors),
        );
        let need = (self.vector_segs.len() * 16).max(256) as u64;
        if self.vector_segs_buf.as_ref().is_none_or(|(b, _)| b.size() < need) {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vo-vector-segs"),
                size: need.next_power_of_two(),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vo-vector-segs"),
                layout: &self.vector_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() }],
            });
            self.vector_segs_buf = Some((buf, bind));
        }
        if !self.vector_segs.is_empty() {
            queue.write_buffer(&self.vector_segs_buf.as_ref().unwrap().0, 0, bytemuck::cast_slice(&self.vector_segs));
        }
        let need = (self.materials.len() * std::mem::size_of::<GpuMaterial>()).max(256) as u64;
        if self.material_buf.as_ref().is_none_or(|(b, _)| b.size() < need) {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vo-materials"),
                size: need.next_power_of_two(),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vo-materials"),
                layout: &self.material_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() }],
            });
            self.material_buf = Some((buf, bind));
        }
        if !self.materials.is_empty() {
            queue.write_buffer(&self.material_buf.as_ref().unwrap().0, 0, bytemuck::cast_slice(&self.materials));
        }
        write_uniforms(
            device,
            queue,
            &mut self.uniform_buf,
            "vo-layers",
            &self.layer_layout,
            std::mem::size_of::<LayerUniform>(),
            &self.uniforms,
            self.uniform_stride,
        );
        if let Some(fx) = &self.fx {
            if self.scene.is_some() {
                // The final pass's block goes last.
                let mode = if target_format == EFFECT_FORMAT {
                    0.0
                } else if target_format.is_srgb() {
                    1.0
                } else {
                    2.0
                };
                let offset = self.fx_uniforms.len();
                self.fx_uniforms.extend_from_slice(bytemuck::bytes_of(&FxUniform { mode, ..Default::default() }));
                self.fx_uniforms.resize(offset + self.uniform_stride as usize, 0);
                write_uniforms(
                    device,
                    queue,
                    &mut self.fx_buf,
                    "vo-fx",
                    &fx.fx_layout,
                    std::mem::size_of::<FxUniform>(),
                    &self.fx_uniforms,
                    self.uniform_stride,
                );
            }
        }
        // Pipelines this frame needs.
        let mut needed: Vec<(wgpu::TextureFormat, Pipe, BlendMode)> = Vec::new();
        for s in &self.surfaces {
            for c in s.cmds.iter().filter(|c| !matches!(c.pipe, Pipe::Particles | Pipe::Gpu)) {
                let key = (s.format(format), c.pipe, c.blend);
                if !needed.contains(&key) {
                    needed.push(key);
                }
            }
            if s.trail.as_ref().is_some_and(|t| t.fade.is_some_and(|k| k < 1.0)) {
                needed.push((format, Pipe::Fade, BlendMode::Normal));
            }
            if s.trail.as_ref().is_some_and(|t| t.fresh.is_some()) {
                needed.push((format, Pipe::TrailMax, BlendMode::Normal));
            }
            if s.bloom.is_some() {
                needed.push((EFFECT_FORMAT, Pipe::Down, BlendMode::Normal));
                needed.push((EFFECT_FORMAT, Pipe::Up, BlendMode::Normal));
            }
        }
        if self.scene.is_some() {
            needed.push((target_format, Pipe::Final, BlendMode::Normal));
        }
        for (format, pipe, blend) in needed {
            self.pipeline(format, pipe, blend);
        }
    }

    fn encode_passes(
        &self,
        target: &Target,
        format: wgpu::TextureFormat,
        enc: &mut wgpu::CommandEncoder,
        painters: &mut [Box<dyn GpuPainter>],
    ) {
        // Surfaces are numbered in pre-order, so a child always has a higher number than its
        // parent: going from the last to the first draws every layer before it is composited.
        for i in (0..self.surfaces.len()).rev() {
            let s = &self.surfaces[i];
            if s.size[0] == 0 {
                continue;
            }
            let view = match s.tex {
                Some(t) => &self.pool[t].view,
                None => &target.view,
            };
            // A trail that goes on draws this frame on its own, then merges it (below).
            let merge = s.trail.as_ref().and_then(|t| Some((t.fade?, t.fresh?)));
            let trail_view = view;
            let view = match merge {
                Some((_, f)) => &self.pool[f].view,
                None => view,
            };
            let mut pass = match merge {
                Some(_) => begin(enc, view, "vo-trail-frame"),
                // The target itself takes the caller's load op; the effect path's scene
                // texture and every layer texture clear.
                None if s.tex.is_none() => begin_with(enc, view, "vo-target", self.load),
                None => begin(enc, view, if i == 0 { "vo-scene" } else { "vo-layer" }),
            };
            // A pass break ends the pass, runs outside it, and resumes on what is there.
            let mut cmds = &s.cmds[..];
            loop {
                let split = cmds.iter().position(|c| c.pipe.breaks_pass()).unwrap_or(cmds.len());
                self.run(&mut pass, s, &cmds[..split], s.format(format));
                if split == cmds.len() {
                    break;
                }
                drop(pass);
                self.run_break(enc, view, s, &cmds[split], format, painters);
                pass = begin_load(enc, view, "vo-resume");
                cmds = &cmds[split + 1..];
            }
            drop(pass);
            if let Some((k, fresh)) = merge {
                let mut pass = begin_load(enc, trail_view, "vo-trail");
                if k < 1.0 {
                    pass.set_pipeline(self.cached_pipeline(format, Pipe::Fade, BlendMode::Normal));
                    // Each channel fades by its own half life; alpha with the slowest.
                    let d = s.trail.as_ref().map_or([1.0; 3], |t| t.decay).map(|d| d.max(1e-3) as f64);
                    let c = |d: f64| (k as f64).powf(1.0 / d);
                    let a = c(d[0].max(d[1]).max(d[2]));
                    pass.set_blend_constant(wgpu::Color { r: c(d[0]), g: c(d[1]), b: c(d[2]), a });
                    pass.draw(0..3, 0..1);
                }
                // The larger of the afterglow and the new frame, per channel: content that holds
                // still keeps its brightness, and what moved away fades.
                pass.set_pipeline(self.cached_pipeline(format, Pipe::TrailMax, BlendMode::Normal));
                pass.set_bind_group(0, &self.pool[fresh].bind, &[]);
                pass.draw(0..3, 0..1);
            }
            self.mark(enc, Section::Draw { surface: i });
            if let Some(b) = &s.bloom {
                self.encode_bloom(enc, s, b);
                self.mark(enc, Section::Bloom { surface: i });
            }
        }
        if let (Some(scene), Some(fx)) = (self.scene, &self.fx) {
            let bind = self.sampled_bind(fx, scene, scene);
            let mut pass = begin_with(enc, &target.view, "vo-final", self.load);
            pass.set_pipeline(self.cached_pipeline(target.format(), Pipe::Final, BlendMode::Normal));
            pass.set_bind_group(0, &bind, &[]);
            let offset = (self.fx_uniforms.len() - self.uniform_stride as usize) as u32;
            pass.set_bind_group(1, &self.fx_buf.as_ref().unwrap().1, &[offset]);
            pass.draw(0..3, 0..1);
            drop(pass);
            self.mark(enc, Section::Final);
        }
    }

    fn mark(&self, enc: &mut wgpu::CommandEncoder, section: Section) {
        if let Some(p) = &self.profiler {
            p.mark(enc, section);
        }
    }

    fn encode_bloom(&self, enc: &mut wgpu::CommandEncoder, s: &Surface, b: &BloomPlan) {
        let fx = self.fx.as_ref().expect("effect frame");
        let fx_bind = &self.fx_buf.as_ref().expect("uploaded").1;
        let pass = |enc: &mut wgpu::CommandEncoder, dst: usize, src: usize, low: usize, pipe: Pipe, fx_offset: u32| {
            let bind = self.sampled_bind(fx, src, low);
            let mut p = begin(enc, &self.pool[dst].view, "vo-bloom");
            p.set_pipeline(self.cached_pipeline(EFFECT_FORMAT, pipe, BlendMode::Normal));
            p.set_bind_group(0, &bind, &[]);
            p.set_bind_group(1, fx_bind, &[fx_offset]);
            p.draw(0..3, 0..1);
        };
        let layer = s.tex.expect("sized");
        let n = b.levels as usize;
        for l in 0..n {
            let src = if l == 0 { layer } else { b.down[l - 1] };
            pass(enc, b.down[l], src, src, Pipe::Down, b.fx[l]);
        }
        for l in (0..n - 1).rev() {
            let low = if l == n - 2 { b.down[n - 1] } else { b.up[l + 1] };
            pass(enc, b.up[l], b.down[l], low, Pipe::Up, b.fx[n + l]);
        }
    }

    fn sampled_bind(&self, fx: &FxShared, src: usize, low: usize) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-fx"),
            layout: &fx.sampled_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&self.pool[src].view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&self.pool[low].view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&fx.sampler) },
            ],
        })
    }

    /// A pass break command: the encoder is free and the surface's pass has ended.
    fn run_break(
        &self,
        enc: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        surface: &Surface,
        c: &Cmd,
        format: wgpu::TextureFormat,
        painters: &mut [Box<dyn GpuPainter>],
    ) {
        match c.pipe {
            Pipe::Gpu => {
                let Some(scissor) = surface_scissor(c.scissor, surface) else { return };
                let [ox, oy] = surface.origin.map(|v| v as f32);
                let encode_srgb = !format.is_srgb() && format != wgpu::TextureFormat::Rgba16Float;
                for g in &self.gpu_items[c.range.start as usize..c.range.end as usize] {
                    let mut cx = PaintCx {
                        device: &self.device,
                        queue: &self.queue,
                        enc: &mut *enc,
                        view,
                        format,
                        size: surface.size,
                        source: &*g.item.source.0,
                        rect: g.item.rect,
                        t: g.item.t,
                        to_px: g.to_px.then(&Affine::translate(-ox, -oy)),
                        warp: g.warp.map(|w| {
                            Warp([[1.0, 0.0, ox], [0.0, 1.0, oy], [0.0, 0.0, 1.0]]).then(&w).then(&Warp([
                                [1.0, 0.0, -ox],
                                [0.0, 1.0, -oy],
                                [0.0, 0.0, 1.0],
                            ]))
                        }),
                        stage: StagePx { origin: [g.stage.origin[0] - ox, g.stage.origin[1] - oy], ..g.stage },
                        opacity: g.opacity,
                        scissor,
                        encode_srgb,
                    };
                    for p in painters.iter_mut() {
                        if p.paint(&mut cx) {
                            break;
                        }
                    }
                }
            }
            _ => unreachable!("not a pass break"),
        }
    }

    fn run(&self, pass: &mut wgpu::RenderPass<'_>, surface: &Surface, cmds: &[Cmd], format: wgpu::TextureFormat) {
        let layers = &self.uniform_buf.as_ref().expect("uploaded").1;
        for c in cmds {
            let mut sc = c.scissor;
            let child =
                matches!(c.pipe, Pipe::Composite | Pipe::CompositeBloom | Pipe::CompositeMask | Pipe::CompositeStyle)
                    .then(|| &self.surfaces[c.range.start as usize]);
            if let Some(child) = child {
                if child.size[0] == 0 {
                    continue;
                }
                sc = intersect(sc, child.rect());
            }
            let Some([x, y, w, h]) = surface_scissor(sc, surface) else { continue };
            pass.set_scissor_rect(x, y, w, h);
            if c.pipe == Pipe::Particles {
                self.particles.draw(pass, c.range.clone());
                continue;
            }
            pass.set_pipeline(self.cached_pipeline(format, c.pipe, c.blend));
            pass.set_bind_group(1, layers, &[c.uniform]);
            pass.set_bind_group(3, &self.signals_bind, &[]);
            if matches!(c.pipe, Pipe::Shape | Pipe::Glyph | Pipe::Vector) {
                pass.set_bind_group(2, &self.material_buf.as_ref().expect("uploaded").1, &[]);
            }
            match c.pipe {
                Pipe::Shape => {
                    pass.set_vertex_buffer(0, self.shape_buf.as_ref().unwrap().slice(..));
                    pass.draw(0..4, c.range.clone());
                }
                Pipe::Dot => {
                    pass.set_vertex_buffer(0, self.dot_buf.as_ref().unwrap().slice(..));
                    pass.draw(0..4, c.range.clone());
                }
                Pipe::Glass => {
                    pass.set_vertex_buffer(0, self.glass_buf.as_ref().unwrap().slice(..));
                    pass.draw(0..4, c.range.clone());
                }
                Pipe::Vector => {
                    pass.set_bind_group(0, &self.vector_segs_buf.as_ref().unwrap().1, &[]);
                    pass.set_vertex_buffer(0, self.vector_buf.as_ref().unwrap().slice(..));
                    pass.draw(0..4, c.range.clone());
                }
                Pipe::Glyph => {
                    let Some(atlas) = self.atlas_gpu.bind_group() else { continue };
                    pass.set_bind_group(0, atlas, &[]);
                    pass.set_vertex_buffer(0, self.glyph_buf.as_ref().unwrap().slice(..));
                    pass.draw(0..4, c.range.clone());
                }
                Pipe::Composite | Pipe::CompositeStyle => {
                    let child = child.unwrap();
                    pass.set_bind_group(0, &self.pool[child.tex.unwrap()].bind, &[]);
                    pass.draw(0..3, 0..1);
                }
                Pipe::CompositeBloom => {
                    let child = child.unwrap();
                    let fx = self.fx.as_ref().expect("effect frame");
                    let bloom = child.bloom.as_ref().unwrap().result();
                    let bind = self.sampled_bind(fx, child.tex.unwrap(), bloom);
                    pass.set_bind_group(0, &bind, &[]);
                    pass.draw(0..3, 0..1);
                }
                Pipe::CompositeMask => {
                    let child = child.unwrap();
                    let mask = &self.surfaces[child.mask.expect("masked").0];
                    let view = |s: &Surface| wgpu::BindingResource::TextureView(&self.pool[s.tex.unwrap()].view);
                    let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("vo-composite-mask"),
                        layout: &self.mask_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: view(child) },
                            wgpu::BindGroupEntry { binding: 1, resource: view(mask) },
                        ],
                    });
                    pass.set_bind_group(0, &bind, &[]);
                    pass.draw(0..3, 0..1);
                }
                Pipe::Down | Pipe::Up | Pipe::Final | Pipe::Fade | Pipe::TrailMax => {
                    unreachable!("not a layer command")
                }
                Pipe::Particles => unreachable!("drawn above"),
                Pipe::Gpu => unreachable!("a pass break"),
            }
        }
    }

    fn cached_pipeline(&self, format: wgpu::TextureFormat, pipe: Pipe, blend: BlendMode) -> &wgpu::RenderPipeline {
        let key = (format, pipe, blend);
        &self.pipelines.iter().find(|(k, _)| *k == key).expect("created in upload").1
    }

    /// The shader module for a module slot, from the current sources (see `hot`).
    fn shader_module(&self, slot: usize) -> wgpu::ShaderModule {
        let (label, source) = match slot {
            0 => ("vo-shapes", shape_shader_source_with(&self.material_fns)),
            1 => ("vo-glyphs", glyph_shader_source_with(&self.material_fns)),
            2 => ("vo-composite", composite_shader_source()),
            4 => ("vo-dots", dot_shader_source()),
            5 => ("vo-vectors", vector_shader_source_with(&self.material_fns)),
            7 => ("vo-glass", glass_layer_shader_source()),
            8 => ("vo-trail", TRAIL_WGSL.to_string()),
            _ => ("vo-bloom", bloom_shader_source()),
        };
        self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        })
    }

    /// Builds every shader module made so far again from the current sources (`hot`), inside
    /// a validation error scope. On success the new modules replace the old ones and the
    /// pipeline cache is cleared, so the next frame uses them; on error the old modules and
    /// pipelines stay and the validation message comes back. Returns the number of modules.
    pub fn reload_shaders(&mut self) -> Result<usize, String> {
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let fresh: Vec<(usize, wgpu::ShaderModule)> = (0..self.modules.len())
            .filter(|i| self.modules[*i].is_some())
            .map(|i| (i, self.shader_module(i)))
            .collect();
        // Native wgpu reports the scope at once, as in `encode`.
        let mut pop = std::pin::pin!(scope.pop());
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        if let std::task::Poll::Ready(Some(e)) = std::future::Future::poll(pop.as_mut(), &mut cx) {
            return Err(e.to_string());
        }
        let n = fresh.len();
        for (i, m) in fresh {
            self.modules[i] = Some(m);
        }
        self.pipelines.clear();
        Ok(n)
    }
    /// Creates the pipeline for `format`, `pipe` and `blend` on first use.
    fn pipeline(&mut self, format: wgpu::TextureFormat, pipe: Pipe, blend: BlendMode) {
        let key = (format, pipe, blend);
        if self.pipelines.iter().any(|(k, _)| *k == key) {
            return;
        }
        let slot = match pipe {
            Pipe::Shape => 0,
            Pipe::Glyph => 1,
            Pipe::Composite | Pipe::CompositeBloom | Pipe::CompositeMask | Pipe::CompositeStyle => 2,
            Pipe::Down | Pipe::Up | Pipe::Final | Pipe::Fade => 3,
            Pipe::TrailMax => 8,
            // The particle layer and the painters have their own pipelines.
            Pipe::Particles | Pipe::Gpu => return,
            Pipe::Dot => 4,
            Pipe::Vector => 5,
            Pipe::Glass => 7,
        };
        if self.modules[slot].is_none() {
            self.modules[slot] = Some(self.shader_module(slot));
        }
        let module = self.modules[slot].as_ref().unwrap();
        let fx = self.fx.as_ref();
        let groups: [Option<&wgpu::BindGroupLayout>; 2] = match pipe {
            Pipe::Shape => [None, Some(&self.layer_layout)],
            Pipe::Dot | Pipe::Glass => [None, Some(&self.layer_layout)],
            Pipe::Vector => [Some(&self.vector_layout), Some(&self.layer_layout)],
            Pipe::Glyph => [Some(self.atlas_gpu.layout()), Some(&self.layer_layout)],
            Pipe::Composite | Pipe::CompositeStyle => [Some(&self.composite_layout), Some(&self.layer_layout)],
            Pipe::CompositeBloom => [fx.map(|f| &f.sampled_layout), Some(&self.layer_layout)],
            Pipe::CompositeMask => [Some(&self.mask_layout), Some(&self.layer_layout)],
            Pipe::Down | Pipe::Up | Pipe::Final => [fx.map(|f| &f.sampled_layout), fx.map(|f| &f.fx_layout)],
            Pipe::Fade => [None, None],
            Pipe::TrailMax => [Some(&self.composite_layout), None],
            Pipe::Particles | Pipe::Gpu => unreachable!("returned above"),
        };
        // The signal block is group 3 of every pipeline that draws into a layer.
        let signals = groups[1].filter(|l| std::ptr::eq(*l, &self.layer_layout)).map(|_| &self.signals_layout);
        // Materials (FX15) are group 2 of the content pipelines.
        let with_materials = [groups[0], groups[1], Some(&self.material_layout), signals];
        let groups = [groups[0], groups[1], None, signals];
        let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: if matches!(pipe, Pipe::Shape | Pipe::Glyph | Pipe::Vector) {
                &with_materials
            } else {
                &groups
            },
            immediate_size: 0,
        });
        let (vs, fs, buffers) = match pipe {
            Pipe::Shape => ("vs_shape", "fs_shape", vec![Some(GpuShape::layout())]),
            Pipe::Glyph => ("vs_dl_glyph", "fs_dl_glyph", vec![Some(GlyphInstance::layout())]),
            Pipe::Composite => ("vs_composite", "fs_composite", vec![]),
            Pipe::Dot => ("vs_dot", "fs_dot", vec![Some(GpuDot::layout())]),
            Pipe::Glass => ("vs_glass", "fs_glass", vec![Some(GlassGpu::layout())]),
            Pipe::Vector => ("vs_vector", "fs_vector", vec![Some(GpuVector::layout())]),
            Pipe::CompositeBloom => ("vs_composite", "fs_composite_bloom", vec![]),
            Pipe::CompositeMask => ("vs_composite", "fs_composite_mask", vec![]),
            Pipe::CompositeStyle => ("vs_composite", "fs_composite_style", vec![]),
            Pipe::Down => ("vs_full", "fs_down", vec![]),
            Pipe::Up => ("vs_full", "fs_up", vec![]),
            Pipe::Final => ("vs_full", "fs_final", vec![]),
            Pipe::Fade => ("vs_full", "fs_fade", vec![]),
            Pipe::TrailMax => ("vs_trail", "fs_trail_max", vec![]),
            Pipe::Particles | Pipe::Gpu => unreachable!("returned above"),
        };
        let blend_state = match pipe {
            Pipe::Down | Pipe::Up => None,
            // Over a cleared target this equals replacing; over a loaded one it composites.
            Pipe::Final => Some(PREMULTIPLIED_OVER),
            // Keeps `constant` times what is there: the source is zero.
            Pipe::Fade => Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::Zero,
                    dst_factor: wgpu::BlendFactor::Constant,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::Zero,
                    dst_factor: wgpu::BlendFactor::Constant,
                    operation: wgpu::BlendOperation::Add,
                },
            }),
            Pipe::TrailMax => Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Max,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Max,
                },
            }),
            _ => Some(blend_state(blend)),
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
                    blend: blend_state,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: match pipe {
                    Pipe::Shape | Pipe::Glyph | Pipe::Dot | Pipe::Vector | Pipe::Glass => {
                        wgpu::PrimitiveTopology::TriangleStrip
                    }
                    _ => wgpu::PrimitiveTopology::TriangleList,
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
    /// The frame is on the effect path.
    effects: bool,
    /// 1 when the shaders must encode sRGB themselves.
    encode_srgb: f32,
    target: [u32; 2],
}

/// A scissor in target pixels, `x0, y0, x1, y1`, as `x, y, w, h` in the surface's pixels;
/// `None` when nothing of it is on the surface.
fn surface_scissor(sc: [f32; 4], surface: &Surface) -> Option<[u32; 4]> {
    let [w, h] = surface.size;
    let [ox, oy] = surface.origin.map(|v| v as f32);
    let x0 = ((sc[0] - ox).floor().max(0.0) as u32).min(w);
    let y0 = ((sc[1] - oy).floor().max(0.0) as u32).min(h);
    let x1 = ((sc[2] - ox).ceil().max(0.0) as u32).min(w);
    let y1 = ((sc[3] - oy).ceil().max(0.0) as u32).min(h);
    (x1 > x0 && y1 > y0).then(|| [x0, y0, x1 - x0, y1 - y0])
}

fn bloom_of(target: &LayerTarget) -> Option<Bloom> {
    match target {
        LayerTarget::Offscreen { bloom: Some(b) } if b.strength > 0.0 => Some(*b),
        _ => None,
    }
}

/// Levels from the radius in pixels: level `l` blurs about `2^(l-1)` times as wide as level 1,
/// whose spread is about 2 pixels. The deepest level fades in, so the radius is continuous.
fn bloom_plan(b: Bloom, ppu: f32) -> BloomPlan {
    let (levels, top) = bloom_levels(b.radius * ppu);
    BloomPlan { strength: b.strength, levels, top, down: Vec::new(), up: Vec::new(), fx: Vec::new() }
}

/// A bloom of `radius_px` physical pixels: its pyramid levels below the layer texture, and
/// how much of the deepest one it mixes in (0 to 1), so the radius changes smoothly.
pub(crate) fn bloom_levels(radius_px: f32) -> (u32, f32) {
    let t = (1.0 + (radius_px.max(1e-3) / 2.0).log2()).clamp(1.0, MAX_LEVELS as f32);
    let levels = t.ceil() as u32;
    let top = if levels == 1 { 1.0 } else { t - (levels - 1) as f32 };
    (levels, top)
}

fn texture_entry(binding: u32, filterable: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn begin<'e>(enc: &'e mut wgpu::CommandEncoder, view: &wgpu::TextureView, label: &str) -> wgpu::RenderPass<'e> {
    begin_with(enc, view, label, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT))
}

/// A pass that keeps what the texture holds.
fn begin_load<'e>(enc: &'e mut wgpu::CommandEncoder, view: &wgpu::TextureView, label: &str) -> wgpu::RenderPass<'e> {
    begin_with(enc, view, label, wgpu::LoadOp::Load)
}

fn begin_with<'e>(
    enc: &'e mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    label: &str,
    load: wgpu::LoadOp<wgpu::Color>,
) -> wgpu::RenderPass<'e> {
    let att = [Some(wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
    })];
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &att,
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
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
    corners_box(
        [(r.x, r.y), (r.right(), r.y), (r.right(), r.bottom()), (r.x, r.bottom())]
            .map(|(x, y)| m.apply(Point::new(x, y))),
    )
}

fn corners_box(pts: [Point; 4]) -> [f32; 4] {
    let mut b = EMPTY;
    for p in pts {
        b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
    }
    b
}

/// A glyph quad's box in pixels, with a pixel for the shader's dilation.
fn glyph_bounds(g: &GlyphInstance) -> [f32; 4] {
    let [x0, y0, x1, y1] = g.ext;
    let l = g.lin;
    let at = |x: f32, y: f32| Point::new(l[0] * x + l[2] * y + g.offset[0], l[1] * x + l[3] * y + g.offset[1]);
    let b = corners_box([at(x0, y0), at(x1, y0), at(x1, y1), at(x0, y1)]);
    // Effects reach past the ink, as the vertex stage grows the quad.
    let e = 1.0
        + 0.7 * g.fx[0]
        + g.fx[3].abs()
        + 1.4 * g.fx2[0]
        + g.fx3[0].hypot(g.fx3[1])
        + 0.5 * g.fx3[2]
        + g.fx3[3].max(0.0);
    [b[0] - e, b[1] - e, b[2] + e, b[3] + e]
}

/// Glyph effects into the instance, with lengths from layer units to pixels.
fn apply_glyph_fx(inst: &mut GlyphInstance, e: &vo_scene::display::GlyphFx, st: &LayerState) {
    let px = st.ppu;
    inst.fx = [e.blur.max(0.0) * px, e.dissolve.clamp(0.0, 1.0), e.wipe.clamp(0.0, 1.0), e.chroma * px];
    if e.halo.a > 0.0 && e.halo_radius > 0.0 {
        inst.fx2 = [e.halo_radius * px, 0.0, 0.0, 0.0];
        inst.halo = linear_premul(e.halo, st.opacity);
        inst.fx3[0] = e.halo_offset.x * px;
        inst.fx3[1] = e.halo_offset.y * px;
    }
    inst.fx3[2] = e.outline.max(0.0) * px;
    inst.fx3[3] = e.weight * px;
}

fn intersect(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

/// Grows `into` to cover `b`, when `b` is not empty.
fn grow(into: &mut [f32; 4], b: [f32; 4]) {
    if b[2] > b[0] && b[3] > b[1] {
        *into = [into[0].min(b[0]), into[1].min(b[1]), into[2].max(b[2]), into[3].max(b[3])];
    }
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

/// Writes uniform blocks bound at dynamic offsets, growing the buffer and its bind group when
/// they do not fit.
#[allow(clippy::too_many_arguments)]
fn write_uniforms(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    slot: &mut Option<(wgpu::Buffer, wgpu::BindGroup)>,
    label: &str,
    layout: &wgpu::BindGroupLayout,
    block: usize,
    data: &[u8],
    stride: u32,
) {
    let need = data.len().max(stride as usize) as u64;
    if slot.as_ref().is_none_or(|(b, _)| b.size() < need) {
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: need.next_power_of_two(),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buf,
                    offset: 0,
                    size: wgpu::BufferSize::new(block as u64),
                }),
            }],
        });
        *slot = Some((buf, bind));
    }
    if !data.is_empty() {
        queue.write_buffer(&slot.as_ref().unwrap().0, 0, data);
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
        validate(&dot_shader_source());
        validate(&bloom_shader_source());
    }

    #[test]
    fn bloom_radius_is_continuous() {
        // The deepest level fades in as the radius grows, and the levels step up by one.
        let mut last = bloom_plan(Bloom { strength: 1.0, radius: 1.0 }, 1.0);
        for i in 1..600 {
            let p = bloom_plan(Bloom { strength: 1.0, radius: 1.01f32.powi(i) }, 1.0);
            assert!(p.levels == last.levels || (p.levels == last.levels + 1 && p.top < 0.05), "radius step {i}");
            last = p;
        }
        assert_eq!(last.levels, MAX_LEVELS);
    }

    #[test]
    fn layer_block_fits_one_slot() {
        // The stage warp (d18ae3b) took the block past 256 bytes; the stride follows its
        // size, so two 256-byte alignment slots per layer is the limit.
        assert!(std::mem::size_of::<LayerUniform>() <= 512);
    }
}
