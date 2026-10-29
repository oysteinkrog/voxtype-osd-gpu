//! Text on curved surfaces (EG-14): words printed on a strip that rolls up like a scroll
//! ([`Scroll`], 'Breathing Scroll') or on a page that turns over ([`PageTurn`], 'Page
//! Reveal').
//!
//! [`SurfaceText`] draws a text display list into a texture with the normal 2D renderer, the
//! way [`crate::TextField`] does (no backdrop capture), then builds its mipmaps. A mesh draw
//! shows it through [`crate::Material::texture`] after [`crate::Renderer3d::add_texture`].
//! The texture is drawn again only when the text, the page size or the texel scale changes,
//! so settled text costs one draw.
//!
//! Texel density ([`texel_scale`]): one texel per device pixel at the surface's nearest
//! point ([`magnification`]), at most twice the output scale and at most 2048 texels a side.
//!
//! The two shapes are closed form in their one parameter (`unroll` or `turn`, 0 to 1), so a
//! surface is a pure function of `t` through whatever drives that parameter. Their vertex
//! layout is a [`crate::Ribbon3d`]'s (rows along the length, `uv` x along it and y across from
//! bottom to top), so they are added once and rewritten with
//! [`crate::Renderer3d::update_mesh`].
//!
//! Only older lines, titles and final text go on a curved surface. The reading line (the
//! words being spoken now) never does: it stays flat, crisp Slug text in the 2D layer, so a
//! curl or a turn never gets between the user and what they are saying.
//!
//! Reduced motion: the scroll is always flat (fully unrolled), and a page turn jumps to its
//! end as soon as it starts (any `turn` above 0 is drawn as 1, a flat page turned over).

use std::f32::consts::PI;

use vo_render::{Renderer, Target};
use vo_scene::display::DisplayList;

use crate::camera::{Camera, Projection};
use crate::math::{Mat4, Vec3};
use crate::mesh::{Mesh, Vertex};
use crate::ribbon3d::Ribbon3d;

/// The texture format: what the 2D renderer writes into a plain UNORM target, premultiplied
/// and sRGB encoded. The mesh shader decodes it.
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Most texels on a side.
pub const MAX_SIDE: u32 = 2048;

const MIP_WGSL: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;

struct V {
  @builtin(position) pos: vec4f,
  @location(0) uv: vec2f,
};

@vertex fn vs (@builtin(vertex_index) i: u32) -> V {
  let p = vec2f (f32 ((i << 1u) & 2u), f32 (i & 2u));
  var o: V;
  o.pos = vec4f (p * 2.0 - 1.0, 0.0, 1.0);
  o.uv = vec2f (p.x, 1.0 - p.y);
  return o;
}

// One bilinear tap half way between four texels of the level above: their mean.
@fragment fn fs (v: V) -> @location(0) vec4f {
  return textureSampleLevel (src, smp, v.uv, 0.0);
}
"#;

/// The mipmap shader, for naga checks.
pub fn shader_source() -> &'static str {
    MIP_WGSL
}

/// Texels per logical pixel of the page: `scale * magnify` (device pixels per logical pixel
/// at the nearest point), rounded up to a quarter so small camera moves do not redraw the
/// text, at least a quarter, at most `2 * scale`, and small enough that no side passes
/// [`MAX_SIDE`]. `page` is the page size in logical pixels.
pub fn texel_scale(page: [f32; 2], scale: f32, magnify: f32) -> f32 {
    let k = ((scale * magnify) * 4.0).ceil() / 4.0;
    let k = k.clamp(0.25, 2.0 * scale);
    let side = page[0].max(page[1]).max(1.0);
    k.min(MAX_SIDE as f32 / side)
}

/// The texture size for `page` at `k` texels per logical pixel.
pub fn texel_size(page: [f32; 2], k: f32) -> [u32; 2] {
    page.map(|v| ((v * k).ceil() as u32).clamp(1, MAX_SIDE))
}

/// How much bigger than at `scale` the surface shows at its nearest vertex: device pixels per
/// logical page pixel there, divided by `scale`. `size` is the target in device pixels,
/// `world_per_px` the world units one logical page pixel spans on the surface. Feed it to
/// [`texel_scale`].
pub fn magnification(
    camera: &Camera,
    size: [u32; 2],
    model: &Mat4,
    vertices: &[Vertex],
    world_per_px: f32,
    scale: f32,
) -> f32 {
    let focal = camera.focal_px(size[1] as f32);
    let px_per_unit = match camera.projection {
        Projection::Orthographic { .. } => focal,
        _ => {
            let view = camera.view();
            let depth = vertices
                .iter()
                .map(|v| {
                    let w = model.transform_point(Vec3::from(v.pos));
                    -view.transform_point(Vec3::new(w[0], w[1], w[2]))[2]
                })
                .fold(f32::INFINITY, f32::min);
            focal / depth.max(camera.near)
        }
    };
    px_per_unit * world_per_px / scale.max(1e-6)
}

struct Sized {
    size: [u32; 2],
    /// Where the 2D renderer draws; copied into level 0.
    target: Target,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// One bind group per level from 1, reading the level above; the view it writes.
    mips: Vec<(wgpu::BindGroup, wgpu::TextureView)>,
}

/// A text display list drawn into a mipmapped texture for a curved surface.
pub struct SurfaceText {
    device: wgpu::Device,
    mip: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    sized: Option<Sized>,
    /// What the texture shows: the list, the page size and the texel scale.
    drawn: Option<(DisplayList, [f32; 2], f32)>,
    /// How many times the text has been drawn into the texture.
    pub draws: u64,
    /// Bumped each time the texture is made again (a new size): call
    /// [`crate::Renderer3d::set_texture`] with the new [`SurfaceText::view`] then.
    pub generation: u64,
}

impl SurfaceText {
    pub fn new(device: &wgpu::Device) -> SurfaceText {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vo3d-surface-mip"),
            source: wgpu::ShaderSource::Wgsl(MIP_WGSL.into()),
        });
        let mip = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vo3d-surface-mip"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vo3d-surface-mip"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        SurfaceText { device: device.clone(), mip, sampler, sized: None, drawn: None, draws: 0, generation: 0 }
    }

    /// Records drawing `text` (laid out in a `page` of logical pixels) at `k` texels per
    /// logical pixel ([`texel_scale`]), then its mipmaps, into `enc`, and returns true. The
    /// renderer's buffers are written now, so submit `enc` before the renderer encodes
    /// another list. Records nothing and returns false when the texture already shows this
    /// list at this page size and scale.
    pub fn update(
        &mut self,
        renderer: &mut Renderer,
        text: &DisplayList,
        page: [f32; 2],
        k: f32,
        enc: &mut wgpu::CommandEncoder,
    ) -> bool {
        if self.drawn.as_ref().is_some_and(|(l, p, s)| l == text && *p == page && *s == k) {
            return false;
        }
        self.resize(texel_size(page, k));
        let s = self.sized.as_ref().unwrap();
        renderer.encode(text, k, &s.target, enc);
        let extent = wgpu::Extent3d { width: s.size[0], height: s.size[1], depth_or_array_layers: 1 };
        enc.copy_texture_to_texture(s.target.texture().as_image_copy(), s.texture.as_image_copy(), extent);
        for (bind, view) in &s.mips {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vo3d-surface-mip"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
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
            pass.set_pipeline(&self.mip);
            pass.set_bind_group(0, bind, &[]);
            pass.draw(0..3, 0..1);
        }
        match &mut self.drawn {
            Some((l, p, s)) => {
                l.clone_from(text);
                (*p, *s) = (page, k);
            }
            None => self.drawn = Some((text.clone(), page, k)),
        }
        self.draws += 1;
        true
    }

    fn resize(&mut self, size: [u32; 2]) {
        if self.sized.as_ref().is_some_and(|s| s.size == size) {
            return;
        }
        let d = &self.device;
        let levels = 32 - size[0].max(size[1]).leading_zeros();
        let target = Target::new(d, size[0], size[1], FORMAT);
        let texture = d.create_texture(&wgpu::TextureDescriptor {
            label: Some("vo3d-surface-text"),
            size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let level = |i: u32| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: i,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        let layout = self.mip.get_bind_group_layout(0);
        let mips = (1..levels)
            .map(|i| {
                let bind = d.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("vo3d-surface-mip"),
                    layout: &layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&level(i - 1)) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    ],
                });
                (bind, level(i))
            })
            .collect();
        let view = texture.create_view(&Default::default());
        self.sized = Some(Sized { size, target, texture, view, mips });
        self.drawn = None;
        self.generation += 1;
    }

    /// The texture with all its levels, for [`crate::Renderer3d::add_texture`], once
    /// [`SurfaceText::update`] has run.
    pub fn view(&self) -> Option<&wgpu::TextureView> {
        self.sized.as_ref().map(|s| &s.view)
    }

    /// The texture, for readback (level 0) and debugging.
    pub fn texture(&self) -> Option<&wgpu::Texture> {
        self.sized.as_ref().map(|s| &s.texture)
    }

    /// Its size in texels at level 0.
    pub fn size(&self) -> Option<[u32; 2]> {
        self.sized.as_ref().map(|s| s.size)
    }

    /// GPU memory held: the 2D target plus the texture with all its levels, in bytes.
    pub fn bytes(&self) -> u64 {
        let Some(s) = &self.sized else { return 0 };
        let texel = 4u64;
        let mut total = s.size[0] as u64 * s.size[1] as u64 * texel;
        let (mut w, mut h) = (s.size[0] as u64, s.size[1] as u64);
        for _ in 0..s.texture.mip_level_count() {
            total += w * h * texel;
            (w, h) = ((w / 2).max(1), (h / 2).max(1));
        }
        total
    }
}

/// The grid both shapes share: `segments` rows along the length, `across` columns.
fn grid_indices(segments: u32, across: u32) -> Vec<u32> {
    Ribbon3d { segments, across, ..Ribbon3d::default() }.indices()
}

/// A strip rolled on a cylinder: the part from the left end up to `unroll` of its length lies
/// flat in the plane z = 0, facing +z; the rest is wound behind that plane in a tight spiral
/// (outer turn first), so the roll sits at the edge of the unrolled part with the front of
/// the strip on its outside. The strip lies along x, centred on the origin, width along y.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scroll {
    pub length: f32,
    pub width: f32,
    /// Quads along the length; the roll needs many.
    pub segments: u32,
    pub across: u32,
    /// Radius of the innermost turn.
    pub core: f32,
    /// How far each turn of the spiral sits outside the one inside it.
    pub gap: f32,
    pub reduced_motion: bool,
}

impl Default for Scroll {
    fn default() -> Scroll {
        Scroll { length: 3.0, width: 0.6, segments: 240, across: 1, core: 0.05, gap: 0.012, reduced_motion: false }
    }
}

impl Scroll {
    /// The unroll actually drawn: `unroll` clamped, or 1 under reduced motion.
    pub fn effective(&self, unroll: f32) -> f32 {
        if self.reduced_motion {
            1.0
        } else {
            unroll.clamp(0.0, 1.0)
        }
    }

    /// Radius of the roll's outer turn at `unroll`.
    pub fn outer_radius(&self, unroll: f32) -> f32 {
        let rolled = (1.0 - self.effective(unroll)) * self.length;
        (self.core * self.core + self.gap * rolled / PI).sqrt()
    }

    /// Position and front normal of the strip's centre line at `s` (0 to 1 along it).
    pub fn at(&self, s: f32, unroll: f32) -> (Vec3, Vec3) {
        let u = self.effective(unroll);
        let x0 = -0.5 * self.length;
        let flat = u * self.length;
        let a = s * self.length;
        if a <= flat {
            return (Vec3::new(x0 + a, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
        }
        let b = a - flat;
        let r_out = self.outer_radius(u);
        // Archimedean spiral by arc length: area wound so far is gap * b, so
        // r(b)^2 = r_out^2 - gap * b / pi, and dtheta / db = 1 / r.
        let (r, theta) = if self.gap > 0.0 {
            let r = (r_out * r_out - self.gap * b / PI).max(self.core * self.core).sqrt();
            (r, 2.0 * PI * (r_out - r) / self.gap)
        } else {
            (r_out, b / r_out.max(1e-6))
        };
        let (sn, cs) = theta.sin_cos();
        let pos = Vec3::new(x0 + flat + r * sn, 0.0, -r_out + r * cs);
        (pos, Vec3::new(sn, 0.0, cs))
    }

    pub fn vertex_count(&self) -> usize {
        (self.segments.max(1) as usize + 1) * (self.across.max(1) as usize + 1)
    }

    /// The strip at `unroll`, into `out` (cleared first), in [`crate::Ribbon3d`]'s layout.
    pub fn vertices(&self, unroll: f32, out: &mut Vec<Vertex>) {
        out.clear();
        let (n, cols) = (self.segments.max(1), self.across.max(1));
        for i in 0..=n {
            let s = i as f32 / n as f32;
            let (c, normal) = self.at(s, unroll);
            for j in 0..=cols {
                let v = j as f32 / cols as f32;
                let p = c + Vec3::new(0.0, (v - 0.5) * self.width, 0.0);
                out.push(Vertex { pos: p.to_array(), normal: normal.to_array(), uv: [s, v] });
            }
        }
    }

    pub fn indices(&self) -> Vec<u32> {
        grid_indices(self.segments.max(1), self.across.max(1))
    }

    /// The flat strip, to add once.
    pub fn mesh(&self) -> Mesh {
        let mut vertices = Vec::new();
        self.vertices(1.0, &mut vertices);
        Mesh { vertices, indices: self.indices() }
    }
}

/// A page bent around a cylinder whose axis lies in the page and sweeps from the right edge
/// (`turn` 0, flat) across to the left (`turn` 1, the page lies turned over on its left half
/// side, its back up). The part past the axis curls up toward the viewer (+z) over a cylinder
/// of `radius`, and past half a turn lies flat again, upside down, `2 * radius` above. The
/// page lies in the plane z = 0 facing +z, centred on the origin, length along x.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageTurn {
    pub length: f32,
    pub width: f32,
    pub segments: u32,
    pub across: u32,
    pub radius: f32,
    /// Tilt of the axis from the page's y axis in radians, so a corner leads.
    pub slant: f32,
    pub reduced_motion: bool,
}

impl Default for PageTurn {
    fn default() -> PageTurn {
        PageTurn {
            length: 3.0,
            width: 0.6,
            segments: 96,
            across: 24,
            radius: 0.18,
            slant: 0.25,
            reduced_motion: false,
        }
    }
}

impl PageTurn {
    /// The turn actually drawn: `turn` clamped, or under reduced motion 0 at 0 and 1 past it.
    pub fn effective(&self, turn: f32) -> f32 {
        let t = turn.clamp(0.0, 1.0);
        if self.reduced_motion && t > 0.0 {
            1.0
        } else {
            t
        }
    }

    /// The in-page unit direction across the axis, toward the free edge.
    fn across_axis(&self) -> (f32, f32) {
        let (s, c) = self.slant.sin_cos();
        (c, s)
    }

    /// Distance of the axis from the page centre along [`PageTurn::across_axis`] at `turn`.
    fn axis(&self, turn: f32) -> f32 {
        let (nx, ny) = self.across_axis();
        let reach = 0.5 * self.length * nx.abs() + 0.5 * self.width * ny.abs();
        // A hair past half a turn at 1, so the last corner lies flat too.
        let (hi, lo) = (reach, -reach - PI * self.radius * 1.001 - 1e-5);
        hi + self.effective(turn) * (lo - hi)
    }

    /// Position and front normal of the page point `(s, v)` (0 to 1 along and across).
    pub fn at(&self, s: f32, v: f32, turn: f32) -> (Vec3, Vec3) {
        let (nx, ny) = self.across_axis();
        let (x, y) = ((s - 0.5) * self.length, (v - 0.5) * self.width);
        let d = x * nx + y * ny - self.axis(turn);
        if d <= 0.0 {
            return (Vec3::new(x, y, 0.0), Vec3::new(0.0, 0.0, 1.0));
        }
        let r = self.radius.max(1e-6);
        let (bx, by) = (x - nx * d, y - ny * d);
        if d < PI * r {
            let phi = d / r;
            let (sn, cs) = phi.sin_cos();
            let pos = Vec3::new(bx + nx * r * sn, by + ny * r * sn, r * (1.0 - cs));
            return (pos, Vec3::new(-nx * sn, -ny * sn, cs));
        }
        let back = d - PI * r;
        (Vec3::new(bx - nx * back, by - ny * back, 2.0 * r), Vec3::new(0.0, 0.0, -1.0))
    }

    pub fn vertex_count(&self) -> usize {
        (self.segments.max(1) as usize + 1) * (self.across.max(1) as usize + 1)
    }

    /// The page at `turn`, into `out` (cleared first), in [`crate::Ribbon3d`]'s layout.
    pub fn vertices(&self, turn: f32, out: &mut Vec<Vertex>) {
        out.clear();
        let (n, cols) = (self.segments.max(1), self.across.max(1));
        for i in 0..=n {
            let s = i as f32 / n as f32;
            for j in 0..=cols {
                let v = j as f32 / cols as f32;
                let (p, normal) = self.at(s, v, turn);
                out.push(Vertex { pos: p.to_array(), normal: normal.to_array(), uv: [s, v] });
            }
        }
    }

    pub fn indices(&self) -> Vec<u32> {
        grid_indices(self.segments.max(1), self.across.max(1))
    }

    /// The flat page, to add once.
    pub fn mesh(&self) -> Mesh {
        let mut vertices = Vec::new();
        self.vertices(0.0, &mut vertices);
        Mesh { vertices, indices: self.indices() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_shader_validates_in_naga() {
        let src = shader_source();
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }

    #[test]
    fn texel_size_follows_the_scale() {
        let page = [600.0, 120.0];
        let one = texel_size(page, texel_scale(page, 1.0, 1.0));
        let more = texel_size(page, texel_scale(page, 1.5, 1.0));
        assert_eq!(one, [600, 120]);
        assert_eq!(more, [900, 180]);
        // Nearer than 1:1 needs more texels, but never past twice the scale.
        assert_eq!(texel_scale(page, 1.0, 1.6), 1.75);
        assert_eq!(texel_scale(page, 1.0, 5.0), 2.0);
        assert_eq!(texel_scale(page, 1.5, 5.0), 3.0);
        // Far away it shrinks, to a floor of a quarter.
        assert_eq!(texel_scale(page, 1.0, 0.4), 0.5);
        assert_eq!(texel_scale(page, 1.0, 0.01), 0.25);
        // No side past 2048.
        let wide = [1800.0, 100.0];
        let k = texel_scale(wide, 2.0, 1.0);
        assert!(texel_size(wide, k)[0] <= MAX_SIDE, "{k}");
        // Small moves round to the same quarter, so the text is not drawn again.
        assert_eq!(texel_scale(page, 1.0, 1.01), texel_scale(page, 1.0, 1.2));
    }

    #[test]
    fn magnification_is_one_at_the_matching_distance() {
        // A 600 px page three world units long, seen straight on from where one world unit
        // spans 200 px: one device pixel per logical pixel at scale 1, 1.5x at scale 1.5.
        let page = PageTurn::default();
        let mesh = page.mesh();
        let cam = Camera::orbit(Vec3::ZERO, 0.0, 0.0, 3.0);
        let focal = cam.focal_px(300.0);
        let cam = Camera { distance: focal / 200.0, ..cam };
        let m = magnification(&cam, [600, 300], &Mat4::IDENTITY, &mesh.vertices, 3.0 / 600.0, 1.0);
        assert!((m - 1.0).abs() < 1e-3, "{m}");
        let m15 = magnification(&cam, [900, 450], &Mat4::IDENTITY, &mesh.vertices, 3.0 / 600.0, 1.5);
        assert!((m15 - 1.0).abs() < 1e-3, "{m15}");
        // Turned toward the camera, the near corner is bigger.
        let near = magnification(&cam, [600, 300], &Mat4::rotate_y(0.6), &mesh.vertices, 3.0 / 600.0, 1.0);
        assert!(near > 1.2, "{near}");
    }

    /// Arc length between neighbouring rows of the centre line.
    fn steps(points: &[Vec3]) -> Vec<f32> {
        points.windows(2).map(|w| (w[1] - w[0]).length()).collect()
    }

    #[test]
    fn scroll_keeps_its_length_and_rolls_behind() {
        let sc = Scroll::default();
        let n = 600;
        for u in [0.0, 0.3, 0.5, 0.9, 1.0] {
            let pts: Vec<Vec3> = (0..=n).map(|i| sc.at(i as f32 / n as f32, u).0).collect();
            let total: f32 = steps(&pts).iter().sum();
            assert!((total - sc.length).abs() < 0.01 * sc.length, "unroll {u}: length {total}");
            // The unrolled part is flat and starts at the left end.
            assert_eq!(pts[0], Vec3::new(-0.5 * sc.length, 0.0, 0.0));
            // The roll lies behind the page and inside its outer radius.
            let r = sc.outer_radius(u);
            let centre = Vec3::new(-0.5 * sc.length + u * sc.length, 0.0, -r);
            for (i, p) in pts.iter().enumerate() {
                if (i as f32 / n as f32) * sc.length <= u * sc.length {
                    assert!(p.z == 0.0 && p.y == 0.0, "unroll {u}: {p:?} not flat");
                } else {
                    assert!(p.z <= 1e-5, "{p:?}");
                    assert!((*p - centre).length() <= r + 1e-4, "{p:?} outside the roll");
                }
            }
        }
        // Unrolling moves the roll right and shrinks it.
        assert!(sc.outer_radius(0.2) > sc.outer_radius(0.8));
        assert!((sc.outer_radius(1.0) - sc.core).abs() < 1e-6);
        // The outer turn is continuous with the flat part: same point, same normal.
        let (a, na) = sc.at(0.5, 0.5);
        let (b, nb) = sc.at(0.5 + 1e-4, 0.5);
        assert!((a - b).length() < 1e-3 && na.dot(nb) > 0.999);
    }

    #[test]
    fn scroll_reduced_motion_is_flat() {
        let sc = Scroll { reduced_motion: true, ..Scroll::default() };
        let (mut a, mut b) = (Vec::new(), Vec::new());
        sc.vertices(0.0, &mut a);
        sc.vertices(0.5, &mut b);
        assert_eq!(a, b);
        assert!(a.iter().all(|v| v.pos[2] == 0.0 && v.normal == [0.0, 0.0, 1.0]));
    }

    #[test]
    fn page_turn_is_an_isometry() {
        // Bending never stretches the page: neighbouring points keep their distance.
        let pg = PageTurn::default();
        for turn in [0.0, 0.2, 0.5, 0.8, 1.0] {
            let mut v = Vec::new();
            pg.vertices(turn, &mut v);
            let row = pg.across as usize + 1;
            let ds = pg.length / pg.segments as f32;
            let dv = pg.width / pg.across as f32;
            for i in 0..pg.segments as usize {
                for j in 0..pg.across as usize {
                    let p = |i: usize, j: usize| Vec3::from(v[i * row + j].pos);
                    let along = (p(i + 1, j) - p(i, j)).length();
                    let across = (p(i, j + 1) - p(i, j)).length();
                    // A chord over a curl is a little shorter than the arc.
                    assert!(along <= ds * 1.001 && along >= ds * 0.97, "turn {turn} {i},{j}: {along} vs {ds}");
                    assert!(across <= dv * 1.001 && across >= dv * 0.97, "turn {turn} {i},{j}: {across} vs {dv}");
                }
            }
        }
    }

    #[test]
    fn page_turn_ends() {
        let pg = PageTurn::default();
        let mut v = Vec::new();
        pg.vertices(0.0, &mut v);
        assert!(v.iter().all(|p| p.pos[2] == 0.0 && p.normal == [0.0, 0.0, 1.0]), "turn 0 is flat");
        pg.vertices(1.0, &mut v);
        let r = pg.radius;
        assert!(v.iter().all(|p| (p.pos[2] - 2.0 * r).abs() < 1e-5 && p.normal == [0.0, 0.0, -1.0]), "turn 1");
        // Half way, part is flat, part curls and part lies turned over.
        pg.vertices(0.5, &mut v);
        let flat = v.iter().filter(|p| p.pos[2] == 0.0).count();
        let over = v.iter().filter(|p| p.normal[2] < -0.999).count();
        assert!(flat > v.len() / 5 && over > v.len() / 10, "{flat} flat, {over} over of {}", v.len());
        // Every point stays on its side of the cylinder: never below the page.
        assert!(v.iter().all(|p| p.pos[2] >= 0.0 && p.pos[2] <= 2.0 * r + 1e-5));
        // Reduced motion jumps to the end.
        let rm = PageTurn { reduced_motion: true, ..pg };
        let (mut a, mut b) = (Vec::new(), Vec::new());
        rm.vertices(0.3, &mut a);
        pg.vertices(1.0, &mut b);
        assert_eq!(a, b);
        rm.vertices(0.0, &mut a);
        pg.vertices(0.0, &mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn shapes_face_their_normals() {
        // Every triangle faces its vertex normal, so the front shows the text.
        let check = |v: &[Vertex], idx: &[u32], what: &str| {
            for t in idx.as_chunks::<3>().0 {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| v[i as usize]);
                let face = (Vec3::from(b.pos) - Vec3::from(a.pos)).cross(Vec3::from(c.pos) - Vec3::from(a.pos));
                if face.length() < 1e-9 {
                    continue;
                }
                let n = Vec3::from(a.normal) + Vec3::from(b.normal) + Vec3::from(c.normal);
                assert!(face.dot(n) > 0.0, "{what}: {t:?}");
            }
        };
        let sc = Scroll::default();
        let pg = PageTurn::default();
        let mut v = Vec::new();
        for x in [0.0, 0.25, 0.5, 0.75, 1.0] {
            sc.vertices(x, &mut v);
            check(&v, &sc.indices(), "scroll");
            pg.vertices(x, &mut v);
            check(&v, &pg.indices(), "page");
        }
    }

    #[test]
    fn counts_match() {
        let sc = Scroll::default();
        let pg = PageTurn::default();
        assert_eq!(sc.mesh().vertices.len(), sc.vertex_count());
        assert_eq!(pg.mesh().vertices.len(), pg.vertex_count());
        assert!(pg.mesh().indices.iter().all(|&i| (i as usize) < pg.vertex_count()));
    }
}
