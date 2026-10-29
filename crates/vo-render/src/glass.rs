//! Glass panels (FX06), their animated rim (FX08) and the compositor blur region (FX07).
//!
//! A panel is its rounded rect or any outline (EG-06, [`GlassShape`] or a filled path in
//! [`GlassInstance::outline`]); the outline goes to the shader as a polygon. Inside the
//! renderer's display list pass glass also follows rotated and perspective-tilted layers.
//!
//! [`Glass`] is the material (defined in `vo_scene::glass`, so elements can ask for it):
//! tint, alphas, accent and light settings, with a dark and a light preset. [`GlassDraw`] is one panel at one instant. [`GlassPass`] draws a batch of
//! them as instanced quads into a target, either before the panel content ([`GlassBlend::Over`])
//! or after it, landing behind it ([`GlassBlend::Behind`]).
//!
//! Refraction (EG-05, [`Refract`]): inside the renderer's display list pass, a glass whose
//! layer has a backdrop (EG-01) in the surface it draws into reads that backdrop at an offset
//! from its surface normal, magnified and split per colour channel. It is drawn by the
//! textured glass pipeline, whose group 2 is the material textures' (EG-02). Without a
//! backdrop, and in [`GlassPass`], it draws as plain glass.
//!
//! A client cannot read the desktop behind it. Without compositor blur the panel is a tint
//! with a bevel; with it, the compositor blurs the desktop under [`blur_region`]. Either way
//! the interior tint must keep text readable over any backdrop; [`Glass::contrast`] is the
//! AX01 check.

use vo_scene::display::InputRegion;
use vo_scene::geom::Color;
use vo_scene::geom::Point;
pub use vo_scene::glass::{
    blur_rects, push_outline, Glass, GlassInstance, GlassKind, GlassShape, LensProfile, Refract,
};
use vo_scene::path::VectorSeg;

use crate::color::srgb_to_linear;

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

impl GlassDraw {
    /// The draw for a display list item whose layer maps its space to target logical units
    /// by a scale `k` and an offset `(dx, dy)`. [`GlassPass`] draws axis-aligned and follows
    /// `glass.shape`, not a path outline; the renderer's display list pass does both and
    /// follows rotated and tilted layers.
    pub fn from_instance(g: &GlassInstance, k: f32, dx: f32, dy: f32, opacity: f32, blur_behind: bool) -> GlassDraw {
        GlassDraw {
            rect: [g.rect[0] * k + dx, g.rect[1] * k + dy, g.rect[2] * k, g.rect[3] * k],
            radius: g.radius * k,
            glass: g.glass,
            time: g.age,
            orbit: g.orbit,
            orbit_strength: g.orbit_strength,
            sheen: g.sheen,
            opacity: g.alpha * opacity,
            blur_behind,
        }
    }
}

/// How the pass blends into what the target already holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlassBlend {
    /// Premultiplied over: draw the glass before the panel's content.
    Over,
    /// Destination over: draw it after the content, and it lands behind it.
    Behind,
}

/// The blur region for one rounded rect, for `SurfaceChange::Blur`; see
/// [`vo_scene::glass::blur_rects`].
pub fn blur_region(rect: [f32; 4], radius: f32) -> InputRegion {
    let mut rects = Vec::new();
    blur_rects(rect, radius, &mut rects);
    InputRegion { rects }
}

pub(crate) const GLASS_WGSL: &str = include_str!("wgsl/glass.wgsl");
const GLASS_VIEW_WGSL: &str = include_str!("wgsl/glass_view.wgsl");
pub(crate) const GLASS_LAYER_WGSL: &str = include_str!("wgsl/glass_layer.wgsl");

/// The shader of [`GlassPass`]: `glass_view.wgsl` and `glass.wgsl`.
pub fn glass_view_shader_source() -> String {
    [GLASS_VIEW_WGSL, GLASS_WGSL].join("\n")
}

/// The shader inside the renderer's display list pass: `common.wgsl`, `glass_layer.wgsl` and
/// `glass.wgsl`. Uses the renderer's `wgsl` sources so live reload sees edits.
pub fn glass_layer_shader_source() -> String {
    use crate::hot::wgsl;
    [
        wgsl("common.wgsl", include_str!("wgsl/common.wgsl")),
        wgsl("glass_layer.wgsl", GLASS_LAYER_WGSL),
        wgsl("glass.wgsl", GLASS_WGSL),
    ]
    .join("\n")
}

/// One glass panel as the renderer's display list pass reads it: the instance layout of
/// [`GlassPass`] in layer units, with the layer block doing the mapping to pixels.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GlassGpu([f32; 40]);

impl GlassGpu {
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 10] = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4,
            6 => Float32x4, 7 => Float32x4, 8 => Float32x4, 9 => Float32x4
        ];
        wgpu::VertexBufferLayout {
            array_stride: INSTANCE,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        }
    }

    /// `g` with `opacity` (the layer's) multiplied into its alpha. A shaped panel appends
    /// its outline to `points` (x, y, arc length, 0 in layer units), which the pass binds as
    /// group 0; `segs` is the display list's `vector_segs`, for a path outline. `refract`
    /// says the draw reads a backdrop (EG-05): without one the refraction is off.
    pub fn new(
        g: &GlassInstance,
        opacity: f32,
        segs: &[VectorSeg],
        points: &mut Vec<[f32; 4]>,
        refract: bool,
    ) -> GlassGpu {
        let outline = g.outline_points(segs).map_or([0.0; 4], |pts| push_points(&pts, g.rect, points));
        let outline = exact_ellipse(outline, g.outline.is_none() && g.glass.shape == GlassShape::Ellipse);
        let d = GlassDraw::from_instance(g, 1.0, 0.0, 0.0, opacity, g.glass.blur_behind);
        GlassGpu(instance(&d, 1.0, outline, refract))
    }

    /// Whether the panel bends what is under it (EG-05) and so wants the textured pipeline.
    pub fn refracts(g: &GlassInstance) -> bool {
        g.glass.refract.bends()
    }

    /// Layer-space bounds of what the panel draws, shadow included: x0, y0, x1, y1.
    pub fn reach(g: &GlassInstance) -> [f32; 4] {
        let [x, y, w, h] = g.rect;
        let m = (2.5 * g.glass.shadow[1] + g.glass.shadow[2].abs()).max(0.0) + 2.0;
        [x - m, y - m, x + w + m, y + h + m]
    }
}

/// Bytes per instance: ten vec4.
const INSTANCE: u64 = 10 * 16;

/// Appends the closed polygon `pts` to `out` as x, y and the arc length at each point, and
/// returns the instance's outline block: first point, count, the arc length of the point
/// nearest the top centre of `rect` (where the orbit's turn starts) and the perimeter.
fn push_points(pts: &[Point], rect: [f32; 4], out: &mut Vec<[f32; 4]>) -> [f32; 4] {
    let first = out.len();
    let n = pts.len();
    let top = Point::new(rect[0] + 0.5 * rect[2], rect[1]);
    let (mut s, mut best, mut s_top) = (0.0f32, f32::INFINITY, 0.0f32);
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        out.push([a.x, a.y, s, 0.0]);
        let (ex, ey) = (b.x - a.x, b.y - a.y);
        let len = ex.hypot(ey);
        let h = (((top.x - a.x) * ex + (top.y - a.y) * ey) / (len * len).max(1e-12)).clamp(0.0, 1.0);
        let d = (a.x + ex * h - top.x).hypot(a.y + ey * h - top.y);
        if d < best {
            (best, s_top) = (d, s + h * len);
        }
        s += len;
    }
    [first as f32, n as f32, s_top, s]
}

/// The outline block of [`push_points`] marked as the ellipse filling the panel rect (a
/// negative count) when `ellipse`: the shader then finds the distance in closed form and
/// reads only two points for the arc length, not the whole polygon per pixel (EG-36).
fn exact_ellipse(mut outline: [f32; 4], ellipse: bool) -> [f32; 4] {
    if ellipse && outline[1] > 0.0 {
        outline[1] = -outline[1];
    }
    outline
}

pub struct GlassPass {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    view: wgpu::Buffer,
    /// Outline points and the bind group that holds them with `view`.
    points: wgpu::Buffer,
    bind: wgpu::BindGroup,
    instances: Option<wgpu::Buffer>,
    count: u32,
    encode_srgb: bool,
}

impl GlassPass {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, blend: GlassBlend) -> GlassPass {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vo-glass"),
            source: wgpu::ShaderSource::Wgsl(glass_view_shader_source().into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-glass"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
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
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vo-glass"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_glass"),
                compilation_options: Default::default(),
                buffers: &[Some(GlassGpu::layout())],
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
        let points = points_buffer(device, 256);
        let bind = bind_group(device, &layout, &view, &points);
        GlassPass { pipeline, layout, view, points, bind, instances: None, count: 0, encode_srgb: !format.is_srgb() }
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
        let mut points = Vec::new();
        for d in draws {
            let outline =
                d.glass.shape.points(d.rect, d.radius).map_or([0.0; 4], |pts| push_points(&pts, d.rect, &mut points));
            let outline = exact_ellipse(outline, d.glass.shape == GlassShape::Ellipse);
            // No backdrop here: refraction is off (EG-05).
            data.extend_from_slice(&floats(&instance(d, scale, outline, false)));
        }
        if !points.is_empty() {
            let bytes: Vec<u8> = points.iter().flat_map(floats).collect();
            if self.points.size() < bytes.len() as u64 {
                self.points = points_buffer(device, (bytes.len() as u64).next_power_of_two());
                self.bind = bind_group(device, &self.layout, &self.view, &self.points);
            }
            queue.write_buffer(&self.points, 0, &bytes);
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

fn points_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vo-glass-points"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::Buffer,
    points: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("vo-glass"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: view.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: points.as_entire_binding() },
        ],
    })
}

fn floats<const N: usize>(v: &[f32; N]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// The instance block of `d` at `s` physical pixels per logical pixel; `outline` from
/// [`push_points`], in logical units (the shader scales it), or zero for the rounded rect.
/// `refract` turns the refraction on (EG-05); its film is drawn either way.
fn instance(d: &GlassDraw, s: f32, outline: [f32; 4], refract: bool) -> [f32; 40] {
    let g = &d.glass;
    let lin = |c: Color| [srgb_to_linear(c.r), srgb_to_linear(c.g), srgb_to_linear(c.b)];
    let (tint, accent) = (lin(g.tint), lin(g.accent));
    let r = &g.refract;
    let on = refract && r.bends();
    let angle = match r.profile {
        LensProfile::Prism { angle } => angle,
        _ => 0.0,
    };
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
        outline[0],
        outline[1],
        outline[2],
        outline[3],
        if on { r.profile.id() as f32 } else { -1.0 },
        r.bend,
        r.magnify.max(1e-3),
        r.dispersion,
        r.film.clamp(0.0, 1.0),
        if on { r.wobble } else { 0.0 },
        angle.cos(),
        angle.sin(),
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

    fn validate(src: &str) {
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }

    #[test]
    fn shaders_validate_in_naga() {
        validate(&glass_view_shader_source());
        validate(&[include_str!("wgsl/common.wgsl"), GLASS_LAYER_WGSL, GLASS_WGSL].join("\n"));
        // The textured variant reads the backdrop (EG-05).
        let tex = crate::material::with_textures(
            &[include_str!("wgsl/common.wgsl"), GLASS_LAYER_WGSL, GLASS_WGSL].join("\n"),
        );
        assert!(tex.contains("var mat_backdrop"));
        validate(&tex);
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
