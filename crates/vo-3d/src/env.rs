//! Reflections of the popup itself for the chrome hero (EG-17): an environment texture
//! the chrome look reads instead of its fixed studio.
//!
//! The texture is equirectangular, [`ENV_SIZE`] (256 x 128) texels of `Rgba16Float`, 256 KB
//! ([`ENV_BYTES`]). Column u is a direction's longitude about the vertical axis, with 0.5
//! facing the viewer (+z); row v is its angle from up (y is down). Every texel holds the
//! studio sky the chrome look draws today, from the look's own colours. The front half
//! (directions toward the viewer) also holds the popup's own layers under the hero: the
//! backdrop (EG-01) of the hero's layer, spread over the front half like an azimuthal map:
//! a direction's angle from the viewer is a distance across the plane from the hero's centre,
//! out to [`Env::reach`] radii at the horizon, where it fades into the sky. So chrome over a
//! panel and a waveform shows them bent around its body, each on the side it lies, the
//! nearest toward the middle.
//!
//! Only the popup's own drawing is read, never screen pixels. The hero's layer (or a parent
//! drawing into the same surface) needs `Element::backdrop`, with a transparent tint to
//! capture without drawing it back, and the frame must be on the effect path. Without a
//! backdrop the chrome reflects its analytic studio, exactly as with no environment.
//!
//! The texture is rebuilt only when what it shows changed. A new place (the hero's centre or
//! size in the surface, the backdrop's place or size) or new look colours rebuild it at once.
//! New content under the hero rebuilds it at most 30 times a second, on the next 1/30 s tick
//! of the frame time: a source that knows when the layers under the hero change passes an
//! [`Env::key`] that changes with them; `None` rebuilds on every tick. Between rebuilds the
//! reflection holds the last one, up to 1/30 s behind; renders at the same `t` from the same
//! state give the same bytes.
//!
//! Reduced motion needs nothing here: a reflection is not motion. It follows the backdrop,
//! which is itself reduced or not.
//!
//! A [`Desktop`] (EG-21) is a made-up desktop to reflect instead, built on the CPU: a sky in
//! one tint, a few dim windows at places a seed picks, and a faint panel bar. It needs no
//! backdrop and reads no screen pixels; [`Env::desktop`] puts it in place of the studio and
//! the layers. It is rebuilt only when it changes: a new tint or seed, or its drift moving a
//! quarter of a texel. The same seed, tint and time give the same bytes ([`Desktop::bytes`]).
//! [`Desktop::windows`] gives the same window shapes for a flat reflection on a glass panel.

use std::cell::Cell;

use bytemuck::{Pod, Zeroable};
use vo_render::color::srgb_to_linear;
use vo_render::{BackdropView, PaintCx};
use vo_scene::geom::Color;

const ENV_WGSL: &str = include_str!("wgsl/env.wgsl");

pub fn env_shader_source() -> &'static str {
    ENV_WGSL
}

/// The environment texture's size in texels.
pub const ENV_SIZE: [u32; 2] = [256, 128];
/// Its memory in bytes: `Rgba16Float`, 8 bytes a texel.
pub const ENV_BYTES: u64 = ENV_SIZE[0] as u64 * ENV_SIZE[1] as u64 * 8;
/// Most rebuilds per second.
pub const ENV_RATE: f64 = 30.0;
/// Heroes whose environment a painter keeps at once; the least recently drawn goes first.
const MAX_ENVS: usize = 4;

/// What a chrome hero reflects besides its studio: the layers under it (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Env {
    /// How far across the popup around the hero the reflection sees, in radii: larger pulls
    /// more of the popup into the reflection, each part smaller.
    pub reach: f32,
    /// How much of the layers shows over the sky, 0 to 1.
    pub strength: f32,
    /// Changes when the layers under the hero change. `None`: rebuild every 1/30 s.
    pub key: Option<u64>,
    /// Reflect this made-up desktop instead of the studio and the layers (EG-21). `reach`,
    /// `strength` and `key` are then unused, and no backdrop is needed.
    pub desktop: Option<Desktop>,
}

impl Default for Env {
    fn default() -> Env {
        Env { reach: 3.0, strength: 1.0, key: None, desktop: None }
    }
}

/// The fastest a [`Desktop`] drifts, in environment texels a second.
pub const DRIFT_MAX: f32 = 2.0;
/// Drift moves in steps of this many texels, so a drifting desktop is rebuilt at most
/// `DRIFT_MAX / DRIFT_STEP` (8) times a second.
const DRIFT_STEP: f64 = 0.25;
/// Where the desktop's screen sits in the environment, in texture coordinates: across
/// (0.5 faces the viewer) and down (0 is straight up).
const SCREEN_U: [f32; 2] = [0.32, 0.68];
const SCREEN_V: [f32; 2] = [0.3, 0.64];
/// The panel bar along the screen's foot, in texture rows.
const PANEL_V: [f32; 2] = [0.64, 0.675];

/// A made-up desktop for a chrome hero or a glass panel to reflect without reading the
/// screen (EG-21): see the module docs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Desktop {
    /// The colour everything is a shade of: the sky, the windows and the panel.
    pub tint: Color,
    /// Picks the number of windows (3 to 6), their places and sizes, and their shades.
    pub seed: u64,
    /// Sideways drift of the windows in environment texels a second, up to [`DRIFT_MAX`].
    /// 0 keeps them still; pass 0 under reduced motion.
    pub drift: f32,
}

/// One window of a [`Desktop`], in texture coordinates (`x` across with 0.5 facing the
/// viewer, `y` down), and how light it is as a share of the tint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DesktopWindow {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub light: f32,
}

impl Desktop {
    /// The tint when there is no desktop colour to read: a neutral blue-grey.
    pub fn neutral_tint() -> Color {
        Color::hex(0x6b7a90, 1.0)
    }

    /// A still desktop in `tint`.
    pub fn new(tint: Color, seed: u64) -> Desktop {
        Desktop { tint, seed, drift: 0.0 }
    }

    /// How far the windows have drifted at `t`, in texture widths, in whole drift steps.
    pub fn offset(&self, t: f64) -> f32 {
        let texels = (self.drift.clamp(0.0, DRIFT_MAX) as f64 * t.max(0.0) / DRIFT_STEP).floor() * DRIFT_STEP;
        (texels / ENV_SIZE[0] as f64).fract() as f32
    }

    /// The windows at `t`, back to front. Their `x` grows with the drift and wraps at 1.
    pub fn windows(&self, t: f64) -> Vec<DesktopWindow> {
        let mut rng = self.seed ^ 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            rng = rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = rng;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32
        };
        let n = 3 + (next() * 4.0) as usize;
        let shift = self.offset(t);
        (0..n.min(6))
            .map(|_| {
                let w = 0.07 + 0.09 * next();
                let h = 0.08 + 0.12 * next();
                let x = SCREEN_U[0] + (SCREEN_U[1] - SCREEN_U[0] - w) * next();
                let y = SCREEN_V[0] + (SCREEN_V[1] - SCREEN_V[0] - h) * next();
                DesktopWindow { x: (x + shift).fract(), y, w, h, light: 0.4 + 0.15 * next() }
            })
            .collect()
    }

    /// The environment at `t`, linear RGBA, row by row.
    pub fn texels(&self, t: f64) -> Vec<[f32; 4]> {
        let tint = [srgb_to_linear(self.tint.r), srgb_to_linear(self.tint.g), srgb_to_linear(self.tint.b)];
        let windows = self.windows(t);
        let [w, h] = [ENV_SIZE[0] as f32, ENV_SIZE[1] as f32];
        // How much of the texel span [a, a + 1) lies in [lo, hi), on a circle of `n` texels.
        let cover = |a: f32, lo: f32, hi: f32, n: f32| {
            let one = |lo: f32, hi: f32| (hi.min(a + 1.0) - lo.max(a)).max(0.0);
            one(lo, hi) + one(lo - n, hi - n) + one(lo + n, hi + n)
        };
        let mut out = Vec::with_capacity((ENV_SIZE[0] * ENV_SIZE[1]) as usize);
        for y in 0..ENV_SIZE[1] {
            let v = (y as f32 + 0.5) / h;
            // The sky: light overhead, dark below the horizon.
            let k = 0.5 - 0.42 * v;
            let panel = cover(y as f32, PANEL_V[0] * h, PANEL_V[1] * h, f32::INFINITY);
            for x in 0..ENV_SIZE[0] {
                let mut kx = k;
                // The panel bar across the screen's foot, a little darker than the sky.
                let across = cover(x as f32, SCREEN_U[0] * w, SCREEN_U[1] * w, w);
                kx -= 0.06 * panel * across;
                for win in &windows {
                    let across = cover(x as f32, win.x * w, (win.x + win.w) * w, w).min(1.0);
                    let c = across * cover(y as f32, win.y * h, (win.y + win.h) * h, f32::INFINITY);
                    // A title strip along the top, a little lighter than the body.
                    let title = across * cover(y as f32, win.y * h, (win.y + 0.18 * win.h) * h, f32::INFINITY);
                    kx = kx * (1.0 - c) + win.light * c + 0.12 * title;
                }
                out.push([tint[0] * kx, tint[1] * kx, tint[2] * kx, 1.0]);
            }
        }
        out
    }

    /// [`Desktop::texels`] as the texture holds them: `Rgba16Float`, little endian.
    pub fn bytes(&self, t: f64) -> Vec<u8> {
        self.texels(t).iter().flatten().flat_map(|&c| f16_bits(c).to_le_bytes()).collect()
    }

    /// Changes whenever [`Desktop::bytes`] would.
    fn stamp(&self, t: f64) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let offset = self.offset(t).to_bits();
        for word in [
            self.seed,
            self.tint.r.to_bits() as u64,
            self.tint.g.to_bits() as u64,
            self.tint.b.to_bits() as u64,
            offset as u64,
        ] {
            h = (h ^ word).wrapping_mul(0x0100_0000_01b3);
        }
        h
    }
}

/// `x` as a half float, rounded to nearest; for the non-negative values a desktop holds.
fn f16_bits(x: f32) -> u16 {
    let x = x.clamp(0.0, 65504.0);
    if x < 6.103_515_6e-5 {
        // Below the smallest normal half: subnormal steps of 2^-24.
        return (x * 16_777_216.0).round() as u16;
    }
    let bits = x.to_bits();
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mant = bits & 0x7f_ffff;
    let half = ((exp as u32) << 10) | (mant >> 13);
    // Round half up on the dropped bits; a carry into the exponent is still right.
    (half + ((mant >> 12) & 1)) as u16
}

/// The texture coordinate (0 to 1 each way) of direction `d` (any length, y down, z toward
/// the viewer). The shaders use the same mapping.
pub fn env_uv(d: [f32; 3]) -> [f32; 2] {
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-12);
    let lon = d[0].atan2(d[2]);
    let u = 0.5 + lon / std::f32::consts::TAU;
    let v = (-d[1] / len).clamp(-1.0, 1.0).acos() / std::f32::consts::PI;
    [u, v]
}

/// The texel direction `d` lands in.
pub fn env_texel(d: [f32; 3]) -> [u32; 2] {
    let [u, v] = env_uv(d);
    let x = ((u * ENV_SIZE[0] as f32) as u32).min(ENV_SIZE[0] - 1);
    let y = ((v * ENV_SIZE[1] as f32) as u32).min(ENV_SIZE[1] - 1);
    [x, y]
}

/// The direction (unit) a texture coordinate stands for: the inverse of [`env_uv`].
pub fn env_dir(uv: [f32; 2]) -> [f32; 3] {
    let lon = (uv[0] - 0.5) * std::f32::consts::TAU;
    let lat = uv[1] * std::f32::consts::PI;
    [lat.sin() * lon.sin(), -lat.cos(), lat.sin() * lon.cos()]
}

thread_local! {
    static REBUILDS: Cell<u64> = const { Cell::new(0) };
}

/// Environment rebuilds so far on this thread (a frame paints on the thread that renders it).
pub fn rebuilds() -> u64 {
    REBUILDS.with(Cell::get)
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct EnvUniform {
    centre: [f32; 4],
    backdrop: [f32; 4],
    c1: [f32; 4],
    c2: [f32; 4],
    c3: [f32; 4],
}

/// Everything a texture shows, to tell when it must be rebuilt.
#[derive(Clone, Copy, PartialEq)]
struct Shown {
    key: Option<u64>,
    uniform: [[f32; 4]; 5],
    backdrop_size: [u32; 2],
}

struct EnvMap {
    owner: usize,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    shown: Shown,
    tick: i64,
    used: u64,
}

/// The chrome heroes' environments, one per hero source, and the pass that builds them.
pub(crate) struct EnvCache {
    pipeline: Option<(wgpu::BindGroupLayout, wgpu::RenderPipeline, wgpu::Sampler)>,
    maps: Vec<EnvMap>,
    frame: u64,
}

/// A chrome hero's place for its environment: centre and radius in surface pixels, and the
/// look's studio colours (linear) as the hero shader gets them.
pub(crate) struct EnvFor {
    pub owner: usize,
    pub centre: [f32; 2],
    pub radius: f32,
    pub sky: [[f32; 4]; 3],
}

impl EnvCache {
    pub(crate) const fn new() -> EnvCache {
        EnvCache { pipeline: None, maps: Vec::new(), frame: 0 }
    }

    pub(crate) fn begin_frame(&mut self) {
        self.frame += 1;
    }

    fn prepare(&mut self, device: &wgpu::Device) {
        if self.pipeline.is_some() {
            return;
        }
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo3d-env"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                entry(
                    1,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(2, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vo3d-env"),
            source: wgpu::ShaderSource::Wgsl(ENV_WGSL.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vo3d-env"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vo3d-env"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_env"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_env"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba16Float,
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
            label: Some("vo3d-env"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        self.pipeline = Some((layout, pipeline, sampler));
    }

    /// The environment for `at` with `env`, rebuilt first when it is due. `None` when the
    /// hero's layer has no backdrop: the chrome then keeps its analytic studio.
    pub(crate) fn get(&mut self, cx: &mut PaintCx<'_>, at: &EnvFor, env: &Env) -> Option<&wgpu::TextureView> {
        if let Some(desktop) = env.desktop {
            let shown = Shown { key: Some(desktop.stamp(cx.t)), uniform: [[0.0; 4]; 5], backdrop_size: [0, 0] };
            let i = self.slot(cx.device, at.owner);
            if self.maps[i].shown != shown {
                let m = &mut self.maps[i];
                let size = wgpu::Extent3d { width: ENV_SIZE[0], height: ENV_SIZE[1], depth_or_array_layers: 1 };
                let layout = wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(ENV_SIZE[0] * 8),
                    rows_per_image: Some(ENV_SIZE[1]),
                };
                cx.queue.write_texture(m.texture.as_image_copy(), &desktop.bytes(cx.t), layout, size);
                m.shown = shown;
                REBUILDS.with(|c| c.set(c.get() + 1));
            }
            return Some(&self.maps[i].view);
        }
        let backdrop = cx.backdrop?;
        let uniform = EnvUniform {
            centre: [at.centre[0], at.centre[1], env.reach.max(0.0) * at.radius, env.strength.clamp(0.0, 1.0)],
            backdrop: [backdrop.origin[0], backdrop.origin[1], backdrop.step.max(1.0), 1.0],
            c1: at.sky[0],
            c2: at.sky[1],
            c3: at.sky[2],
        };
        let size = backdrop.view.texture().size();
        let shown = Shown {
            key: env.key,
            uniform: [uniform.centre, uniform.backdrop, uniform.c1, uniform.c2, uniform.c3],
            backdrop_size: [size.width, size.height],
        };
        let tick = (cx.t * ENV_RATE).floor() as i64;
        let i = self.slot(cx.device, at.owner);
        let m = &self.maps[i];
        // A new place or look rebuilds at once; new content under the hero (a new key, or
        // none) on the next 1/30 s tick.
        let moved = m.shown.uniform != shown.uniform || m.shown.backdrop_size != shown.backdrop_size;
        let changed = env.key.is_none() || m.shown.key != shown.key;
        if moved || (changed && m.tick != tick) {
            self.build(cx, i, backdrop, &uniform);
            let m = &mut self.maps[i];
            (m.shown, m.tick) = (shown, tick);
        }
        Some(&self.maps[i].view)
    }

    /// The map for `owner`, made (and marked used) first.
    fn slot(&mut self, device: &wgpu::Device, owner: usize) -> usize {
        let i = match self.maps.iter().position(|m| m.owner == owner) {
            Some(i) => i,
            None => {
                if self.maps.len() >= MAX_ENVS {
                    let lru = (0..self.maps.len()).min_by_key(|&i| self.maps[i].used).unwrap_or(0);
                    self.maps.swap_remove(lru);
                }
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("vo3d-env"),
                    size: wgpu::Extent3d { width: ENV_SIZE[0], height: ENV_SIZE[1], depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view = texture.create_view(&Default::default());
                // A shown state no build matches, and no tick: the first paint builds.
                let never = Shown { key: None, uniform: [[f32::NAN; 4]; 5], backdrop_size: [0, 0] };
                self.maps.push(EnvMap { owner, texture, view, shown: never, tick: i64::MIN, used: 0 });
                self.maps.len() - 1
            }
        };
        self.maps[i].used = self.frame;
        i
    }

    fn build(&mut self, cx: &mut PaintCx<'_>, i: usize, backdrop: BackdropView<'_>, uniform: &EnvUniform) {
        self.prepare(cx.device);
        let (layout, pipeline, sampler) = self.pipeline.as_ref().unwrap();
        let buffer = cx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo3d-env"),
            size: std::mem::size_of::<EnvUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: true,
        });
        buffer.slice(..).get_mapped_range_mut().unwrap().copy_from_slice(bytemuck::bytes_of(uniform));
        buffer.unmap();
        let bind = cx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo3d-env"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(backdrop.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        });
        let mut pass = cx.enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vo3d-env"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.maps[i].view,
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
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        REBUILDS.with(|c| c.set(c.get() + 1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_shader_validates_in_naga() {
        let src = env_shader_source();
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }

    #[test]
    fn memory_is_256_kb() {
        assert_eq!(ENV_BYTES, 256 * 1024);
    }

    #[test]
    fn axes_land_where_documented() {
        // Toward the viewer: the middle column, the middle row.
        assert_eq!(env_texel([0.0, 0.0, 1.0]), [128, 64]);
        // Up is the top row, down the bottom one (y is down).
        assert_eq!(env_texel([0.0, -1.0, 0.0])[1], 0);
        assert_eq!(env_texel([0.0, 1.0, 0.0])[1], 127);
        // Right is a quarter turn on (u 0.75), left a quarter back (u 0.25).
        assert_eq!(env_texel([1.0, 0.0, 0.0]), [192, 64]);
        assert_eq!(env_texel([-1.0, 0.0, 0.0]), [64, 64]);
        // Away from the viewer: the seam at the edges.
        let back = env_texel([0.0, 0.0, -1.0])[0];
        assert!(back == 0 || back == 255, "{back}");
        // Front directions fill the middle half of the columns.
        for d in [[0.7, 0.3, 0.2], [-0.7, -0.3, 0.2], [0.1, 0.9, 0.05]] {
            let x = env_texel(d)[0];
            assert!((64..192).contains(&x), "{d:?} -> {x}");
        }
    }

    #[test]
    fn mapping_round_trips_and_ignores_length() {
        for y in 0..ENV_SIZE[1] {
            for x in (0..ENV_SIZE[0]).step_by(7) {
                let uv = [(x as f32 + 0.5) / ENV_SIZE[0] as f32, (y as f32 + 0.5) / ENV_SIZE[1] as f32];
                let d = env_dir(uv);
                assert!(((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]) - 1.0).abs() < 1e-4);
                assert_eq!(env_texel(d), [x, y], "{uv:?}");
                assert_eq!(env_texel([3.0 * d[0], 3.0 * d[1], 3.0 * d[2]]), [x, y]);
            }
        }
    }

    /// The env shader's studio must stay the one the hero draws without an environment.
    #[test]
    fn studio_matches_the_hero_shader() {
        let body = |src: &str, colours: &str| {
            let start = src.find("fn studio").expect("studio");
            let end = start + src[start..].find("\n}\n").expect("end");
            src[start..end].replace(colours, "C.")
        };
        assert_eq!(body(ENV_WGSL, "env.c"), body(crate::sdf::hero_shader_source(), "hero.c"));
    }

    fn desktop(tint: u32, seed: u64) -> Desktop {
        Desktop::new(Color::hex(tint, 1.0), seed)
    }

    /// Hue in degrees of a linear colour, and its chroma (max minus min).
    fn hue(c: [f32; 4]) -> (f32, f32) {
        let [r, g, b, _] = c;
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let d = max - min;
        if d <= 1e-6 {
            return (0.0, 0.0);
        }
        let h = if max == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        (60.0 * h, d)
    }

    /// The sky's mean hue, weighting each texel by its chroma (a circular mean).
    fn mean_hue(texels: &[[f32; 4]]) -> f32 {
        let (mut x, mut y) = (0.0f64, 0.0f64);
        for &c in texels {
            let (h, w) = hue(c);
            x += (w * h.to_radians().cos()) as f64;
            y += (w * h.to_radians().sin()) as f64;
        }
        (y.atan2(x).to_degrees() as f32).rem_euclid(360.0)
    }

    #[test]
    fn desktop_sky_takes_the_tint_hue() {
        for tint in [0x3d7ae0, 0xe0603d, 0x5fbf6a, 0x9a5fd0, 0x6b7a90] {
            let want = hue([
                srgb_to_linear(((tint >> 16) & 0xff) as f32 / 255.0),
                srgb_to_linear(((tint >> 8) & 0xff) as f32 / 255.0),
                srgb_to_linear((tint & 0xff) as f32 / 255.0),
                1.0,
            ])
            .0;
            let got = mean_hue(&desktop(tint, 3).texels(0.0));
            let off = (got - want + 180.0).rem_euclid(360.0) - 180.0;
            assert!(off.abs() < 10.0, "tint {tint:06x}: hue {got:.1}, want {want:.1}");
        }
    }

    #[test]
    fn desktop_same_seed_same_bytes() {
        let d = desktop(0x3d7ae0, 42);
        assert_eq!(d.bytes(0.0), d.bytes(0.0));
        assert_eq!(d.bytes(0.0).len() as u64, ENV_BYTES);
        assert_ne!(d.bytes(0.0), desktop(0x3d7ae0, 43).bytes(0.0), "another seed, the same desktop");
        let moving = Desktop { drift: 1.5, ..d };
        assert_eq!(moving.bytes(7.3), moving.bytes(7.3));
    }

    #[test]
    fn desktop_has_three_to_six_windows_on_its_screen() {
        for seed in 0..200 {
            let w = desktop(0x3d7ae0, seed).windows(0.0);
            assert!((3..=6).contains(&w.len()), "seed {seed}: {} windows", w.len());
            for win in w {
                assert!(win.x >= SCREEN_U[0] && win.x + win.w <= SCREEN_U[1] + 1e-6, "{win:?}");
                assert!(win.y >= SCREEN_V[0] && win.y + win.h <= SCREEN_V[1] + 1e-6, "{win:?}");
            }
        }
    }

    #[test]
    fn desktop_windows_show_in_the_bytes() {
        // A window's middle is lighter than the bare sky at the same height outside it.
        let d = desktop(0x3d7ae0, 9);
        let px = d.texels(0.0);
        for win in d.windows(0.0) {
            let row = ((win.y + 0.6 * win.h) * ENV_SIZE[1] as f32) as usize;
            let col = ((win.x + 0.5 * win.w) * ENV_SIZE[0] as f32) as usize;
            let sky = px[row * ENV_SIZE[0] as usize + 10][2];
            assert!(px[row * ENV_SIZE[0] as usize + col][2] > sky * 1.05, "{win:?}");
        }
    }

    #[test]
    fn desktop_drift_stays_under_two_texels_a_second() {
        let still = desktop(0x3d7ae0, 5);
        assert_eq!(still.bytes(0.0), still.bytes(100.0), "no drift, yet it moved");
        let fast = Desktop { drift: 50.0, ..still };
        let moved = (fast.offset(10.0) - fast.offset(0.0)) * ENV_SIZE[0] as f32;
        assert!((moved - 10.0 * DRIFT_MAX).abs() < DRIFT_STEP as f32 + 1e-3, "{moved} texels in 10 s");
        // The same windows, shifted: none appear, none vanish.
        let (a, b) = (fast.windows(0.0), fast.windows(3.0));
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(&b).all(|(p, q)| p.y == q.y && p.w == q.w));
    }

    #[test]
    fn desktop_stamp_follows_the_bytes() {
        let d = Desktop { drift: 1.0, ..desktop(0x3d7ae0, 5) };
        // Within one quarter-texel step: the same stamp; across it, a new one.
        assert_eq!(d.stamp(1.0), d.stamp(1.2));
        assert_ne!(d.stamp(1.0), d.stamp(1.3));
        assert_ne!(d.stamp(1.0), desktop(0x3d7ae1, 5).stamp(1.0));
    }

    #[test]
    fn half_floats_round_trip() {
        assert_eq!(f16_bits(0.0), 0);
        assert_eq!(f16_bits(1.0), 0x3c00);
        assert_eq!(f16_bits(0.5), 0x3800);
        assert_eq!(f16_bits(65504.0), 0x7bff);
        assert_eq!(f16_bits(0.333_333_34), 0x3555);
        assert_eq!(f16_bits(6.0e-8), 1);
    }
}
