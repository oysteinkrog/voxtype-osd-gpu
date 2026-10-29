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

use std::cell::Cell;

use bytemuck::{Pod, Zeroable};
use vo_render::{BackdropView, PaintCx};

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
}

impl Default for Env {
    fn default() -> Env {
        Env { reach: 3.0, strength: 1.0, key: None }
    }
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
        let i = match self.maps.iter().position(|m| m.owner == at.owner) {
            Some(i) => i,
            None => {
                if self.maps.len() >= MAX_ENVS {
                    let lru = (0..self.maps.len()).min_by_key(|&i| self.maps[i].used).unwrap_or(0);
                    self.maps.swap_remove(lru);
                }
                let view = cx
                    .device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("vo3d-env"),
                        size: wgpu::Extent3d { width: ENV_SIZE[0], height: ENV_SIZE[1], depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba16Float,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    })
                    .create_view(&Default::default());
                // A shown state no build matches, and no tick: the first paint builds.
                let never = Shown { key: None, uniform: [[f32::NAN; 4]; 5], backdrop_size: [0, 0] };
                self.maps.push(EnvMap { owner: at.owner, view, shown: never, tick: i64::MIN, used: 0 });
                self.maps.len() - 1
            }
        };
        self.maps[i].used = self.frame;
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
}
