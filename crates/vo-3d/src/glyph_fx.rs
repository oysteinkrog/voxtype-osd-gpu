//! Glyph field effects: shader motion built on each letter's distance field
//! (`crate::glyph_sdf`), drawn as a GPU element over or under live text.
//!
//! - [`Effect::Ink`] (MO-82): a new letter blooms like ink dropped on wet paper. Ink spreads
//!   from the letter's centre along noisy fibres, hangs past its edges as a soft cloud and is
//!   drawn back into crisp type, leaving a faint trace at the centre. The text draws the
//!   letter itself from its first frame, so the reading line stays readable; the ink is
//!   colour over and around it.
//! - [`Effect::Light`] (MO-84): the words as a stencil in front of a moving light. Under the
//!   text ([`Side::Under`]) the light falls on the panel with caustics that move with the
//!   voice, and each letter casts a long soft shadow away from it; an onset sends a ring of
//!   light out. Over the text ([`Side::Over`]) the letters nearest the light take its accent.
//! - [`Effect::Morph`] (MO-87): a word flows into its correction. Shared letters stay solid
//!   and slide to their new place; the changed ones drain and fill through a smooth union
//!   and melt into drops. The look hides the word's own glyphs while it runs
//!   (`Element::words_drawn_elsewhere`).
//!
//! The app describes the glyphs (font, glyph id, pen position in the element's box, size,
//! colour, start time) in a [`GlyphFx`] and puts it in `Element::gpu`; [`install`] adds the
//! painter. Everything is a pure function of the frame time and the signal bus, so a live
//! session and an export draw the same frames. Reduced motion draws each effect's settled
//! state: the ink trace, still light with still shadows, the corrected word.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use vo_core::signals::{Sig, SignalsCell};
use vo_render::color::srgb_to_linear;
use vo_render::text::slug_gpu::PREMULTIPLIED_OVER;
use vo_render::{GpuPainter, PaintCx, Renderer};
use vo_scene::font::{FontId, FontRegistry};
use vo_scene::geom::{Color, Point};
use vo_scene::gpu::GpuSource;

use crate::glyph_sdf::{glyph_sdf, GlyphSdf};

const FX_WGSL: &str = include_str!("wgsl/glyph_fx.wgsl");
const ATLAS: u32 = 2048;

pub fn shader_source() -> &'static str {
    FX_WGSL
}

/// MO-82: ink diffusion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ink {
    /// The ink's colour.
    pub color: Color,
    /// Seconds from arrival until the letter is clean.
    pub secs: f32,
    /// How far the cloud hangs past the letter's edge at first, in em.
    pub reach: f32,
    /// Seconds the trace at the centre lasts after the bloom, and its strength (0..1).
    pub trace_secs: f32,
    pub trace: f32,
    /// How much a loud arrival widens the cloud: reach times `1 + voice * level`.
    pub voice: f32,
}

impl Default for Ink {
    fn default() -> Ink {
        Ink { color: Color::hex(0x3d8bff, 1.0), secs: 0.42, reach: 0.22, trace_secs: 1.4, trace: 0.22, voice: 0.8 }
    }
}

/// MO-84: light through letters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    /// The light on the panel.
    pub color: Color,
    /// What the letters nearest the light take.
    pub accent: Color,
    /// Shadow length in logical pixels, and strength (0..1).
    pub shadow_len: f32,
    pub shadow: f32,
    /// Caustic strength, and cell size in logical pixels.
    pub caustics: f32,
    pub cell: f32,
    /// How far the glow reaches from the light, in logical pixels.
    pub falloff: f32,
    /// Seconds for the light to cross the box and back with no voice.
    pub sweep_secs: f32,
    /// How strong the accent on the letters is.
    pub sheen: f32,
    /// How far in from the element's edges the light fades in, in logical pixels.
    pub edge_fade: f32,
}

impl Default for Light {
    fn default() -> Light {
        Light {
            color: Color::hex(0xbcd8ff, 1.0),
            accent: Color::hex(0x5dd3ff, 1.0),
            shadow_len: 70.0,
            shadow: 0.85,
            caustics: 1.6,
            cell: 150.0,
            falloff: 320.0,
            sweep_secs: 14.0,
            sheen: 0.85,
            edge_fade: 28.0,
        }
    }
}

/// Which pass of the light a GPU element draws: put an `Under` element before the text and an
/// `Over` element after it, both with the same glyphs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Under,
    Over,
}

/// MO-87: liquid morph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Morph {
    /// Seconds from the old word to the new.
    pub secs: f32,
    /// How far the old word's edges move in by the end, and the new word's at the start, in
    /// em. About a stroke's width: less leaves both words showing longer.
    pub shrink: f32,
    /// The radius of the liquid join, in em.
    pub bridge: f32,
    /// How far the changed letters melt mid-way (the radius their field is averaged over),
    /// in em.
    pub melt: f32,
    /// A tint the flowing parts take mid-way; alpha 0 keeps the text's colour.
    pub tint: Color,
}

impl Default for Morph {
    fn default() -> Morph {
        Morph { secs: 0.48, shrink: 0.06, bridge: 0.16, melt: 0.06, tint: Color::hex(0x3d8bff, 0.0) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
    Ink(Ink),
    Light(Light, Side),
    Morph(Morph),
}

/// Which word of a morph a glyph belongs to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Part {
    /// Both: a letter the old and new word share. It slides from `from_dx` to its place.
    #[default]
    Both,
    Old,
    New,
}

/// One glyph for an effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxGlyph {
    pub font: FontId,
    pub glyph: u32,
    /// Pen position on the baseline, in the element's box (logical units).
    pub pos: Point,
    /// Em size in logical units.
    pub size: f32,
    pub color: Color,
    /// Scene time the effect starts for this glyph: its arrival (ink) or its word's revision
    /// (morph). Unused by the light.
    pub start: f64,
    /// Morph: which word, which group of glyphs morphs together, and for a shared letter how
    /// far left of its new place (logical units) it starts.
    pub part: Part,
    pub group: u32,
    pub from_dx: f32,
}

impl FxGlyph {
    pub fn new(font: FontId, glyph: u32, pos: Point, size: f32, color: Color, start: f64) -> FxGlyph {
        FxGlyph { font, glyph, pos, size, color, start, part: Part::Both, group: 0, from_dx: 0.0 }
    }

    pub fn part(self, part: Part, group: u32) -> FxGlyph {
        FxGlyph { part, group, ..self }
    }

    pub fn from_dx(self, dx: f32) -> FxGlyph {
        FxGlyph { from_dx: dx, ..self }
    }
}

/// What a glyph field element draws. Build a new one when the glyphs change.
#[derive(Clone)]
pub struct GlyphFx {
    pub effect: Effect,
    pub glyphs: Vec<FxGlyph>,
    /// The voice, for the light's position and strength and the ink's reach.
    pub signals: Option<SignalsCell>,
    pub reduced: bool,
    /// Where the glyph outlines come from: the registry the app's text uses.
    pub fonts: Option<FontRegistry>,
}

impl std::fmt::Debug for GlyphFx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlyphFx")
            .field("effect", &self.effect)
            .field("glyphs", &self.glyphs.len())
            .field("reduced", &self.reduced)
            .finish()
    }
}

impl GlyphFx {
    pub fn new(effect: Effect, glyphs: Vec<FxGlyph>) -> GlyphFx {
        GlyphFx { effect, glyphs, signals: None, reduced: false, fonts: None }
    }

    pub fn signals(mut self, s: SignalsCell) -> GlyphFx {
        self.signals = Some(s);
        self
    }

    pub fn fonts(mut self, fonts: FontRegistry) -> GlyphFx {
        self.fonts = Some(fonts);
        self
    }

    pub fn reduced(mut self, reduced: bool) -> GlyphFx {
        self.reduced = reduced;
        self
    }

    /// As the source of `Element::gpu`.
    pub fn item(self) -> Arc<dyn GpuSource> {
        Arc::new(self)
    }

    /// The scene time after which nothing changes.
    pub fn settled_at(&self) -> f64 {
        let last = self.glyphs.iter().map(|g| g.start).fold(f64::NEG_INFINITY, f64::max);
        match self.effect {
            Effect::Light(..) if !self.reduced => f64::INFINITY,
            Effect::Light(..) => 0.0,
            Effect::Ink(ink) => last + (ink.secs + ink.trace_secs) as f64,
            Effect::Morph(m) => last + m.secs as f64,
        }
    }

    fn level(&self, t: f64) -> (f32, f32) {
        match &self.signals {
            Some(s) => {
                let s = s.get();
                (s.envelope(Sig::Level, 0.03, 0.4, t), s.onset_age(t))
            }
            None => (0.0, 1000.0),
        }
    }

    /// Where the light is at `t` in a box `w` by `h` (logical units, from its top left), and
    /// how bright (0..1). With no voice it drifts slowly across and back; the voice pushes it
    /// on and brightens it.
    pub fn light_at(&self, light: &Light, t: f64, w: f32, h: f32) -> (Point, f32) {
        if self.reduced {
            return (Point::new(0.12 * w, 0.15 * h), 0.6);
        }
        let (level, _) = self.level(t);
        let phase = (t / light.sweep_secs.max(0.1) as f64 * std::f64::consts::TAU) as f32;
        let x = 0.5 - 0.42 * phase.cos() + 0.12 * level;
        let y = 0.12 + 0.1 * (0.6 * phase).sin();
        (Point::new(x * w, y * h), (0.45 + 0.55 * level).min(1.0))
    }
}

impl GpuSource for GlyphFx {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn until(&self) -> f64 {
        self.settled_at()
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FxUniform {
    viewport: [f32; 4],
    dst: [f32; 4],
    rect: [f32; 4],
    time: [f32; 4],
    light: [f32; 4],
    p0: [f32; 4],
    p1: [f32; 4],
    c0: [f32; 4],
    c1: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuGlyph {
    atlas: [f32; 4],
    em: [f32; 4],
    place: [f32; 4],
    color: [f32; 4],
    info: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuQuad {
    rect: [f32; 4],
    range: [u32; 4],
}

/// Linear light, straight alpha.
fn lin(c: Color) -> [f32; 4] {
    [srgb_to_linear(c.r), srgb_to_linear(c.g), srgb_to_linear(c.b), c.a]
}

#[derive(Clone, Copy)]
struct Slot {
    rect: [u32; 4],
    em: [f32; 4],
}

/// The atlas of glyph fields: shelves in one `R16Float` texture, filled as glyphs are first
/// drawn. When it is full it starts again from empty.
struct Atlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    slots: HashMap<(FontId, u32), Option<Slot>>,
    shelf: [u32; 3],
}

impl Atlas {
    fn new(device: &wgpu::Device) -> Atlas {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vo3d-glyph-fx-atlas"),
            size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Atlas { texture, view, slots: HashMap::new(), shelf: [0, 0, 0] }
    }

    /// Room for `w` by `h` texels, or `None` when the atlas is full.
    fn place(&mut self, w: u32, h: u32) -> Option<[u32; 2]> {
        let [x, y, row] = self.shelf;
        let (x, y, row) = if x + w > ATLAS { (0, y + row + 1, 0) } else { (x, y, row) };
        if y + h > ATLAS || w > ATLAS {
            return None;
        }
        self.shelf = [x + w + 1, y, row.max(h)];
        Some([x, y])
    }

    fn get(&mut self, queue: &wgpu::Queue, fonts: &FontRegistry, font: FontId, glyph: u32) -> Option<Slot> {
        if let Some(s) = self.slots.get(&(font, glyph)) {
            return *s;
        }
        let sdf = fonts.get(font).and_then(|face| glyph_sdf(&face, glyph));
        let slot = sdf.and_then(|sdf| {
            let at = match self.place(sdf.w, sdf.h) {
                Some(at) => at,
                None => {
                    self.slots.clear();
                    self.shelf = [0, 0, 0];
                    self.place(sdf.w, sdf.h)?
                }
            };
            self.upload(queue, &sdf, at);
            Some(Slot { rect: [at[0], at[1], sdf.w, sdf.h], em: sdf.em })
        });
        self.slots.insert((font, glyph), slot);
        slot
    }

    fn upload(&self, queue: &wgpu::Queue, sdf: &GlyphSdf, at: [u32; 2]) {
        let half: Vec<u16> = sdf.dist.iter().map(|&d| f16_bits(d)).collect();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: at[0], y: at[1], z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&half),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(sdf.w * 2), rows_per_image: Some(sdf.h) },
            wgpu::Extent3d { width: sdf.w, height: sdf.h, depth_or_array_layers: 1 },
        );
    }
}

/// An `f32` as IEEE half bits, rounded to nearest. Distances here are small, so no infinities.
fn f16_bits(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32 - 127 + 15;
    let man = b & 0x7f_ffff;
    if exp <= 0 {
        if exp < -10 {
            return sign;
        }
        let m = (man | 0x80_0000) >> (1 - exp);
        return sign | ((m + 0x1000) >> 13) as u16;
    }
    if exp >= 31 {
        return sign | 0x7bff;
    }
    let r = ((exp as u32) << 10 | (man >> 13)) + ((man >> 12) & 1);
    sign | r.min(0x7bff) as u16
}

struct Pipes {
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    main: wgpu::RenderPipeline,
    mask: wgpu::RenderPipeline,
}

/// Draws [`GlyphFx`] elements. `vo_3d::install` adds one, or add it with [`install`]. The
/// atlas keys glyphs by font id, so one painter serves one font registry.
#[derive(Default)]
pub struct GlyphFxPainter {
    atlas: Option<Atlas>,
    pipes: Option<Pipes>,
    sampler: Option<wgpu::Sampler>,
    /// The stencil mask for the light, and a 1 by 1 stand-in for passes that do not read it.
    mask: Option<([u32; 2], wgpu::Texture, wgpu::TextureView)>,
    blank: Option<wgpu::TextureView>,
}

impl GlyphFxPainter {
    fn pipes(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        if self.pipes.as_ref().is_some_and(|p| p.format == format) {
            return;
        }
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty,
            count: None,
        };
        let storage = wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let tex = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo3d-glyph-fx"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                entry(1, storage),
                entry(2, storage),
                entry(3, tex),
                entry(4, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
                entry(5, tex),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vo3d-glyph-fx"),
            source: wgpu::ShaderSource::Wgsl(FX_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vo3d-glyph-fx"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipe = |entry: &str, format, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("vo3d-glyph-fx"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_fx"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
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
            })
        };
        let max = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };
        let main = pipe("fs_fx", format, PREMULTIPLIED_OVER);
        let mask = pipe("fs_mask", wgpu::TextureFormat::R8Unorm, wgpu::BlendState { color: max, alpha: max });
        self.pipes = Some(Pipes { format, layout, main, mask });
    }

    fn mask_view(&mut self, device: &wgpu::Device, size: [u32; 2]) -> wgpu::TextureView {
        if self.mask.as_ref().is_none_or(|m| m.0 != size) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("vo3d-glyph-fx-mask"),
                size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            self.mask = Some((size, texture, view));
        }
        self.mask.as_ref().unwrap().2.clone()
    }

    fn blank(&mut self, device: &wgpu::Device) -> wgpu::TextureView {
        self.blank
            .get_or_insert_with(|| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("vo3d-glyph-fx-blank"),
                        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::R8Unorm,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    })
                    .create_view(&Default::default())
            })
            .clone()
    }
}

/// The glyphs and quads one element draws, in surface pixels.
struct Batch {
    glyphs: Vec<GpuGlyph>,
    /// Glyph quads (one glyph each) and group quads (morph words).
    quads: Vec<GpuQuad>,
}

fn storage(device: &wgpu::Device, label: &str, bytes: &[u8]) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    // A storage binding must not be empty.
    let pad = [0u8; 80];
    let contents = if bytes.is_empty() { &pad[..] } else { bytes };
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents,
        usage: wgpu::BufferUsages::STORAGE,
    })
}

impl GpuPainter for GlyphFxPainter {
    fn paint(&mut self, cx: &mut PaintCx<'_>) -> bool {
        let Some(src) = cx.source.as_any().downcast_ref::<GlyphFx>() else { return false };
        if cx.opacity <= 0.0 {
            return true;
        }
        let t = cx.t;
        let [x0, y0, x1, y1] = cx.rect_px();
        let scale = (cx.to_px.a * cx.to_px.a + cx.to_px.b * cx.to_px.b).sqrt();
        let origin = cx.to_px.apply(Point::new(cx.rect.x, cx.rect.y));
        let Some(fonts) = &src.fonts else { return true };
        let atlas = self.atlas.get_or_insert_with(|| Atlas::new(cx.device));

        // Glyphs in surface pixels, with the time since their start.
        let reduced = src.reduced;
        let mut batch = Batch { glyphs: Vec::new(), quads: Vec::new() };
        let mut groups: Vec<(u32, Vec<GpuGlyph>)> = Vec::new();
        for (i, g) in src.glyphs.iter().enumerate() {
            let age = match src.effect {
                Effect::Ink(ink) if reduced => ink.secs + 0.01,
                Effect::Morph(m) if reduced => m.secs,
                _ => (t - g.start).max(0.0) as f32,
            };
            let live = match src.effect {
                Effect::Ink(ink) => age < ink.secs + ink.trace_secs,
                // After the morph the new word stays drawn until the text takes it back.
                Effect::Morph(m) => !((reduced || age > m.secs) && g.part == Part::Old),
                Effect::Light(..) => true,
            };
            if !live || g.color.a <= 0.0 {
                continue;
            }
            let Some(slot) = atlas.get(cx.queue, fonts, g.font, g.glyph) else { continue };
            let pen = Point::new(origin.x + g.pos.x * scale, origin.y + g.pos.y * scale);
            let ppe = g.size * scale;
            let part = match g.part {
                Part::Both => 0.0,
                Part::Old => 1.0,
                Part::New => 2.0,
            };
            let gg = GpuGlyph {
                atlas: slot.rect.map(|v| v as f32),
                em: slot.em,
                place: [pen.x, pen.y, ppe, age],
                color: lin(g.color),
                info: [part, (i % 97) as f32 + 1.0, g.from_dx * scale, 0.0],
            };
            match src.effect {
                Effect::Morph(_) => match groups.iter_mut().find(|(k, _)| *k == g.group) {
                    Some((_, v)) => v.push(gg),
                    None => groups.push((g.group, vec![gg])),
                },
                _ => {
                    let e = slot.em;
                    let r = [pen.x + e[0] * ppe, pen.y - e[3] * ppe, pen.x + e[2] * ppe, pen.y - e[1] * ppe];
                    batch.quads.push(GpuQuad { rect: r, range: [batch.glyphs.len() as u32, 1, 0, 0] });
                    batch.glyphs.push(gg);
                }
            }
        }
        for (_, gs) in groups {
            let mut r = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            for g in &gs {
                let (e, p) = (g.em, g.place);
                let dx = g.info[2].min(0.0);
                r = [
                    r[0].min(p[0] + e[0] * p[2] + dx),
                    r[1].min(p[1] - e[3] * p[2]),
                    r[2].max(p[0] + e[2] * p[2] + g.info[2].max(0.0)),
                    r[3].max(p[1] - e[1] * p[2]),
                ];
            }
            batch.quads.push(GpuQuad { rect: r, range: [batch.glyphs.len() as u32, gs.len() as u32, 0, 0] });
            batch.glyphs.extend(gs);
        }

        let (level, onset) = src.level(t);
        let rect_w = (x1 - x0).max(1.0);
        let rect_h = (y1 - y0).max(1.0);
        let mut u = FxUniform {
            viewport: [cx.size[0] as f32, cx.size[1] as f32, cx.opacity, cx.encode_srgb as u32 as f32],
            dst: [0.0, 0.0, cx.size[0] as f32, cx.size[1] as f32],
            rect: [x0, y0, rect_w, rect_h],
            time: [0.0, t as f32, level, if reduced { 1000.0 } else { onset }],
            light: [0.0, 0.0, 0.0, reduced as u32 as f32],
            p0: [0.0; 4],
            p1: [0.0; 4],
            c0: [0.0; 4],
            c1: [0.0; 4],
        };
        let mut under = false;
        match src.effect {
            Effect::Ink(ink) => {
                let arrival = src.glyphs.iter().map(|g| g.start).fold(f64::NEG_INFINITY, f64::max);
                let (l, _) = if arrival.is_finite() { src.level(arrival) } else { (0.0, 0.0) };
                u.p0 = [ink.secs, ink.reach, ink.trace_secs, ink.trace];
                u.p1 = [ink.voice, l, 0.0, 0.0];
                u.c0 = lin(ink.color);
            }
            Effect::Morph(m) => {
                u.time[0] = 1.0;
                u.p0 = [m.secs, m.shrink, m.bridge, m.melt];
                u.c0 = lin(m.tint);
            }
            Effect::Light(light, side) => {
                let (at, inten) = src.light_at(&light, t, cx.rect.w, cx.rect.h);
                let lp = cx.to_px.apply(Point::new(cx.rect.x + at.x, cx.rect.y + at.y));
                u.light = [lp.x, lp.y, inten, reduced as u32 as f32];
                u.p0 = [light.shadow_len * scale, light.shadow, light.caustics, light.cell * scale];
                u.p1 = [light.falloff * scale, 0.6, light.sheen, light.edge_fade * scale];
                u.c0 = lin(light.color);
                u.c1 = lin(light.accent);
                match side {
                    Side::Under => {
                        u.time[0] = 2.0;
                        under = true;
                    }
                    Side::Over => u.time[0] = 4.0,
                }
            }
        }
        if batch.quads.is_empty() && !under {
            return true;
        }

        self.pipes(cx.device, cx.format);
        let sampler = self
            .sampler
            .get_or_insert_with(|| {
                cx.device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("vo3d-glyph-fx"),
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    ..Default::default()
                })
            })
            .clone();
        let glyph_buf = storage(cx.device, "vo3d-glyph-fx-glyphs", bytemuck::cast_slice(&batch.glyphs));
        let quad_buf = storage(cx.device, "vo3d-glyph-fx-quads", bytemuck::cast_slice(&batch.quads));
        let atlas_view = self.atlas.as_ref().unwrap().view.clone();
        let device = cx.device;
        let bind = |pipes: &Pipes, u: &FxUniform, mask: &wgpu::TextureView| {
            use wgpu::util::DeviceExt;
            let ub = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vo3d-glyph-fx"),
                contents: bytemuck::bytes_of(u),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vo3d-glyph-fx"),
                layout: &pipes.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: glyph_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: quad_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&atlas_view) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&sampler) },
                    wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(mask) },
                ],
            })
        };

        let mask_view = if under {
            // The stencil mask at half the box's resolution.
            let size = [((rect_w / 2.0).ceil() as u32).max(1), ((rect_h / 2.0).ceil() as u32).max(1)];
            let view = self.mask_view(device, size);
            let pipes = self.pipes.as_ref().unwrap();
            let mu = FxUniform { dst: [x0, y0, rect_w, rect_h], ..u };
            let blank = self.blank.clone();
            let bg = bind(pipes, &mu, blank.as_ref().unwrap_or(&atlas_view));
            let mut pass = cx.enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vo3d-glyph-fx-mask"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if !batch.quads.is_empty() {
                pass.set_pipeline(&pipes.mask);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..4, 0..batch.quads.len() as u32);
            }
            view
        } else {
            self.blank(device)
        };

        // The light under the text is one quad over the whole box, after the glyph quads.
        let count = if under {
            batch.quads.clear();
            0
        } else {
            batch.quads.len() as u32
        };
        let quad_buf2;
        let pipes = self.pipes.as_ref().unwrap();
        let bg = if under {
            let q = [GpuQuad { rect: [x0, y0, x1, y1], range: [0, 0, 0, 0] }];
            quad_buf2 = storage(device, "vo3d-glyph-fx-quads", bytemuck::cast_slice(&q));
            let ub = {
                use wgpu::util::DeviceExt;
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("vo3d-glyph-fx"),
                    contents: bytemuck::bytes_of(&u),
                    usage: wgpu::BufferUsages::UNIFORM,
                })
            };
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vo3d-glyph-fx"),
                layout: &pipes.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: glyph_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: quad_buf2.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&atlas_view) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&sampler) },
                    wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&mask_view) },
                ],
            })
        } else {
            bind(pipes, &u, &mask_view)
        };
        let mut pass = cx.enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vo3d-glyph-fx"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: cx.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let [sx, sy, sw, sh] = cx.scissor;
        pass.set_scissor_rect(sx, sy, sw, sh);
        pass.set_pipeline(&pipes.main);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..4, 0..if under { 1 } else { count });
        true
    }
}

/// Adds a [`GlyphFxPainter`] to `renderer`.
pub fn install(renderer: &mut Renderer) {
    renderer.add_painter(Box::new(GlyphFxPainter::default()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fx_shader_validates_in_naga() {
        let src = shader_source();
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }

    #[test]
    fn uniform_matches_the_shader() {
        assert_eq!(std::mem::size_of::<FxUniform>(), 9 * 16);
        assert_eq!(std::mem::size_of::<GpuGlyph>(), 5 * 16);
        assert_eq!(std::mem::size_of::<GpuQuad>(), 2 * 16);
    }

    #[test]
    fn half_floats_round_trip() {
        for v in [0.0f32, 0.5, -0.25, 0.013, -0.7, 1.25] {
            let h = f16_bits(v);
            let back = half_to_f32(h);
            assert!((back - v).abs() < 1e-3, "{v} -> {h:#x} -> {back}");
        }
    }

    fn half_to_f32(h: u16) -> f32 {
        let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
        let e = ((h >> 10) & 0x1f) as i32;
        let m = (h & 0x3ff) as f32;
        if e == 0 {
            return s * m / 1024.0 * 2f32.powi(-14);
        }
        s * (1.0 + m / 1024.0) * 2f32.powi(e - 15)
    }

    #[test]
    fn settles_after_the_last_glyph() {
        let g = FxGlyph::new(0, 1, Point::ZERO, 20.0, Color::WHITE, 2.0);
        let ink = GlyphFx::new(Effect::Ink(Ink::default()), vec![g]);
        assert!((ink.settled_at() - (2.0 + 0.42 + 1.4)).abs() < 1e-4);
        let light = GlyphFx::new(Effect::Light(Light::default(), Side::Under), vec![g]);
        assert!(light.settled_at().is_infinite());
        assert_eq!(light.reduced(true).settled_at(), 0.0);
    }

    #[test]
    fn light_follows_the_voice() {
        let fx = GlyphFx::new(Effect::Light(Light::default(), Side::Under), Vec::new());
        let l = Light::default();
        // With no voice it starts at the left and crosses over half a sweep.
        let (a, _) = fx.light_at(&l, 0.0, 600.0, 300.0);
        let (b, _) = fx.light_at(&l, l.sweep_secs as f64 / 2.0, 600.0, 300.0);
        assert!(a.x < 100.0 && b.x > 500.0, "{a:?} {b:?}");
        // Reduced motion: one still place.
        let r = fx.clone().reduced(true);
        assert_eq!(r.light_at(&l, 0.0, 600.0, 300.0), r.light_at(&l, 5.0, 600.0, 300.0));
    }
}
