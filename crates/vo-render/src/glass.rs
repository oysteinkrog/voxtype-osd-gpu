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
        let base = if g.outline.is_none() { wavy_base(g.glass.shape, g.rect, g.radius) } else { None };
        let outline = g.outline_points(segs).map_or([0.0; 4], |pts| push_points(&pts, g.rect, base, points));
        // The renderer shares the buffer with the vector segments, three entries each: pad to
        // a whole segment, or a vector drawn after this panel reads its segments shifted (EG-38).
        points.resize(points.len().next_multiple_of(crate::vector::SEG_TEXELS), [0.0; 4]);
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

/// Appends the closed polygon `pts` to `out` as x, y and the arc length at each point, after
/// the two header entries of [`outline_header`], and returns the instance's outline block:
/// first point, count, the arc length of the point nearest the top centre of `rect` (where
/// the orbit's turn starts) and the perimeter. `base` is the rounded box a wavy outline
/// follows ([`wavy_base`]).
fn push_points(pts: &[Point], rect: [f32; 4], base: Option<[f32; 3]>, out: &mut Vec<[f32; 4]>) -> [f32; 4] {
    out.extend(outline_header(pts, rect, base));
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

/// The rounded box a [`GlassShape::Wavy`] outline waves about, as insets from `rect` and a
/// corner radius (as `vo_scene`'s wavy outline builds it), or `None` for any other shape.
fn wavy_base(shape: GlassShape, rect: [f32; 4], radius: f32) -> Option<[f32; 3]> {
    let GlassShape::Wavy { amplitude, .. } = shape else {
        return None;
    };
    let a = amplitude.abs();
    let (iw, ih) = (rect[2] - 2.0 * a, rect[3] - 2.0 * a);
    (iw > 0.0 && ih > 0.0).then(|| [a, a, (radius - a).clamp(0.0, 0.5 * iw.min(ih))])
}

/// The header ahead of an outline's points (EG-38), two entries the shader reads at
/// `first - 2` and `first - 1`.
///
/// The first is a rounded box `S` centred on `rect` (insets x and y, corner radius) and `g`,
/// the least signed distance from `S` to any point of the outline. A pixel `p` is then at
/// least `g - sd_S(p)` inside the outline, so deep inside the shader skips the polygon. `g`
/// is -1e30 (never deep) when the centre of `rect` is outside the outline.
///
/// The second lets a long outline that runs once round `S` (a wavy one) test only the edges
/// near a pixel: point `i` sits at index `n * border_param(i) - offset`, within `slack`;
/// `reach` is the farthest any point lies from `S` plus the longest edge. `slack` is -1 when
/// the shader must loop over every edge.
fn outline_header(pts: &[Point], rect: [f32; 4], base: Option<[f32; 3]>) -> [[f32; 4]; 2] {
    const OFF: [[f32; 4]; 2] = [[0.0, 0.0, 0.0, -1e30], [0.0, -1.0, 0.0, 1e30]];
    let n = pts.len();
    let c = Point::new(rect[0] + 0.5 * rect[2], rect[1] + 0.5 * rect[3]);
    let half = [0.5 * rect[2], 0.5 * rect[3]];
    if n < 3 || !winds_round(pts, c) {
        return OFF;
    }
    let local = |p: Point| [p.x - c.x, p.y - c.y];
    // Boxes the size of `rect` and, for an outline with few points (a regular polygon sits
    // well inside its box), smaller ones of the same shape; square to round corners.
    let candidates: Vec<[f32; 3]> = match base {
        Some(b) => vec![b],
        None => {
            let sizes: &[f32] = if n <= 64 { &[1.0, 0.75, 0.5] } else { &[1.0] };
            sizes
                .iter()
                .flat_map(|&f| {
                    let (ix, iy) = ((1.0 - f) * half[0], (1.0 - f) * half[1]);
                    let m = f * half[0].min(half[1]);
                    [0.0, 0.5, 1.0].map(|r| [ix, iy, r * m])
                })
                .collect()
        }
    };
    // The candidate whose deep region (S grown by g) is largest.
    let mut best: Option<([f32; 4], f32)> = None;
    for [ix, iy, r] in candidates {
        let h = [half[0] - ix, half[1] - iy];
        if h[0] <= 0.0 || h[1] <= 0.0 {
            continue;
        }
        let r = r.clamp(0.0, h[0].min(h[1]));
        let g = (0..n)
            .map(|i| edge_min_sd(local(pts[i]), local(pts[(i + 1) % n]), h, r))
            .fold(f32::INFINITY, f32::min);
        let (dx, dy, dr) = (h[0] + g, h[1] + g, (r + g).max(0.0));
        if dx <= 0.0 || dy <= 0.0 {
            continue;
        }
        let area = 4.0 * dx * dy - (4.0 - std::f32::consts::PI) * dr * dr;
        if best.is_none_or(|b| area > b.1) {
            best = Some(([ix, iy, r, g], area));
        }
    }
    let Some((s, _)) = best else {
        return OFF;
    };
    let mut header = [s, OFF[1]];
    // The farthest out the outline reaches from S: at a point, since sd is convex along an edge.
    let h = [half[0] - s[0], half[1] - s[1]];
    header[1][3] = pts.iter().map(|p| sd_box(local(*p), h, s[2])).fold(f32::NEG_INFINITY, f32::max) + 1e-3;
    if base.is_some() && n > 24 {
        let mut reach = 0.0f32;
        let (mut lo, mut hi, mut d0) = (f32::INFINITY, f32::NEG_INFINITY, 0.0);
        for (i, p) in pts.iter().enumerate() {
            let q = local(*p);
            let next = pts[(i + 1) % n];
            let edge = (next.x - p.x).hypot(next.y - p.y);
            // An edge's nearest point is within half its length of one of its ends.
            reach = reach.max(sd_box(q, h, s[2]).abs() + 0.5 * edge);
            let d = border_param(q, h, s[2]) * n as f32 - i as f32;
            if i == 0 {
                d0 = d;
            }
            // Unwrapped next to point 0's offset.
            let d = d - n as f32 * ((d - d0) / n as f32).round();
            (lo, hi) = (lo.min(d), hi.max(d));
        }
        let slack = 0.5 * (hi - lo);
        if slack < n as f32 / 8.0 {
            header[1] = [0.5 * (lo + hi), slack, reach, header[1][3]];
        }
    }
    header
}

/// Whether the closed polygon `pts` winds round `p` (nonzero rule, as the shader).
fn winds_round(pts: &[Point], p: Point) -> bool {
    let n = pts.len();
    let mut wind = 0;
    for i in 0..n {
        let (a, b) = (pts[(i + n - 1) % n], pts[i]);
        let c = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
        if a.y <= p.y {
            if b.y > p.y && c > 0.0 {
                wind += 1;
            }
        } else if b.y <= p.y && c < 0.0 {
            wind -= 1;
        }
    }
    wind != 0
}

/// Signed distance from the rounded box of half size `h` and corner radius `r` centred on the
/// origin (the shader's `sd_box`).
fn sd_box(q: [f32; 2], h: [f32; 2], r: f32) -> f32 {
    let (dx, dy) = (q[0].abs() - h[0] + r, q[1].abs() - h[1] + r);
    dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - r
}

/// The least [`sd_box`] along the segment `a`-`b`, a little under (the function is convex
/// along a line, so a ternary search finds it).
fn edge_min_sd(a: [f32; 2], b: [f32; 2], h: [f32; 2], r: f32) -> f32 {
    let at = |t: f32| sd_box([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t], h, r);
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let (m0, m1) = (t0 + (t1 - t0) / 3.0, t1 - (t1 - t0) / 3.0);
        if at(m0) < at(m1) {
            t1 = m1;
        } else {
            t0 = m0;
        }
    }
    let len = (b[0] - a[0]).hypot(b[1] - a[1]);
    at(0.5 * (t0 + t1)).min(at(0.0)).min(at(1.0)) - (1e-3 * len + 1e-3)
}

/// Where `q` sits along the border of the rounded box (half size `h`, corner radius `r`,
/// centred on the origin), 0..1 clockwise from the top centre: the shader's `border_param`.
fn border_param(q: [f32; 2], half: [f32; 2], r: f32) -> f32 {
    use std::f32::consts::FRAC_PI_2;
    let h = [half[0] - r, half[1] - r];
    let quarter = FRAC_PI_2 * r;
    let total = 4.0 * (h[0] + h[1]) + 4.0 * quarter;
    let x = q[0].abs();
    let (to_tb, to_side) = (half[1] - q[1].abs(), half[0] - x);
    let mut s = if x > h[0] && q[1].abs() > h[1] {
        let d = [x - h[0], q[1].abs() - h[1]];
        if q[1] < 0.0 {
            h[0] + r * d[0].atan2(d[1])
        } else {
            h[0] + quarter + 2.0 * h[1] + r * d[1].atan2(d[0])
        }
    } else if to_tb <= to_side {
        if q[1] < 0.0 {
            x
        } else {
            h[0] + 2.0 * quarter + 2.0 * h[1] + (h[0] - x)
        }
    } else {
        h[0] + quarter + (q[1] + h[1])
    };
    if q[0] < 0.0 {
        s = total - s;
    }
    (s / total).rem_euclid(1.0)
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
            let base = wavy_base(d.glass.shape, d.rect, d.radius);
            let outline = d
                .glass
                .shape
                .points(d.rect, d.radius)
                .map_or([0.0; 4], |pts| push_points(&pts, d.rect, base, &mut points));
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

    /// The outline header (EG-38) on wavy, chamfered, regular and notched outlines: where the
    /// shader takes `g - sd_S` as the depth the pixel is that far inside or deeper, where it
    /// takes `sd_S - gmax` as the distance out the pixel is that far outside or farther, and
    /// every point of a wavy outline sits within `slack` of its index.
    #[test]
    fn outline_header_bounds_hold() {
        use vo_scene::frame::Corner;
        let rect = [10.0, 20.0, 400.0, 150.0];
        let shapes = [
            GlassShape::Wavy { amplitude: 3.0, wavelength: 34.0 },
            GlassShape::Wavy { amplitude: 6.0, wavelength: 18.0 },
            GlassShape::chamfer(12.0),
            GlassShape::Polygon { sides: 3, turn: 0.0 },
            GlassShape::Polygon { sides: 6, turn: 0.1 },
            GlassShape::Corners([Corner::Notch(20.0), Corner::Round(30.0), Corner::Chamfer(25.0), Corner::Notch(10.0)]),
        ];
        for shape in shapes {
            let pts = shape.points(rect, 24.0).unwrap();
            let base = wavy_base(shape, rect, 24.0);
            let [h0, h1] = outline_header(&pts, rect, base);
            assert!(h0[3] > -1e29, "{shape:?}: no deep region");
            let c = [rect[0] + 0.5 * rect[2], rect[1] + 0.5 * rect[3]];
            let hs = [0.5 * rect[2] - h0[0], 0.5 * rect[3] - h0[1]];
            let n = pts.len();
            let dist = |p: Point| {
                (0..n)
                    .map(|i| {
                        let (a, b) = (pts[i], pts[(i + 1) % n]);
                        let (ex, ey, wx, wy) = (b.x - a.x, b.y - a.y, p.x - a.x, p.y - a.y);
                        let h = ((wx * ex + wy * ey) / (ex * ex + ey * ey).max(1e-12)).clamp(0.0, 1.0);
                        (wx - ex * h).hypot(wy - ey * h)
                    })
                    .fold(f32::INFINITY, f32::min)
            };
            let (mut deep, mut far) = (0, 0);
            for iy in 0..60 {
                for ix in 0..120 {
                    let p = Point::new(rect[0] - 40.0 + ix as f32 * 4.0, rect[1] - 40.0 + iy as f32 * 4.0);
                    let sd = sd_box([p.x - c[0], p.y - c[1]], hs, h0[2]);
                    let inside = winds_round(&pts, p);
                    if h0[3] - sd > 0.0 {
                        deep += 1;
                        assert!(inside, "{shape:?}: {p:?} counted inside");
                        assert!(dist(p) + 1e-3 >= h0[3] - sd, "{shape:?}: {p:?} not as deep as the bound");
                    }
                    if sd - h1[3] > 0.0 {
                        far += 1;
                        assert!(!inside, "{shape:?}: {p:?} counted outside");
                        assert!(dist(p) + 1e-3 >= sd - h1[3], "{shape:?}: {p:?} not as far out as the bound");
                    }
                }
            }
            assert!(deep > 400 && far > 500, "{shape:?}: deep {deep}, far {far}");
            if base.is_some() {
                assert!(h1[1] >= 0.0 && h1[1] < 2.0, "{shape:?}: slack {}", h1[1]);
                for (i, p) in pts.iter().enumerate() {
                    let f = border_param([p.x - c[0], p.y - c[1]], hs, h0[2]) * n as f32 - h1[0];
                    let d = (f - i as f32).rem_euclid(n as f32);
                    assert!(d.min(n as f32 - d) <= h1[1] + 1e-2, "{shape:?}: point {i} at index {f}");
                }
            }
        }
    }
}
