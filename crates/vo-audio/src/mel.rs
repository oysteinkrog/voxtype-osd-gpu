//! A mel spectrogram as a smooth scrolling texture (FX-114).
//!
//! Each frame the painter takes the last `secs` seconds of the 24 mel bands from the signal
//! bus, on the fixed 100 Hz grid, into a small `R8` texture (columns by bands) and draws the
//! element's box from it: linear filtering between bands and frames, a four-stop colour map,
//! grain that scrolls with the data, dark seams at word boundaries and a fade at the old edge.
//! The newest audio is at the right edge. Between frames the picture scrolls by the fraction
//! of a frame the time has moved, so it glides rather than steps.
//!
//! ```ignore
//! vo_audio::install(&mut renderer);
//! let mel = Arc::new(MelTexture::new(cell.clone(), 4.0).seams(word_edges));
//! Element::gpu("mel", mel).size(600.0, 90.0).wake_on_signals(4.0)
//! ```

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use vo_core::hash::rand_f32;
use vo_core::signals::{Sig, SignalsCell, FRAME_SECS, MEL_BANDS};
use vo_render::{GpuPainter, PaintCx};
use vo_scene::geom::Color;
use vo_scene::gpu::GpuSource;

/// Seams drawn at most.
pub const MAX_SEAMS: usize = 32;

/// Four colour stops over band energy 0 to 1, straight-alpha sRGB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorMap {
    pub stops: [(f32, Color); 4],
}

impl ColorMap {
    /// Black through brown and orange to pale yellow, like the amber mock-up.
    pub const AMBER: ColorMap = ColorMap {
        stops: [
            (0.0, Color { r: 0.05, g: 0.03, b: 0.02, a: 0.0 }),
            (0.35, Color { r: 0.42, g: 0.18, b: 0.03, a: 0.85 }),
            (0.7, Color { r: 0.98, g: 0.58, b: 0.12, a: 1.0 }),
            (1.0, Color { r: 1.0, g: 0.93, b: 0.70, a: 1.0 }),
        ],
    };
    /// Navy through cyan to white.
    pub const ICE: ColorMap = ColorMap {
        stops: [
            (0.0, Color { r: 0.02, g: 0.04, b: 0.10, a: 0.0 }),
            (0.35, Color { r: 0.06, g: 0.22, b: 0.45, a: 0.85 }),
            (0.7, Color { r: 0.25, g: 0.78, b: 1.0, a: 1.0 }),
            (1.0, Color { r: 0.92, g: 0.98, b: 1.0, a: 1.0 }),
        ],
    };
    /// Black through purple and red to yellow (magma-like).
    pub const MAGMA: ColorMap = ColorMap {
        stops: [
            (0.0, Color { r: 0.0, g: 0.0, b: 0.02, a: 0.0 }),
            (0.35, Color { r: 0.36, g: 0.07, b: 0.43, a: 0.9 }),
            (0.7, Color { r: 0.93, g: 0.33, b: 0.27, a: 1.0 }),
            (1.0, Color { r: 0.99, g: 0.93, b: 0.60, a: 1.0 }),
        ],
    };
}

/// A scrolling mel spectrogram texture. See the module docs.
#[derive(Debug, Clone)]
pub struct MelTexture {
    pub cell: SignalsCell,
    /// Seconds across the box.
    pub secs: f64,
    /// Band energy (0 is -100 dB, 1 is 0 dB) at the bottom and the top of the colour map.
    pub range: (f32, f32),
    pub map: ColorMap,
    /// Grain strength, 0 to 1: per frame and pixel row brightness noise that moves with the
    /// data.
    pub grain: f32,
    /// Scene times of word starts and ends, drawn as dark gaps. The newest [`MAX_SEAMS`] in view
    /// are drawn.
    pub seams: Arc<[f64]>,
    /// Seam width in physical pixels.
    pub seam_px: f32,
    /// Fade at the old (left) edge as a fraction of the width.
    pub fade: f32,
}

impl MelTexture {
    pub fn new(cell: SignalsCell, secs: f64) -> MelTexture {
        MelTexture {
            cell,
            secs: secs.max(0.05),
            range: (0.42, 0.8),
            map: ColorMap::AMBER,
            grain: 0.35,
            seams: Arc::from([]),
            seam_px: 3.0,
            fade: 0.06,
        }
    }

    pub fn seams(mut self, seams: impl Into<Arc<[f64]>>) -> MelTexture {
        self.seams = seams.into();
        self
    }

    pub fn map(mut self, map: ColorMap) -> MelTexture {
        self.map = map;
        self
    }

    /// Columns in the data texture: the window plus one frame each side for the scroll.
    fn cols(&self) -> usize {
        (self.secs / FRAME_SECS).ceil() as usize + 2
    }

    /// The data texture at `t`, row 0 the highest band, and the column (fractional) at the
    /// box's left edge.
    pub fn data(&self, t: f64) -> (Vec<u8>, f32) {
        let signals = self.cell.get();
        let cols = self.cols();
        let mut px = vec![0u8; cols * MEL_BANDS];
        let mut newest_at = t;
        for b in 0..MEL_BANDS {
            let (v, at) = signals.history(Sig::Band(b as u8), t, 1.0 / FRAME_SECS, cols);
            newest_at = at;
            let row = MEL_BANDS - 1 - b;
            for (c, x) in v.iter().enumerate() {
                px[row * cols + c] = (x.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
        // Column c holds the frame `cols - 1 - c` steps before the newest; the right edge is
        // the read time, `t - newest_at` after the newest.
        let right = (cols - 1) as f64 + (t - newest_at) / FRAME_SECS;
        (px, (right - self.secs / FRAME_SECS) as f32)
    }

    /// Seam positions across the box (0 left, 1 right) at `t`, newest first.
    pub fn seam_xs(&self, t: f64) -> Vec<f32> {
        let read = t - self.cell.get().playout();
        self.seams
            .iter()
            .rev()
            .map(|&s| 1.0 - ((read - s) / self.secs) as f32)
            .filter(|x| (0.0..=1.0).contains(x))
            .take(MAX_SEAMS)
            .collect()
    }
}

impl GpuSource for MelTexture {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn until(&self) -> f64 {
        // The last column scrolls out of the box `secs` after the last frame.
        self.cell.get().active_until(self.secs).unwrap_or(0.0)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniform {
    /// The box in surface pixels: x0, y0, x1, y1.
    rect: [f32; 4],
    /// Surface width, height, opacity, 1 to encode sRGB.
    surface: [f32; 4],
    /// Column at the left edge, columns across the box, texture columns, grain.
    scroll: [f32; 4],
    /// Range low and high, fade, seam width in pixels.
    look: [f32; 4],
    stops: [[f32; 4]; 4],
    stop_at: [f32; 4],
    seams: [[f32; 4]; MAX_SEAMS / 4],
    /// Seam count, a grain seed, 2 spare.
    misc: [f32; 4],
}

const WGSL: &str = r#"
struct U {
    rect: vec4f,
    surface: vec4f,
    scroll: vec4f,
    look: vec4f,
    stops: array<vec4f, 4>,
    stop_at: vec4f,
    seams: array<vec4f, 8>,
    misc: vec4f,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var data: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct V {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
};

@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    let c = vec2f(f32(i & 1u), f32((i >> 1u) & 1u));
    let px = mix(u.rect.xy, u.rect.zw, c);
    var v: V;
    v.pos = vec4f(px.x / u.surface.x * 2.0 - 1.0, 1.0 - px.y / u.surface.y * 2.0, 0.0, 1.0);
    v.uv = c;
    return v;
}

fn hash(p: vec2f) -> f32 {
    return fract(sin(dot(p, vec2f(12.9898, 78.233))) * 43758.5453);
}

fn to_linear(c: vec3f) -> vec3f {
    return select(pow((c + 0.055) / 1.055, vec3f(2.4)), c / 12.92, c <= vec3f(0.04045));
}

fn colour(e: f32) -> vec4f {
    var c = u.stops[0];
    for (var i = 1; i < 4; i++) {
        let a = u.stop_at[i - 1];
        let b = u.stop_at[i];
        if (e > a) {
            c = mix(u.stops[i - 1], u.stops[i], clamp((e - a) / max(b - a, 1e-6), 0.0, 1.0));
        }
    }
    return c;
}

@fragment fn fs(v: V) -> @location(0) vec4f {
    let cols = u.scroll.z;
    let col = u.scroll.x + v.uv.x * u.scroll.y;
    let raw = textureSample(data, samp, vec2f((col + 0.5) / cols, v.uv.y)).r;
    var e = clamp((raw - u.look.x) / max(u.look.y - u.look.x, 1e-6), 0.0, 1.0);
    // Grain fixed to the data: per frame column and pixel row.
    let n = hash(vec2f(floor(col) + u.misc.y, floor(v.pos.y)));
    e = clamp(e * (1.0 + u.scroll.w * (n - 0.5)), 0.0, 1.0);
    var c = colour(e);
    var a = c.a * u.surface.z * smoothstep(0.0, max(u.look.z, 1e-6), v.uv.x);
    let w = u.rect.z - u.rect.x;
    for (var i = 0; i < i32(u.misc.x); i++) {
        let d = abs(v.uv.x - u.seams[i / 4][i % 4]) * w;
        a *= smoothstep(0.25 * u.look.w, 0.5 * u.look.w + 0.75, d);
    }
    var rgb = c.rgb;
    if (u.surface.w == 0.0) {
        rgb = to_linear(rgb);
    }
    return vec4f(rgb * a, a);
}
"#;

struct ItemGpu {
    uniform: wgpu::Buffer,
    tex: wgpu::Texture,
    cols: usize,
    bind: wgpu::BindGroup,
}

/// Draws [`MelTexture`] items. Add it with `vo_audio::install` or `Renderer::add_painter`.
#[derive(Default)]
pub struct MelPainter {
    layout: Option<wgpu::BindGroupLayout>,
    sampler: Option<wgpu::Sampler>,
    module: Option<wgpu::ShaderModule>,
    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    /// One set per item drawn this frame: queue writes land before the whole command buffer.
    items: Vec<ItemGpu>,
    used: usize,
}

impl MelPainter {
    fn setup(&mut self, device: &wgpu::Device) {
        if self.layout.is_some() {
            return;
        }
        let vis = wgpu::ShaderStages::VERTEX_FRAGMENT;
        self.layout = Some(device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-mel"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: vis,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: vis,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: vis,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        }));
        self.sampler = Some(device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vo-mel"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        }));
        self.module = Some(device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vo-mel"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        }));
    }

    fn ensure_pipeline(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        let (layout, module) = (self.layout.as_ref().unwrap(), self.module.as_ref().unwrap());
        self.pipelines.entry(format).or_insert_with(|| {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vo-mel"),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            let over = wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("vo-mel"),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState { color: over, alpha: over }),
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
        });
    }

    /// This frame's buffers for the next item, with a texture of `cols` columns.
    fn item(&mut self, device: &wgpu::Device, cols: usize) -> usize {
        let i = self.used;
        self.used += 1;
        if self.items.get(i).is_some_and(|it| it.cols == cols) {
            return i;
        }
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-mel"),
            size: std::mem::size_of::<Uniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vo-mel"),
            size: wgpu::Extent3d { width: cols as u32, height: MEL_BANDS as u32, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-mel"),
            layout: self.layout.as_ref().unwrap(),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(self.sampler.as_ref().unwrap()),
                },
            ],
        });
        let it = ItemGpu { uniform, tex, cols, bind };
        if i < self.items.len() {
            self.items[i] = it;
        } else {
            self.items.push(it);
        }
        i
    }
}

impl GpuPainter for MelPainter {
    fn begin_frame(&mut self) {
        self.used = 0;
    }

    fn paint(&mut self, cx: &mut PaintCx<'_>) -> bool {
        let Some(mel) = cx.source.as_any().downcast_ref::<MelTexture>() else { return false };
        let [x0, y0, x1, y1] = cx.rect_px();
        if cx.opacity <= 0.0 || x1 <= x0 || y1 <= y0 {
            return true;
        }
        self.setup(cx.device);
        let cols = mel.cols();
        let i = self.item(cx.device, cols);
        let (px, left) = mel.data(cx.t);
        let it = &self.items[i];
        cx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &it.tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &px,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(cols as u32), rows_per_image: None },
            wgpu::Extent3d { width: cols as u32, height: MEL_BANDS as u32, depth_or_array_layers: 1 },
        );
        let seams = mel.seam_xs(cx.t);
        let mut seam_arr = [[0.0f32; 4]; MAX_SEAMS / 4];
        for (k, x) in seams.iter().enumerate() {
            seam_arr[k / 4][k % 4] = *x;
        }
        let m = &mel.map.stops;
        let u = Uniform {
            rect: [x0, y0, x1, y1],
            surface: [cx.size[0] as f32, cx.size[1] as f32, cx.opacity, cx.encode_srgb as u8 as f32],
            scroll: [left, (mel.secs / FRAME_SECS) as f32, cols as f32, mel.grain],
            look: [mel.range.0, mel.range.1, mel.fade, mel.seam_px],
            stops: m.map(|(_, c)| [c.r, c.g, c.b, c.a]),
            stop_at: m.map(|(p, _)| p),
            seams: seam_arr,
            // The seed changes the grain pattern per element, not per frame.
            misc: [seams.len() as f32, (rand_f32(17, cols as u32, 0, 0) * 1000.0).floor(), 0.0, 0.0],
        };
        cx.queue.write_buffer(&it.uniform, 0, bytemuck::bytes_of(&u));
        self.ensure_pipeline(cx.device, cx.format);
        let (pipeline, bind) = (&self.pipelines[&cx.format], &self.items[i].bind);
        let mut pass = cx.enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vo-mel"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: cx.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        let [sx, sy, sw, sh] = cx.scissor;
        pass.set_scissor_rect(sx, sy, sw, sh);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..4, 0..1);
        true
    }
}

#[cfg(test)]
mod tests {
    use vo_core::signals::{SignalFrame, Signals};

    use super::*;

    fn cell() -> SignalsCell {
        let frames = (0..200).map(|i| {
            let mut mel = [0.0; MEL_BANDS];
            mel[5] = i as f32 / 200.0;
            SignalFrame { mel, ..SignalFrame::silent(i as f64 * FRAME_SECS) }
        });
        SignalsCell::new(Signals::recorded(frames, 0.0).with_playout(0.0))
    }

    #[test]
    fn data_has_newest_on_the_right_and_low_bands_at_the_bottom() {
        let m = MelTexture::new(cell(), 1.0);
        let (px, left) = m.data(1.5);
        let cols = m.cols();
        let row = MEL_BANDS - 1 - 5;
        let line = &px[row * cols..(row + 1) * cols];
        assert!(line[cols - 1] > line[0], "{line:?}");
        assert!(px[(MEL_BANDS - 1) * cols..].iter().all(|&v| v == 0));
        // The box spans `secs` of columns ending one column from the texture's right edge.
        assert!((left - 1.0).abs() < 1e-3, "{left}");
    }

    #[test]
    fn seams_sit_at_their_time_and_drop_out_of_view() {
        let m = MelTexture::new(cell(), 1.0).seams(vec![0.2, 1.0, 1.25]);
        let xs = m.seam_xs(1.5);
        assert_eq!(xs.len(), 2);
        assert!((xs[0] - 0.75).abs() < 1e-5 && (xs[1] - 0.5).abs() < 1e-5, "{xs:?}");
    }

    #[test]
    fn wakes_until_the_last_column_leaves() {
        let m = MelTexture::new(cell(), 1.0);
        // 1.99 s newest frame, 30 ms gap allowance, then the 1 s window.
        assert!(m.until() > 3.0 && m.until() < 3.05, "{}", m.until());
    }

    #[test]
    fn shader_parses() {
        naga::front::wgsl::parse_str(WGSL).unwrap();
    }
}
