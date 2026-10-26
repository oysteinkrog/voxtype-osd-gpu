//! Draws a `vo_scene` display list into a texture.
//!
//! [`Renderer::render`] walks the layers in paint order and records one command per run of
//! shapes or glyphs, with a 256-byte layer block (transform, clip, opacity) bound at a dynamic
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
//! - **Emitters** are not drawn (M4).
//! - **Dots** have a pipeline of their own with 16-byte instances (`dots.rs`).

use std::ops::Range;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use skrifa::GlyphId;
use vo_scene::display::{BlendMode, Bloom, DisplayList, GlyphInstance as DlGlyph, ItemKind, LayerTarget};
use vo_scene::font::{Face, FontRegistry};
use vo_scene::geom::{Affine, Point, RRect};
use vo_scene::path::PATH_FONT;

use crate::color::linear_premul;
use crate::dots::GpuDot;
use crate::hot::wgsl;
use crate::shapes::{pixels_per_unit, GpuShape};
use crate::text::slug_gpu::{self, Atlas, GlyphInstance, SlugAtlasGpu, Uniforms, PREMULTIPLIED_OVER};
use crate::vector::{vector_shader_source, GpuVector};

const COMMON_WGSL: &str = include_str!("wgsl/common.wgsl");
const SHAPES_WGSL: &str = include_str!("wgsl/shapes.wgsl");
const GLYPHS_WGSL: &str = include_str!("wgsl/glyphs.wgsl");
const COMPOSITE_WGSL: &str = include_str!("wgsl/composite.wgsl");
const DOTS_WGSL: &str = include_str!("wgsl/dots.wgsl");
const BLOOM_WGSL: &str = include_str!("wgsl/bloom.wgsl");

/// The format of the scene and of every layer texture on the effect path.
pub const EFFECT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Layer texture sizes round up to this, so a layer that moves or grows a little keeps its
/// pooled texture, and every bloom level is an exact half of the one above.
const ALIGN: u32 = 64;
/// Frames a pooled texture may go unused before it is freed.
const POOL_KEEP_FRAMES: u64 = 120;
/// Bloom levels at most, below the layer texture.
const MAX_LEVELS: u32 = 6;
/// How much of the lower level each bloom upsample mixes in.
const BLOOM_MIX: f32 = 0.65;

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

/// The shape shader: `common.wgsl` and `shapes.wgsl`.
pub fn shape_shader_source() -> String {
    [wgsl("common.wgsl", COMMON_WGSL), wgsl("shapes.wgsl", SHAPES_WGSL)].join("\n")
}

/// The display-list glyph shader: the Slug shader, `common.wgsl` and `glyphs.wgsl`.
pub fn glyph_shader_source() -> String {
    [slug_gpu::shader_source().into(), wgsl("common.wgsl", COMMON_WGSL), wgsl("glyphs.wgsl", GLYPHS_WGSL)].join("\n")
}

/// The offscreen layer composite shader: `common.wgsl` and `composite.wgsl`.
pub fn composite_shader_source() -> String {
    [wgsl("common.wgsl", COMMON_WGSL), wgsl("composite.wgsl", COMPOSITE_WGSL)].join("\n")
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pipe {
    Shape,
    Glyph,
    Composite,
    Dot,
    Vector,
    CompositeBloom,
    Down,
    Up,
    Final,
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
}

impl Surface {
    fn new() -> Surface {
        Surface { cmds: Vec::new(), bounds: EMPTY, origin: [0, 0], size: [0, 0], tex: None, bloom: None }
    }

    /// The texture's rectangle in target pixels.
    fn rect(&self) -> [f32; 4] {
        let [x, y] = self.origin.map(|v| v as f32);
        [x, y, x + self.size[0] as f32, y + self.size[1] as f32]
    }
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
    /// Made on the first effect frame: sampled textures and the Fx block.
    fx: Option<FxShared>,
    modules: [Option<wgpu::ShaderModule>; 7],
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
    vector_buf: Option<wgpu::Buffer>,
    vector_layout: wgpu::BindGroupLayout,
    vector_segs_buf: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    uniform_buf: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    fx_buf: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    pool: Vec<PoolTex>,
    quality: Quality,
    /// Light-mode shading (FX14).
    light: bool,
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
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-composite"),
            entries: &[texture_entry(0, false)],
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
            fx: None,
            modules: [None, None, None, None, None, None, None],
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
        let effects = self.quality == Quality::Full && list.layers.iter().any(|l| bloom_of(&l.target).is_some());
        let format = if effects { EFFECT_FORMAT } else { target.format() };
        let encode_srgb = !format.is_srgb() && !matches!(format, wgpu::TextureFormat::Rgba16Float);
        self.shapes.clear();
        self.glyphs.clear();
        self.dots.clear();
        self.vectors.clear();
        self.vector_segs.clear();
        self.uniforms.clear();
        self.fx_uniforms.clear();
        self.surfaces.clear();
        self.frame += 1;
        for p in &mut self.pool {
            p.used = false;
        }
        let mut root = Surface::new();
        root.size = [w, h];
        root.bounds = [0.0, 0.0, w as f32, h as f32];
        self.surfaces.push(root);
        self.scene = None;
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
            };
            let frame = Frame { list, scale, effects, encode_srgb: encode_srgb as u32 as f32, target: [w, h] };
            self.visit(&frame, 0, &top, 0);
        }
        self.place_surfaces(format);
        if let Some(scope) = scope {
            let mut pop = std::pin::pin!(scope.pop());
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            if let std::task::Poll::Ready(Some(e)) = std::future::Future::poll(pop.as_mut(), &mut cx) {
                eprintln!("vo-render: effect textures failed ({e}); dropping to quality {:?}", self.quality.lower());
                self.quality = self.quality.lower();
                self.pool.clear();
                return self.encode(list, scale, target, enc);
            }
        }
        self.upload(format, target.format(), w, h);
        self.encode_passes(target, format, enc);
        self.pool.retain(|p| p.used || self.frame - p.last_used < POOL_KEEP_FRAMES);
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
            LayerTarget::Offscreen { bloom } => {
                let opacity = parent.opacity * layer.opacity;
                if opacity <= 0.0 {
                    return;
                }
                let uniform = self.push_uniform(f, parent, opacity);
                let child = self.surfaces.len();
                let mut s = Surface::new();
                if let Some(b) = bloom.filter(|b| f.effects && b.strength > 0.0) {
                    s.bloom = Some(bloom_plan(b, ppu));
                }
                self.surfaces.push(s);
                let pipe = if self.surfaces[child].bloom.is_some() { Pipe::CompositeBloom } else { Pipe::Composite };
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
        let smear = [
            parent.smear[0] + (to_px.a * mb.x + to_px.c * mb.y),
            parent.smear[1] + (to_px.b * mb.x + to_px.d * mb.y),
        ];
        let state = LayerState { to_px, ppu, opacity, clip, scissor, smear };
        // Content reaches half the smear further either way.
        let reach = |b: [f32; 4]| {
            let (hx, hy) = (0.5 * smear[0].abs(), 0.5 * smear[1].abs());
            [b[0] - hx, b[1] - hy, b[2] + hx, b[3] + hy]
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
                    for g in &f.list.glyphs[range] {
                        if let Some(inst) = self.glyph(g, &state) {
                            grow(&mut self.surfaces[draw_surface].bounds, intersect(reach(glyph_bounds(&inst)), scissor));
                            self.glyphs.push(inst);
                        }
                    }
                    self.push_cmd(draw_surface, Pipe::Glyph, start..self.glyphs.len() as u32, uniform, blend, scissor);
                }
                ItemKind::Vectors => {
                    let start = self.vectors.len() as u32;
                    for v in &f.list.vectors[range] {
                        if let Some(g) = GpuVector::new(f.list, v, &to_px, ppu, opacity, &mut self.vector_segs) {
                            grow(&mut self.surfaces[draw_surface].bounds, intersect(g.bbox, scissor));
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
                // Drawn by their own lanes' pipelines, not wired in here yet.
                ItemKind::Emitters => {}
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
                    grow(&mut self.surfaces[draw_surface].bounds, intersect(b, scissor));
                    self.push_cmd(draw_surface, Pipe::Dot, start..self.dots.len() as u32, uniform, blend, scissor);
                }
                ItemKind::Layer => self.visit(f, item.start, &state, draw_surface),
            }
        }
        if draw_surface != surface {
            self.close_surface(draw_surface, surface, parent.scissor, f.target);
        }
    }

    /// Sizes an offscreen layer's texture from what it drew, and adds the part its parent
    /// shows to the parent's bounds.
    fn close_surface(&mut self, child: usize, parent: usize, parent_scissor: [f32; 4], target: [u32; 2]) {
        let s = &mut self.surfaces[child];
        let b = s.bounds;
        if b[2] <= b[0] || b[3] <= b[1] {
            s.size = [0, 0];
            return;
        }
        let pad = s.bloom.as_ref().map_or(0.0, |p| (3 << p.levels) as f32);
        // Content past the target only matters through the bloom's reach.
        let limit = [-pad, -pad, target[0] as f32 + pad, target[1] as f32 + pad];
        let b = intersect([b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad], limit);
        let (x0, y0) = (b[0].floor() as i32, b[1].floor() as i32);
        let w = ((b[2].ceil() as i32 - x0).max(1) as u32).next_multiple_of(ALIGN);
        let h = ((b[3].ceil() as i32 - y0).max(1) as u32).next_multiple_of(ALIGN);
        s.origin = [x0, y0];
        s.size = [w, h];
        if let Some(p) = &mut s.bloom {
            // Keep the smallest level at least 4 pixels.
            let fit = (w.min(h) / 4).max(1).ilog2().max(1);
            if p.levels > fit {
                (p.levels, p.top) = (fit, 1.0);
            }
        }
        let shown = intersect(s.rect(), parent_scissor);
        grow(&mut self.surfaces[parent].bounds, shown);
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
        let mut u = LayerUniform {
            smear,
            m: [m.a, m.b, m.c, m.d],
            t: [m.e, m.f, st.ppu, opacity],
            viewport: [0.0, 0.0, f.encode_srgb, 0.0],
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
                if matches!(c.pipe, Pipe::Composite | Pipe::CompositeBloom) {
                    let child = &self.surfaces[c.range.start as usize];
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
            let tex = self.acquire(w, h, format);
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
        if let Some(i) = self
            .pool
            .iter()
            .position(|p| !p.used && p.texture.width() == w && p.texture.height() == h && p.texture.format() == format)
        {
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
        self.pool.push(PoolTex { texture, view, bind, used: true, last_used: frame });
        self.pool.len() - 1
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
            for c in &s.cmds {
                let key = (format, c.pipe, c.blend);
                if !needed.contains(&key) {
                    needed.push(key);
                }
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

    fn encode_passes(&self, target: &Target, format: wgpu::TextureFormat, enc: &mut wgpu::CommandEncoder) {
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
            let mut pass = begin(enc, view, if i == 0 { "vo-target" } else { "vo-layer" });
            self.run(&mut pass, s, format);
            drop(pass);
            if let Some(b) = &s.bloom {
                self.encode_bloom(enc, s, b);
            }
        }
        if let (Some(scene), Some(fx)) = (self.scene, &self.fx) {
            let bind = self.sampled_bind(fx, scene, scene);
            let mut pass = begin(enc, &target.view, "vo-final");
            pass.set_pipeline(self.cached_pipeline(target.format(), Pipe::Final, BlendMode::Normal));
            pass.set_bind_group(0, &bind, &[]);
            let offset = (self.fx_uniforms.len() - self.uniform_stride as usize) as u32;
            pass.set_bind_group(1, &self.fx_buf.as_ref().unwrap().1, &[offset]);
            pass.draw(0..3, 0..1);
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

    fn run(&self, pass: &mut wgpu::RenderPass<'_>, surface: &Surface, format: wgpu::TextureFormat) {
        let [w, h] = surface.size;
        let [ox, oy] = surface.origin.map(|v| v as f32);
        let layers = &self.uniform_buf.as_ref().expect("uploaded").1;
        for c in &surface.cmds {
            let mut sc = c.scissor;
            let child = matches!(c.pipe, Pipe::Composite | Pipe::CompositeBloom)
                .then(|| &self.surfaces[c.range.start as usize]);
            if let Some(child) = child {
                if child.size[0] == 0 {
                    continue;
                }
                sc = intersect(sc, child.rect());
            }
            let x0 = ((sc[0] - ox).floor().max(0.0) as u32).min(w);
            let y0 = ((sc[1] - oy).floor().max(0.0) as u32).min(h);
            let x1 = ((sc[2] - ox).ceil().max(0.0) as u32).min(w);
            let y1 = ((sc[3] - oy).ceil().max(0.0) as u32).min(h);
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
                Pipe::Dot => {
                    pass.set_vertex_buffer(0, self.dot_buf.as_ref().unwrap().slice(..));
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
                Pipe::Composite => {
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
                Pipe::Down | Pipe::Up | Pipe::Final => unreachable!("not a layer command"),
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
            0 => ("vo-shapes", shape_shader_source()),
            1 => ("vo-glyphs", glyph_shader_source()),
            2 => ("vo-composite", composite_shader_source()),
            4 => ("vo-dots", dot_shader_source()),
            5 => ("vo-vectors", vector_shader_source()),
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
            Pipe::Composite | Pipe::CompositeBloom => 2,
            Pipe::Down | Pipe::Up | Pipe::Final => 3,
            Pipe::Dot => 4,
            Pipe::Vector => 5,
        };
        if self.modules[slot].is_none() {
            self.modules[slot] = Some(self.shader_module(slot));
        }
        let module = self.modules[slot].as_ref().unwrap();
        let fx = self.fx.as_ref();
        let groups: [Option<&wgpu::BindGroupLayout>; 2] = match pipe {
            Pipe::Shape => [None, Some(&self.layer_layout)],
            Pipe::Dot => [None, Some(&self.layer_layout)],
            Pipe::Vector => [Some(&self.vector_layout), Some(&self.layer_layout)],
            Pipe::Glyph => [Some(self.atlas_gpu.layout()), Some(&self.layer_layout)],
            Pipe::Composite => [Some(&self.composite_layout), Some(&self.layer_layout)],
            Pipe::CompositeBloom => [fx.map(|f| &f.sampled_layout), Some(&self.layer_layout)],
            Pipe::Down | Pipe::Up | Pipe::Final => [fx.map(|f| &f.sampled_layout), fx.map(|f| &f.fx_layout)],
        };
        let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &groups,
            immediate_size: 0,
        });
        let (vs, fs, buffers) = match pipe {
            Pipe::Shape => ("vs_shape", "fs_shape", vec![Some(GpuShape::layout())]),
            Pipe::Glyph => ("vs_dl_glyph", "fs_dl_glyph", vec![Some(GlyphInstance::layout())]),
            Pipe::Composite => ("vs_composite", "fs_composite", vec![]),
            Pipe::Dot => ("vs_dot", "fs_dot", vec![Some(GpuDot::layout())]),
            Pipe::Vector => ("vs_vector", "fs_vector", vec![Some(GpuVector::layout())]),
            Pipe::CompositeBloom => ("vs_composite", "fs_composite_bloom", vec![]),
            Pipe::Down => ("vs_full", "fs_down", vec![]),
            Pipe::Up => ("vs_full", "fs_up", vec![]),
            Pipe::Final => ("vs_full", "fs_final", vec![]),
        };
        let blend_state = match pipe {
            Pipe::Down | Pipe::Up | Pipe::Final => None,
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
                    Pipe::Shape | Pipe::Glyph | Pipe::Dot | Pipe::Vector => wgpu::PrimitiveTopology::TriangleStrip,
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

fn bloom_of(target: &LayerTarget) -> Option<Bloom> {
    match target {
        LayerTarget::Offscreen { bloom: Some(b) } if b.strength > 0.0 => Some(*b),
        _ => None,
    }
}

/// Levels from the radius in pixels: level `l` blurs about `2^(l-1)` times as wide as level 1,
/// whose spread is about 2 pixels. The deepest level fades in, so the radius is continuous.
fn bloom_plan(b: Bloom, ppu: f32) -> BloomPlan {
    let radius_px = (b.radius * ppu).max(1e-3);
    let t = (1.0 + (radius_px / 2.0).log2()).clamp(1.0, MAX_LEVELS as f32);
    let levels = t.ceil() as u32;
    let top = if levels == 1 { 1.0 } else { t - (levels - 1) as f32 };
    BloomPlan { strength: b.strength, levels, top, down: Vec::new(), up: Vec::new(), fx: Vec::new() }
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
    let att = [Some(wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
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
    [b[0] - 1.0, b[1] - 1.0, b[2] + 1.0, b[3] + 1.0]
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
        assert!(std::mem::size_of::<LayerUniform>() <= 256);
    }
}
