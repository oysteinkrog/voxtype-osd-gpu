//! Glass panels (FX06), their animated rim (FX08) and the compositor blur region (FX07).
//!
//! [`Glass`] is the material: tint, alphas, accent and light settings, with a dark and a
//! light preset. [`GlassDraw`] is one panel at one instant. [`GlassPass`] draws a batch of
//! them as instanced quads into a target, either before the panel content ([`GlassBlend::Over`])
//! or after it, landing behind it ([`GlassBlend::Behind`]).
//!
//! A client cannot read the desktop behind it. Without compositor blur the panel is a tint
//! with a bevel; with it, the compositor blurs the desktop under [`blur_region`]. Either way
//! the interior tint must keep text readable over any backdrop, so [`Glass::contrast`]
//! checks text against the tint composited over pure white and pure black, in linear and in
//! sRGB blending (compositors differ), and takes the worst (AX01).

use vo_scene::display::{IRect, InputRegion};
use vo_scene::geom::Color;

use crate::color::srgb_to_linear;

/// Light or dark glass: which way the edge line and the grain go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlassKind {
    Light,
    Dark,
}

/// The glass material. Lengths are logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glass {
    pub kind: GlassKind,
    /// Interior tint, sRGB. Its alpha is the interior opacity the contrast gate checks.
    pub tint: Color,
    /// Tint alpha right at the edge, rising to the interior alpha over 12 logical pixels.
    pub edge_alpha: f32,
    /// Colour of the orbiting rim segment, sRGB.
    pub accent: Color,
    /// Bevel width.
    pub bevel: f32,
    /// Rim light, Fresnel and edge line strength, 0 to about 1.5.
    pub rim: f32,
    /// Grain amount, relative to the tint.
    pub grain: f32,
    /// Offset between colour channels in the rim light.
    pub dispersion: f32,
    /// Drop shadow alpha, blur and downward offset. Alpha 0 turns it off.
    pub shadow: [f32; 3],
}

impl Glass {
    /// Dark smoked glass for light text. The interior tint keeps white text at 4.5:1 or more
    /// over pure white.
    pub const fn dark() -> Glass {
        Glass {
            kind: GlassKind::Dark,
            tint: Color::rgba(0.055, 0.063, 0.082, 0.92),
            edge_alpha: 0.42,
            accent: Color::rgba(0.45, 0.70, 1.0, 1.0),
            bevel: 12.0,
            rim: 1.0,
            grain: 0.14,
            dispersion: 0.9,
            shadow: [0.30, 14.0, 6.0],
        }
    }

    /// Light frosted glass for dark text.
    pub const fn light() -> Glass {
        Glass {
            kind: GlassKind::Light,
            tint: Color::rgba(0.965, 0.97, 0.98, 0.78),
            edge_alpha: 0.40,
            accent: Color::rgba(0.20, 0.50, 1.0, 1.0),
            bevel: 12.0,
            rim: 1.0,
            grain: 0.5,
            dispersion: 0.9,
            shadow: [0.22, 16.0, 7.0],
        }
    }

    /// The lowest contrast ratio of `text` against the panel interior, over pure white and
    /// pure black backdrops, blended in linear light and in sRGB. `light_margin` is the most
    /// luminance the glass's own light (rim, Fresnel, grain) adds in the interior.
    pub fn contrast(&self, text: Color, light_margin: f32) -> f32 {
        let text_l = luminance([text.r, text.g, text.b].map(srgb_to_linear));
        let tint = [self.tint.r, self.tint.g, self.tint.b];
        let a = self.tint.a;
        let mut worst = f32::INFINITY;
        for back in [0.0f32, 1.0] {
            let linear = tint.map(|c| srgb_to_linear(c) * a + back * (1.0 - a));
            let srgb = tint.map(|c| srgb_to_linear(c * a + back * (1.0 - a)));
            for panel in [linear, srgb] {
                let l = luminance(panel);
                // The margin moves the panel towards the text's own luminance: the worse way.
                let l = if text_l > l { l + light_margin } else { (l - light_margin).max(0.0) };
                worst = worst.min(ratio(text_l, l));
            }
        }
        worst
    }
}

fn luminance(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

fn ratio(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// One glass panel at one instant. `rect` and `radius` are logical pixels in target space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassDraw {
    pub rect: [f32; 4],
    pub radius: f32,
    pub glass: Glass,
    /// Seconds, for anything that moves.
    pub time: f32,
    /// Head of the orbiting rim segment, 0..1 clockwise from the top centre, or `None`.
    pub orbit: Option<f32>,
    pub orbit_strength: f32,
    /// Sheen sweep progress 0..1, or `None` outside the sweep.
    pub sheen: Option<f32>,
    pub opacity: f32,
    /// The compositor blurs behind this panel.
    pub blur_behind: bool,
}

/// How the pass blends into what the target already holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlassBlend {
    /// Premultiplied over: draw the glass before the panel's content.
    Over,
    /// Destination over: draw it after the content, and it lands behind it.
    Behind,
}

/// Rectangles covering a rounded rect, one band per pixel row of the corner radius, each
/// rounded outward to whole logical pixels, for `SurfaceChange::Blur`. Coarser bands leave
/// visible blur steps outside the corner (the stair-step test in the `blur_probe` example).
pub fn blur_region(rect: [f32; 4], radius: f32) -> InputRegion {
    let (x0, y0) = (rect[0].floor() as i32, rect[1].floor() as i32);
    let (x1, y1) = ((rect[0] + rect[2]).ceil() as i32, (rect[1] + rect[3]).ceil() as i32);
    let r = radius.min(rect[2] / 2.0).min(rect[3] / 2.0).max(0.0);
    let ri = r.ceil() as i32;
    let mut rects = Vec::with_capacity(2 * ri as usize + 1);
    for row in 0..ri {
        // The row's edge nearest the straight side has the smallest inset in the row.
        let dy = (r - (row + 1) as f32).max(0.0);
        let inset = (r - (r * r - dy * dy).sqrt()).floor() as i32;
        let w = x1 - x0 - 2 * inset;
        rects.push(IRect { x: x0 + inset, y: y0 + row, w, h: 1 });
        rects.push(IRect { x: x0 + inset, y: y1 - row - 1, w, h: 1 });
    }
    rects.push(IRect { x: x0, y: y0 + ri, w: x1 - x0, h: (y1 - y0 - 2 * ri).max(0) });
    InputRegion { rects }
}

/// Bytes per instance: seven vec4.
const INSTANCE: u64 = 7 * 16;

pub struct GlassPass {
    pipeline: wgpu::RenderPipeline,
    view: wgpu::Buffer,
    bind: wgpu::BindGroup,
    instances: Option<wgpu::Buffer>,
    count: u32,
    encode_srgb: bool,
}

impl GlassPass {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, blend: GlassBlend) -> GlassPass {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vo-glass"),
            source: wgpu::ShaderSource::Wgsl(include_str!("wgsl/glass.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-glass"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(16),
                },
                count: None,
            }],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vo-glass"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let (src_factor, dst_factor) = match blend {
            GlassBlend::Over => (wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrcAlpha),
            GlassBlend::Behind => (wgpu::BlendFactor::OneMinusDstAlpha, wgpu::BlendFactor::One),
        };
        let comp = wgpu::BlendComponent { src_factor, dst_factor, operation: wgpu::BlendOperation::Add };
        let attributes = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4,
            6 => Float32x4
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vo-glass"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_glass"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: INSTANCE,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attributes,
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_glass"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState { color: comp, alpha: comp }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let view = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-glass-view"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-glass"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: view.as_entire_binding() }],
        });
        GlassPass { pipeline, view, bind, instances: None, count: 0, encode_srgb: !format.is_srgb() }
    }

    /// Uploads `draws` for a target of `size` physical pixels at `scale` physical pixels per
    /// logical pixel. Call before [`GlassPass::draw`] or [`GlassPass::encode`].
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: [u32; 2],
        scale: f32,
        draws: &[GlassDraw],
    ) {
        let view = [size[0] as f32, size[1] as f32, if self.encode_srgb { 1.0 } else { 0.0 }, 0.0];
        queue.write_buffer(&self.view, 0, &floats(&view));
        let mut data = Vec::with_capacity(draws.len() * INSTANCE as usize);
        for d in draws {
            data.extend_from_slice(&floats(&instance(d, scale)));
        }
        self.count = draws.len() as u32;
        if data.is_empty() {
            return;
        }
        let fits = self.instances.as_ref().is_some_and(|b| b.size() >= data.len() as u64);
        if !fits {
            self.instances = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vo-glass-instances"),
                size: (data.len() as u64).next_power_of_two().max(INSTANCE * 4),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        queue.write_buffer(self.instances.as_ref().expect("made above"), 0, &data);
    }

    /// Records the prepared panels into an open render pass.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(buf) = &self.instances else { return };
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, buf.slice(..));
        pass.draw(0..4, 0..self.count);
    }

    /// Records the prepared panels in a pass of their own that keeps `view`'s contents.
    pub fn encode(
        &self,
        enc: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        timestamps: Option<wgpu::RenderPassTimestampWrites<'_>>,
    ) {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vo-glass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: timestamps,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.draw(&mut pass);
    }
}

fn floats<const N: usize>(v: &[f32; N]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn instance(d: &GlassDraw, s: f32) -> [f32; 28] {
    let g = &d.glass;
    let lin = |c: Color| [srgb_to_linear(c.r), srgb_to_linear(c.g), srgb_to_linear(c.b)];
    let (tint, accent) = (lin(g.tint), lin(g.accent));
    [
        d.rect[0] * s,
        d.rect[1] * s,
        d.rect[2] * s,
        d.rect[3] * s,
        d.radius * s,
        s,
        d.time,
        if g.kind == GlassKind::Dark { 1.0 } else { 0.0 },
        tint[0],
        tint[1],
        tint[2],
        g.tint.a,
        accent[0],
        accent[1],
        accent[2],
        g.edge_alpha,
        d.orbit.unwrap_or(-1.0),
        d.orbit_strength,
        d.sheen.unwrap_or(-1.0),
        d.opacity,
        g.bevel,
        g.rim,
        g.grain,
        g.dispersion,
        g.shadow[0],
        g.shadow[1],
        g.shadow[2],
        if d.blur_behind { 1.0 } else { 0.0 },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AX01 for the presets with the text colours the demos use: body text, captions and the
    /// status accent, at least 4.5:1 over pure white and pure black in both blend spaces.
    #[test]
    fn presets_keep_text_contrast() {
        let dark = Glass::dark();
        for text in [Color::rgba(0.96, 0.97, 0.98, 1.0), Color::rgba(0.82, 0.84, 0.88, 1.0)] {
            let c = dark.contrast(text, 0.02);
            assert!(c >= 4.5, "dark glass: {text:?} at {c:.2}:1");
        }
        let light = Glass::light();
        for text in [Color::rgba(0.07, 0.08, 0.10, 1.0), Color::rgba(0.24, 0.26, 0.30, 1.0)] {
            let c = light.contrast(text, 0.02);
            assert!(c >= 4.5, "light glass: {text:?} at {c:.2}:1");
        }
    }

    #[test]
    fn blur_region_covers_the_rounded_rect() {
        let rect = [10.3, 20.6, 200.0, 80.0];
        let r = 18.0;
        let region = blur_region(rect, r);
        assert_eq!(region.rects.len(), 2 * 18 + 1);
        let inside = |x: f32, y: f32| {
            region
                .rects
                .iter()
                .any(|q| x >= q.x as f32 && x < (q.x + q.w) as f32 && y >= q.y as f32 && y < (q.y + q.h) as f32)
        };
        // Every pixel centre inside the shape is in the region.
        for yi in 0..120 {
            for xi in 0..230 {
                let (x, y) = (xi as f32 + 0.5, yi as f32 + 0.5);
                let qx = (x - rect[0] - rect[2] / 2.0).abs() - rect[2] / 2.0 + r;
                let qy = (y - rect[1] - rect[3] / 2.0).abs() - rect[3] / 2.0 + r;
                let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
                if d < 0.0 {
                    assert!(inside(x, y), "({x}, {y}) is in the shape but not the region");
                }
            }
        }
    }
}
